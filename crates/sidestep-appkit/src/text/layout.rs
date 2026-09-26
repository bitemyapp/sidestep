//! Laying text out the way AppKit's string drawing does: attribute runs and
//! paragraph styles in, lines of positioned glyph runs, backgrounds and
//! decorations out.
//!
//! parley does the Unicode work: bidi, line breaking, shaping (harfrust)
//! and font fallback. This module decides where the lines go:
//!
//! - Text is cut into segments at paragraph separators and at line
//!   separators (U+2028, and NEL, as AppKit takes it). Each segment is one
//!   parley layout, and a separator never reaches the shaper; a paragraph's
//!   base direction is found once for all its segments, and a segment
//!   after a line separator starts with the bidi context of the text
//!   before it.
//! - A line is as tall as the ascents plus the descents of its fonts, each
//!   rounded to a whole point, as AppKit's typesetter does; the fonts'
//!   leading is added only when the caller asks for it. A baseline offset
//!   adds its points above or below, unrounded. Then the paragraph style's
//!   `lineHeightMultiple`, minimum and maximum apply, extra height going
//!   above the text.
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
//! glyphs) the old one is dropped. Laid-out text is plain data behind an
//! `Arc`, so any thread may keep and draw it.
//!
//! [`segment_lines`] is also the core of `text::lines`, the line, cluster
//! and caret layout TextKit builds on: asked for them, each line keeps its
//! clusters in visual order with their byte ranges and positions.

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::sync::Arc;

use icu_properties::props::{
    BidiClass, BidiMirroringGlyph, BidiPairedBracketType, EmojiPresentation, ExtendedPictographic, GraphemeClusterBreak,
};
use icu_properties::{CodePointMapData, CodePointSetData};
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

impl LineBreak {
    pub fn truncates(self) -> bool {
        matches!(self, LineBreak::TruncateHead | LineBreak::TruncateTail | LineBreak::TruncateMiddle)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Direction {
    Natural,
    LeftToRight,
    RightToLeft,
}

/// A strong direction, as the bidi algorithm classes characters: what a
/// paragraph's direction comes from, and what numbers and neutrals after
/// a strong character resolve by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Strong {
    Left,
    Right,
    /// An Arabic letter: right to left, and European digits after it
    /// become Arabic ones.
    Arabic,
}

/// What a character is to [`first_strong`], [`last_strong`] and the
/// other questions below.
enum Bidi {
    Strong(Strong),
    /// A number, which resolves by the strong text before it; an Arabic
    /// number always runs right to left.
    Number {
        arabic: bool,
    },
    Isolate,
    PopIsolate,
    /// An explicit embedding or override, or the end of one.
    Explicit,
    Neutral,
}

fn bidi(c: char) -> Bidi {
    // ASCII letters are strong left to right, digits are numbers, and
    // nothing else in ASCII is strong or directs.
    if c.is_ascii() {
        return if c.is_ascii_alphabetic() {
            Bidi::Strong(Strong::Left)
        } else if c.is_ascii_digit() {
            Bidi::Number { arabic: false }
        } else {
            Bidi::Neutral
        };
    }
    let class = CodePointMapData::<BidiClass>::new().get(c);
    match class {
        BidiClass::LeftToRight => Bidi::Strong(Strong::Left),
        BidiClass::RightToLeft => Bidi::Strong(Strong::Right),
        BidiClass::ArabicLetter => Bidi::Strong(Strong::Arabic),
        BidiClass::EuropeanNumber => Bidi::Number { arabic: false },
        BidiClass::ArabicNumber => Bidi::Number { arabic: true },
        BidiClass::LeftToRightIsolate | BidiClass::RightToLeftIsolate | BidiClass::FirstStrongIsolate => Bidi::Isolate,
        BidiClass::PopDirectionalIsolate => Bidi::PopIsolate,
        BidiClass::LeftToRightEmbedding
        | BidiClass::RightToLeftEmbedding
        | BidiClass::LeftToRightOverride
        | BidiClass::RightToLeftOverride
        | BidiClass::PopDirectionalFormat => Bidi::Explicit,
        _ => Bidi::Neutral,
    }
}

/// The first strong character of `text` outside isolates, as the bidi
/// algorithm finds a paragraph's direction.
pub(crate) fn first_strong(text: &str) -> Option<Strong> {
    let mut isolates = 0u32;
    for c in text.chars() {
        match bidi(c) {
            Bidi::Isolate => isolates += 1,
            Bidi::PopIsolate => isolates = isolates.saturating_sub(1),
            Bidi::Strong(s) if isolates == 0 => return Some(s),
            _ => {}
        }
    }
    None
}

/// The last strong character of `text` outside isolates: what numbers and
/// neutrals at the start of the text after it resolve by. Text that ends
/// inside an isolate gives none. (Explicit embeddings open at the end
/// aren't followed.)
pub(crate) fn last_strong(text: &str) -> Option<Strong> {
    let mut isolates = 0u32;
    for c in text.chars().rev() {
        match bidi(c) {
            Bidi::PopIsolate => isolates += 1,
            Bidi::Isolate if isolates == 0 => return None,
            Bidi::Isolate => isolates -= 1,
            Bidi::Strong(s) if isolates == 0 => return Some(s),
            _ => {}
        }
    }
    None
}

/// Whether `text` has neither strong characters nor numbers: its neutrals
/// resolve by what comes after it.
pub(crate) fn all_neutral(text: &str) -> bool {
    text.chars().all(|c| matches!(bidi(c), Bidi::Neutral | Bidi::Isolate | Bidi::PopIsolate | Bidi::Explicit))
}

/// Whether every character of `text` goes the way of a paragraph running
/// right to left (`rtl`) or not, whatever is around it: nothing strong the
/// other way, no numbers in right-to-left text (they run left to right),
/// no Arabic numbers, and nothing explicit.
pub(crate) fn one_way(text: &str, rtl: bool) -> bool {
    (!rtl && text.is_ascii())
        || text.chars().all(|c| match bidi(c) {
            Bidi::Strong(Strong::Left) | Bidi::Number { arabic: false } => !rtl,
            Bidi::Strong(_) => rtl,
            Bidi::Neutral => true,
            Bidi::Number { arabic: true } | Bidi::Isolate | Bidi::PopIsolate | Bidi::Explicit => false,
        })
}

/// Whether `text` has paired brackets, which the bidi algorithm resolves
/// together however far apart they are (its rule N0), or explicit
/// embeddings, overrides or isolates, which reach to their ends.
pub(crate) fn pairs_or_embeds(text: &str) -> bool {
    let brackets = CodePointMapData::<BidiMirroringGlyph>::new();
    text.chars().any(|c| {
        if c.is_ascii() {
            return matches!(c, '(' | ')' | '[' | ']' | '{' | '}');
        }
        match bidi(c) {
            Bidi::Isolate | Bidi::PopIsolate | Bidi::Explicit => true,
            Bidi::Neutral => brackets.get(c).paired_bracket_type != BidiPairedBracketType::None,
            _ => false,
        }
    })
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
    /// The tab stops in the order they were given; `None` is AppKit's
    /// default twelve ([`tab_stops`]). A tab goes to the first stop in the
    /// list beyond it, as in AppKit, so an unsorted list keeps its order.
    /// A list lives as long as the styles and layouts that use it.
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

/// `NSUnderlineStyle` bits: the line's style in the low byte, the pattern
/// in the next, and whether to leave out the spaces between words.
mod underline {
    pub const DOUBLE: i64 = 0x09;
    pub const THICK: i64 = 0x02;
    pub const PATTERN: i64 = 0x0f00;
    pub const DOT: i64 = 0x0100;
    pub const DASH: i64 = 0x0200;
    pub const DASH_DOT: i64 = 0x0300;
    pub const DASH_DOT_DOT: i64 = 0x0400;
    pub const BY_WORD: i64 = 0x8000;
}

/// Outlined glyphs: `NSStrokeWidth`, a percentage of the font size
/// (positive strokes the outline alone, negative fills it too; 0 is plain
/// text), and `NSStrokeColor`, the text's own color if none.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Stroke {
    pub width: f32,
    pub color: Option<Color>,
}

/// A shadow cast by text, as `NSShadow` describes one: an offset in points
/// with y up (AppKit's convention whichever way the view faces), a blur
/// radius and a color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Shadow {
    pub offset: [f32; 2],
    pub blur: f32,
    pub color: Color,
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
    pub stroke: Stroke,
    /// `NSObliqueness`: the skew to slant glyphs by (the tangent of the
    /// angle), right for positive values.
    pub obliqueness: f32,
    pub shadow: Option<Shadow>,
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
            stroke: Stroke::default(),
            obliqueness: 0.0,
            shadow: None,
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
        // One word for the rarer attributes, which equality compares in
        // full: this runs for every string drawn.
        let rare = u64::from(self.stroke.width.to_bits()) | u64::from(self.obliqueness.to_bits()) << 32;
        h.write_u64(rare ^ u64::from(self.shadow.is_some()));
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

    /// Degrees to slant glyphs by for `NSObliqueness`.
    fn slant(&self) -> f32 {
        if self.obliqueness == 0.0 { 0.0 } else { self.obliqueness.atan().to_degrees() }
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
pub(crate) fn lay_out(text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> Arc<TextLayout> {
    match cached(text, attrs, runs, opts) {
        Ok(layout) => layout,
        Err(hash) => super::with_ctx(|ctx| {
            let layout = Arc::new(compute(ctx, text, attrs, runs, opts));
            let entry = Entry { text: text.into(), attrs: attrs.into(), runs: runs.into(), opts: *opts, layout };
            ctx.layouts.insert(hash, entry)
        }),
    }
}

/// `text` laid out, if this thread has it cached; if not, the key to lay it
/// out under (see [`job`]).
pub(crate) fn cached(text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> Result<Arc<TextLayout>, u64> {
    with_cached(text, attrs, runs, opts, |layout| layout.clone())
}

/// `f` with `text` laid out, if this thread has it cached, which spares
/// the layout's reference count; if not, the key to lay it out under.
/// `f` must not lay text out.
pub(crate) fn with_cached<R>(
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    opts: &Options,
    f: impl FnOnce(&Arc<TextLayout>) -> R,
) -> Result<R, u64> {
    let mut h = Fx::default();
    text.hash(&mut h);
    attrs.iter().for_each(|a| a.hash_into(&mut h));
    runs.hash(&mut h);
    opts.hash_into(&mut h);
    let hash = h.finish();
    super::with_ctx(|ctx| ctx.layouts.find(hash, text, attrs, runs, opts).map(f).ok_or(hash))
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
pub(crate) fn lay_out_all(jobs: Vec<Job>) -> Vec<Arc<TextLayout>> {
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
                    layout: Arc::new(layout),
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
    layout: Arc<TextLayout>,
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
    fn get(&mut self, hash: u64, text: &str, attrs: &[Attrs], runs: &[Run], opts: &Options) -> Option<Arc<TextLayout>> {
        self.find(hash, text, attrs, runs, opts).cloned()
    }

    /// The cached layout, moved to the newer generation if it was in the
    /// older.
    fn find(
        &mut self,
        hash: u64,
        text: &str,
        attrs: &[Attrs],
        runs: &[Run],
        opts: &Options,
    ) -> Option<&Arc<TextLayout>> {
        let fresh = self.new.get(&hash).and_then(|b| b.iter().position(|e| e.is(text, attrs, runs, opts)));
        if fresh.is_none() {
            let bucket = self.old.get_mut(&hash)?;
            let at = bucket.iter().position(|e| e.is(text, attrs, runs, opts))?;
            let entry = bucket.swap_remove(at);
            self.push(hash, entry);
        }
        let bucket = self.new.get(&hash)?;
        let at = fresh.unwrap_or(bucket.len() - 1);
        bucket.get(at).map(|e| &e.layout)
    }

    /// Cache `entry`, unless an equal one is cached already, and return the
    /// layout the cache keeps.
    fn insert(&mut self, hash: u64, entry: Entry) -> Arc<TextLayout> {
        match self.get(hash, &entry.text, &entry.attrs, &entry.runs, &entry.opts) {
            Some(layout) => layout,
            None => self.push(hash, entry),
        }
    }

    fn push(&mut self, hash: u64, entry: Entry) -> Arc<TextLayout> {
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
            '\n' | '\u{2029}' => (true, c.len_utf8()),
            '\r' if chars.peek().is_some_and(|&(_, n)| n == '\n') => {
                chars.next();
                (true, 2)
            }
            '\r' => (true, 1),
            // U+2028, and NEL, which AppKit also takes to end a line
            // within a paragraph.
            '\u{2028}' | '\u{85}' => (false, c.len_utf8()),
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
pub(crate) fn runs_in(runs: &[Run], from: &mut usize, start: usize, end: usize) -> Vec<Run> {
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

/// A cluster of a laid-out line: a byte range of the segment's text, where
/// it is and how wide, and which way it runs. A tab's cluster reaches to
/// where the text after it starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ByteCluster {
    pub start: usize,
    pub end: usize,
    /// The left edge, from the container's left.
    pub x: f32,
    pub advance: f32,
    pub rtl: bool,
}

/// A line of a segment, relative to its own top left corner.
// Only `text::lines` reads some fields, and only its tests use it yet.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LaidLine {
    pub height: f32,
    /// How far the baseline is above the line's bottom: the fonts'
    /// descent, rounded, plus any lowering.
    pub descent: f32,
    /// The fonts' ascent above the baseline, rounded, plus any raising;
    /// the line's height may add to it.
    pub ascent: f32,
    pub leading: f32,
    /// How far the text reaches, trailing whitespace included and the
    /// alignment's offset left out, as AppKit measures a line.
    pub right: f32,
    /// The line's head indent.
    pub indent: f32,
    /// Where the line's content starts, from the container's left: the
    /// indent and the alignment's offset.
    pub x: f32,
    /// The content's advance, trailing whitespace included, and that of
    /// the trailing whitespace.
    pub advance: f32,
    pub trailing: f32,
    pub runs: Vec<PlacedRun>,
    pub fills: Vec<PlacedFill>,
    /// Where the line's text starts and ends in the segment.
    pub text_start: usize,
    pub text_end: usize,
    /// The line's clusters in visual order, left to right, when asked for.
    pub clusters: Vec<ByteCluster>,
    /// For a truncated line, the bytes of the segment its ellipsis stands
    /// for.
    pub elided: Option<(usize, usize)>,
    /// The line ends inside a word too long for a line, where no break was
    /// allowed.
    pub forced: bool,
}

/// A line placed in the layout, for truncating it later.
struct Placed {
    segment: usize,
    /// The run cursor (see [`runs_in`]) as it was for the line's segment.
    cursor: usize,
    text_start: usize,
    first_in_paragraph: bool,
    /// Where its paragraph starts, and the paragraph's direction.
    paragraph: usize,
    direction: Direction,
    top: f32,
    runs: usize,
    fills: usize,
}

/// The direction to lay out the paragraph whose segments start at
/// `segments[at]` in: its style's, or, if that is natural and line
/// separators cut the paragraph, the one its first strong character gives,
/// found once for all its segments (each alone could find another). A
/// paragraph of one segment is left to find its own.
fn paragraph_direction(text: &str, segments: &[Segment], at: usize, style: Direction) -> Direction {
    if style != Direction::Natural || segments.get(at + 1).is_none_or(|s| s.first_in_paragraph) {
        return style;
    }
    resolved(&text[segments[at].start..paragraph_end(segments, at)], style)
}

/// Where the paragraph that segment `at` is in ends.
fn paragraph_end(segments: &[Segment], at: usize) -> usize {
    segments[at + 1..].iter().take_while(|s| !s.first_in_paragraph).last().map_or(segments[at].end, |s| s.end)
}

/// `style`, or for natural, the direction the first strong character of
/// the paragraph `text` gives.
fn resolved(text: &str, style: Direction) -> Direction {
    match (style, first_strong(text)) {
        (Direction::Natural, Some(Strong::Right | Strong::Arabic)) => Direction::RightToLeft,
        (Direction::Natural, _) => Direction::LeftToRight,
        (d, _) => d,
    }
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
    let (mut paragraph, mut direction) = (0, Direction::Natural);
    'segments: for (index, seg) in segments.iter().enumerate() {
        let seg_cursor = cursor;
        let seg_runs = runs_in(runs, &mut cursor, seg.start, seg.end);
        let para = &attrs[seg_runs[0].attrs as usize].paragraph;
        let seg_text = &text[seg.start..seg.end];
        // A segment after a line separator goes on in its paragraph's
        // direction, from the strong text before it.
        let context = if seg.first_in_paragraph {
            (paragraph, direction) = (seg.start, paragraph_direction(text, &segments, index, para.direction));
            None
        } else {
            last_strong(&text[paragraph..seg.start])
        };
        let req = Req { context, ..Req::lines(seg.first_in_paragraph, direction) };
        let lines = segment_lines(ctx, seg_text, attrs, &seg_runs, para, req, opts);
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
                paragraph,
                direction,
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
        // From inside its paragraph, the line keeps the paragraph's
        // direction and resolves by the strong text before it.
        let (direction, context) = if start > p.paragraph {
            let direction = resolved(&text[p.paragraph..paragraph_end(&segments, p.segment)], p.direction);
            (direction, last_strong(&text[p.paragraph..start]))
        } else {
            (p.direction, None)
        };
        let req =
            Req { tail: Some((LineBreak::TruncateTail, more)), context, ..Req::lines(p.first_in_paragraph, direction) };
        let lines = segment_lines(ctx, &text[start..seg.end], attrs, &rest_runs, para, req, opts);
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

fn place(out: &mut TextLayout, line: LaidLine, top: f32) {
    out.runs.extend(line.runs.into_iter().map(|r| PlacedRun { y: r.y + top, ..r }));
    out.fills.extend(line.fills.into_iter().map(|mut f| {
        f.rect[1] += top;
        f.rect[3] += top;
        f
    }));
}

/// How [`segment_lines`] lays a segment out.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Req {
    /// The segment starts a paragraph, so its first line takes the first
    /// line's indent.
    pub first_in_paragraph: bool,
    /// Truncate to one line with an ellipsis, as the mode says, whatever
    /// the paragraph style says: if the text doesn't fit, or, for the tail
    /// mode, always when the flag says something cut off follows.
    pub tail: Option<(LineBreak, bool)>,
    /// The base direction: the paragraph style's, or one the caller found
    /// for the whole paragraph.
    pub direction: Direction,
    /// For text laid out from inside a paragraph, the last strong
    /// character of the paragraph before it, which numbers, neutrals and
    /// brackets at its start resolve by; it needs an explicit `direction`.
    pub context: Option<Strong>,
    /// Record each line's clusters.
    pub clusters: bool,
}

impl Req {
    pub fn lines(first_in_paragraph: bool, direction: Direction) -> Req {
        Req { first_in_paragraph, tail: None, direction, context: None, clusters: false }
    }
}

/// The lines of one segment, as `req` asks.
pub(crate) fn segment_lines(
    ctx: &mut Ctx,
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    para: &Paragraph,
    req: Req,
    opts: &Options,
) -> Vec<LaidLine> {
    let tail_indent = para.tail_indent as f32;
    let right = if tail_indent > 0.0 { tail_indent.min(opts.width) } else { opts.width + tail_indent };
    let first_indent =
        if req.first_in_paragraph { para.first_line_head_indent } else { para.head_indent }.max(0.0) as f32;
    let rest_indent = para.head_indent.max(0.0) as f32;
    let more = matches!(req.tail, Some((LineBreak::TruncateTail, true)));
    if text.is_empty() && !more {
        return vec![empty_line(&attrs[runs[0].attrs as usize], para, opts)];
    }
    let mode = req.tail.map_or(para.line_break, |(mode, _)| mode);
    let wraps = opts.all_lines && req.tail.is_none() && matches!(mode, LineBreak::WordWrap | LineBreak::CharWrap);
    let truncates = mode.truncates() && right.is_finite();
    let settings = Settings { mode, wraps, direction: req.direction, context: req.context };

    let (mut layout, shift) = build(ctx, text, attrs, runs, &settings);
    break_and_tab(&mut layout, text, shift, para, right, first_indent, rest_indent);
    let avail = right - first_indent;
    if more || (truncates && content_width(&layout) > avail + 0.01) {
        let clusters = clusters(&layout, shift);
        for extra in 0..4 {
            let (short, short_runs, cut) = elide(ctx, text, attrs, runs, &clusters, avail, mode, extra, &settings);
            let (mut short_layout, short_shift) = build(ctx, &short, attrs, &short_runs, &settings);
            break_and_tab(&mut short_layout, &short, short_shift, para, right, first_indent, rest_indent);
            if content_width(&short_layout) <= avail + 0.01 || extra == 3 {
                short_layout.align(alignment(para.alignment), AlignmentOptions::default());
                let mut lines = extract(&short_layout, short_shift, attrs, runs[0].attrs, para, opts, req.clusters);
                // Positions in the text refer to the original text.
                for line in &mut lines {
                    line.text_start = cut.original(line.text_start);
                    line.text_end = cut.original(line.text_end);
                    for c in &mut line.clusters {
                        (c.start, c.end) = cut.cluster(c.start, c.end);
                    }
                    line.elided = Some((cut.head, cut.tail_from));
                }
                return lines;
            }
        }
    }
    layout.align(alignment(para.alignment), AlignmentOptions::default());
    let mut lines = extract(&layout, shift, attrs, runs[0].attrs, para, opts, req.clusters);
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
    context: Option<Strong>,
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

/// A parley layout of `text`, and how many bytes of direction marks
/// precede the text in it.
fn build(ctx: &mut Ctx, text: &str, attrs: &[Attrs], runs: &[Run], settings: &Settings) -> (Layout<Brush>, usize) {
    // A leading mark sets an explicit base direction; parley otherwise
    // takes it from the first strong character, as "natural" asks. Text
    // from inside a paragraph gets a second mark after it standing for the
    // strong text before it (the bidi algorithm's rules W2, W7, N0 and N1
    // look back to it), unless the first already says as much. Marks are
    // invisible and take no room; their glyphs are dropped (`extract`).
    let mark = match (settings.direction, settings.context) {
        (Direction::Natural, _) => "",
        (Direction::LeftToRight, Some(Strong::Right)) => "\u{200E}\u{200F}",
        (Direction::LeftToRight, Some(Strong::Arabic)) => "\u{200E}\u{61C}",
        (Direction::LeftToRight, _) => "\u{200E}",
        (Direction::RightToLeft, Some(Strong::Left)) => "\u{200F}\u{200E}",
        (Direction::RightToLeft, Some(Strong::Arabic)) => "\u{200F}\u{61C}",
        (Direction::RightToLeft, _) => "\u{200F}",
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
    // The last field says a piece is shaped apart from the ones before it
    // (see `cut_runs`).
    let mut pieces: Vec<(usize, usize, u32, Piece, bool)> = Vec::with_capacity(runs.len() + 2 * special.len());
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
                pieces.push((at, p0, run.attrs, Piece::Text, false));
            }
            pieces.push((p0, p1, run.attrs, kind, false));
            at = p1;
        }
        if at < run.end || run.start == run.end {
            pieces.push((at, run.end, run.attrs, Piece::Text, false));
        }
    }
    if text.len() > RUN_BYTES {
        pieces = cut_runs(text, pieces);
    }
    let mut used: Vec<(u32, Piece, bool)> = pieces.iter().map(|p| (p.2, p.3, p.4)).collect();
    used.sort_unstable();
    used.dedup();
    let features: Vec<Vec<FontFeature>> = used
        .iter()
        .map(|&(i, _, apart)| {
            let mut features = attrs[i as usize].features();
            if apart {
                // A feature no font has, off: it changes nothing but the
                // list, which parley compares to start a new run.
                features.push(FontFeature::new(Tag::new(b"zzzz"), 0));
            }
            features
        })
        .collect();
    let families: Vec<[FontFamilyName<'_>; 2]> = used
        .iter()
        .map(|&(i, _, _)| {
            let family = FontFamilyName::Named(Cow::Borrowed(&*attrs[i as usize].font.face.family));
            [FontFamilyName::Generic(GenericFamily::Emoji), family]
        })
        .collect();
    let mut builder = ctx.lcx.style_run_builder(&mut ctx.fcx, full, 1.0, false);
    builder.reserve(used.len(), pieces.len());
    let styles: Vec<u16> = used
        .iter()
        .zip(features.iter().zip(&families))
        .map(|(&(i, kind, _), (features, families))| {
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
    for &(_, end, index, kind, apart) in &pieces {
        let style = styles[used.binary_search(&(index, kind, apart)).unwrap_or(0)];
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

/// The most text parley shapes as one run here. It keeps a cluster's place
/// in its run in 16 bits, so a run past 64 KiB would give later clusters
/// and lines wrong text ranges; and it walks a run's glyphs from the run's
/// start for each change of style in it (`Line::items`), so a long run
/// with many colored spans, such as an unwrapped line of highlighted code,
/// costs the square of its length. (On 70 KB with a color a word, cutting
/// every 32 KiB lays out in 990 ms, every 4 KiB in 110 ms.)
const RUN_BYTES: usize = 4 << 10;

/// `pieces` of `text`, with one cut about every [`RUN_BYTES`]: parley
/// shapes pieces whose styles differ only in things like color as one run,
/// so a long stretch of text (one attribute run, or many) switches from
/// then on between its styles and copies of them that parley takes as
/// different (the last field), which starts a new run. A cut goes after
/// whitespace, where nothing shapes across, or else between two characters
/// that don't join the ones around them into clusters.
fn cut_runs(text: &str, pieces: Vec<(usize, usize, u32, Piece, bool)>) -> Vec<(usize, usize, u32, Piece, bool)> {
    let mut out = Vec::with_capacity(pieces.len() + 2 * text.len() / RUN_BYTES);
    let (mut apart, mut since) = (false, 0);
    for (mut start, end, attrs, kind, _) in pieces {
        while since + (end - start) > RUN_BYTES {
            let cut = cut_before(text, start, start + (RUN_BYTES - since));
            if cut > start {
                out.push((start, cut, attrs, kind, apart));
                start = cut;
            }
            (apart, since) = (!apart, 0);
        }
        since += end - start;
        out.push((start, end, attrs, kind, apart));
    }
    out
}

/// Where to cut `text` between `start` and `limit`: after the last
/// whitespace in the 4 KiB before `limit`, or else between the last two
/// characters there that are clusters of their own, or else at the last
/// character boundary; `start` if there is none after it.
fn cut_before(text: &str, start: usize, limit: usize) -> usize {
    let mut limit = limit.min(text.len());
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    let mut from = limit.saturating_sub(4096).max(start);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let near = &text[from..limit];
    if let Some((i, c)) = near.char_indices().rev().find(|&(_, c)| c.is_whitespace()) {
        return from + i + c.len_utf8();
    }
    let breaks = CodePointMapData::<GraphemeClusterBreak>::new();
    let alone = |c: char| breaks.get(c) == GraphemeClusterBreak::Other;
    let mut after = None;
    for (i, c) in near.char_indices().rev() {
        if alone(c) && after.is_some_and(alone) {
            return from + i + c.len_utf8();
        }
        after = Some(c);
    }
    limit
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
/// stop, which moves what follows. The next stop is the first in the list
/// beyond the tab; past the last, stops follow it every
/// `defaultTabInterval`, or there are none if that is 0 and the tab takes
/// no room, as in AppKit.
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
            // The decimal point centers on the stop; without one, the end
            // of the text goes there.
            Some(Tab { location, kind: TabKind::Decimal }) => {
                let segment = &text[at + 1..tabs.get(id as usize + 1).copied().unwrap_or(text.len())];
                match segment.find('.') {
                    Some(dot) => {
                        let (from, to) = (at + 1 + shift, at + 1 + dot + shift);
                        let point = advance_between(layout, to, to + 1);
                        (location - x - advance_between(layout, from, to) - point / 2.0).max(0.0)
                    }
                    None => (location - end).max(0.0),
                }
            }
            None if para.default_tab_interval > 0.0 => {
                let interval = para.default_tab_interval as f32;
                let last = stops.last().map_or(0.0, |t| t.location).min(x);
                last + (((x + 0.001 - last) / interval).floor().max(0.0) + 1.0) * interval - x
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
///
/// parley lets a line's first space hang past the width and breaks after
/// it, so text ending in spaces that don't fit ends in a line of nothing,
/// or of the other spaces, where AppKit hangs them all on the line before
/// (measured on macOS: "hello world " in the width of "hello world" is one
/// line). That line is then broken again with room for them, and aligned
/// in the width it had.
fn break_lines(layout: &mut Layout<Brush>, right: f32, first_indent: f32, rest_indent: f32) {
    break_with(layout, right, first_indent, rest_indent, None);
    let n = layout.len();
    if !right.is_finite() || n < 2 {
        return;
    }
    let (Some(before), Some(last)) = (layout.get(n - 2), layout.get(n - 1)) else { return };
    let spaces = last.items().all(|item| match item {
        PositionedLayoutItem::GlyphRun(run) => run.run().clusters().all(|c| c.is_space_or_nbsp()),
        PositionedLayoutItem::InlineBox(_) => false,
    });
    let hung = before.metrics().trailing_whitespace;
    if before.break_reason() == parley::BreakReason::Regular && hung > 0.0 && spaces {
        let room = hung + last.metrics().advance;
        break_with(layout, right, first_indent, rest_indent, Some((n - 2, room)));
    }
}

/// [`break_lines`], giving line `wider.0`, if any, `wider.1` more room.
fn break_with(
    layout: &mut Layout<Brush>,
    right: f32,
    first_indent: f32,
    rest_indent: f32,
    wider: Option<(usize, f32)>,
) {
    let mut breaker = layout.break_lines();
    let finite = right.is_finite();
    // parley holds no line wider than the layout.
    let widest = right.max(0.0) + wider.map_or(0.0, |w| w.1);
    breaker.state_mut().set_layout_max_advance(if finite { widest } else { f32::INFINITY });
    let mut indent = first_indent;
    let mut line = 0;
    loop {
        let avail = if finite { (right - indent).max(0.0) } else { f32::INFINITY };
        let room = wider.filter(|w| w.0 == line).map_or(0.0, |w| w.1);
        let state = breaker.state_mut();
        state.set_line_x(if finite { indent.min(right.max(0.0)) } else { indent });
        state.set_line_max_advance(avail + room);
        match breaker.break_next() {
            Some(parley::YieldData::LineBreak(_)) => {
                if room > 0.0 {
                    breaker.set_prior_line_width(avail);
                }
                indent = rest_indent;
                line += 1;
            }
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

/// How a truncated line's text was cut: `text[..head]`, an ellipsis, then
/// `text[tail_from..]`.
#[derive(Clone, Copy, Debug)]
struct Cut {
    head: usize,
    tail_from: usize,
}

impl Cut {
    /// Where a byte of the shortened text was in the original.
    fn original(&self, at: usize) -> usize {
        let dots = self.head + '…'.len_utf8();
        if at <= self.head {
            at
        } else if at < dots {
            self.tail_from
        } else {
            at - dots + self.tail_from
        }
    }

    /// A cluster's range in the original: the ellipsis's covers what it
    /// stands for.
    fn cluster(&self, start: usize, end: usize) -> (usize, usize) {
        if start == self.head && end == self.head + '…'.len_utf8() {
            (self.head, self.tail_from)
        } else {
            (self.original(start), self.original(end))
        }
    }
}

/// `text` with what doesn't fit in `avail` replaced by an ellipsis, as the
/// truncating `mode` asks, giving up `extra` more clusters than the
/// measurement says; its runs, and how it was cut.
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
) -> (String, Vec<Run>, Cut) {
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
    (short, out, Cut { head: kept_head.len(), tail_from })
}

/// A line with nothing on it, as tall as its font makes it.
fn empty_line(attrs: &Attrs, para: &Paragraph, opts: &Options) -> LaidLine {
    let m = &attrs.font.face.metrics;
    let size = attrs.font.size;
    let (ascent, descent) = ((m.ascent * size).round(), (-m.descent * size).round());
    let leading = m.leading * size;
    let height = line_height(ascent, descent, leading, para, opts);
    LaidLine {
        height,
        descent,
        ascent,
        leading,
        right: 0.0,
        indent: 0.0,
        x: 0.0,
        advance: 0.0,
        trailing: 0.0,
        runs: Vec::new(),
        fills: Vec::new(),
        text_start: 0,
        text_end: 0,
        clusters: Vec::new(),
        elided: None,
        forced: false,
    }
}

/// A line's height from its largest ascent and descent, rounded, and its
/// fonts' largest leading.
fn line_height(ascent: f32, descent: f32, leading: f32, para: &Paragraph, opts: &Options) -> f32 {
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
    height
}

/// A glyph run found on a line, before the line's height is known.
struct Pending {
    font: u32,
    /// The face drawn with its outlines stroked, for `NSStrokeWidth`.
    stroked: Option<u32>,
    size: f32,
    x: f32,
    advance: f32,
    attrs: u32,
    glyphs: Arc<[Glyph]>,
}

/// The lines of a broken and aligned layout. `base` names the attributes
/// whose font sizes a line with no glyphs; `clusters` asks for each line's
/// clusters.
fn extract(
    layout: &Layout<Brush>,
    shift: usize,
    attrs: &[Attrs],
    base: u32,
    para: &Paragraph,
    opts: &Options,
    clusters: bool,
) -> Vec<LaidLine> {
    let mut lines = Vec::with_capacity(layout.len());
    let mut pending: Vec<Pending> = Vec::new();
    for line in layout.lines() {
        let m = line.metrics();
        let (mut ascent, mut descent, mut leading) = (0.0f32, 0.0f32, 0.0f32);
        pending.clear();
        let mut by_word = false;
        // Where each glyph run starts among its parley run's glyphs, to
        // leave out the direction mark's: (the run, glyphs before).
        let mut from = (usize::MAX, 0);
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else { continue };
            let run = glyph_run.run();
            let first = if from.0 == run.index() { from.1 } else { 0 };
            if shift > 0 {
                from = (run.index(), first + glyph_run.glyphs().count());
            }
            // Control characters, and the direction marks alone, take no
            // part in the line.
            if run.font_size() <= HIDDEN_SIZE || run.text_range().end <= shift {
                continue;
            }
            let mark = if shift > 0 && run.text_range().start < shift { mark_glyphs(run, shift) } else { 0..0 };
            let index = glyph_run.style().brush.0;
            let a = &attrs[index as usize];
            let rm = *run.metrics();
            // Raised text makes room above, lowered text below: whole
            // points of the font's, and the offset as it is.
            ascent = ascent.max(rm.ascent.round() + a.baseline_offset.max(0.0));
            descent = descent.max(rm.descent.round() - a.baseline_offset.min(0.0));
            leading = leading.max(rm.leading);
            by_word |= (a.underline.style | a.strikethrough.style) & underline::BY_WORD != 0;
            let mut pen = 0.0;
            let glyphs: Arc<[Glyph]> = glyph_run
                .glyphs()
                .enumerate()
                .filter_map(|(i, g)| {
                    let glyph = Glyph { id: g.id, x: pen + g.x, y: g.y };
                    pen += g.advance;
                    (!mark.contains(&(first + i))).then_some(glyph)
                })
                .collect();
            let synthesis = run.synthesis();
            let skew = synthesis.skew().unwrap_or(0.0) + a.slant();
            let face = |stroke: f32| {
                fonts::register(
                    run.font(),
                    run.normalized_coords(),
                    fonts::Synth { embolden: synthesis.embolden(), skew, stroke },
                )
            };
            pending.push(Pending {
                font: face(0.0),
                stroked: (a.stroke.width != 0.0).then(|| face(a.stroke.width.abs() / 100.0)),
                size: run.font_size(),
                x: glyph_run.offset(),
                advance: glyph_run.advance(),
                attrs: index,
                glyphs,
            });
        }
        if pending.is_empty() {
            let a = &attrs[base as usize];
            let fm = &a.font.face.metrics;
            ascent = (fm.ascent * a.font.size).round();
            descent = (-fm.descent * a.font.size).round();
            leading = fm.leading * a.font.size;
        }
        let height = line_height(ascent, descent, leading, para, opts);
        let baseline = height - descent;
        let mut out = LaidLine {
            height,
            descent,
            ascent,
            leading,
            right: m.advance,
            indent: m.inline_min_coord,
            x: m.inline_min_coord + m.offset,
            advance: m.advance,
            trailing: m.trailing_whitespace,
            runs: Vec::new(),
            fills: Vec::new(),
            text_start: line.text_range().start.saturating_sub(shift),
            text_end: line.text_range().end.saturating_sub(shift),
            clusters: Vec::new(),
            elided: None,
            forced: line.break_reason() == parley::BreakReason::Emergency,
        };
        if clusters {
            out.clusters = line_clusters(layout, &line, shift);
        }
        // The spaces between words, which by-word decorations skip.
        let spaces = if by_word { spaces(&line) } else { Vec::new() };
        let mut shadows = Vec::new();
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
            // Where the lines go comes from the text's own font, not a
            // fallback's, so that they run straight through emoji and
            // other scripts.
            let (fm, size) = (&a.font.face.metrics, a.font.size);
            for (decoration, offset, size) in [
                (a.underline, fm.underline_position * size, fm.underline_thickness * size),
                (a.strikethrough, fm.strikeout_position * size, fm.strikeout_thickness * size),
            ] {
                decorate(&mut out.fills, decoration, a.color, p.x, p.advance, y - offset, size, &spaces);
            }
            if p.glyphs.is_empty() {
                continue;
            }
            let run =
                |font: u32, color: Color| PlacedRun { font, size: p.size, x: p.x, y, glyphs: p.glyphs.clone(), color };
            let stroke_color = a.stroke.color.unwrap_or(a.color);
            // Text drawn first, in the shadow's color and offset (y up, as
            // AppKit gives it). Its blur needs a blurred glyph op from the
            // rasterizer, which it hasn't yet: the shadow is sharp.
            if let Some(shadow) = a.shadow {
                for font in [(a.stroke.width <= 0.0).then_some(p.font), p.stroked].into_iter().flatten() {
                    shadows.push(PlacedRun {
                        x: p.x + shadow.offset[0],
                        y: y - shadow.offset[1],
                        ..run(font, shadow.color)
                    });
                }
            }
            // A positive stroke width outlines the glyphs; a negative one
            // fills them and outlines them too.
            if a.stroke.width <= 0.0 {
                out.runs.push(run(p.font, a.color));
            }
            if let Some(stroked) = p.stroked {
                out.runs.push(run(stroked, stroke_color));
            }
        }
        if !shadows.is_empty() {
            shadows.append(&mut out.runs);
            out.runs = shadows;
        }
        lines.push(out);
    }
    lines
}

/// Which of `run`'s glyphs, in visual order, draw the direction marks that
/// the text was given in front (its first `shift` bytes): invisible
/// glyphs, but ones that a line laid out from anywhere else wouldn't have.
/// The marks come first in the text, so their clusters are side by side,
/// at the run's left end, or its right end in a right-to-left run.
fn mark_glyphs(run: &parley::Run<'_, Brush>, shift: usize) -> std::ops::Range<usize> {
    let (mut at, mut marks) = (0, None::<std::ops::Range<usize>>);
    for cluster in run.visual_clusters() {
        let count = cluster.glyphs().count();
        if cluster.text_range().start < shift {
            marks.get_or_insert(at..at).end = at + count;
        }
        at += count;
    }
    marks.unwrap_or(0..0)
}

/// The clusters of `line` in visual order, left to right, placed from the
/// container's left. The box after a tab widens the tab's cluster.
fn line_clusters(layout: &Layout<Brush>, line: &parley::Line<'_, Brush>, shift: usize) -> Vec<ByteCluster> {
    let m = line.metrics();
    // Tab boxes on the line: where each is, how wide, and its tab's byte.
    let boxes: Vec<(f32, f32, usize)> = if layout.inline_boxes().is_empty() {
        Vec::new()
    } else {
        line.items()
            .filter_map(|item| match item {
                PositionedLayoutItem::InlineBox(b) => {
                    let index = layout.inline_boxes().iter().find(|ib| ib.id == b.id)?.index;
                    Some((b.x, b.width, index.checked_sub(1 + shift)?))
                }
                PositionedLayoutItem::GlyphRun(_) => None,
            })
            .collect()
    };
    let mut out = Vec::with_capacity(line.text_range().len());
    let mut pen = m.inline_min_coord + m.offset;
    let mut next_box = 0;
    for run in line.runs() {
        // Boxes take their room between runs.
        while let Some(&(x, width, _)) = boxes.get(next_box)
            && x <= pen + 0.01
        {
            pen += width;
            next_box += 1;
        }
        let rtl = run.is_rtl();
        for cluster in run.visual_clusters() {
            let range = cluster.text_range();
            let advance = cluster.advance();
            if range.start >= shift {
                out.push(ByteCluster { start: range.start - shift, end: range.end - shift, x: pen, advance, rtl });
            }
            pen += advance;
        }
    }
    // Each box widens its tab's cluster where they touch: the box follows
    // the tab on the right, or on the left in right-to-left text.
    for &(x, width, tab) in &boxes {
        let Some(c) = out.iter_mut().find(|c| c.start == tab) else { continue };
        if (c.x + c.advance - x).abs() < 0.01 {
            c.advance += width;
        } else if (x + width - c.x).abs() < 0.01 {
            c.x = x;
            c.advance += width;
        }
    }
    out
}

/// Where the spaces of `line` are, as (x0, x1).
fn spaces(line: &parley::Line<'_, Brush>) -> Vec<(f32, f32)> {
    let m = line.metrics();
    let mut pen = m.inline_min_coord + m.offset;
    let mut out: Vec<(f32, f32)> = Vec::new();
    for run in line.runs() {
        for cluster in run.visual_clusters() {
            let advance = cluster.advance();
            if cluster.is_space_or_nbsp() {
                match out.last_mut() {
                    Some(last) if (last.1 - pen).abs() < 0.01 => last.1 = pen + advance,
                    _ => out.push((pen, pen + advance)),
                }
            }
            pen += advance;
        }
    }
    out
}

/// Add the rectangles of an underline or strikethrough whose top is at `y`,
/// under `x..x + width`, leaving out `spaces` if the style says by word.
#[allow(clippy::too_many_arguments)]
fn decorate(
    fills: &mut Vec<PlacedFill>,
    d: Decoration,
    text: Color,
    x: f32,
    width: f32,
    y: f32,
    size: f32,
    spaces: &[(f32, f32)],
) {
    // The low byte is the line's style: single, thick or double.
    let style = d.style & 0xff;
    if style == 0 || width <= 0.0 {
        return;
    }
    let color = d.color.unwrap_or(text);
    let thickness = size.max(0.5) * if style & 0x0f == underline::THICK { 2.0 } else { 1.0 };
    let double = style & 0x0f == underline::DOUBLE;
    // Dots and dashes in units of the line's thickness, a point at least,
    // measured from the container's left so that runs continue each other.
    let unit = thickness.max(1.0);
    let pattern: &[f32] = match d.style & underline::PATTERN {
        underline::DOT => &[1.0, 1.0],
        underline::DASH => &[4.0, 2.0],
        underline::DASH_DOT => &[4.0, 2.0, 1.0, 2.0],
        underline::DASH_DOT_DOT => &[4.0, 2.0, 1.0, 2.0, 1.0, 2.0],
        _ => &[],
    };
    let mut rect = |x0: f32, x1: f32| {
        if x1 - x0 <= 0.01 {
            return;
        }
        fills.push(PlacedFill { rect: [x0, y, x1, y + thickness], color, background: false });
        if double {
            let y = y + 2.0 * thickness.max(1.0);
            fills.push(PlacedFill { rect: [x0, y, x1, y + thickness], color, background: false });
        }
    };
    let mut dashes = |x0: f32, x1: f32| {
        if pattern.is_empty() {
            return rect(x0, x1);
        }
        if !(x0.is_finite() && x1.is_finite()) {
            return;
        }
        // Whole periods, counted in double precision: adding a period to a
        // single-precision position far out (past 2^24 points) can leave it
        // where it was, and the count must end.
        let period = f64::from(pattern.iter().sum::<f32>() * unit);
        let (k0, k1) = ((f64::from(x0) / period).floor() as i64, (f64::from(x1) / period).ceil() as i64);
        for k in k0..k1 {
            let mut on = k as f64 * period;
            for (i, &len) in pattern.iter().enumerate() {
                let end = on + f64::from(len * unit);
                if i % 2 == 0 {
                    rect((on as f32).max(x0), (end as f32).min(x1));
                }
                on = end;
            }
        }
    };
    if d.style & underline::BY_WORD == 0 {
        return dashes(x, x + width);
    }
    let mut at = x;
    for &(s0, s1) in spaces.iter().filter(|s| s.1 > x && s.0 < x + width) {
        dashes(at, s0.max(at));
        at = at.max(s1);
    }
    dashes(at, x + width);
}
