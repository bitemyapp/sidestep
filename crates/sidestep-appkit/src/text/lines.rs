//! Text laid out for TextKit: lines with their UTF-16 ranges and metrics,
//! clusters with caret positions and bidi levels, and glyph runs ready to
//! record, on any thread.
//!
//! `NSTextStorage` and `NSLayoutManager` may live on any thread, so the
//! input is plain data (the text as UTF-8, attributes resolved to
//! [`Attrs`] and runs of them over UTF-16 code units, which is how
//! `NSString` counts) and so is the output: everything here is `Send` and
//! `Sync`. Each thread lays text out with its own parley context, as string
//! drawing does.
//!
//! - [`lay_out_paragraph`] lays out one paragraph, from its start or from
//!   any line start in it, as many lines as asked for. A long paragraph is
//!   shaped only as far as those lines need (see [`window_lines`]), so an
//!   editor laying out what it shows, or what an edit touched, pays for
//!   that and not for the whole paragraph.
//! - [`Frame`] stacks a text's paragraphs, answers the geometry questions a
//!   layout manager is asked (the character at a point, the caret at an
//!   index, the rectangles of a selection, the line fragment of an index,
//!   all in UTF-16 indexes), and after an edit lays out again only the
//!   lines that can have changed ([`Frame::edit`]).
//!
//! Lines break as string drawing breaks them (`text::layout`), with the
//! paragraph's base direction found once for the whole paragraph, so a
//! paragraph laid out from one of its lines gets the lines it would have
//! had. Offsets in a line are relative to its paragraph, and a cluster's to
//! its line, so that an edit moves the paragraphs and lines after it
//! without touching their clusters.
//!
//! Carets follow AppKit's layout manager, as measured on macOS: between two
//! characters of one direction the caret is at their shared edge; where
//! directions meet, it goes to the character nearer the paragraph's own
//! direction (the lower bidi level), and the other edge is the secondary
//! caret. The line's start and end count as characters of the paragraph's
//! direction, so the caret at the end of a right-to-left paragraph is at
//! its left end.

use std::ops::Range;
use std::sync::Arc;

use icu_properties::CodePointMapData;
use icu_properties::props::BidiClass;

use super::Ctx;
use super::layout::{self, Align, Attrs, Direction, LaidLine, LineBreak, Options, PlacedFill, PlacedRun, Req, Run};

/// A run of attributes over UTF-16 code units of the text: `attrs` indexes
/// the attributes passed alongside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: u32,
    pub end: u32,
    pub attrs: u32,
}

/// Text with its attributes, as a text storage holds it: `spans` cover
/// the text in order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Styled<'a> {
    pub text: &'a str,
    pub attrs: &'a [Attrs],
    pub spans: &'a [Span],
}

/// Where lines go, as a text container says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Container {
    /// The width lines break at, from the container's left; infinite for
    /// none.
    pub width: f32,
    /// The most lines to lay out; 0 for no limit.
    pub max_lines: usize,
    /// How to cut the last line when the limit leaves text out: one of the
    /// truncating modes, or `None` to leave the line as it is.
    pub truncation: Option<LineBreak>,
    /// Add the fonts' leading to line heights (`usesFontLeading`).
    pub font_leading: bool,
}

impl Container {
    pub const UNBOUNDED: Container =
        Container { width: f32::INFINITY, max_lines: 0, truncation: None, font_leading: false };
}

/// Characters drawn as one: a character, a share of a ligature, an emoji
/// sequence, a character with its marks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cluster {
    /// The UTF-16 range of the cluster, from its line's start.
    pub start: u32,
    pub end: u32,
    /// Its left edge from the container's left, and its width. A tab's
    /// reaches to where the text after it starts.
    pub x: f32,
    pub advance: f32,
    /// Its bidi embedding level: even runs left to right, odd right to
    /// left.
    pub level: u8,
}

impl Cluster {
    fn rtl(&self) -> bool {
        self.level & 1 == 1
    }

    /// The edge where the cluster's text starts, in its own direction.
    fn leading(&self) -> f32 {
        if self.rtl() { self.x + self.advance } else { self.x }
    }

    fn trailing(&self) -> f32 {
        if self.rtl() { self.x } else { self.x + self.advance }
    }
}

/// A laid-out line.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    /// The UTF-16 range of the line in its paragraph: its characters and
    /// trailing whitespace, and the separator that ends it, if one does.
    pub range: Range<u32>,
    /// UTF-16 units of separator ending `range`: 0 where the line wraps, 1,
    /// or 2 for "\r\n".
    pub separator: u8,
    /// The line's top, from the top of the first line laid out with it.
    pub top: f32,
    pub height: f32,
    /// The baseline, from the line's top.
    pub baseline: f32,
    /// The fonts' largest ascent and descent, each rounded to whole points
    /// as the height is, with any baseline offsets, and their leading.
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
    /// Where the content starts, from the container's left: the indent and
    /// the alignment's offset.
    pub x: f32,
    /// The content's typographic width, trailing whitespace included, and
    /// the trailing whitespace's.
    pub width: f32,
    pub trailing_whitespace: f32,
    /// The paragraph runs right to left.
    pub rtl: bool,
    /// The clusters, left to right.
    pub clusters: Box<[Cluster]>,
    /// For a truncated line, the characters its ellipsis stands for, from
    /// the paragraph's start.
    pub elided: Option<Range<u32>>,
    /// Glyph runs and fills to record, from the container's left and the
    /// line's top (a run's `y` is its baseline).
    pub runs: Box<[PlacedRun]>,
    pub fills: Box<[PlacedFill]>,
}

impl Line {
    /// Where the line's characters end: before its separator.
    pub fn content_end(&self) -> u32 {
        self.range.end - u32::from(self.separator)
    }

    fn level(&self) -> u8 {
        u8::from(self.rtl)
    }

    /// The edges where the line's text starts and ends, in the paragraph's
    /// direction.
    fn start_edge(&self) -> f32 {
        if self.rtl { self.x + self.width } else { self.x }
    }

    fn end_edge(&self) -> f32 {
        if self.rtl { self.x } else { self.x + self.width }
    }

    /// The cluster holding the character at `index` (from the paragraph's
    /// start).
    fn cluster_at(&self, index: u32) -> Option<&Cluster> {
        let at = index.checked_sub(self.range.start)?;
        self.clusters.iter().find(|c| c.start <= at && at < c.end)
    }

    /// Where the caret goes before the character at `index` (from the
    /// paragraph's start), and, where two directions meet, the secondary
    /// caret. An index inside a cluster goes to the cluster's start.
    pub fn caret_x(&self, index: u32) -> (f32, Option<f32>) {
        let mut index = index.clamp(self.range.start, self.content_end());
        if let Some(c) = self.cluster_at(index) {
            index = self.range.start + c.start;
        }
        // What comes before the caret and after it: its level and the edge
        // touching the caret. The line's ends count as the paragraph's.
        let before = (index > self.range.start)
            .then(|| self.cluster_at(index - 1))
            .flatten()
            .map_or((self.level(), self.start_edge()), |c| (c.level, c.trailing()));
        let after = (index < self.content_end())
            .then(|| self.cluster_at(index))
            .flatten()
            .map_or((self.level(), self.end_edge()), |c| (c.level, c.leading()));
        match before.0.cmp(&after.0) {
            std::cmp::Ordering::Equal => (after.1, None),
            std::cmp::Ordering::Greater => (after.1, Some(before.1)),
            std::cmp::Ordering::Less => (before.1, Some(after.1)),
        }
    }

    /// The character at `x` and where an insertion point there goes.
    pub fn hit(&self, x: f32) -> Hit {
        let Some(last) = self.clusters.last() else {
            return Hit { index: self.range.start, upstream: false, character: self.range.start, fraction: 0.0 };
        };
        let c = self.clusters.iter().find(|c| x < c.x + c.advance).unwrap_or(last);
        let through = if c.advance > 0.0 { ((x - c.x) / c.advance).clamp(0.0, 1.0) } else { 0.0 };
        let fraction = if c.rtl() { 1.0 - through } else { through };
        let (start, end) = (self.range.start + c.start, self.range.start + c.end);
        // Past a line's end, before its separator; past a wrapped line's
        // end, at that end rather than the next line's start.
        let index = (if fraction < 0.5 { start } else { end }).min(self.content_end());
        let upstream = self.separator == 0 && index == self.range.end;
        Hit { index, upstream, character: start, fraction }
    }

    /// The stretches of the line, left to right, that the characters in
    /// `range` (from the paragraph's start) cover: more than one where
    /// directions mix.
    pub fn spans(&self, range: Range<u32>) -> Vec<(f32, f32)> {
        let mut out: Vec<(f32, f32)> = Vec::new();
        let (from, to) = (range.start.saturating_sub(self.range.start), range.end.saturating_sub(self.range.start));
        for c in self.clusters.iter().filter(|c| c.start < to && c.end > from) {
            match out.last_mut() {
                Some(last) if (last.1 - c.x).abs() < 0.01 => last.1 = c.x + c.advance,
                _ => out.push((c.x, c.x + c.advance)),
            }
        }
        out
    }
}

/// Where a point falls in text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hit {
    /// Where an insertion point goes: at the nearer edge of the character.
    pub index: u32,
    /// The insertion point belongs at the end of the line above rather than
    /// at the start of the next (a click past the end of a wrapped line).
    pub upstream: bool,
    /// The character under the point, and how far through it the point is,
    /// in the character's own direction.
    pub character: u32,
    pub fraction: f32,
}

/// Spacing a paragraph's style asks for around its lines.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Spacing {
    pub line: f32,
    pub before: f32,
    pub after: f32,
}

/// A paragraph's lines, or some of them.
#[derive(Clone, Debug)]
pub(crate) struct ParagraphLines {
    pub lines: Vec<Line>,
    /// The paragraph's length, separator included, in UTF-16 units and in
    /// bytes.
    pub len: u32,
    pub bytes: usize,
    /// The lines reach the paragraph's end (a limit didn't stop them).
    pub complete: bool,
    pub spacing: Spacing,
    /// The paragraph's style and its base direction, as found.
    pub style: layout::Paragraph,
    pub rtl: bool,
}

impl ParagraphLines {
    /// From the first line's top to the last one's bottom.
    pub fn height(&self) -> f32 {
        self.lines.last().map_or(0.0, |l| l.top + l.height)
    }

    /// The line holding `index` (from the paragraph's start); with
    /// `upstream`, an index where a wrapped line ends is that line's.
    pub fn line_at(&self, index: u32, upstream: bool) -> Option<usize> {
        let at = self.lines.partition_point(|l| l.range.end <= index);
        let at = if at == self.lines.len() {
            // The end of a paragraph without a separator, or past the
            // lines laid out.
            let last = self.lines.len().checked_sub(1)?;
            if index > self.lines[last].range.end {
                return None;
            }
            last
        } else {
            at
        };
        if upstream && at > 0 && index == self.lines[at].range.start && self.lines[at - 1].separator == 0 {
            return Some(at - 1);
        }
        Some(at)
    }
}

/// Lay out the paragraph that starts `styled.text`, from `from` (a line
/// start, in UTF-16 units from the paragraph's start), with the lines' tops
/// from the first one's. `container.max_lines` caps the lines laid out
/// here; the next call can go on from where the last one ends.
pub(crate) fn lay_out_paragraph(styled: Styled<'_>, container: &Container, from: u32) -> ParagraphLines {
    super::with_ctx(|ctx| paragraph(ctx, styled, 0, container, from, container.max_lines))
}

/// Lay out the paragraph starting `styled.text` from `from`, at most
/// `limit` lines (0 for all). `base` is the text's start in the spans'
/// UTF-16 coordinates.
fn paragraph(
    ctx: &mut Ctx,
    styled: Styled<'_>,
    base: u32,
    container: &Container,
    from: u32,
    limit: usize,
) -> ParagraphLines {
    let extent = Extent::new(styled.text);
    paragraph_in(ctx, styled, &extent, base, container, from, limit)
}

/// Where the paragraph starting a text ends, and what finding one's way
/// in it takes: found once however many times it is laid out in pieces.
struct Extent<'a> {
    /// The paragraph without its separator.
    content: &'a str,
    /// Bytes and UTF-16 units of it with its separator, and of the
    /// separator alone.
    bytes: usize,
    len: u32,
    sep16: u8,
    index: Utf16,
    /// The byte ranges between line separators.
    segments: Vec<(usize, usize)>,
}

impl<'a> Extent<'a> {
    fn new(text: &'a str) -> Extent<'a> {
        let (content_end, sep_bytes, sep16) = paragraph_end(text);
        let bytes = content_end + sep_bytes;
        let index = Utf16::new(&text[..bytes]);
        let content = &text[..content_end];
        Extent { content, bytes, len: index.utf16(bytes), sep16, index, segments: segments(content) }
    }
}

/// [`paragraph`], for a paragraph whose extent is known.
fn paragraph_in(
    ctx: &mut Ctx,
    styled: Styled<'_>,
    extent: &Extent<'_>,
    base: u32,
    container: &Container,
    from: u32,
    limit: usize,
) -> ParagraphLines {
    let text = styled.text;
    let Extent { content, bytes, len, sep16, ref index, ref segments } = *extent;
    let content_end = content.len();
    let runs = byte_runs(styled.spans, base, len, index, &text[..bytes]);
    let para = &styled.attrs[runs[0].attrs as usize].paragraph;
    let rtl = base_direction(para, content);
    let direction = if rtl { Direction::RightToLeft } else { Direction::LeftToRight };
    let opts = Options {
        width: container.width,
        height: f32::INFINITY,
        all_lines: true,
        font_leading: container.font_leading,
        truncate_last: false,
    };
    let spacing = Spacing {
        line: para.line_spacing as f32,
        before: para.paragraph_spacing_before as f32,
        after: para.paragraph_spacing as f32,
    };
    let mut out = ParagraphLines { lines: Vec::new(), len, bytes, complete: true, spacing, style: para.clone(), rtl };
    let from = index.byte(content, from.min(index.utf16(content_end)));
    let mut cursor = 0;
    let mut top = 0.0;
    let mut last_start = None;
    for (i, &(seg_start, seg_end)) in segments.iter().enumerate() {
        if seg_end <= from && seg_start < from {
            continue;
        }
        if limit > 0 && out.lines.len() >= limit {
            out.complete = false;
            break;
        }
        let last_segment = i + 1 == segments.len();
        let start = seg_start.max(from);
        let seg_runs = layout::runs_in(&runs, &mut cursor, start, seg_end);
        let req = Req { first_in_paragraph: start == 0, tail: None, direction, clusters: true };
        let want = if limit == 0 { 0 } else { limit - out.lines.len() };
        let (laid, complete) =
            window_lines(ctx, &content[start..seg_end], styled.attrs, &seg_runs, para, req, &opts, want);
        let n = laid.len();
        for (k, l) in laid.into_iter().enumerate() {
            let separator = if complete && k + 1 == n { if last_segment { sep16 } else { 1 } } else { 0 };
            let empty = l.clusters.is_empty() && l.runs.is_empty();
            let mut line = line_of(l, start, index, rtl, separator, top);
            if empty {
                line.x = empty_x(para, rtl, container.width, start == 0);
            }
            top += line.height + spacing.line;
            last_start = Some((start, seg_end, last_segment));
            out.lines.push(line);
        }
        if !complete {
            out.complete = false;
            break;
        }
    }
    // A limit that leaves text out cuts the last line short, if asked to;
    // the tail mode also shows that text follows the paragraph.
    if let (Some(mode), Some(line), Some((seg, seg_end, last_segment))) =
        (container.truncation, out.lines.last(), last_start)
        && limit > 0
        && out.lines.len() >= limit
    {
        let follows = !last_segment || bytes < text.len();
        if !out.complete || (follows && mode == LineBreak::TruncateTail) {
            let start = index.byte(content, line.range.start).max(seg);
            let seg_runs = layout::runs_in(&runs, &mut 0, start, seg_end);
            let req = Req { first_in_paragraph: start == 0, tail: Some((mode, follows)), direction, clusters: true };
            let laid = layout::segment_lines(ctx, &content[start..seg_end], styled.attrs, &seg_runs, para, req, &opts);
            if let Some(l) = laid.into_iter().next() {
                let top = line.top;
                let separator = if last_segment { sep16 } else { 1 };
                out.lines.pop();
                out.lines.push(line_of(l, start, index, rtl, separator, top));
                out.complete = last_segment;
            }
        }
    }
    out
}

/// Whether a paragraph with style `para` and text `content` runs right to
/// left: as its style says, or as its first strong character does.
fn base_direction(para: &layout::Paragraph, content: &str) -> bool {
    match para.direction {
        Direction::LeftToRight => false,
        Direction::RightToLeft => true,
        Direction::Natural => first_strong(content) == Some(true),
    }
}

/// A laid-out line of the segment starting at byte `start` of its
/// paragraph, whose UTF-16 offsets `index` knows.
fn line_of(l: LaidLine, start: usize, index: &Utf16, rtl: bool, separator: u8, top: f32) -> Line {
    let u = |b: usize| index.utf16(start + b);
    let line_start = u(l.text_start);
    let base = u8::from(rtl);
    let clusters = l
        .clusters
        .iter()
        .map(|c| Cluster {
            start: u(c.start) - line_start,
            end: u(c.end) - line_start,
            x: c.x,
            advance: c.advance,
            level: if c.rtl == rtl { base } else { base + 1 },
        })
        .collect();
    Line {
        range: line_start..u(l.text_end) + u32::from(separator),
        separator,
        top,
        height: l.height,
        baseline: l.height - l.descent,
        ascent: l.ascent,
        descent: l.descent,
        leading: l.leading,
        x: l.x,
        width: l.advance,
        trailing_whitespace: l.trailing,
        rtl,
        clusters,
        elided: l.elided.map(|(a, b)| u(a)..u(b)),
        runs: l.runs.into(),
        fills: l.fills.into(),
    }
}

/// Where an empty line's caret goes: its start edge, as alignment and
/// indents place it.
fn empty_x(para: &layout::Paragraph, rtl: bool, width: f32, first: bool) -> f32 {
    let indent = if first { para.first_line_head_indent } else { para.head_indent }.max(0.0) as f32;
    if !width.is_finite() {
        return indent;
    }
    let tail = para.tail_indent as f32;
    let right = if tail > 0.0 { tail.min(width) } else { width + tail };
    match para.alignment {
        Align::Left => indent,
        Align::Right => right,
        Align::Center => (indent + right) / 2.0,
        Align::Natural | Align::Justified if rtl => right,
        Align::Natural | Align::Justified => indent,
    }
}

/// Paragraphs shorter than this are shaped whole.
const MIN_WINDOW: usize = 4096;

/// The first `want` lines of a segment (all of them for 0), and whether
/// they are all it has. A long segment is shaped only as far as they need:
/// a window of it, doubled until the lines that end inside it are enough.
/// The window's last line might go on past it, so it isn't taken; the
/// lines before it break as they would in the whole text, since a line
/// ends at a break the text before the window's end decides.
#[allow(clippy::too_many_arguments)]
fn window_lines(
    ctx: &mut Ctx,
    text: &str,
    attrs: &[Attrs],
    runs: &[Run],
    para: &layout::Paragraph,
    req: Req,
    opts: &Options,
    want: usize,
) -> (Vec<LaidLine>, bool) {
    let wraps = matches!(para.line_break, LineBreak::WordWrap | LineBreak::CharWrap) && opts.width.is_finite();
    // A guess at a line's bytes, generous: the width over characters of
    // 0.4 em, three bytes each unless the text starts in ASCII.
    let size = attrs[runs[0].attrs as usize].font.size.max(1.0);
    let bytes = if text.as_bytes()[..text.len().min(256)].is_ascii() { 1 } else { 3 };
    let per_line = (opts.width / (size * 0.4)).clamp(8.0, 4096.0) as usize * bytes;
    let mut window = (want + 1).saturating_mul(per_line).max(256);
    loop {
        if want == 0 || !wraps || text.len() <= MIN_WINDOW || window >= text.len() {
            let mut lines = layout::segment_lines(ctx, text, attrs, runs, para, req, opts);
            let complete = want == 0 || lines.len() <= want;
            lines.truncate(if want == 0 { usize::MAX } else { want });
            return (lines, complete);
        }
        let mut cut = window;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let cut_runs = layout::runs_in(runs, &mut 0, 0, cut);
        let mut lines = layout::segment_lines(ctx, &text[..cut], attrs, &cut_runs, para, req, opts);
        lines.pop();
        if lines.len() >= want {
            lines.truncate(want);
            return (lines, false);
        }
        window = window.saturating_mul(2);
    }
}

/// Where the first paragraph of `text` ends: the end of its content and
/// its separator's length in bytes and in UTF-16 units (none when the text
/// ends first).
fn paragraph_end(text: &str) -> (usize, usize, u8) {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(k) = bytes[from..].iter().position(|&b| matches!(b, b'\n' | b'\r' | 0xE2 | 0xC2)) {
        let at = from + k;
        match bytes[at] {
            b'\n' => return (at, 1, 1),
            b'\r' if bytes.get(at + 1) == Some(&b'\n') => return (at, 2, 2),
            b'\r' => return (at, 1, 1),
            // U+2029 and U+0085.
            0xE2 if bytes[at + 1..].starts_with(&[0x80, 0xA9]) => return (at, 3, 1),
            0xC2 if bytes.get(at + 1) == Some(&0x85) => return (at, 2, 1),
            _ => from = at + 1,
        }
    }
    (text.len(), 0, 0)
}

/// The byte ranges of a paragraph's text between line separators (U+2028).
fn segments(content: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (at, _) in content.match_indices('\u{2028}') {
        out.push((start, at));
        start = at + '\u{2028}'.len_utf8();
    }
    out.push((start, content.len()));
    out
}

/// The spans over the paragraph `base..base + len` as byte runs of its
/// text. A paragraph no span covers (the empty one after a text's last
/// separator) takes the attributes before it, as typing there would.
fn byte_runs(spans: &[Span], base: u32, len: u32, index: &Utf16, text: &str) -> Vec<Run> {
    let first = spans.partition_point(|s| s.end <= base);
    let mut out: Vec<Run> = spans[first..]
        .iter()
        .take_while(|s| s.start < base + len)
        .map(|s| Run {
            start: index.byte(text, s.start.max(base) - base),
            end: index.byte(text, s.end.min(base + len) - base),
            attrs: s.attrs,
        })
        .filter(|r| r.end > r.start)
        .collect();
    if out.is_empty() {
        let attrs = spans.get(first).or(spans.last()).map_or(0, |s| s.attrs);
        out.push(Run { start: 0, end: text.len(), attrs });
    }
    out
}

/// `spans` as byte runs of `text`, as string drawing takes attributes
/// (`layout::lay_out`).
pub(crate) fn runs_of(text: &str, spans: &[Span]) -> Vec<Run> {
    let index = Utf16::new(text);
    byte_runs(spans, 0, index.utf16(text.len()), &index, text)
}

/// The base direction the first strong character gives, skipping isolated
/// text as the bidi algorithm does: right to left or not, or `None` if no
/// character is strong.
fn first_strong(text: &str) -> Option<bool> {
    let classes = CodePointMapData::<BidiClass>::new();
    let mut isolates = 0u32;
    for c in text.chars() {
        // ASCII letters are strong left to right, and nothing else in
        // ASCII is strong or isolates.
        if c.is_ascii() {
            if isolates == 0 && c.is_ascii_alphabetic() {
                return Some(false);
            }
            continue;
        }
        let class = classes.get(c);
        if class == BidiClass::LeftToRightIsolate
            || class == BidiClass::RightToLeftIsolate
            || class == BidiClass::FirstStrongIsolate
        {
            isolates += 1;
        } else if class == BidiClass::PopDirectionalIsolate {
            isolates = isolates.saturating_sub(1);
        } else if isolates == 0 && class == BidiClass::LeftToRight {
            return Some(false);
        } else if isolates == 0 && (class == BidiClass::RightToLeft || class == BidiClass::ArabicLetter) {
            return Some(true);
        }
    }
    None
}

/// UTF-16 offsets of a string's bytes and back. ASCII text needs no table;
/// otherwise each character that isn't ASCII has an entry where it ends,
/// and the ASCII between them counts one for one.
struct Utf16 {
    /// (byte, UTF-16 offset) where each character that isn't ASCII ends.
    ends: Vec<(u32, u32)>,
}

impl Utf16 {
    fn new(text: &str) -> Utf16 {
        let mut ends = Vec::new();
        if !text.is_ascii() {
            let mut units = 0u32;
            for (at, c) in text.char_indices() {
                units += c.len_utf16() as u32;
                if !c.is_ascii() {
                    ends.push(((at + c.len_utf8()) as u32, units));
                }
            }
        }
        Utf16 { ends }
    }

    /// The UTF-16 offset of byte `byte`, a character boundary.
    fn utf16(&self, byte: usize) -> u32 {
        let byte = byte as u32;
        match self.ends.partition_point(|e| e.0 <= byte) {
            0 => byte,
            i => {
                let (b, u) = self.ends[i - 1];
                u + (byte - b)
            }
        }
    }

    /// The byte of UTF-16 offset `units` in `text`, or of the start of the
    /// character it falls inside.
    fn byte(&self, text: &str, units: u32) -> usize {
        let mut byte = match self.ends.partition_point(|e| e.1 <= units) {
            0 => units as usize,
            i => {
                let (b, u) = self.ends[i - 1];
                (b + (units - u)) as usize
            }
        }
        .min(text.len());
        while !text.is_char_boundary(byte) {
            byte -= 1;
        }
        byte
    }
}

/// A text's paragraphs laid out and stacked: the first line's top is 0,
/// and paragraphs are spaced as string drawing spaces them (the line
/// spacing and the paragraph spacing after one and before the next). A
/// text ending in a paragraph separator, or empty, ends in an empty line,
/// as the insertion point after the separator needs.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub paragraphs: Vec<FrameParagraph>,
    pub container: Container,
    /// The text's length in UTF-16 units and in bytes.
    len: u32,
    bytes: usize,
}

/// A paragraph of a frame.
#[derive(Clone, Debug)]
pub(crate) struct FrameParagraph {
    /// Where it starts in the text, in UTF-16 units and in bytes.
    pub start: u32,
    pub byte: usize,
    /// Its first line's top.
    pub top: f32,
    /// Shared, so that a frame copies cheaply and one on another thread
    /// can keep what it drew.
    pub lines: Arc<ParagraphLines>,
}

impl FrameParagraph {
    /// Its last line's bottom.
    pub fn bottom(&self) -> f32 {
        self.top + self.lines.height()
    }
}

/// A line of a frame, with where its paragraph is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameLine<'a> {
    pub paragraph: usize,
    pub index: usize,
    /// The paragraph's start in the text and its top.
    pub start: u32,
    pub top: f32,
    pub line: &'a Line,
}

impl FrameLine<'_> {
    /// The line's UTF-16 range in the text.
    pub fn range(&self) -> Range<u32> {
        self.start + self.line.range.start..self.start + self.line.range.end
    }

    pub fn top(&self) -> f32 {
        self.top + self.line.top
    }
}

/// A caret: where it is, how tall, and the secondary caret where
/// directions meet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Caret {
    pub x: f32,
    pub top: f32,
    pub height: f32,
    pub secondary: Option<f32>,
}

/// A line fragment: the line's UTF-16 range in the text, and its box.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fragment {
    pub range: Range<u32>,
    pub top: f32,
    pub height: f32,
    pub baseline: f32,
    /// The content: where it starts from the container's left, and its
    /// width, trailing whitespace included.
    pub x: f32,
    pub width: f32,
}

/// Half the lines laid out first while looking for the old ones again
/// after an edit, in a paragraph long enough to be shaped in windows.
const EDIT_CHUNK: usize = 2;

impl Frame {
    /// Lay out all of `styled`.
    pub fn new(styled: Styled<'_>, container: Container) -> Frame {
        super::with_ctx(|ctx| {
            let mut frame = Frame { paragraphs: Vec::new(), container, len: 0, bytes: styled.text.len() };
            let mut left = container.max_lines;
            let (paragraphs, len) = frame.lay_out(ctx, styled, 0, 0, styled.text.len(), None, &mut left);
            frame.paragraphs = paragraphs;
            frame.len = len;
            frame
        })
    }

    /// The text's length in UTF-16 units.
    pub fn len(&self) -> u32 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// From the first line's top to the last one's bottom.
    pub fn height(&self) -> f32 {
        self.paragraphs.last().map_or(0.0, FrameParagraph::bottom)
    }

    /// The widest line's reach from the container's left.
    pub fn used_width(&self) -> f32 {
        self.lines().map(|l| l.line.x + l.line.width).fold(0.0, f32::max)
    }

    /// Every line, top to bottom.
    pub fn lines(&self) -> impl Iterator<Item = FrameLine<'_>> {
        self.paragraphs.iter().enumerate().flat_map(|(p, para)| {
            para.lines.lines.iter().enumerate().map(move |(index, line)| FrameLine {
                paragraph: p,
                index,
                start: para.start,
                top: para.top,
                line,
            })
        })
    }

    /// The paragraphs from byte `byte` (UTF-16 `start`) of the text up to
    /// byte `end` (a paragraph's start, or the text's end), placed after
    /// `above` (the paragraph before them, if any), and where they end in
    /// UTF-16 units. `left` counts down the container's line limit.
    #[allow(clippy::too_many_arguments)]
    fn lay_out(
        &self,
        ctx: &mut Ctx,
        styled: Styled<'_>,
        mut byte: usize,
        mut start: u32,
        end: usize,
        mut above: Option<(f32, Spacing)>,
        left: &mut usize,
    ) -> (Vec<FrameParagraph>, u32) {
        let mut out = Vec::new();
        let limited = self.container.max_lines > 0;
        loop {
            let at_end = byte >= styled.text.len();
            // After the text's last separator (or in an empty text) comes
            // an empty paragraph; past `end` in the middle, nothing.
            let ends_open = at_end && (styled.text.is_empty() || ends_in_separator(&styled.text[..byte]));
            if (byte >= end && !ends_open) || (limited && *left == 0) || (at_end && !ends_open) {
                break;
            }
            let rest = Styled { text: &styled.text[byte..], ..styled };
            let lines = paragraph(ctx, rest, start, &self.container, 0, if limited { *left } else { 0 });
            if limited {
                *left = left.saturating_sub(lines.lines.len());
            }
            let top = match above {
                Some((bottom, spacing)) => bottom + spacing.line + spacing.after + lines.spacing.before,
                None => 0.0,
            };
            let para = FrameParagraph { start, byte, top, lines: Arc::new(lines) };
            above = Some((para.bottom(), para.lines.spacing));
            (byte, start) = (byte + para.lines.bytes, start + para.lines.len);
            let stop = para.lines.bytes == 0;
            out.push(para);
            if stop {
                break;
            }
        }
        (out, start)
    }

    /// The paragraph holding `index`: the last starting at or before it.
    fn paragraph_at(&self, index: u32) -> usize {
        self.paragraphs.partition_point(|p| p.start <= index).saturating_sub(1)
    }

    /// The line holding `index`; with `upstream`, an index where a wrapped
    /// line ends is that line's rather than the next's.
    pub fn line_at(&self, index: u32, upstream: bool) -> Option<FrameLine<'_>> {
        let p = self.paragraph_at(index);
        let para = self.paragraphs.get(p)?;
        let at = para.lines.line_at(index.checked_sub(para.start)?, upstream)?;
        Some(FrameLine { paragraph: p, index: at, start: para.start, top: para.top, line: &para.lines.lines[at] })
    }

    /// The line at height `y`: the one whose box or the spacing below it
    /// holds it, the first above the text and the last below it.
    pub fn line_at_y(&self, y: f32) -> Option<FrameLine<'_>> {
        let p = self.paragraphs.partition_point(|p| p.top <= y).saturating_sub(1);
        let para = self.paragraphs.get(p)?;
        let lines = &para.lines.lines;
        let at = lines.partition_point(|l| para.top + l.top <= y).saturating_sub(1);
        let line = lines.get(at)?;
        Some(FrameLine { paragraph: p, index: at, start: para.start, top: para.top, line })
    }

    /// The character at a point, from the container's top left, and where
    /// an insertion point there goes.
    pub fn index_at(&self, x: f32, y: f32) -> Hit {
        let Some(line) = self.line_at_y(y) else {
            return Hit { index: 0, upstream: false, character: 0, fraction: 0.0 };
        };
        let hit = line.line.hit(x);
        Hit { index: hit.index + line.start, character: hit.character + line.start, ..hit }
    }

    /// The caret before the character at `index`.
    pub fn caret(&self, index: u32, upstream: bool) -> Option<Caret> {
        let line = self.line_at(index.min(self.len), upstream)?;
        let (x, secondary) = line.line.caret_x(index.min(self.len) - line.start);
        Some(Caret { x, top: line.top(), height: line.line.height, secondary })
    }

    /// The line fragment holding `index`.
    pub fn fragment(&self, index: u32) -> Option<Fragment> {
        let line = self.line_at(index.min(self.len), false)?;
        let l = line.line;
        Some(Fragment {
            range: line.range(),
            top: line.top(),
            height: l.height,
            baseline: l.baseline,
            x: l.x,
            width: l.width,
        })
    }

    /// The rectangles (x0, y0, x1, y1) that show `range` selected, line by
    /// line and left to right. Where the selection goes on past a line's
    /// end (its separator, or into the next line), it reaches the
    /// container's edge, or the widest line's end if the container has no
    /// width.
    pub fn selection_rects(&self, range: Range<u32>) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        if range.is_empty() {
            return out;
        }
        let edge = if self.container.width.is_finite() { self.container.width } else { self.used_width() };
        let first = self.paragraph_at(range.start);
        for (p, para) in self.paragraphs.iter().enumerate().skip(first) {
            if para.start >= range.end && p > first {
                break;
            }
            let (from, to) = (range.start.saturating_sub(para.start), range.end.saturating_sub(para.start));
            for line in &para.lines.lines {
                if line.range.end <= from && !(line.range.is_empty() && line.range.start == from) {
                    continue;
                }
                if line.range.start >= to {
                    break;
                }
                let (y0, y1) = (para.top + line.top, para.top + line.top + line.height);
                for (x0, x1) in line.spans(from..to) {
                    out.push([x0, y0, x1, y1]);
                }
                let end = line.content_end();
                if to > end && from <= end {
                    let (x0, x1) = if line.rtl { (0.0, line.end_edge()) } else { (line.end_edge(), edge) };
                    if x1 > x0 {
                        out.push([x0, y0, x1, y1]);
                    }
                }
            }
        }
        out
    }

    /// Lay the frame out again after an edit: `old` (UTF-16, in the text
    /// as it was) became `inserted` units of `styled.text`, the text as it
    /// is now, or its attributes changed there. Only lines the edit can
    /// have changed are laid out again. In a paragraph the edit stays in,
    /// that is from the line before the one it starts in (a shorter word
    /// can move up) until a line starts, after the edit, where an old one
    /// started, from which the old lines are kept; otherwise the
    /// paragraphs it touches are laid out whole. The paragraphs after it
    /// move without being laid out.
    ///
    /// The lines before the edit keep the bidi levels they had, which text
    /// inserted after them could in principle change (a neutral character
    /// resolves by the strong ones around it).
    pub fn edit(&mut self, styled: Styled<'_>, old: Range<u32>, inserted: u32) {
        if self.container.max_lines > 0 || self.paragraphs.is_empty() || old.end > self.len || old.start > old.end {
            *self = Frame::new(styled, self.container);
            return;
        }
        let byte_delta = styled.text.len() as isize - self.bytes as isize;
        let delta = i64::from(inserted) - i64::from(old.end - old.start);
        let p0 = self.paragraph_at(old.start);
        let p1 = self.paragraph_at(old.end);
        let rest = super::with_ctx(|ctx| {
            if p0 == p1 && self.edit_in_paragraph(ctx, styled, p0, &old, inserted, byte_delta) {
                return p0 + 1;
            }
            let (start, byte) = (self.paragraphs[p0].start, self.paragraphs[p0].byte);
            let end = match self.paragraphs.get(p1 + 1) {
                Some(next) => (next.byte as isize + byte_delta) as usize,
                None => styled.text.len(),
            };
            let above = p0.checked_sub(1).map(|p| (self.paragraphs[p].bottom(), self.paragraphs[p].lines.spacing));
            let (new, _) = self.lay_out(ctx, styled, byte, start, end, above, &mut 0);
            let count = new.len();
            self.paragraphs.splice(p0..=p1, new);
            p0 + count
        });
        self.len = (i64::from(self.len) + delta) as u32;
        self.bytes = styled.text.len();
        // What follows moves along the text by the edit's length, and down
        // the page by the change in height.
        if let (Some(above), Some(first)) =
            (rest.checked_sub(1).and_then(|p| self.paragraphs.get(p)), self.paragraphs.get(rest))
        {
            let spacing = above.lines.spacing;
            let dy = above.bottom() + spacing.line + spacing.after + first.lines.spacing.before - first.top;
            for para in &mut self.paragraphs[rest..] {
                para.start = (i64::from(para.start) + delta) as u32;
                para.byte = (para.byte as isize + byte_delta) as usize;
                para.top += dy;
            }
        }
    }

    /// Lay paragraph `p` out again after an edit inside it (see
    /// [`edit`](Self::edit)); false if the edit changed which paragraphs
    /// there are, the paragraph's style or its direction, for the caller to
    /// lay them out whole.
    fn edit_in_paragraph(
        &mut self,
        ctx: &mut Ctx,
        styled: Styled<'_>,
        p: usize,
        old: &Range<u32>,
        inserted: u32,
        byte_delta: isize,
    ) -> bool {
        let container = self.container;
        let para = &self.paragraphs[p];
        let lines = &para.lines;
        let rel = old.start - para.start;
        let Some(last) = lines.lines.last() else { return false };
        // The paragraph must still be one, ending where it did, moved; its
        // style comes from its first character.
        let bytes = lines.bytes as isize + byte_delta;
        let text = &styled.text[para.byte..];
        let extent = Extent::new(text);
        if !lines.complete || extent.bytes as isize != bytes || (extent.sep16 > 0) != (last.separator > 0) {
            return false;
        }
        // Its style and direction must be as they were, or every line
        // changes.
        let first = styled.spans.partition_point(|s| s.end <= para.start);
        let attrs = styled.spans.get(first).filter(|s| s.start <= para.start).map(|s| s.attrs as usize);
        let Some(style) = attrs.and_then(|a| styled.attrs.get(a)).map(|a| &a.paragraph) else { return false };
        if *style != lines.style || base_direction(style, extent.content) != lines.rtl {
            return false;
        }
        let delta = i64::from(inserted) - i64::from(old.end - old.start);
        let edit_end = rel + inserted;
        let r = lines.line_at(rel, false).unwrap_or(0).saturating_sub(1);
        let restart = lines.lines[r].range.start;
        let styled = Styled { text, ..styled };
        let spacing = lines.spacing;
        let mut fresh: Vec<Line> = Vec::new();
        let mut rejoin = None;
        let mut from = restart;
        let mut top = lines.lines[r].top;
        // Most edits meet the old lines again within a few: a few lines at
        // first, more each time they don't.
        let mut chunk = EDIT_CHUNK;
        'chunks: loop {
            let byte = extent.index.byte(extent.content, from);
            chunk = if extent.content.len() - byte <= MIN_WINDOW { 0 } else { chunk * 2 };
            let got = paragraph_in(ctx, styled, &extent, para.start, &container, from, chunk);
            for mut line in got.lines {
                // After the edit, a line starting where an old one did
                // (neither the paragraph's first) goes on as the old did.
                let was = i64::from(line.range.start) - delta;
                if line.range.start >= edit_end
                    && line.range.start > restart
                    && was > 0
                    && let Ok(j) = lines.lines.binary_search_by_key(&(was as u32), |l| l.range.start)
                {
                    rejoin = Some((j, top));
                    break 'chunks;
                }
                line.top = top;
                top += line.height + spacing.line;
                from = line.range.end;
                fresh.push(line);
            }
            if got.complete || fresh.is_empty() {
                break;
            }
        }
        let para = &mut self.paragraphs[p];
        let lines = Arc::make_mut(&mut para.lines);
        match rejoin {
            Some((j, top)) => {
                let dy = top - lines.lines[j].top;
                for line in &mut lines.lines[j..] {
                    let shift = |v: u32| (i64::from(v) + delta) as u32;
                    line.range = shift(line.range.start)..shift(line.range.end);
                    line.elided = line.elided.take().map(|e| shift(e.start)..shift(e.end));
                    line.top += dy;
                }
                lines.lines.splice(r..j, fresh);
            }
            None => {
                lines.lines.truncate(r);
                lines.lines.extend(fresh);
            }
        }
        lines.len = (i64::from(lines.len) + delta) as u32;
        lines.bytes = bytes as usize;
        true
    }
}

/// Whether `text` ends in a paragraph separator.
fn ends_in_separator(text: &str) -> bool {
    text.ends_with(['\n', '\r', '\u{2029}', '\u{85}'])
}

#[cfg(test)]
mod tests;

// Laid-out text crosses threads.
const _: () = {
    const fn shareable<T: Send + Sync>() {}
    shareable::<Frame>();
    shareable::<ParagraphLines>();
};
