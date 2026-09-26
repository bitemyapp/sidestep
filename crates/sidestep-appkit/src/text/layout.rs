//! Laying text out the way AppKit's string drawing does: attribute runs and
//! paragraph styles in, lines of positioned glyph runs, backgrounds and
//! decorations out.
//!
//! parley does the Unicode work: bidi, line breaking, shaping (harfrust)
//! and font fallback. This module decides where the lines go:
//!
//! - Text is cut into segments at paragraph separators and at U+2028. Each
//!   segment is one parley layout, so each paragraph finds its own base
//!   direction, and a separator never reaches the shaper.
//! - A line is as tall as the ascents plus the descents of its fonts, each
//!   rounded to a whole point, as AppKit's typesetter does; the fonts'
//!   leading is added only when the caller asks for it. Then the paragraph
//!   style's `lineHeightMultiple`, minimum and maximum apply, extra height
//!   going above the text.
//! - `lineSpacing` goes between lines, `paragraphSpacing` after a paragraph
//!   and `paragraphSpacingBefore` before one, but nothing above the first
//!   line or below the last.
//! - Word wrapping breaks inside a word that doesn't fit on a line by
//!   itself. Clipping and the truncating modes don't wrap; truncation
//!   replaces what doesn't fit with an ellipsis at the head, the middle or
//!   the tail.
//! - A width limit caps the reported width. A height limit drops the lines
//!   after the first that don't fit entirely (the first is always kept),
//!   and can truncate the last one that does; it then ends in an ellipsis
//!   whenever any text follows it, as AppKit's does.
//!
//! Layouts are cached per thread by text, attributes and options, in two
//! generations: a hit in the old one moves the entry to the new one, and
//! when the new one fills up (2048 entries or about 4 MB of text and
//! glyphs) the old one is dropped.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use icu_properties::CodePointSetData;
use icu_properties::props::{EmojiPresentation, ExtendedPictographic};
use parley::setting::Tag;
use parley::{
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontFeature, FontFeatures, FontStyle, FontWeight,
    FontWidth, GenericFamily, InlineBox, InlineBoxKind, Layout, OverflowWrap, PositionedLayoutItem, TextStyle,
    TextWrapMode, WordBreak,
};

use super::fonts::{self, Face, Features};
use super::{Brush, Ctx};
use crate::protocol::{Color, Glyph};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Align {
    Natural,
    Left,
    Right,
    Center,
    Justified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LineBreak {
    WordWrap,
    CharWrap,
    Clip,
    TruncateHead,
    TruncateTail,
    TruncateMiddle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Direction {
    Natural,
    LeftToRight,
    RightToLeft,
}

/// A paragraph style, as `NSParagraphStyle` holds it. Lengths keep the
/// `CGFloat`s they were set to, so that they read back exactly and compare
/// as AppKit's do; layout works in `f32`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Paragraph {
    pub alignment: Align,
    pub line_break: LineBreak,
    pub line_spacing: f64,
    pub paragraph_spacing: f64,
    pub paragraph_spacing_before: f64,
    pub head_indent: f64,
    pub first_line_head_indent: f64,
    /// From the leading edge if positive, from the trailing edge if not.
    pub tail_indent: f64,
    pub min_line_height: f64,
    pub max_line_height: f64,
    pub line_height_multiple: f64,
    pub direction: Direction,
    pub default_tab_interval: f64,
    /// The tab stops, sorted by location; `None` is AppKit's default
    /// twelve ([`tab_stops`]). A list lives as long as the styles and
    /// layouts that use it.
    pub tabs: Option<Arc<[Tab]>>,
}

/// A tab stop: where it is and how text lines up on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tab {
    pub location: f32,
    pub kind: TabKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TabKind {
    Left,
    Right,
    Center,
    Decimal,
}

/// AppKit's default tab stops: twelve left stops, 28 points apart.
pub(crate) const DEFAULT_TABS: [Tab; 12] = {
    let mut tabs = [Tab { location: 0.0, kind: TabKind::Left }; 12];
    let mut i = 0;
    while i < tabs.len() {
        tabs[i].location = 28.0 * (i + 1) as f32;
        i += 1;
    }
    tabs
};

/// The tab stops of `para`.
pub(crate) fn tab_stops(para: &Paragraph) -> &[Tab] {
    para.tabs.as_deref().unwrap_or(&DEFAULT_TABS)
}

impl Default for Paragraph {
    fn default() -> Self {
        Paragraph {
            alignment: Align::Natural,
            line_break: LineBreak::WordWrap,
            line_spacing: 0.0,
            paragraph_spacing: 0.0,
            paragraph_spacing_before: 0.0,
            head_indent: 0.0,
            first_line_head_indent: 0.0,
            tail_indent: 0.0,
            min_line_height: 0.0,
            max_line_height: 0.0,
            line_height_multiple: 0.0,
            direction: Direction::Natural,
            default_tab_interval: 0.0,
            tabs: None,
        }
    }
}

impl Paragraph {
    fn floats(&self) -> [f64; 10] {
        [
            self.line_spacing,
            self.paragraph_spacing,
            self.paragraph_spacing_before,
            self.head_indent,
            self.first_line_head_indent,
            self.tail_indent,
            self.min_line_height,
            self.max_line_height,
            self.line_height_multiple,
            self.default_tab_interval,
        ]
    }

    /// Hash what equality compares. Adding zero turns -0 into 0, which
    /// compare equal.
    pub fn hash_into(&self, h: &mut impl Hasher) {
        (self.alignment, self.line_break, self.direction).hash(h);
        self.floats().map(|v| (v + 0.0).to_bits()).hash(h);
        match &self.tabs {
            None => h.write_usize(usize::MAX),
            Some(tabs) => {
                h.write_usize(tabs.len());
                tabs.iter().for_each(|t| ((t.location + 0.0).to_bits(), t.kind).hash(h));
            }
        }
    }
}

/// A font at a size, as text is laid out in it.
#[derive(Clone, Debug)]
pub(crate) struct TextFont {
    pub face: Arc<Face>,
    pub size: f32,
    pub tabular_digits: bool,
    pub features: Option<Features>,
}

impl PartialEq for TextFont {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.face, &other.face)
            && self.size.to_bits() == other.size.to_bits()
            && self.tabular_digits == other.tabular_digits
            && self.features == other.features
    }
}

/// An underline or strikethrough: `NSUnderlineStyle` bits and a color, the
/// text's own if none.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Decoration {
    pub style: i64,
    pub color: Option<Color>,
}

/// The attributes of a run of text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Attrs {
    pub font: TextFont,
    pub color: Color,
    pub background: Option<Color>,
    pub underline: Decoration,
    pub strikethrough: Decoration,
    /// Extra space after each character; `Some(0)` turns kerning off.
    pub kern: Option<f32>,
    /// Points to raise the text by.
    pub baseline_offset: f32,
    /// 0: none, 1: the default ones, 2: all of them.
    pub ligatures: i64,
    pub paragraph: Paragraph,
}

impl Attrs {
    pub fn new(font: TextFont) -> Self {
        Attrs {
            font,
            color: [0.0, 0.0, 0.0, 1.0],
            background: None,
            underline: Decoration::default(),
            strikethrough: Decoration::default(),
            kern: None,
            baseline_offset: 0.0,
            ligatures: 1,
            paragraph: Paragraph::default(),
        }
    }

    fn hash_into(&self, h: &mut impl Hasher) {
        (Arc::as_ptr(&self.font.face) as usize, self.font.size.to_bits(), self.font.tabular_digits).hash(h);
        self.font.features.hash(h);
        let color = |c: Option<Color>| c.map(|c| c.map(f32::to_bits));
        (color(Some(self.color)), color(self.background)).hash(h);
        (self.underline.style, color(self.underline.color), self.strikethrough.style, color(self.strikethrough.color))
            .hash(h);
        (self.kern.map(f32::to_bits), self.baseline_offset.to_bits(), self.ligatures).hash(h);
        self.paragraph.hash_into(h);
    }

    fn features(&self) -> Vec<FontFeature> {
        // The font's own settings first, so the attributes' win.
        let mut features: Vec<FontFeature> = self
            .font
            .features
            .iter()
            .flat_map(|f| f.iter())
            .map(|&(tag, value)| FontFeature::new(Tag::new(&tag), value))
            .collect();
        if self.ligatures == 0 {
            features.push(FontFeature::new(Tag::new(b"liga"), 0));
            features.push(FontFeature::new(Tag::new(b"clig"), 0));
        } else if self.ligatures >= 2 {
            features.push(FontFeature::new(Tag::new(b"dlig"), 1));
        }
        if self.kern == Some(0.0) {
            features.push(FontFeature::new(Tag::new(b"kern"), 0));
        }
        if self.font.tabular_digits {
            features.push(FontFeature::new(Tag::new(b"tnum"), 1));
        }
        features
    }
}

/// A run of text with the same attributes: a byte range and an index into
/// the attributes passed alongside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Run {
    pub start: usize,
    pub end: usize,
    pub attrs: u32,
}

/// Where and how to lay text out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Options {
    /// The container's width; infinite for none.
    pub width: f32,
    /// The container's height; infinite for none.
    pub height: f32,
    /// Lay out every line (`NSStringDrawingUsesLineFragmentOrigin`), not
    /// just the first.
    pub all_lines: bool,
    /// Add the fonts' leading to line heights.
    pub font_leading: bool,
    /// Truncate the last line that fits if some don't.
    pub truncate_last: bool,
}

impl Options {
    pub const UNBOUNDED: Options = Options {
        width: f32::INFINITY,
        height: f32::INFINITY,
        all_lines: true,
        font_leading: false,
        truncate_last: false,
    };

    fn hash_into(&self, h: &mut impl Hasher) {
        (self.width.to_bits(), self.height.to_bits(), self.all_lines, self.font_leading, self.truncate_last).hash(h);
    }
}

/// Glyphs in one face and color, placed relative to the layout's top left
/// corner (`y` is the baseline).
#[derive(Clone, Debug)]
pub(crate) struct PlacedRun {
    pub font: u32,
    pub size: f32,
    pub x: f32,
    pub y: f32,
    pub glyphs: Arc<[Glyph]>,
    pub color: Color,
}

/// A rectangle to fill: a background, drawn before the glyphs, or a
/// decoration, drawn after them and snapped to whole points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlacedFill {
    pub rect: [f32; 4],
    pub color: Color,
    pub background: bool,
}

/// Text laid out.
#[derive(Clone, Debug, Default)]
pub(crate) struct TextLayout {
    pub width: f32,
    pub height: f32,
    /// The first line's descent, rounded as its height was.
    pub first_descent: f32,
    pub runs: Vec<PlacedRun>,
    pub fills: Vec<PlacedFill>,
}

/// Lay `text` out, or find it in this thread's cache. `runs` cover the text
/// in order and index into `attrs`; there is at least one.
pub(crate) fn lay_out(text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> Rc<TextLayout> {
    match cached(text, attrs, runs, opts) {
        Ok(layout) => layout,
        Err(hash) => super::with_ctx(|ctx| {
            let layout = Rc::new(compute(ctx, text, attrs, runs, opts));
            let entry = Entry { text: text.into(), attrs: attrs.into(), runs: runs.into(), opts: *opts, layout };
            ctx.layouts.insert(hash, entry)
        }),
    }
}

/// `text` laid out, if this thread has it cached; if not, the key to lay it
/// out under (see [`job`]).
pub(crate) fn cached(text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> Result<Rc<TextLayout>, u64> {
    let mut h = Fx::default();
    text.hash(&mut h);
    attrs.iter().for_each(|a| a.hash_into(&mut h));
    runs.hash(&mut h);
    opts.hash_into(&mut h);
    let hash = h.finish();
    super::with_ctx(|ctx| ctx.layouts.get(hash, text, attrs, runs, opts).ok_or(hash))
}

/// Text to lay out later, maybe on another thread.
pub(crate) struct Job {
    pub text: String,
    pub attrs: Vec<Attrs>,
    pub runs: Vec<Run>,
    pub opts: Options,
    hash: u64,
}

pub(crate) fn job(text: String, attrs: Vec<Attrs>, runs: Vec<Run>, opts: Options, hash: u64) -> Job {
    Job { text, attrs, runs, opts, hash }
}

impl Job {
    /// Whether the job lays out `text` as asked, so that text drawn again
    /// before the job is done can share it.
    pub fn is(&self, text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> bool {
        self.text == text && self.attrs == attrs && self.runs == runs && self.opts == *opts
    }
}

/// Lay `jobs` out, sharing the work with other threads when there's
/// enough of it, and cache them on this thread.
pub(crate) fn lay_out_all(jobs: Vec<Job>) -> Vec<Rc<TextLayout>> {
    // About 60 µs of work: below it, waking helpers costs more than it saves.
    let bytes: usize = jobs.iter().map(|j| j.text.len()).sum();
    let done: Vec<(Job, TextLayout)> = if jobs.len() >= 4 && bytes >= 512 {
        super::pool::run(jobs)
    } else {
        super::with_ctx(|ctx| {
            jobs.into_iter()
                .map(|j| {
                    let layout = compute(ctx, &j.text, &j.attrs, &j.runs, &j.opts);
                    (j, layout)
                })
                .collect()
        })
    };
    super::with_ctx(|ctx| {
        done.into_iter()
            .map(|(j, layout)| {
                let entry = Entry {
                    text: j.text.into(),
                    attrs: j.attrs.into(),
                    runs: j.runs.into(),
                    opts: j.opts,
                    layout: Rc::new(layout),
                };
                ctx.layouts.insert(j.hash, entry)
            })
            .collect()
    })
}

/// FxHash: a multiply and a rotate per word. Fast for short keys, and the
/// keys here are the program's own strings.
#[derive(Default, Clone, Copy)]
pub(crate) struct Fx(u64);

impl Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.write_u64(u64::from_le_bytes(*chunk));
        }
        let mut last = [0u8; 8];
        last[..rest.len()].copy_from_slice(rest);
        self.write_u64(u64::from_le_bytes(last) ^ rest.len() as u64);
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type FxBuild = BuildHasherDefault<Fx>;

pub(crate) struct Entry {
    text: Box<str>,
    attrs: Box<[Attrs]>,
    runs: Box<[Run]>,
    opts: Options,
    layout: Rc<TextLayout>,
}

impl Entry {
    fn is(&self, text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> bool {
        *self.text == *text && *self.attrs == *attrs && *self.runs == *runs && self.opts == *opts
    }
}

impl Entry {
    /// Roughly what the entry keeps alive.
    fn bytes(&self) -> usize {
        let glyphs: usize = self.layout.runs.iter().map(|r| r.glyphs.len()).sum();
        self.text.len() + glyphs * size_of::<Glyph>() + 256
    }
}

/// Entries and bytes per generation.
const GENERATION: usize = 2048;
const GENERATION_BYTES: usize = 4 << 20;

#[derive(Default)]
pub(crate) struct Cache {
    new: HashMap<u64, Vec<Entry>, FxBuild>,
    old: HashMap<u64, Vec<Entry>, FxBuild>,
    len: usize,
    bytes: usize,
}

impl Cache {
    fn get(&mut self, hash: u64, text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> Option<Rc<TextLayout>> {
        if let Some(entry) = self.new.get(&hash).and_then(|b| b.iter().find(|e| e.is(text, attrs, runs, opts))) {
            return Some(entry.layout.clone());
        }
        let bucket = self.old.get_mut(&hash)?;
        let at = bucket.iter().position(|e| e.is(text, attrs, runs, opts))?;
        let entry = bucket.swap_remove(at);
        Some(self.push(hash, entry))
    }

    /// Cache `entry`, unless an equal one is cached already, and return the
    /// layout the cache keeps.
    fn insert(&mut self, hash: u64, entry: Entry) -> Rc<TextLayout> {
        match self.get(hash, &entry.text, &entry.attrs, &entry.runs, &entry.opts) {
            Some(layout) => layout,
            None => self.push(hash, entry),
        }
    }

    fn push(&mut self, hash: u64, entry: Entry) -> Rc<TextLayout> {
        if self.len >= GENERATION || self.bytes >= GENERATION_BYTES {
            self.old = std::mem::take(&mut self.new);
            (self.len, self.bytes) = (0, 0);
        }
        self.bytes += entry.bytes();
        let layout = entry.layout.clone();
        self.new.entry(hash).or_default().push(entry);
        self.len += 1;
        layout
    }

    /// Entries in the newer generation.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len
    }
}

/// A piece of text between separators.
struct Segment {
    start: usize,
    end: usize,
    /// Starts a paragraph, rather than following a line separator.
    first_in_paragraph: bool,
}

fn segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let (mut start, mut first) = (0, true);
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let (paragraph, len) = match c {
            '\n' | '\u{2029}' | '\u{85}' => (true, c.len_utf8()),
            '\r' if chars.peek().is_some_and(|&(_, n)| n == '\n') => {
                chars.next();
                (true, 2)
            }
            '\r' => (true, 1),
            '\u{2028}' => (false, 3),
            _ => continue,
        };
        out.push(Segment { start, end: i, first_in_paragraph: first });
        (start, first) = (i + len, paragraph);
    }
    out.push(Segment { start, end: text.len(), first_in_paragraph: first });
    out
}

/// The runs over `start..end`, relative to `start`, covering it without
/// gaps. An empty range gets the attributes at `start`. Runs are in order,
/// and ranges are asked for in order, so `from` remembers where to look:
/// the runs before it end before `start`. That keeps text with many runs
/// and many paragraphs linear.
fn runs_in(runs: &[Run], from: &mut usize, start: usize, end: usize) -> Vec<Run> {
    while *from + 1 < runs.len() && runs[*from].end <= start {
        *from += 1;
    }
    let window = runs.get(*from..).unwrap_or_default();
    let mut out: Vec<Run> = window
        .iter()
        .take_while(|r| r.start < end)
        .filter(|r| r.end > start)
        .map(|r| Run { start: r.start.max(start) - start, end: r.end.min(end) - start, attrs: r.attrs })
        .collect();
    if out.is_empty() {
        // The first run that doesn't end before `start`, or the last one.
        let attrs = window.first().map_or(0, |r| r.attrs);
        out.push(Run { start: 0, end: end - start, attrs });
    }
    out[0].start = 0;
    for i in 1..out.len() {
        out[i].start = out[i - 1].end;
    }
    if let Some(last) = out.last_mut() {
        last.end = end - start;
    }
    out.retain(|r| r.end > r.start || end == start);
    out
}

/// A line of a segment, relative to its own top left corner.
struct Line {
    height: f32,
    descent: f32,
    /// How far the text reaches, trailing whitespace included and the
    /// alignment's offset left out, as AppKit measures a line.
    right: f32,
    /// The line's head indent.
    indent: f32,
    runs: Vec<PlacedRun>,
    fills: Vec<PlacedFill>,
    /// Where the line's text starts in the segment.
    text_start: usize,
}

/// A line placed in the layout, for truncating it later.
struct Placed {
    segment: usize,
    /// The run cursor (see [`runs_in`]) as it was for the line's segment.
    cursor: usize,
    text_start: usize,
    first_in_paragraph: bool,
    top: f32,
    runs: usize,
    fills: usize,
}

pub(crate) fn compute(ctx: &mut Ctx, text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> TextLayout {
    let mut out = TextLayout::default();
    let mut rights = Vec::new();
    let mut placed: Option<Placed> = None;
    let mut bottom = 0.0f32;
    let mut previous: Option<&Paragraph> = None;
    let mut cut_short = false;
    let mut cursor = 0;
    let segments = segments(text);
    'segments: for (index, seg) in segments.iter().enumerate() {
        let seg_cursor = cursor;
        let seg_runs = runs_in(runs, &mut cursor, seg.start, seg.end);
        let para = &attrs[seg_runs[0].attrs as usize].paragraph;
        let seg_text = &text[seg.start..seg.end];
        let lines = segment_lines(ctx, seg_text, attrs, &seg_runs, para, seg.first_in_paragraph, opts, None);
        for (i, line) in lines.into_iter().enumerate() {
            let mut top = bottom;
            if let Some(prev) = previous {
                top += prev.line_spacing as f32;
                if i == 0 && seg.first_in_paragraph {
                    top += (prev.paragraph_spacing + para.paragraph_spacing_before) as f32;
                }
                // The first line is kept however little height there is,
                // as AppKit keeps it; the others only if they fit whole.
                if top + line.height > opts.height + 1e-3 {
                    cut_short = true;
                    break 'segments;
                }
            } else {
                out.first_descent = line.descent;
            }
            placed = Some(Placed {
                segment: index,
                cursor: seg_cursor,
                text_start: line.text_start,
                first_in_paragraph: i == 0 && seg.first_in_paragraph,
                top,
                runs: out.runs.len(),
                fills: out.fills.len(),
            });
            rights.push(line.right);
            bottom = top + line.height;
            place(&mut out, line, top);
            previous = Some(para);
            if !opts.all_lines {
                break 'segments;
            }
        }
    }
    if cut_short
        && opts.truncate_last
        && let Some(p) = placed
    {
        let seg = &segments[p.segment];
        let start = seg.start + p.text_start;
        // The rest of the line's paragraph goes on one line, ending in an
        // ellipsis if it doesn't fit, and also whenever any text comes
        // after the paragraph, if only an empty line: AppKit shows that
        // something was cut off. (A separator ending the text isn't.)
        let more = segments.get(p.segment + 1).is_some_and(|next| next.start < text.len());
        let mut from = p.cursor;
        let rest_runs = runs_in(runs, &mut from, start, seg.end);
        let para = &attrs[rest_runs[0].attrs as usize].paragraph;
        let lines =
            segment_lines(ctx, &text[start..seg.end], attrs, &rest_runs, para, p.first_in_paragraph, opts, Some(more));
        if let Some(line) = lines.into_iter().next() {
            out.runs.truncate(p.runs);
            out.fills.truncate(p.fills);
            rights.pop();
            rights.push(line.right);
            place(&mut out, line, p.top);
        }
    }
    out.height = bottom;
    out.width = rights.into_iter().fold(0.0, f32::max).min(opts.width).max(0.0);
    out
}

fn place(out: &mut TextLayout, line: Line, top: f32) {
    out.runs.extend(line.runs.into_iter().map(|r| PlacedRun { y: r.y + top, ..r }));
    out.fills.extend(line.fills.into_iter().map(|mut f| {
        f.rect[1] += top;
        f.rect[3] += top;
        f
    }));
}

/// The lines of one segment. `tail` truncates it to one line with an
/// ellipsis at the end, whatever the paragraph style says: if the text
/// doesn't fit, or always if `tail` is `Some(true)`.
#[allow(clippy::too_many_arguments)]
fn segment_lines(
    ctx: &mut Ctx,
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    para: &Paragraph,
    first_in_paragraph: bool,
    opts: &Options,
    tail: Option<bool>,
) -> Vec<Line> {
    let tail_indent = para.tail_indent as f32;
    let right = if tail_indent > 0.0 { tail_indent.min(opts.width) } else { opts.width + tail_indent };
    let first_indent = if first_in_paragraph { para.first_line_head_indent } else { para.head_indent }.max(0.0) as f32;
    let rest_indent = para.head_indent.max(0.0) as f32;
    let more = tail == Some(true);
    if text.is_empty() && !more {
        return vec![empty_line(&attrs[runs[0].attrs as usize], para, opts)];
    }
    let mode = if tail.is_some() { LineBreak::TruncateTail } else { para.line_break };
    let wraps = opts.all_lines && tail.is_none() && matches!(mode, LineBreak::WordWrap | LineBreak::CharWrap);
    let truncates = matches!(mode, LineBreak::TruncateHead | LineBreak::TruncateTail | LineBreak::TruncateMiddle)
        && right.is_finite();
    let settings = Settings { mode, wraps, direction: para.direction };

    let (mut layout, shift) = build(ctx, text, attrs, runs, &settings);
    break_and_tab(&mut layout, text, shift, para, right, first_indent, rest_indent);
    let avail = right - first_indent;
    if more || (truncates && content_width(&layout) > avail + 0.01) {
        let clusters = clusters(&layout, shift);
        for extra in 0..4 {
            let (short, short_runs) = elide(ctx, text, attrs, runs, &clusters, avail, mode, extra, &settings);
            let (mut short_layout, short_shift) = build(ctx, &short, attrs, &short_runs, &settings);
            break_and_tab(&mut short_layout, &short, short_shift, para, right, first_indent, rest_indent);
            if content_width(&short_layout) <= avail + 0.01 || extra == 3 {
                short_layout.align(alignment(para.alignment), AlignmentOptions::default());
                let mut lines = extract(&short_layout, short_shift, attrs, runs[0].attrs, para, opts);
                // Where the lines start refers to the original text.
                lines.iter_mut().for_each(|l| l.text_start = 0);
                return lines;
            }
        }
    }
    layout.align(alignment(para.alignment), AlignmentOptions::default());
    let mut lines = extract(&layout, shift, attrs, runs[0].attrs, para, opts);
    ctx.scratch = layout;
    // AppKit counts indents in the width only of text that wraps, and no
    // line reaches past the tail indent. Justified lines, all but the
    // paragraph's last, reach all the way.
    let wrapped = lines.len() > 1;
    let justified = para.alignment == Align::Justified;
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter_mut().enumerate() {
        let reach = if justified && i < last { f32::INFINITY } else { line.right };
        line.right = (reach + if wrapped { line.indent } else { 0.0 }).min(right);
    }
    lines
}

struct Settings {
    mode: LineBreak,
    wraps: bool,
    direction: Direction,
}

fn alignment(align: Align) -> Alignment {
    match align {
        Align::Natural => Alignment::Start,
        Align::Left => Alignment::Left,
        Align::Right => Alignment::Right,
        Align::Center => Alignment::Center,
        Align::Justified => Alignment::Justify,
    }
}

/// A parley layout of `text`, and how many bytes of direction mark precede
/// the text in it.
fn build(ctx: &mut Ctx, text: &str, attrs: &[Attrs], runs: &[Run], settings: &Settings) -> (Layout<Brush>, usize) {
    // A leading mark sets an explicit base direction; parley otherwise
    // takes it from the first strong character, as "natural" asks.
    let mark = match settings.direction {
        Direction::Natural => "",
        Direction::LeftToRight => "\u{200E}",
        Direction::RightToLeft => "\u{200F}",
    };
    // Control characters become spaces too small to see, rather than
    // missing-glyph boxes; after each tab, a box reaches the next tab stop
    // (see `break_and_tab`).
    let controls = text.bytes().any(|b| b < 0x20 || b == 0x7f);
    let full: Cow<'_, str> = match (mark.is_empty(), controls) {
        (true, false) => Cow::Borrowed(text),
        _ => Cow::Owned(mark.chars().chain(text.chars().map(|c| if c.is_ascii_control() { ' ' } else { c })).collect()),
    };
    let full = &*full;
    let shift = mark.len();
    // Runs cut where emoji and control characters start and end:
    // (start, end, attributes, kind).
    let mut special: Vec<(usize, usize, Piece)> =
        emoji_ranges(text).into_iter().map(|(a, b)| (a, b, Piece::Emoji)).collect();
    if controls {
        special.extend(
            text.bytes().enumerate().filter(|(_, b)| *b < 0x20 || *b == 0x7f).map(|(i, _)| (i, i + 1, Piece::Hidden)),
        );
        special.sort_unstable_by_key(|p| p.0);
        // parley wants its style runs one after the other, so a range may
        // not overlap the one before it; what does is cut off.
        let mut end = 0;
        special.retain_mut(|p| {
            p.0 = p.0.max(end);
            end = end.max(p.1);
            p.0 < p.1
        });
    }
    // Both lists are in order, so one pass over the specials serves every
    // run: `next` is the first special that doesn't end before the run.
    let mut pieces: Vec<(usize, usize, u32, Piece)> = Vec::with_capacity(runs.len() + 2 * special.len());
    let mut next = 0;
    for run in runs {
        let mut at = run.start;
        while next < special.len() && special[next].1 <= run.start {
            next += 1;
        }
        for &(p0, p1, kind) in special[next..].iter().take_while(|p| p.0 < run.end) {
            let (p0, p1) = (p0.max(at), p1.min(run.end));
            if p1 <= p0 {
                continue;
            }
            if p0 > at {
                pieces.push((at, p0, run.attrs, Piece::Text));
            }
            pieces.push((p0, p1, run.attrs, kind));
            at = p1;
        }
        if at < run.end || run.start == run.end {
            pieces.push((at, run.end, run.attrs, Piece::Text));
        }
    }
    let mut used: Vec<(u32, Piece)> = pieces.iter().map(|p| (p.2, p.3)).collect();
    used.sort_unstable();
    used.dedup();
    let features: Vec<Vec<FontFeature>> = used.iter().map(|&(i, _)| attrs[i as usize].features()).collect();
    let families: Vec<[FontFamilyName<'_>; 2]> = used
        .iter()
        .map(|&(i, _)| {
            let family = FontFamilyName::Named(Cow::Borrowed(&*attrs[i as usize].font.face.family));
            [FontFamilyName::Generic(GenericFamily::Emoji), family]
        })
        .collect();
    let mut builder = ctx.lcx.style_run_builder(&mut ctx.fcx, full, 1.0, false);
    builder.reserve(used.len(), pieces.len());
    let styles: Vec<u16> = used
        .iter()
        .zip(features.iter().zip(&families))
        .map(|(&(i, kind), (features, families))| {
            let a = &attrs[i as usize];
            match kind {
                Piece::Text => {
                    builder.push_style(text_style(a, i, FontFamily::Single(families[1].clone()), features, settings))
                }
                // Emoji look for a color emoji face first, as on macOS,
                // where they'd otherwise take the text's face's plain
                // glyphs.
                Piece::Emoji => {
                    builder.push_style(text_style(a, i, FontFamily::List(Cow::Borrowed(families)), features, settings))
                }
                Piece::Hidden => {
                    let style = text_style(a, i, FontFamily::Single(families[1].clone()), features, settings);
                    builder.push_style(TextStyle { font_size: HIDDEN_SIZE, letter_spacing: 0.0, ..style })
                }
            }
        })
        .collect();
    // Each piece starts where the one before ended (the first at the
    // direction mark), and the last reaches the end: parley asserts both,
    // and runs that don't quite cover the text mustn't make it panic.
    let mut cursor = 0;
    for &(_, end, index, kind) in &pieces {
        let style = styles[used.binary_search(&(index, kind)).unwrap_or(0)];
        let end = (end + shift).clamp(cursor, full.len());
        builder.push_style_run(style, cursor..end);
        cursor = end;
    }
    if cursor < full.len() {
        builder.push_style_run(styles.last().copied().unwrap_or(0), cursor..full.len());
    }
    if controls {
        for (id, (at, _)) in text.match_indices('\t').enumerate() {
            let index = at + 1 + shift;
            builder.push_inline_box(InlineBox {
                id: id as u64,
                kind: InlineBoxKind::InFlow,
                index,
                width: 0.0,
                height: 0.0,
            });
        }
    }
    let mut layout = std::mem::take(&mut ctx.scratch);
    builder.build_into(&mut layout, full);
    (layout, shift)
}

/// What a piece of a run holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Piece {
    Text,
    Emoji,
    /// A control character, drawn as a space too small to see.
    Hidden,
}

/// The font size control characters are laid out at: small enough that
/// their space takes no room, large enough to shape.
const HIDDEN_SIZE: f32 = 1.0 / 64.0;

/// Byte ranges of `text` to draw as emoji: characters shown as emoji by
/// default, others asked to be with U+FE0F, flags and keycaps, with the
/// modifiers, joiners and tags that continue their sequences.
fn emoji_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    if text.is_ascii() {
        return out;
    }
    let presentation = CodePointSetData::new::<EmojiPresentation>();
    let pictographic = CodePointSetData::new::<ExtendedPictographic>();
    let regional = |c: char| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let next = chars.peek().map(|&(_, n)| n);
        let keycap = matches!(c, '0'..='9' | '#' | '*')
            && next == Some('\u{FE0F}')
            && text[i + 1..].starts_with("\u{FE0F}\u{20E3}");
        if !(presentation.contains(c)
            || regional(c)
            || keycap
            || (pictographic.contains(c) && next == Some('\u{FE0F}')))
        {
            continue;
        }
        let mut end = i + c.len_utf8();
        let mut flag = regional(c);
        while let Some(&(j, n)) = chars.peek() {
            let joins = n == '\u{200D}';
            let continues = joins
                || matches!(n, '\u{FE0F}' | '\u{20E3}' | '\u{1F3FB}'..='\u{1F3FF}' | '\u{E0020}'..='\u{E007F}')
                || (flag && regional(n));
            if !continues {
                break;
            }
            flag = false;
            chars.next();
            end = j + n.len_utf8();
            // A joiner takes the character after it into the sequence, but
            // never a control character, which is laid out apart.
            if joins
                && let Some(&(k, m)) = chars.peek()
                && !m.is_control()
            {
                chars.next();
                end = k + m.len_utf8();
            }
        }
        match out.last_mut() {
            Some(last) if last.1 == i => last.1 = end,
            _ => out.push((i, end)),
        }
    }
    out
}

fn text_style<'a>(
    attrs: &'a Attrs,
    index: u32,
    font_family: FontFamily<'a>,
    features: &'a [FontFeature],
    settings: &Settings,
) -> TextStyle<'a, 'a, Brush> {
    let face = &attrs.font.face;
    TextStyle {
        font_family,
        font_size: attrs.font.size,
        font_width: FontWidth::from_ratio(face.stretch),
        font_style: if face.italic { FontStyle::Italic } else { FontStyle::Normal },
        font_weight: FontWeight::new(face.weight),
        font_features: FontFeatures::List(Cow::Borrowed(features)),
        brush: Brush(index),
        letter_spacing: attrs.kern.unwrap_or(0.0),
        word_break: if settings.mode == LineBreak::CharWrap { WordBreak::BreakAll } else { WordBreak::Normal },
        overflow_wrap: OverflowWrap::Anywhere,
        text_wrap_mode: if settings.wraps { TextWrapMode::Wrap } else { TextWrapMode::NoWrap },
        ..TextStyle::default()
    }
}

/// Break lines, then size the tab boxes one after the other: each reaches
/// from where it lands to where the text after it lines up on the next tab
/// stop, which moves what follows. Past the last stop, tabs go every
/// `defaultTabInterval`, or nowhere if that is 0, as in AppKit.
fn break_and_tab(
    layout: &mut Layout<Brush>,
    text: &str,
    shift: usize,
    para: &Paragraph,
    right: f32,
    first_indent: f32,
    rest_indent: f32,
) {
    break_lines(layout, right, first_indent, rest_indent);
    if layout.inline_boxes().is_empty() {
        return;
    }
    let stops = tab_stops(para);
    let tabs: Vec<usize> = text.match_indices('\t').map(|(at, _)| at).collect();
    for (id, &at) in tabs.iter().enumerate() {
        let id = id as u64;
        let Some((x, end)) = tab_position(layout, id) else { continue };
        let width = match stops.iter().find(|t| t.location > x + 0.001) {
            Some(Tab { location, kind: TabKind::Left }) => location - x,
            // The text up to the next tab or the line's end ends on the
            // stop, or centers on it.
            Some(Tab { location, kind: TabKind::Right }) => (location - end).max(0.0),
            Some(Tab { location, kind: TabKind::Center }) => (location - (x + end) / 2.0).max(0.0),
            // The decimal point goes on the stop; without one, the end.
            Some(Tab { location, kind: TabKind::Decimal }) => {
                let segment = &text[at + 1..tabs.get(id as usize + 1).copied().unwrap_or(text.len())];
                match segment.find('.') {
                    Some(dot) => {
                        let (from, to) = (at + 1 + shift, at + 1 + dot + shift);
                        (location - x - advance_between(layout, from, to)).max(0.0)
                    }
                    None => (location - end).max(0.0),
                }
            }
            None if para.default_tab_interval > 0.0 => {
                let interval = para.default_tab_interval as f32;
                ((x / interval).floor() + 1.0) * interval - x
            }
            None => 0.0,
        };
        if let Some(tab) = layout.inline_boxes_mut().iter_mut().find(|b| b.id == id) {
            tab.width = width;
        }
        break_lines(layout, right, first_indent, rest_indent);
    }
}

/// The advance of the clusters of `layout` from byte `from` up to `to`.
fn advance_between(layout: &Layout<Brush>, from: usize, to: usize) -> f32 {
    let mut total = 0.0;
    for line in layout.lines() {
        for run in line.runs() {
            for cluster in run.clusters() {
                let range = cluster.text_range();
                if range.start >= from && range.end <= to {
                    total += cluster.advance();
                }
            }
        }
    }
    total
}

/// Where tab box `id` is, and where the text after it ends: at the next
/// tab box on its line or at the line's end.
fn tab_position(layout: &Layout<Brush>, id: u64) -> Option<(f32, f32)> {
    for line in layout.lines() {
        let mut found = None;
        for item in line.items() {
            match (item, found) {
                (PositionedLayoutItem::InlineBox(b), None) if b.id == id => found = Some(b.x),
                (PositionedLayoutItem::InlineBox(b), Some(x)) => return Some((x, b.x)),
                _ => {}
            }
        }
        if let Some(x) = found {
            let m = line.metrics();
            return Some((x, m.inline_min_coord + m.offset + m.advance - m.trailing_whitespace));
        }
    }
    None
}

/// Break lines, the first `first_indent` from the leading edge and the
/// rest `rest_indent`, all ending at `right`.
fn break_lines(layout: &mut Layout<Brush>, right: f32, first_indent: f32, rest_indent: f32) {
    let mut breaker = layout.break_lines();
    let finite = right.is_finite();
    breaker.state_mut().set_layout_max_advance(if finite { right.max(0.0) } else { f32::INFINITY });
    let mut indent = first_indent;
    loop {
        let avail = if finite { (right - indent).max(0.0) } else { f32::INFINITY };
        let state = breaker.state_mut();
        state.set_line_x(if finite { indent.min(right.max(0.0)) } else { indent });
        state.set_line_max_advance(avail);
        match breaker.break_next() {
            Some(parley::YieldData::LineBreak(_)) => indent = rest_indent,
            Some(_) => {}
            None => break,
        }
    }
    breaker.finish();
}

/// The widest line's content, trailing whitespace left out.
fn content_width(layout: &Layout<Brush>) -> f32 {
    layout.lines().map(|l| l.metrics().advance - l.metrics().trailing_whitespace).fold(0.0, f32::max)
}

/// Clusters in logical order: byte range in the original text and advance.
fn clusters(layout: &Layout<Brush>, shift: usize) -> Vec<(usize, usize, f32)> {
    let mut out = Vec::new();
    for line in layout.lines() {
        for run in line.runs() {
            for cluster in run.clusters() {
                let range = cluster.text_range();
                if range.start >= shift {
                    out.push((range.start - shift, range.end - shift, cluster.advance()));
                }
            }
        }
    }
    out.sort_unstable_by_key(|c| c.0);
    out
}

/// `text` with what doesn't fit in `avail` replaced by an ellipsis, as the
/// truncating `mode` asks, giving up `extra` more clusters than the
/// measurement says.
#[allow(clippy::too_many_arguments)]
fn elide(
    ctx: &mut Ctx,
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    clusters: &[(usize, usize, f32)],
    avail: f32,
    mode: LineBreak,
    extra: usize,
    settings: &Settings,
) -> (String, Vec<Run>) {
    let attrs_at =
        |byte: usize| runs.iter().find(|r| r.start <= byte && byte < r.end).map_or(runs[0].attrs, |r| r.attrs);
    let ellipsis_width = |ctx: &mut Ctx, index: u32| {
        let run = [Run { start: 0, end: '…'.len_utf8(), attrs: index }];
        let (mut layout, _) = build(ctx, "…", attrs, &run, &Settings { wraps: false, ..*settings });
        layout.break_all_lines(None);
        layout.width()
    };
    let n = clusters.len();
    let (mut head, mut tail) = (0, 0); // clusters kept at each end
    match mode {
        LineBreak::TruncateHead => {
            let budget = avail - ellipsis_width(ctx, attrs_at(0));
            let mut used = 0.0;
            while tail < n && used + clusters[n - 1 - tail].2 <= budget {
                used += clusters[n - 1 - tail].2;
                tail += 1;
            }
            tail = tail.saturating_sub(extra);
        }
        LineBreak::TruncateMiddle => {
            let budget = avail - ellipsis_width(ctx, attrs_at(0));
            let mut used = 0.0;
            loop {
                let mut grew = false;
                if head + tail < n && used + clusters[head].2 <= budget {
                    used += clusters[head].2;
                    head += 1;
                    grew = true;
                }
                if head + tail < n && used + clusters[n - 1 - tail].2 <= budget {
                    used += clusters[n - 1 - tail].2;
                    tail += 1;
                    grew = true;
                }
                if !grew {
                    break;
                }
            }
            for i in 0..extra {
                if i % 2 == 0 { tail = tail.saturating_sub(1) } else { head = head.saturating_sub(1) }
            }
        }
        _ => {
            let last = clusters.iter().rev().find(|c| !text[c.0..c.1].trim().is_empty()).map_or(0, |c| c.0);
            let budget = avail - ellipsis_width(ctx, attrs_at(last));
            let mut used = 0.0;
            while head < n && used + clusters[head].2 <= budget {
                used += clusters[head].2;
                head += 1;
            }
            head = head.saturating_sub(extra);
        }
    }
    let head_end = if head == 0 { 0 } else { clusters[head - 1].1 };
    let tail_start = if tail == 0 { text.len() } else { clusters[n - tail].0 };
    let kept_head = text[..head_end].trim_end();
    let kept_tail = if head == 0 && mode != LineBreak::TruncateTail {
        text[tail_start..].trim_start()
    } else {
        &text[tail_start..]
    };
    let tail_from = text.len() - kept_tail.len();
    let ellipsis_attrs = if kept_head.is_empty() {
        attrs_at(tail_from.min(text.len().saturating_sub(1)))
    } else {
        attrs_at(kept_head.len() - 1)
    };
    let mut short = String::with_capacity(kept_head.len() + 3 + kept_tail.len());
    short.push_str(kept_head);
    short.push('…');
    short.push_str(kept_tail);
    let dots = kept_head.len()..kept_head.len() + '…'.len_utf8();
    let mut out = Vec::new();
    for r in runs {
        if r.start < kept_head.len() {
            out.push(Run { start: r.start, end: r.end.min(kept_head.len()), attrs: r.attrs });
        }
    }
    out.push(Run { start: dots.start, end: dots.end, attrs: ellipsis_attrs });
    for r in runs {
        if r.end > tail_from {
            let start = r.start.max(tail_from) - tail_from + dots.end;
            out.push(Run { start, end: r.end - tail_from + dots.end, attrs: r.attrs });
        }
    }
    (short, out)
}

/// A line with nothing on it, as tall as its font makes it.
fn empty_line(attrs: &Attrs, para: &Paragraph, opts: &Options) -> Line {
    let m = &attrs.font.face.metrics;
    let size = attrs.font.size;
    let (height, descent) = line_height(m.ascent * size, -m.descent * size, m.leading * size, para, opts);
    Line { height, descent, right: 0.0, indent: 0.0, runs: Vec::new(), fills: Vec::new(), text_start: 0 }
}

/// A line's height and its descent below the baseline, from its fonts'
/// largest ascent, descent and leading.
fn line_height(ascent: f32, descent: f32, leading: f32, para: &Paragraph, opts: &Options) -> (f32, f32) {
    let (ascent, descent) = (ascent.round(), descent.round());
    let mut height = ascent + descent + if opts.font_leading { leading.max(0.0) } else { 0.0 };
    if para.line_height_multiple > 0.0 {
        height *= para.line_height_multiple as f32;
    }
    if para.min_line_height > 0.0 {
        height = height.max(para.min_line_height as f32);
    }
    if para.max_line_height > 0.0 {
        height = height.min(para.max_line_height as f32);
    }
    (height, descent)
}

/// A glyph run found on a line, before the line's height is known.
struct Pending {
    font: u32,
    size: f32,
    x: f32,
    advance: f32,
    attrs: u32,
    metrics: parley::RunMetrics,
    glyphs: Arc<[Glyph]>,
}

/// The lines of a broken and aligned layout. `base` names the attributes
/// whose font sizes a line with no glyphs.
fn extract(
    layout: &Layout<Brush>,
    shift: usize,
    attrs: &[Attrs],
    base: u32,
    para: &Paragraph,
    opts: &Options,
) -> Vec<Line> {
    let mut lines = Vec::with_capacity(layout.len());
    let mut pending: Vec<Pending> = Vec::new();
    for line in layout.lines() {
        let m = line.metrics();
        let (mut ascent, mut descent, mut leading) = (0.0f32, 0.0f32, 0.0f32);
        pending.clear();
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else { continue };
            let run = glyph_run.run();
            if run.font_size() <= HIDDEN_SIZE {
                continue;
            }
            let index = glyph_run.style().brush.0;
            let a = &attrs[index as usize];
            let rm = *run.metrics();
            // Raised text makes room above, lowered text below.
            ascent = ascent.max(rm.ascent + a.baseline_offset.max(0.0));
            descent = descent.max(rm.descent - a.baseline_offset.min(0.0));
            leading = leading.max(rm.leading);
            let mut pen = 0.0;
            let glyphs: Arc<[Glyph]> = glyph_run
                .glyphs()
                .map(|g| {
                    let glyph = Glyph { id: g.id, x: pen + g.x, y: g.y };
                    pen += g.advance;
                    glyph
                })
                .collect();
            pending.push(Pending {
                font: fonts::register(run.font(), run.normalized_coords(), &run.synthesis()),
                size: run.font_size(),
                x: glyph_run.offset(),
                advance: glyph_run.advance(),
                attrs: index,
                metrics: rm,
                glyphs,
            });
        }
        if pending.is_empty() {
            let a = &attrs[base as usize];
            let fm = &a.font.face.metrics;
            ascent = fm.ascent * a.font.size;
            descent = -fm.descent * a.font.size;
            leading = fm.leading * a.font.size;
        }
        let (height, desc) = line_height(ascent, descent, leading, para, opts);
        let baseline = height - desc;
        let mut out = Line {
            height,
            descent: desc,
            right: m.advance,
            indent: m.inline_min_coord,
            runs: Vec::new(),
            fills: Vec::new(),
            text_start: line.text_range().start.saturating_sub(shift),
        };
        for p in pending.drain(..) {
            let a = &attrs[p.attrs as usize];
            let y = baseline - a.baseline_offset;
            if let Some(background) = a.background {
                out.fills.push(PlacedFill {
                    rect: [p.x, 0.0, p.x + p.advance, height],
                    color: background,
                    background: true,
                });
            }
            for (decoration, offset, size) in [
                (a.underline, p.metrics.underline_offset, p.metrics.underline_size),
                (a.strikethrough, p.metrics.strikethrough_offset, p.metrics.strikethrough_size),
            ] {
                decorate(&mut out.fills, decoration, a.color, p.x, p.advance, y - offset, size);
            }
            if !p.glyphs.is_empty() {
                out.runs.push(PlacedRun { font: p.font, size: p.size, x: p.x, y, glyphs: p.glyphs, color: a.color });
            }
        }
        lines.push(out);
    }
    lines
}

/// Add the rectangles of an underline or strikethrough whose top is at `y`.
fn decorate(fills: &mut Vec<PlacedFill>, d: Decoration, text: Color, x: f32, width: f32, y: f32, size: f32) {
    // The low byte is the line's style: single, thick or double.
    let style = d.style & 0xff;
    if style == 0 || width <= 0.0 {
        return;
    }
    let color = d.color.unwrap_or(text);
    let thickness = size.max(0.5) * if style & 0x0f == 0x02 { 2.0 } else { 1.0 };
    let rect = |y: f32| PlacedFill { rect: [x, y, x + width, y + thickness], color, background: false };
    fills.push(rect(y));
    if style & 0x0f == 0x09 {
        fills.push(rect(y + 2.0 * thickness.max(1.0)));
    }
}
