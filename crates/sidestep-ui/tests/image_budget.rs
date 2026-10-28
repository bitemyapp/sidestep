//! What drawing a photo into a view costs, by the way it lands: an image
//! viewer's whole window (1400 × 900 points at 2×) drawn offscreen with a
//! 2800 × 1800 photo one to one (nearest, the copy blend mode, a third
//! opaque, smooth, smooth a quarter point off the pixel grid), enlarged
//! two and three times, shrunk a little, and a 6000 × 4000 photo shrunk
//! to fit, nearest and smooth. The best of eight draws of each is
//! printed, beside drawing nothing and a solid fill (the render's own
//! cost: its pixels, and handing them over). Run it in a release build:
//!
//! ```sh
//! cargo test --release -p sidestep-ui --test image_budget
//! ```
//!
//! It asserts nothing about the times (machines differ), only that a
//! photo drawn one to one keeps its pixels, smooth or not. A debug build
//! draws a quarter of the size, once each.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::time::Instant;

    use sidestep_ui::kurbo::{Rect, Size};
    use sidestep_ui::*;

    /// Straight RGBA pixels that differ from their neighbours, so any
    /// resampling shows.
    fn photo(w: u32, h: u32) -> Vec<u8> {
        let mut seed = 0x9e37_79b9_u32;
        (0..w * h)
            .flat_map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let [r, g, b, _] = seed.to_le_bytes();
                [r, g, b, 255]
            })
            .collect()
    }

    pub fn run() {
        // SAFETY: nothing else runs yet.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
        let quarter = cfg!(debug_assertions);
        let k = if quarter { 0.5 } else { 1.0 };
        let reps = if quarter { 1 } else { 8 };
        let view = Size::new(1400.0 * k, 900.0 * k);
        let (vw, vh) = ((2800.0 * k) as u32, (1800.0 * k) as u32);
        let fit_px = photo(vw, vh);
        let fit = Image::from_rgba(vw, vh, &fit_px).expect("pixels");
        // The same, a third opaque (a screenshot with a shadow, say).
        let faint_px: Vec<u8> = fit_px.chunks(4).flat_map(|p| [p[0], p[1], p[2], 90]).collect();
        let faint = Image::from_rgba(vw, vh, &faint_px).expect("pixels");
        let (fw, fh) = ((6000.0 * k) as u32, (4000.0 * k) as u32);
        let full = Image::from_rgba(fw, fh, &photo(fw, fh)).expect("pixels");
        let whole = Rect::from_origin_size((0.0, 0.0), view);

        // One to one, smooth or not, in either blend mode: the photo's own
        // pixels.
        for (smooth, blend) in [(false, BlendMode::Normal), (false, BlendMode::Copy), (true, BlendMode::Normal)] {
            let drawn = offscreen::render(view, 2.0, Appearance::DARK, |c| {
                c.set_blend_mode(blend);
                c.draw_image_with(&fit, None, whole, ImageOptions { smooth, ..ImageOptions::default() });
            });
            let px = drawn.to_rgba8().expect("pixels");
            assert!(px == fit_px, "drawn one to one ({smooth}, {blend:?}), the photo's pixels");
        }

        let at = |x: f64, y: f64, w: f64, h: f64| Rect::new(x * k, y * k, (x + w) * k, (y + h) * k);
        // What's drawn: the name, the image, its src and dst, whether
        // smooth, and the blend mode.
        type Case<'a> = (&'a str, &'a Image, Option<Rect>, Rect, bool, BlendMode);
        let cases: Vec<Case> = vec![
            ("one to one, nearest", &fit, None, whole, false, BlendMode::Normal),
            ("one to one, nearest, copy", &fit, None, whole, false, BlendMode::Copy),
            ("one to one, translucent", &faint, None, whole, false, BlendMode::Normal),
            ("one to one, smooth", &fit, None, whole, true, BlendMode::Normal),
            (
                "one to one, quarter point off, smooth",
                &fit,
                None,
                at(0.25, 0.25, 1400.0, 900.0),
                true,
                BlendMode::Normal,
            ),
            ("2x up, nearest", &full, Some(at(0.0, 0.0, 1400.0, 900.0)), whole, false, BlendMode::Normal),
            ("3x up, nearest", &full, Some(at(0.0, 0.0, 2800.0 / 3.0, 600.0)), whole, false, BlendMode::Normal),
            ("0.9x down, nearest", &fit, None, at(0.0, 0.0, 1260.0, 810.0), false, BlendMode::Normal),
            ("6000 x 4000 to fit, nearest", &full, None, at(0.0, 0.0, 1350.0, 900.0), false, BlendMode::Normal),
            ("2x up, smooth", &full, Some(at(0.0, 0.0, 1400.0, 900.0)), whole, true, BlendMode::Normal),
            ("0.9x down, smooth", &fit, None, at(0.0, 0.0, 1260.0, 810.0), true, BlendMode::Normal),
            ("6000 x 4000 to fit, smooth", &full, None, at(0.0, 0.0, 1350.0, 900.0), true, BlendMode::Normal),
        ];
        let time = |draw: &dyn Fn(&mut Canvas)| {
            let t = Instant::now();
            let _ = offscreen::render(view, 2.0, Appearance::DARK, draw);
            t.elapsed().as_secs_f64() * 1000.0
        };
        let best = |draw: &dyn Fn(&mut Canvas)| (0..reps).map(|_| time(draw)).fold(f64::MAX, f64::min);
        // A render kept, as a program keeps what it rendered: with none
        // outstanding, the allocator may hand each render fresh pages,
        // which can cost more than the drawing.
        let _kept = offscreen::render(view, 2.0, Appearance::DARK, |c| c.draw_image(&fit, whole));
        println!("{:40} {:>9}", "nothing drawn", format!("{:.2} ms", best(&|_| {})));
        println!(
            "{:40} {:>9}",
            "a solid fill",
            format!("{:.2} ms", best(&|c| c.fill_rect(whole, Color::rgb8(20, 20, 30))))
        );
        for (name, image, src, dst, smooth, blend) in cases {
            let draw = |c: &mut Canvas| {
                c.set_blend_mode(blend);
                c.draw_image_with(image, src, dst, ImageOptions { smooth, ..ImageOptions::default() });
            };
            println!("{name:40} {:>9}", format!("{:.2} ms", best(&draw)));
        }
        println!("image_budget: ok");
    }
}
