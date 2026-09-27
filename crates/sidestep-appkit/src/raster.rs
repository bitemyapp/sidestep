//! CPU rasterization of recorded ops, touching only the damaged rectangles.
//! It runs on any thread: the render thread for windows, and the drawing
//! thread for bitmap contexts (`context`), each with its own caches.
//!
//! Canvases hold premultiplied RGBA, tiny-skia's format: a pixel is a
//! `u32` whose bytes in memory are red, green, blue and alpha, which is
//! what wl_shm calls ABGR8888. Presenting copies into buffers of that
//! format as they are, or swizzles to XRGB8888 in the same pass where the
//! compositor lacks it ([`to_xrgb`]).
//!
//! Ops are in points; a canvas has `scale` pixels per point. A fill covers
//! the pixels whose centers lie inside it, and so does a damaged rectangle
//! or a clip, so a pixel's value never depends on how the damage was cut
//! up, at any scale. Plain fills are drawn here and glyphs in
//! `text::raster`; everything else (paths, strokes, images, gradients,
//! blend modes, groups, shadows) in [`ops`].

pub(crate) mod effects;
pub(crate) mod images;
pub(crate) mod ops;
pub(crate) mod pixels;

use crate::protocol::{Color, Op, Rect};
pub(crate) use crate::text::Glyphs;

/// Pixels of a layer (or of one tile of it, or of a group being drawn),
/// `origin_y` being the layer coordinate (points) of the first row when
/// `y0` is 0. `width` and `height` count pixels.
pub(crate) struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    /// Pixels from one row to the next, at least `width`.
    pub stride: usize,
    pub origin_y: f32,
    /// Pixels per point.
    pub scale: f32,
    /// Where the canvas's top left pixel is on the layer's pixel grid, for
    /// a canvas holding part of it (a group being drawn); 0 otherwise.
    pub x0: i32,
    pub y0: i32,
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u32], width: u32, height: u32, origin_y: f32, scale: f32) -> Self {
        Canvas { px, width, height, stride: width as usize, origin_y, scale, x0: 0, y0: 0 }
    }

    /// The same pixels, borrowed again.
    pub fn reborrow(&mut self) -> Canvas<'_> {
        Canvas { px: self.px, ..*self }
    }

    /// Layer points to this canvas's pixels.
    pub fn transform(&self) -> tiny_skia::Transform {
        let s = self.scale;
        tiny_skia::Transform::from_row(s, 0.0, 0.0, s, -(self.x0 as f32), -(self.origin_y * s) - self.y0 as f32)
    }

    /// The pixels whose centers lie in `r` (layer points), within the
    /// canvas: x0, y0, x1, y1.
    pub fn pixels(&self, r: &Rect) -> Option<(usize, usize, usize, usize)> {
        let edge = |v: f32| (v - 0.5).ceil();
        self.pixels_by(r, edge, edge)
    }

    /// The pixels a clip lets drawing reach: those whose centers its
    /// rectangle takes in, or with clip paths (whose antialiased edges
    /// reach into pixels their rectangle doesn't take the centers of),
    /// every pixel it touches; the paths' mask says how much of each.
    pub fn clip_pixels(&self, draw: &crate::protocol::Draw, damage: &Rect) -> Option<(usize, usize, usize, usize)> {
        let r = draw.clip.intersect(damage);
        if draw.mask.is_some() { self.pixels_by(&r, f32::floor, f32::ceil) } else { self.pixels(&r) }
    }

    fn pixels_by(
        &self,
        r: &Rect,
        low: impl Fn(f32) -> f32,
        high: impl Fn(f32) -> f32,
    ) -> Option<(usize, usize, usize, usize)> {
        let s = self.scale;
        let x0 = low(r.x0 * s - self.x0 as f32).max(0.0);
        let x1 = high(r.x1 * s - self.x0 as f32).min(self.width as f32);
        let y0 = low((r.y0 - self.origin_y) * s - self.y0 as f32).max(0.0);
        let y1 = high((r.y1 - self.origin_y) * s - self.y0 as f32).min(self.height as f32);
        (x0 < x1 && y0 < y1).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
    }
}

/// `c` (straight RGBA) as a canvas pixel.
#[inline]
pub(crate) fn premultiplied(c: Color) -> u32 {
    let a = c[3].clamp(0.0, 1.0);
    let ch = |v: f32| (v.clamp(0.0, 1.0) * a * 255.0 + 0.5) as u8;
    u32::from_ne_bytes([ch(c[0]), ch(c[1]), ch(c[2]), (a * 255.0 + 0.5) as u8])
}

/// A canvas pixel's bytes: red, green, blue, alpha (premultiplied).
#[inline]
pub(crate) fn channels(p: u32) -> [u8; 4] {
    p.to_ne_bytes()
}

/// A canvas pixel as XRGB8888 (in memory: blue, green, red, then the
/// alpha, which that format ignores).
#[inline]
pub(crate) fn to_xrgb(p: u32) -> u32 {
    let [r, g, b, a] = p.to_ne_bytes();
    u32::from_ne_bytes([b, g, r, a])
}

/// Pixels as the bytes tiny-skia reads and writes.
pub(crate) fn as_bytes(px: &mut [u32]) -> &mut [u8] {
    // SAFETY: a u32 is four initialized bytes with no stricter alignment
    // than u8's, and the length covers exactly the same memory.
    unsafe { std::slice::from_raw_parts_mut(px.as_mut_ptr().cast::<u8>(), px.len() * 4) }
}

/// Mix every byte of `a` and `b`: `a · (255 − t) + b · t`, rounded as
/// dividing by 255 would. Red and blue share a multiply, green and alpha
/// another; each lane's sum stays below 2¹⁶.
#[inline]
pub(crate) fn lerp(a: u32, b: u32, t: u32) -> u32 {
    let inv = 255 - t;
    let lo = (a & 0x00ff_00ff) * inv + (b & 0x00ff_00ff) * t + 0x0080_0080;
    let hi = ((a >> 8) & 0x00ff_00ff) * inv + ((b >> 8) & 0x00ff_00ff) * t + 0x0080_0080;
    let lo = ((lo + ((lo >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let hi = (hi + ((hi >> 8) & 0x00ff_00ff)) & 0xff00_ff00;
    lo | hi
}

/// Opaque `solid` over `dst` with coverage `a` (0 to 255).
#[inline]
pub(crate) fn blend(dst: u32, solid: u32, a: u32) -> u32 {
    lerp(dst, solid, a)
}

/// `src` (premultiplied) over `dst`, each channel rounded.
#[inline]
pub(crate) fn over(dst: u32, src: u32) -> u32 {
    let inv = 255 - u32::from(src.to_ne_bytes()[3]);
    let lo = (dst & 0x00ff_00ff) * inv + 0x0080_0080;
    let hi = ((dst >> 8) & 0x00ff_00ff) * inv + 0x0080_0080;
    let lo = ((lo + ((lo >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let hi = (hi + ((hi >> 8) & 0x00ff_00ff)) & 0xff00_ff00;
    // Premultiplied channels never exceed their alpha, so no byte carries.
    src + (lo | hi)
}

/// Draw `ops` into the parts of `canvas` in `rects`.
pub(crate) fn paint(canvas: &mut Canvas, glyphs: &mut Glyphs, rects: &[Rect], ops: &[Op]) {
    for rect in rects {
        ops::run(canvas, glyphs, rect, ops);
    }
}

/// A rectangle filled over what's there.
pub(crate) fn fill(canvas: &mut Canvas, r: &Rect, color: Color) {
    let a = (color[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    if a == 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = canvas.pixels(r) else { return };
    let solid = premultiplied([color[0], color[1], color[2], 1.0]);
    let stride = canvas.stride;
    for y in y0..y1 {
        let row = &mut canvas.px[y * stride + x0..y * stride + x1];
        if a >= 255 {
            row.fill(solid);
        } else {
            row.iter_mut().for_each(|p| *p = blend(*p, solid, a));
        }
    }
}

/// A rectangle's pixels replaced by `pixel`.
pub(crate) fn replace(canvas: &mut Canvas, r: &Rect, pixel: u32) {
    let Some((x0, y0, x1, y1)) = canvas.pixels(r) else { return };
    let stride = canvas.stride;
    for y in y0..y1 {
        canvas.px[y * stride + x0..y * stride + x1].fill(pixel);
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    fn canvas_with(px: &mut [u32], w: u32, h: u32, scale: f32) -> Canvas<'_> {
        Canvas::new(px, w, h, 0.0, scale)
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
                let Some((x0, y0, x1, y1)) = c.pixels(&Rect::new(x, 0.0, x + w, 4.0)) else {
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
    fn pixels_are_premultiplied_rgba_in_memory() {
        assert_eq!(channels(premultiplied([1.0, 0.5, 0.0, 1.0])), [255, 128, 0, 255]);
        assert_eq!(channels(premultiplied([1.0, 1.0, 1.0, 0.5])), [128, 128, 128, 128]);
        // XRGB8888 is blue, green, red in memory.
        assert_eq!(to_xrgb(premultiplied([1.0, 0.5, 0.0, 1.0])).to_ne_bytes(), [0, 128, 255, 255]);
    }

    #[test]
    fn compositing_rounds_like_dividing() {
        let exact_lerp = |a: [u8; 4], b: [u8; 4], t: u32| {
            let mut out = [0u8; 4];
            for i in 0..4 {
                out[i] = ((u32::from(a[i]) * (255 - t) + u32::from(b[i]) * t + 127) / 255) as u8;
            }
            out
        };
        let exact_over = |dst: [u8; 4], src: [u8; 4]| {
            let inv = 255 - u32::from(src[3]);
            let mut out = [0u8; 4];
            for i in 0..4 {
                out[i] = (u32::from(src[i]) + (u32::from(dst[i]) * inv + 127) / 255) as u8;
            }
            out
        };
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..200_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let a = (seed as u32).to_ne_bytes();
            let b = ((seed >> 32) as u32).to_ne_bytes();
            let t = (seed >> 24) as u32 % 256;
            let got = channels(lerp(u32::from_ne_bytes(a), u32::from_ne_bytes(b), t));
            assert_eq!(got, exact_lerp(a, b, t), "{a:?} {b:?} {t}");
            // Premultiplied pixels: no channel above its alpha.
            let src = [b[0].min(b[3]), b[1].min(b[3]), b[2].min(b[3]), b[3]];
            let dst = [a[0].min(a[3]), a[1].min(a[3]), a[2].min(a[3]), a[3]];
            let got = channels(over(u32::from_ne_bytes(dst), u32::from_ne_bytes(src)));
            assert_eq!(got, exact_over(dst, src), "{dst:?} over {src:?}");
        }
    }
}
