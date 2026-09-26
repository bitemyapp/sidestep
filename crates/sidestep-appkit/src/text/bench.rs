//! What a page of text costs to draw: recording the 60 lines of
//! `examples/appkit-slice` through `drawAtPoint:withAttributes:` on the
//! main thread, then compositing them on the render thread with the glyph
//! cache warm and cold. Medians of seven runs; run in release mode:
//!
//! ```sh
//! scripts/linux-cargo test --release -p sidestep-appkit bench_ -- --ignored --nocapture
//! ```

use std::hint::black_box;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSColor, NSFont, NSFontAttributeName, NSFontWeightRegular, NSForegroundColorAttributeName, NSStringDrawing,
};
use objc2_foundation::{NSDictionary, NSPoint, NSString};

use crate::graphics::{self, Xf};
use crate::protocol::{Op, Rect};
use crate::raster::{self, Canvas, Glyphs};

const LINES: usize = 60;
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;

fn line(i: usize) -> String {
    format!("{i:04}  fn example_{i}(value: usize) -> Result<Vec<String>, Error> {{ todo!() }}")
}

/// Median microseconds per call of `f`, over seven runs of `iters`.
fn median(iters: u32, mut f: impl FnMut()) -> f64 {
    f();
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iters {
                f();
            }
            start.elapsed().as_secs_f64() * 1e6 / f64::from(iters)
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

fn attributes() -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: a valid weight; the keys are constant strings.
    let font = unsafe { NSFont::monospacedSystemFontOfSize_weight(13.0, NSFontWeightRegular) };
    let color = NSColor::colorWithSRGBRed_green_blue_alpha(0.80, 0.84, 0.96, 1.0);
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_slices(&keys, &[&*font as &AnyObject, &*color as &AnyObject])
}

fn record(strings: &[Retained<NSString>], attrs: &NSDictionary<NSString, AnyObject>) -> Vec<Op> {
    graphics::begin_recording();
    graphics::set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32));
    for (i, s) in strings.iter().enumerate() {
        // SAFETY: the dictionary holds valid attributes.
        unsafe { s.drawAtPoint_withAttributes(NSPoint::new(16.0, 16.0 + i as f64 * 20.0), Some(attrs)) };
    }
    graphics::end_recording()
}

fn composite(glyphs: &mut Glyphs, px: &mut [u32], ops: &[Op]) {
    let mut canvas = Canvas { px, width: WIDTH, height: HEIGHT, origin_y: 0.0, scale: 1.0 };
    raster::paint(&mut canvas, glyphs, &[Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32)], ops);
}

#[test]
#[ignore = "a benchmark; run in release mode"]
fn bench_page_of_text() {
    graphics::install_string_drawing();
    let attrs = attributes();
    let strings: Vec<_> = (0..LINES).map(|i| NSString::from_str(&line(i))).collect();
    let chars: usize = (0..LINES).map(|i| line(i).chars().count()).sum();

    let recorded = median(200, || {
        black_box(record(&strings, &attrs));
    });
    // Lines never drawn before: shaped and laid out, not found in a cache.
    let mut next = 0;
    let fresh = median(20, || {
        let strings: Vec<_> = (0..LINES).map(|i| NSString::from_str(&line(10_000 + next * LINES + i))).collect();
        next += 1;
        black_box(record(&strings, &attrs));
    });

    let ops = record(&strings, &attrs);
    let mut px = vec![0u32; (WIDTH * HEIGHT) as usize];
    let mut glyphs = Glyphs::default();
    let warm = median(200, || composite(&mut glyphs, &mut px, &ops));
    let cold = median(20, || composite(&mut Glyphs::default(), &mut px, &ops));

    println!("page of {LINES} lines, {chars} characters");
    println!("record (drawAtPoint:, laid out before) {recorded:>9.1} µs");
    println!("record (drawAtPoint:, new lines)       {fresh:>9.1} µs");
    println!("composite (glyph cache warm)           {warm:>9.1} µs");
    println!("composite (glyph cache cold)           {cold:>9.1} µs");
    println!("glyphs composited per µs (warm)        {:>9.1}", chars as f64 / warm);
}

#[test]
#[ignore = "a benchmark; run in release mode"]
fn bench_repeated_label() {
    // A table's first frame: one label never drawn before, in every row.
    graphics::install_string_drawing();
    let attrs = attributes();
    let mut next = 0;
    let mut pass = |rows: usize| {
        let label = NSString::from_str(&format!("label {next}"));
        next += 1;
        graphics::begin_recording();
        graphics::set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32));
        for i in 0..rows {
            // SAFETY: the dictionary holds valid attributes.
            unsafe { label.drawAtPoint_withAttributes(NSPoint::new(16.0, i as f64 * 13.0), Some(&attrs)) };
        }
        black_box(graphics::end_recording());
    };
    let once = median(50, || pass(1));
    let sixty = median(50, || pass(60));
    println!("a new label drawn once in a pass      {once:>9.1} µs");
    println!("a new label drawn in 60 rows          {sixty:>9.1} µs");
}

#[test]
#[ignore = "a benchmark; run in release mode"]
fn bench_many_runs() {
    // An attributed string's worth of runs: two attributes alternating
    // word by word, as syntax colors do, over many paragraphs. Laid out
    // directly, so nothing comes from the cache.
    use crate::text::fonts::{self, Design, FontSpec};
    use crate::text::layout::{self, Attrs, Options, Run, TextFont};
    let face = fonts::resolve(&FontSpec::system(Design::Monospaced, 13.0));
    let a = Attrs::new(TextFont { face, size: 13.0, tabular_digits: false, features: None });
    let b = Attrs { color: [1.0, 0.0, 0.0, 1.0], ..a.clone() };
    let attrs = [a, b];
    for lines in [500, 1000, 2000] {
        let text: String = (0..lines).map(|i| format!("let value_{i} = compute(input, {i}) + offset;\n")).collect();
        let mut runs = Vec::new();
        let mut at = 0;
        for (i, word) in text.split_inclusive(' ').enumerate() {
            runs.push(Run { start: at, end: at + word.len(), attrs: (i % 2) as u32 });
            at += word.len();
        }
        let one = [Run { start: 0, end: text.len(), attrs: 0 }];
        let lay = |runs: &[Run]| {
            crate::text::with_ctx(|ctx| black_box(layout::compute(ctx, &text, &attrs, runs, &Options::UNBOUNDED)));
        };
        let many = median(1, || lay(&runs)) / 1e3;
        let single = median(1, || lay(&one)) / 1e3;
        println!(
            "{lines} lines, {} runs: {many:.2} ms, one run {single:.2} ms, extra {:.2} ms ({:.2} µs a line)",
            runs.len(),
            many - single,
            (many - single) * 1e3 / lines as f64
        );
    }
}

#[test]
#[ignore = "a benchmark; run in release mode"]
fn bench_opening_fonts() {
    let start = Instant::now();
    crate::text::fonts::shared();
    println!("opening the system's fonts: {:.1} ms", start.elapsed().as_secs_f64() * 1e3);
    let start = Instant::now();
    let face =
        crate::text::fonts::resolve(&crate::text::fonts::FontSpec::system(crate::text::fonts::Design::Default, 13.0));
    println!("resolving the system font: {:.2} ms ({})", start.elapsed().as_secs_f64() * 1e3, face.postscript_name);
}
