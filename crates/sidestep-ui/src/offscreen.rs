//! Drawing into pixels of one's own, with no application running and no
//! display: [`render`] gives a [`Canvas`] for an image of a size, and
//! rasterizes what's drawn on it into an [`Image`], whose pixels
//! [`Image::to_rgba8`] reads. It goes through the rasterizer and the text
//! engine windows go through (tiny-skia, parley, swash), so what it draws
//! is what a window at the same scale shows, pixel for pixel: a Wayland
//! compositor drawing its own labels with it sets them in the same type
//! as the windows around them, and tests compare drawings to golden
//! images.
//!
//! It runs on the thread that calls it, any thread, which keeps a glyph
//! cache and an image cache of its own for it, as the render thread does:
//! the first label in a font at a size rasterizes its glyphs, and later
//! ones composite them (a 200 × 22 point label at scale 2 takes some tens
//! of microseconds then, in a release build).

use std::cell::RefCell;
use std::sync::Arc;

use kurbo::Size;
use sidestep_engine::protocol;
use sidestep_engine::raster::images::{ImageData, Pixels, next_key};
use sidestep_engine::raster::{self, Glyphs};

use crate::canvas::Canvas;
use crate::color::Appearance;
use crate::image::Image;

/// The widest and tallest an image drawn offscreen gets, in pixels.
pub const MAX_PIXELS: u32 = 16384;

thread_local!(static GLYPHS: RefCell<Glyphs> = RefCell::default());

/// Draw with `draw` into an image `size` points big at `scale` pixels a
/// point, in `appearance` (what [`Canvas::system_color`] answers), and
/// rasterize it. The image starts transparent; it is `size × scale`
/// pixels, rounded up (at least one, at most [`MAX_PIXELS`] a side), and
/// its [`size`](Image::size) is in points, so drawn into a window at that
/// size it maps pixel to pixel at that scale. The canvas's damage is all
/// of it.
pub fn render(size: Size, scale: f64, appearance: Appearance, draw: impl FnOnce(&mut Canvas)) -> Image {
    let scale = if scale.is_finite() { scale.clamp(1.0 / 16.0, 16.0) } else { 1.0 };
    let side = |points: f64| {
        let px = (points.max(0.0) * scale - 1e-6).ceil();
        if px.is_finite() { (px as u32).clamp(1, MAX_PIXELS) } else { 1 }
    };
    let (width, height) = (side(size.width), side(size.height));
    let bounds = protocol::Rect::new(0.0, 0.0, size.width.max(0.0) as f32, size.height.max(0.0) as f32);
    let mut canvas = Canvas::blank(size, scale, vec![bounds], appearance);
    draw(&mut canvas);
    // Images the canvas made for itself live until the pixels are drawn.
    let (ops, _temporaries) = canvas.finish();
    let mut px = vec![0u32; width as usize * height as usize];
    if !ops.is_empty() {
        let mut target = raster::Canvas::new(&mut px, width, height, 0.0, scale as f32);
        // All of the pixels, an edge rounded up included.
        let all = protocol::Rect::new(0.0, 0.0, (width as f64 / scale) as f32, (height as f64 / scale) as f32);
        GLYPHS.with(|g| match g.try_borrow_mut() {
            Ok(mut glyphs) => raster::paint(&mut target, &mut glyphs, &[all], &ops),
            // Drawing offscreen from inside a drawing offscreen.
            Err(_) => raster::paint(&mut target, &mut Glyphs::default(), &[all], &ops),
        });
    }
    let bytes: Arc<[u8]> = Arc::from(&*raster::as_bytes(&mut px));
    let data = ImageData { key: next_key(), generation: 0, width, height, pixels: Pixels::Rgba(bytes) };
    Image::from_data(Arc::new(data), 1.0 / scale)
}

#[cfg(test)]
mod tests {
    use kurbo::{Point, Rect};

    use super::*;
    use crate::{Color, Font, TextStyle};

    fn label(scale: f64) -> Image {
        render(Size::new(200.0, 22.0), scale, Appearance::LIGHT, |c| {
            c.fill_rect(Rect::new(0.0, 0.0, 200.0, 22.0), Color::WHITE);
            let style = TextStyle::new(Font::system(12.0), Color::BLACK);
            c.draw_label("Soroban — Untitled", &style, Point::new(4.0, 3.0));
        })
    }

    #[test]
    fn text_changes_pixels() {
        let image = label(2.0);
        assert_eq!(image.pixel_size(), (400, 44));
        assert_eq!(image.size(), Size::new(200.0, 22.0));
        let px = image.to_rgba8().expect("pixels");
        assert_eq!(px.len(), 400 * 44 * 4);
        let dark = px.as_chunks::<4>().0.iter().filter(|p| p[0] < 128 && p[3] == 255).count();
        assert!(dark > 200, "the label's glyphs darken pixels: {dark}");
        // Right of the text, white.
        let at = |x: usize, y: usize| &px[(y * 400 + x) * 4..][..4];
        assert_eq!(at(399, 43), [255, 255, 255, 255]);
    }

    #[test]
    fn images_start_transparent_and_round_up() {
        let image = render(Size::new(10.5, 3.0), 1.5, Appearance::DARK, |c| {
            assert_eq!(c.appearance(), Appearance::DARK);
            assert_eq!(c.scale(), 1.5);
            c.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Color::rgba(1.0, 0.0, 0.0, 0.5));
        });
        assert_eq!(image.pixel_size(), (16, 5));
        let straight = image.to_rgba8().expect("pixels");
        let premultiplied = image.to_premultiplied_rgba8().expect("pixels");
        assert_eq!(&straight[..4], [255, 0, 0, 128]);
        assert_eq!(&premultiplied[..4], [128, 0, 0, 128]);
        assert_eq!(&straight[straight.len() - 4..], [0, 0, 0, 0]);
        assert_eq!(render(Size::ZERO, 2.0, Appearance::LIGHT, |_| {}).pixel_size(), (1, 1));
    }

    #[test]
    fn other_threads_draw_the_same() {
        let here = label(2.0).to_rgba8();
        let there = std::thread::spawn(|| label(2.0).to_rgba8()).join().expect("the thread");
        assert!(here == there, "the same pixels on another thread");
    }

    /// `cargo test --release -p sidestep-ui offscreen -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn timing_offscreen_labels() {
        let first = std::time::Instant::now();
        let _ = label(2.0);
        let first = first.elapsed().as_secs_f64() * 1000.0;
        let warm = sidestep_engine::backend::median(|| {
            for _ in 0..100 {
                let _ = label(2.0);
            }
        }) / 100.0;
        // New text each time: laid out, its glyphs cached.
        let mut n = 0;
        let new = sidestep_engine::backend::median(|| {
            for _ in 0..100 {
                n += 1;
                let _ = render(Size::new(200.0, 22.0), 2.0, Appearance::LIGHT, |c| {
                    let style = TextStyle::new(Font::system(12.0), Color::BLACK);
                    c.draw_label(&format!("Untitled {n}"), &style, Point::new(4.0, 3.0));
                });
            }
        }) / 100.0;
        println!(
            "a 200 × 22 point label at scale 2: first {first:.3} ms, then {:.1} µs, {:.1} µs for new text",
            warm * 1000.0,
            new * 1000.0
        );
        assert!(warm < 1.0 && new < 1.0, "{warm} ms, {new} ms");
    }
}
