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

use tiny_skia::{Mask, PixmapMut, Transform};

use crate::protocol::{GradientSpec, ShadowSpec};

/// A thread's shadow buffers, reused: the coverage, its alpha, and a
/// spare for the blur.
#[derive(Default)]
struct Scratch {
    cover: Vec<u32>,
    alpha: Vec<u8>,
    tmp: Vec<u8>,
}

thread_local!(static SCRATCH: RefCell<Scratch> = RefCell::default());

/// How far (pixels) a shadow blurred by `blur` pixels reaches beyond its
/// shape.
pub(crate) fn reach(blur: f32) -> f32 {
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

/// Blur `a` (`w` × `h` coverage) in place with a box of `size` pixels,
/// along rows, then along columns.
fn box_blur(a: &mut [u8], tmp: &mut Vec<u8>, w: usize, h: usize, size: usize) {
    if size <= 1 {
        return;
    }
    let r = size / 2;
    tmp.clear();
    tmp.resize(a.len(), 0);
    let pass = |src: &[u8], dst: &mut [u8], len: usize, lines: usize, at: &dyn Fn(usize, usize) -> usize| {
        for line in 0..lines {
            let mut sum: u32 = 0;
            // The window [i - r, i + r], zeros outside.
            for i in 0..=r.min(len - 1) {
                sum += u32::from(src[at(line, i)]);
            }
            for i in 0..len {
                dst[at(line, i)] = ((sum + size as u32 / 2) / size as u32) as u8;
                if i + r + 1 < len {
                    sum += u32::from(src[at(line, i + r + 1)]);
                }
                if i >= r {
                    sum -= u32::from(src[at(line, i - r)]);
                }
            }
        }
    };
    pass(a, tmp, w, h, &|line, i| line * w + i);
    pass(tmp, a, h, w, &|line, i| i * w + line);
}

/// Draw `spec`'s shadow of a shape under what `pm` (a region of the
/// canvas) holds, inside `mask`. `coverage` draws the shape (in any
/// opaque color) onto a transparent pixmap, with the region's transform
/// followed by the one it's given, which places the pixels that can cast
/// shadow into the region.
pub(crate) fn shadow(
    pm: &mut PixmapMut,
    spec: &ShadowSpec,
    scale: f32,
    mask: Option<&Mask>,
    coverage: impl FnOnce(&mut PixmapMut, Transform),
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
        s.cover.clear();
        s.cover.resize(ww * wh, 0);
        let Some(mut target) = PixmapMut::from_bytes(super::as_bytes(&mut s.cover), ww as u32, wh as u32) else {
            return;
        };
        coverage(&mut target, Transform::from_translate((r as isize + dx) as f32, (r as isize + dy) as f32));
        s.alpha.clear();
        s.alpha.extend(s.cover.iter().map(|p| p.to_ne_bytes()[3]));
        if blur > 0.0 {
            for size in boxes(blur / 2.0) {
                box_blur(&mut s.alpha, &mut s.tmp, ww, wh, size);
            }
        }
        let c = spec.color.map(|v| v.clamp(0.0, 1.0));
        let mask = mask.map(Mask::data);
        for (i, px) in pm.data_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            let mut cov = u32::from(s.alpha[(y + r) * ww + x + r]);
            if let Some(m) = mask {
                cov = (cov * u32::from(m[i]) + 127) / 255;
            }
            if cov == 0 {
                continue;
            }
            let a = cov as f32 / 255.0 * c[3];
            let ch = |v: f32| (v * a * 255.0 + 0.5) as u8;
            let shade = u32::from_ne_bytes([ch(c[0]), ch(c[1]), ch(c[2]), (a * 255.0 + 0.5) as u8]);
            *px = super::over(u32::from_ne_bytes(*px), shade).to_ne_bytes();
        }
    });
}

/// The shader for a gradient in user space.
pub(crate) fn gradient(g: &GradientSpec) -> Option<tiny_skia::Shader<'static>> {
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
        let mut tmp = Vec::new();
        let mut spread = vec![0u8; w * h];
        spread[20 * w + 18..20 * w + 23].fill(255);
        for size in boxes(2.0) {
            box_blur(&mut spread, &mut tmp, w, h, size);
        }
        assert_eq!(spread[20 * w + 10], spread[20 * w + 30], "symmetric");
        assert!(spread[20 * w + 20] > spread[20 * w + 24] && spread[20 * w + 24] > spread[20 * w + 27]);
        assert_eq!(spread[20 * w + 38], 0, "and bounded");
    }
}
