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

#[test]
fn edits_lay_out_what_they_touch() {
    // After each edit, the frame must be the one laying the new text out
    // from scratch gives.
    let mut a = attrs();
    a.paragraph.paragraph_spacing = 3.0;
    let b = Attrs { color: [1.0, 0.0, 0.0, 1.0], underline: layout::Decoration { style: 1, color: None }, ..a.clone() };
    let attrs = [a, b];
    let mut text = format!("{}\n\n{}\nshort\n{}", long_paragraph(80), long_paragraph(30), long_paragraph(700));
    let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
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
    let container = wide(180.0);
    let spans: Vec<Span> = spans_of(&text);
    let mut f = Frame::new(Styled { text: &text, attrs: &attrs, spans: &spans }, container);
    for step in 0..160 {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let at = next(chars.len() + 1);
        let byte = chars.get(at).map_or(text.len(), |c| c.0);
        let (old_len, insert) = match next(7) {
            0 => (0, "x".to_string()),
            // Right to left, which can turn a paragraph around.
            6 => (next(2), "\u{5d0}\u{5d1} ".to_string()),
            1 => (0, " word ".to_string()),
            2 => (0, "\n".to_string()),
            3 => (next(8), String::new()),
            4 => (next(3), "é😀".to_string()),
            _ => (next(4), "replacement".to_string()),
        };
        let end = chars.get(at + old_len).map_or(text.len(), |c| c.0);
        let start16 = units(&text[..byte]);
        let old = start16..start16 + units(&text[byte..end]);
        text.replace_range(byte..end, &insert);
        let spans: Vec<Span> = spans_of(&text);
        let styled = Styled { text: &text, attrs: &attrs, spans: &spans };
        f.edit(styled, old, units(&insert));
        let fresh = Frame::new(styled, container);
        assert_eq!(f.len(), fresh.len(), "step {step}");
        assert_eq!(f.paragraphs.len(), fresh.paragraphs.len(), "step {step}");
        for (p, (x, y)) in f.paragraphs.iter().zip(&fresh.paragraphs).enumerate() {
            assert_eq!((x.start, x.byte), (y.start, y.byte), "step {step}, paragraph {p}");
            assert!((x.top - y.top).abs() < 0.01, "step {step}, paragraph {p}: {} vs {}", x.top, y.top);
            assert_eq!((x.lines.len, x.lines.bytes), (y.lines.len, y.lines.bytes), "step {step}, paragraph {p}");
            assert_eq!(x.lines.lines.len(), y.lines.lines.len(), "step {step}, paragraph {p}");
            for (i, (l, m)) in x.lines.lines.iter().zip(&y.lines.lines).enumerate() {
                same_line(l, m, &format!("step {step}, paragraph {p}, line {i}"));
                assert!((l.top - m.top).abs() < 0.01, "step {step}, paragraph {p}, line {i}");
            }
        }
    }
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
    for (x, y) in f.paragraphs[1].lines.lines.iter().zip(&fresh.paragraphs[1].lines.lines) {
        same_line(x, y, "after the edit");
    }
    assert_eq!(f.paragraphs[2].start, fresh.paragraphs[2].start);
    eprintln!("an edit in a {}-byte paragraph: {took:?}", new.len());
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
    assert_eq!(paragraph_end("ab\u{85}cd"), (2, 2, 1));
    // U+2028 separates lines, not paragraphs; other characters sharing
    // the separators' lead bytes don't end one.
    assert_eq!(paragraph_end("a\u{2028}b—é\u{a0}c"), ("a\u{2028}b—é\u{a0}c".len(), 0, 0));
    let f = frame("one\r\ntwo\u{2029}three", &attrs(), 200.0);
    let starts: Vec<u32> = f.paragraphs.iter().map(|p| p.start).collect();
    assert_eq!(starts, [0, 5, 9]);
    assert_eq!(f.paragraphs[0].lines.lines[0].separator, 2);
}

#[test]
fn the_first_strong_character_outside_isolates_decides() {
    assert_eq!(first_strong("123 abc"), Some(false));
    assert_eq!(first_strong("123 \u{5d0}"), Some(true));
    assert_eq!(first_strong("123 !"), None);
    // Isolated text doesn't count: an isolated English word before Hebrew.
    assert_eq!(first_strong("\u{2066}abc\u{2069} \u{5d0}"), Some(true));
    assert_eq!(first_strong("\u{2067}\u{5d0}\u{2069} abc"), Some(false));
    assert_eq!(first_strong("\u{627}"), Some(true), "Arabic letters are strong right to left");
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
