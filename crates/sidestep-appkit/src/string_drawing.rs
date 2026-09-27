//! String drawing: the methods AppKit adds to `NSString` for drawing and
//! measuring with an attribute dictionary.
//!
//! The attributes become a [`layout::Attrs`] and the text is laid out by
//! [`layout::lay_out`], which takes a list of attribute runs, so drawing an
//! attributed string is the same call with one run per attribute range
//! ([`draw`], [`measure`]). Drawing records a glyph run op per run of
//! glyphs, fills for backgrounds before them and fills for underlines and
//! strikethroughs after them.
//!
//! The methods are defined on a helper class for their encodings and added
//! to `NSString` by a link-time category (`NSStringDrawing`), which the
//! runtime attaches when `NSString` registers, whatever loads first. So a
//! program can measure or draw a string as its very first AppKit call.
//!
//! `NSAttributedString` gets the same methods without the dictionary
//! (`size`, `drawAtPoint:`, `drawInRect:`, `drawWithRect:options:context:`,
//! `boundingRectWithSize:options:context:`) from a second category: its
//! runs become one set of attributes per distinct dictionary, missing
//! attributes take the defaults (Helvetica 12, which is the 12-point
//! interface font here, in black), and each paragraph is laid out in the
//! paragraph style of its first character, as AppKit's are.
//! `NSStringDrawingContext` reports the bounds of what was measured or
//! drawn; it doesn't shrink text (`minimumScaleFactor` is kept but its
//! `actualScaleFactor` is always 1).
//!
//! [`attribute_spans`] turns an attributed string's attribute dictionaries
//! into the attributes and UTF-16 spans that `text::lines` lays out (and,
//! through byte runs, that [`draw`] and [`measure`] take), and
//! [`record_frame`] records laid-out lines for a layout manager.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send, sel};
#[allow(deprecated)] // NSObliqueness, which TextKit 2 leaves out and string drawing draws.
use objc2_app_kit::{
    NSAttachmentAttributeName, NSBackgroundColorAttributeName, NSBaselineOffsetAttributeName, NSColor, NSFont,
    NSFontAttributeName, NSForegroundColorAttributeName, NSKernAttributeName, NSLigatureAttributeName,
    NSObliquenessAttributeName, NSParagraphStyle, NSParagraphStyleAttributeName, NSShadowAttributeName,
    NSStrikethroughColorAttributeName, NSStrikethroughStyleAttributeName, NSStringDrawingContext,
    NSStringDrawingOptions, NSStrokeColorAttributeName, NSStrokeWidthAttributeName, NSUnderlineColorAttributeName,
    NSUnderlineStyleAttributeName,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize, NSString};

use crate::font::{number, text_font};
use crate::graphics::{self, Xf, with_recorder};
use crate::paragraph::paragraph_of;
use crate::protocol::{GlyphRun, Op, Rect};
use crate::text::fonts::{self, Design, FontSpec};
use crate::text::layout::{self, Attrs, FxBuild, Options, PlacedFill, PlacedRun, Run, Shadow, TextFont, TextLayout};
use crate::text::lines::{Frame, Line, Span};

type Attributes = NSDictionary<NSString, AnyObject>;

/// Where text goes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Place {
    /// `drawAtPoint:`: the corner of the text nearest the view's origin.
    Point(NSPoint),
    /// `drawInRect:`: wrapped to the rectangle's width and clipped to it.
    /// Lines below it are clipped away, not dropped, as on macOS.
    Rect(NSRect),
    /// `drawWithRect:options:`: with `usesLineFragmentOrigin`, as `Rect`,
    /// except that `truncatesLastVisibleLine` keeps to the height and
    /// truncates the last line in it; without, one line on a baseline at
    /// the rectangle's origin.
    WithRect(NSRect, NSStringDrawingOptions),
}

/// An attributed string's attachments: for each of its attributes (by
/// index), the dictionary of the attributes with an attachment box (its
/// `NSAttachmentAttributeName` value is the attachment).
pub(crate) type Attachments = [Option<Retained<Attributes>>];

/// Draw text with attribute runs into the view being drawn. Text laid out
/// before is recorded at once; other text waits for the end of the pass,
/// when it is laid out together (see `text::pool`).
pub(crate) fn draw(text: &str, attrs: &[Attrs], runs: &[Run], place: Place) {
    draw_with(text, attrs, runs, place, &[]);
}

/// [`draw`], drawing the attachments `objects` holds (see [`Attachments`])
/// into their boxes. Text with attachments is laid out at once, so that
/// they are drawn with it, in order.
pub(crate) fn draw_with(text: &str, attrs: &[Attrs], runs: &[Run], place: Place, objects: &Attachments) {
    let setting = || crate::attachment::Setting::drawing(width_of(place));
    let opts = match place {
        Place::Point(_) => Options::UNBOUNDED,
        // An empty rectangle shows nothing.
        Place::Rect(r) if r.size.width <= 0.0 || r.size.height <= 0.0 => return,
        Place::Rect(r) => Options { width: limit(r.size.width), ..Options::UNBOUNDED },
        Place::WithRect(r, o) if !o.contains(NSStringDrawingOptions::TruncatesLastVisibleLine) => {
            Options { height: f32::INFINITY, ..options(r.size, o) }
        }
        Place::WithRect(r, o) => options(r.size, o),
    };
    if !graphics::recording() {
        return;
    }
    if objects.iter().any(Option::is_some) {
        let laid = layout::lay_out(text, attrs, runs, &opts);
        let mut boxes = Vec::new();
        let mut shown = NSRect::ZERO;
        with_recorder(|rec| {
            let (left, top, clip) = placement(rec.xf, rec.clip, place, &laid);
            emit(&mut rec.ops, &laid, left, top, clip);
            shown = rec.xf.inverse_rect(clip);
            for a in &laid.attachments {
                let r = Rect::new(left + a.rect[0], top + a.rect[1], left + a.rect[2], top + a.rect[3]);
                if !clip.intersect(&r).is_empty() {
                    boxes.push((rec.xf.inverse_rect(r), a.byte, a.attrs));
                }
            }
        });
        draw_attachments(&boxes, shown, |byte| text[..byte].encode_utf16().count(), objects, &setting());
        return;
    }
    let recorded = layout::with_cached(text, attrs, runs, &opts, |laid| {
        with_recorder(|rec| {
            let (left, top, clip) = placement(rec.xf, rec.clip, place, laid);
            emit(&mut rec.ops, laid, left, top, clip);
        })
    });
    match recorded {
        Ok(()) => {}
        Err(hash) => with_recorder(|rec| {
            // A bitmap context draws as it goes: lay the text out now.
            if rec.immediate {
                let laid = layout::lay_out(text, attrs, runs, &opts);
                let (left, top, clip) = placement(rec.xf, rec.clip, place, &laid);
                emit(&mut rec.ops, &laid, left, top, clip);
                return;
            }
            let job = rec.pending.job_for(text, attrs, runs, opts, hash);
            rec.pending.places.push(Deferred { at: rec.ops.len(), job, xf: rec.xf, clip: rec.clip, place });
        }),
    }
}

/// Draw the attachments of `boxes` (their rects in user space, bytes and
/// attributes' indexes), clipped to `shown`; `index` turns a byte into the
/// UTF-16 index the attachments are told.
fn draw_attachments(
    boxes: &[(NSRect, usize, u32)],
    shown: NSRect,
    index: impl Fn(usize) -> usize,
    objects: &Attachments,
    setting: &crate::attachment::Setting,
) {
    if boxes.is_empty() {
        return;
    }
    objc2_app_kit::NSGraphicsContext::saveGraphicsState_class();
    crate::context::NSRectClip(shown);
    for &(rect, byte, attrs) in boxes {
        let Some(Some(dict)) = objects.get(attrs as usize) else { continue };
        // SAFETY: the key is a constant this crate exports.
        let Some(object) = dict.objectForKey(unsafe { NSAttachmentAttributeName }) else { continue };
        let at = crate::attachment::Drawn { rect, index: index(byte), attributes: Some(dict), view: None };
        crate::attachment::draw(&object, &at, setting, None);
    }
    objc2_app_kit::NSGraphicsContext::restoreGraphicsState_class();
}

/// The width text drawn at `place` is laid out in, if it has one.
fn width_of(place: Place) -> Option<f64> {
    match place {
        Place::Point(_) => None,
        Place::Rect(r) | Place::WithRect(r, _) => Some(r.size.width),
    }
}

/// Text a pass drew before it was laid out: the jobs to lay out, once for
/// each distinct text however often it was drawn, and where each drawing
/// goes.
#[derive(Default)]
pub(crate) struct Pending {
    jobs: Vec<layout::Job>,
    /// The first job with each key.
    by_key: HashMap<u64, usize, FxBuild>,
    places: Vec<Deferred>,
}

impl Pending {
    /// The job that lays `text` out as asked: one already pending if the
    /// pass drew the same text before (a label repeated down a table), or
    /// a new one.
    fn job_for(&mut self, text: &str, attrs: &[Attrs], runs: &[Run], opts: Options, key: u64) -> usize {
        let found = self.by_key.get(&key).copied();
        if let Some(at) = found
            && self.jobs[at].is(text, attrs, runs, &opts)
        {
            return at;
        }
        let at = self.jobs.len();
        self.jobs.push(layout::job(text.into(), attrs.into(), runs.into(), opts, key));
        if found.is_none() {
            self.by_key.insert(key, at);
        }
        at
    }
}

/// Text drawn before it was laid out: where its ops go, its job, and what
/// placing it needs.
struct Deferred {
    at: usize,
    job: usize,
    xf: Xf,
    clip: Rect,
    place: Place,
}

/// A pass's ops, with the text it deferred laid out and put in place.
pub(crate) fn finish(ops: Vec<Op>, pending: Pending) -> Vec<Op> {
    if pending.places.is_empty() {
        return ops;
    }
    let laid = layout::lay_out_all(pending.jobs);
    let extra: usize = pending.places.iter().map(|d| laid[d.job].runs.len() + laid[d.job].fills.len()).sum();
    let mut out = Vec::with_capacity(ops.len() + extra);
    let mut ops = ops.into_iter();
    let mut next = 0;
    for d in pending.places {
        out.extend(ops.by_ref().take(d.at - next));
        next = d.at;
        let laid = &laid[d.job];
        let (left, top, clip) = placement(d.xf, d.clip, d.place, laid);
        emit(&mut out, laid, left, top, clip);
    }
    out.extend(ops);
    out
}

/// Where laid-out text goes in the layer, and what it's clipped to, given
/// the view's transform and clip.
fn placement(xf: Xf, clip: Rect, place: Place, laid: &TextLayout) -> (f32, f32, Rect) {
    // The corner of the text's box nearest the view's origin: its top in a
    // flipped view, its bottom otherwise.
    let at_point = |p: NSPoint| {
        let (x, ya) = xf.point(p.x, p.y);
        let (_, yb) = xf.point(p.x, p.y + f64::from(laid.height));
        (x as f32, ya.min(yb) as f32, clip)
    };
    let lines = NSStringDrawingOptions::UsesLineFragmentOrigin;
    match place {
        Place::Point(p) => at_point(p),
        // Without a height, the rectangle's origin is a point to draw at,
        // as for drawAtPoint:.
        Place::WithRect(r, o) if o.contains(lines) && r.size.height <= 0.0 => at_point(r.origin),
        Place::Rect(r) | Place::WithRect(r, _) if matches!(place, Place::Rect(_)) || laid_in_lines(place) => {
            let area = xf.rect(r);
            let clip = if r.size.width > 0.0 {
                clip.intersect(&area)
            } else {
                clip.intersect(&Rect { x1: f32::INFINITY, ..area })
            };
            (area.x0, area.y0, clip)
        }
        // One line on a baseline at the origin.
        Place::Rect(r) | Place::WithRect(r, _) => {
            let (x, baseline) = xf.point(r.origin.x, r.origin.y);
            (x as f32, baseline as f32 - (laid.height - laid.first_descent), clip)
        }
    }
}

fn laid_in_lines(place: Place) -> bool {
    matches!(place, Place::WithRect(_, o) if o.contains(NSStringDrawingOptions::UsesLineFragmentOrigin))
}

/// The size of text with attribute runs, as `sizeWithAttributes:` or, with
/// options, `boundingRectWithSize:options:` measures it.
pub(crate) fn measure(
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    bounds: Option<(NSSize, NSStringDrawingOptions)>,
) -> NSRect {
    let opts = bounds.map_or(Options::UNBOUNDED, |(size, o)| options(size, o));
    let laid = layout::lay_out(text, attrs, runs, &opts);
    let y = if opts.all_lines { 0.0 } else { -laid.first_descent };
    NSRect::new(NSPoint::new(0.0, f64::from(y)), NSSize::new(f64::from(laid.width), f64::from(laid.height)))
}

fn limit(extent: f64) -> f32 {
    if extent > 0.0 { extent as f32 } else { f32::INFINITY }
}

fn options(size: NSSize, o: NSStringDrawingOptions) -> Options {
    let all_lines = o.contains(NSStringDrawingOptions::UsesLineFragmentOrigin);
    Options {
        width: limit(size.width),
        // One line on a baseline takes no notice of a height, as in AppKit.
        height: if all_lines { limit(size.height) } else { f32::INFINITY },
        all_lines,
        font_leading: o.contains(NSStringDrawingOptions::UsesFontLeading),
        truncate_last: o.contains(NSStringDrawingOptions::TruncatesLastVisibleLine),
        attachments_as_glyphs: false,
    }
}

/// Record `laid` with its top left corner at (`left`, `top`) in the layer.
fn emit(ops: &mut Vec<Op>, laid: &TextLayout, left: f32, top: f32, clip: Rect) {
    emit_parts(ops, &laid.runs, &laid.fills, left, top, clip);
}

/// Record glyph runs and fills placed from (`left`, `top`) in the layer:
/// backgrounds, then glyphs, then decorations.
fn emit_parts(ops: &mut Vec<Op>, runs: &[PlacedRun], fills: &[PlacedFill], left: f32, top: f32, clip: Rect) {
    if clip.is_empty() {
        return;
    }
    let fill = |ops: &mut Vec<Op>, rect: Rect, color| {
        let rect = rect.intersect(&clip);
        if !rect.is_empty() {
            ops.push(Op::Fill { rect, color });
        }
    };
    for f in fills.iter().filter(|f| f.background) {
        let [x0, y0, x1, y1] = f.rect;
        fill(ops, Rect::new(left + x0, top + y0, left + x1, top + y1), f.color);
    }
    for run in runs {
        let (x, y) = (left + run.x, top + run.y);
        // Glyphs reach at most about twice their size from the baseline.
        let reach = run.size * 2.0;
        if y - reach > clip.y1 || y + reach < clip.y0 {
            continue;
        }
        ops.push(Op::Glyphs(GlyphRun {
            font: run.font,
            size: run.size,
            x,
            y,
            glyphs: run.glyphs.clone(),
            color: run.color,
            clip,
        }));
    }
    for f in fills.iter().filter(|f| !f.background) {
        // Lines are drawn crisp: whole points, at least one thick.
        let [x0, y0, x1, y1] = f.rect;
        let y = (top + y0).round();
        let thickness = (y1 - y0).round().max(1.0);
        fill(ops, Rect::new(left + x0, y, left + x1, y + thickness), f.color);
    }
}

/// Record the lines of `frame` with its top left corner at `origin` in the
/// view being drawn (the corner at the top of the view as it shows, flipped
/// or not): what a layout manager draws. The lines in the clip are found by
/// halving, so a long text records as fast as the part of it in view.
// TextKit, which a later workstream builds, records its lines through
// this; until then only tests do.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn record_frame(frame: &Frame, origin: NSPoint) {
    with_recorder(|rec| {
        let (x, y) = rec.xf.point(origin.x, origin.y);
        let (left, top, clip) = (x as f32, y as f32, rec.clip);
        let first = frame.paragraphs.partition_point(|p| top + p.bottom() < clip.y0);
        for para in &frame.paragraphs[first..] {
            if top + para.top > clip.y1 {
                break;
            }
            record_lines(&mut rec.ops, &para.lines.lines, left, top + para.top, clip);
        }
    });
}

/// Record one laid-out line with its left edge and top at `origin` in the
/// view being drawn: its backgrounds, or its glyphs and decorations (a
/// layout manager's `drawBackgroundForGlyphRange:atPoint:` and
/// `drawGlyphsForGlyphRange:atPoint:`, between which a text view draws its
/// selection).
pub(crate) fn record_line(line: &Line, origin: NSPoint, backgrounds: bool) {
    with_recorder(|rec| {
        let (x, y) = rec.xf.point(origin.x, origin.y);
        let (left, top, clip) = (x as f32, y as f32, rec.clip);
        if top - line.height > clip.y1 || top + 2.0 * line.height < clip.y0 {
            return;
        }
        let fills: Vec<PlacedFill> = line.fills.iter().filter(|f| f.background == backgrounds).cloned().collect();
        let runs: &[PlacedRun] = if backgrounds { &[] } else { &line.runs };
        emit_parts(&mut rec.ops, runs, &fills, left, top, clip);
    });
}

/// The attachments' boxes of a laid-out line whose left edge and top are
/// at `origin` in the view being drawn (as [`record_line`] takes it): each
/// one's rectangle in the view's space, for drawing it, and the box.
pub(crate) fn line_boxes(line: &Line, origin: NSPoint) -> Vec<(NSRect, crate::text::lines::LineAttachment)> {
    if line.attachments.is_empty() {
        return Vec::new();
    }
    let Some(xf) = crate::context::with_state(|st| st.rec.xf) else { return Vec::new() };
    let (x, y) = xf.point(origin.x, origin.y);
    let (left, top) = (x as f32, y as f32);
    line.attachments
        .iter()
        .map(|a| {
            let r = Rect::new(left + a.rect[0], top + a.rect[1], left + a.rect[2], top + a.rect[3]);
            (xf.inverse_rect(r), *a)
        })
        .collect()
}

/// Record `lines`, the first's top at `top`: those in the clip.
#[cfg_attr(not(test), allow(dead_code))]
fn record_lines(ops: &mut Vec<Op>, lines: &[Line], left: f32, top: f32, clip: Rect) {
    let first = lines.partition_point(|l| top + l.top + l.height < clip.y0);
    for line in &lines[first..] {
        let y = top + line.top;
        if y > clip.y1 {
            break;
        }
        emit_parts(ops, &line.runs, &line.fills, left, y, clip);
    }
}

/// Attributes and UTF-16 spans of them for text whose attribute
/// dictionaries cover UTF-16 ranges in order, as an attributed string
/// enumerates them: what drawing, measuring or laying out an
/// `NSAttributedString` or a text storage takes. Ranges with the same
/// dictionary share their attributes.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn attribute_spans(ranges: &[(Range<u32>, Option<&Attributes>)]) -> (Vec<Attrs>, Vec<Span>) {
    let mut attrs: Vec<Attrs> = Vec::new();
    // By dictionary: a text storage with thousands of runs has thousands of
    // them, or a few shared.
    let mut seen: HashMap<*const Attributes, u32, FxBuild> = HashMap::default();
    let mut spans = Vec::with_capacity(ranges.len());
    for (range, dict) in ranges {
        let key = dict.map_or(std::ptr::null(), |d| d as *const Attributes);
        let index = *seen.entry(key).or_insert_with(|| {
            attrs.push(attrs_of(*dict));
            attrs.len() as u32 - 1
        });
        spans.push(Span { start: range.start, end: range.end, attrs: index });
    }
    if attrs.is_empty() {
        attrs.push(attrs_of(None));
    }
    (attrs, spans)
}

/// The attributes string drawing understands, read from a dictionary.
/// Numbers are read with `doubleValue`, so anything that answers it will
/// do.
pub(crate) fn attrs_of(dict: Option<&Attributes>) -> Attrs {
    let mut attrs = Attrs::new(default_font());
    let Some(dict) = dict else { return attrs };
    let mut left = dict.count();
    let mut get = |key: &NSString| {
        if left == 0 {
            return None;
        }
        let value = dict.objectForKey(key);
        left -= usize::from(value.is_some());
        value
    };
    let color = |value: Option<Retained<AnyObject>>| {
        value.and_then(|v| v.downcast::<NSColor>().ok()).map(|c| crate::color::resolve(&c))
    };
    let int = |value: Option<Retained<AnyObject>>| value.and_then(|v| number(&v)).map(|n| n as i64);
    // SAFETY: the keys are constants this crate exports.
    unsafe {
        if let Some(font) = get(NSFontAttributeName).and_then(|v| v.downcast::<NSFont>().ok()) {
            attrs.font = text_font(&font);
        }
        if let Some(c) = color(get(NSForegroundColorAttributeName)) {
            attrs.color = c;
        }
        if let Some(style) = get(NSParagraphStyleAttributeName).and_then(|v| v.downcast::<NSParagraphStyle>().ok()) {
            attrs.paragraph = paragraph_of(&style);
        }
        attrs.background = color(get(NSBackgroundColorAttributeName));
        attrs.underline.style = int(get(NSUnderlineStyleAttributeName)).unwrap_or(0);
        attrs.strikethrough.style = int(get(NSStrikethroughStyleAttributeName)).unwrap_or(0);
        attrs.kern = get(NSKernAttributeName).and_then(|v| number(&v)).map(|k| k as f32);
        attrs.baseline_offset = get(NSBaselineOffsetAttributeName).and_then(|v| number(&v)).unwrap_or(0.0) as f32;
        attrs.ligatures = int(get(NSLigatureAttributeName)).unwrap_or(1);
        attrs.underline.color = color(get(NSUnderlineColorAttributeName));
        attrs.strikethrough.color = color(get(NSStrikethroughColorAttributeName));
        attrs.stroke.width = get(NSStrokeWidthAttributeName).and_then(|v| number(&v)).unwrap_or(0.0) as f32;
        attrs.stroke.color = color(get(NSStrokeColorAttributeName));
        #[allow(deprecated)]
        let obliqueness = NSObliquenessAttributeName;
        attrs.obliqueness = get(obliqueness).and_then(|v| number(&v)).unwrap_or(0.0) as f32;
        attrs.shadow = get(NSShadowAttributeName).and_then(|v| shadow_of(&v));
        if let Some(value) = get(NSAttachmentAttributeName) {
            let (fm, size) = (&attrs.font.face.metrics, attrs.font.size);
            let ascent = f64::from((fm.ascent * size).round());
            let line = ascent + f64::from((-fm.descent * size).round());
            attrs.attachment = crate::attachment::metrics(&value, Some(dict), ascent, line);
        }
    }
    attrs
}

/// A shadow from an `NSShadowAttributeName` value: an object whose
/// `shadowOffset`, `shadowBlurRadius` and `shadowColor` have `NSShadow`'s
/// signatures, which are checked first, since the value can be any object.
/// A shadow whose color is nil (or not a color) casts nothing, as in
/// AppKit; a new `NSShadow`'s color, black at a third, comes from its own
/// getter.
fn shadow_of(value: &AnyObject) -> Option<Shadow> {
    let class = value.class();
    let signed = class.verify_sel::<(), NSSize>(sel!(shadowOffset)).is_ok()
        && class.verify_sel::<(), f64>(sel!(shadowBlurRadius)).is_ok()
        && class.verify_sel::<(), *mut AnyObject>(sel!(shadowColor)).is_ok();
    if !signed {
        return None;
    }
    // SAFETY: the receiver's class has the three methods, with these
    // argument and return types (checked above).
    let (offset, blur, color) = unsafe {
        let offset: NSSize = msg_send![value, shadowOffset];
        let blur: f64 = msg_send![value, shadowBlurRadius];
        let color: Option<Retained<AnyObject>> = msg_send![value, shadowColor];
        (offset, blur, color)
    };
    let color = color?.downcast::<NSColor>().ok()?;
    let color = crate::color::resolve(&color);
    Some(Shadow { offset: [offset.width as f32, offset.height as f32], blur: blur as f32, color })
}

/// Text without a font attribute is drawn in the 12-point interface font.
fn default_font() -> TextFont {
    thread_local!(static DEFAULT: RefCell<Option<TextFont>> = const { RefCell::new(None) });
    DEFAULT.with(|d| {
        d.borrow_mut()
            .get_or_insert_with(|| TextFont {
                face: fonts::resolve(&FontSpec::system(Design::Default, 12.0)),
                size: 12.0,
                tabular_digits: false,
                features: None,
            })
            .clone()
    })
}

fn this_string<T>(this: &T) -> String {
    // SAFETY: these methods are installed on NSString and only ever run with
    // a string as the receiver.
    unsafe { &*(this as *const T).cast::<NSString>() }.to_string()
}

/// One run of attributes over all of `text`.
fn whole(text: &str) -> [Run; 1] {
    [Run { start: 0, end: text.len(), attrs: 0 }]
}

/// `dict`, if its attributes have an attachment's box (see
/// [`Attachments`]).
pub(crate) fn attachment_in(dict: Option<&Attributes>, attrs: &Attrs) -> [Option<Retained<Attributes>>; 1] {
    [attrs.attachment.and(dict).map(objc2::Message::retain)]
}

/// A string's attributes, worked out for text laid out `width` wide (for
/// what an attachment's methods are told), its first character's.
fn attrs_in(dict: Option<&Attributes>, width: Option<f64>) -> Attrs {
    let setting = crate::attachment::Setting::drawing(width);
    crate::attachment::in_setting(setting, || crate::attachment::at_index(0, || attrs_of(dict)))
}

fn draw_string<T>(this: &T, attrs: Option<&Attributes>, place: Place) {
    let text = this_string(this);
    let resolved = attrs_in(attrs, width_of(place));
    let objects = attachment_in(attrs, &resolved);
    draw_with(&text, &[resolved], &whole(&text), place, &objects);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringDrawing"]
    struct StringDrawing;

    impl StringDrawing {
        #[unsafe(method(drawAtPoint:withAttributes:))]
        fn draw_at_point(&self, point: NSPoint, attrs: Option<&Attributes>) {
            draw_string(self, attrs, Place::Point(point));
        }

        #[unsafe(method(drawInRect:withAttributes:))]
        fn draw_in_rect(&self, rect: NSRect, attrs: Option<&Attributes>) {
            draw_string(self, attrs, Place::Rect(rect));
        }

        #[unsafe(method(drawWithRect:options:attributes:context:))]
        fn draw_with_rect(
            &self,
            rect: NSRect,
            options: NSStringDrawingOptions,
            attrs: Option<&Attributes>,
            context: Option<&NSStringDrawingContext>,
        ) {
            let text = this_string(self);
            let resolved = [attrs_in(attrs, Some(rect.size.width))];
            let objects = attachment_in(attrs, &resolved[0]);
            draw_with(&text, &resolved, &whole(&text), Place::WithRect(rect, options), &objects);
            report(context, || measure(&text, &resolved, &whole(&text), Some((rect.size, options))));
        }

        #[unsafe(method(drawWithRect:options:attributes:))]
        fn draw_with_rect_no_context(&self, rect: NSRect, options: NSStringDrawingOptions, attrs: Option<&Attributes>) {
            draw_string(self, attrs, Place::WithRect(rect, options));
        }

        #[unsafe(method(sizeWithAttributes:))]
        fn size_with_attributes(&self, attrs: Option<&Attributes>) -> NSSize {
            let text = this_string(self);
            measure(&text, &[attrs_in(attrs, None)], &whole(&text), None).size
        }

        #[unsafe(method(boundingRectWithSize:options:attributes:context:))]
        fn bounding_rect(
            &self,
            size: NSSize,
            options: NSStringDrawingOptions,
            attrs: Option<&Attributes>,
            context: Option<&NSStringDrawingContext>,
        ) -> NSRect {
            let text = this_string(self);
            let bounds = measure(&text, &[attrs_in(attrs, Some(size.width))], &whole(&text), Some((size, options)));
            report(context, || bounds);
            bounds
        }

        #[unsafe(method(boundingRectWithSize:options:attributes:))]
        fn bounding_rect_no_context(
            &self,
            size: NSSize,
            options: NSStringDrawingOptions,
            attrs: Option<&Attributes>,
        ) -> NSRect {
            let text = this_string(self);
            measure(&text, &[attrs_in(attrs, Some(size.width))], &whole(&text), Some((size, options)))
        }
    }
);

// NSString's drawing methods, as AppKit's NSStringDrawing category adds
// them.
sidestep_runtime::category!("NSString"(NSStringDrawing), |category| {
    // SAFETY: the helper's methods treat their receiver as an NSString.
    unsafe { category.add_methods_of(StringDrawing::class()) };
});

/// An attributed string's text, its attributes (one per distinct
/// dictionary) and its runs over the text's bytes: what [`draw`] and
/// [`measure`] take.
pub(crate) struct Parts {
    pub text: String,
    pub attrs: Vec<Attrs>,
    pub runs: Vec<Run>,
    /// The attachments of the attributes with one (see [`Attachments`]).
    pub attachments: Vec<Option<Retained<Attributes>>>,
}

/// The parts of `string`, read through its primitives (so a subclass with
/// text of its own, such as a text storage, is read as it answers), for
/// string drawing given `width` or none (what attachments are told).
pub(crate) fn attributed_parts(string: &NSAttributedString, width: Option<f64>) -> Parts {
    crate::attachment::in_setting(crate::attachment::Setting::drawing(width), || parts_of(string))
}

fn parts_of(string: &NSAttributedString) -> Parts {
    sidestep_foundation::with_runs(string, |text, refs| {
        let mut attrs: Vec<Attrs> = Vec::new();
        let mut attachments = Vec::new();
        let mut seen: HashMap<*const Attributes, u32, FxBuild> = HashMap::default();
        let mut runs = Vec::with_capacity(refs.len());
        for r in refs.iter().filter(|r| !r.utf8.is_empty()) {
            let index = *seen.entry(Retained::as_ptr(&r.attrs)).or_insert_with(|| {
                let a = crate::attachment::at_index(r.utf16.start, || attrs_of(Some(&r.attrs)));
                let [object] = attachment_in(Some(&r.attrs), &a);
                attrs.push(a);
                attachments.push(object);
                attrs.len() as u32 - 1
            });
            runs.push(Run { start: r.utf8.start, end: r.utf8.end, attrs: index });
        }
        if runs.is_empty() {
            attrs.push(attrs_of(None));
            attachments.push(None);
            runs.push(Run { start: 0, end: text.len(), attrs: 0 });
        }
        Parts { text: text.to_owned(), attrs, runs, attachments }
    })
}

fn this_attributed<T>(this: &T, width: Option<f64>) -> Parts {
    attributed_parts(crate::rich::receiver(this), width)
}

impl Parts {
    fn draw(&self, place: Place) {
        draw_with(&self.text, &self.attrs, &self.runs, place, &self.attachments);
    }

    fn measure(&self, bounds: Option<(NSSize, NSStringDrawingOptions)>) -> NSRect {
        measure(&self.text, &self.attrs, &self.runs, bounds)
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepAttributedStringDrawing"]
    struct AttributedStringDrawing;

    impl AttributedStringDrawing {
        #[unsafe(method(size))]
        fn size(&self) -> NSSize {
            this_attributed(self, None).measure(None).size
        }

        #[unsafe(method(drawAtPoint:))]
        fn draw_at_point(&self, point: NSPoint) {
            this_attributed(self, None).draw(Place::Point(point));
        }

        #[unsafe(method(drawInRect:))]
        fn draw_in_rect(&self, rect: NSRect) {
            this_attributed(self, Some(rect.size.width)).draw(Place::Rect(rect));
        }

        #[unsafe(method(drawWithRect:options:context:))]
        fn draw_with_rect(&self, rect: NSRect, options: NSStringDrawingOptions, context: Option<&NSStringDrawingContext>) {
            let parts = this_attributed(self, Some(rect.size.width));
            parts.draw(Place::WithRect(rect, options));
            report(context, || parts.measure(Some((rect.size, options))));
        }

        #[unsafe(method(drawWithRect:options:))]
        fn draw_with_rect_no_context(&self, rect: NSRect, options: NSStringDrawingOptions) {
            this_attributed(self, Some(rect.size.width)).draw(Place::WithRect(rect, options));
        }

        #[unsafe(method(boundingRectWithSize:options:context:))]
        fn bounding_rect(
            &self,
            size: NSSize,
            options: NSStringDrawingOptions,
            context: Option<&NSStringDrawingContext>,
        ) -> NSRect {
            let bounds = this_attributed(self, Some(size.width)).measure(Some((size, options)));
            report(context, || bounds);
            bounds
        }

        #[unsafe(method(boundingRectWithSize:options:))]
        fn bounding_rect_no_context(&self, size: NSSize, options: NSStringDrawingOptions) -> NSRect {
            this_attributed(self, Some(size.width)).measure(Some((size, options)))
        }
    }
);

// NSAttributedString's drawing methods, as AppKit's NSStringDrawing
// category adds them. Foundation's class registers first; this attaches to
// it at link time, whatever loads first.
sidestep_runtime::category!("NSAttributedString"(NSStringDrawing), |category| {
    // SAFETY: the helper's methods treat their receiver as an attributed
    // string.
    unsafe { category.add_methods_of(AttributedStringDrawing::class()) };
});

sidestep_runtime::static_class!(pub NSSTRINGDRAWINGCONTEXT, NSSTRINGDRAWINGCONTEXT_META = "NSStringDrawingContext", || {
    let _ = NSStringDrawingContextImpl::class();
});

pub(crate) struct ContextIvars {
    minimum_scale: Cell<f64>,
    actual_scale: Cell<f64>,
    total_bounds: Cell<NSRect>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a context is used
    // by one thread at a time.
    #[unsafe(super(NSObject))]
    #[name = "NSStringDrawingContext"]
    #[ivars = ContextIvars]
    pub(crate) struct NSStringDrawingContextImpl;

    impl NSStringDrawingContextImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // A new context has measured nothing: no scale, no bounds.
            let this = this.set_ivars(ContextIvars {
                minimum_scale: Cell::new(0.0),
                actual_scale: Cell::new(0.0),
                total_bounds: Cell::new(NSRect::ZERO),
            });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(minimumScaleFactor))]
        fn minimum_scale_factor(&self) -> f64 {
            self.ivars().minimum_scale.get()
        }

        #[unsafe(method(setMinimumScaleFactor:))]
        fn set_minimum_scale_factor(&self, factor: f64) {
            self.ivars().minimum_scale.set(factor);
        }

        #[unsafe(method(actualScaleFactor))]
        fn actual_scale_factor(&self) -> f64 {
            self.ivars().actual_scale.get()
        }

        #[unsafe(method(totalBounds))]
        fn total_bounds(&self) -> NSRect {
            self.ivars().total_bounds.get()
        }
    }

    unsafe impl NSObjectProtocol for NSStringDrawingContextImpl {}
);

/// Tell a drawing context what was measured or drawn: its bounds (as
/// `boundingRect…` gives them for the same size and options) and a scale of
/// 1, since text isn't shrunk to fit. Nothing for no context, or an object
/// that isn't one.
fn report(context: Option<&NSStringDrawingContext>, bounds: impl FnOnce() -> NSRect) {
    let Some(context) = context else { return };
    let object: &AnyObject = context;
    let Some(ours) = object.downcast_ref::<NSStringDrawingContextImpl>() else { return };
    ours.ivars().total_bounds.set(bounds());
    ours.ivars().actual_scale.set(1.0);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_objects::number;
    use crate::text::layout::Align;

    #[test]
    fn attributes_are_read_from_the_dictionary() {
        let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
        let blue = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0);
        let (thick, kern, raise, none) = (number(2.0), number(1.5), number(3.0), number(0.0));
        // SAFETY: the keys are constant strings.
        let keys = unsafe {
            [
                NSForegroundColorAttributeName,
                NSBackgroundColorAttributeName,
                NSUnderlineStyleAttributeName,
                NSUnderlineColorAttributeName,
                NSStrikethroughStyleAttributeName,
                NSKernAttributeName,
                NSBaselineOffsetAttributeName,
                NSLigatureAttributeName,
            ]
        };
        let values: [&AnyObject; 8] = [&red, &blue, &thick, &blue, &thick, &kern, &raise, &none];
        let attrs = attrs_of(Some(&NSDictionary::from_slices(&keys, &values)));
        assert_eq!(attrs.color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(attrs.background, Some([0.0, 0.0, 1.0, 1.0]));
        assert_eq!((attrs.underline.style, attrs.underline.color), (2, Some([0.0, 0.0, 1.0, 1.0])));
        assert_eq!((attrs.strikethrough.style, attrs.strikethrough.color), (2, None));
        assert_eq!((attrs.kern, attrs.baseline_offset, attrs.ligatures), (Some(1.5), 3.0, 0));
        let plain = attrs_of(None);
        assert_eq!((plain.color, plain.kern, plain.ligatures), ([0.0, 0.0, 0.0, 1.0], None, 1));
        assert_eq!(plain.font.size, 12.0, "text without a font is 12-point");
    }

    fn record(f: impl FnOnce()) -> Vec<Op> {
        graphics::begin_recording();
        graphics::set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, 800.0, 3000.0));
        f();
        graphics::end_recording()
    }

    fn text_at(text: &str, y: f64) {
        draw(text, &[attrs_of(None)], &whole(text), Place::Point(NSPoint::new(0.0, y)));
    }

    #[test]
    fn points_are_the_corner_nearest_the_origin() {
        let text = "corner";
        let laid = layout::lay_out(text, &[attrs_of(None)], &whole(text), &Options::UNBOUNDED);
        let baseline = laid.height - laid.first_descent;
        for (xf, top) in [
            // A flipped view: the point is the text's top left corner.
            (Xf::IDENTITY, 10.0),
            // An unflipped one, 800 points tall: its bottom left corner.
            (Xf { tx: 0.0, a: -1.0, ty: 800.0 }, 790.0 - laid.height),
        ] {
            graphics::begin_recording();
            graphics::set_view(xf, Rect::new(0.0, 0.0, 800.0, 800.0));
            draw(text, &[attrs_of(None)], &whole(text), Place::Point(NSPoint::new(5.0, 10.0)));
            let ops = graphics::end_recording();
            let Some(Op::Glyphs(run)) = ops.first() else { panic!("glyphs") };
            assert_eq!((run.x, run.y), (5.0, top + baseline));
            // drawWithRect: without usesLineFragmentOrigin puts the baseline
            // at the rectangle's origin, whichever way the view faces.
            graphics::begin_recording();
            graphics::set_view(xf, Rect::new(0.0, 0.0, 800.0, 800.0));
            let r = NSRect::new(NSPoint::new(5.0, 10.0), NSSize::new(100.0, 20.0));
            draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, NSStringDrawingOptions(0)));
            let ops = graphics::end_recording();
            let Some(Op::Glyphs(run)) = ops.first() else { panic!("glyphs") };
            assert_eq!(run.y, xf.point(0.0, 10.0).1 as f32);
        }
    }

    #[test]
    fn text_laid_out_later_keeps_its_place() {
        let fill = Op::Fill { rect: Rect::new(0.0, 0.0, 10.0, 10.0), color: [1.0, 0.0, 0.0, 1.0] };
        let ops = record(|| {
            text_at("deferred, first", 0.0);
            graphics::push(fill.clone());
            text_at("deferred, second", 100.0);
        });
        let kinds: Vec<_> = ops.iter().map(|op| matches!(op, Op::Glyphs(_))).collect();
        assert_eq!(kinds, [true, false, true]);
        let (Op::Glyphs(a), Op::Glyphs(b)) = (&ops[0], &ops[2]) else { unreachable!() };
        assert!(b.y > a.y + 90.0);
        // Drawn again, the text is found laid out and recorded at once.
        let again = record(|| text_at("deferred, first", 0.0));
        let Op::Glyphs(c) = &again[0] else { panic!("glyphs") };
        assert!(Arc::ptr_eq(&a.glyphs, &c.glyphs) && (a.x, a.y) == (c.x, c.y));
    }

    fn glyph_runs(ops: &[Op]) -> Vec<&GlyphRun> {
        ops.iter().filter_map(|op| if let Op::Glyphs(run) = op { Some(run) } else { None }).collect()
    }

    fn with_style(set: impl FnOnce(&mut layout::Paragraph)) -> Attrs {
        let mut attrs = attrs_of(None);
        set(&mut attrs.paragraph);
        attrs
    }

    /// A flipped view and an unflipped one, 800 points tall, and the clip
    /// of each.
    fn views() -> [(Xf, Rect); 2] {
        let bounds = Rect::new(0.0, 0.0, 800.0, 800.0);
        [(Xf::IDENTITY, bounds), (Xf { tx: 0.0, a: -1.0, ty: 800.0 }, bounds)]
    }

    fn record_in(xf: Xf, clip: Rect, f: impl FnOnce()) -> Vec<Op> {
        graphics::begin_recording();
        graphics::set_view(xf, clip);
        f();
        graphics::end_recording()
    }

    #[test]
    fn rectangles_align_and_clip_their_text() {
        let text = "Hello";
        let r = NSRect::new(NSPoint::new(20.0, 30.0), NSSize::new(300.0, 40.0));
        let plain = layout::lay_out(text, &[attrs_of(None)], &whole(text), &Options::UNBOUNDED);
        let baseline = plain.height - plain.first_descent;
        for (xf, bounds) in views() {
            let area = xf.rect(r);
            for (align, x) in [
                (Align::Left, 20.0),
                (Align::Natural, 20.0),
                (Align::Right, 320.0 - plain.width),
                (Align::Center, 20.0 + (300.0 - plain.width) / 2.0),
            ] {
                let a = with_style(|p| p.alignment = align);
                let ops = record_in(xf, bounds, || draw(text, std::slice::from_ref(&a), &whole(text), Place::Rect(r)));
                let runs = glyph_runs(&ops);
                assert_eq!(runs.len(), 1, "{align:?}");
                assert!((runs[0].x - x).abs() < 0.01, "{align:?}: {} vs {x}", runs[0].x);
                assert_eq!(
                    runs[0].y,
                    area.y0 + baseline,
                    "the text's top is the rectangle's, whichever way the view faces"
                );
                assert_eq!(runs[0].clip, area.intersect(&bounds), "clipped to the rectangle");
            }
        }
        // Justified lines but the last reach across the rectangle.
        let long = "The quick brown fox jumps over the lazy dog and keeps on running far away from here";
        let a = with_style(|p| p.alignment = Align::Justified);
        let ops = record_in(Xf::IDENTITY, views()[0].1, || draw(long, &[a], &whole(long), Place::Rect(r)));
        let runs = glyph_runs(&ops);
        let first_line: Vec<_> = runs.iter().filter(|run| run.y == runs[0].y).collect();
        let end = first_line.iter().map(|run| run.x + run.glyphs.last().map_or(0.0, |g| g.x)).fold(0.0, f32::max);
        assert!(end > 300.0 && end <= 320.01, "the first line is spread to the right edge ({end})");
        assert!(runs.iter().any(|run| run.y > runs[0].y), "and wraps");
    }

    #[test]
    fn lines_past_the_rectangle_are_clipped_not_dropped() {
        let line = layout::lay_out("A", &[attrs_of(None)], &whole("A"), &Options::UNBOUNDED).height;
        let r = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, f64::from(line) * 1.5));
        let text = "First\nSecond";
        let ops = record_in(Xf::IDENTITY, views()[0].1, || draw(text, &[attrs_of(None)], &whole(text), Place::Rect(r)));
        let runs = glyph_runs(&ops);
        assert_eq!(runs.len(), 2, "both lines are drawn");
        assert!(runs[1].y > runs[0].y && runs[1].clip.y1 == line * 1.5, "the second is cut at the rectangle");
    }

    #[test]
    fn drawing_with_line_fragments_places_like_a_rectangle() {
        let text = "Hello\nWorld";
        let line = layout::lay_out("A", &[attrs_of(None)], &whole("A"), &Options::UNBOUNDED).height;
        let lfo = NSStringDrawingOptions::UsesLineFragmentOrigin;
        let truncate = lfo | NSStringDrawingOptions::TruncatesLastVisibleLine;
        let dots = layout::lay_out("…", &[attrs_of(None)], &whole("…"), &Options::UNBOUNDED).runs[0].glyphs[0].id;
        for (xf, bounds) in views() {
            let r = NSRect::new(NSPoint::new(10.0, 50.0), NSSize::new(300.0, 100.0));
            let rect = record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::Rect(r)));
            let with = record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, lfo)));
            let (rect, with) = (glyph_runs(&rect), glyph_runs(&with));
            assert_eq!(rect.len(), 2);
            assert!(rect.iter().zip(&with).all(|(a, b)| (a.x, a.y, a.clip) == (b.x, b.y, b.clip)));

            // With truncatesLastVisibleLine, only lines that fit are drawn,
            // the last with an ellipsis if text was cut off...
            let r = NSRect::new(NSPoint::new(10.0, 50.0), NSSize::new(300.0, f64::from(line) * 1.5));
            let ops =
                record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, truncate)));
            let runs = glyph_runs(&ops);
            assert!(runs.iter().all(|run| run.y == runs[0].y), "one line");
            assert_eq!(runs.last().and_then(|run| run.glyphs.last()).map(|g| g.id), Some(dots));
            // ...and the first line always, clipped, however short the
            // rectangle.
            let r = NSRect::new(NSPoint::new(10.0, 50.0), NSSize::new(300.0, 5.0));
            let ops =
                record_in(xf, bounds, || draw(text, &[attrs_of(None)], &whole(text), Place::WithRect(r, truncate)));
            let runs = glyph_runs(&ops);
            assert!(!runs.is_empty(), "the first line is drawn");
            assert_eq!(runs[0].clip, xf.rect(r));
        }
    }

    #[test]
    fn the_same_new_text_drawn_again_is_laid_out_once() {
        let text = "a label repeated down a table";
        let before = crate::text::with_ctx(|ctx| ctx.layouts.len());
        let ops = record(|| (0..10).for_each(|i| text_at(text, f64::from(i) * 20.0)));
        let runs = glyph_runs(&ops);
        assert_eq!(runs.len(), 10);
        assert!(runs.windows(2).all(|w| Arc::ptr_eq(&w[0].glyphs, &w[1].glyphs) && w[1].y > w[0].y));
        assert_eq!(crate::text::with_ctx(|ctx| ctx.layouts.len()), before + 1, "cached once");
    }

    #[test]
    fn many_lines_are_laid_out_together_as_one_by_one() {
        let lines: Vec<String> =
            (0..40).map(|i| format!("line {i} of a page laid out on the pool's threads")).collect();
        let ops = record(|| lines.iter().enumerate().for_each(|(i, l)| text_at(l, i as f64 * 20.0)));
        assert_eq!(ops.len(), 40);
        for (op, line) in ops.iter().zip(&lines) {
            let Op::Glyphs(run) = op else { panic!("glyphs") };
            let alone = layout::lay_out(line, &[attrs_of(None)], &whole(line), &Options::UNBOUNDED);
            assert_eq!(*run.glyphs, *alone.runs[0].glyphs, "{line}");
        }
    }

    // A stand-in for NSShadow, which the drawing workstream is adding:
    // string drawing reads shadows through these messages.
    objc2::define_class!(
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "SidestepTestShadow"]
        struct TestShadow;

        impl TestShadow {
            #[unsafe(method(shadowOffset))]
            fn offset(&self) -> NSSize {
                NSSize::new(2.0, 3.0)
            }

            #[unsafe(method(shadowBlurRadius))]
            fn blur(&self) -> f64 {
                1.5
            }

            #[unsafe(method_id(shadowColor))]
            fn color(&self) -> Option<Retained<NSColor>> {
                Some(NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 0.5))
            }
        }
    );

    // A shadow whose color was set to nil, which AppKit doesn't draw.
    objc2::define_class!(
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "SidestepTestColorlessShadow"]
        struct ColorlessShadow;

        impl ColorlessShadow {
            #[unsafe(method(shadowOffset))]
            fn offset(&self) -> NSSize {
                NSSize::new(2.0, 3.0)
            }

            #[unsafe(method(shadowBlurRadius))]
            fn blur(&self) -> f64 {
                0.0
            }

            #[unsafe(method_id(shadowColor))]
            fn color(&self) -> Option<Retained<NSColor>> {
                None
            }
        }
    );

    // Methods of the right names with other signatures, which mustn't be
    // sent as NSShadow's.
    objc2::define_class!(
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "SidestepTestOddShadow"]
        struct OddShadow;

        impl OddShadow {
            #[unsafe(method(shadowOffset))]
            fn offset(&self) -> f64 {
                2.0
            }

            #[unsafe(method(shadowBlurRadius))]
            fn blur(&self) -> f64 {
                0.0
            }

            #[unsafe(method_id(shadowColor))]
            fn color(&self) -> Option<Retained<NSColor>> {
                Some(NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 0.5))
            }
        }
    );

    #[allow(deprecated)]
    #[test]
    fn strokes_slants_and_shadows_are_read_and_drawn() {
        use objc2::AnyThread;
        // SAFETY: NSObject's designated initializer.
        let shadow: Retained<TestShadow> = unsafe { msg_send![TestShadow::alloc(), init] };
        let (width, oblique, red) =
            (number(3.0), number(0.25), NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0));
        // SAFETY: the keys are constant strings.
        let keys = unsafe {
            [NSStrokeWidthAttributeName, NSStrokeColorAttributeName, NSObliquenessAttributeName, NSShadowAttributeName]
        };
        let values: [&AnyObject; 4] = [&width, &red, &oblique, &shadow];
        let attrs = attrs_of(Some(&NSDictionary::from_slices(&keys, &values)));
        assert_eq!((attrs.stroke.width, attrs.stroke.color), (3.0, Some([1.0, 0.0, 0.0, 1.0])));
        assert_eq!(attrs.obliqueness, 0.25);
        assert_eq!(attrs.shadow, Some(Shadow { offset: [2.0, 3.0], blur: 1.5, color: [0.0, 0.0, 1.0, 0.5] }));
        // No color, no shadow; and nothing is sent to methods without
        // NSShadow's signatures, or to what isn't a shadow at all.
        let shadow_from = |value: &AnyObject| {
            // SAFETY: the key is a constant string.
            let key = unsafe { NSShadowAttributeName };
            attrs_of(Some(&NSDictionary::from_slices(&[key], &[value]))).shadow
        };
        // SAFETY: NSObject's designated initializer.
        let colorless: Retained<ColorlessShadow> = unsafe { msg_send![ColorlessShadow::alloc(), init] };
        // SAFETY: as above.
        let odd: Retained<OddShadow> = unsafe { msg_send![OddShadow::alloc(), init] };
        assert_eq!(shadow_from(&colorless), None);
        assert_eq!(shadow_from(&odd), None);
        assert_eq!(shadow_from(&red), None);

        let text = "Outline";
        let plain = layout::lay_out(text, &[attrs_of(None)], &whole(text), &Options::UNBOUNDED);
        let laid = layout::lay_out(text, std::slice::from_ref(&attrs), &whole(text), &Options::UNBOUNDED);
        // The shadow first, moved right and up, in its color; then the
        // outline alone (a positive width), in the stroke color.
        assert_eq!(laid.runs.len(), 2);
        let (shade, outline) = (&laid.runs[0], &laid.runs[1]);
        assert_eq!(shade.color, [0.0, 0.0, 1.0, 0.5]);
        assert_eq!((shade.x - outline.x, outline.y - shade.y), (2.0, 3.0));
        assert_eq!(outline.color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!((laid.width, laid.height), (plain.width, plain.height), "none of it takes room");
        let face = crate::text::fonts::face_data(outline.font).unwrap();
        assert!((face.stroke - 0.03).abs() < 1e-6, "3% of the size ({})", face.stroke);
        assert!((face.skew - 0.25f32.atan().to_degrees()).abs() <= 0.05, "to a tenth of a degree");
        assert_ne!(outline.font, plain.runs[0].font);
        // A negative width fills and strokes: the plain glyphs in the text's
        // color, then the outline.
        let filled = Attrs { stroke: layout::Stroke { width: -3.0, color: None }, shadow: None, ..attrs };
        let laid = layout::lay_out(text, &[filled], &whole(text), &Options::UNBOUNDED);
        assert_eq!(laid.runs.len(), 2);
        assert_eq!((laid.runs[0].color, laid.runs[1].color), ([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]));
        let (fill, stroke) = (
            crate::text::fonts::face_data(laid.runs[0].font).unwrap(),
            crate::text::fonts::face_data(laid.runs[1].font).unwrap(),
        );
        assert_eq!((fill.stroke, stroke.stroke > 0.0), (0.0, true));
    }

    #[test]
    fn underline_patterns_and_words() {
        let text = "two words";
        let with = |style: i64| {
            let mut a = attrs_of(None);
            a.underline.style = style;
            layout::lay_out(text, &[a], &whole(text), &Options::UNBOUNDED)
        };
        let lines = |l: &TextLayout| l.fills.iter().filter(|f| !f.background).map(|f| f.rect).collect::<Vec<_>>();
        let solid = lines(&with(1));
        assert_eq!(solid.len(), 1);
        let (x0, x1) = (solid[0][0], solid[0][2]);
        // Dots: many short pieces within the solid line's reach.
        let dots = lines(&with(1 | 0x100));
        assert!(dots.len() > 5, "{}", dots.len());
        assert!(dots.iter().all(|r| r[0] >= x0 - 0.01 && r[2] <= x1 + 0.01 && r[2] - r[0] <= 1.01));
        let dashes = lines(&with(1 | 0x200));
        assert!(dashes.len() < dots.len() && dashes.iter().any(|r| r[2] - r[0] > 3.0));
        // By word: the space between the words is left out.
        let words = lines(&with(1 | 0x8000));
        assert_eq!(words.len(), 2);
        assert!(words[0][2] < words[1][0]);
        assert!((words[0][0] - x0).abs() < 0.01 && (words[1][2] - x1).abs() < 0.01);
        // Double lines stay double in pieces.
        assert_eq!(lines(&with(9 | 0x8000)).len(), 4);
    }

    #[test]
    fn frames_record_the_lines_in_view() {
        use crate::text::lines::{Container, Frame, Styled};
        let text: String = (0..200).map(|i| format!("line {i} of a text view\n")).collect();
        let (attrs, spans) = attribute_spans(&[(0..text.encode_utf16().count() as u32, None)]);
        let frame = Frame::new(
            Styled { text: &text, attrs: &attrs, spans: &spans },
            Container { width: 300.0, ..Container::UNBOUNDED },
        );
        let line = frame.paragraphs[0].lines.lines[0].clone();
        assert!(line.ascent > 0.0 && line.descent > 0.0 && line.leading >= 0.0 && line.fills.is_empty());
        let row = line.height;
        // Scrolled so that lines 100 to 104 or so are in view.
        let ops = record_in(Xf::IDENTITY, Rect::new(0.0, 0.0, 800.0, row * 4.5), || {
            record_frame(&frame, NSPoint::new(10.0, -f64::from(row) * 100.0))
        });
        let runs = glyph_runs(&ops);
        assert!((5..=7).contains(&runs.len()), "only the lines in view ({})", runs.len());
        let at = |i: usize| frame.lines().nth(i).unwrap();
        let hundred = at(100);
        assert_eq!((hundred.paragraph, hundred.index), (100, 0));
        let expect = &hundred.line.runs[0];
        assert!(runs.iter().any(|r| r.x == 10.0 + expect.x && Arc::ptr_eq(&r.glyphs, &expect.glyphs)));
        assert!(!frame.is_empty());
    }

    #[test]
    fn attributed_strings_draw_their_runs() {
        use objc2_foundation::{NSMutableAttributedString, NSRange};
        let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
        let string = NSMutableAttributedString::from_nsstring(&NSString::from_str("aé😀 plain red"));
        // SAFETY: a constant key and a color, over a range in the text.
        unsafe { string.addAttribute_value_range(NSForegroundColorAttributeName, &red, NSRange::new(11, 3)) };
        let parts = attributed_parts(&string, None);
        assert_eq!(parts.text, "aé😀 plain red");
        // One set of attributes per dictionary, the runs over bytes.
        assert_eq!(parts.attrs.len(), 2);
        let bounds: Vec<_> = parts.runs.iter().map(|r| (r.start, r.end)).collect();
        assert_eq!(bounds, [(0, 14), (14, 17)]);
        assert_eq!(parts.attrs[parts.runs[1].attrs as usize].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(parts.attrs[0].font.size, 12.0, "no font is the 12-point default");
        // Drawn, the red run is red; empty text still has a run.
        let ops = record(|| parts.draw(Place::Point(NSPoint::new(0.0, 0.0))));
        assert!(glyph_runs(&ops).iter().any(|r| r.color == [1.0, 0.0, 0.0, 1.0]));
        let empty = attributed_parts(&NSMutableAttributedString::from_nsstring(&NSString::from_str("")), None);
        assert_eq!((empty.runs.len(), empty.attrs.len()), (1, 1));
        assert_eq!(empty.measure(None).size.height, parts.measure(None).size.height);
    }

    #[test]
    fn attributed_ranges_share_their_attributes() {
        let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
        // SAFETY: the key is a constant string.
        let dict = NSDictionary::from_slices(&[unsafe { NSForegroundColorAttributeName }], &[&*red as &AnyObject]);
        let (attrs, spans) = attribute_spans(&[(0..3, Some(&dict)), (3..5, None), (5..9, Some(&dict))]);
        assert_eq!(attrs.len(), 2);
        assert_eq!(spans.iter().map(|s| s.attrs).collect::<Vec<_>>(), [0, 1, 0]);
        assert_eq!(attrs[0].color, [1.0, 0.0, 0.0, 1.0]);
        // As byte runs, for drawing: "é" is two bytes and one UTF-16 unit,
        // "😀" four bytes and two units. A range ending inside it gives it
        // to the next.
        let text = "aé😀bcdef";
        let runs = crate::text::lines::runs_of(text, &spans);
        let bounds: Vec<_> = runs.iter().map(|r| (r.start, r.end, r.attrs)).collect();
        assert_eq!(bounds, [(0, 3, 0), (3, 8, 1), (8, 12, 0)]);
        let laid = layout::lay_out(text, &attrs, &runs, &Options::UNBOUNDED);
        assert!(
            laid.runs.iter().any(|r| r.color == [1.0, 0.0, 0.0, 1.0]) && laid.runs.iter().any(|r| r.color[0] == 0.0)
        );
    }
}
