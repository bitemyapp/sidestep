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
use std::time::{Duration, Instant};

use kurbo::Size;
use sidestep_engine::protocol;
use sidestep_engine::raster::images::{ImageData, Pixels, next_key};
use sidestep_engine::raster::{self, Glyphs};
use sidestep_engine::stats;

use crate::FrameTiming;
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
    rendered(size, scale, appearance, None, draw)
}

/// [`render`] only where `damage` says (rectangles in points), as a
/// window redraws its damage: the canvas's [`needs`](Canvas::needs) and
/// [`damage`](Canvas::damage) answer for those rectangles, only their
/// pixels are rasterized, and the rest stay transparent. What a frame
/// redrawing that damage costs a window, drawn once, its rectangles
/// rasterized one after another rather than their bounding box.
pub fn render_damage(
    size: Size,
    scale: f64,
    appearance: Appearance,
    damage: &[kurbo::Rect],
    draw: impl FnOnce(&mut Canvas),
) -> Image {
    rendered(size, scale, appearance, Some(damage), draw)
}

fn rendered(
    size: Size,
    scale: f64,
    appearance: Appearance,
    damage: Option<&[kurbo::Rect]>,
    draw: impl FnOnce(&mut Canvas),
) -> Image {
    let scale = if scale.is_finite() { scale.clamp(1.0 / 16.0, 16.0) } else { 1.0 };
    let side = |points: f64| {
        let px = (points.max(0.0) * scale - 1e-6).ceil();
        if px.is_finite() { (px as u32).clamp(1, MAX_PIXELS) } else { 1 }
    };
    let (width, height) = (side(size.width), side(size.height));
    let bounds = protocol::Rect::new(0.0, 0.0, size.width.max(0.0) as f32, size.height.max(0.0) as f32);
    // All of the pixels, an edge rounded up included.
    let all = protocol::Rect::new(0.0, 0.0, (width as f64 / scale) as f32, (height as f64 / scale) as f32);
    let (marked, painted) = match damage {
        None => (vec![bounds], vec![all]),
        Some(rects) => {
            let rects: Vec<protocol::Rect> = rects
                .iter()
                .map(|r| protocol::Rect::new(r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32).intersect(&all))
                .filter(|r| !r.is_empty())
                .collect();
            (rects.clone(), rects)
        }
    };
    let recording = stats::on().then(Instant::now);
    let mut canvas = Canvas::blank(size, scale, marked, appearance);
    draw(&mut canvas);
    // Images the canvas made for itself live until the pixels are drawn.
    let (ops, _temporaries) = canvas.finish();
    let rasterizing = recording.map(|r| (r.elapsed(), Instant::now()));
    // Drawn straight into the image's pixels, which start transparent
    // (zeroed by the allocator: a large image's pages are the system's
    // zero pages until drawn on), rather than drawn and then copied out: a
    // window's worth at 2× is 20 MB.
    let mut bytes = zeroed(width as usize * height as usize * 4);
    let px = pixels_of(Arc::get_mut(&mut bytes).expect("a new image's pixels are its own"));
    if !ops.is_empty() && !painted.is_empty() {
        let mut target = raster::Canvas::new(px, width, height, 0.0, scale as f32);
        GLYPHS.with(|g| match g.try_borrow_mut() {
            Ok(mut glyphs) => raster::paint(&mut target, &mut glyphs, &painted, &ops),
            // Drawing offscreen from inside a drawing offscreen.
            Err(_) => raster::paint(&mut target, &mut Glyphs::default(), &painted, &ops),
        });
    }
    if let Some((record, started)) = rasterizing {
        stats::offscreen(record, started.elapsed());
    }
    let data = ImageData { key: next_key(), generation: 0, width, height, pixels: Pixels::Rgba(bytes) };
    Image::from_data(Arc::new(data), 1.0 / scale)
}

/// `len` zero bytes, shared.
fn zeroed(len: usize) -> Arc<[u8]> {
    // SAFETY: zero bytes are valid u8s.
    unsafe { Arc::<[u8]>::new_zeroed_slice(len).assume_init() }
}

/// Bytes as the canvas pixels they hold, four to a pixel.
fn pixels_of(bytes: &mut [u8]) -> &mut [u32] {
    // SAFETY: any four bytes are a u32, and `align_to_mut` gives only the
    // aligned middle: all of them when they start on a u32's boundary, as
    // an `Arc`'s data does (after its two counters).
    let (before, px, after) = unsafe { bytes.align_to_mut::<u32>() };
    assert!(before.is_empty() && after.is_empty(), "sidestep: an image's pixels start on a pixel's boundary");
    px
}

/// Measure offscreen renders, as windows' frames are measured, summarizing
/// them every `period` for [`frame_stats`] (the summaries so far dropped),
/// or stop (`None`). It's the switch [`Cx::set_frame_stats`](crate::Cx::set_frame_stats)
/// throws, for windows and renders alike, and needs no application
/// running; `SIDESTEP_FRAME_STATS=1` measures from the start and prints
/// the summaries, renders' as `offscreen`.
pub fn set_frame_stats(period: Option<Duration>) {
    stats::set_period(period);
}

/// Offscreen renders over a period ([`frame_stats`]): how many, and how
/// long drawing (recording ops) and rasterizing took.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderStats {
    pub period: Duration,
    pub renders: u32,
    pub record: FrameTiming,
    pub rasterize: FrameTiming,
    pub total: FrameTiming,
}

/// The latest summary of the renders on every thread, if renders are
/// measured and some were made in the last period.
pub fn frame_stats() -> Option<RenderStats> {
    let s = stats::latest().into_iter().find(|s| s.window == stats::OFFSCREEN)?;
    let us = |v: u32| Duration::from_micros(u64::from(v));
    let timing = |t: &stats::Timing| FrameTiming { p50: us(t.p50), p95: us(t.p95), max: us(t.max) };
    Some(RenderStats {
        period: s.period,
        renders: s.frames,
        record: s.record.as_ref().map(timing).unwrap_or_default(),
        rasterize: timing(&s.rasterize),
        total: timing(&s.total),
    })
}

#[cfg(test)]
mod tests {
    use kurbo::{Point, Rect};

    use super::*;
    use crate::{Color, Font, ImageOptions, TextStyle};

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

    /// Drawn where the damage says, the pixels there are the whole
    /// drawing's, the rest stay transparent, and the canvas answers for
    /// the damage alone.
    #[test]
    fn damage_draws_only_its_rectangles() {
        let draw = |c: &mut Canvas| {
            c.fill_rect(Rect::new(0.0, 0.0, 60.0, 40.0), Color::rgb(0.2, 0.3, 0.4));
            // Inside the second rectangle: antialiased edges cut by damage
            // come out a little differently, in windows as here.
            c.fill(&kurbo::Circle::new((48.0, 30.0), 6.0), Color::rgba(0.9, 0.5, 0.1, 0.7));
            let style = TextStyle::new(Font::system(9.0), Color::WHITE);
            c.draw_label("Damage", &style, Point::new(3.0, 5.0));
        };
        let size = Size::new(60.0, 40.0);
        let whole = render(size, 2.0, Appearance::DARK, draw).to_rgba8().expect("pixels").to_vec();
        let damage = [Rect::new(2.0, 3.0, 25.5, 12.0), Rect::new(40.0, 20.0, 58.0, 39.0)];
        let mut asked = Vec::new();
        let part = render_damage(size, 2.0, Appearance::DARK, &damage, |c| {
            asked = c.damage();
            assert!(c.needs(Rect::new(41.0, 21.0, 42.0, 22.0)));
            assert!(!c.needs(Rect::new(30.0, 13.0, 35.0, 18.0)));
            draw(c)
        });
        assert_eq!(asked, damage);
        let part = part.to_rgba8().expect("pixels");
        let inside = |x: usize, y: usize| {
            let (px, py) = (x as f64 / 2.0 + 0.25, y as f64 / 2.0 + 0.25);
            damage.iter().any(|r| r.contains(Point::new(px, py)))
        };
        for y in 0..80 {
            for x in 0..120 {
                let at = (y * 120 + x) * 4;
                if inside(x, y) {
                    assert_eq!(part[at..at + 4], whole[at..at + 4], "({x}, {y})");
                } else {
                    assert_eq!(part[at + 3], 0, "({x}, {y}) is left alone");
                }
            }
        }
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
    fn renders_are_measured_as_frames() {
        set_frame_stats(Some(Duration::from_millis(30)));
        let deadline = Instant::now() + Duration::from_secs(10);
        let stats = loop {
            let _ = label(2.0);
            if let Some(s) = frame_stats().filter(|s| s.renders >= 3) {
                break s;
            }
            assert!(Instant::now() < deadline, "renders never summarized");
        };
        set_frame_stats(None);
        assert!(stats.record.max > Duration::ZERO && stats.rasterize.max > Duration::ZERO, "{stats:?}");
        assert!(stats.total.max >= stats.rasterize.max, "{stats:?}");
    }

    #[test]
    fn other_threads_draw_the_same() {
        let here = label(2.0).to_rgba8();
        let there = std::thread::spawn(|| label(2.0).to_rgba8()).join().expect("the thread");
        assert!(here == there, "the same pixels on another thread");
    }

    /// A photo drawn smooth where its pixels land one to one on the
    /// render's, at fractional scales too, keeps them: nothing to smooth.
    #[test]
    fn smooth_photos_drawn_one_to_one_keep_their_pixels() {
        // A strip of every width from 20 pixels to 140, as many points as
        // that makes at each scale (the sums in floats rarely come to one).
        for scale in [1.25, 1.5, 1.75, 2.5, 3.0] {
            for px in 20u32..140 {
                let (w, h) = (f64::from(px) / scale, 4.0 / scale);
                let strip: Vec<u8> =
                    (0..px * 4).flat_map(|i| [(i * 7) as u8, (i * 13) as u8, (i * 5) as u8, 255]).collect();
                let image = Image::from_rgba(px, 4, &strip).expect("pixels");
                let drawn = render(Size::new(w + 8.0, h + 8.0), scale, Appearance::LIGHT, |c| {
                    c.draw_image_with(&image, None, Rect::new(4.0, 4.0, 4.0 + w, 4.0 + h), ImageOptions::default());
                });
                let got = drawn.to_rgba8().expect("pixels");
                let (side, at) = (drawn.pixel_size().0 as usize, (4.0 * scale) as usize);
                for y in 0..4 {
                    let row = &got[((at + y) * side + at) * 4..][..px as usize * 4];
                    assert!(row == &strip[y * px as usize * 4..][..px as usize * 4], "{px} pixels at {scale}");
                }
            }
        }
        for (scale, px) in [(1.0, 200u32), (1.25, 250), (1.5, 300), (2.0, 400)] {
            let photo: Vec<u8> =
                (0..px * px).flat_map(|i| [(i * 7) as u8, (i * 13 + i / px) as u8, (i / px * 5) as u8, 255]).collect();
            let image = Image::from_rgba(px, px, &photo).expect("pixels");
            let drawn = render(Size::new(220.0, 220.0), scale, Appearance::LIGHT, |c| {
                c.draw_image_with(&image, None, Rect::new(8.0, 20.0, 208.0, 220.0), ImageOptions::default());
            });
            let got = drawn.to_rgba8().expect("pixels");
            let side = drawn.pixel_size().0 as usize;
            let (x0, y0) = ((8.0 * scale) as usize, (20.0 * scale) as usize);
            let differ = (0..px as usize * px as usize)
                .filter(|&i| {
                    let (x, y) = (i % px as usize, i / px as usize);
                    got[((y0 + y) * side + x0 + x) * 4..][..4] != photo[i * 4..][..4]
                })
                .count();
            assert_eq!(differ, 0, "{differ} pixels changed at scale {scale}");
        }
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

    /// A window's plain content, as `tests/frame_budget.rs` draws it, its
    /// damage cleared first as a window's pass clears it.
    pub(crate) fn plain_window(size: Size, scale: f64) -> Vec<protocol::Op> {
        use kurbo::RoundedRect;
        let bounds = protocol::Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
        let mut c = Canvas::new(size, scale, vec![bounds], Some(Color::hex(0x202020)), Appearance::DARK);
        c.fill_rect(Rect::from_origin_size(Point::ZERO, size), Color::hex(0x1b1d23));
        c.fill_rect(Rect::new(0.0, 0.0, 220.0, size.height), Color::hex(0x23262e));
        let label = TextStyle::new(Font::system(13.0), Color::hex(0xd8dce4));
        let dim = TextStyle::new(Font::system(11.0), Color::hex(0x8a90a0));
        let mut y = 12.0;
        while y < size.height - 20.0 {
            c.draw_label("Sidebar item", &label, Point::new(16.0, y));
            c.draw_label("Soroban — a row of a list, with its details", &label, Point::new(240.0, y));
            c.draw_label("12:41", &dim, Point::new(size.width - 60.0, y + 1.0));
            y += 28.0;
        }
        for i in 0..6 {
            let x = 260.0 + f64::from(i) * 140.0;
            c.fill(&RoundedRect::new(x, 60.0, x + 120.0, 140.0, 8.0), Color::hex(0x3a6ea5));
        }
        c.finish().0
    }

    /// A window drawn in bands on several threads is the pixels one thread
    /// draws: fills, glyphs and rounded rectangles, at whole and
    /// fractional scales, whole and in part.
    #[test]
    fn bands_draw_windows_as_one_thread_does() {
        let size = Size::new(1440.0, 900.0);
        for scale in [1.0, 1.5, 2.0] {
            let ops = plain_window(size, scale);
            let (w, h) = ((size.width * scale).ceil() as u32, (size.height * scale).ceil() as u32);
            for damage in [
                protocol::Rect::new(0.0, 0.0, size.width as f32, size.height as f32),
                protocol::Rect::new(100.0, 33.5, 1300.0, 870.25),
            ] {
                let (mut one, mut banded) = (vec![0u32; (w * h) as usize], vec![0u32; (w * h) as usize]);
                let mut canvas = raster::Canvas::new(&mut one, w, h, 0.0, scale as f32);
                raster::paint(&mut canvas, &mut Glyphs::default(), &[damage], &ops);
                let mut canvas = raster::Canvas::new(&mut banded, w, h, 0.0, scale as f32);
                raster::bands::paint_banded(&mut canvas, &mut Glyphs::default(), &[damage], &ops);
                let differ = one.iter().zip(&banded).filter(|(a, b)| a != b).count();
                assert_eq!(differ, 0, "{differ} pixels differ at scale {scale} in {damage:?}");
            }
        }
    }

    /// What rasterizing a whole 1440 × 900 point window takes, by kind of
    /// op: `cargo test --release -p sidestep-ui timing_whole -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn timing_whole_window_rasterizing() {
        use sidestep_engine::protocol::Op;
        let size = Size::new(1440.0, 900.0);
        for scale in [1.0, 2.0] {
            let ops = plain_window(size, scale);
            let (w, h) = ((size.width * scale) as u32, (size.height * scale) as u32);
            let mut px = vec![0u32; (w * h) as usize];
            let all = protocol::Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
            let mut glyphs = Glyphs::default();
            let mut time = |ops: &[Op]| {
                sidestep_engine::backend::median(|| {
                    let mut canvas = raster::Canvas::new(&mut px, w, h, 0.0, scale as f32);
                    raster::paint(&mut canvas, &mut glyphs, &[all], ops);
                })
            };
            let kind = |f: fn(&Op) -> bool| ops.iter().filter(|op| f(op)).cloned().collect::<Vec<Op>>();
            let whole = time(&ops);
            let fills = time(&kind(|op| matches!(op, Op::Fill { .. } | Op::FillWith { .. })));
            let glyph_runs = time(&kind(|op| matches!(op, Op::Glyphs(_))));
            let paths = time(&kind(|op| matches!(op, Op::FillPath { .. })));
            let mut warm = Glyphs::default();
            let banded = sidestep_engine::backend::median(|| {
                let mut canvas = raster::Canvas::new(&mut px, w, h, 0.0, scale as f32);
                raster::bands::paint_banded(&mut canvas, &mut warm, &[all], &ops);
            });
            println!(
                "{w} × {h} pixels: {whole:.2} ms ({banded:.2} ms in bands); fills {fills:.2} ms, {} glyph runs {glyph_runs:.2} ms, paths {paths:.2} ms",
                kind(|op| matches!(op, Op::Glyphs(_))).len()
            );
        }
    }
}
