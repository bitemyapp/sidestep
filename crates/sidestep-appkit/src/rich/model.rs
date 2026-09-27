//! Rich text as the readers and writers see it: plain Rust values, with no
//! objects, so the formats are tested on their own and the conversion to
//! and from `NSAttributedString` (`convert`) is written once.
//!
//! A [`Doc`] is UTF-8 text with character runs, each a [`CharStyle`], and
//! paragraphs, each with a [`ParaStyle`] or none; the two cover the text
//! independently. Readers build one with [`Builder`]; writers get one from
//! an attributed string and walk its paragraphs and their runs.

use std::ops::Range;

/// Where a font comes from when none of its names is found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Generic {
    /// Helvetica: the interface font's design.
    #[default]
    Sans,
    /// Times.
    Serif,
    /// Courier.
    Mono,
    /// The system font (`-apple-system`, `system-ui`).
    System,
}

/// A font: what to find it by, its size, and the traits to give it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Font {
    /// PostScript or family names, best first.
    pub names: Vec<String>,
    /// The family, for writers that name families (HTML); may be empty.
    pub family: String,
    pub generic: Generic,
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
}

impl Font {
    pub fn named(name: &str, generic: Generic, size: f64) -> Font {
        Font { names: vec![name.to_owned()], family: String::new(), generic, size, bold: false, italic: false }
    }
}

/// The color spaces rich text names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Space {
    Srgb,
    /// Calibrated RGB: what an RTF color table without Cocoa's extended
    /// table means.
    GenericRgb,
    DisplayP3,
    Gray,
    Cmyk,
}

impl Space {
    /// How many components a color in this space has, alpha not counted.
    pub fn components(self) -> usize {
        match self {
            Space::Gray => 1,
            Space::Cmyk => 4,
            _ => 3,
        }
    }
}

/// A color.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Color {
    pub space: Space,
    /// The components in `space`, as many as it has, then alpha.
    pub components: Vec<f64>,
    /// A system color's name (`textColor`), which readers look up first.
    pub name: Option<String>,
    /// The color in sRGB with alpha, for formats that know no other space.
    pub srgb: [f64; 4],
}

impl Color {
    pub fn srgb(r: f64, g: f64, b: f64, a: f64) -> Color {
        Color { space: Space::Srgb, components: vec![r, g, b, a], name: None, srgb: [r, g, b, a] }
    }

    /// A color in `space` from its components and alpha. Its sRGB look is
    /// approximate for spaces other than RGB (no color management).
    pub fn in_space(space: Space, mut components: Vec<f64>, alpha: f64) -> Color {
        components.resize(space.components(), 0.0);
        let srgb = match space {
            Space::Gray => [components[0], components[0], components[0], alpha],
            Space::Cmyk => {
                let k = 1.0 - components[3];
                [(1.0 - components[0]) * k, (1.0 - components[1]) * k, (1.0 - components[2]) * k, alpha]
            }
            _ => [components[0], components[1], components[2], alpha],
        };
        components.push(alpha);
        Color { space, components, name: None, srgb }
    }

    pub fn alpha(&self) -> f64 {
        self.components.last().copied().unwrap_or(1.0)
    }
}

/// A shadow: its offset (up is positive), blur radius and color.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Shadow {
    pub offset: (f64, f64),
    pub blur: f64,
    pub color: Option<Color>,
}

/// Character formatting: the attributes rich text carries, each absent
/// (or zero) unless set.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CharStyle {
    pub font: Option<Font>,
    pub color: Option<Color>,
    pub background: Option<Color>,
    /// `NSUnderlineStyle` bits; 0 for none.
    pub underline: i64,
    pub underline_color: Option<Color>,
    pub strikethrough: i64,
    pub strikethrough_color: Option<Color>,
    /// A link's URL, as written.
    pub link: Option<String>,
    pub baseline_offset: f64,
    pub superscript: i64,
    pub kern: Option<f64>,
    pub ligature: Option<i64>,
    pub shadow: Option<Shadow>,
    pub expansion: f64,
    pub obliqueness: f64,
    pub stroke_width: f64,
    pub stroke_color: Option<Color>,
}

/// Paragraph alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Right,
    Center,
    Justified,
    #[default]
    Natural,
}

/// A paragraph's base writing direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Direction {
    #[default]
    Natural,
    LeftToRight,
    RightToLeft,
}

/// A tab stop's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabKind {
    Left,
    Right,
    Center,
    Decimal,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Tab {
    pub location: f64,
    pub kind: TabKind,
}

/// Paragraph formatting, with `NSParagraphStyle`'s defaults.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ParaStyle {
    pub alignment: Align,
    pub first_line_head_indent: f64,
    pub head_indent: f64,
    /// As `NSParagraphStyle` keeps it: from the leading margin when
    /// positive, from the trailing one when negative or zero.
    pub tail_indent: f64,
    pub line_spacing: f64,
    pub paragraph_spacing: f64,
    pub paragraph_spacing_before: f64,
    pub minimum_line_height: f64,
    pub maximum_line_height: f64,
    pub line_height_multiple: f64,
    pub direction: Direction,
    pub tabs: Vec<Tab>,
    pub default_tab_interval: f64,
    pub header_level: i64,
    pub tightening: bool,
}

/// The tab stops a new paragraph style has: twelve, 28 points apart.
pub(crate) fn default_tabs() -> Vec<Tab> {
    (1..=12).map(|i| Tab { location: 28.0 * f64::from(i), kind: TabKind::Left }).collect()
}

impl Default for ParaStyle {
    fn default() -> ParaStyle {
        ParaStyle {
            alignment: Align::Natural,
            first_line_head_indent: 0.0,
            head_indent: 0.0,
            tail_indent: 0.0,
            line_spacing: 0.0,
            paragraph_spacing: 0.0,
            paragraph_spacing_before: 0.0,
            minimum_line_height: 0.0,
            maximum_line_height: 0.0,
            line_height_multiple: 0.0,
            direction: Direction::Natural,
            tabs: default_tabs(),
            default_tab_interval: 0.0,
            header_level: 0,
            tightening: true,
        }
    }
}

/// Document attributes, as `documentAttributes` dictionaries hold them.
/// Lengths are points.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DocAttrs {
    pub paper_size: Option<(f64, f64)>,
    pub left_margin: Option<f64>,
    pub right_margin: Option<f64>,
    pub top_margin: Option<f64>,
    pub bottom_margin: Option<f64>,
    pub view_size: Option<(f64, f64)>,
    pub view_zoom: Option<f64>,
    pub view_mode: Option<i64>,
    pub read_only: Option<i64>,
    pub hyphenation_factor: Option<f64>,
    pub default_tab_interval: Option<f64>,
    pub cocoa_version: Option<f64>,
    pub text_scaling: Option<i64>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Vec<String>,
    pub comment: Option<String>,
    pub company: Option<String>,
    pub copyright: Option<String>,
    pub editor: Option<String>,
    pub manager: Option<String>,
    pub category: Option<String>,
    pub background: Option<Color>,
}

/// US Letter, as AppKit's documents default to.
pub(crate) const PAPER: (f64, f64) = (612.0, 792.0);
/// RTF's default margins: 1.25 inches left and right, 1 inch top and bottom.
pub(crate) const MARGINS: [f64; 4] = [90.0, 90.0, 72.0, 72.0];

impl DocAttrs {
    /// How wide the text is on the page: what a right indent is measured
    /// against when a tail indent from the leading margin is written.
    pub fn text_width(&self) -> f64 {
        let paper = self.paper_size.unwrap_or(PAPER).0;
        paper - self.left_margin.unwrap_or(MARGINS[0]) - self.right_margin.unwrap_or(MARGINS[1])
    }
}

/// Rich text: UTF-8 text, character runs and paragraphs.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Doc {
    pub text: String,
    /// Each run's end in bytes and its style; in order, covering the text.
    pub runs: Vec<(usize, CharStyle)>,
    /// Each paragraph's end in bytes (after its separator) and its style,
    /// if it has one; in order, covering the text.
    pub paras: Vec<(usize, Option<ParaStyle>)>,
    pub attrs: DocAttrs,
}

impl Doc {
    /// The runs with their ranges.
    pub fn run_ranges(&self) -> impl Iterator<Item = (Range<usize>, &CharStyle)> {
        let mut start = 0;
        self.runs.iter().map(move |(end, style)| {
            let r = start..*end;
            start = *end;
            (r, style)
        })
    }

    /// The paragraphs with their ranges.
    pub fn para_ranges(&self) -> impl Iterator<Item = (Range<usize>, Option<&ParaStyle>)> {
        let mut start = 0;
        self.paras.iter().map(move |(end, style)| {
            let r = start..*end;
            start = *end;
            (r, style.as_ref())
        })
    }
}

/// Where the paragraphs of `text` end: after each paragraph separator
/// (LF, CR, CR LF, U+2029; not U+2028, which only ends a line), and at the
/// end of the text if it doesn't end in one.
pub(crate) fn paragraph_ends(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut ends = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                i += 1;
                ends.push(i);
            }
            b'\r' => {
                i += if bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                ends.push(i);
            }
            0xE2 if bytes[i..].starts_with("\u{2029}".as_bytes()) => {
                i += 3;
                ends.push(i);
            }
            _ => i += 1,
        }
    }
    if ends.last() != Some(&text.len()) || text.is_empty() {
        ends.push(text.len());
    }
    ends
}

/// Builds a [`Doc`] as a reader goes: text in the current character style,
/// and paragraph ends in the style in effect there.
#[derive(Default)]
pub(crate) struct Builder {
    doc: Doc,
    /// Where the paragraph being read started.
    para_start: usize,
}

impl Builder {
    pub fn new() -> Builder {
        Builder::default()
    }

    /// Append `text` in `style`, joining the last run when it has the
    /// same style.
    pub fn push(&mut self, text: &str, style: &CharStyle) {
        if text.is_empty() {
            return;
        }
        self.doc.text.push_str(text);
        let end = self.doc.text.len();
        match self.doc.runs.last_mut() {
            Some((last, s)) if s == style => *last = end,
            _ => self.doc.runs.push((end, style.clone())),
        }
    }

    /// End the paragraph being read with `separator` in `style`, the
    /// paragraph in `para`.
    pub fn end_paragraph(&mut self, separator: &str, style: &CharStyle, para: Option<&ParaStyle>) {
        self.push(separator, style);
        self.close_paragraph(para);
    }

    /// Close the paragraph read so far (its separator already pushed, or
    /// at the end of the text).
    fn close_paragraph(&mut self, para: Option<&ParaStyle>) {
        let end = self.doc.text.len();
        if end > self.para_start || self.doc.paras.is_empty() && end == 0 {
            self.doc.paras.push((end, para.cloned()));
            self.para_start = end;
        }
    }

    /// How long the text read so far is, in bytes.
    pub fn len(&self) -> usize {
        self.doc.text.len()
    }

    /// Whether the text read so far is empty or ends a paragraph.
    pub fn at_paragraph_start(&self) -> bool {
        self.para_start == self.doc.text.len()
    }

    /// Take away the last character of the text, if it is `c`.
    pub fn pop_if(&mut self, c: char) -> bool {
        if !self.doc.text.ends_with(c) || self.doc.text.len() - c.len_utf8() < self.para_start {
            return false;
        }
        let end = self.doc.text.len() - c.len_utf8();
        self.doc.text.truncate(end);
        // Runs that started at or after `end` go; the one before ends there.
        while self.doc.runs.len() > 1 && self.doc.runs[self.doc.runs.len() - 2].0 >= end {
            self.doc.runs.pop();
        }
        match self.doc.runs.last_mut() {
            Some((last, _)) if end > 0 => *last = end,
            _ => self.doc.runs.clear(),
        }
        true
    }

    /// The document, the last paragraph closed in `para`.
    pub fn finish(mut self, para: Option<&ParaStyle>) -> Doc {
        if self.doc.text.len() > self.para_start || self.doc.paras.is_empty() {
            self.doc.paras.push((self.doc.text.len(), para.cloned()));
        }
        self.doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_end_after_their_separators() {
        assert_eq!(paragraph_ends(""), [0]);
        assert_eq!(paragraph_ends("a"), [1]);
        assert_eq!(paragraph_ends("a\nb"), [2, 3]);
        assert_eq!(paragraph_ends("a\n"), [2]);
        assert_eq!(paragraph_ends("a\r\nb\rc\u{2029}d\u{2028}e"), [3, 5, 9, 14]);
    }

    #[test]
    fn builders_join_runs_and_keep_paragraphs() {
        let bold = CharStyle { underline: 1, ..CharStyle::default() };
        let plain = CharStyle::default();
        let centered = ParaStyle { alignment: Align::Center, ..ParaStyle::default() };
        let mut b = Builder::new();
        b.push("a", &plain);
        b.push("b", &plain);
        b.push("c", &bold);
        b.end_paragraph("\n", &bold, Some(&centered));
        b.push("d", &plain);
        assert!(b.pop_if('d') && !b.pop_if('\n'));
        b.push("e", &plain);
        let doc = b.finish(None);
        assert_eq!(doc.text, "abc\ne");
        assert_eq!(doc.runs, [(2, plain.clone()), (4, bold), (5, plain)]);
        assert_eq!(doc.paras, [(4, Some(centered)), (5, None)]);
        assert_eq!(Builder::new().finish(None).paras, [(0, None)]);
    }
}
