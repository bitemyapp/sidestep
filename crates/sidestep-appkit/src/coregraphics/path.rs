//! `CGPath` and `CGMutablePath`, and the path building CGContext shares
//! ([`Shape`]).
//!
//! A shape is a `kurbo::BezPath` built as CoreGraphics builds paths, which
//! differs from `NSBezierPath`'s: a move right after a move replaces it, a
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
//! pins these against macOS. Transforms given with each element are
//! applied to its points.
//!
//! A path is one class for both kinds: an immutable path's shape is fixed
//! when it's made; a mutable one's is behind a `RefCell` and changed by one
//! thread at a time, as CoreGraphics allows. Either is read without
//! touching the `RefCell`'s borrow count, so threads may read a path none
//! is changing (a finished `CGMutablePath` shared for drawing, say), as
//! they may in CoreGraphics. Shapes are shared through an `Arc`, so copying a
//! path copies nothing, and a context adding a path to its own takes the
//! same `Arc`; editing a mutable path someone shares copies it first. The
//! tiny-skia path drawing needs is made on first use and kept with the
//! shape.

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use kurbo::{Affine, BezPath, PathEl, Point, Shape as _};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFTypeID, CGAffineTransform, CGFloat, CGPoint, CGRect};
use objc2_core_graphics::{
    CGLineCap, CGLineJoin, CGMutablePath, CGPath, CGPathApplierFunction, CGPathApplyBlock, CGPathElement,
    CGPathElementType,
};
use objc2_foundation::{NSString, NSUInteger};

use super::geometry::{affine_at, edges};

pub(crate) use sidestep_engine::path::Shape;

fn apply(m: Option<Affine>, p: Point) -> Point {
    m.map_or(p, |m| m * p)
}

/// What CoreGraphics adds to a [`Shape`] in its own terms: rectangles as
/// `CGRect`s (standardized, the null rectangle's ellipse at infinity),
/// `CGPathIsRect` and stroking with its caps and joins.
pub(crate) trait CgShape {
    /// Add a rectangle, remembering it was made as one if it's all there
    /// is and has no transform (see [`as_rect`](Self::as_rect)).
    fn add_rect_made(&mut self, r: CGRect, m: Option<Affine>);
    fn add_cg_rect(&mut self, r: CGRect, m: Option<Affine>);
    /// Four quarter ellipses counterclockwise from the right edge's middle,
    /// closed.
    fn add_cg_ellipse(&mut self, r: CGRect, m: Option<Affine>);
    /// A rectangle with corners `w` × `h` (at most half its size), from its
    /// right edge's middle counterclockwise, closed; a plain rectangle
    /// without corners.
    fn add_cg_rounded_rect(&mut self, r: CGRect, w: f64, h: f64, m: Option<Affine>);
    /// The rectangle this is, if it's one, as `CGPathIsRect` answers
    /// (measured on macOS): a rectangle made as one, whatever its size; or
    /// one its elements trace ([`Shape::traced_rect`]).
    fn as_rect(&self) -> Option<CGRect>;
    /// The outline of this stroked, as a fill, as `CGPathCreateCopyByStrokingPath`
    /// makes it: a negative width is taken as its size, and a subpath of no
    /// length with round or square caps is a dot (a circle, or a square
    /// standing on a corner, as macOS makes it).
    fn stroked(&self, width: f64, cap: CGLineCap, join: CGLineJoin, miter: f64) -> BezPath;
}

impl CgShape for Shape {
    fn add_rect_made(&mut self, r: CGRect, m: Option<Affine>) {
        let whole = self.is_empty() && m.is_none_or(|m| m == Affine::IDENTITY);
        self.add_cg_rect(r, m);
        if whole {
            let s = super::geometry::standardize(r);
            self.made_rect = Some([s.origin.x, s.origin.y, s.size.width, s.size.height]);
        }
    }

    fn add_cg_rect(&mut self, r: CGRect, m: Option<Affine>) {
        self.add_rect(edges(r), m);
    }

    fn add_cg_ellipse(&mut self, r: CGRect, m: Option<Affine>) {
        self.add_ellipse(edges(r), super::geometry::is_null(r), m);
    }

    fn add_cg_rounded_rect(&mut self, r: CGRect, w: f64, h: f64, m: Option<Affine>) {
        self.add_rounded_rect(edges(r), w, h, m);
    }

    fn as_rect(&self) -> Option<CGRect> {
        if let Some([x, y, w, h]) = self.made_rect {
            return Some(super::geometry::rect(x, y, w, h));
        }
        let r = self.traced_rect()?;
        Some(super::geometry::rect(r.x0, r.y0, r.width(), r.height()))
    }

    fn stroked(&self, width: f64, cap: CGLineCap, join: CGLineJoin, miter: f64) -> BezPath {
        let width = width.abs();
        let style = kurbo::Stroke {
            width,
            join: match join {
                CGLineJoin::Round => kurbo::Join::Round,
                CGLineJoin::Bevel => kurbo::Join::Bevel,
                _ => kurbo::Join::Miter,
            },
            miter_limit: miter.max(1.0),
            start_cap: cap_of(cap),
            end_cap: cap_of(cap),
            ..kurbo::Stroke::new(width)
        };
        let mut out = kurbo::stroke(self.path.iter(), &style, &kurbo::StrokeOpts::default(), 0.01);
        if width > 0.0 && matches!(cap, CGLineCap::Round | CGLineCap::Square) {
            for p in dots(&self.path) {
                let h = width / 2.0;
                if cap == CGLineCap::Round {
                    out.extend(kurbo::Circle::new(p, h).path_elements(0.01));
                } else {
                    let d = h * std::f64::consts::SQRT_2;
                    out.move_to((p.x + d, p.y));
                    out.line_to((p.x, p.y + d));
                    out.line_to((p.x - d, p.y));
                    out.line_to((p.x, p.y - d));
                    out.close_path();
                }
            }
        }
        out
    }
}

/// Where `path` has subpaths of no length that aren't a lone move: a move
/// and segments or a close all at its point.
fn dots(path: &BezPath) -> Vec<Point> {
    let mut out = Vec::new();
    let mut at: Option<(Point, bool)> = None;
    let flush = |at: Option<(Point, bool)>, out: &mut Vec<Point>| {
        if let Some((p, true)) = at {
            out.push(p);
        }
    };
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                flush(at.take(), &mut out);
                at = Some((p, false));
            }
            PathEl::LineTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => {
                let degenerate = match *el {
                    PathEl::QuadTo(c, _) => at.is_some_and(|(s, _)| c == s),
                    PathEl::CurveTo(c1, c2, _) => at.is_some_and(|(s, _)| c1 == s && c2 == s),
                    _ => true,
                };
                at = match at {
                    Some((s, _)) if s == p && degenerate => Some((s, true)),
                    _ => None,
                };
            }
            PathEl::ClosePath => {
                if let Some((s, _)) = at {
                    at = Some((s, true));
                }
            }
        }
    }
    flush(at, &mut out);
    out
}

fn cap_of(cap: CGLineCap) -> kurbo::Cap {
    match cap {
        CGLineCap::Round => kurbo::Cap::Round,
        CGLineCap::Square => kurbo::Cap::Square,
        _ => kurbo::Cap::Butt,
    }
}

pub(crate) struct PathIvars {
    mutable: bool,
    /// The shape. An immutable path's is never borrowed mutably.
    shape: RefCell<Arc<Shape>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. An immutable
    // path's shape is never written after it's made; a mutable path is used
    // by one thread at a time, as CoreGraphics' are.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGPath"]
    #[ivars = PathIvars]
    pub(crate) struct CGPathImpl;

    impl CGPathImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let mut text = format!("Path {:#x}:\n", self as *const Self as usize);
            self.with(|s| {
                for el in s.elements() {
                    let line = match el {
                        PathEl::MoveTo(p) => format!("  moveto ({}, {})\n", p.x, p.y),
                        PathEl::LineTo(p) => format!("    lineto ({}, {})\n", p.x, p.y),
                        PathEl::QuadTo(c, p) => format!("    quadto ({}, {}) ({}, {})\n", c.x, c.y, p.x, p.y),
                        PathEl::CurveTo(a, b, p) => {
                            format!("    curveto ({}, {}) ({}, {}) ({}, {})\n", a.x, a.y, b.x, b.y, p.x, p.y)
                        }
                        PathEl::ClosePath => "    closepath\n".to_string(),
                    };
                    text.push_str(&line);
                }
            });
            NSString::from_str(&text)
        }
    }

    unsafe impl NSObjectProtocol for CGPathImpl {
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<CGPathImpl>()).is_some_and(|o| {
                std::ptr::eq(self, o) || *self.snapshot() == *o.snapshot()
            })
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            self.with(|s| s.elements().len())
        }
    }
);

impl CGPathImpl {
    /// Run `f` on the shape as it stands.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&Shape) -> R) -> R {
        // SAFETY: the shape is borrowed mutably only inside `edit`, which
        // runs no program code, so it's never borrowed mutably while this
        // thread reads it; another thread changing a path this one reads is
        // the program's race, as in CoreGraphics. Reading without touching
        // the borrow count keeps threads that share a path for reading off
        // it (they'd race on the count).
        match unsafe { self.ivars().shape.try_borrow_unguarded() } {
            Ok(shape) => f(shape),
            Err(_) => f(&Shape::default()),
        }
    }

    /// The shape as it stands, shared.
    pub(crate) fn snapshot(&self) -> Arc<Shape> {
        self.with_arc(Arc::clone)
    }

    /// Run `f` on the shared shape as it stands.
    fn with_arc<R>(&self, f: impl FnOnce(&Arc<Shape>) -> R) -> R {
        // SAFETY: as in `with`.
        match unsafe { self.ivars().shape.try_borrow_unguarded() } {
            Ok(shape) => f(shape),
            Err(_) => f(&Arc::default()),
        }
    }

    /// Change a mutable path (an immutable one stays as it is).
    fn edit(&self, f: impl FnOnce(&mut Shape)) {
        if self.ivars().mutable {
            f(Arc::make_mut(&mut self.ivars().shape.borrow_mut()));
        }
    }

    pub(crate) fn as_cg(&self) -> &CGPath {
        // SAFETY: CGPathImpl is what CGPath names.
        unsafe { &*(self as *const Self).cast::<CGPath>() }
    }
}

/// A new path of `shape`, mutable or not.
pub(crate) fn new_path(shape: Arc<Shape>, mutable: bool) -> Retained<CGPathImpl> {
    let this = CGPathImpl::alloc().set_ivars(PathIvars { mutable, shape: RefCell::new(shape) });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

pub(crate) fn path_imp(p: &CGPath) -> &CGPathImpl {
    // SAFETY: every CGPath is a CGPathImpl.
    unsafe { &*(p as *const CGPath).cast::<CGPathImpl>() }
}

fn mutable_imp(p: &CGMutablePath) -> &CGPathImpl {
    // SAFETY: every CGMutablePath is a CGPathImpl.
    unsafe { &*(p as *const CGMutablePath).cast::<CGPathImpl>() }
}

fn point(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

fn cg_point(p: Point) -> CGPoint {
    CGPoint { x: p.x, y: p.y }
}

/// Edit a mutable path with a transform from a nullable pointer.
///
/// # Safety
///
/// `m` is null or points at a transform.
unsafe fn edit(path: Option<&CGMutablePath>, m: *const CGAffineTransform, f: impl FnOnce(&mut Shape, Option<Affine>)) {
    let Some(path) = path else { return };
    // SAFETY: as the caller promises.
    let m = unsafe { affine_at(m) };
    mutable_imp(path).edit(|s| f(s, m));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_PATH
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathCreateMutable() -> Option<NonNull<CGMutablePath>> {
    Some(super::owned(new_path(Arc::default(), true)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathCreateCopy(path: Option<&CGPath>) -> Option<NonNull<CGPath>> {
    Some(super::owned(new_path(path_imp(path?).snapshot(), false)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathCreateMutableCopy(path: Option<&CGPath>) -> Option<NonNull<CGMutablePath>> {
    Some(super::owned(new_path(path_imp(path?).snapshot(), true)))
}

fn transformed_copy(
    path: Option<&CGPath>,
    transform: *const CGAffineTransform,
    mutable: bool,
) -> Option<Retained<CGPathImpl>> {
    let shape = path_imp(path?).snapshot();
    // SAFETY: the caller passes null or a transform.
    let shape = match unsafe { affine_at(transform) } {
        Some(m) => Arc::new(shape.transformed(m)),
        None => shape,
    };
    Some(new_path(shape, mutable))
}

/// # Safety
///
/// `transform` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathCreateCopyByTransformingPath(
    path: Option<&CGPath>,
    transform: *const CGAffineTransform,
) -> Option<NonNull<CGPath>> {
    transformed_copy(path, transform, false).map(super::owned)
}

/// # Safety
///
/// `transform` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathCreateMutableCopyByTransformingPath(
    path: Option<&CGPath>,
    transform: *const CGAffineTransform,
) -> Option<NonNull<CGMutablePath>> {
    transformed_copy(path, transform, true).map(super::owned)
}

fn made(f: impl FnOnce(&mut Shape)) -> Option<NonNull<CGPath>> {
    let mut shape = Shape::default();
    f(&mut shape);
    Some(super::owned(new_path(Arc::new(shape), false)))
}

/// # Safety
///
/// `transform` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathCreateWithRect(
    rect: CGRect,
    transform: *const CGAffineTransform,
) -> Option<NonNull<CGPath>> {
    // SAFETY: as the caller promises.
    let m = unsafe { affine_at(transform) };
    made(|s| s.add_rect_made(rect, m))
}

/// # Safety
///
/// `transform` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathCreateWithEllipseInRect(
    rect: CGRect,
    transform: *const CGAffineTransform,
) -> Option<NonNull<CGPath>> {
    // SAFETY: as the caller promises.
    let m = unsafe { affine_at(transform) };
    made(|s| s.add_cg_ellipse(rect, m))
}

/// # Safety
///
/// `transform` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathCreateWithRoundedRect(
    rect: CGRect,
    corner_width: CGFloat,
    corner_height: CGFloat,
    transform: *const CGAffineTransform,
) -> Option<NonNull<CGPath>> {
    // SAFETY: as the caller promises.
    let m = unsafe { affine_at(transform) };
    made(|s| s.add_cg_rounded_rect(rect, corner_width, corner_height, m))
}

/// # Safety
///
/// `transform` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddRoundedRect(
    path: Option<&CGMutablePath>,
    transform: *const CGAffineTransform,
    rect: CGRect,
    corner_width: CGFloat,
    corner_height: CGFloat,
) {
    // SAFETY: as the caller promises.
    unsafe { edit(path, transform, |s, m| s.add_cg_rounded_rect(rect, corner_width, corner_height, m)) }
}

/// # Safety
///
/// `transform` is null or points at a transform; `lengths` holds `count`
/// lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathCreateCopyByDashingPath(
    path: Option<&CGPath>,
    transform: *const CGAffineTransform,
    phase: CGFloat,
    lengths: *const CGFloat,
    count: usize,
) -> Option<NonNull<CGPath>> {
    let shape = path_imp(path?).snapshot();
    // SAFETY: as the caller promises.
    let (m, lengths) = unsafe { (affine_at(transform), super::slice(lengths, count)) };
    let dashed = dash(&shape.path, phase, lengths);
    let mut out = Shape::from_path(dashed);
    if let Some(m) = m {
        out = out.transformed(m);
    }
    Some(super::owned(new_path(Arc::new(out), false)))
}

/// `path` cut into dashes `lengths` long (on, off, on, …) from `phase`
/// into them, as `CGPathCreateCopyByDashingPath` cuts it (measured on
/// macOS): each subpath starts the pattern over, a dash goes on round
/// corners without a break, and a closed subpath's last dash doesn't join
/// its first (so this isn't `kurbo::dash`, which joins them). A length's
/// size counts, not its sign; no lengths leave the path as it was; lengths
/// that add up to nothing, or dashes too many to make (more than a
/// million, as Skia caps them), leave nothing. An odd pattern repeats
/// twice, as PostScript's does.
pub(crate) fn dash(path: &BezPath, phase: f64, lengths: &[f64]) -> BezPath {
    use kurbo::{ParamCurve, ParamCurveArclen};
    /// The most dashes a path is cut into.
    const MOST: f64 = 1_000_000.0;
    if lengths.is_empty() {
        return path.clone();
    }
    let mut pattern: Vec<f64> = lengths.iter().map(|l| l.abs()).collect();
    if pattern.iter().any(|l| !l.is_finite()) || !phase.is_finite() {
        return path.clone();
    }
    if pattern.len() % 2 == 1 {
        pattern.extend_from_within(..);
    }
    let period: f64 = pattern.iter().sum();
    let total: f64 = path.segments().map(|seg| seg.arclen(1e-3)).sum();
    // (A path with points that aren't numbers has no length to walk.)
    if period <= 0.0 || !total.is_finite() || total / period * pattern.len() as f64 > MOST {
        return BezPath::new();
    }
    // Where the pattern starts: which length, and how much of it is left.
    let start = || {
        let mut at = phase.rem_euclid(period);
        let mut i = 0;
        while at >= pattern[i] && at > 0.0 {
            at -= pattern[i];
            i = (i + 1) % pattern.len();
        }
        (i, pattern[i] - at)
    };
    let accuracy = 1e-6;
    let mut out = BezPath::new();
    // Each subpath on its own.
    let mut subpaths: Vec<Vec<PathEl>> = Vec::new();
    for el in path.elements() {
        match el {
            PathEl::MoveTo(_) => subpaths.push(vec![*el]),
            _ => match subpaths.last_mut() {
                Some(sp) => sp.push(*el),
                None => subpaths.push(vec![*el]),
            },
        }
    }
    for sp in subpaths {
        let (mut i, mut left) = start();
        let mut drawing = false;
        for seg in kurbo::segments(sp.iter().copied()) {
            let len = seg.arclen(accuracy);
            let mut pos = 0.0;
            loop {
                let step = left.min(len - pos).max(0.0);
                if i % 2 == 0 {
                    let t0 = if pos <= 0.0 { 0.0 } else { seg.inv_arclen(pos, accuracy) };
                    let t1 = if pos + step >= len { 1.0 } else { seg.inv_arclen(pos + step, accuracy) };
                    let piece = seg.subsegment(t0..t1);
                    if !drawing {
                        out.move_to(piece.start());
                        drawing = true;
                    }
                    match piece {
                        kurbo::PathSeg::Line(l) => out.line_to(l.p1),
                        kurbo::PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
                        kurbo::PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
                    }
                }
                pos += step;
                left -= step;
                if left <= 1e-9 {
                    i = (i + 1) % pattern.len();
                    left = pattern[i];
                    if i % 2 == 1 {
                        drawing = false;
                    }
                }
                if pos >= len - 1e-9 {
                    break;
                }
            }
        }
    }
    out
}

/// # Safety
///
/// `transform` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathCreateCopyByStrokingPath(
    path: Option<&CGPath>,
    transform: *const CGAffineTransform,
    line_width: CGFloat,
    line_cap: CGLineCap,
    line_join: CGLineJoin,
    miter_limit: CGFloat,
) -> Option<NonNull<CGPath>> {
    let shape = path_imp(path?).snapshot();
    let mut out = Shape::from_path(shape.stroked(line_width, line_cap, line_join, miter_limit));
    // SAFETY: as the caller promises.
    if let Some(m) = unsafe { affine_at(transform) } {
        out = out.transformed(m);
    }
    Some(super::owned(new_path(Arc::new(out), false)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathCreateCopyByFlattening(
    path: Option<&CGPath>,
    flattening_threshold: CGFloat,
) -> Option<NonNull<CGPath>> {
    let shape = path_imp(path?).snapshot();
    let mut flat = BezPath::new();
    kurbo::flatten(shape.path.iter(), flattening_threshold.max(0.01), |el| flat.push(el));
    Some(super::owned(new_path(Arc::new(Shape::from_path(flat)), false)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathEqualToPath(path1: Option<&CGPath>, path2: Option<&CGPath>) -> bool {
    match (path1, path2) {
        (Some(a), Some(b)) => {
            let (a, b) = (path_imp(a), path_imp(b));
            std::ptr::eq(a, b) || *a.snapshot() == *b.snapshot()
        }
        (None, None) => true,
        _ => false,
    }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathMoveToPoint(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
) {
    // SAFETY: as the caller promises.
    unsafe { edit(path, m, |s, m| s.move_to(apply(m, point(x, y)))) }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddLineToPoint(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
) {
    // SAFETY: as the caller promises.
    unsafe {
        edit(path, m, |s, m| {
            s.line_to(apply(m, point(x, y)));
        })
    }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddQuadCurveToPoint(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    cpx: CGFloat,
    cpy: CGFloat,
    x: CGFloat,
    y: CGFloat,
) {
    // SAFETY: as the caller promises.
    unsafe {
        edit(path, m, |s, m| {
            s.quad_to(apply(m, point(cpx, cpy)), apply(m, point(x, y)));
        })
    }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddCurveToPoint(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    cp1x: CGFloat,
    cp1y: CGFloat,
    cp2x: CGFloat,
    cp2y: CGFloat,
    x: CGFloat,
    y: CGFloat,
) {
    // SAFETY: as the caller promises.
    unsafe {
        edit(path, m, |s, m| {
            s.curve_to(apply(m, point(cp1x, cp1y)), apply(m, point(cp2x, cp2y)), apply(m, point(x, y)));
        })
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathCloseSubpath(path: Option<&CGMutablePath>) {
    // SAFETY: no transform.
    unsafe { edit(path, std::ptr::null(), |s, _| s.close()) }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddRect(path: Option<&CGMutablePath>, m: *const CGAffineTransform, rect: CGRect) {
    // SAFETY: as the caller promises.
    unsafe { edit(path, m, |s, m| s.add_rect_made(rect, m)) }
}

/// # Safety
///
/// `m` is null or points at a transform; `rects` holds `count` rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddRects(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    rects: *const CGRect,
    count: usize,
) {
    // SAFETY: as the caller promises.
    let rects = unsafe { super::slice(rects, count) };
    // SAFETY: as the caller promises.
    unsafe { edit(path, m, |s, m| rects.iter().for_each(|r| s.add_cg_rect(*r, m))) }
}

/// # Safety
///
/// `m` is null or points at a transform; `points` holds `count` points.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddLines(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    points: *const CGPoint,
    count: usize,
) {
    // SAFETY: as the caller promises.
    let points: Vec<Point> = unsafe { super::slice(points, count) }.iter().map(|p| point(p.x, p.y)).collect();
    // SAFETY: as the caller promises.
    unsafe { edit(path, m, |s, m| s.add_lines(&points, m)) }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddEllipseInRect(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    rect: CGRect,
) {
    // SAFETY: as the caller promises.
    unsafe { edit(path, m, |s, m| s.add_cg_ellipse(rect, m)) }
}

/// # Safety
///
/// `matrix` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddRelativeArc(
    path: Option<&CGMutablePath>,
    matrix: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
    radius: CGFloat,
    start_angle: CGFloat,
    delta: CGFloat,
) {
    // SAFETY: as the caller promises.
    unsafe { edit(path, matrix, |s, m| s.add_sweep(point(x, y), radius, start_angle, delta.abs(), delta < 0.0, m)) }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddArc(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    x: CGFloat,
    y: CGFloat,
    radius: CGFloat,
    start_angle: CGFloat,
    end_angle: CGFloat,
    clockwise: bool,
) {
    // SAFETY: as the caller promises.
    unsafe { edit(path, m, |s, m| s.add_arc(point(x, y), radius, start_angle, end_angle, clockwise, m)) }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddArcToPoint(
    path: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    x1: CGFloat,
    y1: CGFloat,
    x2: CGFloat,
    y2: CGFloat,
    radius: CGFloat,
) {
    // SAFETY: as the caller promises.
    unsafe { edit(path, m, |s, m| s.add_arc_to(point(x1, y1), point(x2, y2), radius, m)) }
}

/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathAddPath(
    path1: Option<&CGMutablePath>,
    m: *const CGAffineTransform,
    path2: Option<&CGPath>,
) {
    let Some(other) = path2.map(|p| path_imp(p).snapshot()) else { return };
    // SAFETY: as the caller promises.
    unsafe { edit(path1, m, |s, m| s.add_shape(&other, m)) }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathIsEmpty(path: Option<&CGPath>) -> bool {
    path.is_none_or(|p| path_imp(p).with(Shape::is_empty))
}

/// # Safety
///
/// `rect` is null or valid to write.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathIsRect(path: Option<&CGPath>, rect: *mut CGRect) -> bool {
    let Some(r) = path.and_then(|p| path_imp(p).with(Shape::as_rect)) else { return false };
    // SAFETY: as the caller promises.
    unsafe { super::store(rect, r) };
    true
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathGetCurrentPoint(path: Option<&CGPath>) -> CGPoint {
    path.map_or(CGPoint { x: 0.0, y: 0.0 }, |p| cg_point(path_imp(p).with(Shape::current)))
}

pub(crate) fn rect_of(r: Option<kurbo::Rect>) -> CGRect {
    r.map_or(super::geometry::NULL, |r| super::geometry::rect(r.x0, r.y0, r.width(), r.height()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathGetBoundingBox(path: Option<&CGPath>) -> CGRect {
    rect_of(path.and_then(|p| path_imp(p).with(Shape::control_bounds)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPathGetPathBoundingBox(path: Option<&CGPath>) -> CGRect {
    rect_of(path.and_then(|p| path_imp(p).with(Shape::tight_bounds)))
}

/// Whether `point`, transformed by `m`, is inside the path.
///
/// # Safety
///
/// `m` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathContainsPoint(
    path: Option<&CGPath>,
    m: *const CGAffineTransform,
    point: CGPoint,
    eo_fill: bool,
) -> bool {
    let Some(path) = path else { return false };
    // SAFETY: as the caller promises.
    let p = apply(unsafe { affine_at(m) }, self::point(point.x, point.y));
    path_imp(path).with(|s| s.contains(p, eo_fill))
}

/// Call `f` with each element as CoreGraphics hands it out.
fn each_element(shape: &Shape, mut f: impl FnMut(&CGPathElement)) {
    for el in shape.elements() {
        let (kind, mut pts): (_, [CGPoint; 3]) = match *el {
            PathEl::MoveTo(p) => (CGPathElementType::MoveToPoint, [cg_point(p), CGPoint::ZERO, CGPoint::ZERO]),
            PathEl::LineTo(p) => (CGPathElementType::AddLineToPoint, [cg_point(p), CGPoint::ZERO, CGPoint::ZERO]),
            PathEl::QuadTo(c, p) => (CGPathElementType::AddQuadCurveToPoint, [cg_point(c), cg_point(p), CGPoint::ZERO]),
            PathEl::CurveTo(a, b, p) => (CGPathElementType::AddCurveToPoint, [cg_point(a), cg_point(b), cg_point(p)]),
            PathEl::ClosePath => (CGPathElementType::CloseSubpath, [CGPoint::ZERO; 3]),
        };
        let element = CGPathElement { r#type: kind, points: NonNull::from(&mut pts).cast() };
        f(&element);
    }
}

/// # Safety
///
/// `function` is a valid applier for `info`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathApply(path: Option<&CGPath>, info: *mut c_void, function: CGPathApplierFunction) {
    let (Some(path), Some(function)) = (path, function) else { return };
    // A snapshot: the applier may change the path.
    let shape = path_imp(path).snapshot();
    // SAFETY: the element and its points live through the call.
    each_element(&shape, |el| unsafe { function(info, NonNull::from(el)) });
}

/// # Safety
///
/// `block` is a valid block.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPathApplyWithBlock(path: &CGPath, block: CGPathApplyBlock) {
    if block.is_null() {
        return;
    }
    let shape = path_imp(path).snapshot();
    // SAFETY: as the caller promises.
    let block = unsafe { &*block };
    each_element(&shape, |el| block.call((NonNull::from(el),)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(f: impl FnOnce(&mut Shape)) -> Vec<PathEl> {
        let mut s = Shape::default();
        f(&mut s);
        s.elements().to_vec()
    }

    #[test]
    fn building_follows_coregraphics() {
        let p = |x, y| Point::new(x, y);
        // A move after a move replaces it; a line on nothing is dropped.
        assert_eq!(
            shape(|s| {
                s.move_to(p(1.0, 1.0));
                s.move_to(p(2.0, 2.0));
            }),
            [PathEl::MoveTo(p(2.0, 2.0))]
        );
        assert!(
            shape(|s| {
                s.line_to(p(1.0, 1.0));
            })
            .is_empty()
        );
        // A close adds no move, and a second adds nothing.
        let els = shape(|s| {
            s.move_to(p(0.0, 0.0));
            s.line_to(p(1.0, 0.0));
            s.close();
            s.close();
            s.line_to(p(5.0, 5.0));
        });
        assert_eq!(
            els,
            [PathEl::MoveTo(p(0.0, 0.0)), PathEl::LineTo(p(1.0, 0.0)), PathEl::ClosePath, PathEl::LineTo(p(5.0, 5.0))]
        );
        // Arcs: a quarter turn a curve, from the start.
        let els = shape(|s| s.add_arc(p(10.0, 10.0), 5.0, 0.0, std::f64::consts::PI, false, None));
        assert_eq!(els.len(), 3);
        let els = shape(|s| s.add_arc(p(0.0, 0.0), 1.0, 0.0, -2.0 * std::f64::consts::PI, false, None));
        assert_eq!(els.len(), 1, "no sweep");
        let els = shape(|s| s.add_arc(p(0.0, 0.0), 1.0, 0.0, 2.0 * std::f64::consts::PI, true, None));
        assert_eq!(els.len(), 5, "a whole turn clockwise");
    }
}
