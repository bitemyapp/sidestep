//! CPU rasterization of recorded ops into XRGB8888 pixels, touching only the
//! damaged rectangles. Runs on the render thread.
//!
//! Ops are in points; a canvas has `scale` pixels per point. A fill covers
//! the pixels whose centers lie inside it, and so does a damaged rectangle
//! or a clip, so a pixel's value never depends on how the damage was cut
//! up, at any scale.

use crate::protocol::{Color, Op, Rect};
pub(crate) use crate::text::Glyphs;

/// Pixels of a layer (or of one tile of it), `origin_y` being the layer
/// coordinate (points) of the first row. `width` and `height` count
/// pixels.
pub(crate) struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub origin_y: f32,
    /// Pixels per point.
    pub scale: f32,
}

fn channels(c: Color) -> ([u32; 3], u32) {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    ([ch(c[0]), ch(c[1]), ch(c[2])], ch(c[3]))
}

#[inline]
fn blend(dst: u32, rgb: [u32; 3], a: u32) -> u32 {
    if a >= 255 {
        return (rgb[0] << 16) | (rgb[1] << 8) | rgb[2];
    }
    let inv = 255 - a;
    let mix = |shift: u32, s: u32| ((((dst >> shift) & 0xff) * inv + s * a) + 127) / 255;
    (mix(16, rgb[0]) << 16) | (mix(8, rgb[1]) << 8) | mix(0, rgb[2])
}

pub(crate) fn paint(canvas: &mut Canvas, glyphs: &mut Glyphs, rects: &[Rect], ops: &[Op]) {
    for rect in rects {
        for op in ops {
            match op {
                Op::Fill { rect: r, color } => fill(canvas, &r.intersect(rect), *color),
                Op::Path { points, color, clip } => path(canvas, points, *color, &clip.intersect(rect)),
                Op::Glyphs(run) => crate::text::draw_glyphs(canvas, glyphs, run, &run.clip.intersect(rect)),
            }
        }
    }
}

/// The pixels whose centers lie in `r` (layer points), within the canvas.
fn device_pixels(canvas: &Canvas, r: &Rect) -> Option<(usize, usize, usize, usize)> {
    let s = canvas.scale;
    let edge = |v: f32| (v * s - 0.5).ceil();
    let x0 = edge(r.x0).max(0.0);
    let x1 = edge(r.x1).min(canvas.width as f32);
    let y0 = edge(r.y0 - canvas.origin_y).max(0.0);
    let y1 = edge(r.y1 - canvas.origin_y).min(canvas.height as f32);
    (x0 < x1 && y0 < y1).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
}

fn fill(canvas: &mut Canvas, r: &Rect, color: Color) {
    let Some((x0, y0, x1, y1)) = device_pixels(canvas, r) else { return };
    let (rgb, a) = channels(color);
    let w = canvas.width as usize;
    for y in y0..y1 {
        let row = &mut canvas.px[y * w + x0..y * w + x1];
        if a >= 255 {
            row.fill((rgb[0] << 16) | (rgb[1] << 8) | rgb[2]);
        } else {
            row.iter_mut().for_each(|p| *p = blend(*p, rgb, a));
        }
    }
}

fn path(canvas: &mut Canvas, points: &[[f32; 2]], color: Color, clip: &Rect) {
    if points.len() < 3 {
        return;
    }
    let bounds = points
        .iter()
        .fold(Rect::new(f32::MAX, f32::MAX, f32::MIN, f32::MIN), |r, p| r.union(&Rect::new(p[0], p[1], p[0], p[1])));
    // The pixels the path can touch (antialiasing included), in the clip.
    let (s, oy) = (canvas.scale, canvas.origin_y);
    let touched = Rect::new(bounds.x0 * s, (bounds.y0 - oy) * s, bounds.x1 * s, (bounds.y1 - oy) * s).round_out();
    let Some((cx0, cy0, cx1, cy1)) = device_pixels(canvas, clip) else { return };
    let x0 = (touched.x0.max(0.0) as usize).max(cx0);
    let y0 = (touched.y0.max(0.0) as usize).max(cy0);
    let x1 = (touched.x1.max(0.0) as usize).min(cx1);
    let y1 = (touched.y1.max(0.0) as usize).min(cy1);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let Some(mut pm) = tiny_skia::Pixmap::new(w, h) else { return };
    // Points to this pixmap's pixels.
    let at = |p: &[f32; 2]| (p[0] * s - x0 as f32, (p[1] - oy) * s - y0 as f32);
    let mut pb = tiny_skia::PathBuilder::new();
    let (mx, my) = at(&points[0]);
    pb.move_to(mx, my);
    for p in &points[1..] {
        let (x, y) = at(p);
        pb.line_to(x, y);
    }
    pb.close();
    let Some(shape) = pb.finish() else { return };
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(
        tiny_skia::Color::from_rgba(color[0], color[1], color[2], color[3]).unwrap_or(tiny_skia::Color::BLACK),
    );
    paint.anti_alias = true;
    pm.fill_path(&shape, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    let stride = canvas.width as usize;
    for (i, p) in pm.pixels().iter().enumerate() {
        let a = p.alpha() as u32;
        if a == 0 {
            continue;
        }
        let (x, y) = (x0 + i % w as usize, y0 + i / w as usize);
        let d = &mut canvas.px[y * stride + x];
        // Premultiplied source over the destination.
        let inv = 255 - a;
        let mix = |shift: u32, s: u8| s as u32 + (((*d >> shift) & 0xff) * inv + 127) / 255;
        *d = (mix(16, p.red()) << 16) | (mix(8, p.green()) << 8) | mix(0, p.blue());
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    fn canvas_with(px: &mut [u32], w: u32, h: u32, scale: f32) -> Canvas<'_> {
        Canvas { px, width: w, height: h, origin_y: 0.0, scale }
    }

    fn covered(px: &[u32]) -> usize {
        px.iter().filter(|&&p| p != 0).count()
    }

    #[test]
    fn fills_scale_to_pixels() {
        let mut px = vec![0u32; 40 * 40];
        let mut c = canvas_with(&mut px, 40, 40, 2.0);
        fill(&mut c, &Rect::new(1.0, 1.0, 5.0, 3.0), [1.0, 1.0, 1.0, 1.0]);
        // 4 × 2 points at 2 pixels a point.
        assert_eq!(covered(&px), 8 * 4);
        assert_ne!(px[2 * 40 + 2], 0);
        assert_eq!(px[40 + 1], 0);
    }

    #[test]
    fn neighbours_tile_without_gaps_at_fractional_scales() {
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0, 2.5] {
            let mut px = vec![0u32; 64 * 8];
            let c = canvas_with(&mut px, 64, 8, scale);
            // Adjacent fills, one after another, each adding one to what's there.
            let mut x = 0.0;
            for w in [1.0, 2.5, 0.7, 3.3, 4.0, 1.5] {
                let Some((x0, y0, x1, y1)) = device_pixels(&c, &Rect::new(x, 0.0, x + w, 4.0)) else {
                    x += w;
                    continue;
                };
                for y in y0..y1 {
                    for p in &mut c.px[y * 64 + x0..y * 64 + x1] {
                        *p += 1;
                    }
                }
                x += w;
            }
            // Every pixel whose center is inside the run, exactly once.
            let row = &px[..((x * scale - 0.5).ceil() as usize).min(64)];
            assert!(row.iter().all(|&p| p == 1), "scale {scale}: {row:?}");
        }
    }

    #[test]
    fn paths_scale() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = canvas_with(&mut px, 20, 20, 2.0);
        let square = [[1.0, 1.0], [6.0, 1.0], [6.0, 6.0], [1.0, 6.0]];
        path(&mut c, &square, [1.0, 1.0, 1.0, 1.0], &Rect::new(0.0, 0.0, 10.0, 10.0));
        // 5 × 5 points is 10 × 10 pixels.
        assert_eq!(covered(&px), 100);
    }
}
