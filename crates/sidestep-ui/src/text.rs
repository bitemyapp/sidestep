//! Text: [`Font`]s from the system's fontconfig, [`TextStyle`]s, and
//! [`TextLayout`], text laid out in lines with what editing needs of it
//! (the character at a point, carets, selections).
//!
//! Text is shaped and laid out on the thread that asks (parley: bidi, line
//! breaking, shaping with harfrust, font fallback), and drawn by the render
//! thread from its glyph cache. Offsets into text are byte offsets of its
//! UTF-8, on character boundaries.

use std::ops::Range;
use std::sync::Arc;

use kurbo::{Point, Rect, Size};
use sidestep_engine::text::fonts::{self, Design, Face, Family, FontSpec};
use sidestep_engine::text::layout::{self, Align, Attrs, Decoration, LineBreak, Paragraph, TextFont};
use sidestep_engine::text::lines::{Container, Frame, Span, Styled};

use crate::color::Color;

/// A font at a size: a face the system has, found through fontconfig as
/// GTK and Qt programs find theirs.
#[derive(Clone)]
pub struct Font {
    spec: FontSpec,
    face: Arc<Face>,
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("family", &&*self.face.family_name)
            .field("size", &self.spec.size)
            .field("weight", &self.spec.weight)
            .field("italic", &self.spec.italic)
            .finish()
    }
}

impl PartialEq for Font {
    fn eq(&self, other: &Font) -> bool {
        Arc::ptr_eq(&self.face, &other.face) && self.spec.size.to_bits() == other.spec.size.to_bits()
    }
}

/// A font's vertical metrics at its size, in points (y down: `descent` is
/// positive below the baseline).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    pub ascent: f64,
    pub descent: f64,
    /// Space the font asks for between lines.
    pub leading: f64,
    pub cap_height: f64,
    pub x_height: f64,
}

impl Font {
    fn of(spec: FontSpec) -> Font {
        let face = fonts::resolve(&spec);
        Font { spec, face }
    }

    /// The desktop's interface font (`system-ui`).
    pub fn system(size: f64) -> Font {
        Font::of(FontSpec::system(Design::Default, size))
    }

    /// The desktop's monospaced font.
    pub fn monospace(size: f64) -> Font {
        Font::of(FontSpec::system(Design::Monospaced, size))
    }

    pub fn serif(size: f64) -> Font {
        Font::of(FontSpec::system(Design::Serif, size))
    }

    /// A family the system has, a PostScript name (`DejaVuSans-Bold`) or a
    /// full name (`DejaVu Sans Bold`); `None` if nothing has that name.
    pub fn named(name: &str, size: f64) -> Option<Font> {
        let spec = fonts::spec_named(name, size)?;
        (!spec.missing).then(|| Font::of(spec))
    }

    /// The families the system has.
    pub fn families() -> Vec<String> {
        fonts::family_names()
    }

    /// This font in another weight: CSS's scale, 100 (thin) to 900
    /// (black), 400 regular and 700 bold.
    pub fn with_weight(&self, weight: f32) -> Font {
        Font::of(FontSpec { weight: weight.clamp(1.0, 1000.0), ..self.spec.clone() })
    }

    pub fn bold(&self) -> Font {
        self.with_weight(700.0)
    }

    pub fn italic(&self) -> Font {
        Font::of(FontSpec { italic: true, ..self.spec.clone() })
    }

    pub fn with_size(&self, size: f64) -> Font {
        Font { spec: FontSpec { size, ..self.spec.clone() }, face: self.face.clone() }
    }

    /// Digits of equal width, for numbers in columns or changing in place.
    pub fn with_tabular_digits(&self) -> Font {
        Font { spec: FontSpec { tabular_digits: true, ..self.spec.clone() }, face: self.face.clone() }
    }

    pub fn size(&self) -> f64 {
        self.spec.size
    }

    pub fn weight(&self) -> f32 {
        self.spec.weight
    }

    /// The family the face is from.
    pub fn family_name(&self) -> &str {
        &self.face.family_name
    }

    pub fn is_monospaced(&self) -> bool {
        self.face.fixed_pitch || self.spec.family == Family::System(Design::Monospaced)
    }

    pub fn metrics(&self) -> FontMetrics {
        let m = &self.face.metrics;
        let s = self.spec.size;
        FontMetrics {
            ascent: f64::from(m.ascent) * s,
            descent: -f64::from(m.descent) * s,
            leading: f64::from(m.leading) * s,
            cap_height: f64::from(m.cap_height) * s,
            x_height: f64::from(m.x_height) * s,
        }
    }

    pub(crate) fn text_font(&self) -> TextFont {
        TextFont {
            face: self.face.clone(),
            size: self.spec.size as f32,
            tabular_digits: self.spec.tabular_digits,
            features: self.spec.features.clone(),
        }
    }
}

/// How a run of text looks.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub font: Font,
    pub color: Color,
    /// Filled behind the text.
    pub background: Option<Color>,
    pub underline: bool,
    pub strikethrough: bool,
    /// Extra space after each character, in points (0 is the font's own
    /// spacing).
    pub letter_spacing: f64,
    /// Points to raise the text by (negative lowers it).
    pub baseline_offset: f64,
}

impl TextStyle {
    pub fn new(font: Font, color: Color) -> TextStyle {
        TextStyle {
            font,
            color,
            background: None,
            underline: false,
            strikethrough: false,
            letter_spacing: 0.0,
            baseline_offset: 0.0,
        }
    }

    pub fn underlined(mut self) -> TextStyle {
        self.underline = true;
        self
    }

    pub fn with_background(mut self, color: Color) -> TextStyle {
        self.background = Some(color);
        self
    }

    fn attrs(&self, paragraph: &Paragraph) -> Attrs {
        let mut attrs = Attrs::new(self.font.text_font());
        attrs.color = self.color.raw();
        attrs.background = self.background.map(Color::raw);
        // NSUnderlineStyleSingle.
        let single = |on: bool| Decoration { style: i64::from(on), color: None };
        attrs.underline = single(self.underline);
        attrs.strikethrough = single(self.strikethrough);
        attrs.kern = (self.letter_spacing != 0.0).then_some(self.letter_spacing as f32);
        attrs.baseline_offset = self.baseline_offset as f32;
        attrs.paragraph = paragraph.clone();
        attrs
    }
}

/// How lines line up in a layout's width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Alignment {
    /// Left in left-to-right text, right in right-to-left text.
    #[default]
    Start,
    Left,
    Center,
    Right,
    /// Both edges, but for a paragraph's last line.
    Justified,
}

/// What happens to a line too long for the layout's width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Wrap {
    /// Break between words, inside a word too long for a line by itself.
    #[default]
    Words,
    /// Break between characters.
    Characters,
    /// Don't break: lines run past the width (drawing clips them where the
    /// caller clips).
    None,
    /// Don't break, and end a line that doesn't fit with an ellipsis.
    Truncate,
}

/// Text laid out: lines broken to a width, with the geometry of every
/// character in them. Laying out is the costly part; drawing a layout,
/// and asking it where things are, is cheap, so keep layouts and make new
/// ones when the text, its styles or the width change.
#[derive(Clone, Debug)]
pub struct TextLayout {
    text: Arc<str>,
    frame: Arc<Frame>,
    width: Option<f64>,
}

/// Where a point falls in a layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Where an insertion point there goes: a byte offset, at the nearer
    /// edge of the character under the point.
    pub offset: usize,
    /// The character under the point (its first byte).
    pub character: usize,
    /// The point is past the end of a wrapped line: the caret belongs at
    /// that line's end rather than at the start of the next.
    pub upstream: bool,
}

/// A caret: its line's top and height at `x`, and where directions meet,
/// the secondary caret's `x`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub x: f64,
    pub top: f64,
    pub height: f64,
    pub secondary: Option<f64>,
}

impl Caret {
    pub fn rect(&self, thickness: f64) -> Rect {
        Rect::new(self.x, self.top, self.x + thickness, self.top + self.height)
    }
}

/// A line of a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct LineMetrics {
    /// Its byte range in the text, trailing whitespace and the separator
    /// ending it included.
    pub range: Range<usize>,
    pub top: f64,
    pub height: f64,
    /// The baseline, from the layout's top.
    pub baseline: f64,
    /// Where its content starts, and how wide it is.
    pub x: f64,
    pub width: f64,
}

/// What a [`TextLayout`] is made from: text in runs of styles, and how its
/// lines go.
#[derive(Clone, Debug)]
pub struct TextLayoutBuilder {
    text: String,
    base: TextStyle,
    runs: Vec<(Range<usize>, TextStyle)>,
    width: Option<f64>,
    alignment: Alignment,
    wrap: Wrap,
    max_lines: usize,
    line_spacing: f64,
    paragraph_spacing: f64,
    line_height_multiple: f64,
}

impl TextLayoutBuilder {
    /// Style `range` (bytes, on character boundaries) as `style`, over the
    /// base style and earlier runs.
    pub fn style(mut self, range: Range<usize>, style: TextStyle) -> Self {
        self.runs.push((range, style));
        self
    }

    /// Break lines at this width (points); none for no limit.
    pub fn width(mut self, width: impl Into<Option<f64>>) -> Self {
        self.width = width.into().filter(|w| w.is_finite());
        self
    }

    pub fn alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn wrap(mut self, wrap: Wrap) -> Self {
        self.wrap = wrap;
        self
    }

    /// Lay out at most `lines` lines (0 for no limit); with
    /// [`Wrap::Truncate`], the last one ends in an ellipsis if text is left
    /// out.
    pub fn max_lines(mut self, lines: usize) -> Self {
        self.max_lines = lines;
        self
    }

    /// Points between lines of a paragraph.
    pub fn line_spacing(mut self, points: f64) -> Self {
        self.line_spacing = points;
        self
    }

    /// Points after each paragraph but the last.
    pub fn paragraph_spacing(mut self, points: f64) -> Self {
        self.paragraph_spacing = points;
        self
    }

    /// Lines this many times their natural height (0 or 1: natural).
    pub fn line_height_multiple(mut self, multiple: f64) -> Self {
        self.line_height_multiple = multiple;
        self
    }

    pub fn build(self) -> TextLayout {
        let paragraph = Paragraph {
            alignment: match self.alignment {
                Alignment::Start => Align::Natural,
                Alignment::Left => Align::Left,
                Alignment::Center => Align::Center,
                Alignment::Right => Align::Right,
                Alignment::Justified => Align::Justified,
            },
            line_break: match self.wrap {
                Wrap::Words => LineBreak::WordWrap,
                Wrap::Characters => LineBreak::CharWrap,
                Wrap::None => LineBreak::Clip,
                Wrap::Truncate => LineBreak::TruncateTail,
            },
            line_spacing: self.line_spacing,
            paragraph_spacing: self.paragraph_spacing,
            line_height_multiple: self.line_height_multiple,
            ..Paragraph::default()
        };
        // Cut the text where styles start and end, each piece taking the
        // last run over it.
        let len = self.text.len();
        let mut cuts: Vec<usize> = vec![0, len];
        for (range, _) in &self.runs {
            cuts.push(range.start.min(len));
            cuts.push(range.end.min(len));
        }
        cuts.retain(|&c| self.text.is_char_boundary(c));
        cuts.sort_unstable();
        cuts.dedup();
        let mut attrs: Vec<Attrs> = vec![self.base.attrs(&paragraph)];
        let mut styles: Vec<&TextStyle> = vec![&self.base];
        let mut spans = Vec::new();
        let mut utf16 = 0u32;
        for pair in cuts.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            let style = self.runs.iter().rev().find(|(r, _)| r.start <= start && end <= r.end).map(|(_, s)| s);
            let index = match style {
                None => 0,
                Some(style) => match styles.iter().position(|s| *s == style) {
                    Some(i) => i,
                    None => {
                        styles.push(style);
                        attrs.push(style.attrs(&paragraph));
                        attrs.len() - 1
                    }
                },
            };
            let units = self.text[start..end].encode_utf16().count() as u32;
            spans.push(Span { start: utf16, end: utf16 + units, attrs: index as u32 });
            utf16 += units;
        }
        if spans.is_empty() {
            spans.push(Span { start: 0, end: 0, attrs: 0 });
        }
        let container = Container {
            width: self.width.map_or(f32::INFINITY, |w| w as f32),
            max_lines: self.max_lines,
            truncation: (self.wrap == Wrap::Truncate).then_some(LineBreak::TruncateTail),
            ..Container::UNBOUNDED
        };
        let frame = Frame::new(Styled { text: &self.text, attrs: &attrs, spans: &spans }, container);
        TextLayout { text: self.text.into(), frame: Arc::new(frame), width: self.width }
    }
}

impl TextLayout {
    /// `text` in one style, unbounded: a line per paragraph.
    pub fn new(text: &str, style: &TextStyle) -> TextLayout {
        TextLayout::builder(text, style).build()
    }

    /// `text` in `style` broken into lines at `width` points.
    pub fn wrapped(text: &str, style: &TextStyle, width: f64) -> TextLayout {
        TextLayout::builder(text, style).width(width).build()
    }

    /// A layout of `text` with `base` as the style of whatever no run
    /// restyles.
    pub fn builder(text: &str, base: &TextStyle) -> TextLayoutBuilder {
        TextLayoutBuilder {
            text: text.to_owned(),
            base: base.clone(),
            runs: Vec::new(),
            width: None,
            alignment: Alignment::Start,
            wrap: Wrap::Words,
            max_lines: 0,
            line_spacing: 0.0,
            paragraph_spacing: 0.0,
            line_height_multiple: 0.0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The width lines were broken at, if any.
    pub fn max_width(&self) -> Option<f64> {
        self.width
    }

    /// How much room the text takes: its widest line (from the layout's
    /// left, to where its last character that shows ends: the spaces a line
    /// wrapped after don't count), and its lines' height.
    pub fn size(&self) -> Size {
        let width = self
            .frame
            .lines()
            .map(|l| f64::from(l.line.x + l.line.width - l.line.trailing_whitespace))
            .fold(0.0, f64::max);
        Size::new(width, f64::from(self.frame.height()))
    }

    /// The first line's baseline, from the top: where to line the text up
    /// with other text.
    pub fn first_baseline(&self) -> f64 {
        self.frame.lines().next().map_or(0.0, |l| f64::from(l.top() + l.line.baseline))
    }

    pub fn lines(&self) -> Vec<LineMetrics> {
        self.frame
            .lines()
            .map(|l| {
                let range = l.range();
                LineMetrics {
                    range: self.byte(range.start)..self.byte(range.end),
                    top: f64::from(l.top()),
                    height: f64::from(l.line.height),
                    baseline: f64::from(l.top() + l.line.baseline),
                    x: f64::from(l.line.x),
                    width: f64::from(l.line.width),
                }
            })
            .collect()
    }

    /// The character at `point` (from the layout's top left) and where an
    /// insertion point there goes.
    pub fn hit_test(&self, point: Point) -> Hit {
        let hit = self.frame.index_at(point.x as f32, point.y as f32);
        Hit { offset: self.byte(hit.index), character: self.byte(hit.character), upstream: hit.upstream }
    }

    /// The caret before the character at byte `offset`; `upstream` puts a
    /// caret at a wrapped line's end on that line rather than the next.
    pub fn caret(&self, offset: usize, upstream: bool) -> Caret {
        let c = self.frame.caret(self.utf16(offset), upstream);
        c.map_or(Caret { x: 0.0, top: 0.0, height: 0.0, secondary: None }, |c| Caret {
            x: f64::from(c.x),
            top: f64::from(c.top),
            height: f64::from(c.height),
            secondary: c.secondary.map(f64::from),
        })
    }

    /// The rectangles that show bytes `range` selected, line by line.
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<Rect> {
        let range = self.utf16(range.start)..self.utf16(range.end);
        self.frame
            .selection_rects(range)
            .into_iter()
            .map(|[x0, y0, x1, y1]| Rect::new(f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1)))
            .collect()
    }

    pub(crate) fn frame(&self) -> &Frame {
        &self.frame
    }

    /// The UTF-16 index of byte `offset` (clamped to the text, and back to
    /// a character boundary).
    fn utf16(&self, offset: usize) -> u32 {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        self.text[..offset].encode_utf16().count() as u32
    }

    /// The byte offset of UTF-16 index `index`.
    fn byte(&self, index: u32) -> usize {
        let mut units = 0u32;
        for (byte, c) in self.text.char_indices() {
            if units >= index {
                return byte;
            }
            units += c.len_utf16() as u32;
        }
        self.text.len()
    }
}

/// Measure `text` in `style` on one line per paragraph, from the text
/// engine's cache: what [`Canvas::draw_label`](crate::Canvas::draw_label)
/// takes up.
pub fn measure(text: &str, style: &TextStyle) -> Size {
    let laid = label(text, style);
    Size::new(f64::from(laid.width), f64::from(laid.height))
}

/// `text` laid out for a label, through the thread's layout cache.
pub(crate) fn label(text: &str, style: &TextStyle) -> Arc<layout::TextLayout> {
    let attrs = [style.attrs(&Paragraph::default())];
    let runs = [layout::Run { start: 0, end: text.len(), attrs: 0 }];
    layout::lay_out(text, &attrs, &runs, &layout::Options::UNBOUNDED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> TextStyle {
        TextStyle::new(Font::system(13.0), Color::BLACK)
    }

    #[test]
    fn offsets_convert_through_utf16() {
        let text = "aé𝄞b\nc";
        let layout = TextLayout::new(text, &style());
        for (byte, _) in text.char_indices().chain([(text.len(), ' ')]) {
            assert_eq!(layout.byte(layout.utf16(byte)), byte, "byte {byte}");
        }
        // Inside a character, back to its start.
        assert_eq!(layout.utf16(2), layout.utf16(1));
    }

    #[test]
    fn lines_follow_paragraphs_and_width() {
        let layout = TextLayout::new("one\ntwo", &style());
        let lines = layout.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].range, 0..4);
        assert_eq!(lines[1].range, 4..7);
        assert!(lines[1].top >= lines[0].top + lines[0].height - 0.01);
        let narrow = TextLayout::wrapped("a few words that wrap", &style(), 40.0);
        assert!(narrow.lines().len() > 1, "{:?}", narrow.lines());
        assert!(narrow.size().width <= 40.0 + 1.0);
    }

    #[test]
    fn hits_and_carets_agree() {
        let layout = TextLayout::new("hello world", &style());
        let caret = layout.caret(5, false);
        assert!(caret.x > 0.0 && caret.height > 0.0);
        let hit = layout.hit_test(Point::new(caret.x + 0.1, caret.top + 1.0));
        assert_eq!(hit.offset, 5);
        assert!(layout.hit_test(Point::new(-10.0, 0.0)).offset == 0);
        assert_eq!(layout.hit_test(Point::new(1e4, 1.0)).offset, 11);
        let rects = layout.selection_rects(0..5);
        assert_eq!(rects.len(), 1);
        assert!((rects[0].x1 - caret.x).abs() < 0.5);
    }

    #[test]
    fn runs_restyle_parts() {
        let bold = TextStyle::new(Font::system(13.0).bold(), Color::BLACK);
        let plain = TextLayout::new("plain bold", &style());
        let mixed = TextLayout::builder("plain bold", &style()).style(6..10, bold).build();
        assert!(mixed.size().width >= plain.size().width);
        assert_eq!(mixed.text(), "plain bold");
    }

    #[test]
    fn labels_measure_as_they_lay_out() {
        let size = measure("Hello", &style());
        assert!(size.width > 10.0 && size.height > 10.0, "{size:?}");
        assert_eq!(measure("", &style()).width, 0.0);
    }
}
