//! Lines, clusters, carets and hit testing, with the fonts of the Linux
//! development image. Where AppKit's layout manager was measured on macOS
//! (with Helvetica, whose widths differ), the checks hold what doesn't
//! depend on the font: the order carets go in, which edges meet, where a
//! click past a line's end lands.

use std::sync::Arc;

use super::*;
use crate::text::fonts::{self, Design, Family, FontSpec};
use crate::text::layout::{Paragraph, Tab, TabKind, TextFont};

fn sans(size: f32) -> TextFont {
    let face = fonts::resolve(&FontSpec {
        family: Family::Named("DejaVu Sans".into()),
        ..FontSpec::system(Design::Default, 0.0)
    });
    assert_eq!(&*face.family, "DejaVu Sans", "DejaVu Sans is installed");
    TextFont { face, size, tabular_digits: false, features: None }
}

fn attrs() -> Attrs {
    Attrs::new(sans(12.0))
}

fn units(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

fn whole(text: &str) -> [Span; 1] {
    [Span { start: 0, end: units(text), attrs: 0 }]
}

fn frame(text: &str, attrs: &Attrs, width: f32) -> Frame {
    let spans = whole(text);
    Frame::new(
        Styled { text, attrs: std::slice::from_ref(attrs), spans: &spans },
        Container { width, ..Container::UNBOUNDED },
    )
}

fn para(text: &str, attrs: &Attrs, container: Container, from: u32) -> ParagraphLines {
    let spans = whole(text);
    lay_out_paragraph(Styled { text, attrs: std::slice::from_ref(attrs), spans: &spans }, &container, from)
}

fn wide(width: f32) -> Container {
    Container { width, ..Container::UNBOUNDED }
}

/// The line's text, for messages.
fn text_of(text: &str, range: Range<u32>) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    String::from_utf16_lossy(&units[range.start as usize..range.end as usize])
}

#[test]
fn lines_cover_the_paragraph_in_utf16() {
    let text = "Hello wörld, this 😀 wraps around\n";
    let laid = para(text, &attrs(), wide(70.0), 0);
    assert!(laid.complete && laid.lines.len() >= 3, "{}", laid.lines.len());
    assert_eq!(laid.len, units(text));
    assert_eq!(laid.bytes, text.len());
    // One after the other, the last ending with the separator.
    assert_eq!(laid.lines[0].range.start, 0);
    for pair in laid.lines.windows(2) {
        assert_eq!(pair[0].range.end, pair[1].range.start);
        assert_eq!(pair[0].separator, 0, "wrapped lines end without one");
        assert!(pair[1].top >= pair[0].top + pair[0].height);
    }
    let last = laid.lines.last().unwrap();
    assert_eq!((last.range.end, last.separator), (units(text), 1));
    for line in &laid.lines {
        // A wrapped line keeps its trailing space in its range, and its
        // width counts it.
        assert!(line.width - line.trailing_whitespace <= 70.0 + 0.01, "{}", text_of(text, line.range.clone()));
        assert!(line.baseline > 0.0 && line.baseline < line.height);
        // Clusters left to right, from the line's start, within it.
        assert!(line.clusters.windows(2).all(|w| w[1].x >= w[0].x + w[0].advance - 0.01));
        let content = line.content_end() - line.range.start;
        assert!(line.clusters.iter().all(|c| c.start < c.end && c.end <= content));
    }
    // The emoji is one cluster of two UTF-16 units.
    let emoji = units("Hello wörld, this ");
    let line = &laid.lines[laid.line_at(emoji, false).unwrap()];
    let c = line.clusters.iter().find(|c| line.range.start + c.start == emoji).expect("the emoji's cluster");
    assert_eq!(c.end - c.start, 2);
}

#[test]
fn carets_meet_where_directions_do() {
    // As AppKit's layout manager places them (measured on macOS): through
    // "abc " rightwards, through the Hebrew leftwards, through " def"
    // rightwards; where the directions meet, the caret goes to the
    // left-to-right side and the other edge is the secondary one.
    let text = "abc \u{5d0}\u{5d1}\u{5d2} def";
    let f = frame(text, &attrs(), f32::INFINITY);
    let caret = |i: u32| f.caret(i, false).unwrap();
    let x = |i: u32| caret(i).x;
    for i in 0..4 {
        assert!(x(i) < x(i + 1), "{i}");
        assert_eq!(caret(i).secondary, None, "{i}");
    }
    assert!(x(4) < x(6) && x(6) < x(5) && x(5) < x(7), "the Hebrew runs right to left");
    for i in 7..11 {
        assert!(x(i) < x(i + 1), "{i}");
    }
    // At 4, after the space: primary where the space ends, secondary at
    // the right end of the Hebrew; at 7 the other way round.
    assert_eq!(caret(4).secondary, Some(x(7)));
    assert_eq!(caret(7).secondary, Some(x(4)));
    assert_eq!(caret(5).secondary, None);

    // A right-to-left paragraph: the other way, and its end is its left
    // edge.
    let text = "\u{5d0}\u{5d1}\u{5d2} abc";
    let f = frame(text, &attrs(), 300.0);
    let caret = |i: u32| f.caret(i, false).unwrap();
    let x = |i: u32| caret(i).x;
    for i in 0..4 {
        assert!(x(i) > x(i + 1), "{i}");
    }
    assert!(x(0) > 250.0, "a right-to-left paragraph starts at the right");
    assert!(x(7) < x(5) && x(5) < x(6) && x(6) < x(4), "abc runs left to right");
    let line = f.line_at(0, false).unwrap().line;
    assert_eq!(x(7), line.x, "the end is the left end");
    assert!(caret(4).secondary.is_some() && caret(7).secondary == Some(x(4)));

    // Numbers in Hebrew in a left-to-right paragraph. AppKit's carets
    // (Helvetica 12), in index order: 0, 6.7, 13.3, 19.3, 22.7, 60.6,
    // 53.8, 37.2, 43.8, 50.5, 33.8, 28.7, 68.3, 71.7, 78.3, 85.0, 88.3.
    // Where the digits meet the Hebrew (7 and 9) the caret goes to the
    // digits' edge, which run the paragraph's way, with the space's as the
    // secondary.
    let text = "abc \u{5d0}\u{5d1} 12 \u{5d2}\u{5d3} def";
    let f = frame(text, &attrs(), f32::INFINITY);
    let caret = |i: u32| f.caret(i, false).unwrap();
    let apple = [0.0, 6.7, 13.3, 19.3, 22.7, 60.6, 53.8, 37.2, 43.8, 50.5, 33.8, 28.7, 68.3, 71.7, 78.3, 85.0, 88.3];
    let order = |xs: &[f32]| {
        let mut order: Vec<usize> = (0..xs.len()).collect();
        order.sort_by(|&a, &b| xs[a].total_cmp(&xs[b]));
        order
    };
    let ours: Vec<f32> = (0..=units(text)).map(|i| caret(i).x).collect();
    assert_eq!(order(&ours), order(&apple), "{ours:?}");
    for (at, secondary) in [(4, 12), (7, 9), (9, 7), (12, 4)] {
        assert_eq!(caret(at).secondary, Some(caret(secondary).x), "at {at}");
    }
    assert!([5, 6, 8, 10, 11].iter().all(|&i| caret(i).secondary.is_none()));
}

#[test]
fn selections_split_where_directions_mix() {
    let text = "abc \u{5d0}\u{5d1}\u{5d2} def";
    let f = frame(text, &attrs(), f32::INFINITY);
    let x = |i: u32| f.caret(i, false).unwrap().x;
    // "c אב": c and the space, then alef and bet, which sit apart from
    // them at the right of the Hebrew.
    let rects = f.selection_rects(2..6);
    assert_eq!(rects.len(), 2, "{rects:?}");
    assert!((rects[0][0] - x(2)).abs() < 0.01 && (rects[0][2] - x(4)).abs() < 0.01);
    assert!((rects[1][0] - x(6)).abs() < 0.01 && (rects[1][2] - x(7)).abs() < 0.01);
    // All of it is one stretch.
    let all = f.selection_rects(0..units(text));
    assert_eq!(all.len(), 1);
    let line = f.line_at(0, false).unwrap().line;
    assert!((all[0][0] - line.x).abs() < 0.01 && (all[0][2] - (line.x + line.width)).abs() < 0.01);
}

#[test]
fn clicks_find_the_nearer_edge() {
    let text = "Hello world this wraps around";
    let f = frame(text, &attrs(), 70.0);
    let first = f.line_at(0, false).unwrap();
    let y = first.top() + first.line.height / 2.0;
    // Every caret position of the first line is found where it is drawn.
    for i in first.range().start..first.range().end {
        let hit = f.index_at(f.caret(i, false).unwrap().x + 0.3, y);
        assert_eq!(hit.index, i, "{i}");
    }
    // Before the line: its start. Past a wrapped line's end: that end,
    // held to the line (upstream), on the character before it.
    assert_eq!(f.index_at(-10.0, y).index, 0);
    let past = f.index_at(500.0, y);
    assert_eq!((past.index, past.upstream), (first.range().end, true));
    assert_eq!((past.character, past.fraction), (first.range().end - 1, 1.0));
    let caret = f.caret(past.index, past.upstream).unwrap();
    assert_eq!(caret.top, first.top(), "upstream, the caret stays on the line");
    assert!(f.caret(past.index, false).unwrap().top > first.top(), "downstream, it starts the next");
    // Above the text, the first line; below it, the last.
    assert_eq!(f.index_at(2.0, -100.0).index, 0);
    assert_eq!(f.index_at(500.0, 1e4).index, units(text));

    // Past the end of a paragraph's line: before its separator.
    let f = frame("ab  \ncd", &attrs(), 200.0);
    let hit = f.index_at(150.0, 3.0);
    assert_eq!((hit.index, hit.upstream), (4, false));
    let second = f.line_at(5, false).unwrap();
    assert_eq!(f.index_at(150.0, second.top() + 3.0).index, 7);
}

#[test]
fn clicks_between_paragraphs() {
    // Measured on macOS with NSLayoutManager ("aaa\nbbb\nccc", Helvetica
    // 12, spacing before 20, after 10, line spacing 5): line fragments at
    // 0..29, 29..78 and 78..112, text at 0, 49 and 98. A click in the
    // spacing before a paragraph lands in it; in the line spacing or the
    // spacing after one, in the paragraph above.
    let mut a = attrs();
    a.paragraph.paragraph_spacing = 10.0;
    a.paragraph.paragraph_spacing_before = 20.0;
    a.paragraph.line_spacing = 5.0;
    let f = frame("aaa\nbbb\nccc", &a, 300.0);
    let (first, second) = (&f.paragraphs[0], &f.paragraphs[1]);
    assert_eq!(second.top, first.bottom() + 5.0 + 10.0 + 20.0);
    for (y, character) in [
        (first.bottom() - 1.0, 0),
        (first.bottom() + 1.0, 0),
        (first.bottom() + 14.0, 0),
        (second.top - 19.0, 4),
        (second.top - 1.0, 4),
        (second.top + 1.0, 4),
    ] {
        assert_eq!(f.index_at(1.0, y).character, character, "y {y}");
    }
}

#[test]
fn clicks_in_right_to_left_text() {
    let text = "\u{5d0}\u{5d1}\u{5d2}";
    let f = frame(text, &attrs(), 200.0);
    let line = f.line_at(0, false).unwrap().line;
    let right = line.x + line.width;
    // The right end is the start; the left end, the end.
    assert_eq!(f.index_at(right - 0.5, 5.0).index, 0);
    assert_eq!(f.index_at(line.x + 0.5, 5.0).index, 3);
    assert_eq!(f.index_at(right + 50.0, 5.0).index, 0);
    assert_eq!(f.index_at(0.0, 5.0).index, 3);
    // The fraction runs in the character's direction.
    let alef = &line.clusters[line.clusters.len() - 1];
    let hit = f.index_at(alef.x + alef.advance * 0.25, 5.0);
    assert_eq!(hit.character, 0);
    assert!((hit.fraction - 0.75).abs() < 0.01, "{}", hit.fraction);
}

#[test]
fn tab_clusters_reach_the_stop() {
    let text = "a\tb";
    let f = frame(text, &attrs(), f32::INFINITY);
    let line = f.line_at(0, false).unwrap().line;
    let tab = line.clusters.iter().find(|c| c.start == 1).expect("the tab's cluster");
    assert!((tab.x + tab.advance - 28.0).abs() < 0.01, "the tab reaches the first stop ({})", tab.x + tab.advance);
    let b = line.clusters.iter().find(|c| c.start == 2).unwrap();
    assert!((b.x - 28.0).abs() < 0.01);
    // A click in the tab's room lands before or after it.
    assert_eq!(f.index_at(8.0, 5.0).index, 1);
    assert_eq!(f.index_at(26.0, 5.0).index, 2);
}

#[test]
fn a_limit_truncates_the_last_line() {
    let text = "The quick brown fox jumps over the lazy dog and keeps on running";
    let container =
        Container { width: 100.0, max_lines: 2, truncation: Some(LineBreak::TruncateTail), ..Container::UNBOUNDED };
    let laid = para(text, &attrs(), container, 0);
    assert_eq!(laid.lines.len(), 2);
    let last = &laid.lines[1];
    let elided = last.elided.clone().expect("an ellipsis");
    assert_eq!(elided.end, units(text), "it stands for the rest");
    assert_eq!(last.range.end, units(text));
    // Its cluster covers what it stands for.
    let dots = last.clusters.last().unwrap();
    assert_eq!((last.range.start + dots.start, last.range.start + dots.end), (elided.start, elided.end));
    assert!(last.width <= 100.0 + 0.01);
    // Without truncation, the lines stop, and the rest can follow.
    let two = para(text, &attrs(), Container { truncation: None, ..container }, 0);
    assert!(!two.complete && two.lines[1].elided.is_none());
    let rest = para(text, &attrs(), wide(100.0), two.lines[1].range.end);
    let all = para(text, &attrs(), wide(100.0), 0);
    assert_eq!(rest.lines.len() + 2, all.lines.len());
}

/// The parts of a line that must come out the same however it was laid
/// out.
fn same_line(a: &Line, b: &Line, what: &str) {
    assert_eq!(a.range, b.range, "{what}");
    assert_eq!((a.separator, a.height, a.baseline), (b.separator, b.height, b.baseline), "{what}");
    assert!((a.x - b.x).abs() < 0.01 && (a.width - b.width).abs() < 0.01, "{what}");
    assert_eq!(a.clusters.len(), b.clusters.len(), "{what}");
    for (p, q) in a.clusters.iter().zip(b.clusters.iter()) {
        assert_eq!((p.start, p.end, p.level), (q.start, q.end, q.level), "{what}");
        assert!((p.x - q.x).abs() < 0.01 && (p.advance - q.advance).abs() < 0.01, "{what}");
    }
    let glyphs = |l: &Line| l.runs.iter().flat_map(|r| r.glyphs.iter().map(|g| g.id)).collect::<Vec<_>>();
    assert_eq!(glyphs(a), glyphs(b), "{what}");
}

/// Words of prose lengths, the same every run.
fn long_paragraph(words: usize) -> String {
    let list =
        ["lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing", "elit", "sed", "do", "a", "tempor"];
    let mut seed = words as u64 | 1;
    (0..words)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            list[(seed % list.len() as u64) as usize]
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn restarting_at_a_line_gives_the_same_lines() {
    let mut a = attrs();
    a.paragraph.head_indent = 10.0;
    a.paragraph.first_line_head_indent = 30.0;
    a.paragraph.line_spacing = 2.0;
    let text = format!("{}\u{2028}{}\n", long_paragraph(60), long_paragraph(40));
    let all = para(&text, &a, wide(150.0), 0);
    assert!(all.lines.len() > 10);
    for k in [1, 3, all.lines.len() / 2, all.lines.len() - 1] {
        let from = para(&text, &a, wide(150.0), all.lines[k].range.start);
        assert_eq!(from.lines.len(), all.lines.len() - k, "from line {k}");
        for (i, (x, y)) in from.lines.iter().zip(&all.lines[k..]).enumerate() {
            same_line(x, y, &format!("line {} from {k}", k + i));
            assert!((x.top + all.lines[k].top - y.top).abs() < 0.01, "tops count from the first laid out");
        }
    }
    // The line separator's line ends with it; the line after it takes the
    // head indent, not the first line's.
    let sep =
        all.lines.iter().position(|l| l.separator == 1 && l.range.end < all.len).expect("a line ending in U+2028");
    assert!((all.lines[sep + 1].x - 10.0).abs() < 0.01 && (all.lines[0].x - 30.0).abs() < 0.01);
}

/// Lays `text` out from the start of each line `starting` picks, and
/// checks the lines are the ones laying it all out gives; how many.
fn restarts_match(text: &str, a: &Attrs, width: f32, starting: impl Fn(&str) -> bool) -> usize {
    let all = para(text, a, wide(width), 0);
    let mut checked = 0;
    for (k, line) in all.lines.iter().enumerate().skip(1) {
        if !starting(&text_of(text, line.range.clone())) {
            continue;
        }
        checked += 1;
        let from = para(text, a, wide(width), line.range.start);
        assert_eq!(from.lines.len(), all.lines.len() - k);
        for (i, (x, y)) in from.lines.iter().zip(&all.lines[k..]).enumerate() {
            same_line(x, y, &format!("width {width}, line {} from {k}", k + i));
            assert_eq!(x.context, y.context);
        }
    }
    checked
}

#[test]
fn restarting_keeps_the_bidi_context() {
    // Numbers, neutrals and brackets at a line's start resolve by the
    // strong text before it, on the line before.
    let digit = |l: &str| l.starts_with(|c: char| c.is_ascii_digit());
    let widths = [70.0, 90.0, 110.0, 130.0];
    let count = |text: &str, starting: &dyn Fn(&str) -> bool| {
        widths.iter().map(|&w| restarts_match(text, &attrs(), w, starting)).sum::<usize>()
    };
    // Numbers after Hebrew in a left-to-right paragraph: level 2, drawn
    // right to left with the Hebrew around them.
    let hebrew: String = (0..40).map(|i| format!("\u{5e9}\u{5dc}\u{5d5}\u{5dd} {} ", 100 + i)).collect();
    assert!(count(&format!("abc {hebrew}"), &digit) > 3);
    // After Arabic, European digits become Arabic ones, and a plus sign
    // between them stays a neutral.
    let arabic: String = (0..40).map(|i| format!("\u{639}\u{631}\u{628}\u{64a} {}+{} ", i, 100 + i)).collect();
    assert!(count(&format!("abc {arabic}"), &digit) > 3);
    // A right-to-left paragraph quoting English in brackets, lines
    // starting at the bracket: after English, the pair goes with it.
    let quoted: String = (0..30).map(|i| format!("\u{5e9}\u{5dc}\u{5d5}\u{5dd} abc (hello world {i}) ")).collect();
    assert!(count(&quoted, &|l: &str| l.starts_with('(')) > 3);
}

#[test]
fn a_long_paragraph_is_shaped_only_as_far_as_needed() {
    // Laid out in windows, the first lines of a long paragraph are the
    // ones laying it all out gives.
    let text = long_paragraph(3000);
    assert!(text.len() > 4 * MIN_WINDOW);
    let all = para(&text, &attrs(), wide(200.0), 0);
    let limited = Container { max_lines: 12, ..wide(200.0) };
    let first = para(&text, &attrs(), limited, 0);
    assert!(!first.complete);
    assert_eq!(first.lines.len(), 12);
    for (i, (a, b)) in first.lines.iter().zip(&all.lines).enumerate() {
        same_line(a, b, &format!("line {i}"));
    }
    // And from the middle.
    let k = all.lines.len() / 2;
    let middle = para(&text, &attrs(), limited, all.lines[k].range.start);
    for (i, (a, b)) in middle.lines.iter().zip(&all.lines[k..]).enumerate() {
        same_line(a, b, &format!("line {}", k + i));
    }
}

#[test]
fn paragraphs_past_64_kib_keep_their_places() {
    // parley keeps a cluster's place in its shaping run in 16 bits; runs
    // are cut well before that, in one attribute run or in many that
    // differ only in color (which parley would shape as one).
    let text = long_paragraph(12_000);
    assert!(text.len() > 70_000);
    let n = units(&text);
    let a = [attrs(), Attrs { color: [1.0, 0.0, 0.0, 1.0], ..attrs() }];
    let mut colored = Vec::new();
    for (i, words) in text.split_inclusive(' ').collect::<Vec<_>>().chunks(40).enumerate() {
        let start = colored.last().map_or(0, |s: &Span| s.end);
        colored.push(Span { start, end: start + words.iter().map(|w| units(w)).sum::<u32>(), attrs: i as u32 % 2 });
    }
    for (spans, width) in
        [(&whole(&text)[..], 300.0), (&whole(&text)[..], f32::INFINITY), (&colored[..], f32::INFINITY)]
    {
        let f = Frame::new(Styled { text: &text, attrs: &a, spans }, wide(width));
        let ranges: Vec<Range<u32>> = f.lines().map(|l| l.range()).collect();
        assert_eq!((ranges[0].start, ranges.last().unwrap().end), (0, n), "width {width}");
        assert!(ranges.windows(2).all(|w| w[0].end == w[1].start && w[0].start < w[0].end), "width {width}");
        for line in f.lines() {
            let covered: u32 = line.line.clusters.iter().map(|c| c.end - c.start).sum();
            assert_eq!(covered, line.line.content_end() - line.line.range.start);
        }
        for i in [n / 2, n - 5, n] {
            assert!(f.caret(i, false).is_some() && f.fragment(i).is_some(), "width {width}: {i}");
        }
        let last = f.lines().last().unwrap();
        assert_eq!(f.index_at(1e9, last.top() + 1.0).index, n);
    }
}

#[test]
fn empty_text_and_trailing_separators_have_a_line() {
    let f = frame("", &attrs(), 100.0);
    assert_eq!(f.paragraphs.len(), 1);
    assert!(f.height() > 0.0);
    assert_eq!(f.caret(0, false).map(|c| c.top), Some(0.0));
    let f = frame("Hi\n", &attrs(), 100.0);
    assert_eq!(f.paragraphs.len(), 2, "the line after the separator");
    let caret = f.caret(3, false).unwrap();
    assert!(caret.top > 0.0 && caret.x == 0.0);
    assert_eq!(f.index_at(50.0, caret.top + 2.0).index, 3);
    assert!((f.height() - 2.0 * f.paragraphs[0].lines.height()).abs() < 0.01);
    // Right-to-left and centered empty lines put the caret where text would
    // start.
    let mut a = attrs();
    a.paragraph.alignment = Align::Center;
    let f = frame("x\n", &a, 100.0);
    assert_eq!(f.caret(2, false).unwrap().x, 50.0);
}

#[test]
fn fragments_and_selections_span_lines() {
    let text = "Hello world this wraps\n\nand more";
    let f = frame(text, &attrs(), 70.0);
    let frag = f.fragment(0).unwrap();
    assert_eq!(frag.range.start, 0);
    assert!(frag.width > 0.0 && frag.height > 0.0 && frag.baseline < frag.height);
    // A selection across a wrap and an empty line: each line's text, and
    // on to the container's edge where it goes on past a line.
    let rects = f.selection_rects(3..units(text) - 2);
    let lines: Vec<f32> = rects.iter().map(|r| r[1]).collect();
    assert!(lines.windows(2).all(|w| w[1] >= w[0]), "top to bottom");
    assert!(rects.iter().filter(|r| r[2] == 70.0).count() >= 2, "{rects:?}");
    let empty = f.line_at(units("Hello world this wraps\n"), false).unwrap();
    assert!(
        rects.iter().any(|r| r[1] == empty.top() && r[0] == 0.0 && r[2] == 70.0),
        "the empty line is selected across"
    );
}

/// Checks an edited frame against one laid out afresh.
fn same_frame(f: &Frame, fresh: &Frame, what: &str) {
    assert_eq!(f.len(), fresh.len(), "{what}");
    assert!((f.used_width() - fresh.used_width()).abs() < 0.01, "{what}");
    assert_eq!(f.paragraphs.len(), fresh.paragraphs.len(), "{what}");
    for (p, (x, y)) in f.paragraphs.iter().zip(&fresh.paragraphs).enumerate() {
        assert_eq!((x.start, x.byte), (y.start, y.byte), "{what}, paragraph {p}");
        assert!((x.top - y.top).abs() < 0.01, "{what}, paragraph {p}: {} vs {}", x.top, y.top);
        assert_eq!((x.lines.len, x.lines.bytes), (y.lines.len, y.lines.bytes), "{what}, paragraph {p}");
        assert_eq!(x.lines.lines.len(), y.lines.lines.len(), "{what}, paragraph {p}");
        for (i, (l, m)) in x.lines.lines.iter().zip(&y.lines.lines).enumerate() {
            same_line(l, m, &format!("{what}, paragraph {p}, line {i}"));
            assert!((l.top - m.top).abs() < 0.01 && l.context == m.context, "{what}, paragraph {p}, line {i}");
        }
    }
}

#[test]
fn edits_lay_out_what_they_touch() {
    // After each edit, the frame must be the one laying the new text out
    // from scratch gives. A narrow width makes more lines, and more words
    // too long for one.
    random_edits(0x9e37_79b9_7f4a_7c15, 180.0, 160);
    random_edits(0x2468_1357_aaaa_5555, 60.0, 120);
}

/// `steps` edits of kinds chosen by `seed`, each checked against a fresh
/// layout, in a container `width` wide.
fn random_edits(seed: u64, width: f32, steps: usize) {
    let mut a = attrs();
    a.paragraph.paragraph_spacing = 3.0;
    let b = Attrs { color: [1.0, 0.0, 0.0, 1.0], underline: layout::Decoration { style: 1, color: None }, ..a.clone() };
    let attrs = [a, b];
    let numbers: String = (0..30).map(|i| format!("\u{5e9}\u{5dc}\u{5d5}\u{5dd} {} (x) ", 100 + i)).collect();
    let mut text =
        format!("{}\n\n{}\nshort\n{numbers}\n{}", long_paragraph(80), long_paragraph(30), long_paragraph(700));
    let mut rng = seed;
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n.max(1) as u64) as usize
    };
    let spans_of = |text: &str| {
        // Two attributes alternating every 37 units, so that edits cross
        // runs.
        let len = units(text);
        (0..len.div_ceil(37)).map(|i| Span { start: i * 37, end: ((i + 1) * 37).min(len), attrs: i % 2 }).collect()
    };
    let container = wide(width);
    let spans: Vec<Span> = spans_of(&text);
    let mut f = Frame::new(Styled { text: &text, attrs: &attrs, spans: &spans }, container);
    for step in 0..steps {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let at = next(chars.len() + 1);
        let byte = chars.get(at).map_or(text.len(), |c| c.0);
        let (old_len, insert) = match next(10) {
            0 => (0, "x".to_string()),
            // Right to left, which can turn a paragraph around.
            6 => (next(2), "\u{5d0}\u{5d1} ".to_string()),
            1 => (0, " word ".to_string()),
            2 => (0, "\n".to_string()),
            3 => (next(8), String::new()),
            4 => (next(3), "é😀".to_string()),
            // Numbers and brackets, which resolve by the text around them.
            7 => (next(2), " 12 (3) ".to_string()),
            // "\r" and "\r\n", which can meet a "\n" or a "\r".
            8 => (0, "\r".to_string()),
            9 => (next(2), "\r\n".to_string()),
            _ => (next(4), "replacement".to_string()),
        };
        let end = chars.get(at + old_len).map_or(text.len(), |c| c.0);
        let start16 = units(&text[..byte]);
        let old = start16..start16 + units(&text[byte..end]);
        text.replace_range(byte..end, &insert);
        let spans: Vec<Span> = spans_of(&text);
        let styled = Styled { text: &text, attrs: &attrs, spans: &spans };
        f.edit(styled, old, units(&insert));
        same_frame(&f, &Frame::new(styled, container), &format!("width {width}, step {step}"));
    }
}

#[test]
fn edits_that_make_a_cr_lf() {
    // A lone "\r" ends a paragraph; a "\n" after it makes one "\r\n" of
    // the two, typed there or brought there by a deletion.
    let a = [attrs()];
    let container = wide(120.0);
    let edit = |f: &mut Frame, text: &mut String, byte: usize, end: usize, insert: &str| {
        let start = units(&text[..byte]);
        let old = start..start + units(&text[byte..end]);
        text.replace_range(byte..end, insert);
        let spans = whole(text);
        let styled = Styled { text, attrs: &a, spans: &spans };
        f.edit(styled, old, units(insert));
        same_frame(f, &Frame::new(styled, container), &format!("{text:?}"));
    };
    for (text, range, insert) in [
        ("a\rX\nb", 2..3, ""),
        ("a\rXY\nb", 2..4, ""),
        ("ab\rcd", 3..3, "\n"),
        ("ab\r", 3..3, "\n"),
        ("a\r\nb", 2..2, "x"),
        // Paragraphs made whole in a text ending in a separator, whose
        // empty last paragraph is kept.
        ("ab\ncd\n", 4..4, "\n"),
        ("ab\r\rcd\n", 2..4, "é"),
    ] {
        let mut text = text.to_string();
        let spans = whole(&text);
        let mut f = Frame::new(Styled { text: &text, attrs: &a, spans: &spans }, container);
        edit(&mut f, &mut text, range.start, range.end, insert);
    }
    // And at random, among many of them.
    let mut text = String::from("é\rx\nab\r\ncd 😀 ef\rgh\n");
    let spans = whole(&text);
    let mut f = Frame::new(Styled { text: &text, attrs: &a, spans: &spans }, container);
    let mut rng = 0x1234_5678_9abc_def1_u64;
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n.max(1) as u64) as usize
    };
    for _ in 0..300 {
        let chars: Vec<usize> = text.char_indices().map(|c| c.0).chain([text.len()]).collect();
        let at = next(chars.len());
        let (len, insert) =
            [(0, "\r"), (0, "\n"), (next(3), "é😀"), (next(3), ""), (next(2), "\r\n"), (0, "w ")][next(6)];
        let end = chars[(at + len).min(chars.len() - 1)];
        edit(&mut f, &mut text, chars[at], end, insert);
    }
}

#[test]
fn edits_that_shorten_a_word_too_long_for_a_line() {
    // A word too long for a line starts on a line of its own, after a
    // line holding only a space, and is cut where the width ends. Return
    // on its second line leaves a word that fits after the space: the
    // first line changes, though the edit is two lines further on.
    let text = format!(" {} tail", "m".repeat(60));
    let a = attrs();
    let mut found = 0;
    for width in (60..=200).step_by(3) {
        let container = wide(width as f32);
        let before = frame(&text, &a, container.width);
        let lines: Vec<&Line> = before.lines().map(|l| l.line).collect();
        if lines.len() < 3 || lines[0].range != (0..1) || !lines[1].forced {
            continue;
        }
        found += 1;
        let at = lines[2].range.start;
        let new = format!("{}\n{}", &text[..at as usize], &text[at as usize..]);
        let spans = whole(&new);
        let styled = Styled { text: &new, attrs: std::slice::from_ref(&a), spans: &spans };
        let mut f = before.clone();
        f.edit(styled, at..at, 1);
        same_frame(&f, &Frame::new(styled, container), &format!("width {width}"));
    }
    assert!(found > 0);
}

#[test]
fn edits_between_brackets_far_apart() {
    // Brackets resolve together, by what they hold and what comes before
    // them: in a right-to-left paragraph, a pair after English holding
    // English runs left to right with it, until a Hebrew letter goes in.
    // Both brackets, lines away from the edit, change.
    let text = format!("\u{5e9}\u{5dc}\u{5d5}\u{5dd} abc ({}) \u{5e9}\u{5dc}\u{5d5}\u{5dd}", "hello world ".repeat(30));
    let container = wide(120.0);
    let before = frame(&text, &attrs(), container.width);
    assert!(before.lines().count() > 10);
    let at = units(&text) / 2;
    let at = at + text_of(&text, at..units(&text)).find(' ').unwrap() as u32 + 1;
    let mut new: Vec<u16> = text.encode_utf16().collect();
    new.splice(at as usize..at as usize, "\u{5d0} ".encode_utf16());
    let new = String::from_utf16(&new).unwrap();
    let (a, spans) = (attrs(), whole(&new));
    let styled = Styled { text: &new, attrs: std::slice::from_ref(&a), spans: &spans };
    let mut f = before.clone();
    f.edit(styled, at..at, 2);
    same_frame(&f, &Frame::new(styled, container), "a Hebrew letter in brackets");
}

#[test]
fn edits_that_change_what_numbers_follow() {
    // Replacing the Latin word before a line that starts with a number
    // by a Hebrew one takes the number from level 0 to 2. The line after
    // the edit starts where it did, but mustn't be kept as it was.
    let mut a = attrs();
    a.paragraph.direction = Direction::LeftToRight;
    let text: String = (0..30).map(|i| format!("\u{5e9}\u{5dc}\u{5d5}\u{5dd} abc {} ", 100 + i)).collect();
    let mut found = 0;
    for width in (60..=200).step_by(7) {
        let container = wide(width as f32);
        let before = frame(&text, &a, container.width);
        let lines: Vec<Range<u32>> = before.lines().map(|l| l.range()).collect();
        for pair in lines.windows(2) {
            let starts_with_digit = text_of(&text, pair[1].clone()).starts_with(|c: char| c.is_ascii_digit());
            if !starts_with_digit || !text_of(&text, pair[0].clone()).ends_with("abc ") {
                continue;
            }
            found += 1;
            let at = pair[1].start - 4;
            let mut new: Vec<u16> = text.encode_utf16().collect();
            new.splice(at as usize..at as usize + 3, "\u{5d0}\u{5d1}\u{5d2}".encode_utf16());
            let new = String::from_utf16(&new).unwrap();
            let spans = whole(&new);
            let styled = Styled { text: &new, attrs: std::slice::from_ref(&a), spans: &spans };
            let mut f = before.clone();
            f.edit(styled, at..at + 3, 3);
            same_frame(&f, &Frame::new(styled, container), &format!("width {width}, at {at}"));
        }
    }
    assert!(found > 0);
}

#[test]
fn an_edit_in_a_long_paragraph_lays_out_a_few_lines() {
    // Typing in the middle of a long paragraph: the frame after the edit
    // shares the untouched paragraphs' lines, and keeps the old lines past
    // the edit once they line up again.
    let text = format!("before\n{}\nafter", long_paragraph(4000));
    let a = [attrs()];
    let spans = whole(&text);
    let container = wide(300.0);
    let mut f = Frame::new(Styled { text: &text, attrs: &a, spans: &spans }, container);
    let before = f.paragraphs[0].lines.clone();
    let long = f.paragraphs[1].lines.clone();
    let middle = f.paragraphs[1].lines.lines.len() / 2;
    let at = f.paragraphs[1].start + f.paragraphs[1].lines.lines[middle].range.start + 3;
    let byte = at as usize; // ASCII
    let new = format!("{}xyz{}", &text[..byte], &text[byte..]);
    let spans = whole(&new);
    let started = std::time::Instant::now();
    f.edit(Styled { text: &new, attrs: &a, spans: &spans }, at..at, 3);
    let took = started.elapsed();
    assert!(Arc::ptr_eq(&before, &f.paragraphs[0].lines), "paragraphs before the edit are kept");
    // Lines far from the edit, before it and after it, are the old ones
    // (moved), not laid out again: they share their glyphs.
    let glyphs = |l: &Line| l.runs[0].glyphs.clone();
    let now = &f.paragraphs[1].lines.lines;
    assert_eq!(now.len(), long.lines.len());
    assert!(Arc::ptr_eq(&glyphs(&now[3]), &glyphs(&long.lines[3])));
    let last = now.len() - 1;
    assert!(Arc::ptr_eq(&glyphs(&now[last]), &glyphs(&long.lines[last])));
    assert_eq!(now[last].range.start, long.lines[last].range.start + 3);
    assert!(!Arc::ptr_eq(&glyphs(&now[middle]), &glyphs(&long.lines[middle])), "the edited line is new");
    let fresh = Frame::new(Styled { text: &new, attrs: &a, spans: &spans }, container);
    same_frame(&f, &fresh, "after the edit");
    eprintln!("an edit in a {}-byte paragraph: {took:?}", new.len());

    // Return there makes two paragraphs of it, which keep the old lines
    // far from it; deleting it joins them again, as they were.
    let split = format!("{}\n{}", &new[..byte], &new[byte..]);
    let split_spans = whole(&split);
    f.edit(Styled { text: &split, attrs: &a, spans: &split_spans }, at..at, 1);
    same_frame(&f, &Frame::new(Styled { text: &split, attrs: &a, spans: &split_spans }, container), "split");
    let (head, tail) = (&f.paragraphs[1].lines.lines, &f.paragraphs[2].lines.lines);
    assert!(Arc::ptr_eq(&glyphs(&head[3]), &glyphs(&long.lines[3])));
    assert!(Arc::ptr_eq(&glyphs(tail.last().unwrap()), &glyphs(&long.lines[last])));
    f.edit(Styled { text: &new, attrs: &a, spans: &spans }, at..at + 1, 0);
    same_frame(&f, &fresh, "joined");
    let now = &f.paragraphs[1].lines.lines;
    assert!(Arc::ptr_eq(&glyphs(&now[3]), &glyphs(&long.lines[3])));
    assert!(Arc::ptr_eq(&glyphs(&now[last]), &glyphs(&long.lines[last])));
}

#[test]
fn frames_cross_threads() {
    let text = "laid out on another thread, drawn on this one";
    let a = attrs();
    let there = {
        let a = a.clone();
        std::thread::spawn(move || {
            let spans = whole(text);
            Arc::new(Frame::new(Styled { text, attrs: std::slice::from_ref(&a), spans: &spans }, wide(120.0)))
        })
        .join()
        .unwrap()
    };
    let here = frame(text, &a, 120.0);
    for (x, y) in there.lines().zip(here.lines()) {
        same_line(x.line, y.line, "across threads");
    }
}

#[test]
fn explicit_tabs_and_intervals_follow_the_list() {
    // As on macOS: a tab goes to the first stop in the list beyond it,
    // however the list is ordered, and past the last stop every
    // defaultTabInterval from it.
    let mut a = attrs();
    let tab = |location, kind| Tab { location, kind };
    a.paragraph = Paragraph {
        tabs: Some([tab(45.0, TabKind::Left), tab(15.0, TabKind::Left)].into()),
        default_tab_interval: 30.0,
        ..Paragraph::default()
    };
    let starts = |text: &str| {
        let f = frame(text, &a, f32::INFINITY);
        let line = f.line_at(0, false).unwrap().line;
        line.clusters.iter().map(|c| (c.start, c.x)).collect::<Vec<_>>()
    };
    let x = starts("a\tx");
    assert_eq!(x.last().map(|c| c.1), Some(45.0), "{x:?}");
    let x = starts("abcdefghijklm\tx");
    let last = x.last().unwrap().1;
    assert!([75.0, 105.0].contains(&last), "{last}");
}

#[test]
fn paragraphs_end_at_every_separator() {
    assert_eq!(paragraph_end("ab\ncd"), (2, 1, 1));
    assert_eq!(paragraph_end("ab\r\ncd"), (2, 2, 2));
    assert_eq!(paragraph_end("ab\rcd"), (2, 1, 1));
    assert_eq!(paragraph_end("ab\u{2029}cd"), (2, 3, 1));
    // U+2028 and NEL separate lines, not paragraphs (NEL as AppKit takes
    // it); other characters sharing the separators' lead bytes don't end
    // one.
    assert_eq!(paragraph_end("a\u{2028}b\u{85}—é\u{a0}c"), ("a\u{2028}b\u{85}—é\u{a0}c".len(), 0, 0));
    let f = frame("one\r\ntwo\u{2029}three", &attrs(), 200.0);
    let starts: Vec<u32> = f.paragraphs.iter().map(|p| p.start).collect();
    assert_eq!(starts, [0, 5, 9]);
    assert_eq!(f.paragraphs[0].lines.lines[0].separator, 2);
    let f = frame("a\u{85}b\u{2028}c\u{85}", &attrs(), 200.0);
    assert_eq!(f.paragraphs.len(), 1);
    let lines: Vec<_> = f.lines().map(|l| (l.range(), l.line.separator)).collect();
    assert_eq!(lines, [(0..2, 1), (2..4, 1), (4..6, 1), (6..6, 0)]);
}

#[test]
fn the_first_strong_character_outside_isolates_decides() {
    use layout::{first_strong, last_strong};
    assert_eq!(first_strong("123 abc"), Some(Strong::Left));
    assert_eq!(first_strong("123 \u{5d0}"), Some(Strong::Right));
    assert_eq!(first_strong("123 !"), None);
    // Isolated text doesn't count: an isolated English word before Hebrew.
    assert_eq!(first_strong("\u{2066}abc\u{2069} \u{5d0}"), Some(Strong::Right));
    assert_eq!(first_strong("\u{2067}\u{5d0}\u{2069} abc"), Some(Strong::Left));
    assert_eq!(first_strong("\u{627}"), Some(Strong::Arabic), "Arabic letters are strong right to left");
    // And looking back, as numbers and neutrals do.
    assert_eq!(last_strong("abc \u{5d0} 123 (!"), Some(Strong::Right));
    assert_eq!(last_strong("\u{627}1 abc 2"), Some(Strong::Left));
    assert_eq!(last_strong("\u{5d0} \u{2066}abc\u{2069} 1"), Some(Strong::Right));
    assert_eq!(last_strong("1, 2"), None);
    assert_eq!(last_strong("abc \u{2067}\u{5d0}"), Some(Strong::Right));
    assert_eq!(last_strong("abc \u{2067} "), None, "inside an isolate");
    assert!(layout::all_neutral(" (—) ") && !layout::all_neutral(" 1 ") && !layout::all_neutral("\u{5d0}"));
}

#[test]
fn utf16_offsets_map_both_ways() {
    let text = "aé😀b";
    let index = Utf16::new(text);
    let pairs: Vec<(usize, u32)> = text.char_indices().map(|(b, _)| (b, index.utf16(b))).collect();
    assert_eq!(pairs, [(0, 0), (1, 1), (3, 2), (7, 4)]);
    assert_eq!(index.utf16(text.len()), 5);
    for (b, u) in pairs {
        assert_eq!(index.byte(text, u), b);
    }
    // Between the halves of a surrogate pair: the character's start.
    assert_eq!(index.byte(text, 3), 3);
    // ASCII text needs no table.
    assert!(Utf16::new("plain").ends.is_empty());
}
