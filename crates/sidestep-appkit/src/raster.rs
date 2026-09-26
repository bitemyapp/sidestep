//! CPU rasterization of recorded ops into XRGB8888 pixels, touching only the
//! damaged rectangles. Runs on the render thread.

use crate::protocol::{Color, Op, Rect};
pub(crate) use crate::text::Glyphs;

/// Pixels of a layer (or of one tile of it), `origin_y` being the layer
/// coordinate of the first row.
pub(crate) struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub origin_y: f32,
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

/// Integer pixel bounds of `r` inside the canvas, or `None` if empty.
fn pixels(canvas: &Canvas, r: &Rect) -> Option<(usize, usize, usize, usize)> {
    let r = r.translate(0.0, -canvas.origin_y).round_out();
    let x0 = r.x0.max(0.0) as usize;
    let y0 = r.y0.max(0.0) as usize;
    let x1 = (r.x1.min(canvas.width as f32)).max(0.0) as usize;
    let y1 = (r.y1.min(canvas.height as f32)).max(0.0) as usize;
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
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

fn fill(canvas: &mut Canvas, r: &Rect, color: Color) {
    let Some((x0, y0, x1, y1)) = pixels(canvas, r) else { return };
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
    let area = bounds.round_out().intersect(clip);
    let Some((x0, y0, x1, y1)) = pixels(canvas, &area) else { return };
    let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let Some(mut pm) = tiny_skia::Pixmap::new(w, h) else { return };
    let (ox, oy) = (x0 as f32, y0 as f32 + canvas.origin_y);
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(points[0][0] - ox, points[0][1] - oy);
    for p in &points[1..] {
        pb.line_to(p[0] - ox, p[1] - oy);
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
