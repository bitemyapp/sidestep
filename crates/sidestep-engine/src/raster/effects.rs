//! Shadows and gradients.
//!
//! A shadow is the shape's coverage, blurred, moved by the offset, tinted
//! with the shadow's color and composited under the shape. The blur is
//! three box blurs in a row, which comes close to a Gaussian whose
//! standard deviation is half the blur radius, as `NSShadow` describes
//! its radius. The coverage is taken over all the pixels that can reach
//! the region being drawn (the region moved back by the offset, and the
//! blur's reach around it), not just the region: a shadow's pixels are the
//! same however the damage was cut, across scroll tiles too.
//!
//! Gradients become tiny-skia shaders, padded past their ends; one that
//! shouldn't extend there fills only the band it covers, which the main
//! thread makes the shape it fills (see `gradient`).

use std::cell::RefCell;

use tiny_skia::{Mask, PixmapMut, PixmapRef, Transform};

use crate::protocol::{GradientSpec, ShadowSpec};

/// A thread's shadow buffers, reused: the coverage, its alpha, a spare
/// for the blur, and its columns' running sums.
#[derive(Default)]
struct Scratch {
    cover: Vec<u32>,
    alpha: Vec<u8>,
    tmp: Vec<u8>,
    sums: Vec<u32>,
}

thread_local!(static SCRATCH: RefCell<Scratch> = RefCell::default());

/// How far (pixels) a shadow blurred by `blur` pixels reaches beyond its
/// shape.
pub fn reach(blur: f32) -> f32 {
    if blur <= 0.0 { 0.0 } else { blur * 1.5 + 2.0 }
}

/// The widths of three box blurs approximating a Gaussian of deviation
/// `sigma` (the usual construction, as in the CSS filter specification).
fn boxes(sigma: f32) -> [usize; 3] {
    let ideal = (12.0 * sigma * sigma / 3.0 + 1.0).sqrt();
    let mut lower = ideal.floor() as usize;
    if lower.is_multiple_of(2) {
        lower = lower.saturating_sub(1);
    }
    let upper = lower + 2;
    let m = ((12.0 * sigma * sigma - 3.0 * (lower * lower) as f32 - 12.0 * lower as f32 - 9.0)
        / (-4.0 * lower as f32 - 4.0))
        .round() as usize;
    [0, 1, 2].map(|i| if i < m { lower } else { upper })
}

/// Blur `a` (`w` × `h` coverage) in place with a box of `size` pixels (odd),
/// along rows, then along columns: each pixel the rounded mean of the
/// `size` around it, zeros outside. The columns go a row at a time with a
/// running sum per column, so both passes read memory in order.
fn box_blur(a: &mut [u8], tmp: &mut Vec<u8>, sums: &mut Vec<u32>, w: usize, h: usize, size: usize) {
    if size <= 1 || w == 0 || h == 0 {
        return;
    }
    let r = size / 2;
    let mean = Mean::new(size as u32);
    tmp.clear();
    tmp.resize(a.len(), 0);
    for (src, dst) in a.chunks_exact(w).zip(tmp.chunks_exact_mut(w)) {
        let mut sum: u32 = src[..=r.min(w - 1)].iter().map(|&v| u32::from(v)).sum();
        for (i, out) in dst.iter_mut().enumerate() {
            *out = mean.of(sum);
            if i + r + 1 < w {
                sum += u32::from(src[i + r + 1]);
            }
            if i >= r {
                sum -= u32::from(src[i - r]);
            }
        }
    }
    sums.clear();
    sums.resize(w, 0);
    for row in tmp.chunks_exact(w).take(r + 1) {
        sums.iter_mut().zip(row).for_each(|(s, &v)| *s += u32::from(v));
    }
    for y in 0..h {
        a[y * w..(y + 1) * w].iter_mut().zip(sums.iter()).for_each(|(out, &s)| *out = mean.of(s));
        if y + r + 1 < h {
            sums.iter_mut().zip(&tmp[(y + r + 1) * w..(y + r + 2) * w]).for_each(|(s, &v)| *s += u32::from(v));
        }
        if y >= r {
            sums.iter_mut().zip(&tmp[(y - r) * w..(y - r + 1) * w]).for_each(|(s, &v)| *s -= u32::from(v));
        }
    }
}

/// The rounded mean of `n` coverages from their sum, `(sum + n / 2) / n`,
/// as a multiply and a shift: exact for every sum `n` bytes can make, as
/// the error `m · n − 2⁴⁰` (at most `n`) times a sum (under `256 · n`) stays
/// under 2⁴⁰ for any box narrower than 65,536 pixels.
#[derive(Clone, Copy)]
struct Mean {
    half: u32,
    m: u64,
}

impl Mean {
    fn new(n: u32) -> Mean {
        Mean { half: n / 2, m: (1u64 << 40) / u64::from(n) + 1 }
    }

    #[inline]
    fn of(self, sum: u32) -> u8 {
        ((u64::from(sum + self.half) * self.m) >> 40) as u8
    }
}

/// Draw `spec`'s shadow of a shape under what `pm` (a region of the
/// canvas) holds, inside `mask`. `coverage` draws the shape (in any
/// opaque color) onto a transparent pixmap, with the region's transform
/// followed by the one it's given, which places the pixels that can cast
/// shadow into the region.
pub fn shadow(
    pm: &mut PixmapMut,
    spec: &ShadowSpec,
    scale: f32,
    mask: Option<&Mask>,
    coverage: impl FnOnce(&mut PixmapMut, Transform),
) {
    cast(pm, spec, scale, mask, |s, (ww, wh), (ox, oy)| {
        s.cover.clear();
        s.cover.resize(ww * wh, 0);
        let Some(mut target) = PixmapMut::from_bytes(super::as_bytes(&mut s.cover), ww as u32, wh as u32) else {
            return false;
        };
        coverage(&mut target, Transform::from_translate(ox as f32, oy as f32));
        s.alpha.clear();
        s.alpha.extend(s.cover.iter().map(|p| p.to_ne_bytes()[3]));
        true
    });
}

/// As [`shadow`], cast by premultiplied pixels (a group's) whose top left
/// is at `at` in the region: their alpha is the coverage, taken as it is
/// rather than drawn again.
pub fn shadow_of(
    pm: &mut PixmapMut,
    spec: &ShadowSpec,
    scale: f32,
    mask: Option<&Mask>,
    src: PixmapRef,
    at: (i32, i32),
) {
    cast(pm, spec, scale, mask, |s, (ww, wh), (ox, oy)| {
        s.alpha.clear();
        s.alpha.resize(ww * wh, 0);
        let (sw, sh) = (src.width() as isize, src.height() as isize);
        // Source pixel (x, y) is the window's (x + dx, y + dy).
        let (dx, dy) = (at.0 as isize + ox, at.1 as isize + oy);
        let (x0, x1) = (0.max(-dx), sw.min(ww as isize - dx));
        if x0 >= x1 {
            return true;
        }
        let data = src.data();
        for y in 0.max(-dy)..sh.min(wh as isize - dy) {
            let from = &data[((y * sw + x0) * 4) as usize..((y * sw + x1) * 4) as usize];
            let to = (y + dy) as usize * ww + (x0 + dx) as usize;
            for (a, p) in s.alpha[to..to + (x1 - x0) as usize].iter_mut().zip(from.as_chunks::<4>().0) {
                *a = p[3];
            }
        }
        true
    });
}

/// The shadow, from `coverage`, which fills the scratch's alpha (a window
/// the size it is given) with what casts it, offset as it is given.
fn cast(
    pm: &mut PixmapMut,
    spec: &ShadowSpec,
    scale: f32,
    mask: Option<&Mask>,
    coverage: impl FnOnce(&mut Scratch, (usize, usize), (isize, isize)) -> bool,
) {
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    let blur = spec.blur * scale;
    // Moved by the offset (whole pixels; shadows are soft anyway).
    let (dx, dy) = ((spec.dx * scale).round() as isize, (spec.dy * scale).round() as isize);
    // The coverage that reaches the region: region pixel (x, y) shows the
    // blurred coverage at (x − dx, y − dy), which the blur takes from `r`
    // pixels around. The window puts that at (x + r, y + r).
    let r = reach(blur).ceil() as usize;
    let (ww, wh) = (w + 2 * r, h + 2 * r);
    SCRATCH.with(|scratch| {
        let mut fresh = Scratch::default();
        let mut held = scratch.try_borrow_mut();
        let s = match held.as_deref_mut() {
            Ok(s) => s,
            Err(_) => &mut fresh,
        };
        if !coverage(s, (ww, wh), (r as isize + dx, r as isize + dy)) {
            return;
        }
        if blur > 0.0 {
            for size in boxes(blur / 2.0) {
                box_blur(&mut s.alpha, &mut s.tmp, &mut s.sums, ww, wh, size);
            }
        }
        let c = spec.color.map(|v| v.clamp(0.0, 1.0));
        // The shadow's pixel at each coverage, worked out once.
        let shades: [u32; 256] = std::array::from_fn(|cov| {
            let a = cov as f32 / 255.0 * c[3];
            let ch = |v: f32| (v * a * 255.0 + 0.5) as u8;
            u32::from_ne_bytes([ch(c[0]), ch(c[1]), ch(c[2]), (a * 255.0 + 0.5) as u8])
        });
        let mask = mask.map(Mask::data);
        for (y, row) in pm.data_mut().as_chunks_mut::<4>().0.chunks_exact_mut(w).enumerate() {
            let covs = &s.alpha[(y + r) * ww + r..(y + r) * ww + r + w];
            for (x, (px, &cov)) in row.iter_mut().zip(covs).enumerate() {
                let mut cov = u32::from(cov);
                if let Some(m) = mask {
                    cov = (cov * u32::from(m[y * w + x]) + 127) / 255;
                }
                if cov != 0 {
                    *px = super::over(u32::from_ne_bytes(*px), shades[cov as usize]).to_ne_bytes();
                }
            }
        }
    });
}

/// The shader for a gradient in user space.
pub fn gradient(g: &GradientSpec) -> Option<tiny_skia::Shader<'static>> {
    let stops: Vec<tiny_skia::GradientStop> =
        g.stops.iter().map(|&(at, c)| tiny_skia::GradientStop::new(at.clamp(0.0, 1.0), super::ops::color(c))).collect();
    let (start, end) = (tiny_skia::Point::from_xy(g.start.0, g.start.1), tiny_skia::Point::from_xy(g.end.0, g.end.1));
    match g.radii {
        None => tiny_skia::LinearGradient::new(start, end, stops, tiny_skia::SpreadMode::Pad, Transform::identity()),
        Some((r0, r1)) => tiny_skia::RadialGradient::new(
            start,
            r0.max(0.0),
            end,
            r1.max(0.0),
            stops,
            tiny_skia::SpreadMode::Pad,
            Transform::identity(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_boxes_blur_like_a_gaussian() {
        // A lit bar spreads symmetrically, fading with distance.
        let (w, h) = (41, 41);
        let (mut tmp, mut sums) = (Vec::new(), Vec::new());
        let mut spread = vec![0u8; w * h];
        spread[20 * w + 18..20 * w + 23].fill(255);
        for size in boxes(2.0) {
            box_blur(&mut spread, &mut tmp, &mut sums, w, h, size);
        }
        assert_eq!(spread[20 * w + 10], spread[20 * w + 30], "symmetric");
        assert!(spread[20 * w + 20] > spread[20 * w + 24] && spread[20 * w + 24] > spread[20 * w + 27]);
        assert_eq!(spread[20 * w + 38], 0, "and bounded");
    }

    /// The blur as defined: each pixel the rounded mean of the `size` around
    /// it along a row, then along a column, zeros outside.
    fn plain_blur(a: &mut [u8], w: usize, h: usize, size: usize) {
        let r = size as isize / 2;
        let mean = |sum: u32| ((sum + size as u32 / 2) / size as u32) as u8;
        let at = |a: &[u8], x: isize, y: isize| {
            if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
                0
            } else {
                u32::from(a[y as usize * w + x as usize])
            }
        };
        let rows: Vec<u8> =
            (0..w * h).map(|i| mean((-r..=r).map(|d| at(a, (i % w) as isize + d, (i / w) as isize)).sum())).collect();
        for (i, out) in a.iter_mut().enumerate() {
            *out = mean((-r..=r).map(|d| at(&rows, (i % w) as isize, (i / w) as isize + d)).sum());
        }
    }

    #[test]
    fn box_blurs_are_exact_means() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut tmp, mut sums) = (Vec::new(), Vec::new());
        for (w, h) in [(1, 1), (3, 70), (61, 5), (97, 43), (128, 128)] {
            for size in [1, 3, 5, 9, 33, 129, 201] {
                // Coverage as shadows see it: runs of solid, edges, nothing.
                let mut a: Vec<u8> =
                    (0..w * h).map(|_| if next() % 3 == 0 { 0 } else { (next() % 256) as u8 }).collect();
                a.iter_mut().take(w * h / 3).for_each(|v| *v = 255);
                let mut want = a.clone();
                plain_blur(&mut want, w, h, size);
                box_blur(&mut a, &mut tmp, &mut sums, w, h, size);
                assert!(a == want, "{w}x{h}, box of {size}");
            }
        }
        // The means are exact for every sum, at every width a box can be.
        for n in [1u32, 3, 7, 63, 255, 1023, 4095, 65_535] {
            let m = Mean::new(n);
            for sum in (0..=255 * n).step_by((n as usize / 64).max(1)).chain([255 * n]) {
                assert_eq!(u32::from(m.of(sum)), (sum + n / 2) / n, "sum {sum} of {n}");
            }
        }
    }
}
