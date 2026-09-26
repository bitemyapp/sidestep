//! The one door the theme paints through: rectangles, rounded rectangles,
//! ellipses, strokes, arcs and text, in the coordinates of the view being
//! drawn, recorded for the render thread.
//!
//! Today's recorder knows axis-aligned fills and filled polygons, so
//! curves are flattened here, finely enough for a 2× display, and a stroke
//! is the ring between two outlines: one polygon that runs round the outer
//! edge, crosses to the inner edge and runs back round it the other way,
//! whose crossing cancels itself under the non-zero rule. Painters outside
//! this module never touch `Op` or the recorder, so when the recorder
//! learns shapes of its own only this file changes.
//!
//! Corner radii are given as the corners are seen, top left first and
//! clockwise, whichever way the view is flipped: shapes are built after
//! mapping to the layer, whose y axis points down.

use objc2_foundation::{NSPoint, NSRect};

use crate::graphics::{self, with_recorder};
use crate::protocol::{Color, Op, Rect};
use crate::string_drawing::{self, Place};
use crate::text::layout::{Attrs, Run};

/// Radii of the top-left, top-right, bottom-right and bottom-left corners.
pub(crate) type Radii = [f64; 4];

/// The same radius at every corner.
pub(crate) fn radii(r: f64) -> Radii {
    [r; 4]
}

/// Fill `r` (view coordinates) with `color`: crisp edges on whole pixels.
pub(crate) fn fill_rect(r: NSRect, color: Color) {
    if color[3] <= 0.0 {
        return;
    }
    with_recorder(|rec| {
        let rect = rec.xf.rect(r).intersect(&rec.clip);
        if !rect.is_empty() {
            rec.ops.push(Op::Fill { rect, color });
        }
    });
}

/// Fill `r` with rounded corners, antialiased.
pub(crate) fn fill_round_rect(r: NSRect, radii: Radii, color: Color) {
    if color[3] <= 0.0 || r.size.width <= 0.0 || r.size.height <= 0.0 {
        return;
    }
    if radii.iter().all(|&r| r <= 0.0) {
        return fill_rect(r, color);
    }
    with_recorder(|rec| {
        let area = rec.xf.rect(r);
        let mut points = Vec::new();
        round_rect(&mut points, area, radii, false);
        push_path(rec, points, color);
    });
}

/// Stroke the inside edge of `r`, `width` points thick, with rounded
/// corners.
pub(crate) fn stroke_round_rect(r: NSRect, radii: Radii, width: f64, color: Color) {
    if color[3] <= 0.0 || width <= 0.0 || r.size.width <= 0.0 || r.size.height <= 0.0 {
        return;
    }
    with_recorder(|rec| {
        let outer = rec.xf.rect(r);
        let w = width as f32;
        let inner = Rect::new(outer.x0 + w, outer.y0 + w, outer.x1 - w, outer.y1 - w);
        let mut points = Vec::new();
        round_rect(&mut points, outer, radii, false);
        if !inner.is_empty() {
            let inner_radii = radii.map(|r| (r - width).max(0.0));
            ring(&mut points, |p| round_rect(p, inner, inner_radii, true));
        }
        push_path(rec, points, color);
    });
}

/// Fill the ellipse inscribed in `r`.
pub(crate) fn fill_ellipse(r: NSRect, color: Color) {
    if color[3] <= 0.0 {
        return;
    }
    with_recorder(|rec| {
        let area = rec.xf.rect(r);
        let mut points = Vec::new();
        ellipse(&mut points, area, false);
        push_path(rec, points, color);
    });
}

/// Stroke the inside edge of the ellipse inscribed in `r`.
pub(crate) fn stroke_ellipse(r: NSRect, width: f64, color: Color) {
    if color[3] <= 0.0 || width <= 0.0 {
        return;
    }
    with_recorder(|rec| {
        let outer = rec.xf.rect(r);
        let w = width as f32;
        let inner = Rect::new(outer.x0 + w, outer.y0 + w, outer.x1 - w, outer.y1 - w);
        let mut points = Vec::new();
        ellipse(&mut points, outer, false);
        if !inner.is_empty() {
            ring(&mut points, |p| ellipse(p, inner, true));
        }
        push_path(rec, points, color);
    });
}

/// Stroke a line through `points`, `width` points wide, with mitred
/// joins and square ends, as check marks and chevrons are drawn.
pub(crate) fn stroke_polyline(points: &[NSPoint], width: f64, color: Color) {
    if color[3] <= 0.0 || points.len() < 2 || width <= 0.0 {
        return;
    }
    with_recorder(|rec| {
        let pts: Vec<[f32; 2]> = points
            .iter()
            .map(|p| {
                let (x, y) = rec.xf.point(p.x, p.y);
                [x as f32, y as f32]
            })
            .collect();
        let outline = offset_outline(&pts, width as f32 / 2.0);
        push_path(rec, outline, color);
    });
}

/// Stroke an arc of the circle at `center` with `radius` (to the middle of
/// the line), from `start` through `sweep` radians, clockwise on screen,
/// `width` points wide with round ends. Angles start at the top.
pub(crate) fn stroke_arc(center: NSPoint, radius: f64, start: f64, sweep: f64, width: f64, color: Color) {
    if color[3] <= 0.0 || sweep.abs() < 1e-3 || radius <= 0.0 {
        return;
    }
    with_recorder(|rec| {
        let (cx, cy) = rec.xf.point(center.x, center.y);
        let (cx, cy, r, half) = (cx as f32, cy as f32, radius as f32, width as f32 / 2.0);
        let steps = segments(r + half, sweep.abs() as f32);
        let at = |angle: f32, radius: f32| [cx + radius * angle.sin(), cy - radius * angle.cos()];
        let (a0, a1) = (start as f32, (start + sweep) as f32);
        let mut points = Vec::with_capacity(2 * steps + 16);
        for i in 0..=steps {
            points.push(at(a0 + (a1 - a0) * i as f32 / steps as f32, r + half));
        }
        // A round end, then back along the inside.
        cap(&mut points, at(a1, r), half, a1, 1.0);
        for i in (0..=steps).rev() {
            points.push(at(a0 + (a1 - a0) * i as f32 / steps as f32, r - half));
        }
        cap(&mut points, at(a0, r), half, a0, -1.0);
        push_path(rec, points, color);
    });
}

/// Draw text with one set of attributes into `r`, clipped to it, as
/// `drawInRect:` places it.
pub(crate) fn text(text: &str, attrs: &Attrs, r: NSRect) {
    string_drawing::draw(
        text,
        std::slice::from_ref(attrs),
        &[Run { start: 0, end: text.len(), attrs: 0 }],
        Place::Rect(r),
    );
}

/// Draw text with attribute runs into `r`.
pub(crate) fn text_runs(text: &str, attrs: &[Attrs], runs: &[Run], r: NSRect) {
    string_drawing::draw(text, attrs, runs, Place::Rect(r));
}

/// Run `f` with drawing clipped to `r` (view coordinates) as well.
pub(crate) fn with_clip(r: NSRect, f: impl FnOnce()) {
    let mut saved = None;
    with_recorder(|rec| {
        saved = Some(rec.clip);
        rec.clip = rec.clip.intersect(&rec.xf.rect(r).round_out());
    });
    f();
    if let Some(clip) = saved {
        with_recorder(|rec| rec.clip = clip);
    }
}

/// Whether anything is being recorded (a view is being drawn).
pub(crate) fn recording() -> bool {
    graphics::recording()
}

// Shapes, in layer coordinates (y down).

fn push_path(rec: &mut graphics::Recorder, points: Vec<[f32; 2]>, color: Color) {
    if points.len() < 3 {
        return;
    }
    let bounds = points
        .iter()
        .fold(Rect::new(f32::MAX, f32::MAX, f32::MIN, f32::MIN), |r, p| r.union(&Rect::new(p[0], p[1], p[0], p[1])));
    if bounds.intersect(&rec.clip).is_empty() && !(bounds.x0 == bounds.x1 || bounds.y0 == bounds.y1) {
        return;
    }
    rec.ops.push(Op::Path { points, color, clip: rec.clip });
}

/// Segments for an arc of `sweep` radians and radius `r` points: fine
/// enough that the chord strays under a tenth of a pixel at 2×.
fn segments(r: f32, sweep: f32) -> usize {
    // Error ≈ r·θ²/8 in pixels at 2×, per segment of angle θ.
    let theta = (0.4 / (2.0 * r.max(0.5))).sqrt().min(0.5);
    ((sweep / theta).ceil() as usize).clamp(2, 256)
}

/// Append the outline of a rounded rectangle, clockwise on screen (or
/// anticlockwise when `reverse`).
fn round_rect(out: &mut Vec<[f32; 2]>, r: Rect, radii: Radii, reverse: bool) {
    let (w, h) = (r.x1 - r.x0, r.y1 - r.y0);
    let limit = w.min(h) / 2.0;
    let rad = radii.map(|v| (v as f32).clamp(0.0, limit));
    let start = out.len();
    // Each corner: the corner itself, its radius, and the screen angle its
    // arc starts at (0 at the top, clockwise).
    use std::f32::consts::{FRAC_PI_2, PI};
    let corners = [
        ([r.x0, r.y0], rad[0], -FRAC_PI_2),
        ([r.x1, r.y0], rad[1], 0.0),
        ([r.x1, r.y1], rad[2], FRAC_PI_2),
        ([r.x0, r.y1], rad[3], PI),
    ];
    for ([x, y], radius, a0) in corners {
        if radius <= 0.0 {
            out.push([x, y]);
            continue;
        }
        // The arc's center, inside the corner.
        let (cx, cy) =
            (if x == r.x0 { x + radius } else { x - radius }, if y == r.y0 { y + radius } else { y - radius });
        let n = segments(radius, FRAC_PI_2);
        for i in 0..=n {
            let a = a0 + FRAC_PI_2 * i as f32 / n as f32;
            out.push([cx + radius * a.sin(), cy - radius * a.cos()]);
        }
    }
    if reverse {
        out[start..].reverse();
    }
}

/// Append the outline of the ellipse inscribed in `r`.
fn ellipse(out: &mut Vec<[f32; 2]>, r: Rect, reverse: bool) {
    let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
    let (rx, ry) = ((r.x1 - r.x0) / 2.0, (r.y1 - r.y0) / 2.0);
    let n = segments(rx.max(ry), std::f32::consts::TAU);
    let start = out.len();
    for i in 0..n {
        let a = std::f32::consts::TAU * i as f32 / n as f32;
        out.push([cx + rx * a.sin(), cy - ry * a.cos()]);
    }
    if reverse {
        out[start..].reverse();
    }
}

/// Close the outline in `out`, then append an inner one (going the other
/// way) and cross back, so the inside is left out under the non-zero rule.
fn ring(out: &mut Vec<[f32; 2]>, inner: impl FnOnce(&mut Vec<[f32; 2]>)) {
    let Some(&first) = out.first() else { return };
    out.push(first);
    let at = out.len();
    inner(out);
    if let Some(&inner_first) = out.get(at) {
        out.push(inner_first);
    }
    // The path closes back to `first`, over the crossing just made.
}

/// A half-circle end of radius `r` at `p` on a line heading along the arc
/// at `angle`, from the outside of the arc to the inside (`dir` 1) or back
/// (`dir` -1).
fn cap(out: &mut Vec<[f32; 2]>, p: [f32; 2], r: f32, angle: f32, dir: f32) {
    let n = segments(r, std::f32::consts::PI).max(4);
    // Starting on the outer edge, the outward normal of the circle at
    // `angle`, and bulging along the arc's clockwise direction; the start
    // cap turns both round.
    let (nx, ny) = (angle.sin() * dir, -angle.cos() * dir);
    let (tx, ty) = (angle.cos() * dir, angle.sin() * dir);
    for i in 1..n {
        let t = std::f32::consts::PI * i as f32 / n as f32;
        let (s, c) = t.sin_cos();
        out.push([p[0] + r * (nx * c + tx * s), p[1] + r * (ny * c + ty * s)]);
    }
}

/// The outline of a polyline stroked `half` points each side: along the
/// left side, then back along the right, with mitred joins.
fn offset_outline(pts: &[[f32; 2]], half: f32) -> Vec<[f32; 2]> {
    let n = pts.len();
    let dir = |a: [f32; 2], b: [f32; 2]| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        (dx / len, dy / len)
    };
    let side = |sign: f32| -> Vec<[f32; 2]> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let d_in = if i > 0 { Some(dir(pts[i - 1], pts[i])) } else { None };
            let d_out = if i + 1 < n { Some(dir(pts[i], pts[i + 1])) } else { None };
            let p = pts[i];
            // Normals point left of the direction of travel (sign 1).
            let normal = |d: (f32, f32)| (-d.1 * sign, d.0 * sign);
            match (d_in, d_out) {
                (None, Some(d)) => {
                    let (nx, ny) = normal(d);
                    // A square end: out by half the width.
                    out.push([p[0] + nx * half - d.0 * half, p[1] + ny * half - d.1 * half]);
                }
                (Some(d), None) => {
                    let (nx, ny) = normal(d);
                    out.push([p[0] + nx * half + d.0 * half, p[1] + ny * half + d.1 * half]);
                }
                (Some(a), Some(b)) => {
                    let (n1, n2) = (normal(a), normal(b));
                    let (mx, my) = (n1.0 + n2.0, n1.1 + n2.1);
                    let len2 = mx * mx + my * my;
                    if len2 < 1e-6 {
                        out.push([p[0] + n1.0 * half, p[1] + n1.1 * half]);
                    } else {
                        // The miter, limited to twice the half width.
                        let scale = (2.0 / len2).min(4.0);
                        out.push([p[0] + mx * half * scale, p[1] + my * half * scale]);
                    }
                }
                (None, None) => {}
            }
        }
        out
    };
    let mut outline = side(1.0);
    let mut right = side(-1.0);
    right.reverse();
    outline.extend(right);
    outline
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{Xf, begin_recording, end_recording, set_view};
    use crate::raster::{Canvas, Glyphs, paint};
    use objc2_foundation::NSSize;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    /// Record with a flipped view at the layer's origin and rasterize at
    /// `scale` onto a black `w`×`h` point canvas; returns the red channel.
    fn draw(w: u32, h: u32, scale: f32, flipped: bool, f: impl FnOnce()) -> Vec<u8> {
        begin_recording();
        let xf = if flipped { Xf::IDENTITY } else { Xf { tx: 0.0, a: -1.0, ty: h as f64 } };
        set_view(xf, Rect::new(0.0, 0.0, w as f32, h as f32));
        f();
        let ops = end_recording();
        let (pw, ph) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let mut px = vec![0u32; (pw * ph) as usize];
        let mut canvas = Canvas { px: &mut px, width: pw, height: ph, origin_y: 0.0, scale };
        paint(&mut canvas, &mut Glyphs::default(), &[Rect::new(0.0, 0.0, w as f32, h as f32)], &ops);
        px.iter().map(|p| (p >> 16) as u8).collect()
    }

    const WHITE: Color = [1.0, 1.0, 1.0, 1.0];

    #[test]
    fn round_rects_cover_their_area_less_the_corners() {
        for scale in [1.0, 2.0] {
            let red = draw(20, 20, scale, true, || fill_round_rect(rect(2.0, 2.0, 16.0, 16.0), radii(4.0), WHITE));
            let w = (20.0 * scale) as usize;
            let at = |x: f32, y: f32| red[(y * scale) as usize * w + (x * scale) as usize];
            assert_eq!(at(10.0, 10.0), 255, "center at {scale}");
            assert_eq!(at(1.0, 10.0), 0, "outside at {scale}");
            // The corner is cut away; the edge's middle isn't.
            assert_eq!(at(2.2, 2.2), 0, "corner at {scale}");
            assert_eq!(at(2.5, 10.0), 255, "edge at {scale}");
            // Antialiased: some partial coverage along the curve.
            assert!(red.iter().any(|&v| v > 0 && v < 255), "antialiased at {scale}");
        }
    }

    #[test]
    fn per_corner_radii_follow_the_screen_whichever_way_the_view_is_flipped() {
        for flipped in [true, false] {
            let red =
                draw(20, 20, 1.0, flipped, || fill_round_rect(rect(0.0, 0.0, 20.0, 20.0), [8.0, 0.0, 0.0, 0.0], WHITE));
            // Only the top-left corner, as seen, is rounded.
            assert_eq!(red[0], 0, "flipped {flipped}");
            assert_eq!(red[19], 255, "flipped {flipped}");
            assert_eq!(red[19 * 20], 255, "flipped {flipped}");
            assert_eq!(red[19 * 20 + 19], 255, "flipped {flipped}");
        }
    }

    #[test]
    fn strokes_leave_the_inside_alone() {
        for scale in [1.0, 2.0] {
            let red =
                draw(20, 20, scale, true, || stroke_round_rect(rect(2.0, 2.0, 16.0, 16.0), radii(4.0), 2.0, WHITE));
            let w = (20.0 * scale) as usize;
            let at = |x: f32, y: f32| red[(y * scale) as usize * w + (x * scale) as usize];
            assert_eq!(at(10.0, 10.0), 0, "inside at {scale}");
            assert_eq!(at(10.0, 2.5), 255, "top edge at {scale}");
            assert_eq!(at(17.5, 10.0), 255, "right edge at {scale}");
            assert_eq!(at(10.0, 5.0), 0, "just inside the edge at {scale}");
        }
        let ring = draw(20, 20, 2.0, true, || stroke_ellipse(rect(0.0, 0.0, 20.0, 20.0), 2.0, WHITE));
        assert_eq!(ring[20 * 40 + 20], 0);
        assert_eq!(ring[40 + 20], 255);
    }

    #[test]
    fn polylines_and_arcs_draw_where_asked() {
        let red = draw(20, 20, 1.0, true, || {
            stroke_polyline(&[NSPoint::new(2.0, 10.0), NSPoint::new(18.0, 10.0)], 2.0, WHITE)
        });
        assert_eq!(red[9 * 20 + 10], 255);
        assert_eq!(red[5 * 20 + 10], 0);
        let red = draw(20, 20, 1.0, true, || {
            stroke_arc(NSPoint::new(10.0, 10.0), 8.0, 0.0, std::f64::consts::PI, 2.0, WHITE)
        });
        // The right half of the circle is drawn, the left isn't.
        assert_eq!(red[10 * 20 + 17], 255);
        assert_eq!(red[10 * 20 + 2], 0);
    }
}
