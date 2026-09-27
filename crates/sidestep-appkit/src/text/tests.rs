//! Shaping, fallback, bidi and layout checks that depend on the fonts a
//! Linux system has (the development image has DejaVu and Noto, CJK and
//! color emoji included). Checks whose fonts are missing are skipped.

use std::sync::Arc;

use super::fonts::{self, Design, Family, FontSpec};
use super::layout::{
    self, Align, Attrs, Decoration, Direction, LineBreak, Options, PlacedRun, Run, TextFont, TextLayout,
};

fn font(family: &str, size: f32) -> Option<TextFont> {
    let exists = super::with_ctx(|ctx| ctx.fcx.collection.family_id(family).is_some());
    exists.then(|| TextFont {
        face: fonts::resolve(&FontSpec {
            family: Family::Named(family.into()),
            ..FontSpec::system(Design::Default, 0.0)
        }),
        size,
        tabular_digits: false,
        features: None,
    })
}

fn sans(size: f32) -> TextFont {
    font("DejaVu Sans", size).expect("DejaVu Sans is installed")
}

fn has_family(part: &str) -> bool {
    super::with_ctx(|ctx| ctx.fcx.collection.family_names().any(|n| n.contains(part)))
}

fn lay(text: &str, attrs: Attrs, opts: Options) -> Arc<TextLayout> {
    layout::lay_out(text, &[attrs], &[Run { start: 0, end: text.len(), attrs: 0 }], &opts)
}

fn width(text: &str, attrs: Attrs) -> f32 {
    lay(text, attrs, Options::UNBOUNDED).width
}

fn glyphs(laid: &TextLayout) -> Vec<u32> {
    laid.runs.iter().flat_map(|r| r.glyphs.iter().map(|g| g.id)).collect()
}

fn in_width(width: f32) -> Options {
    Options { width, ..Options::UNBOUNDED }
}

#[test]
fn kerning_pulls_pairs_together() {
    let a = Attrs::new(sans(24.0));
    let pair = width("AV", a.clone());
    let apart = width("A", a.clone()) + width("V", a.clone());
    assert!(pair < apart - 0.5, "{pair} vs {apart}");
    let unkerned = Attrs { kern: Some(0.0), ..a.clone() };
    assert!((width("AV", unkerned) - apart).abs() < 0.01, "NSKern 0 turns kerning off");
    let tracked = Attrs { kern: Some(2.0), ..a };
    assert!((width("AV", tracked) - pair - 4.0).abs() < 0.01, "NSKern adds after each character");
}

#[test]
fn ligatures_follow_the_attribute() {
    let Some(serif) = font("DejaVu Serif", 24.0).or_else(|| font("Noto Serif", 24.0)) else { return };
    let with = glyphs(&lay("office", Attrs::new(serif.clone()), Options::UNBOUNDED)).len();
    let without = glyphs(&lay("office", Attrs { ligatures: 0, ..Attrs::new(serif) }, Options::UNBOUNDED)).len();
    assert_eq!(without, 6);
    assert!(with < without, "the ffi ligature makes one glyph of three ({with})");
}

#[test]
fn font_features_reach_the_shaper() {
    let Some(serif) = font("DejaVu Serif", 24.0).or_else(|| font("Noto Serif", 24.0)) else { return };
    let plain = glyphs(&lay("office", Attrs::new(serif.clone()), Options::UNBOUNDED)).len();
    let off = TextFont { features: Some([(*b"liga", 0)].into()), ..serif };
    assert!(plain < 6);
    assert_eq!(glyphs(&lay("office", Attrs::new(off), Options::UNBOUNDED)).len(), 6);
}

#[test]
fn fallback_finds_cjk_and_emoji() {
    let a = Attrs::new(sans(20.0));
    if has_family("CJK") {
        let laid = lay("漢字かな", a.clone(), Options::UNBOUNDED);
        assert!(glyphs(&laid).iter().all(|&g| g != 0), "no missing glyphs");
        let latin = lay("abc", a.clone(), Options::UNBOUNDED);
        assert_ne!(laid.runs[0].font, latin.runs[0].font, "another face fills in");
    }
    if has_family("Emoji") {
        let laid = lay("a😀b", a, Options::UNBOUNDED);
        assert!(glyphs(&laid).iter().all(|&g| g != 0));
        assert_eq!(laid.runs.len(), 3, "the emoji comes from its own face");
    }
}

#[test]
fn right_to_left_runs_are_reversed() {
    let a = Attrs::new(sans(20.0));
    // Hebrew letters shaped alone, to recognize them in a line.
    let alef = glyphs(&lay("א", a.clone(), Options::UNBOUNDED))[0];
    let gimel = glyphs(&lay("ג", a.clone(), Options::UNBOUNDED))[0];
    let laid = lay("abc אבג def", a.clone(), Options::UNBOUNDED);
    let order = glyphs(&laid);
    let (first, last) =
        (order.iter().position(|&g| g == alef).unwrap(), order.iter().position(|&g| g == gimel).unwrap());
    assert!(last < first, "alef, first in memory, is drawn right of gimel");
    let xs: Vec<f32> = laid.runs.iter().map(|r| r.x).collect();
    assert!(xs.windows(2).all(|w| w[0] <= w[1]), "runs come in visual order");

    // A right-to-left paragraph lines up on the right by default.
    let laid = lay("שלום", a.clone(), in_width(200.0));
    assert!(laid.runs[0].x > 100.0);
    // Unless its style says left to right.
    let ltr = Attrs { paragraph: layout::Paragraph { direction: Direction::LeftToRight, ..a.paragraph.clone() }, ..a };
    assert!(lay("שלום", ltr, in_width(200.0)).runs[0].x < 1.0);
}

#[test]
fn arabic_letters_join() {
    let Some(arabic) = font("Noto Sans Arabic", 20.0) else { return };
    let a = Attrs::new(arabic);
    let alone = glyphs(&lay("س", a.clone(), Options::UNBOUNDED))[0];
    let word = glyphs(&lay("سسس", a, Options::UNBOUNDED));
    assert_eq!(word.len(), 3);
    assert!(word.iter().all(|&g| g != alone), "initial, medial and final forms");
}

fn ellipsis(a: &Attrs) -> u32 {
    glyphs(&lay("…", a.clone(), Options::UNBOUNDED))[0]
}

fn truncated(mode: LineBreak, width: f32) -> (Arc<TextLayout>, u32) {
    let mut a = Attrs::new(sans(13.0));
    a.paragraph.line_break = mode;
    let laid = lay("The quick brown fox jumps over the lazy dog", a.clone(), in_width(width));
    (laid, ellipsis(&a))
}

#[test]
fn truncation_puts_the_ellipsis_where_asked() {
    let (tail, dots) = truncated(LineBreak::TruncateTail, 120.0);
    assert!(tail.width <= 120.0);
    assert_eq!(*glyphs(&tail).last().unwrap(), dots);
    let (head, dots) = truncated(LineBreak::TruncateHead, 120.0);
    assert_eq!(glyphs(&head)[0], dots);
    assert!(head.width <= 120.0);
    let (middle, dots) = truncated(LineBreak::TruncateMiddle, 120.0);
    let ids = glyphs(&middle);
    let at = ids.iter().position(|&g| g == dots).expect("an ellipsis");
    assert!(at > 2 && at < ids.len() - 3, "in the middle");
    // Text that fits is left alone.
    let (whole, dots) = truncated(LineBreak::TruncateTail, 1000.0);
    assert!(!glyphs(&whole).contains(&dots));
}

#[test]
fn alignment_places_lines_in_the_width() {
    let mut a = Attrs::new(sans(13.0));
    assert!(lay("Hello", a.clone(), in_width(200.0)).runs[0].x.abs() < 0.01);
    let text_width = width("Hello", a.clone());
    for (align, x) in
        [(Align::Right, 200.0 - text_width), (Align::Center, (200.0 - text_width) / 2.0), (Align::Left, 0.0)]
    {
        a.paragraph.alignment = align;
        let laid = lay("Hello", a.clone(), in_width(200.0));
        assert!((laid.runs[0].x - x).abs() < 0.5, "{align:?}: {} vs {x}", laid.runs[0].x);
    }
    // Justified lines, all but the last, fill the width.
    a.paragraph.alignment = Align::Justified;
    let laid = lay("aaa bbb ccc ddd eee fff ggg hhh iii", a, in_width(100.0));
    let first_line: Vec<&PlacedRun> = laid.runs.iter().filter(|r| r.y == laid.runs[0].y).collect();
    let end = first_line.iter().map(|r| r.x + r.glyphs.last().map_or(0.0, |g| g.x)).fold(0.0, f32::max);
    assert!(end > 85.0, "the first line is spread out ({end})");
}

#[test]
fn decorations_and_backgrounds() {
    let mut a = Attrs::new(sans(20.0));
    a.underline = Decoration { style: 1, color: Some([1.0, 0.0, 0.0, 1.0]) };
    a.strikethrough = Decoration { style: 2, color: None };
    a.background = Some([0.0, 0.0, 1.0, 1.0]);
    let laid = lay("Hello", a.clone(), Options::UNBOUNDED);
    let baseline = laid.runs[0].y;
    let background: Vec<_> = laid.fills.iter().filter(|f| f.background).collect();
    assert_eq!(background.len(), 1);
    assert_eq!((background[0].rect[1], background[0].rect[3]), (0.0, laid.height), "the whole line's height");
    let lines: Vec<_> = laid.fills.iter().filter(|f| !f.background).collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].rect[1] > baseline && lines[0].color == [1.0, 0.0, 0.0, 1.0], "underline below, in its color");
    assert!(lines[1].rect[3] < baseline && lines[1].color == a.color, "strikethrough above, in the text's color");
    assert!(lines[1].rect[3] - lines[1].rect[1] > lines[0].rect[3] - lines[0].rect[1], "thick is thicker");
}

#[test]
fn baseline_offset_raises_text() {
    let a = Attrs::new(sans(20.0));
    let plain = lay("x", a.clone(), Options::UNBOUNDED);
    let raised = lay("x", Attrs { baseline_offset: 5.0, ..a }, Options::UNBOUNDED);
    assert!(raised.height > plain.height, "the line grows to fit");
    let bottom = |l: &TextLayout| l.height - l.runs[0].y;
    assert_eq!(bottom(&raised), bottom(&plain) + 5.0);
}

#[test]
fn layouts_are_cached_and_faces_registered_once() {
    let a = Attrs::new(sans(13.0));
    let first = lay("cached", a.clone(), Options::UNBOUNDED);
    let second = lay("cached", a.clone(), Options::UNBOUNDED);
    assert!(Arc::ptr_eq(&first, &second));
    let other = lay("cached", a, in_width(10.0));
    assert!(!Arc::ptr_eq(&first, &other));
    assert_eq!(first.runs[0].font, lay("other text", Attrs::new(sans(13.0)), Options::UNBOUNDED).runs[0].font);
}

#[test]
fn slants_and_strokes_come_in_steps() {
    // An app animating NSObliqueness or NSStrokeWidth gets a face per step
    // too fine to see, not one per value.
    let font = sans(13.0).face.font.clone().expect("a font file");
    let ids = |synth: &dyn Fn(f32) -> fonts::Synth| {
        let mut ids: Vec<u32> = (0..1000).map(|i| fonts::register(&font, &[], synth(i as f32 / 1000.0))).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    assert!(ids(&|t| fonts::Synth { skew: 10.0 + t * 0.2, ..Default::default() }) <= 3);
    assert!(ids(&|t| fonts::Synth { stroke: 0.03 + t * 0.002, ..Default::default() }) <= 3);
    let face = |synth| fonts::face_data(fonts::register(&font, &[], synth)).unwrap();
    // A hairline stays a stroke; what isn't a number, or is too slanted to
    // draw, is held to what is.
    assert_eq!(face(fonts::Synth { stroke: 1e-6, ..Default::default() }).stroke, 0.001);
    assert_eq!(face(fonts::Synth { skew: f32::NAN, ..Default::default() }).skew, 0.0);
    assert_eq!(face(fonts::Synth { skew: 1e9, ..Default::default() }).skew, 89.0);
}

#[test]
fn runs_past_64_kib_keep_every_glyph() {
    // parley keeps a cluster's place in its shaping run in 16 bits: past
    // 64 KiB of one run, a cluster would seem to be the direction mark in
    // front and be dropped. Runs are cut well before that.
    let text = "ab cd ".repeat(12_000);
    let mut a = Attrs::new(sans(12.0));
    a.paragraph.direction = Direction::LeftToRight;
    assert_eq!(glyphs(&lay(&text, a, Options::UNBOUNDED)).len(), text.len());
}

#[test]
fn dotted_lines_far_from_the_origin_end() {
    // Far out, a float's steps outgrow a dot's period: the pattern is
    // counted in whole periods, so drawing it always ends.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut a = Attrs::new(sans(12.0));
        a.underline = Decoration { style: 1 | 0x100, color: None };
        a.paragraph.first_line_head_indent = 4.0e7;
        let _ = tx.send(lay("ab", a, Options::UNBOUNDED).fills.len());
    });
    // (That far out, points are 4 apart in single precision, too coarse to
    // draw dots in: what matters is that the count is bounded.)
    let fills = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("laid out");
    assert!(fills < 100, "{fills}");
}

#[test]
fn names_resolve_like_apple_ones() {
    let named = |n: &str| fonts::spec_named(n, 12.0);
    let bold = named("DejaVuSans-Bold").expect("a PostScript name");
    assert_eq!((bold.family.clone(), bold.weight), (Family::Named("DejaVu Sans".into()), 700.0));
    let oblique = named("DejaVu Sans Mono Bold Oblique").expect("a full name");
    assert!(oblique.italic && oblique.weight == 700.0);
    assert_eq!(named("Menlo-Regular").map(|s| s.family), Some(Family::System(Design::Monospaced)));
    assert_eq!(named("Helvetica-Bold").map(|s| s.weight), Some(700.0));
    assert!(named("Nonexistent Family").is_none());
    assert!(fonts::resolve(&named("Menlo").unwrap()).fixed_pitch);
}

#[test]
fn weights_map_through_the_named_ones() {
    for (ns, css) in
        [(-0.8, 100.0), (-0.4, 300.0), (0.0, 400.0), (0.23, 500.0), (0.3, 600.0), (0.4, 700.0), (0.62, 900.0)]
    {
        assert!((fonts::css_weight(ns) - css).abs() < 0.01, "{ns}");
    }
    let bold = fonts::resolve(&FontSpec { weight: 700.0, ..FontSpec::system(Design::Default, 13.0) });
    assert!(bold.postscript_name.contains("Bold"), "{}", bold.postscript_name);
}

#[test]
fn tabs_reach_the_next_stop() {
    let a = Attrs::new(sans(13.0));
    let laid = lay("a\tb\tc", a.clone(), Options::UNBOUNDED);
    assert!(glyphs(&laid).iter().all(|&g| g != 0), "no missing-glyph box for a tab");
    let xs: Vec<f32> = laid.runs.iter().flat_map(|r| r.glyphs.iter().map(move |g| r.x + g.x)).collect();
    // Tabs draw nothing; b is at the first stop, c at the second.
    assert_eq!(xs.len(), 3);
    assert_eq!((xs[1], xs[2]), (28.0, 56.0));
    let past = lay(
        "\t\t\t\t\t\t\t\t\t\t\t\t\tx",
        Attrs { paragraph: layout::Paragraph { default_tab_interval: 50.0, ..a.paragraph.clone() }, ..a },
        Options::UNBOUNDED,
    );
    let x = past.runs.last().map(|r| r.x + r.glyphs.last().unwrap().x).unwrap();
    assert_eq!(x, 386.0, "past the twelve stops, every defaultTabInterval from the last (as on macOS)");
}

#[test]
fn explicit_tab_stops() {
    let a = Attrs::new(sans(13.0));
    let with = |tabs: &[layout::Tab], interval: f64| Attrs {
        paragraph: layout::Paragraph { tabs: Some(tabs.into()), default_tab_interval: interval, ..a.paragraph.clone() },
        ..a.clone()
    };
    let (w_a, w_bbb) = (width("a", a.clone()), width("bbb", a.clone()));
    let tab = |location, kind| layout::Tab { location, kind };
    assert!((width("a\tb", with(&[], 0.0)) - width("ab", a.clone())).abs() < 0.01, "no stop: no room");
    assert!((width("a\t\tb", with(&[], 40.0)) - (80.0 + width("b", a.clone()))).abs() < 0.01);
    assert!((width("a\tbbb", with(&[tab(100.0, layout::TabKind::Right)], 0.0)) - 100.0).abs() < 0.01);
    let centered = width("a\tbbb", with(&[tab(100.0, layout::TabKind::Center)], 0.0));
    assert!((centered - (100.0 + w_bbb / 2.0)).abs() < 0.01);
    let left = width("a\tbbb", with(&[tab(100.0, layout::TabKind::Left)], 0.0));
    assert!((left - (100.0 + w_bbb)).abs() < 0.01 && w_a > 0.0);
    // A decimal tab centers the decimal point on the stop, as on macOS.
    let decimal = with(&[tab(100.0, layout::TabKind::Decimal)], 0.0);
    let laid = lay("a\t12.5", decimal.clone(), Options::UNBOUNDED);
    let dot = laid.runs.iter().flat_map(|r| r.glyphs.iter().map(move |g| r.x + g.x)).nth(3).unwrap();
    let point = width(".", a.clone());
    assert!((dot + point / 2.0 - 100.0).abs() < 0.01, "{dot}");
    assert!((width("a\t125", decimal) - 100.0).abs() < 0.01, "without a point, the end");
    // A tab alone makes a line of the font's height.
    assert_eq!(lay("\t", a.clone(), Options::UNBOUNDED).height, lay("x", a, Options::UNBOUNDED).height);
}

fn with_height(height: f32, truncate_last: bool) -> Options {
    Options { width: 200.0, height, truncate_last, ..Options::UNBOUNDED }
}

#[test]
fn the_first_line_is_kept_however_short_the_height() {
    let a = Attrs::new(sans(12.0));
    let line = lay("Hello", a.clone(), Options::UNBOUNDED);
    let laid = lay("Hello\nWorld", a.clone(), with_height(5.0, false));
    assert_eq!((laid.width, laid.height), (line.width, line.height));
    assert_eq!(glyphs(&laid), glyphs(&line));
    // Truncated, it shows that more text follows.
    let laid = lay("Hello\nWorld", a.clone(), with_height(5.0, true));
    assert_eq!((laid.width, laid.height), (width("Hello…", a.clone()), line.height));
    // One line on a baseline takes no notice of a height at all (the
    // caller passes an infinite one), and a later line needs to fit whole.
    let two = lay("Hello\nWorld", a.clone(), with_height(line.height * 2.0, false));
    assert_eq!(two.height, line.height * 2.0);
    let cut = lay("Hello\nWorld", a, with_height(line.height * 2.0 - 0.5, false));
    assert_eq!(cut.height, line.height);
}

#[test]
fn a_cut_off_paragraph_leaves_an_ellipsis() {
    let a = Attrs::new(sans(12.0));
    let dots = ellipsis(&a);
    let line = lay("Hi", a.clone(), Options::UNBOUNDED).height;
    let last = |text: &str| {
        let laid = lay(text, a.clone(), with_height(line * 1.5, true));
        assert_eq!(laid.height, line, "{text:?}");
        glyphs(&laid).last().copied()
    };
    // Whenever anything follows the last line that fits, if only an empty
    // line, as AppKit does.
    for text in ["Hi\nWorld", "Hi\n\n", "Hi\n ", "Hi\u{2028}World", "Hi\n\nWorld"] {
        assert_eq!(last(text), Some(dots), "{text:?}");
    }
    // A separator that ends the text hides nothing.
    for text in ["Hi\n", "Hi\u{2028}"] {
        assert_ne!(last(text), Some(dots), "{text:?}");
    }
    // The width is that of the text with its ellipsis.
    let laid = lay("Hi\nWorld", a.clone(), with_height(line * 1.5, true));
    assert!((laid.width - width("Hi…", a.clone())).abs() < 0.01);
    // Without truncation, the last line is left as it is.
    assert!(!glyphs(&lay("Hi\nWorld", a, with_height(line * 1.5, false))).contains(&dots));
}

#[test]
fn a_joiner_before_a_control_character_lays_out() {
    // A joiner used to take the control character after it into its emoji
    // sequence, which then overlapped the character's own run, and parley
    // panicked.
    let a = Attrs::new(sans(13.0));
    for text in [
        "😀\u{200D}\t",
        "😀\u{200D}\0",
        "😀\u{200D}\u{1b}[0m",
        "a👨\u{200D}\u{1b}[0m",
        "😀\u{200D}\u{7f}x",
        "\u{200D}\t",
    ] {
        for mode in [LineBreak::WordWrap, LineBreak::TruncateTail] {
            let mut attrs = a.clone();
            attrs.paragraph.line_break = mode;
            let laid = lay(text, attrs, in_width(30.0));
            assert!(laid.height > 0.0, "{text:?}");
        }
    }
}

/// A small generator, so that the cases are the same every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next() % items.len() as u64) as usize]
    }
}

#[test]
fn text_of_every_kind_lays_out_without_panicking() {
    const PIECES: &[&str] = &[
        "a",
        "Hello",
        " ",
        "  ",
        "\t",
        "\n",
        "\r\n",
        "\u{2028}",
        "\u{2029}",
        "\0",
        "\u{1b}",
        "\u{7f}",
        "\u{200D}",
        "\u{FE0F}",
        "\u{20E3}",
        "1",
        "#",
        "😀",
        "👨‍👩‍👧",
        "🇫🇷",
        "🏳️‍🌈",
        "👍🏽",
        "1️⃣",
        "漢字",
        "かな",
        "שלום",
        "مرحبا",
        "\u{200E}",
        "\u{200F}",
        "…",
        "office",
        "AV",
        "x\u{301}",
        "\u{E0067}",
        "ﷺ",
        "𝕏",
    ];
    let modes = [
        LineBreak::WordWrap,
        LineBreak::CharWrap,
        LineBreak::Clip,
        LineBreak::TruncateHead,
        LineBreak::TruncateTail,
        LineBreak::TruncateMiddle,
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let base = Attrs::new(sans(13.0));
    let other = Attrs { kern: Some(1.0), baseline_offset: 2.0, ..Attrs::new(sans(20.0)) };
    for case in 0..1500 {
        let text: String = (0..rng.next() % 12).map(|_| *rng.pick(PIECES)).collect();
        let mut attrs = [base.clone(), other.clone()];
        for a in &mut attrs {
            a.paragraph.line_break = *rng.pick(&modes);
            a.paragraph.head_indent = (rng.next() % 3) as f64 * 7.0;
            a.paragraph.tail_indent = -((rng.next() % 2) as f64) * 9.0;
            a.paragraph.direction = *rng.pick(&[Direction::Natural, Direction::LeftToRight, Direction::RightToLeft]);
        }
        // Two runs, cut at a character boundary.
        let cut = text.char_indices().map(|(i, _)| i).nth((rng.next() % 4) as usize).unwrap_or(text.len());
        let runs = if cut == 0 || cut == text.len() {
            vec![Run { start: 0, end: text.len(), attrs: 0 }]
        } else {
            vec![Run { start: 0, end: cut, attrs: 0 }, Run { start: cut, end: text.len(), attrs: 1 }]
        };
        let opts = Options {
            width: *rng.pick(&[f32::INFINITY, 40.0, 7.0, 120.0]),
            height: *rng.pick(&[f32::INFINITY, 5.0, 30.0]),
            all_lines: !rng.next().is_multiple_of(4),
            font_leading: rng.next().is_multiple_of(2),
            truncate_last: rng.next().is_multiple_of(2),
            // Not drawn from `rng`, which would change every case.
            attachments_as_glyphs: case % 2 == 0,
        };
        let laid = layout::lay_out(&text, &attrs, &runs, &opts);
        assert!(laid.width.is_finite() && laid.height.is_finite() && laid.height > 0.0, "case {case}: {text:?}");
    }
}

#[test]
fn many_runs_lay_out_as_one() {
    // Words alternating between two attributes over many paragraphs, as an
    // attributed string with syntax colors has them.
    let a = Attrs::new(sans(13.0));
    let b = Attrs { color: [1.0, 0.0, 0.0, 1.0], ..a.clone() };
    let text: String = (0..200).map(|i| format!("line {i} has a few words\n")).collect();
    let mut runs = Vec::new();
    for (i, (at, word)) in text
        .split_inclusive(' ')
        .scan(0, |at, w| {
            let start = *at;
            *at += w.len();
            Some((start, w))
        })
        .enumerate()
    {
        runs.push(Run { start: at, end: at + word.len(), attrs: (i % 2) as u32 });
    }
    let laid = layout::lay_out(&text, &[a.clone(), b], &runs, &Options::UNBOUNDED);
    let plain = layout::lay_out(&text, &[a], &[Run { start: 0, end: text.len(), attrs: 0 }], &Options::UNBOUNDED);
    assert_eq!((laid.width, laid.height), (plain.width, plain.height));
    assert_eq!(glyphs(&laid), glyphs(&plain));
    assert!(laid.runs.iter().any(|r| r.color == [1.0, 0.0, 0.0, 1.0]));
}

#[test]
fn text_laid_out_twice_is_cached_once() {
    let a = Attrs::new(sans(13.0));
    let text = "cached once, however often it is laid out";
    let runs = [Run { start: 0, end: text.len(), attrs: 0 }];
    let jobs = (0..3).map(|_| {
        let hash = layout::cached(text, std::slice::from_ref(&a), &runs, &Options::UNBOUNDED).unwrap_err();
        layout::job(text.into(), vec![a.clone()], runs.to_vec(), Options::UNBOUNDED, hash)
    });
    let before = super::with_ctx(|ctx| ctx.layouts.len());
    let laid = layout::lay_out_all(jobs.collect());
    assert_eq!(super::with_ctx(|ctx| ctx.layouts.len()), before + 1);
    assert!(Arc::ptr_eq(&laid[0], &laid[1]) && Arc::ptr_eq(&laid[1], &laid[2]));
}

#[test]
fn decorations_run_straight_through_fallback_fonts() {
    // Hebrew and an emoji come from other faces than the text's; the
    // underline keeps the text's font's place and thickness through them.
    let mut a = Attrs::new(sans(20.0));
    a.underline = Decoration { style: 1, color: None };
    let laid = lay("Hello שלום 😀 end", a, Options::UNBOUNDED);
    let mut lines: Vec<[f32; 4]> = laid.fills.iter().filter(|f| !f.background).map(|f| f.rect).collect();
    assert!(lines.len() >= 3, "a piece per run");
    assert!(lines.iter().all(|r| (r[1], r[3]) == (lines[0][1], lines[0][3])), "{lines:?}");
    lines.sort_by(|a, b| a[0].total_cmp(&b[0]));
    assert!(lines.windows(2).all(|w| (w[1][0] - w[0][2]).abs() < 0.01), "end to end: {lines:?}");
}

/// "a", an attachment's box `width` × 30 reaching 5 below the baseline,
/// and "b", laid out in `opts`.
fn with_box(width: f32, opts: Options) -> (Arc<TextLayout>, Attrs) {
    let a = Attrs::new(sans(13.0));
    let boxed = Attrs { attachment: Some(layout::Attachment { width, height: 30.0, y: -5.0 }), ..a.clone() };
    let text = "a\u{FFFC}b";
    let runs =
        [Run { start: 0, end: 1, attrs: 0 }, Run { start: 1, end: 4, attrs: 1 }, Run { start: 4, end: 5, attrs: 0 }];
    (layout::lay_out(text, &[a.clone(), boxed], &runs, &opts), a)
}

#[test]
fn attachments_take_their_boxes_room() {
    let (laid, a) = with_box(40.0, Options::UNBOUNDED);
    let (wa, wb) = (width("a", a.clone()), width("b", a.clone()));
    assert_eq!(laid.attachments.len(), 1);
    let placed = laid.attachments[0];
    assert_eq!((placed.byte, placed.attrs), (1, 1), "the U+FFFC's byte and its attributes");
    let [x0, y0, x1, y1] = placed.rect;
    assert!((x0 - wa).abs() < 0.01 && (x1 - x0 - 40.0).abs() < 0.01, "{:?}", placed.rect);
    assert!((y1 - y0 - 30.0).abs() < 0.01, "{:?}", placed.rect);
    assert!((laid.width - (wa + 40.0 + wb)).abs() < 0.05, "the character itself takes no room: {}", laid.width);
    // Its top and bottom are the line's: 25 above the baseline, 5 below.
    let plain = lay("ab", a, Options::UNBOUNDED);
    assert!(laid.height > plain.height && laid.height >= 30.0, "{} vs {}", laid.height, plain.height);
    assert!(y0 >= 0.0 && y1 <= laid.height, "{:?} in {}", placed.rect, laid.height);
    assert!(laid.first_descent >= 5.0, "{}", laid.first_descent);
    assert!((laid.height - laid.first_descent + 5.0 - y1).abs() < 0.01, "the box's bottom 5 below the baseline");
}

#[test]
fn attachments_too_wide_for_the_line_start_the_next() {
    let (one, a) = with_box(40.0, Options::UNBOUNDED);
    let wa = width("a", a.clone());
    let (laid, _) = with_box(40.0, in_width(wa + 20.0));
    let [x0, y0, ..] = laid.attachments[0].rect;
    assert!(x0.abs() < 0.01, "at the start of the second line: {:?}", laid.attachments[0].rect);
    assert!(y0 > 0.0 && laid.height > one.height, "below the first line");

    // A box wider than the line still goes on one, alone.
    let (laid, _) = with_box(100.0, in_width(50.0));
    assert_eq!(laid.attachments.len(), 1);
    let [x0, _, x1, _] = laid.attachments[0].rect;
    assert!(x0.abs() < 0.01 && (x1 - 100.0).abs() < 0.01, "{:?}", laid.attachments[0].rect);
}

#[test]
fn baseline_offsets_move_attachments() {
    let a = Attrs::new(sans(13.0));
    let text = "a\u{FFFC}b";
    let runs =
        [Run { start: 0, end: 1, attrs: 0 }, Run { start: 1, end: 4, attrs: 1 }, Run { start: 4, end: 5, attrs: 0 }];
    for glyphs in [false, true] {
        let opts = Options { attachments_as_glyphs: glyphs, ..Options::UNBOUNDED };
        let with_offset = |offset: f32| {
            let attachment = Some(layout::Attachment { width: 20.0, height: 30.0, y: 0.0 });
            let boxed = Attrs { attachment, baseline_offset: offset, ..a.clone() };
            layout::lay_out(text, &[a.clone(), boxed], &runs, &opts)
        };
        let (flat, raised) = (with_offset(0.0), with_offset(6.0));
        let above = |l: &TextLayout| l.height - l.first_descent - l.attachments[0].rect[3];
        // Drawn 6 points higher over the baseline either way.
        assert!((above(&raised) - above(&flat) - 6.0).abs() < 0.01, "TextKit 1: {glyphs}");
        // The line makes room for the box as it is in string drawing (it
        // overhangs the top), and as it's drawn in TextKit 1.
        let grown = raised.height - flat.height;
        let want = if glyphs { 6.0 } else { 0.0 };
        assert!((grown - want).abs() < 0.01, "TextKit 1: {glyphs}: grew {grown}");
    }
}
