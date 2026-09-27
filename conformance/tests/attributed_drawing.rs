//! Measuring and drawing attributed strings (`size`, `drawAtPoint:`,
//! `drawInRect:`, `drawWithRect:options:context:`,
//! `boundingRectWithSize:options:context:`) and `NSStringDrawingContext`,
//! on macOS and on Linux alike. Sizes are checked against the same text
//! measured as an `NSString` with the same attributes, and relations
//! between them, never as numbers, so they hold whatever fonts a system has
//! (DejaVu alone on CI's Ubuntu); drawing is checked by its pixels in a
//! bitmap.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use common::*;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAttributedStringNSExtendedStringDrawing, NSAttributedStringNSStringDrawing, NSBackgroundColorAttributeName,
    NSColor, NSFont, NSFontAttributeName, NSFontDescriptorSymbolicTraits, NSForegroundColorAttributeName,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSStringDrawing, NSStringDrawingContext,
    NSStringDrawingOptions, NSStringNSExtendedStringDrawing, NSTextAlignment,
};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSMutableAttributedString, NSRange, NSRect, NSSize, NSString,
};

use sidestep as _;

type Attributes = Retained<NSDictionary<NSString, AnyObject>>;

fn helvetica(size: f64) -> Retained<NSFont> {
    NSFont::userFontOfSize(size).expect("the user font")
}

fn bold(size: f64) -> Retained<NSFont> {
    let d =
        helvetica(size).fontDescriptor().fontDescriptorWithSymbolicTraits(NSFontDescriptorSymbolicTraits::TraitBold);
    NSFont::fontWithDescriptor_size(&d, size).expect("a bold font")
}

fn font_attrs(font: &NSFont) -> Attributes {
    // SAFETY: a constant key.
    NSDictionary::from_slices(&[unsafe { NSFontAttributeName }], &[font as &AnyObject])
}

fn string_size(text: &str, attrs: Option<&Attributes>) -> NSSize {
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSString::from_str(text).sizeWithAttributes(attrs.map(|a| &**a)) }
}

fn attributed(text: &str, attrs: &Attributes) -> Retained<NSAttributedString> {
    // SAFETY: the dictionary holds valid attributes.
    unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(text), attrs) }
}

fn lines() -> NSStringDrawingOptions {
    NSStringDrawingOptions::UsesLineFragmentOrigin
}

/// Text with no attributes is measured in Helvetica 12, as an `NSString`
/// with no attributes is.
fn default_attributes(_: MainThreadMarker) {
    let text = "Hello, world";
    let plain = NSAttributedString::from_nsstring(&NSString::from_str(text));
    assert_eq!(plain.size(), string_size(text, None));
    assert_eq!(plain.size(), string_size(text, Some(&font_attrs(&helvetica(12.0)))));
    let empty = NSAttributedString::from_nsstring(&NSString::from_str("")).size();
    assert_eq!((empty.width, empty.height), (0.0, string_size("x", None).height), "an empty string is a line tall");
}

/// One run of attributes measures as an `NSString` does with them, with
/// any options.
fn one_run_matches_string_drawing(_: MainThreadMarker) {
    let attrs = font_attrs(&helvetica(14.0));
    let text = "The quick brown fox jumps over the lazy dog\nand runs away";
    let a = attributed(text, &attrs);
    assert_eq!(a.size(), string_size(text, Some(&attrs)));
    let truncate = lines() | NSStringDrawingOptions::TruncatesLastVisibleLine;
    let leading = lines() | NSStringDrawingOptions::UsesFontLeading;
    for (w, h) in [(0.0, 0.0), (120.0, 0.0), (120.0, 30.0), (400.0, 0.0)] {
        for options in [NSStringDrawingOptions(0), lines(), leading, truncate] {
            let size = NSSize::new(w, h);
            let got = a.boundingRectWithSize_options_context(size, options, None);
            // SAFETY: the dictionary holds valid attributes.
            let want = unsafe {
                NSString::from_str(text).boundingRectWithSize_options_attributes_context(
                    size,
                    options,
                    Some(&attrs),
                    None,
                )
            };
            assert_eq!(got, want, "{w}×{h}, options {:#x}", options.0);
        }
    }
}

/// Runs in several fonts lay out together: as wide as their parts, as
/// tall as the tallest.
fn runs_measure_together(_: MainThreadMarker) {
    let (big, small) = (font_attrs(&helvetica(24.0)), font_attrs(&helvetica(12.0)));
    let s = NSMutableAttributedString::from_nsstring(&NSString::from_str("Big small"));
    // SAFETY: attribute dictionaries over ranges in the text.
    unsafe {
        s.setAttributes_range(Some(&small), NSRange::new(0, 9));
        s.setAttributes_range(Some(&big), NSRange::new(0, 3));
    }
    let size = s.size();
    let (a, b) = (string_size("Big", Some(&big)), string_size(" small", Some(&small)));
    assert!((size.width - (a.width + b.width)).abs() <= 1.0, "{size:?} vs {a:?} + {b:?}");
    assert_eq!(size.height, a.height.max(b.height));
    // A bold run is wider than the same text plain.
    let s = NSMutableAttributedString::from_nsstring(&NSString::from_str("wide"));
    // SAFETY: as above.
    unsafe { s.setAttributes_range(Some(&font_attrs(&bold(12.0))), NSRange::new(0, 4)) };
    assert!(s.size().width > string_size("wide", Some(&small)).width);
}

/// Each paragraph is laid out in the style of its first character.
fn paragraph_styles_per_paragraph(_: MainThreadMarker) {
    let font = font_attrs(&helvetica(12.0));
    let spaced = NSMutableParagraphStyle::new();
    spaced.setParagraphSpacing(10.0);
    let with_style = |range: NSRange| {
        let s = NSMutableAttributedString::from_nsstring(&NSString::from_str("One\nTwo"));
        // SAFETY: attribute dictionaries over ranges in the text.
        unsafe {
            s.setAttributes_range(Some(&font), NSRange::new(0, 7));
            s.addAttribute_value_range(NSParagraphStyleAttributeName, &spaced, range);
        }
        s.size().height
    };
    let plain = attributed("One\nTwo", &font).size().height;
    assert_eq!(with_style(NSRange::new(0, 4)), plain + 10.0, "spacing after the first paragraph");
    assert_eq!(with_style(NSRange::new(4, 3)), plain, "none after the last");
    assert_eq!(with_style(NSRange::new(2, 3)), plain, "the first paragraph's first character has no style");
    // A paragraph aligned right in a wide rectangle: its text ends at the
    // rectangle's right edge, and the first paragraph's stays at the left.
    let right = NSMutableParagraphStyle::new();
    right.setAlignment(NSTextAlignment::Right);
    let s = NSMutableAttributedString::from_nsstring(&NSString::from_str("Left\nRight"));
    // SAFETY: as above.
    unsafe {
        s.setAttributes_range(Some(&font_attrs(&bold(20.0))), NSRange::new(0, 10));
        s.addAttribute_value_range(NSParagraphStyleAttributeName, &right, NSRange::new(5, 5));
    }
    let line = attributed("L", &font_attrs(&bold(20.0))).size().height as isize;
    let rep = bitmap(200, 3 * line);
    draw_in(&rep, |_| s.drawInRect(rect(0.0, 0.0, 200.0, 3.0 * line as f64)));
    let top = ink_columns(&rep, 0, line - 2);
    let bottom = ink_columns(&rep, line + 2, 2 * line);
    assert!(top.0 < 10 && top.1 < 100, "the first line at the left: {top:?}");
    assert!(bottom.0 > 100 && bottom.1 > 190, "the second at the right: {bottom:?}");
}

/// The leftmost and rightmost columns with ink in rows `y0..y1`.
fn ink_columns(rep: &objc2_app_kit::NSBitmapImageRep, y0: isize, y1: isize) -> (isize, isize) {
    let (mut min, mut max) = (isize::MAX, isize::MIN);
    for y in y0..y1 {
        for x in 0..rep.pixelsWide() {
            if pixel(rep, x, y)[3] > 128 {
                min = min.min(x);
                max = max.max(x);
            }
        }
    }
    (min, max)
}

/// A drawing context reports what was measured or drawn.
fn drawing_context(_: MainThreadMarker) {
    let context = NSStringDrawingContext::new();
    assert_eq!((context.minimumScaleFactor(), context.actualScaleFactor()), (0.0, 0.0));
    assert_eq!(context.totalBounds(), NSRect::ZERO);
    let a = attributed("Hello wide world of text", &font_attrs(&helvetica(12.0)));
    let r = a.boundingRectWithSize_options_context(NSSize::new(60.0, 0.0), lines(), Some(&context));
    assert_eq!((context.totalBounds(), context.actualScaleFactor()), (r, 1.0));
    let rep = bitmap(100, 60);
    let context = NSStringDrawingContext::new();
    let size = NSSize::new(60.0, 0.0);
    draw_in(&rep, |_| {
        a.drawWithRect_options_context(rect(10.0, 10.0, 60.0, 0.0), NSStringDrawingOptions(0), Some(&context))
    });
    let want = a.boundingRectWithSize_options_context(size, NSStringDrawingOptions(0), None);
    assert_eq!((context.totalBounds(), context.actualScaleFactor()), (want, 1.0));
    // NSString's drawing reports to one too.
    let context = NSStringDrawingContext::new();
    let s = NSString::from_str("str");
    // SAFETY: no attributes, and a context.
    let r = unsafe { s.boundingRectWithSize_options_attributes_context(NSSize::ZERO, lines(), None, Some(&context)) };
    assert_eq!(context.totalBounds(), r);
}

fn colored(text: &str, font: &NSFont, runs: &[(NSRange, Retained<NSColor>)]) -> Retained<NSMutableAttributedString> {
    let s = NSMutableAttributedString::from_nsstring(&NSString::from_str(text));
    // SAFETY: attribute dictionaries over ranges in the text.
    unsafe {
        s.setAttributes_range(Some(&font_attrs(font)), NSRange::new(0, s.length()));
        for (range, color) in runs {
            s.addAttribute_value_range(NSForegroundColorAttributeName, color, *range);
        }
    }
    s
}

fn is_red(p: [u8; 4]) -> bool {
    p[3] > 200 && p[0] > 200 && p[1] < 60 && p[2] < 60
}

fn is_blue(p: [u8; 4]) -> bool {
    p[3] > 200 && p[2] > 200 && p[0] < 60 && p[1] < 60
}

/// Each run is drawn in its own color, where the text's size says, at the
/// point asked for (its bottom left corner, the bitmap being unflipped).
fn drawn_runs_have_their_colors(_: MainThreadMarker) {
    let font = bold(30.0);
    let red = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0);
    let blue = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0);
    let s = colored("MMMM", &font, &[(NSRange::new(0, 2), red), (NSRange::new(2, 2), blue)]);
    let size = s.size();
    let (w, h) = (200, 80);
    let rep = bitmap(w, h);
    draw_in(&rep, |_| s.drawAtPoint(pt(10.0, 10.0)));
    let (mut red_max, mut blue_min, mut reds, mut blues) = (0, isize::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let p = pixel(&rep, x, y);
            if p[3] == 0 {
                continue;
            }
            // Everything is inside the text's box: from the point up, as
            // wide and tall as the text measures (a pixel's slack).
            let (bx, by) = (x as f64, (h - 1 - y) as f64);
            assert!(bx >= 9.0 && bx <= 10.0 + size.width + 1.0, "ink at x {x}");
            assert!(by >= 9.0 && by <= 10.0 + size.height + 1.0, "ink at y {y}");
            if is_red(p) {
                red_max = red_max.max(x);
                reds += 1;
            }
            if is_blue(p) {
                blue_min = blue_min.min(x);
                blues += 1;
            }
        }
    }
    assert!(reds > 50 && blues > 50, "{reds} red and {blues} blue pixels");
    assert!(red_max < blue_min, "red ({red_max}) before blue ({blue_min})");
}

/// A background color fills behind its run's text.
fn backgrounds(_: MainThreadMarker) {
    let s = NSMutableAttributedString::from_nsstring(&NSString::from_str("    "));
    let green = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 1.0, 0.0, 1.0);
    // SAFETY: attribute dictionaries over the text.
    unsafe {
        s.setAttributes_range(Some(&font_attrs(&helvetica(20.0))), NSRange::new(0, 4));
        s.addAttribute_value_range(NSBackgroundColorAttributeName, &green, NSRange::new(0, 4));
    }
    let size = s.size();
    let rep = bitmap(100, 50);
    draw_in(&rep, |_| s.drawAtPoint(pt(5.0, 5.0)));
    let (cx, cy) = (5.0 + size.width / 2.0, 50.0 - (5.0 + size.height / 2.0));
    assert_px(&rep, cx as isize, cy as isize, GREEN);
    assert_px(&rep, 2, 2, CLEAR);
}

/// `drawInRect:` wraps to the rectangle and clips what doesn't fit;
/// `drawWithRect:options:` without line fragments puts one line's
/// baseline at the rectangle's origin.
fn rectangles(_: MainThreadMarker) {
    let s = colored("WWWW WWWW WWWW WWWW WWWW WWWW WWWW", &bold(20.0), &[]);
    let rep = bitmap(100, 100);
    let area = rect(20.0, 20.0, 50.0, 40.0);
    draw_in(&rep, |_| s.drawInRect(area));
    let mut inside = 0;
    for y in 0..100 {
        for x in 0..100 {
            if pixel(&rep, x, y)[3] == 0 {
                continue;
            }
            let (bx, by) = (x as f64, (99 - y) as f64);
            assert!(
                (19.0..=71.0).contains(&bx) && (19.0..=61.0).contains(&by),
                "ink outside the rectangle at ({x}, {y})"
            );
            inside += 1;
        }
    }
    assert!(inside > 50);
    // One line on a baseline: capitals stand on it.
    let caps = colored("HHH", &bold(20.0), &[]);
    let rep = bitmap(100, 60);
    draw_in(&rep, |_| caps.drawWithRect_options_context(rect(10.0, 20.0, 80.0, 30.0), NSStringDrawingOptions(0), None));
    let rows: Vec<isize> = (0..60).filter(|&y| (0..100).any(|x| pixel(&rep, x, y)[3] > 128)).collect();
    let lowest = 59 - rows.iter().max().expect("ink");
    assert!((19..=21).contains(&lowest), "the baseline at y 20, the ink's bottom at {lowest}");
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("default_attributes", default_attributes),
        ("one_run_matches_string_drawing", one_run_matches_string_drawing),
        ("runs_measure_together", runs_measure_together),
        ("paragraph_styles_per_paragraph", paragraph_styles_per_paragraph),
        ("drawing_context", drawing_context),
        ("drawn_runs_have_their_colors", drawn_runs_have_their_colors),
        ("backgrounds", backgrounds),
        ("rectangles", rectangles),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
