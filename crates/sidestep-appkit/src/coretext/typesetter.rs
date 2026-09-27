//! `CTTypesetter`, `CTFramesetter` and `CTFrame`: lines broken to a width
//! and stacked in a rectangle, by the text engine's line breaking (each
//! paragraph laid out once, its lines taken as they are).
//!
//! A typesetter suggests where a line from an index breaks (at a word
//! boundary, or inside a word that doesn't fit on a line of its own, or
//! between any two clusters) as a count of UTF-16 units, trailing
//! whitespace and a paragraph's separator included, as CoreText counts,
//! and makes lines of any range. It keeps the lines of the last paragraph
//! it broke, so a loop asking for one line after another lays each
//! paragraph out once. An offset moves where tab stops fall: they're
//! measured from that far before the line's start (measured on macOS).
//!
//! A framesetter fills the bounding box of its path (other shapes aren't
//! followed) with each paragraph's lines, in its style: indents,
//! alignment, line heights and spacing. Paragraphs are broken whole and
//! their lines cut where the range asked for ends, as on macOS. Lines are
//! as tall as their ascent, descent and leading, each rounded to whole
//! points, and the first baseline is the first line's rounded ascent below
//! the top (measured on macOS); lines that don't fit whole are left out of
//! the frame's visible range, while its string range is the range asked
//! for. Line origins are from the box's bottom left corner.

use std::ops::Range;
use std::ptr::NonNull;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFArray, CFAttributedString, CFDictionary, CFIndex, CFRange, CFTypeID, CGPoint, CGSize};
use objc2_core_graphics::{CGContext, CGPath};
use objc2_core_text::{CTFrame, CTFramesetter, CTLine, CTTypesetter};
use objc2_foundation::{NSArray, NSDictionary, NSString};

use super::attrs::{Styled, styled};
use super::line::{CTLineImpl, draw_line, line_from, line_of};
use super::{cf_range, owned};
use crate::text::glyphs::{self, Breaking};
use crate::text::layout::{Align, Paragraph};

pub(crate) struct TypesetterIvars {
    styled: Styled,
    /// The last paragraph's lines a suggestion broke: how, and where each
    /// line starts, the paragraph's end last.
    broken: Mutex<Option<(Broken, Vec<usize>)>>,
}

/// How a paragraph's lines were broken: its end, the width, offset and
/// breaking (words or clusters).
#[derive(Clone, Copy, PartialEq)]
struct Broken {
    end: usize,
    width: u64,
    offset: u64,
    clusters: bool,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; typesetters are
    // immutable but for the lines last broken, kept behind a lock.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTTypesetter"]
    #[ivars = TypesetterIvars]
    pub(crate) struct CTTypesetterImpl;

    unsafe impl NSObjectProtocol for CTTypesetterImpl {}
);

fn typesetter_imp(t: &CTTypesetter) -> &CTTypesetterImpl {
    // SAFETY: every CTTypesetter is a CTTypesetterImpl.
    unsafe { &*(t as *const CTTypesetter).cast::<CTTypesetterImpl>() }
}

fn new_typesetter(styled: Styled) -> Retained<CTTypesetterImpl> {
    let this = CTTypesetterImpl::alloc().set_ivars(TypesetterIvars { styled, broken: Mutex::new(None) });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A range in UTF-16 units of `styled`: a zero length reaches the end.
fn clip(styled: &Styled, range: CFRange) -> Range<usize> {
    super::range_in(range, styled.utf16_len, true)
}

/// The paragraph style of the character at `unit`.
fn paragraph_at(styled: &Styled, unit: usize) -> &Paragraph {
    let at = styled.units.partition_point(|u| u.end <= unit).min(styled.units.len().saturating_sub(1));
    let attrs = styled.runs.get(at).map_or(0, |r| r.attrs);
    &styled.attrs[attrs as usize].paragraph
}

/// The UTF-16 length of the first line laid out from `start`, broken
/// between words or clusters to `width` from `offset`.
fn first_line(typesetter: &CTTypesetterImpl, start: usize, clusters: bool, width: f64, offset: f64) -> CFIndex {
    let i = typesetter.ivars();
    let styled = &i.styled;
    if start >= styled.utf16_len {
        return 0;
    }
    let end = styled.paragraph_end(start);
    let how = Broken { end, width: width.to_bits(), offset: offset.to_bits(), clusters };
    let next = |starts: &[usize]| starts.iter().copied().find(|&s| s > start);
    let mut broken = i.broken.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((was, starts)) = broken.as_ref()
        && *was == how
        && starts.binary_search(&start).is_ok()
    {
        return next(starts).map_or(end - start, |n| n - start).max(1) as CFIndex;
    }
    let (text, runs) = styled.slice(start..end);
    let breaking = if clusters { Breaking::Clusters(width as f32) } else { Breaking::Words(width as f32) };
    let lines = glyphs::glyph_lines_at(&text, &styled.attrs, &runs, breaking, styled.direction, offset as f32);
    let mut starts: Vec<usize> = lines.iter().map(|l| start + l.range.start).filter(|&s| s < end).collect();
    starts.push(end);
    starts.dedup();
    let length = next(&starts).map_or(end - start, |n| n - start).clamp(1, end - start);
    *broken = Some((how, starts));
    length as CFIndex
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_TYPESETTER
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterCreateWithAttributedString(
    string: Option<&CFAttributedString>,
) -> Option<NonNull<CTTypesetter>> {
    Some(owned(new_typesetter(styled(string?))))
}

/// The options (unbounded layout, bidi processing turned off, a forced
/// embedding level) aren't applied.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterCreateWithAttributedStringAndOptions(
    string: Option<&CFAttributedString>,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CTTypesetter>> {
    CTTypesetterCreateWithAttributedString(string)
}

/// A line of `range` (to the end for a zero length), its tab stops
/// measured from `offset` before its start.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterCreateLineWithOffset(
    typesetter: Option<&CTTypesetter>,
    range: CFRange,
    offset: f64,
) -> Option<NonNull<CTLine>> {
    let styled = &typesetter_imp(typesetter?).ivars().styled;
    let range = clip(styled, range);
    if offset == 0.0 || !offset.is_finite() {
        return Some(owned(line_of(styled, range)));
    }
    let (text, runs) = styled.slice(range.clone());
    let lines = glyphs::glyph_lines_at(&text, &styled.attrs, &runs, Breaking::None, styled.direction, offset as f32);
    let line = lines.into_iter().next().unwrap_or_default();
    Some(owned(line_from(styled, &line, range.start, range)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterCreateLine(
    typesetter: Option<&CTTypesetter>,
    range: CFRange,
) -> Option<NonNull<CTLine>> {
    CTTypesetterCreateLineWithOffset(typesetter, range, 0.0)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterSuggestLineBreakWithOffset(
    typesetter: Option<&CTTypesetter>,
    start: CFIndex,
    width: f64,
    offset: f64,
) -> CFIndex {
    let Some(t) = typesetter else { return 0 };
    first_line(typesetter_imp(t), start.max(0) as usize, false, width, offset)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterSuggestLineBreak(
    typesetter: Option<&CTTypesetter>,
    start: CFIndex,
    width: f64,
) -> CFIndex {
    CTTypesetterSuggestLineBreakWithOffset(typesetter, start, width, 0.0)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterSuggestClusterBreakWithOffset(
    typesetter: Option<&CTTypesetter>,
    start: CFIndex,
    width: f64,
    offset: f64,
) -> CFIndex {
    let Some(t) = typesetter else { return 0 };
    first_line(typesetter_imp(t), start.max(0) as usize, true, width, offset)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTTypesetterSuggestClusterBreak(
    typesetter: Option<&CTTypesetter>,
    start: CFIndex,
    width: f64,
) -> CFIndex {
    CTTypesetterSuggestClusterBreakWithOffset(typesetter, start, width, 0.0)
}

// Framesetters.

pub(crate) struct FramesetterIvars {
    typesetter: Retained<CTTypesetterImpl>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; framesetters are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTFramesetter"]
    #[ivars = FramesetterIvars]
    pub(crate) struct CTFramesetterImpl;

    unsafe impl NSObjectProtocol for CTFramesetterImpl {}
);

fn framesetter_imp(f: &CTFramesetter) -> &CTFramesetterImpl {
    // SAFETY: every CTFramesetter is a CTFramesetterImpl.
    unsafe { &*(f as *const CTFramesetter).cast::<CTFramesetterImpl>() }
}

fn new_framesetter(typesetter: Retained<CTTypesetterImpl>) -> Retained<CTFramesetterImpl> {
    let this = CTFramesetterImpl::alloc().set_ivars(FramesetterIvars { typesetter });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFramesetterGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_FRAMESETTER
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFramesetterCreateWithAttributedString(
    string: Option<&CFAttributedString>,
) -> Option<NonNull<CTFramesetter>> {
    Some(owned(new_framesetter(new_typesetter(styled(string?)))))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFramesetterCreateWithTypesetter(
    typesetter: Option<&CTTypesetter>,
) -> Option<NonNull<CTFramesetter>> {
    Some(owned(new_framesetter(typesetter_imp(typesetter?).retain())))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFramesetterGetTypesetter(
    framesetter: Option<&CTFramesetter>,
) -> Option<NonNull<CTTypesetter>> {
    Some(super::borrowed(&*framesetter_imp(framesetter?).ivars().typesetter))
}

/// A line placed in a frame: the line, its origin (from the box's bottom
/// left), and how far down its bottom is.
struct Placed {
    line: Retained<CTLineImpl>,
    x: f64,
    baseline: f64,
    bottom: f64,
}

/// Lay `range` of `styled` out in a box `width` × `height` (infinite for
/// none): the lines that fit whole, and where the text laid out ends.
fn fill(styled: &Styled, range: Range<usize>, width: f64, height: f64) -> (Vec<Placed>, usize) {
    let mut placed = Vec::new();
    let mut top = 0.0f64;
    let mut at = range.start;
    let mut previous: Option<Paragraph> = None;
    'paragraphs: while at < range.end {
        // The whole paragraph is broken; its lines are cut where the range
        // ends.
        let end = styled.paragraph_end(at);
        let para = paragraph_at(styled, at).clone();
        let (text, runs) = styled.slice(at..end);
        let tail = if para.tail_indent > 0.0 { para.tail_indent } else { width + para.tail_indent };
        let first_width = (tail - para.first_line_head_indent.max(0.0)).max(1.0);
        // Lines after the first are as wide as the head indent leaves; the
        // first line takes its own indent (laid out apart if they differ).
        let lines =
            glyphs::glyph_lines(&text, &styled.attrs, &runs, Breaking::Words(first_width as f32), styled.direction);
        let mut lines: Vec<(glyphs::GlyphLine, usize)> = lines.into_iter().map(|l| (l, at)).collect();
        if para.head_indent != para.first_line_head_indent && lines.len() > 1 {
            let rest_start = at + lines[0].0.range.end;
            let rest_width = (tail - para.head_indent.max(0.0)).max(1.0);
            let (rest_text, rest_runs) = styled.slice(rest_start..end);
            let rest = glyphs::glyph_lines(
                &rest_text,
                &styled.attrs,
                &rest_runs,
                Breaking::Words(rest_width as f32),
                styled.direction,
            );
            lines.truncate(1);
            lines.extend(rest.into_iter().map(|l| (l, rest_start)));
        }
        for (i, (gl, base)) in lines.into_iter().enumerate() {
            let line_range = base + gl.range.start..(base + gl.range.end).min(range.end);
            if line_range.start >= range.end {
                at = range.end;
                break 'paragraphs;
            }
            let gl = if base + gl.range.end > range.end { gl.cut(range.end - base) } else { gl };
            let line = line_from(styled, &gl, base, line_range.clone());
            let (ascent, descent, leading) = line.metrics();
            let mut line_height = ascent.round() + descent.round() + leading.round();
            if para.line_height_multiple > 0.0 {
                line_height *= para.line_height_multiple;
            }
            if para.min_line_height > 0.0 {
                line_height = line_height.max(para.min_line_height);
            }
            if para.max_line_height > 0.0 {
                line_height = line_height.min(para.max_line_height);
            }
            let mut line_top = top;
            if let Some(prev) = &previous {
                line_top += prev.line_spacing;
                if i == 0 {
                    line_top += prev.paragraph_spacing + para.paragraph_spacing_before;
                }
            }
            let bottom = line_top + line_height;
            if bottom > height + 1e-6 {
                at = line_range.start;
                break 'paragraphs;
            }
            let indent = if i == 0 { para.first_line_head_indent } else { para.head_indent }.max(0.0);
            let room = tail - indent;
            let content = line.width() - line.trailing();
            let factor = match para.alignment {
                Align::Right => 1.0,
                Align::Center => 0.5,
                _ => 0.0,
            };
            let x = indent + ((room - content) * factor).max(0.0);
            let baseline = bottom - (descent.round() + leading.round());
            placed.push(Placed { line, x, baseline, bottom });
            top = bottom;
            previous = Some(para.clone());
            at = line_range.end;
        }
        at = at.max(end.min(range.end));
    }
    (placed, at.min(range.end))
}

pub(crate) struct FrameIvars {
    lines: Retained<NSArray<CTLineImpl>>,
    origins: Vec<CGPoint>,
    range: CFRange,
    visible: CFRange,
    path: Retained<AnyObject>,
    attributes: Option<Retained<NSDictionary<NSString, AnyObject>>>,
    /// The box's origin, which drawing puts the origins relative to.
    corner: CGPoint,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; frames are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTFrame"]
    #[ivars = FrameIvars]
    pub(crate) struct CTFrameImpl;

    unsafe impl NSObjectProtocol for CTFrameImpl {}
);

fn frame_imp(f: &CTFrame) -> &CTFrameImpl {
    // SAFETY: every CTFrame is a CTFrameImpl.
    unsafe { &*(f as *const CTFrame).cast::<CTFrameImpl>() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFrameGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_FRAME
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFramesetterCreateFrame(
    framesetter: Option<&CTFramesetter>,
    range: CFRange,
    path: Option<&CGPath>,
    attributes: Option<&CFDictionary>,
) -> Option<NonNull<CTFrame>> {
    let (framesetter, path) = (framesetter?, path?);
    let styled = &framesetter_imp(framesetter).ivars().typesetter.ivars().styled;
    let range = clip(styled, range);
    let bounds = objc2_core_graphics::CGPath::bounding_box(Some(path));
    let (w, h) = (bounds.size.width, bounds.size.height);
    let (placed, end) = fill(styled, range.clone(), w, h);
    let origins: Vec<CGPoint> = placed.iter().map(|p| CGPoint { x: p.x, y: h - p.baseline }).collect();
    let lines: Vec<Retained<CTLineImpl>> = placed.into_iter().map(|p| p.line).collect();
    let ivars = FrameIvars {
        lines: NSArray::from_retained_slice(&lines),
        origins,
        range: cf_range(range.clone()),
        visible: cf_range(range.start..end.max(range.start)),
        // SAFETY: a CGPath is an object.
        path: unsafe { Retained::retain(path as *const CGPath as *mut AnyObject) }?,
        // SAFETY: a CFDictionary is an NSDictionary here.
        attributes: attributes
            .map(|a| unsafe { &*(a as *const CFDictionary).cast::<NSDictionary<NSString, AnyObject>>() }.retain()),
        corner: bounds.origin,
    };
    let this = CTFrameImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let frame: Retained<CTFrameImpl> = unsafe { msg_send![super(this), init] };
    Some(owned(frame))
}

/// The size the text needs in `constraints` (0 or less for no limit):
/// the widest line (its trailing whitespace left out) and the lines'
/// height, and what fits.
///
/// # Safety
///
/// `fit_range` is null or valid to write a range through.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFramesetterSuggestFrameSizeWithConstraints(
    framesetter: Option<&CTFramesetter>,
    range: CFRange,
    _attributes: Option<&CFDictionary>,
    constraints: CGSize,
    fit_range: *mut CFRange,
) -> CGSize {
    let Some(framesetter) = framesetter else { return CGSize { width: 0.0, height: 0.0 } };
    let styled = &framesetter_imp(framesetter).ivars().typesetter.ivars().styled;
    let range = clip(styled, range);
    let limit = |v: f64| if v > 0.0 && v < 1e7 { v } else { f64::INFINITY };
    let (placed, end) = fill(styled, range.clone(), limit(constraints.width), limit(constraints.height));
    let width = placed.iter().map(|p| p.x + p.line.width() - p.line.trailing()).fold(0.0, f64::max);
    let height = placed.last().map_or(0.0, |p| p.bottom);
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(fit_range, cf_range(range.start..end.max(range.start))) };
    CGSize { width, height }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFrameGetStringRange(frame: Option<&CTFrame>) -> CFRange {
    frame.map_or(CFRange { location: 0, length: 0 }, |f| frame_imp(f).ivars().range)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFrameGetVisibleStringRange(frame: Option<&CTFrame>) -> CFRange {
    frame.map_or(CFRange { location: 0, length: 0 }, |f| frame_imp(f).ivars().visible)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFrameGetPath(frame: Option<&CTFrame>) -> Option<NonNull<CGPath>> {
    Some(super::borrowed(&*frame_imp(frame?).ivars().path))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFrameGetFrameAttributes(frame: Option<&CTFrame>) -> Option<NonNull<CFDictionary>> {
    frame_imp(frame?).ivars().attributes.as_deref().map(super::borrowed)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFrameGetLines(frame: Option<&CTFrame>) -> Option<NonNull<CFArray>> {
    Some(super::borrowed(&*frame_imp(frame?).ivars().lines))
}

/// # Safety
///
/// `origins` has room for the range's origins.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFrameGetLineOrigins(frame: Option<&CTFrame>, range: CFRange, origins: *mut CGPoint) {
    let Some(frame) = frame else { return };
    // SAFETY: as the caller promises.
    unsafe { super::line::copy_out(&frame_imp(frame).ivars().origins, range, origins) };
}

/// Each line drawn at its origin (from the path's box), through the text
/// matrix.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFrameDraw(frame: Option<&CTFrame>, context: Option<&CGContext>) {
    let (Some(frame), Some(context)) = (frame, context) else { return };
    let i = frame_imp(frame).ivars();
    for (line, origin) in i.lines.iter().zip(&i.origins) {
        crate::coregraphics::context::with_state(context, |st| {
            let [a, b, c, d, _, _] = st.text_matrix.as_coeffs();
            st.text_matrix = kurbo::Affine::new([a, b, c, d, i.corner.x + origin.x, i.corner.y + origin.y]);
        });
        draw_line(&line, context);
    }
}
