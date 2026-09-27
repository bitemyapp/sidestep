//! CoreText's hot paths, timed: what a math or diagram renderer calls for
//! each formula it lays out and draws. Runs the same on macOS (against
//! CoreText) and Linux (Sidestep); prints the median of many runs of each,
//! in microseconds. Build it in release: `cargo run --release -p ctbench`.
//! It times DejaVu Sans (TrueType outlines), then each font file named on
//! the command line (a CFF-flavoured OpenType math font, say, whose glyphs
//! have no boxes of their own: `cargo run --release -p ctbench --
//! path/to/font.otf`).

use std::hint::black_box;
use std::ptr::NonNull;
use std::time::Instant;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFAttributedString, CFData, CFRetained, CFString, CGPoint, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGDataProvider, CGFont, CGImageAlphaInfo, kCGColorSpaceSRGB,
};
use objc2_core_text::{CTFont, CTFontOrientation, CTLine, kCTFontAttributeName};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString};

use sidestep as _;

const FONT: &[u8] = include_bytes!("../../../conformance/tests/fixtures/DejaVuSans.ttf");
const FORMULA: &str = "f(x) = ∑ aₙ xⁿ + √(1 − x²)";

/// The median time of `f` over `runs` runs, in microseconds.
fn median(runs: usize, mut f: impl FnMut()) -> f64 {
    let mut times: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[runs / 2]
}

fn context() -> CFRetained<CGContext> {
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap();
    // SAFETY: no data: the context allocates.
    unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), 400, 60, 8, 0, Some(&space), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .unwrap()
}

fn main() {
    bench("DejaVu Sans", FONT.to_vec());
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let name = std::path::Path::new(&path).file_name().map_or(path.clone(), |n| n.to_string_lossy().into_owned());
        bench(&name, bytes);
    }
}

fn bench(name: &str, bytes: Vec<u8>) {
    let data = CFData::from_bytes(&bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data)).unwrap();
    let file = CGFont::with_data_provider(&provider).unwrap();
    let make_font = || unsafe { CTFont::with_graphics_font(&file, 18.0, std::ptr::null(), None) };
    let font = make_font();
    let key: &NSString = unsafe { &*(kCTFontAttributeName as *const CFString).cast() };
    let value: &AnyObject = unsafe { &*(&*font as *const CTFont).cast() };
    let attrs = NSDictionary::from_slices(&[key], &[value]);
    let string = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(FORMULA),
            Some(&attrs),
        )
    };
    let cf: &CFAttributedString = unsafe { &*Retained::as_ptr(&string).cast() };
    let line = unsafe { CTLine::with_attributed_string(cf) };
    let units: Vec<u16> = FORMULA.encode_utf16().collect();
    let mut glyphs = vec![0u16; units.len()];
    let mut advances = vec![CGSize::ZERO; units.len()];
    unsafe {
        font.glyphs_for_characters(
            NonNull::new(units.as_ptr().cast_mut()).unwrap(),
            NonNull::new(glyphs.as_mut_ptr()).unwrap(),
            units.len() as isize,
        )
    };
    let positions: Vec<CGPoint> = (0..glyphs.len()).map(|i| CGPoint::new(4.0 + 10.0 * i as f64, 20.0)).collect();
    let c = context();
    let rows: Vec<(&str, f64)> = vec![
        ("font from a CGFont (made before)", median(2000, || drop(black_box(make_font())))),
        (
            "glyphs for characters",
            median(20000, || unsafe {
                black_box(font.glyphs_for_characters(
                    NonNull::new(units.as_ptr().cast_mut()).unwrap(),
                    NonNull::new(glyphs.as_mut_ptr()).unwrap(),
                    units.len() as isize,
                ));
            }),
        ),
        (
            "advances for glyphs",
            median(20000, || unsafe {
                black_box(font.advances_for_glyphs(
                    CTFontOrientation::Horizontal,
                    NonNull::new(glyphs.as_ptr().cast_mut()).unwrap(),
                    advances.as_mut_ptr(),
                    glyphs.len() as isize,
                ));
            }),
        ),
        (
            "bounding rects for glyphs",
            median(20000, || unsafe {
                black_box(font.bounding_rects_for_glyphs(
                    CTFontOrientation::Horizontal,
                    NonNull::new(glyphs.as_ptr().cast_mut()).unwrap(),
                    std::ptr::null_mut(),
                    glyphs.len() as isize,
                ));
            }),
        ),
        (
            "line from an attributed string",
            median(2000, || drop(black_box(unsafe { CTLine::with_attributed_string(cf) }))),
        ),
        (
            "line's typographic bounds",
            median(20000, || unsafe {
                black_box(line.typographic_bounds(std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()));
            }),
        ),
        (
            "line drawn into a bitmap context",
            median(2000, || unsafe {
                CGContext::set_text_position(Some(&c), 4.0, 20.0);
                line.draw(&c);
            }),
        ),
        (
            "glyphs drawn by position",
            median(2000, || unsafe {
                font.draw_glyphs(
                    NonNull::new(glyphs.as_ptr().cast_mut()).unwrap(),
                    NonNull::new(positions.as_ptr().cast_mut()).unwrap(),
                    glyphs.len(),
                    &c,
                );
            }),
        ),
        (
            "path for a glyph",
            median(20000, || drop(black_box(unsafe { font.path_for_glyph(glyphs[0], std::ptr::null()) }))),
        ),
    ];
    println!("{name}: {} characters, {} glyphs", units.len(), glyphs.len());
    for (name, us) in rows {
        println!("{name:40} {us:8.2} µs");
    }
}
