//! The theme drawn against reference images: the parts in their states,
//! in both palettes at scales 1 and 2, recorded and rasterized as a
//! window's are and compared with the PNGs in `golden/`. Run with
//! `SIDESTEP_BLESS=1` to write them afresh after a deliberate change, and
//! look at them before committing. Text isn't drawn: it depends on the
//! system's fonts.

use std::path::PathBuf;

use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::paint::{self, radii};
use super::palette::{DARK, LIGHT, Palette};
use super::parts::{self, Axis, Emphasis, Outline, RADIUS, State};
use crate::graphics::{Xf, begin_recording, end_recording, set_view};
use crate::protocol::Rect;
use crate::raster::{Canvas, Glyphs, paint as rasterize};

const W: f64 = 340.0;
const H: f64 = 214.0;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Every part, in a flipped view the size of the sheet.
fn sheet(p: &Palette) {
    let axis = Axis { flipped: true };
    let rest = State::default();
    let pressed = State { pressed: true, ..rest };
    let on = State { on: true, ..rest };
    let disabled = State { disabled: true, ..rest };
    paint::fill_rect(rect(0.0, 0.0, W, H), p.window);

    // Push buttons: at rest, pressed, default, destructive, disabled and
    // focused.
    let push = [
        (Emphasis::Normal, rest),
        (Emphasis::Normal, pressed),
        (Emphasis::Default, rest),
        (Emphasis::Destructive, rest),
        (Emphasis::Normal, disabled),
        (Emphasis::Normal, rest),
    ];
    for (i, (emphasis, state)) in push.into_iter().enumerate() {
        parts::button_bezel(p, rect(10.0 + 54.0 * i as f64, 10.0, 46.0, 24.0), radii(RADIUS), emphasis, state);
    }
    parts::focus_ring(p, Outline::RoundRect(rect(280.0, 10.0, 46.0, 24.0), radii(RADIUS)));

    // Check boxes, radio buttons and switches.
    let checks = [rest, on, State { mixed: true, ..rest }, State { on: true, disabled: true, ..rest }];
    for (i, state) in checks.into_iter().enumerate() {
        parts::check_box(p, rect(10.0 + 24.0 * i as f64, 50.0, 16.0, 16.0), axis, state);
    }
    for (i, state) in [rest, on, State { on: true, disabled: true, ..rest }].into_iter().enumerate() {
        parts::radio(p, rect(110.0 + 24.0 * i as f64, 50.0, 16.0, 16.0), state);
    }
    parts::focus_ring(p, Outline::Ellipse(rect(158.0, 50.0, 16.0, 16.0)));
    parts::switch(p, rect(190.0, 46.0, 54.0, 24.0), 0.0, rest);
    parts::switch(p, rect(254.0, 46.0, 54.0, 24.0), 1.0, on);

    // Fields, boxes, the help and disclosure buttons.
    parts::entry(p, rect(10.0, 84.0, 90.0, 24.0), Some(p.view), rest);
    parts::entry(p, rect(110.0, 84.0, 60.0, 24.0), None, disabled);
    parts::plain_border(p, rect(180.0, 84.0, 40.0, 24.0), rest);
    parts::card(p, rect(230.0, 80.0, 50.0, 32.0));
    parts::separator(p, rect(236.0, 92.0, 38.0, 8.0));
    parts::help(p, rect(288.0, 84.0, 24.0, 24.0), axis, rest);
    parts::disclosure(p, rect(316.0, 84.0, 13.0, 13.0), axis, false, rest);
    parts::disclosure(p, rect(316.0, 97.0, 13.0, 13.0), axis, true, rest);

    // Indicators.
    parts::progress_bar(p, rect(10.0, 120.0, 90.0, 20.0), 0.4, rest);
    parts::progress_pulse(p, rect(110.0, 120.0, 90.0, 20.0), 0.3, rest);
    parts::spinner(p, rect(210.0, 118.0, 24.0, 24.0), 0.25, rest);
    parts::spinner(p, rect(240.0, 122.0, 16.0, 16.0), 0.6, disabled);

    // A segmented control of three, the middle one chosen and the last
    // pressed; a stepper with its upper half held; a slider with ticks.
    let whole = rect(10.0, 150.0, 120.0, 24.0);
    parts::segment_trough(p, whole, rest);
    parts::segment(p, rect(10.0, 150.0, 40.0, 24.0), [RADIUS, 0.0, 0.0, RADIUS], false, None, rest);
    parts::segment(p, rect(50.0, 150.0, 40.0, 24.0), radii(0.0), true, None, rest);
    parts::segment(p, rect(90.0, 150.0, 40.0, 24.0), [0.0, RADIUS, RADIUS, 0.0], false, None, pressed);
    parts::separated_segment(p, rect(140.0, 150.0, 30.0, 24.0), true, None, rest);
    parts::stepper(p, rect(180.0, 149.0, 20.0, 26.0), axis, Some(true), rest);
    let bar = rect(215.0, 159.0, 110.0, 6.0);
    parts::slider_track(p, bar, false, Some(260.0), None, rest);
    for i in 0..5 {
        let x = 219.0 + 25.0 * i as f64;
        parts::tick(p, rect(x, 161.0, 2.0, 2.0), x < 260.0, rest);
    }
    parts::knob(p, rect(251.0, 153.0, 18.0, 18.0), rest);

    // Pop-up buttons, at rest and pressed.
    parts::pop_up(p, rect(10.0, 184.0, 100.0, 24.0), axis, 22.0, rest);
    parts::pop_up(p, rect(120.0, 184.0, 100.0, 24.0), axis, 22.0, pressed);
}

/// The sheet in `p` at `scale`, as RGB bytes.
fn render(p: &Palette, scale: f32) -> (u32, u32, Vec<u8>) {
    begin_recording();
    set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, W as f32, H as f32));
    sheet(p);
    let ops = end_recording();
    let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
    let mut px = vec![0u32; (w * h) as usize];
    let mut canvas = Canvas::new(&mut px, w, h, 0.0, scale);
    rasterize(&mut canvas, &mut Glyphs::default(), &[Rect::new(0.0, 0.0, W as f32, H as f32)], &ops);
    // Canvas pixels are RGBA bytes in memory (see `raster`).
    let rgb = px.iter().flat_map(|p| [*p as u8, (p >> 8) as u8, (p >> 16) as u8]).collect();
    (w, h, rgb)
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/theme/golden").join(format!("{name}.png"))
}

fn write_png(path: &PathBuf, w: u32, h: u32, rgb: &[u8]) {
    let file = std::fs::File::create(path).expect("create the image");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().expect("write the header");
    writer.write_image_data(rgb).expect("write the image");
}

fn read_png(path: &PathBuf) -> (u32, u32, Vec<u8>) {
    let file = std::fs::File::open(path).unwrap_or_else(|_| {
        panic!("{} is missing: run with SIDESTEP_BLESS=1 to make it, and look at it", path.display())
    });
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("read the header");
    let mut buf = vec![0; reader.output_buffer_size().expect("a sane size")];
    let info = reader.next_frame(&mut buf).expect("read the image");
    assert_eq!((info.color_type, info.bit_depth), (png::ColorType::Rgb, png::BitDepth::Eight), "{}", path.display());
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

#[test]
fn the_theme_matches_its_goldens() {
    let bless = std::env::var_os("SIDESTEP_BLESS").is_some_and(|v| v != "0");
    let mut failures = Vec::new();
    for (name, palette) in [("light", &LIGHT), ("dark", &DARK)] {
        for scale in [1.0, 2.0] {
            let name = format!("{name}@{scale}x");
            let (w, h, rgb) = render(palette, scale);
            let path = golden_path(&name);
            if bless {
                write_png(&path, w, h, &rgb);
                continue;
            }
            let (gw, gh, golden) = read_png(&path);
            if (gw, gh) != (w, h) {
                failures.push(format!("{name}: {w}x{h}, golden {gw}x{gh}"));
                continue;
            }
            // Antialiasing may round differently on another CPU's SIMD, so
            // a channel may be off by a little; anything more is a change.
            let differing = rgb.iter().zip(&golden).filter(|(a, b)| a.abs_diff(**b) > 3).count();
            if differing > 0 {
                let actual = std::env::temp_dir().join(format!("sidestep-theme-{name}.png"));
                write_png(&actual, w, h, &rgb);
                failures.push(format!("{name}: {differing} channels differ; this run drew {}", actual.display()));
            }
        }
    }
    assert!(failures.is_empty(), "the theme changed:\n{}", failures.join("\n"));
}
