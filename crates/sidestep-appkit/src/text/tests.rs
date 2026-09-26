//! Shaping, fallback, bidi and layout checks that depend on the fonts a
//! Linux system has (the development image has DejaVu and Noto, CJK and
//! color emoji included). Checks whose fonts are missing are skipped.

use std::rc::Rc;

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

fn lay(text: &str, attrs: Attrs, opts: Options) -> Rc<TextLayout> {
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
    let ltr = Attrs { paragraph: layout::Paragraph { direction: Direction::LeftToRight, ..a.paragraph }, ..a };
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

fn truncated(mode: LineBreak, width: f32) -> (Rc<TextLayout>, u32) {
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
    assert!(Rc::ptr_eq(&first, &second));
    let other = lay("cached", a, in_width(10.0));
    assert!(!Rc::ptr_eq(&first, &other));
    assert_eq!(first.runs[0].font, lay("other text", Attrs::new(sans(13.0)), Options::UNBOUNDED).runs[0].font);
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
        Attrs { paragraph: layout::Paragraph { default_tab_interval: 50.0, ..a.paragraph }, ..a },
        Options::UNBOUNDED,
    );
    let x = past.runs.last().map(|r| r.x + r.glyphs.last().unwrap().x).unwrap();
    assert_eq!(x, 350.0, "past the twelve stops, every defaultTabInterval");
}

#[test]
fn explicit_tab_stops() {
    let a = Attrs::new(sans(13.0));
    let with = |tabs: &[layout::Tab], interval: f32| Attrs {
        paragraph: layout::Paragraph { tabs: layout::intern_tabs(tabs), default_tab_interval: interval, ..a.paragraph },
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
    // A decimal tab puts the decimal point on the stop.
    let decimal = with(&[tab(100.0, layout::TabKind::Decimal)], 0.0);
    let laid = lay("a\t12.5", decimal.clone(), Options::UNBOUNDED);
    let dot = laid.runs.iter().flat_map(|r| r.glyphs.iter().map(move |g| r.x + g.x)).nth(3).unwrap();
    assert!((dot - 100.0).abs() < 0.01, "{dot}");
    assert!((width("a\t125", decimal) - 100.0).abs() < 0.01, "without a point, the end");
    // A tab alone makes a line of the font's height.
    assert_eq!(lay("\t", a.clone(), Options::UNBOUNDED).height, lay("x", a, Options::UNBOUNDED).height);
}
