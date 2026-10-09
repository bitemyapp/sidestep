//! Paths: [`Shape`], a path built as CoreGraphics builds paths, and what
//! drawing needs of a `kurbo::BezPath` (tiny-skia's path for it, its tight
//! bounds, hit testing).
//!
//! A shape differs from a plain `BezPath` the way CoreGraphics' paths
//! differ from `NSBezierPath`'s: a move right after a move replaces it, a
//! line or curve with no current point is dropped, a close adds no move
//! (the current point goes back to the subpath's start) and a second close
//! adds nothing; rectangles start at their origin and ellipses and rounded
//! rectangles at their right edge's middle, counterclockwise (in unflipped
//! coordinates); arcs are split into quarter turns from their start and
//! then what's left, their sweep worked out from the angles and direction
//! as CoreGraphics works it out (a full turn or more counterclockwise with
//! the end past the start, a full turn clockwise with it before, a part of
//! one otherwise; more than a thousand turns adds nothing, and a sweep of
//! next to nothing only the start). `conformance/tests/coregraphics.rs`
//! pins these against macOS through `CGPath`, which keeps its shape in an
//! `Arc<Shape>`, as Core Animation's layers do. Transforms given with each
//! element are applied to its points. The tiny-skia path drawing needs is
//! made on first use and kept with the shape.

use std::sync::{Arc, OnceLock};

use kurbo::{Affine, BezPath, ParamCurveNearest, PathEl, Point, Shape as _, Vec2};

/// The control-point distance of a cubic approximating an arc of `angle`
/// radians, as a fraction of the radius.
pub fn kappa(angle: f64) -> f64 {
    4.0 / 3.0 * (angle / 4.0).tan()
}

/// A quarter turn's control-point distance, as a fraction of the radius.
fn quarter() -> f64 {
    kappa(std::f64::consts::FRAC_PI_2)
}

/// A rectangle by its edges: left, top, right, bottom (x0 ≤ x1, y0 ≤ y1).
pub type Edges = (f64, f64, f64, f64);

/// A path's elements as CoreGraphics builds them.
#[derive(Debug, Default)]
pub struct Shape {
    pub path: BezPath,
    /// Where the current subpath started: the current point after a close.
    pub start: Option<Point>,
    /// The rectangle this is (x, y, width, height), when it was made as one
    /// (`CGPathCreateWithRect` or `CGPathAddRect` onto nothing,
    /// untransformed), which `CGPathIsRect` answers as CoreGraphics does,
    /// whatever its size. Any change forgets it.
    pub made_rect: Option<[f64; 4]>,
    /// The path as tiny-skia draws it, made on first use.
    drawn: OnceLock<Option<Arc<tiny_skia::Path>>>,
}

impl Clone for Shape {
    fn clone(&self) -> Self {
        Shape { path: self.path.clone(), start: self.start, made_rect: self.made_rect, drawn: self.drawn.clone() }
    }
}

impl PartialEq for Shape {
    fn eq(&self, other: &Self) -> bool {
        self.path.elements() == other.path.elements()
    }
}

fn apply(m: Option<Affine>, p: Point) -> Point {
    m.map_or(p, |m| m * p)
}

impl Shape {
    pub fn from_path(path: BezPath) -> Shape {
        let start = subpath_start(&path);
        Shape { path, start, made_rect: None, drawn: OnceLock::new() }
    }

    pub fn elements(&self) -> &[PathEl] {
        self.path.elements()
    }

    pub fn is_empty(&self) -> bool {
        self.path.elements().is_empty()
    }

    /// Something changed: drop what drawing made.
    fn changed(&mut self) {
        self.drawn = OnceLock::new();
        self.made_rect = None;
    }

    /// The current point, (0, 0) for none.
    pub fn current(&self) -> Point {
        match self.path.elements().last() {
            Some(PathEl::MoveTo(p) | PathEl::LineTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p)) => *p,
            Some(PathEl::ClosePath) => self.start.unwrap_or(Point::ZERO),
            None => Point::ZERO,
        }
    }

    pub fn move_to(&mut self, p: Point) {
        if let Some(PathEl::MoveTo(last)) = self.path.elements_mut().last_mut() {
            *last = p;
        } else {
            self.path.move_to(p);
        }
        self.start = Some(p);
        self.changed();
    }

    /// Append a segment, if there's a current point to start it from.
    fn segment(&mut self, el: PathEl) -> bool {
        if self.is_empty() {
            return false;
        }
        self.path.push(el);
        self.changed();
        true
    }

    pub fn line_to(&mut self, p: Point) -> bool {
        self.segment(PathEl::LineTo(p))
    }

    pub fn quad_to(&mut self, c: Point, p: Point) -> bool {
        self.segment(PathEl::QuadTo(c, p))
    }

    pub fn curve_to(&mut self, c1: Point, c2: Point, p: Point) -> bool {
        self.segment(PathEl::CurveTo(c1, c2, p))
    }

    pub fn close(&mut self) {
        if !matches!(self.path.elements().last(), None | Some(PathEl::ClosePath)) {
            self.path.close_path();
            self.changed();
        }
    }

    /// A rectangle from its origin, closed.
    pub fn add_rect(&mut self, (x0, y0, x1, y1): Edges, m: Option<Affine>) {
        self.move_to(apply(m, Point::new(x0, y0)));
        for p in [(x1, y0), (x1, y1), (x0, y1)] {
            self.line_to(apply(m, p.into()));
        }
        self.close();
    }

    pub fn add_lines(&mut self, points: &[Point], m: Option<Affine>) {
        let Some((first, rest)) = points.split_first() else { return };
        self.move_to(apply(m, *first));
        for p in rest {
            self.line_to(apply(m, *p));
        }
    }

    /// Four quarter ellipses counterclockwise from the right edge's middle,
    /// closed; with no radii (`at_infinity`, the null rectangle's ellipse,
    /// as CoreGraphics puts it) four curves of nothing.
    pub fn add_ellipse(&mut self, (x0, y0, x1, y1): Edges, at_infinity: bool, m: Option<Affine>) {
        let (rx, ry) = if at_infinity { (0.0, 0.0) } else { ((x1 - x0) / 2.0, (y1 - y0) / 2.0) };
        let (cx, cy) = (x0 + rx, y0 + ry);
        let (kx, ky) = (rx * quarter(), ry * quarter());
        let p = |x: f64, y: f64| apply(m, Point::new(x, y));
        self.move_to(p(x1, cy));
        self.curve_to(p(x1, cy + ky), p(cx + kx, y1), p(cx, y1));
        self.curve_to(p(cx - kx, y1), p(x0, cy + ky), p(x0, cy));
        self.curve_to(p(x0, cy - ky), p(cx - kx, y0), p(cx, y0));
        self.curve_to(p(cx + kx, y0), p(x1, cy - ky), p(x1, cy));
        self.close();
    }

    /// A rectangle with corners `w` × `h` (at most half its size), from its
    /// right edge's middle counterclockwise, closed; a plain rectangle
    /// without corners.
    pub fn add_rounded_rect(&mut self, edges: Edges, w: f64, h: f64, m: Option<Affine>) {
        let (x0, y0, x1, y1) = edges;
        if w.is_nan() || h.is_nan() || w <= 0.0 || h <= 0.0 {
            self.add_rect(edges, m);
            return;
        }
        let (w, h) = (w.min((x1 - x0) / 2.0), h.min((y1 - y0) / 2.0));
        let (kw, kh) = (w * quarter(), h * quarter());
        let p = |x: f64, y: f64| apply(m, Point::new(x, y));
        self.move_to(p(x1, (y0 + y1) / 2.0));
        self.line_to(p(x1, y1 - h));
        self.curve_to(p(x1, y1 - h + kh), p(x1 - w + kw, y1), p(x1 - w, y1));
        self.line_to(p(x0 + w, y1));
        self.curve_to(p(x0 + w - kw, y1), p(x0, y1 - h + kh), p(x0, y1 - h));
        self.line_to(p(x0, y0 + h));
        self.curve_to(p(x0, y0 + h - kh), p(x0 + w - kw, y0), p(x0 + w, y0));
        self.line_to(p(x1 - w, y0));
        self.curve_to(p(x1 - w + kw, y0), p(x1, y0 + h - kh), p(x1, y0 + h));
        self.close();
    }

    /// An arc of the circle around `c` from angle `start` (radians) through
    /// `sweep` (none or more) counterclockwise or clockwise, in quarter
    /// turns from the start and then what's left. It begins with a move on
    /// an empty path and a line from the current point otherwise.
    ///
    /// As CoreGraphics does (measured on macOS): a sweep of more than a
    /// thousand turns adds nothing at all; one that isn't a number, or of
    /// next to nothing, adds only its start.
    pub fn add_sweep(&mut self, c: Point, r: f64, start: f64, sweep: f64, clockwise: bool, m: Option<Affine>) {
        /// Sweeps below this add no curve.
        const NOTHING: f64 = 1e-8;
        if !(start.is_finite() && r.is_finite()) || sweep > 2000.0 * std::f64::consts::PI {
            return;
        }
        let dir = if clockwise { -1.0 } else { 1.0 };
        let at = |a: f64| {
            let (s, co) = a.sin_cos();
            (Point::new(c.x + r * co, c.y + r * s), Vec2::new(-r * s, r * co) * dir)
        };
        let (p0, _) = at(start);
        if self.is_empty() {
            self.move_to(apply(m, p0));
        } else {
            self.line_to(apply(m, p0));
        }
        if !sweep.is_finite() {
            return;
        }
        let q = std::f64::consts::FRAC_PI_2;
        let (mut a0, mut left) = (start, sweep);
        // At most 4000 quarter turns, and what's left.
        while left > NOTHING {
            let step = left.min(q);
            let k = if step == q { quarter() } else { kappa(step) };
            let ((q0, d0), (q1, d1)) = (at(a0), at(a0 + dir * step));
            self.curve_to(apply(m, q0 + d0 * k), apply(m, q1 - d1 * k), apply(m, q1));
            a0 += dir * step;
            left -= step;
        }
    }

    /// `CGPathAddArc`'s arc: counterclockwise (in unflipped coordinates) or
    /// clockwise from `start` to `end`.
    pub fn add_arc(&mut self, c: Point, r: f64, start: f64, end: f64, clockwise: bool, m: Option<Affine>) {
        let tau = std::f64::consts::TAU;
        let sweep = match (clockwise, end >= start) {
            // Past the start the way it goes: that far, however many turns.
            (false, true) => end - start,
            (true, false) => start - end,
            // The other way round: part of a turn, a whole one clockwise
            // for a whole turn's difference.
            (false, false) => (end - start).rem_euclid(tau),
            (true, true) => tau - (end - start).rem_euclid(tau),
        };
        self.add_sweep(c, r, start, sweep, clockwise, m);
    }

    /// A line toward `p1`, then an arc of radius `r` tangent to the lines
    /// from the current point to `p1` and from `p1` to `p2`. Nothing on an
    /// empty path.
    pub fn add_arc_to(&mut self, p1: Point, p2: Point, r: f64, m: Option<Affine>) {
        if self.is_empty() {
            return;
        }
        // The current point in the arc's own coordinates.
        let p0 = match m {
            Some(m) if m.determinant() != 0.0 => m.inverse() * self.current(),
            _ => self.current(),
        };
        let (a, b) = (p0 - p1, p2 - p1);
        if a.hypot2() == 0.0 || r == 0.0 {
            // A corner with no arc: a line to it, and an arc of nothing.
            let at = apply(m, p1);
            self.line_to(at);
            self.curve_to(at, at, at);
            return;
        }
        let cross = a.cross(b);
        if b.hypot2() == 0.0 || cross.abs() <= 1e-12 * a.hypot() * b.hypot() {
            self.line_to(apply(m, p1));
            return;
        }
        let (u, v) = (a.normalize(), b.normalize());
        let angle = u.dot(v).clamp(-1.0, 1.0).acos();
        let d = r / (angle / 2.0).tan();
        let t0 = p1 + u * d;
        let center = p1 + (u + v).normalize() * (r / (angle / 2.0).sin());
        let clockwise = cross > 0.0;
        let a0 = (t0 - center).atan2();
        let sweep = std::f64::consts::PI - angle;
        let sweep =
            if (sweep - std::f64::consts::FRAC_PI_2).abs() < 1e-12 { std::f64::consts::FRAC_PI_2 } else { sweep };
        self.add_sweep(center, r, a0, sweep, clockwise, m);
    }

    /// Another path's elements, transformed, after these: its first move
    /// replaces a move this ends with.
    pub fn add_shape(&mut self, other: &Shape, m: Option<Affine>) {
        for el in other.path.elements() {
            match *el {
                PathEl::MoveTo(p) => self.move_to(apply(m, p)),
                PathEl::LineTo(p) => {
                    self.segment(PathEl::LineTo(apply(m, p)));
                }
                PathEl::QuadTo(c, p) => {
                    self.segment(PathEl::QuadTo(apply(m, c), apply(m, p)));
                }
                PathEl::CurveTo(c1, c2, p) => {
                    self.segment(PathEl::CurveTo(apply(m, c1), apply(m, c2), apply(m, p)));
                }
                PathEl::ClosePath => self.close(),
            }
        }
    }

    /// This shape transformed.
    pub fn transformed(&self, m: Affine) -> Shape {
        let mut path = self.path.clone();
        path.apply_affine(m);
        Shape { path, start: self.start.map(|p| m * p), made_rect: None, drawn: OnceLock::new() }
    }

    /// The bounds of every point, control points too; `None` when empty.
    pub fn control_bounds(&self) -> Option<kurbo::Rect> {
        let mut points = self.path.elements().iter().flat_map(|el| {
            let pts: Vec<Point> = match *el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => vec![p],
                PathEl::QuadTo(c, p) => vec![c, p],
                PathEl::CurveTo(a, b, p) => vec![a, b, p],
                PathEl::ClosePath => vec![],
            };
            pts
        });
        let first = points.next()?;
        Some(points.fold(kurbo::Rect::from_points(first, first), |r, p| r.union_pt(p)))
    }

    /// The tight bounds: curves' extremes, not their control points.
    pub fn tight_bounds(&self) -> Option<kurbo::Rect> {
        tight_bounds(&self.path)
    }

    /// Whether `p` is inside, a point on the outline counting as inside
    /// whatever the rule, open subpaths taken as closed.
    pub fn contains(&self, p: Point, even_odd: bool) -> bool {
        let closed = closed(&self.path);
        on_outline(&closed, p) || {
            let winding = closed.winding(p);
            if even_odd { winding % 2 != 0 } else { winding != 0 }
        }
    }

    /// The rectangle the elements trace, if they trace one as `CGPathIsRect`
    /// sees it (measured on macOS): a move and three lines along the axes,
    /// the first of them vertical, closed. (macOS answers no for the same
    /// four corners taken the other way round, or with a fourth line back
    /// to the start.) A rectangle made as one is in `made_rect`.
    pub fn traced_rect(&self) -> Option<kurbo::Rect> {
        let [PathEl::MoveTo(a), PathEl::LineTo(b), PathEl::LineTo(c), PathEl::LineTo(d), PathEl::ClosePath] =
            *self.path.elements()
        else {
            return None;
        };
        let (vertical, horizontal) = (|p: Point, q: Point| p.x == q.x, |p: Point, q: Point| p.y == q.y);
        let rect = vertical(a, b) && horizontal(b, c) && vertical(c, d) && horizontal(d, a) && a != b && b != c;
        rect.then(|| kurbo::Rect::from_points(a, c))
    }

    /// The path as tiny-skia draws it, made once.
    pub fn drawn(&self) -> Option<Arc<tiny_skia::Path>> {
        self.drawn.get_or_init(|| to_skia(&self.path)).clone()
    }
}

/// Where the last subpath of `path` starts.
fn subpath_start(path: &BezPath) -> Option<Point> {
    path.elements().iter().rev().find_map(|el| match el {
        PathEl::MoveTo(p) => Some(*p),
        _ => None,
    })
}

/// `path` as tiny-skia draws it.
pub fn to_skia(path: &BezPath) -> Option<Arc<tiny_skia::Path>> {
    let mut pb = tiny_skia::PathBuilder::with_capacity(path.elements().len(), path.elements().len() * 3);
    let f = |p: Point| (p.x as f32, p.y as f32);
    for el in path.iter() {
        match el {
            PathEl::MoveTo(p) => {
                let (x, y) = f(p);
                pb.move_to(x, y);
            }
            PathEl::LineTo(p) => {
                let (x, y) = f(p);
                pb.line_to(x, y);
            }
            PathEl::QuadTo(c, p) => {
                let ((cx, cy), (x, y)) = (f(c), f(p));
                pb.quad_to(cx, cy, x, y);
            }
            PathEl::CurveTo(c1, c2, p) => {
                let ((ax, ay), (bx, by), (x, y)) = (f(c1), f(c2), f(p));
                pb.cubic_to(ax, ay, bx, by, x, y);
            }
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish().map(Arc::new)
}

/// The tight bounds: curve extrema, not control points; a lone move
/// counts as a point.
pub fn tight_bounds(path: &BezPath) -> Option<kurbo::Rect> {
    let mut bounds: Option<kurbo::Rect> = None;
    let mut add = |r: kurbo::Rect| bounds = Some(bounds.map_or(r, |b| b.union(r)));
    for seg in path.segments() {
        add(seg.bounding_box());
    }
    for el in path.iter() {
        if let PathEl::MoveTo(p) = el {
            add(kurbo::Rect::from_points(p, p));
        }
    }
    bounds
}

/// Whether `p` lies on one of `path`'s segments, to within rounding.
pub fn on_outline(path: &BezPath, p: Point) -> bool {
    let eps = 1e-9 * (1.0 + p.x.abs().max(p.y.abs()));
    path.segments().any(|seg| {
        let b = seg.bounding_box().inflate(eps, eps);
        b.contains(p) && seg.nearest(p, eps * 0.1).distance_sq <= eps * eps
    })
}

/// The path with every open subpath closed, as filling sees it.
pub fn closed(path: &BezPath) -> BezPath {
    let mut out = BezPath::new();
    let mut open = false;
    for el in path.iter() {
        match el {
            PathEl::MoveTo(_) => {
                if open {
                    out.close_path();
                }
                open = false;
            }
            PathEl::ClosePath => open = false,
            _ => open = true,
        }
        out.push(el);
    }
    if open {
        out.close_path();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_subpaths_fill_as_if_closed() {
        let tri = BezPath::from_vec(vec![
            PathEl::MoveTo(Point::ZERO),
            PathEl::LineTo((10.0, 0.0).into()),
            PathEl::LineTo((0.0, 10.0).into()),
        ]);
        assert_ne!(closed(&tri).winding((2.0, 2.0).into()), 0);
    }
}
