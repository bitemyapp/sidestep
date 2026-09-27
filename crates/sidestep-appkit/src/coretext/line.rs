//! `CTLine` and `CTRun`: a line of text laid out by the text engine
//! (`text::glyphs`), its glyph runs with every glyph's id, position,
//! advance and character index, as CoreText describes them (measured on
//! macOS):
//!
//! - A line is one line whatever the text holds: separators and control
//!   characters are zero-width glyphs (the space's, or the font's own for
//!   U+2028 and U+2029), a tab reaches the next tab stop (every 28 points
//!   by default). Runs split where the attributes, the face (font
//!   fallback) or the direction change; a right-to-left run has its glyphs
//!   left to right, its indices falling. A run's string range covers the
//!   characters its glyphs stand for, a ligature's later ones included. A
//!   run's attributes are the attributed string's dictionary itself, or a
//!   copy naming the face fallback chose.
//! - The line's ascent, descent and leading are the largest of its runs'
//!   fonts' (unrounded, as CoreText gives them), raised and lowered by
//!   baseline offsets; its width includes trailing whitespace, which
//!   `CTLineGetTrailingWhitespaceWidth` reports (with tracking after the
//!   last glyph).
//! - Carets: each character has a leading and a trailing edge, the left
//!   and right of its share of its cluster (a ligature's characters share
//!   its width evenly), right and left in a right-to-left run; between a
//!   kerned pair the edge sits halfway into the kerning. A string index's
//!   primary offset is the trailing edge of the character before it, the
//!   secondary the leading edge of its own (they differ where the
//!   direction changes); a position finds the character under it and the
//!   edge of the half it's in. Carets are enumerated left to right.
//! - Drawing starts at the context's text position, through the text
//!   matrix (`coretext::draw`), backgrounds first, then glyphs, then
//!   underlines and strikethroughs, and moves the text position on by the
//!   line's width; glyphs are black unless the attributes color them.
//!   Underlines sit at least their thickness below the baseline, and
//!   strikethroughs halfway up the x-height, in whole points (as macOS
//!   snaps them to pixels at 1×).
//! - Truncating keeps as many whole clusters as fit beside the token, at
//!   the end, the start, or both (half the room each); the room is the
//!   width less the token's, plus the line's trailing whitespace, which
//!   the kept end of a middle truncation doesn't count. Whitespace next to
//!   the token is left out, and the token's run stands for the characters
//!   it replaces. Without a token nothing is put in their place. A line
//!   whose text fits but for its trailing whitespace loses that; a width
//!   narrower than the token gives no line.
//! - Justifying spreads the room over the line's gaps in stages: word
//!   spaces first (up to half an em each, a space followed by another
//!   taking twice as much), then the gaps between letters and, half as
//!   much, spaces; trailing whitespace is dropped. Narrowing takes up to
//!   11/256 em from each space, then up to 11/128 em from each letter gap
//!   (half that from spaces), then the rest from the spaces (or the
//!   letters, with no spaces); partial justification never narrows.

use std::ops::Range;
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::NSFont;
use objc2_core_foundation::{
    CFArray, CFAttributedString, CFDictionary, CFIndex, CFRange, CFTypeID, CGAffineTransform, CGFloat, CGPoint, CGRect,
    CGSize,
};
use objc2_core_graphics::{CGContext, CGGlyph};
use objc2_core_text::{CTLine, CTLineBoundsOptions, CTLineTruncationType, CTRun, CTRunStatus};
use objc2_foundation::{NSArray, NSMutableCopying, NSMutableDictionary, NSString};
use parley::FontData;

use super::attrs::{Dict, Styled};
use super::draw::{self, GlyphFace, Paints};
use super::font::glyph_bounds;
use super::{cf_range, owned, range_in};
use crate::coregraphics::geometry::rect;
use crate::protocol::Color;
use crate::text::fonts::Synth;
use crate::text::glyphs::{self, Breaking, Class, GlyphLine, GlyphRunData};

/// A run's font's metrics at its size, in points: ascent, descent and
/// leading, the underline's position and thickness, and the x-height
/// (where strikethroughs go).
#[derive(Clone, Copy, Debug, Default)]
struct RunMetrics {
    ascent: f64,
    descent: f64,
    leading: f64,
    underline_position: f64,
    underline_thickness: f64,
    x_height: f64,
}

/// A decoration's style (`NSUnderlineStyle`'s bits) and color.
type Decoration = (i64, Option<Color>);

/// A glyph of a line: its run and its place in it.
type Glyph = (usize, usize);

/// A run's glyphs and what drawing them takes.
#[derive(Clone)]
pub(crate) struct RunIvars {
    pub glyphs: Vec<u16>,
    pub positions: Vec<CGPoint>,
    pub advances: Vec<CGSize>,
    pub indices: Vec<CFIndex>,
    /// Each glyph's advance before kerning adjusted it (its own, plus the
    /// letter spacing), for carets.
    nominal: Vec<f64>,
    class: Vec<Class>,
    attributes: Retained<Dict>,
    font: Retained<NSFont>,
    status: CTRunStatus,
    range: CFRange,
    data: FontData,
    coords: Arc<[i16]>,
    synth: Synth,
    size: f64,
    color: Color,
    from_context: bool,
    /// Stroke width as a percentage of the size, and its color.
    stroke: (f64, Option<Color>),
    underline: Decoration,
    strikethrough: Decoration,
    background: Option<Color>,
    baseline_offset: f64,
    metrics: RunMetrics,
    tracking: f64,
}

/// The items of `items` at `keep`, in that order.
fn pick<T: Copy>(items: &[T], keep: &[usize]) -> Vec<T> {
    keep.iter().map(|&k| items[k]).collect()
}

impl RunIvars {
    /// The run with only glyphs `keep`, in that order.
    fn with_glyphs(&self, keep: &[usize]) -> RunIvars {
        let mut run = self.clone();
        run.glyphs = pick(&self.glyphs, keep);
        run.positions = pick(&self.positions, keep);
        run.advances = pick(&self.advances, keep);
        run.indices = pick(&self.indices, keep);
        run.nominal = pick(&self.nominal, keep);
        run.class = pick(&self.class, keep);
        run
    }

    fn rtl(&self) -> bool {
        self.status.contains(CTRunStatus::RightToLeft)
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; runs are immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTRun"]
    #[ivars = RunIvars]
    pub(crate) struct CTRunImpl;

    impl CTRunImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let r = self.ivars().range;
            let rest = format!("{{string range = ({}, {}), glyph count = {}}}", r.location, r.length, self.ivars().glyphs.len());
            crate::coregraphics::description("CTRun", self, &rest)
        }
    }

    unsafe impl NSObjectProtocol for CTRunImpl {}
);

/// A character's place for carets: its UTF-16 units, its left and right
/// edges from the line's origin, and whether it runs right to left.
#[derive(Clone, Copy, Debug)]
struct CaretChar {
    start: usize,
    end: usize,
    x0: f64,
    x1: f64,
    rtl: bool,
}

impl CaretChar {
    fn leading(&self) -> f64 {
        if self.rtl { self.x1 } else { self.x0 }
    }

    fn trailing(&self) -> f64 {
        if self.rtl { self.x0 } else { self.x1 }
    }
}

pub(crate) struct LineIvars {
    runs: Retained<NSArray<CTRunImpl>>,
    width: f64,
    ascent: f64,
    descent: f64,
    leading: f64,
    /// The largest of the runs' fonts' ascent, descent and leading, before
    /// baseline offsets: the box bounds options other than glyph bounds
    /// report.
    font_box: (f64, f64, f64),
    trailing: f64,
    range: CFRange,
    glyph_count: usize,
    /// The characters' caret edges, left to right.
    carets: Vec<CaretChar>,
    /// `carets` in the order of the text.
    logical: Vec<usize>,
    /// Where the last glyph is, for a line with nothing to draw's image
    /// bounds.
    last_x: f64,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; lines are
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTLine"]
    #[ivars = LineIvars]
    pub(crate) struct CTLineImpl;

    impl CTLineImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let i = self.ivars();
            let rest = format!(
                "{{run count = {}, string range = ({}, {}), width = {}, A/D/L = {}/{}/{}, glyph count = {}}}",
                i.runs.count(),
                i.range.location,
                i.range.length,
                i.width,
                i.ascent,
                i.descent,
                i.leading,
                i.glyph_count
            );
            crate::coregraphics::description("CTLine", self, &rest)
        }
    }

    unsafe impl NSObjectProtocol for CTLineImpl {}
);

pub(crate) fn line_imp(l: &CTLine) -> &CTLineImpl {
    // SAFETY: every CTLine is a CTLineImpl.
    unsafe { &*(l as *const CTLine).cast::<CTLineImpl>() }
}

pub(crate) fn run_imp(r: &CTRun) -> &CTRunImpl {
    // SAFETY: every CTRun is a CTRunImpl.
    unsafe { &*(r as *const CTRun).cast::<CTRunImpl>() }
}

impl CTLineImpl {
    pub(crate) fn runs(&self) -> Vec<Retained<CTRunImpl>> {
        self.ivars().runs.to_vec()
    }

    pub(crate) fn width(&self) -> f64 {
        self.ivars().width
    }

    pub(crate) fn trailing(&self) -> f64 {
        self.ivars().trailing
    }

    pub(crate) fn metrics(&self) -> (f64, f64, f64) {
        let i = self.ivars();
        (i.ascent, i.descent, i.leading)
    }

    /// The starts of the characters carets know, in the order of the
    /// text.
    fn char_starts(&self) -> Vec<usize> {
        let i = self.ivars();
        i.logical.iter().map(|&c| i.carets[c].start).collect()
    }
}

/// The font a run's glyphs are in, and its attributes: the style's (the
/// same dictionary) when layout used the style's font, or a copy naming
/// the face it fell back on.
fn font_and_attributes(styled: &Styled, run: &GlyphRunData) -> (Retained<NSFont>, Retained<Dict>) {
    let style = &styled.styles[run.attrs as usize];
    let own = crate::font::parts(&style.font).1.font.as_ref().map(|f| (f.data.id(), f.index));
    let same = own == Some((run.font.data.id(), run.font.index));
    // SAFETY: the constant is this crate's own.
    let key = super::ns_string(unsafe { objc2_core_text::kCTFontAttributeName });
    let font = if same {
        style.font.clone()
    } else {
        super::font::font_for_data(&run.font, f64::from(run.size)).unwrap_or_else(|| style.font.clone())
    };
    let named = style.dict.as_ref().is_some_and(|d| d.objectForKey(key).is_some());
    let dict = match (&style.dict, same && named) {
        (Some(d), true) => d.clone(),
        (Some(d), false) => {
            let copy: Retained<NSMutableDictionary<NSString, AnyObject>> = d.mutableCopy();
            // SAFETY: a font is an object; the key a constant string.
            unsafe { copy.setObject_forKey(&font, objc2::runtime::ProtocolObject::from_ref(key)) };
            Retained::into_super(copy)
        }
        (None, _) => {
            let font: &AnyObject = &font;
            objc2_foundation::NSDictionary::from_slices(&[key], &[font])
        }
    };
    (font, dict)
}

/// A run object for laid-out glyphs, their indices moved by `base`.
fn make_run(styled: &Styled, run: &GlyphRunData, base: usize) -> Retained<CTRunImpl> {
    let (font, attributes) = font_and_attributes(styled, run);
    let attrs = &styled.attrs[run.attrs as usize];
    let style = &styled.styles[run.attrs as usize];
    let face = crate::font::parts(&font).1.clone();
    let size = f64::from(run.size);
    let per_em = face.units.per_em;
    let scale = if per_em > 0.0 { size / per_em } else { 0.0 };
    let letter = f64::from(attrs.kern.unwrap_or(0.0));
    let own = super::font::glyph_advances(&face, run.glyphs.iter().copied());
    let nominal: Vec<f64> = run
        .advances
        .iter()
        .enumerate()
        .map(|(k, &a)| {
            let own = own.get(k).and_then(Option::as_ref);
            if a == 0.0 { 0.0 } else { own.map_or(a, |g| g.advance * scale + letter) }
        })
        .collect();
    let u = &face.units;
    let metrics = RunMetrics {
        ascent: u.ascent * scale,
        descent: -u.descent * scale,
        leading: u.leading * scale,
        underline_position: u.underline_position * scale,
        underline_thickness: u.underline_thickness * scale,
        x_height: u.x_height * scale,
    };
    let ivars = RunIvars {
        glyphs: run.glyphs.clone(),
        positions: run.positions.iter().map(|&(x, y)| CGPoint { x, y }).collect(),
        advances: run.advances.iter().map(|&a| CGSize { width: a, height: 0.0 }).collect(),
        indices: run.indices.iter().map(|&i| (i + base) as CFIndex).collect(),
        nominal,
        class: run.class.clone(),
        attributes,
        font,
        status: if run.rtl { CTRunStatus::RightToLeft } else { CTRunStatus::NoStatus },
        range: cf_range(run.range.start + base..run.range.end + base),
        data: run.font.clone(),
        coords: run.coords.clone(),
        synth: run.synth,
        size,
        color: attrs.color,
        from_context: style.from_context,
        stroke: (style.stroke_width, attrs.stroke.color),
        underline: (attrs.underline.style, attrs.underline.color),
        strikethrough: (attrs.strikethrough.style, attrs.strikethrough.color),
        background: attrs.background,
        baseline_offset: f64::from(attrs.baseline_offset),
        metrics,
        tracking: style.tracking,
    };
    new_run(ivars)
}

fn new_run(ivars: RunIvars) -> Retained<CTRunImpl> {
    let this = CTRunImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A line of `runs` over UTF-16 `range`; `chars` gives the characters'
/// starting units (for carets inside ligatures).
fn make_line(runs: Vec<Retained<CTRunImpl>>, range: Range<usize>, chars: &[usize]) -> Retained<CTLineImpl> {
    let (mut ascent, mut descent, mut leading) = (0.0f64, 0.0f64, 0.0f64);
    let mut font_box = (0.0f64, 0.0f64, 0.0f64);
    let mut width = 0.0;
    let mut glyph_count = 0;
    for r in &runs {
        let i = r.ivars();
        let m = &i.metrics;
        ascent = ascent.max(m.ascent + i.baseline_offset);
        descent = descent.max(m.descent - i.baseline_offset);
        leading = leading.max(m.leading);
        font_box = (font_box.0.max(m.ascent), font_box.1.max(m.descent), font_box.2.max(m.leading));
        width += i.advances.iter().map(|a| a.width).sum::<f64>();
        glyph_count += i.glyphs.len();
    }
    // Trailing whitespace: the blank glyphs at the end of the text, and
    // the tracking after the last glyph that isn't.
    let mut logical: Vec<(usize, f64, f64, bool, f64)> = Vec::with_capacity(glyph_count);
    for r in &runs {
        let i = r.ivars();
        for k in 0..i.glyphs.len() {
            let blank = i.class[k].is_blank();
            logical.push((i.indices[k].max(0) as usize, i.positions[k].x, i.advances[k].width, blank, i.tracking));
        }
    }
    logical.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut trailing = 0.0;
    for &(_, _, advance, blank, tracking) in logical.iter().rev() {
        if blank {
            trailing += advance;
        } else {
            trailing += tracking;
            break;
        }
    }
    let last_x = runs.last().and_then(|r| r.ivars().positions.last().map(|p| p.x)).unwrap_or(0.0);
    let carets = carets(&runs, range.clone(), chars);
    let mut order: Vec<usize> = (0..carets.len()).collect();
    order.sort_by_key(|&c| carets[c].start);
    order.dedup_by_key(|c| carets[*c].start);
    let runs = NSArray::from_retained_slice(&runs);
    let ivars = LineIvars {
        runs,
        width,
        ascent,
        descent,
        leading,
        font_box,
        trailing,
        range: cf_range(range),
        glyph_count,
        carets,
        logical: order,
        last_x,
    };
    let this = CTLineImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A cluster of a line as carets see it: glyphs of one run standing for
/// the characters from `index`, left to right.
struct CaretCluster {
    index: usize,
    x0: f64,
    x1: f64,
    /// What kerning took from (or added to) the cluster's advance.
    kerning: f64,
    rtl: bool,
}

/// The caret edges of a line's characters, left to right (see the
/// module).
fn carets(runs: &[Retained<CTRunImpl>], range: Range<usize>, chars: &[usize]) -> Vec<CaretChar> {
    let mut clusters: Vec<CaretCluster> = Vec::new();
    for r in runs {
        let i = r.ivars();
        let rtl = i.rtl();
        let mut first = true;
        for k in 0..i.glyphs.len() {
            let index = i.indices[k].max(0) as usize;
            let (x, advance) = (i.positions[k].x, i.advances[k].width);
            let kerning = advance - i.nominal.get(k).copied().unwrap_or(advance);
            match clusters.last_mut() {
                Some(c) if !first && c.index == index => {
                    c.x0 = c.x0.min(x);
                    c.x1 = c.x1.max(x + advance);
                    c.kerning += kerning;
                }
                _ => clusters.push(CaretCluster { index, x0: x, x1: x + advance, kerning, rtl }),
            }
            first = false;
        }
    }
    // Where each cluster's characters end: at the next cluster's in the
    // text, or the line's end.
    let mut starts: Vec<usize> = clusters.iter().map(|c| c.index).collect();
    starts.sort_unstable();
    starts.dedup();
    let end_of = |index: usize| starts.get(starts.partition_point(|&s| s <= index)).copied().unwrap_or(range.end);
    // Edges: between two left-to-right clusters, halfway into the kerning.
    let mut edges: Vec<(f64, f64)> = clusters.iter().map(|c| (c.x0, c.x1)).collect();
    for k in 1..clusters.len() {
        let (before, this) = (&clusters[k - 1], &clusters[k]);
        if !before.rtl && !this.rtl {
            let edge = this.x0 - before.kerning / 2.0;
            edges[k - 1].1 = edge;
            edges[k].0 = edge;
        }
    }
    let mut out = Vec::with_capacity(chars.len().max(clusters.len()));
    for (c, &(x0, x1)) in clusters.iter().zip(&edges) {
        let end = end_of(c.index).max(c.index + 1);
        let mut inside = vec![c.index];
        let from = chars.partition_point(|&u| u <= c.index);
        inside.extend(chars[from..].iter().copied().take_while(|&u| u < end));
        let n = inside.len();
        let share = (x1 - x0) / n as f64;
        for step in 0..n {
            // Left to right: a right-to-left cluster's characters from its
            // last.
            let j = if c.rtl { n - 1 - step } else { step };
            let (start, stop) = (inside[j], inside.get(j + 1).copied().unwrap_or(end));
            let (a, b) = if c.rtl {
                (x1 - share * (j + 1) as f64, x1 - share * j as f64)
            } else {
                (x0 + share * j as f64, x0 + share * (j + 1) as f64)
            };
            out.push(CaretChar { start, end: stop, x0: a, x1: b, rtl: c.rtl });
        }
    }
    out
}

/// Lay `text` of `styled` (UTF-16 units `range` of it) out on one line.
pub(crate) fn line_of(styled: &Styled, range: Range<usize>) -> Retained<CTLineImpl> {
    let (text, runs) = styled.slice(range.clone());
    let lines = glyphs::glyph_lines(&text, &styled.attrs, &runs, Breaking::None, styled.direction);
    let line = lines.into_iter().next().unwrap_or_default();
    line_from(styled, &line, range.start, range)
}

/// A line object for `line`, of text laid out from UTF-16 unit `base` of
/// `styled`, over `range` of it.
pub(crate) fn line_from(styled: &Styled, line: &GlyphLine, base: usize, range: Range<usize>) -> Retained<CTLineImpl> {
    let runs: Vec<Retained<CTRunImpl>> = line.runs.iter().map(|r| make_run(styled, r, base)).collect();
    let chars = styled.char_starts(range.start, range.end);
    make_line(runs, range, &chars)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_LINE
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_RUN
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineCreateWithAttributedString(
    string: Option<&CFAttributedString>,
) -> Option<NonNull<CTLine>> {
    let styled = super::attrs::styled(string?);
    let len = styled.utf16_len;
    Some(owned(line_of(&styled, 0..len)))
}

/// A line of `runs`' data, positions laid out anew from the start.
fn relaid(runs: Vec<RunIvars>, range: Range<usize>, chars: &[usize]) -> Retained<CTLineImpl> {
    let mut x = 0.0;
    let runs: Vec<Retained<CTRunImpl>> = runs
        .into_iter()
        .filter(|r| !r.glyphs.is_empty())
        .map(|mut r| {
            for k in 0..r.glyphs.len() {
                r.positions[k].x = x;
                x += r.advances[k].width;
            }
            new_run(r)
        })
        .collect();
    make_line(runs, range, chars)
}

/// The UTF-16 range a line covers.
fn units(range: CFRange) -> Range<usize> {
    let start = range.location.max(0) as usize;
    start..start + range.length.max(0) as usize
}

/// A cluster of a line for truncation and justification: glyphs of one
/// run standing for the characters from `index` to `end`.
#[derive(Clone, Debug)]
struct Cluster {
    run: usize,
    glyphs: Range<usize>,
    advance: f64,
    blank: bool,
    index: CFIndex,
    end: CFIndex,
}

/// A line's clusters, left to right.
fn clusters_of(runs: &[Retained<CTRunImpl>], line_end: CFIndex) -> Vec<Cluster> {
    let mut out: Vec<Cluster> = Vec::new();
    for (r, run) in runs.iter().enumerate() {
        let i = run.ivars();
        for k in 0..i.glyphs.len() {
            let (advance, blank, index) = (i.advances[k].width, i.class[k].is_blank(), i.indices[k]);
            match out.last_mut() {
                Some(c) if c.run == r && c.index == index => {
                    c.glyphs.end = k + 1;
                    c.advance += advance;
                    c.blank &= blank;
                }
                _ => out.push(Cluster { run: r, glyphs: k..k + 1, advance, blank, index, end: index + 1 }),
            }
        }
    }
    let mut starts: Vec<CFIndex> = out.iter().map(|c| c.index).collect();
    starts.sort_unstable();
    starts.dedup();
    for c in &mut out {
        c.end = starts.get(starts.partition_point(|&s| s <= c.index)).copied().unwrap_or(line_end).max(c.index + 1);
    }
    out
}

/// The runs of `clusters`, each with the glyphs and string range of its
/// clusters there.
fn runs_of(runs: &[Retained<CTRunImpl>], clusters: &[Cluster]) -> Vec<RunIvars> {
    let mut out: Vec<(usize, Vec<usize>, CFIndex, CFIndex)> = Vec::new();
    for c in clusters {
        match out.last_mut() {
            Some(last) if last.0 == c.run => {
                last.1.extend(c.glyphs.clone());
                last.2 = last.2.min(c.index);
                last.3 = last.3.max(c.end);
            }
            _ => out.push((c.run, c.glyphs.clone().collect(), c.index, c.end)),
        }
    }
    out.into_iter()
        .map(|(r, keep, lo, hi)| {
            let mut run = runs[r].ivars().with_glyphs(&keep);
            run.range = CFRange { location: lo, length: hi - lo };
            run
        })
        .collect()
}

/// The token's runs standing for the characters `elided` of the line.
fn token_runs(token: &CTLineImpl, elided: Range<CFIndex>) -> Vec<RunIvars> {
    token
        .ivars()
        .runs
        .iter()
        .map(|run| {
            let mut r = run.ivars().clone();
            r.indices.iter_mut().for_each(|i| *i = elided.start);
            r.range = CFRange { location: elided.start, length: elided.end - elided.start };
            r
        })
        .collect()
}

/// How many of `clusters` fit in `room` taken from the front (or the
/// back), `free` of them at the back counting nothing.
fn fitting<'a>(clusters: impl Iterator<Item = &'a Cluster>, room: f64, free: usize) -> usize {
    let mut used = 0.0;
    let mut n = 0;
    for (k, c) in clusters.enumerate() {
        let advance = if k < free { 0.0 } else { c.advance };
        if used + advance > room + 1e-9 {
            break;
        }
        used += advance;
        n += 1;
    }
    n
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineCreateTruncatedLine(
    line: Option<&CTLine>,
    width: f64,
    kind: CTLineTruncationType,
    token: Option<&CTLine>,
) -> Option<NonNull<CTLine>> {
    let line = line_imp(line?);
    truncated(line, width, kind, token.map(line_imp)).map(owned)
}

fn truncated(
    line: &CTLineImpl,
    width: f64,
    kind: CTLineTruncationType,
    token: Option<&CTLineImpl>,
) -> Option<Retained<CTLineImpl>> {
    let li = line.ivars();
    let chars = line.char_starts();
    let token_width = token.map_or(0.0, CTLineImpl::width);
    if width.is_nan() || width <= 0.0 || width < token_width {
        return None;
    }
    let runs = line.runs();
    let range = units(li.range);
    if li.width <= width + 1e-9 {
        return Some(relaid(runs.iter().map(|r| r.ivars().clone()).collect(), range, &chars));
    }
    // Clusters left to right, as the text runs in a left-to-right line.
    let line_end = li.range.location + li.range.length;
    let clusters = clusters_of(&runs, line_end);
    let n = clusters.len();
    // The whitespace at the end.
    let trailing = clusters.iter().rev().take_while(|c| c.blank).count();
    if li.width - li.trailing <= width + 1e-9 {
        // All but the trailing whitespace fits: the line without it (its
        // range as it was, its runs' only the characters they keep).
        return Some(relaid(runs_of(&runs, &clusters[..n - trailing]), range, &chars));
    }
    let room = width - token_width + li.trailing;
    let (head, tail) = match kind {
        CTLineTruncationType::Start => (0, fitting(clusters.iter().rev(), room, 0)),
        CTLineTruncationType::Middle => {
            // Half the room for each end (measured on macOS: at 24 points
            // in 100, "Hello wonderful world" keeps "He" and "rld", though
            // "Hel" would fit beside them).
            let head = fitting(clusters.iter(), room / 2.0, 0);
            let tail = fitting(clusters.iter().rev(), room / 2.0, trailing).min(n - head);
            (head, tail)
        }
        _ => (fitting(clusters.iter(), room, 0), 0),
    };
    // Whitespace next to the token is left out.
    let mut head = head;
    while head > 0 && clusters[head - 1].blank {
        head -= 1;
    }
    let mut tail = tail;
    while tail > 0 && clusters[n - tail].blank && kind != CTLineTruncationType::End {
        tail -= 1;
    }
    let first_tail = clusters.get(n - tail).map_or(line_end, |c| c.index).min(line_end);
    let elided_start = clusters.get(head).map_or(line_end, |c| c.index).min(first_tail);
    let elided = match kind {
        CTLineTruncationType::Start => li.range.location..first_tail,
        CTLineTruncationType::Middle => elided_start..first_tail,
        _ => elided_start..line_end,
    };
    let mut before = runs_of(&runs, &clusters[..head]);
    let after = runs_of(&runs, &clusters[n - tail..]);
    let middle = match token {
        Some(t) => token_runs(t, elided.clone()),
        None => {
            // Nothing stands for the characters left out: the run before
            // them reaches over them (at the end or in the middle).
            if kind != CTLineTruncationType::Start
                && let Some(last) = before.last_mut()
            {
                last.range.length = elided.end - last.range.location;
            }
            Vec::new()
        }
    };
    before.extend(middle);
    before.extend(after);
    Some(relaid(before, range, &chars))
}

/// How a glyph takes part in justification: its weight in each stage
/// (see the module) and how much it may give up narrowing.
#[derive(Clone, Copy, Debug, Default)]
struct Stretch {
    /// Word spaces' weight, widened (up to half an em each) or narrowed
    /// first, and last.
    spaces: f64,
    /// The weight in the second stage: gaps between letters, and spaces.
    letters: f64,
    /// An em of the glyph's size.
    em: f64,
}

/// The room left over spread over the line's gaps (see the module); the
/// line as it is if the factor is 0.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineCreateJustifiedLine(
    line: Option<&CTLine>,
    factor: CGFloat,
    width: f64,
) -> Option<NonNull<CTLine>> {
    let line = line_imp(line?);
    justified(line, factor, width).map(owned)
}

fn justified(line: &CTLineImpl, factor: f64, width: f64) -> Option<Retained<CTLineImpl>> {
    let li = line.ivars();
    let chars = line.char_starts();
    let range = units(li.range);
    let mut runs: Vec<RunIvars> = line.runs().iter().map(|r| r.ivars().clone()).collect();
    let factor = if factor.is_nan() { 0.0 } else { factor.clamp(0.0, 1.0) };
    if factor == 0.0 {
        return Some(relaid(runs, range, &chars));
    }
    let extra = width - (li.width - li.trailing);
    // Partial justification never narrows.
    if extra < 0.0 && factor < 1.0 {
        return None;
    }
    // Glyphs left to right; trailing whitespace takes no room.
    let order: Vec<Glyph> =
        runs.iter().enumerate().flat_map(|(r, run)| (0..run.glyphs.len()).map(move |k| (r, k))).collect();
    let last_inked = order
        .iter()
        .map(|&(r, k)| (runs[r].indices[k], runs[r].class[k].is_blank()))
        .filter(|&(_, blank)| !blank)
        .map(|(i, _)| i)
        .max();
    let (trailing, kept): (Vec<Glyph>, Vec<Glyph>) = order
        .iter()
        .partition(|&&(r, k)| runs[r].class[k].is_blank() && last_inked.is_none_or(|last| runs[r].indices[k] > last));
    for (r, k) in trailing {
        runs[r].advances[k].width = 0.0;
    }
    let stretch: Vec<Stretch> = kept
        .iter()
        .enumerate()
        .map(|(at, &(r, k))| {
            let class = runs[r].class[k];
            let next = kept.get(at + 1).map(|&(nr, nk)| runs[nr].class[nk]);
            let em = runs[r].size;
            match next {
                None => Stretch { em, ..Stretch::default() },
                _ if class.is_tab() => Stretch { em, ..Stretch::default() },
                Some(n) if class.is_space() && n.is_space() => Stretch { spaces: 2.0, letters: 0.0, em },
                Some(_) if class.is_space() => Stretch { spaces: 1.0, letters: 0.5, em },
                Some(n) if n.is_space() || n.is_tab() => Stretch { em, ..Stretch::default() },
                Some(_) => Stretch { spaces: 0.0, letters: 1.0, em },
            }
        })
        .collect();
    let amount = extra.abs() * factor;
    let give = spread(&stretch, amount, extra > 0.0)?;
    let sign = if extra > 0.0 { 1.0 } else { -1.0 };
    for (&(r, k), g) in kept.iter().zip(give) {
        runs[r].advances[k].width += sign * g;
    }
    Some(relaid(runs, range, &chars))
}

/// How much of `amount` each glyph takes, widening or narrowing (see the
/// module); none if narrowing can't take it all.
fn spread(stretch: &[Stretch], amount: f64, widen: bool) -> Option<Vec<f64>> {
    let mut give = vec![0.0; stretch.len()];
    let mut left = amount;
    // Take up to `cap(s)` from each glyph in proportion, as far as `left`
    // goes.
    let stage = |give: &mut Vec<f64>, left: &mut f64, cap: &dyn Fn(&Stretch) -> f64| {
        let total: f64 = stretch.iter().map(cap).sum();
        if total <= 0.0 || *left <= 0.0 {
            return;
        }
        let taken = left.min(total);
        for (g, s) in give.iter_mut().zip(stretch) {
            *g += taken * cap(s) / total;
        }
        *left -= taken;
    };
    // A share of what's left without limit, by `weight`.
    let rest = |give: &mut Vec<f64>, left: &mut f64, weight: &dyn Fn(&Stretch) -> f64| -> bool {
        let total: f64 = stretch.iter().map(weight).sum();
        if total <= 0.0 {
            return false;
        }
        for (g, s) in give.iter_mut().zip(stretch) {
            *g += *left * weight(s) / total;
        }
        *left = 0.0;
        true
    };
    if widen {
        stage(&mut give, &mut left, &|s| s.spaces * s.em / 2.0);
        if left > 0.0 && !rest(&mut give, &mut left, &|s| s.letters) {
            rest(&mut give, &mut left, &|s| s.spaces);
        }
        return Some(give);
    }
    // 11/128 em a letter gap; half that a space.
    let most = 11.0 / 128.0;
    stage(&mut give, &mut left, &|s| s.spaces * s.em * most / 2.0);
    stage(&mut give, &mut left, &|s| s.letters * s.em * most);
    // The rest from the spaces, or with none, the letters.
    if left > 1e-9 && !rest(&mut give, &mut left, &|s| s.spaces) && !rest(&mut give, &mut left, &|s| s.letters) {
        return None;
    }
    Some(give)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetGlyphCount(line: Option<&CTLine>) -> CFIndex {
    line.map_or(0, |l| line_imp(l).ivars().glyph_count as CFIndex)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetGlyphRuns(line: Option<&CTLine>) -> Option<NonNull<CFArray>> {
    Some(super::borrowed(&*line_imp(line?).ivars().runs))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetStringRange(line: Option<&CTLine>) -> CFRange {
    line.map_or(CFRange { location: 0, length: 0 }, |l| line_imp(l).ivars().range)
}

/// Negative for a line wider than the width, as on macOS; the factor is
/// taken between 0 and 1.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetPenOffsetForFlush(line: Option<&CTLine>, factor: CGFloat, width: f64) -> f64 {
    let Some(line) = line else { return 0.0 };
    let i = line_imp(line).ivars();
    (width - (i.width - i.trailing)) * factor.clamp(0.0, 1.0)
}

/// Where a decoration of `style` goes (x, y, width, height from the
/// line's origin, y up), for a run with `metrics`: underlines at least
/// their thickness below the baseline, strikethroughs halfway up the
/// x-height, thick ones twice as thick, double ones as two lines a third
/// of that apart; edges in whole points (measured on macOS, which snaps
/// them to pixels).
fn decoration_rects(style: i64, strike: bool, metrics: &RunMetrics, x: f64, width: f64) -> Vec<[f64; 4]> {
    let thickness = metrics.underline_thickness.max(0.0);
    let (thick, double) = (style & 0x0f == 0x02, style & 0x0f == 0x09);
    let band = if thick || double { 2.0 * thickness } else { thickness };
    if strike {
        let h = band.ceil().max(1.0);
        let y = (metrics.x_height / 2.0 - h / 2.0).round();
        return vec![[x, y, width, h]];
    }
    if double {
        let line = (band / 3.0).ceil().max(1.0);
        let top = (metrics.underline_position + line).min(-thickness).floor();
        return vec![[x, top - line, width, line], [x, top - 3.0 * line, width, line]];
    }
    let h = band.ceil().max(1.0);
    let top = (metrics.underline_position + band / 2.0).min(-thickness).floor();
    vec![[x, top - h, width, h]]
}

/// Draw a run's glyphs `range` (all for an empty one) with the line's
/// origin at the text position; backgrounds, or decorations, if asked.
fn draw_run(st: &mut crate::context::ContextState, run: &RunIvars, range: Range<usize>, parts: [bool; 3]) {
    let [backgrounds, glyphs, decorations] = parts;
    let positions: Vec<(f64, f64)> = run.positions[range.clone()].iter().map(|p| (p.x, p.y)).collect();
    let m = &run.metrics;
    let x0 = positions.first().map_or(0.0, |p| p.0);
    let width: f64 = run.advances[range.clone()].iter().map(|a| a.width).sum();
    let color = if run.from_context { st.gs.fill } else { run.color };
    if backgrounds && let Some(bg) = run.background {
        draw::fill_text_rect(st, [x0, -m.descent, width, m.ascent + m.descent], bg);
    }
    if glyphs {
        let face = GlyphFace { font: &run.data, coords: &run.coords, synth: run.synth, size: run.size };
        let (w, stroke_color) = run.stroke;
        let paints = if w == 0.0 {
            Paints { fill: Some(color), stroke: None }
        } else {
            let stroke = (stroke_color.unwrap_or(color), w.abs() / 100.0 * run.size);
            Paints { fill: (w < 0.0).then_some(color), stroke: Some(stroke) }
        };
        draw::draw_glyphs(st, &face, &run.glyphs[range], &positions, draw::Positions::Line, paints);
    }
    if decorations {
        for (strike, (style, deco_color)) in [(false, run.underline), (true, run.strikethrough)] {
            if style & 0xff == 0 {
                continue;
            }
            for [x, y, w, h] in decoration_rects(style, strike, m, x0, width) {
                draw::fill_text_rect(st, [x, y + run.baseline_offset, w, h], deco_color.unwrap_or(color));
            }
        }
    }
}

/// Draw `line` into `context` at its text position, moving it on.
pub(crate) fn draw_line(line: &CTLineImpl, context: &CGContext) {
    let runs = line.runs();
    let width = line.width();
    crate::coregraphics::context::with_state(context, |st| {
        for parts in [[true, false, false], [false, true, false], [false, false, true]] {
            for run in &runs {
                let n = run.ivars().glyphs.len();
                draw_run(st, run.ivars(), 0..n, parts);
            }
        }
        let [a, b, c, d, e, f] = st.text_matrix.as_coeffs();
        st.text_matrix = kurbo::Affine::new([a, b, c, d, e + width, f]);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineDraw(line: Option<&CTLine>, context: Option<&CGContext>) {
    let (Some(line), Some(context)) = (line, context) else { return };
    draw_line(line_imp(line), context);
}

/// # Safety
///
/// Each pointer is null or valid to write a float through.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTLineGetTypographicBounds(
    line: Option<&CTLine>,
    ascent: *mut CGFloat,
    descent: *mut CGFloat,
    leading: *mut CGFloat,
) -> f64 {
    let (w, a, d, l) = line.map_or((0.0, 0.0, 0.0, 0.0), |l| {
        let i = line_imp(l).ivars();
        (i.width, i.ascent, i.descent, i.leading)
    });
    // SAFETY: as the caller promises.
    unsafe {
        crate::coregraphics::store(ascent, a);
        crate::coregraphics::store(descent, d);
        crate::coregraphics::store(leading, l);
    }
    w
}

/// The inked bounds of the glyphs `keep` says to, from the line's origin;
/// for none, an empty rectangle at `last_x`.
fn image_bounds(
    runs: &[Retained<CTRunImpl>],
    last_x: f64,
    range: Option<Range<usize>>,
    keep: impl Fn(usize, usize) -> bool,
) -> CGRect {
    let mut all: Option<[f64; 4]> = None;
    for (ri, run) in runs.iter().enumerate() {
        let i = run.ivars();
        let face = crate::font::parts(&i.font).1.clone();
        let scale = if face.units.per_em > 0.0 { i.size / face.units.per_em } else { 0.0 };
        let r = range.clone().unwrap_or(0..i.glyphs.len());
        for (k, b) in glyph_bounds(&face, i.glyphs[r.clone()].iter().copied()).into_iter().enumerate() {
            let Some([x0, y0, x1, y1]) = b else { continue };
            if !keep(ri, r.start + k) {
                continue;
            }
            let p = i.positions[r.start + k];
            let b = [p.x + x0 * scale, p.y + y0 * scale, p.x + x1 * scale, p.y + y1 * scale];
            all = Some(match all {
                None => b,
                Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
            });
        }
    }
    match all {
        Some([x0, y0, x1, y1]) => rect(x0, y0, x1 - x0, y1 - y0),
        None => rect(last_x, 0.0, 0.0, 0.0),
    }
}

/// The text position of `context`, which image bounds are offset by.
fn text_position(context: Option<&CGContext>) -> (f64, f64) {
    context
        .and_then(|c| {
            crate::coregraphics::context::with_state(c, |st| {
                let [.., x, y] = st.text_matrix.as_coeffs();
                (x, y)
            })
        })
        .unwrap_or((0.0, 0.0))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetImageBounds(line: Option<&CTLine>, context: Option<&CGContext>) -> CGRect {
    let Some(line) = line else { return CGRect::ZERO };
    let line = line_imp(line);
    let mut r = image_bounds(&line.runs(), line.ivars().last_x, None, |_, _| true);
    let (x, y) = text_position(context);
    r.origin.x += x;
    r.origin.y += y;
    r
}

/// The glyphs of hanging punctuation at a line's ends (its trailing
/// whitespace aside), by run and glyph, and their widths before and after
/// the rest.
fn hanging(line: &CTLineImpl) -> (Vec<(usize, usize)>, f64, f64) {
    let runs = line.runs();
    let clusters = clusters_of(&runs, line.ivars().range.location + line.ivars().range.length);
    let class = |c: &Cluster| runs[c.run].ivars().class[c.glyphs.start];
    let mut glyphs = Vec::new();
    let (mut before, mut after) = (0.0, 0.0);
    let inked: Vec<&Cluster> = {
        let trailing = clusters.iter().rev().take_while(|c| c.blank).count();
        clusters[..clusters.len() - trailing].iter().collect()
    };
    let mut lo = 0;
    while lo < inked.len() && class(inked[lo]).hangs_before() {
        before += inked[lo].advance;
        glyphs.extend(inked[lo].glyphs.clone().map(|k| (inked[lo].run, k)));
        lo += 1;
    }
    let mut hi = inked.len();
    while hi > lo && class(inked[hi - 1]).hangs_after() {
        after += inked[hi - 1].advance;
        glyphs.extend(inked[hi - 1].glyphs.clone().map(|k| (inked[hi - 1].run, k)));
        hi -= 1;
    }
    (glyphs, before, after)
}

/// The language extents macOS gives Latin text in DejaVu Sans, in ems of
/// the line's largest font: left, below, and the width and height added
/// (other fonts' differ; see the architecture notes).
const LANGUAGE_EXTENTS: [f64; 4] = [0.186_524, 0.517_903_844, 0.298_525, 1.588_705_5];

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetBoundsWithOptions(line: Option<&CTLine>, options: CTLineBoundsOptions) -> CGRect {
    let Some(line) = line else { return CGRect::ZERO };
    let line = line_imp(line);
    let i = line.ivars();
    let content = i.width - i.trailing;
    let hang = options.contains(CTLineBoundsOptions::UseHangingPunctuation);
    // Optical bounds win over glyph bounds (measured on macOS).
    if options.contains(CTLineBoundsOptions::UseGlyphPathBounds)
        && !options.contains(CTLineBoundsOptions::UseOpticalBounds)
    {
        let hung = if hang { hanging(line).0 } else { Vec::new() };
        return image_bounds(&line.runs(), i.last_x, None, |r, k| !hung.contains(&(r, k)));
    }
    if options.contains(CTLineBoundsOptions::IncludeLanguageExtents) {
        let size = line.runs().iter().map(|r| r.ivars().size).fold(0.0, f64::max);
        let [left, below, wider, taller] = LANGUAGE_EXTENTS.map(|v| v * size);
        return rect(-left, -below, content + wider, taller);
    }
    // The fonts' box, before baseline offsets.
    let (ascent, descent, leading) = i.font_box;
    let (x, width) = if hang {
        let (_, before, after) = hanging(line);
        (before, content - before - after)
    } else if options.contains(CTLineBoundsOptions::UseOpticalBounds) {
        (0.0, content)
    } else {
        (0.0, i.width)
    };
    if options.contains(CTLineBoundsOptions::ExcludeTypographicLeading) {
        rect(x, -descent, width, ascent + descent)
    } else {
        rect(x, -descent - leading, width, ascent + descent + leading)
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetTrailingWhitespaceWidth(line: Option<&CTLine>) -> f64 {
    line.map_or(0.0, |l| line_imp(l).ivars().trailing)
}

/// The character under `position` (the first or last for one past the
/// ends), and the edge of the half it's in; -1 for a line with no
/// characters.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineGetStringIndexForPosition(line: Option<&CTLine>, position: CGPoint) -> CFIndex {
    let Some(line) = line else { return -1 };
    let carets = &line_imp(line).ivars().carets;
    let x = position.x;
    let wide = |c: &&CaretChar| c.x1 > c.x0;
    let Some(c) =
        carets.iter().filter(wide).find(|c| x < c.x1).or_else(|| carets.iter().rev().find(wide)).or(carets.last())
    else {
        return -1;
    };
    let left = x < (c.x0 + c.x1) / 2.0;
    (if left != c.rtl { c.start } else { c.end }) as CFIndex
}

/// # Safety
///
/// `secondary` is null or valid to write a float through.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTLineGetOffsetForStringIndex(
    line: Option<&CTLine>,
    index: CFIndex,
    secondary: *mut CGFloat,
) -> CGFloat {
    let (primary, other) = line.map_or((0.0, 0.0), |l| offsets_for(line_imp(l), index));
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(secondary, other) };
    primary
}

/// A string index's primary and secondary caret offsets (see the module).
fn offsets_for(line: &CTLineImpl, index: CFIndex) -> (f64, f64) {
    let i = line.ivars();
    let (carets, logical) = (&i.carets, &i.logical);
    let (Some(&first), Some(&last)) = (logical.first(), logical.last()) else { return (0.0, 0.0) };
    let index = index.max(0) as usize;
    if index <= carets[first].start {
        let at = carets[first].leading();
        return (at, at);
    }
    if index >= carets[last].end {
        let at = carets[last].trailing();
        return (at, at);
    }
    let at = logical.partition_point(|&c| carets[c].start <= index) - 1;
    let c = &carets[logical[at]];
    if index == c.start {
        // At least the first character comes before it.
        (carets[logical[at - 1]].trailing(), c.leading())
    } else {
        // Inside a character (a pair's second unit): after it.
        let next = logical.get(at + 1).map_or(c.trailing(), |&n| carets[n].leading());
        (c.trailing(), next)
    }
}

/// The block `CTLineEnumerateCaretOffsets` calls: the offset, the index,
/// whether it's the leading edge, and where to say stop.
type Caret = block2::DynBlock<dyn Fn(f64, CFIndex, bool, NonNull<bool>)>;

/// Each character's edges, left to right, with the index of its first
/// unit for its leading edge and its last for its trailing edge, until the
/// block stops.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTLineEnumerateCaretOffsets(line: Option<&CTLine>, block: Option<&Caret>) {
    let (Some(line), Some(block)) = (line, block) else { return };
    let carets = line_imp(line).ivars().carets.clone();
    // SAFETY: the block's function takes the block, the offset, the index,
    // whether it's the leading edge and where to say stop.
    let f: unsafe extern "C-unwind" fn(&Caret, f64, CFIndex, bool, NonNull<bool>) =
        unsafe { std::mem::transmute(super::block_function(block)) };
    let mut stop = false;
    for c in &carets {
        let (lead, trail) = (c.start as CFIndex, c.end.saturating_sub(1).max(c.start) as CFIndex);
        let edges =
            if c.rtl { [(c.x0, trail, false), (c.x1, lead, true)] } else { [(c.x0, lead, true), (c.x1, trail, false)] };
        for (offset, index, leading) in edges {
            // SAFETY: as above.
            unsafe { f(block, offset, index, leading, NonNull::from(&mut stop)) };
            if stop {
                return;
            }
        }
    }
}

// Runs.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetGlyphCount(run: Option<&CTRun>) -> CFIndex {
    run.map_or(0, |r| run_imp(r).ivars().glyphs.len() as CFIndex)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetAttributes(run: Option<&CTRun>) -> Option<NonNull<CFDictionary>> {
    Some(super::borrowed(&*run_imp(run?).ivars().attributes))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetStatus(run: Option<&CTRun>) -> CTRunStatus {
    run.map_or(CTRunStatus::NoStatus, |r| run_imp(r).ivars().status)
}

fn ptr_of<T>(items: &[T]) -> *const T {
    if items.is_empty() { std::ptr::null() } else { items.as_ptr() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetGlyphsPtr(run: Option<&CTRun>) -> *const CGGlyph {
    run.map_or(std::ptr::null(), |r| ptr_of(&run_imp(r).ivars().glyphs))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetPositionsPtr(run: Option<&CTRun>) -> *const CGPoint {
    run.map_or(std::ptr::null(), |r| ptr_of(&run_imp(r).ivars().positions))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetAdvancesPtr(run: Option<&CTRun>) -> *const CGSize {
    run.map_or(std::ptr::null(), |r| ptr_of(&run_imp(r).ivars().advances))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetStringIndicesPtr(run: Option<&CTRun>) -> *const CFIndex {
    run.map_or(std::ptr::null(), |r| ptr_of(&run_imp(r).ivars().indices))
}

/// Copy `range` of `items` (all for an empty range) to `buffer`.
///
/// # Safety
///
/// `buffer` is null or has room for the range's items.
pub(crate) unsafe fn copy_out<T: Copy>(items: &[T], range: CFRange, buffer: *mut T) {
    if buffer.is_null() {
        return;
    }
    let r = range_in(range, items.len(), true);
    // SAFETY: as the caller promises.
    unsafe { std::ptr::copy_nonoverlapping(items[r.clone()].as_ptr(), buffer, r.len()) };
}

/// # Safety
///
/// `buffer` has room for the range's glyphs.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTRunGetGlyphs(run: Option<&CTRun>, range: CFRange, buffer: *mut CGGlyph) {
    // SAFETY: as the caller promises.
    if let Some(r) = run {
        unsafe { copy_out(&run_imp(r).ivars().glyphs, range, buffer) }
    }
}

/// # Safety
///
/// `buffer` has room for the range's positions.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTRunGetPositions(run: Option<&CTRun>, range: CFRange, buffer: *mut CGPoint) {
    // SAFETY: as the caller promises.
    if let Some(r) = run {
        unsafe { copy_out(&run_imp(r).ivars().positions, range, buffer) }
    }
}

/// # Safety
///
/// `buffer` has room for the range's advances.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTRunGetAdvances(run: Option<&CTRun>, range: CFRange, buffer: *mut CGSize) {
    // SAFETY: as the caller promises.
    if let Some(r) = run {
        unsafe { copy_out(&run_imp(r).ivars().advances, range, buffer) }
    }
}

/// # Safety
///
/// `buffer` has room for the range's indices.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTRunGetStringIndices(run: Option<&CTRun>, range: CFRange, buffer: *mut CFIndex) {
    // SAFETY: as the caller promises.
    if let Some(r) = run {
        unsafe { copy_out(&run_imp(r).ivars().indices, range, buffer) }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetStringRange(run: Option<&CTRun>) -> CFRange {
    run.map_or(CFRange { location: 0, length: 0 }, |r| run_imp(r).ivars().range)
}

/// The width of glyphs `range` (all for an empty one) and the run's
/// font's ascent, descent and leading.
///
/// # Safety
///
/// Each pointer is null or valid to write a float through.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTRunGetTypographicBounds(
    run: Option<&CTRun>,
    range: CFRange,
    ascent: *mut CGFloat,
    descent: *mut CGFloat,
    leading: *mut CGFloat,
) -> f64 {
    let Some(run) = run else { return 0.0 };
    let i = run_imp(run).ivars();
    let r = range_in(range, i.glyphs.len(), true);
    // SAFETY: as the caller promises.
    unsafe {
        crate::coregraphics::store(ascent, i.metrics.ascent);
        crate::coregraphics::store(descent, i.metrics.descent);
        crate::coregraphics::store(leading, i.metrics.leading);
    }
    i.advances[r].iter().map(|a| a.width).sum()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetImageBounds(
    run: Option<&CTRun>,
    context: Option<&CGContext>,
    range: CFRange,
) -> CGRect {
    let Some(run) = run else { return CGRect::ZERO };
    let imp = run_imp(run).retain();
    let n = imp.ivars().glyphs.len();
    let r = range_in(range, n, true);
    let last = r.end.checked_sub(1).map_or(0.0, |k| imp.ivars().positions[k].x);
    let mut out = image_bounds(&[imp], last, Some(r), |_, _| true);
    let (x, y) = text_position(context);
    out.origin.x += x;
    out.origin.y += y;
    out
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunGetTextMatrix(_run: Option<&CTRun>) -> CGAffineTransform {
    crate::coregraphics::geometry::IDENTITY
}

/// Each glyph's advance and origin, as positioned (no base advances are
/// kept apart from the adjusted ones).
///
/// # Safety
///
/// Each buffer is null or has room for the range's items.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTRunGetBaseAdvancesAndOrigins(
    run: Option<&CTRun>,
    range: CFRange,
    advances: *mut CGSize,
    origins: *mut CGPoint,
) {
    let Some(run) = run else { return };
    let i = run_imp(run).ivars();
    // SAFETY: as the caller promises.
    unsafe {
        copy_out(&i.advances, range, advances);
        let r = range_in(range, i.glyphs.len(), true);
        if !origins.is_null() {
            for (k, at) in r.enumerate() {
                let y = i.positions[at].y;
                origins.add(k).write(CGPoint { x: 0.0, y });
            }
        }
    }
}

/// Draw glyphs `range` of a run (all for an empty one) at their places
/// from the text position, which stays where it is.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTRunDraw(run: Option<&CTRun>, context: Option<&CGContext>, range: CFRange) {
    let (Some(run), Some(context)) = (run, context) else { return };
    let imp = run_imp(run);
    let r = range_in(range, imp.ivars().glyphs.len(), true);
    crate::coregraphics::context::with_state(context, |st| {
        for parts in [[true, false, false], [false, true, false], [false, false, true]] {
            draw_run(st, imp.ivars(), r.clone(), parts);
        }
    });
}
