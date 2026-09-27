//! `NSBezierPath`: paths built from moves, lines, curves and arcs, their
//! geometry, and drawing them.
//!
//! A path is a `kurbo::BezPath` (its elements are `NSBezierPathElement`'s,
//! quadratic curves included) plus its stroke settings. Geometry comes
//! from kurbo: tight bounds, winding numbers for `containsPoint:`,
//! flattening. Drawing converts the path to a tiny-skia path once, keeps it
//! behind an `Arc` until the path changes, and records an op that shares
//! it, so drawing a path again copies nothing.
//!
//! AppKit's element structure is kept exactly, as programs walk it with
//! `elementAtIndex:`: `closePath` adds a move back to the subpath's start
//! after the close, ovals are four curves with no close, rounded
//! rectangles start on the top edge, arcs are split into quarter turns
//! from their start and then what's left, however many turns they make
//! (`conformance/tests/drawing.rs` pins these against macOS). Stroke
//! settings are kept as given, as AppKit keeps them; drawing clamps what it
//! can't use (a miter limit below 1, a negative width).

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};

use kurbo::{Affine, BezPath, ParamCurveNearest, PathEl, Point, Shape};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSBezierPath, NSBezierPathElement, NSLineCapStyle, NSLineJoinStyle, NSWindingRule};
use objc2_core_graphics::CGPath;
use objc2_foundation::{NSAffineTransform, NSCopying, NSInteger, NSPoint, NSRect, NSSize, NSZone};

use crate::protocol::{Op, Paint, StrokeSpec};

/// What a path is drawn with, besides its shape.
#[derive(Clone, Debug, PartialEq)]
struct Style {
    width: f64,
    cap: NSLineCapStyle,
    join: NSLineJoinStyle,
    rule: NSWindingRule,
    miter: f64,
    flatness: f64,
    dash: Option<(Vec<f64>, f64)>,
}

/// The class defaults, for paths made from now on.
static DEFAULTS: Mutex<Style> = Mutex::new(Style {
    width: 1.0,
    cap: NSLineCapStyle::Butt,
    join: NSLineJoinStyle::Miter,
    rule: NSWindingRule::NonZero,
    miter: 10.0,
    flatness: 0.6,
    dash: None,
});

fn defaults() -> Style {
    DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_default(f: impl FnOnce(&mut Style)) {
    f(&mut DEFAULTS.lock().unwrap_or_else(|e| e.into_inner()));
}

pub(crate) struct PathIvars {
    path: RefCell<BezPath>,
    style: RefCell<Style>,
    /// Where the current subpath started, for `closePath`.
    start: Cell<Option<Point>>,
    /// The path as tiny-skia draws it, made on first draw.
    drawn: RefCell<Option<Arc<tiny_skia::Path>>>,
    caches: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a path is used by
    // one thread at a time.
    #[unsafe(super(NSObject))]
    #[name = "NSBezierPath"]
    #[ivars = PathIvars]
    pub(crate) struct NSBezierPathImpl;

    // Making paths.
    impl NSBezierPathImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ivars());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(bezierPath))]
        fn bezier_path() -> Retained<NSBezierPath> {
            new_path()
        }

        #[unsafe(method_id(bezierPathWithRect:))]
        fn with_rect(r: NSRect) -> Retained<NSBezierPath> {
            let p = new_path();
            imp(&p).append_rect(r);
            p
        }

        #[unsafe(method_id(bezierPathWithOvalInRect:))]
        fn with_oval(r: NSRect) -> Retained<NSBezierPath> {
            let p = new_path();
            imp(&p).append_oval(r);
            p
        }

        #[unsafe(method_id(bezierPathWithRoundedRect:xRadius:yRadius:))]
        fn with_rounded_rect(r: NSRect, rx: f64, ry: f64) -> Retained<NSBezierPath> {
            let p = new_path();
            imp(&p).append_rounded_rect(r, rx, ry);
            p
        }

        #[unsafe(method_id(bezierPathWithCGPath:))]
        fn with_cg_path(path: &CGPath) -> Retained<NSBezierPath> {
            // Element for element, as AppKit converts them.
            let shape = crate::coregraphics::path::path_imp(path).snapshot();
            from_bez(&shape.path)
        }
    }

    // CoreGraphics' paths.
    impl NSBezierPathImpl {
        #[unsafe(method(CGPath))]
        fn cg_path(&self) -> *mut CGPath {
            let shape = crate::coregraphics::path::Shape::from_path(self.ivars().path.borrow().clone());
            let path = crate::coregraphics::path::new_path(Arc::new(shape), false);
            Retained::autorelease_ptr(path.as_cg().retain())
        }

        #[unsafe(method(setCGPath:))]
        fn set_cg_path(&self, path: &CGPath) {
            let shape = crate::coregraphics::path::path_imp(path).snapshot();
            let start = shape.path.elements().iter().rev().find_map(|el| match el {
                PathEl::MoveTo(p) => Some(*p),
                _ => None,
            });
            self.edit(|p| *p = shape.path.clone());
            self.ivars().start.set(start);
        }
    }

    // Drawing with the class defaults.
    impl NSBezierPathImpl {
        #[unsafe(method(fillRect:))]
        fn fill_rect(r: NSRect) {
            crate::context::with_state(|st| {
                // As CoreGraphics' drawing does, AppKit's takes CoreGraphics'
                // current path.
                st.path = Default::default();
                st.fill_rect(r, st.gs.fill, st.gs.blend)
            });
        }

        #[unsafe(method(strokeRect:))]
        fn stroke_rect(r: NSRect) {
            let p = BezPath::from_vec(rect_elements(r));
            stroke_path(&to_skia(&p), &defaults());
        }

        #[unsafe(method(clipRect:))]
        fn clip_rect(r: NSRect) {
            crate::context::with_state(|st| st.clip_rect(r));
        }

        #[unsafe(method(strokeLineFromPoint:toPoint:))]
        fn stroke_line(a: NSPoint, b: NSPoint) {
            let mut p = BezPath::new();
            p.move_to(point(a));
            p.line_to(point(b));
            stroke_path(&to_skia(&p), &defaults());
        }

        #[unsafe(method(defaultMiterLimit))]
        fn default_miter_limit() -> f64 {
            defaults().miter
        }

        #[unsafe(method(setDefaultMiterLimit:))]
        fn set_default_miter_limit(v: f64) {
            set_default(|s| s.miter = v);
        }

        #[unsafe(method(defaultFlatness))]
        fn default_flatness() -> f64 {
            defaults().flatness
        }

        #[unsafe(method(setDefaultFlatness:))]
        fn set_default_flatness(v: f64) {
            set_default(|s| s.flatness = v);
        }

        #[unsafe(method(defaultWindingRule))]
        fn default_winding_rule() -> NSWindingRule {
            defaults().rule
        }

        #[unsafe(method(setDefaultWindingRule:))]
        fn set_default_winding_rule(v: NSWindingRule) {
            set_default(|s| s.rule = v);
        }

        #[unsafe(method(defaultLineCapStyle))]
        fn default_line_cap_style() -> NSLineCapStyle {
            defaults().cap
        }

        #[unsafe(method(setDefaultLineCapStyle:))]
        fn set_default_line_cap_style(v: NSLineCapStyle) {
            set_default(|s| s.cap = v);
        }

        #[unsafe(method(defaultLineJoinStyle))]
        fn default_line_join_style() -> NSLineJoinStyle {
            defaults().join
        }

        #[unsafe(method(setDefaultLineJoinStyle:))]
        fn set_default_line_join_style(v: NSLineJoinStyle) {
            set_default(|s| s.join = v);
        }

        #[unsafe(method(defaultLineWidth))]
        fn default_line_width() -> f64 {
            defaults().width
        }

        #[unsafe(method(setDefaultLineWidth:))]
        fn set_default_line_width(v: f64) {
            set_default(|s| s.width = v);
        }
    }

    // Building.
    impl NSBezierPathImpl {
        #[unsafe(method(moveToPoint:))]
        fn move_to(&self, p: NSPoint) {
            self.do_move(p);
        }

        #[unsafe(method(lineToPoint:))]
        fn line_to(&self, p: NSPoint) {
            self.do_line(p);
        }

        #[unsafe(method(curveToPoint:controlPoint1:controlPoint2:))]
        fn curve_to(&self, p: NSPoint, c1: NSPoint, c2: NSPoint) {
            self.do_curve(p, c1, c2);
        }

        #[unsafe(method(curveToPoint:controlPoint:))]
        fn quad_to(&self, p: NSPoint, c: NSPoint) {
            self.do_quad(p, c);
        }

        #[unsafe(method(closePath))]
        fn close_path(&self) {
            self.close();
        }

        #[unsafe(method(removeAllPoints))]
        fn remove_all_points(&self) {
            self.edit(|path| *path = BezPath::new());
            self.ivars().start.set(None);
        }

        #[unsafe(method(relativeMoveToPoint:))]
        fn relative_move_to(&self, d: NSPoint) {
            let p = self.current() + kurbo::Vec2::new(d.x, d.y);
            self.do_move(ns(p));
        }

        #[unsafe(method(relativeLineToPoint:))]
        fn relative_line_to(&self, d: NSPoint) {
            let at = self.current();
            self.do_line(ns(at + kurbo::Vec2::new(d.x, d.y)));
        }

        #[unsafe(method(relativeCurveToPoint:controlPoint1:controlPoint2:))]
        fn relative_curve_to(&self, p: NSPoint, c1: NSPoint, c2: NSPoint) {
            let at = self.current().to_vec2();
            let off = |q: NSPoint| ns(Point::new(q.x, q.y) + at);
            self.do_curve(off(p), off(c1), off(c2));
        }

        #[unsafe(method(relativeCurveToPoint:controlPoint:))]
        fn relative_quad_to(&self, p: NSPoint, c: NSPoint) {
            let at = self.current().to_vec2();
            let off = |q: NSPoint| ns(Point::new(q.x, q.y) + at);
            self.do_quad(off(p), off(c));
        }
    }

    // Stroke settings.
    impl NSBezierPathImpl {
        #[unsafe(method(lineWidth))]
        fn line_width(&self) -> f64 {
            self.ivars().style.borrow().width
        }

        #[unsafe(method(setLineWidth:))]
        fn set_line_width(&self, v: f64) {
            self.ivars().style.borrow_mut().width = v;
        }

        #[unsafe(method(lineCapStyle))]
        fn line_cap_style(&self) -> NSLineCapStyle {
            self.ivars().style.borrow().cap
        }

        #[unsafe(method(setLineCapStyle:))]
        fn set_line_cap_style(&self, v: NSLineCapStyle) {
            self.ivars().style.borrow_mut().cap = v;
        }

        #[unsafe(method(lineJoinStyle))]
        fn line_join_style(&self) -> NSLineJoinStyle {
            self.ivars().style.borrow().join
        }

        #[unsafe(method(setLineJoinStyle:))]
        fn set_line_join_style(&self, v: NSLineJoinStyle) {
            self.ivars().style.borrow_mut().join = v;
        }

        #[unsafe(method(windingRule))]
        fn winding_rule(&self) -> NSWindingRule {
            self.ivars().style.borrow().rule
        }

        #[unsafe(method(setWindingRule:))]
        fn set_winding_rule(&self, v: NSWindingRule) {
            self.ivars().style.borrow_mut().rule = v;
        }

        #[unsafe(method(miterLimit))]
        fn miter_limit(&self) -> f64 {
            self.ivars().style.borrow().miter
        }

        #[unsafe(method(setMiterLimit:))]
        fn set_miter_limit(&self, v: f64) {
            self.ivars().style.borrow_mut().miter = v;
        }

        #[unsafe(method(flatness))]
        fn flatness(&self) -> f64 {
            self.ivars().style.borrow().flatness
        }

        #[unsafe(method(setFlatness:))]
        fn set_flatness(&self, v: f64) {
            self.ivars().style.borrow_mut().flatness = v;
        }

        #[unsafe(method(setLineDash:count:phase:))]
        fn set_line_dash(&self, pattern: *const f64, count: NSInteger, phase: f64) {
            let count = usize::try_from(count).unwrap_or(0);
            // No pattern is no dash; a pattern of no lengths draws solid
            // but keeps its phase, as AppKit's does.
            let dash = if pattern.is_null() {
                None
            } else if count == 0 {
                Some((Vec::new(), phase))
            } else {
                // SAFETY: the caller passes `count` lengths.
                Some((unsafe { std::slice::from_raw_parts(pattern, count) }.to_vec(), phase))
            };
            self.ivars().style.borrow_mut().dash = dash;
        }

        #[unsafe(method(getLineDash:count:phase:))]
        fn get_line_dash(&self, pattern: *mut f64, count: *mut NSInteger, phase: *mut f64) {
            let style = self.ivars().style.borrow();
            let (lengths, p) = style.dash.as_ref().map_or((&[][..], 0.0), |(l, p)| (&l[..], *p));
            // SAFETY: each pointer is null or has room for what it gets
            // (`pattern` for the count the caller asked for first).
            unsafe {
                if !pattern.is_null() {
                    std::ptr::copy_nonoverlapping(lengths.as_ptr(), pattern, lengths.len());
                }
                if !count.is_null() {
                    *count = lengths.len() as NSInteger;
                }
                if !phase.is_null() {
                    *phase = p;
                }
            }
        }

        #[unsafe(method(cachesBezierPath))]
        fn caches_bezier_path(&self) -> bool {
            self.ivars().caches.get()
        }

        #[unsafe(method(setCachesBezierPath:))]
        fn set_caches_bezier_path(&self, flag: bool) {
            self.ivars().caches.set(flag);
        }
    }

    // Drawing and clipping.
    impl NSBezierPathImpl {
        #[unsafe(method(fill))]
        fn fill(&self) {
            let Some(path) = self.drawn() else { return };
            let even_odd = self.ivars().style.borrow().rule == NSWindingRule::EvenOdd;
            crate::context::with_state(|st| {
                st.fill_leftover(even_odd);
                let op = Op::FillPath { path, even_odd, paint: Paint::Solid(st.gs.fill), draw: st.gs.draw() };
                st.push(op);
            });
        }

        #[unsafe(method(stroke))]
        fn stroke(&self) {
            let Some(path) = self.drawn() else { return };
            let style = self.ivars().style.borrow().clone();
            stroke_path(&Some(path), &style);
        }

        #[unsafe(method(addClip))]
        fn add_clip(&self) {
            let even_odd = self.ivars().style.borrow().rule == NSWindingRule::EvenOdd;
            let path = self.drawn().unwrap_or_else(|| Arc::new(empty_path()));
            crate::context::with_state(|st| st.clip_path(path, even_odd, false));
        }

        #[unsafe(method(setClip))]
        fn set_clip(&self) {
            let even_odd = self.ivars().style.borrow().rule == NSWindingRule::EvenOdd;
            let path = self.drawn().unwrap_or_else(|| Arc::new(empty_path()));
            crate::context::with_state(|st| st.clip_path(path, even_odd, true));
        }
    }

    // Derived paths.
    impl NSBezierPathImpl {
        #[unsafe(method_id(bezierPathByFlatteningPath))]
        fn by_flattening(&self) -> Retained<NSBezierPath> {
            let tolerance = self.ivars().style.borrow().flatness.max(0.01);
            let mut flat = BezPath::new();
            kurbo::flatten(self.ivars().path.borrow().iter(), tolerance, |el| flat.push(el));
            self.derived(flat)
        }

        #[unsafe(method_id(bezierPathByReversingPath))]
        fn by_reversing(&self) -> Retained<NSBezierPath> {
            let reversed = reverse(&self.ivars().path.borrow());
            self.derived(reversed)
        }

        #[unsafe(method(transformUsingAffineTransform:))]
        fn transform_using(&self, t: &NSAffineTransform) {
            let a = affine_of(t);
            self.edit(|path| path.apply_affine(a));
            if let Some(s) = self.ivars().start.get() {
                self.ivars().start.set(Some(a * s));
            }
        }
    }

    // Queries.
    impl NSBezierPathImpl {
        #[unsafe(method(isEmpty))]
        fn is_empty(&self) -> bool {
            self.ivars().path.borrow().elements().is_empty()
        }

        #[unsafe(method(currentPoint))]
        fn current_point(&self) -> NSPoint {
            ns(self.current())
        }

        #[unsafe(method(controlPointBounds))]
        fn control_point_bounds(&self) -> NSRect {
            let path = self.ivars().path.borrow();
            if path.elements().is_empty() {
                return NSRect::ZERO;
            }
            rect_of(path.control_box())
        }

        #[unsafe(method(bounds))]
        fn bounds(&self) -> NSRect {
            tight_bounds(&self.ivars().path.borrow()).map_or(NSRect::ZERO, rect_of)
        }

        #[unsafe(method(elementCount))]
        fn element_count(&self) -> NSInteger {
            self.ivars().path.borrow().elements().len() as NSInteger
        }

        #[unsafe(method(elementAtIndex:))]
        fn element_at_index(&self, index: NSInteger) -> NSBezierPathElement {
            self.element(index, std::ptr::null_mut())
        }

        #[unsafe(method(elementAtIndex:associatedPoints:))]
        fn element_at_index_points(&self, index: NSInteger, points: *mut NSPoint) -> NSBezierPathElement {
            self.element(index, points)
        }

        #[unsafe(method(setAssociatedPoints:atIndex:))]
        fn set_associated_points(&self, points: *mut NSPoint, index: NSInteger) {
            if points.is_null() {
                return;
            }
            let Ok(i) = usize::try_from(index) else { return };
            // SAFETY: the caller passes as many points as the element has.
            let at = |k: usize| unsafe { point(*points.add(k)) };
            self.edit(|path| {
                if let Some(el) = path.elements_mut().get_mut(i) {
                    *el = match *el {
                        PathEl::MoveTo(_) => PathEl::MoveTo(at(0)),
                        PathEl::LineTo(_) => PathEl::LineTo(at(0)),
                        PathEl::QuadTo(..) => PathEl::QuadTo(at(0), at(1)),
                        PathEl::CurveTo(..) => PathEl::CurveTo(at(0), at(1), at(2)),
                        PathEl::ClosePath => PathEl::ClosePath,
                    };
                }
            });
        }

        #[unsafe(method(containsPoint:))]
        fn contains_point(&self, p: NSPoint) -> bool {
            let closed = closed(&self.ivars().path.borrow());
            // A point on the outline is inside, whatever the rule; the
            // winding number counts only half of each edge's points.
            on_outline(&closed, point(p)) || {
                let winding = closed.winding(point(p));
                match self.ivars().style.borrow().rule {
                    NSWindingRule::EvenOdd => winding % 2 != 0,
                    _ => winding != 0,
                }
            }
        }
    }

    // Appending.
    impl NSBezierPathImpl {
        #[unsafe(method(appendBezierPath:))]
        fn append_path(&self, other: &NSBezierPath) {
            let other = imp(other).ivars().path.borrow().clone();
            let start = other.elements().iter().rev().find_map(|el| match el {
                PathEl::MoveTo(p) => Some(*p),
                _ => None,
            });
            self.edit(|path| path.extend(other.iter()));
            if start.is_some() {
                self.ivars().start.set(start);
            }
        }

        #[unsafe(method(appendBezierPathWithRect:))]
        fn append_bezier_path_with_rect(&self, r: NSRect) {
            self.append_rect(r);
        }

        #[unsafe(method(appendBezierPathWithPoints:count:))]
        fn append_points(&self, points: *mut NSPoint, count: NSInteger) {
            let count = usize::try_from(count).unwrap_or(0);
            if points.is_null() || count == 0 {
                return;
            }
            // SAFETY: the caller passes `count` points.
            let points = unsafe { std::slice::from_raw_parts(points, count) };
            // They continue the current subpath, if there's a current
            // point, with a line to the first.
            if self.ivars().path.borrow().elements().is_empty() {
                self.do_move(points[0]);
            } else {
                self.do_line(points[0]);
            }
            for p in &points[1..] {
                self.do_line(*p);
            }
        }

        #[unsafe(method(appendBezierPathWithOvalInRect:))]
        fn append_bezier_path_with_oval(&self, r: NSRect) {
            self.append_oval(r);
        }

        #[unsafe(method(appendBezierPathWithArcWithCenter:radius:startAngle:endAngle:clockwise:))]
        fn append_arc_clockwise(&self, c: NSPoint, r: f64, start: f64, end: f64, clockwise: bool) {
            self.append_arc(point(c), r, start, end, clockwise);
        }

        #[unsafe(method(appendBezierPathWithArcWithCenter:radius:startAngle:endAngle:))]
        fn append_arc_ccw(&self, c: NSPoint, r: f64, start: f64, end: f64) {
            self.append_arc(point(c), r, start, end, false);
        }

        #[unsafe(method(appendBezierPathWithArcFromPoint:toPoint:radius:))]
        fn append_tangent_arc(&self, p1: NSPoint, p2: NSPoint, r: f64) {
            self.tangent_arc(point(p1), point(p2), r);
        }

        #[unsafe(method(appendBezierPathWithRoundedRect:xRadius:yRadius:))]
        fn append_bezier_path_with_rounded_rect(&self, r: NSRect, rx: f64, ry: f64) {
            self.append_rounded_rect(r, rx, ry);
        }
    }

    impl NSBezierPathImpl {
        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSBezierPath> {
            // The shape and settings; the drawn form is shared until either
            // changes.
            let p = new_path();
            let it = imp(&p).ivars();
            *it.path.borrow_mut() = self.ivars().path.borrow().clone();
            *it.style.borrow_mut() = self.ivars().style.borrow().clone();
            it.start.set(self.ivars().start.get());
            *it.drawn.borrow_mut() = self.ivars().drawn.borrow().clone();
            it.caches.set(self.ivars().caches.get());
            p
        }
    }

    unsafe impl NSObjectProtocol for NSBezierPathImpl {}

    unsafe impl NSCopying for NSBezierPathImpl {}
);

fn ivars() -> PathIvars {
    PathIvars {
        path: RefCell::new(BezPath::new()),
        style: RefCell::new(defaults()),
        start: Cell::new(None),
        drawn: RefCell::new(None),
        caches: Cell::new(false),
    }
}

fn new_path() -> Retained<NSBezierPath> {
    crate::load_shell::<NSBezierPath>();
    let this = NSBezierPathImpl::alloc().set_ivars(ivars());
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSBezierPathImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSBezierPathImpl is the class NSBezierPath names.
    unsafe { Retained::cast_unchecked(this) }
}

pub(crate) fn imp(p: &NSBezierPath) -> &NSBezierPathImpl {
    // SAFETY: every NSBezierPath is an NSBezierPathImpl.
    unsafe { &*(p as *const NSBezierPath).cast::<NSBezierPathImpl>() }
}

fn point(p: NSPoint) -> Point {
    Point::new(p.x, p.y)
}

fn ns(p: Point) -> NSPoint {
    NSPoint::new(p.x, p.y)
}

fn rect_of(r: kurbo::Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x0, r.y0), NSSize::new(r.width(), r.height()))
}

/// The transform an `NSAffineTransform` holds.
pub(crate) fn affine_of(t: &NSAffineTransform) -> Affine {
    let s = t.transformStruct();
    Affine::new([s.m11, s.m12, s.m21, s.m22, s.tX, s.tY])
}

fn rect_elements(r: NSRect) -> Vec<PathEl> {
    let (x0, y0) = (r.origin.x, r.origin.y);
    let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
    vec![
        PathEl::MoveTo(Point::new(x0, y0)),
        PathEl::LineTo(Point::new(x1, y0)),
        PathEl::LineTo(Point::new(x1, y1)),
        PathEl::LineTo(Point::new(x0, y1)),
        PathEl::ClosePath,
    ]
}

/// How far a cubic's control points sit along the tangents, as a fraction
/// of the radius, to draw an arc of `angle` radians.
pub(crate) fn kappa(angle: f64) -> f64 {
    4.0 / 3.0 * (angle / 4.0).tan()
}

impl NSBezierPathImpl {
    fn do_move(&self, p: NSPoint) {
        self.edit(|path| path.move_to(point(p)));
        self.ivars().start.set(Some(point(p)));
    }

    fn do_line(&self, p: NSPoint) {
        self.ensure_start(point(p));
        self.edit(|path| path.line_to(point(p)));
    }

    fn do_curve(&self, p: NSPoint, c1: NSPoint, c2: NSPoint) {
        self.ensure_start(point(p));
        self.edit(|path| path.curve_to(point(c1), point(c2), point(p)));
    }

    fn do_quad(&self, p: NSPoint, c: NSPoint) {
        self.ensure_start(point(p));
        self.edit(|path| path.quad_to(point(c), point(p)));
    }

    /// Change the path, dropping what drawing made of it.
    fn edit(&self, f: impl FnOnce(&mut BezPath)) {
        f(&mut self.ivars().path.borrow_mut());
        self.ivars().drawn.borrow_mut().take();
    }

    fn current(&self) -> Point {
        let path = self.ivars().path.borrow();
        match path.elements().last() {
            Some(PathEl::MoveTo(p) | PathEl::LineTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p)) => *p,
            Some(PathEl::ClosePath) => self.ivars().start.get().unwrap_or(Point::ZERO),
            None => Point::ZERO,
        }
    }

    /// A segment on an empty path starts where it ends (AppKit raises).
    fn ensure_start(&self, p: Point) {
        if self.ivars().path.borrow().elements().is_empty() {
            self.do_move(ns(p));
        }
    }

    fn close(&self) {
        let Some(start) = self.ivars().start.get() else { return };
        // A close, then a move back to where the subpath began, as AppKit
        // records it.
        self.edit(|path| {
            path.close_path();
            path.move_to(start);
        });
    }

    fn derived(&self, path: BezPath) -> Retained<NSBezierPath> {
        let p = new_path();
        let it = imp(&p);
        *it.ivars().style.borrow_mut() = self.ivars().style.borrow().clone();
        let start = path.elements().iter().rev().find_map(|el| match el {
            PathEl::MoveTo(p) => Some(*p),
            _ => None,
        });
        *it.ivars().path.borrow_mut() = path;
        it.ivars().start.set(start);
        p
    }

    fn element(&self, index: NSInteger, out: *mut NSPoint) -> NSBezierPathElement {
        let path = self.ivars().path.borrow();
        let el = usize::try_from(index).ok().and_then(|i| path.elements().get(i).copied());
        let Some(el) = el else {
            panic!("*** -[NSBezierPath elementAtIndex:associatedPoints:]: index ({index}) beyond bounds");
        };
        let (kind, points): (_, &[Point]) = match &el {
            PathEl::MoveTo(p) => (NSBezierPathElement::MoveTo, std::slice::from_ref(p)),
            PathEl::LineTo(p) => (NSBezierPathElement::LineTo, std::slice::from_ref(p)),
            PathEl::QuadTo(c, p) => (NSBezierPathElement::QuadraticCurveTo, &[*c, *p][..]),
            PathEl::CurveTo(c1, c2, p) => (NSBezierPathElement::CubicCurveTo, &[*c1, *c2, *p][..]),
            PathEl::ClosePath => (NSBezierPathElement::ClosePath, &[][..]),
        };
        if !out.is_null() {
            for (i, p) in points.iter().enumerate() {
                // SAFETY: the caller passes room for the element's points
                // (three at most).
                unsafe { *out.add(i) = ns(*p) };
            }
        }
        kind
    }

    fn append_rect(&self, r: NSRect) {
        self.edit(|path| path.extend(rect_elements(r)));
        self.ivars().start.set(Some(Point::new(r.origin.x, r.origin.y)));
    }

    /// Four quarter ellipses, counterclockwise from the bottom right, not
    /// closed.
    fn append_oval(&self, r: NSRect) {
        let (cx, cy) = (r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0);
        let (rx, ry) = (r.size.width / 2.0, r.size.height / 2.0);
        let at = |deg: f64| {
            let a = deg.to_radians();
            (Point::new(cx + rx * a.cos(), cy + ry * a.sin()), kurbo::Vec2::new(-rx * a.sin(), ry * a.cos()))
        };
        let k = kappa(std::f64::consts::FRAC_PI_2);
        let (start, _) = at(-45.0);
        self.edit(|path| {
            path.move_to(start);
            for i in 0..4 {
                let a0 = -45.0 + 90.0 * f64::from(i);
                let ((p0, d0), (p1, d1)) = (at(a0), at(a0 + 90.0));
                path.curve_to(p0 + d0 * k, p1 - d1 * k, p1);
            }
        });
        self.ivars().start.set(Some(start));
    }

    /// Counterclockwise from the top edge's left end, closed. A rectangle
    /// of negative width or height adds nothing, as in AppKit.
    fn append_rounded_rect(&self, r: NSRect, rx: f64, ry: f64) {
        let (w, h) = (r.size.width, r.size.height);
        if w < 0.0 || h < 0.0 {
            return;
        }
        // (Not `clamp`, which a NaN would make panic.)
        let (rx, ry) = (rx.max(0.0).min(w / 2.0), ry.max(0.0).min(h / 2.0));
        let (x0, y0, x1, y1) = (r.origin.x, r.origin.y, r.origin.x + w, r.origin.y + h);
        if rx <= 0.0 || ry <= 0.0 {
            self.do_move(NSPoint::new(x0, y0));
            self.do_line(NSPoint::new(x1, y0));
            self.do_line(NSPoint::new(x1, y1));
            self.do_line(NSPoint::new(x0, y1));
            self.close();
            return;
        }
        let (kx, ky) = (rx * kappa(std::f64::consts::FRAC_PI_2), ry * kappa(std::f64::consts::FRAC_PI_2));
        let start = Point::new(x0 + rx, y1);
        self.edit(|path| {
            path.move_to(start);
            path.curve_to((x0 + rx - kx, y1), (x0, y1 - ry + ky), (x0, y1 - ry));
            path.line_to((x0, y0 + ry));
            path.curve_to((x0, y0 + ry - ky), (x0 + rx - kx, y0), (x0 + rx, y0));
            path.line_to((x1 - rx, y0));
            path.curve_to((x1 - rx + kx, y0), (x1, y0 + ry - ky), (x1, y0 + ry));
            path.line_to((x1, y1 - ry));
            path.curve_to((x1, y1 - ry + ky), (x1 - rx + kx, y1), (x1 - rx, y1));
        });
        self.ivars().start.set(Some(start));
        self.close();
    }

    /// An arc of the circle around `c`, from `start` to `end` degrees
    /// counterclockwise (in unflipped coordinates) or clockwise: a quarter
    /// turn a curve from the start, then what's left, as AppKit splits it.
    /// A sweep of more than a turn goes round again, as AppKit's does; one
    /// that goes the wrong way is taken the other way round. It begins with
    /// a move on an empty path and a line from the current point otherwise.
    fn append_arc(&self, c: Point, r: f64, start: f64, end: f64, clockwise: bool) {
        let mut sweep = if clockwise { start - end } else { end - start };
        if !sweep.is_finite() {
            return;
        }
        while sweep < 0.0 {
            sweep += 360.0;
        }
        self.append_sweep(c, r, start, sweep, clockwise);
    }

    /// An arc of `sweep` degrees (none or more) from `start`, as
    /// [`append_arc`](Self::append_arc) draws it.
    fn append_sweep(&self, c: Point, r: f64, start: f64, sweep: f64, clockwise: bool) {
        let dir = if clockwise { -1.0 } else { 1.0 };
        let at = |deg: f64| {
            let a = deg.to_radians();
            (Point::new(c.x + r * a.cos(), c.y + r * a.sin()), kurbo::Vec2::new(-r * a.sin(), r * a.cos()) * dir)
        };
        let (p0, _) = at(start);
        if self.ivars().path.borrow().elements().is_empty() {
            self.do_move(ns(p0));
        } else {
            self.do_line(ns(p0));
        }
        let quarter = kappa(std::f64::consts::FRAC_PI_2);
        self.edit(|path| {
            let (mut a0, mut left) = (start, sweep);
            while left > 0.0 {
                let step = left.min(90.0);
                let k = if step == 90.0 { quarter } else { kappa(step.to_radians()) };
                let ((q0, d0), (q1, d1)) = (at(a0), at(a0 + dir * step));
                path.curve_to(q0 + d0 * k, q1 - d1 * k, q1);
                a0 += dir * step;
                left -= step;
            }
        });
    }

    /// A line toward `p1`, then an arc of radius `r` tangent to the lines
    /// from the current point to `p1` and from `p1` to `p2`.
    fn tangent_arc(&self, p1: Point, p2: Point, r: f64) {
        let p0 = self.current();
        let (u, v) = ((p0 - p1).normalize(), (p2 - p1).normalize());
        let cos = u.dot(v);
        let angle = cos.clamp(-1.0, 1.0).acos();
        if !(angle.is_finite() && angle > 1e-9 && (std::f64::consts::PI - angle) > 1e-9) || r <= 0.0 {
            // Collinear: a line to the corner.
            self.do_line(ns(p1));
            return;
        }
        let d = r / (angle / 2.0).tan();
        let t0 = p1 + u * d;
        let bisector = (u + v).normalize();
        let center = p1 + bisector * (r / (angle / 2.0).sin());
        let clockwise = u.cross(v) > 0.0;
        let a0 = (t0 - center).atan2().to_degrees();
        // The turn from one tangent to the other, less than half a turn;
        // a right angle's is a quarter, however the angle rounded.
        let sweep = 180.0 - angle.to_degrees();
        let sweep = if (sweep - sweep.round()).abs() < 1e-9 { sweep.round() } else { sweep };
        self.append_sweep(center, r, a0, sweep, clockwise);
    }

    /// The tiny-skia path, made on first draw and kept until a change.
    fn drawn(&self) -> Option<Arc<tiny_skia::Path>> {
        if let Some(p) = self.ivars().drawn.borrow().as_ref() {
            return Some(p.clone());
        }
        let made = to_skia(&self.ivars().path.borrow())?;
        *self.ivars().drawn.borrow_mut() = Some(made.clone());
        Some(made)
    }
}

/// A new path of `shape`, with the default stroke settings.
pub(crate) fn from_bez(shape: &BezPath) -> Retained<NSBezierPath> {
    let p = new_path();
    let start = shape.elements().iter().rev().find_map(|el| match el {
        PathEl::MoveTo(p) => Some(*p),
        _ => None,
    });
    let it = imp(&p);
    *it.ivars().path.borrow_mut() = shape.clone();
    it.ivars().start.set(start);
    p
}

/// A copy of `p`'s shape.
pub(crate) fn bez_path(p: &NSBezierPath) -> BezPath {
    imp(p).ivars().path.borrow().clone()
}

/// `path` as tiny-skia draws it.
pub(crate) fn to_skia(path: &BezPath) -> Option<Arc<tiny_skia::Path>> {
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

fn empty_path() -> tiny_skia::Path {
    // A degenerate rectangle: clipping to it leaves nothing.
    tiny_skia::PathBuilder::from_rect(tiny_skia::Rect::from_xywh(0.0, 0.0, 0.0001, 0.0001).expect("a rectangle"))
}

fn stroke_path(path: &Option<Arc<tiny_skia::Path>>, style: &Style) {
    let Some(path) = path else { return };
    let spec = StrokeSpec {
        width: style.width.max(0.0) as f32,
        cap: style.cap.0 as u8,
        join: style.join.0 as u8,
        miter: style.miter.max(1.0) as f32,
        dash: style
            .dash
            .as_ref()
            .filter(|(l, _)| !l.is_empty())
            .map(|(l, p)| (l.iter().map(|&v| v as f32).collect(), *p as f32)),
    };
    let stroke = Arc::new(spec);
    crate::context::with_state(|st| {
        st.stroke_leftover(&stroke);
        let op = Op::StrokePath { path: path.clone(), stroke, paint: Paint::Solid(st.gs.stroke), draw: st.gs.draw() };
        st.push(op);
    });
}

/// The tight bounds: curve extrema, not control points; a lone move
/// counts as a point.
pub(crate) fn tight_bounds(path: &BezPath) -> Option<kurbo::Rect> {
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
pub(crate) fn on_outline(path: &BezPath, p: Point) -> bool {
    let eps = 1e-9 * (1.0 + p.x.abs().max(p.y.abs()));
    path.segments().any(|seg| {
        let b = seg.bounding_box().inflate(eps, eps);
        b.contains(p) && seg.nearest(p, eps * 0.1).distance_sq <= eps * eps
    })
}

/// The path with every open subpath closed, as filling sees it.
pub(crate) fn closed(path: &BezPath) -> BezPath {
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

/// Each subpath backward, as AppKit reverses them: an open one starts at
/// its old end; a closed one keeps its start and runs the other way round.
fn reverse(path: &BezPath) -> BezPath {
    let mut out = BezPath::new();
    let els = path.elements();
    let mut i = 0;
    while i < els.len() {
        let PathEl::MoveTo(start) = els[i] else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < els.len() && !matches!(els[j], PathEl::MoveTo(_) | PathEl::ClosePath) {
            j += 1;
        }
        let is_closed = j < els.len() && matches!(els[j], PathEl::ClosePath);
        // The segments, as (from, element) pairs.
        let mut segs = Vec::new();
        let mut at = start;
        for el in &els[i + 1..j] {
            let end = match *el {
                PathEl::LineTo(p) | PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => p,
                _ => at,
            };
            segs.push((at, *el));
            at = end;
        }
        let backward = |out: &mut BezPath, segs: &[(Point, PathEl)]| {
            for &(from, el) in segs.iter().rev() {
                match el {
                    PathEl::LineTo(_) => out.line_to(from),
                    PathEl::QuadTo(c, _) => out.quad_to(c, from),
                    PathEl::CurveTo(c1, c2, _) => out.curve_to(c2, c1, from),
                    _ => {}
                }
            }
        };
        if is_closed {
            out.move_to(start);
            if at != start {
                out.line_to(at);
            }
            backward(&mut out, &segs[..]);
            // The last segment backward ends at the start: close instead.
            if let Some(PathEl::LineTo(p)) = out.elements().last().copied()
                && p == start
                && !segs.is_empty()
            {
                out.pop();
            }
            out.close_path();
            out.move_to(start);
            j += 1;
            // Skip the move AppKit put after the close.
            if matches!(els.get(j), Some(PathEl::MoveTo(p)) if *p == start) {
                j += 1;
            }
        } else {
            out.move_to(at);
            backward(&mut out, &segs);
        }
        i = j;
    }
    out
}

// NSAffineTransform's AppKit methods, copied onto the Foundation class by
// the loader of a helper class's shell, as a category would add them.

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepAffineDrawing"]
    struct AffineDrawing;

    impl AffineDrawing {
        #[unsafe(method(set))]
        fn set(&self) {
            let t = affine_of(this_transform(self));
            crate::context::with_state(|st| st.set_ctm(st.base * t));
        }

        #[unsafe(method(concat))]
        fn concat(&self) {
            let t = affine_of(this_transform(self));
            crate::context::with_state(|st| st.set_ctm(st.gs.ctm * t));
        }

        #[unsafe(method_id(transformBezierPath:))]
        fn transform_bezier_path(&self, path: &NSBezierPath) -> Retained<NSBezierPath> {
            let t = affine_of(this_transform(self));
            let src = imp(path);
            let mut p = src.ivars().path.borrow().clone();
            p.apply_affine(t);
            src.derived(p)
        }
    }
);

fn this_transform<T>(this: &T) -> &NSAffineTransform {
    // SAFETY: these methods are installed on NSAffineTransform and only run
    // with a transform as the receiver.
    unsafe { &*(this as *const T).cast::<NSAffineTransform>() }
}

sidestep_runtime::static_class!(
    pub(crate) AFFINE_DRAWING,
    AFFINE_DRAWING_META = "_SidestepAffineDrawing",
    load_affine_drawing
);

/// Give `NSAffineTransform` its drawing methods, once. Messages the
/// helper's shell, whose loader does the work under the runtime's class
/// loading lock (see `string_drawing::install_string_drawing`).
pub(crate) fn install_affine_drawing() {
    let shell = objc2::class!(_SidestepAffineDrawing);
    // SAFETY: +class takes nothing and returns the receiver.
    let _: *const objc2::runtime::AnyClass = unsafe { msg_send![shell, class] };
}

fn load_affine_drawing() {
    let helper = AffineDrawing::class();
    let target = <NSAffineTransform as ClassType>::class();
    for sel in [objc2::sel!(set), objc2::sel!(concat), objc2::sel!(transformBezierPath:)] {
        let method = helper.instance_method(sel).expect("helper method");
        // SAFETY: the implementation treats its receiver as an
        // NSAffineTransform, and the encoding is the helper method's own.
        unsafe {
            objc2::ffi::class_addMethod(
                (target as *const objc2::runtime::AnyClass).cast_mut(),
                sel,
                method.implementation(),
                method_types(method),
            );
        }
    }
}

fn method_types(method: &objc2::runtime::Method) -> *const std::ffi::c_char {
    // SAFETY: method_getTypeEncoding returns the method's own encoding,
    // which lives as long as the method.
    unsafe { objc2::ffi::method_getTypeEncoding(method) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(els: &[PathEl]) -> BezPath {
        BezPath::from_vec(els.to_vec())
    }

    #[test]
    fn reversing_keeps_closed_starts_and_flips_open_ends() {
        let p = |x, y| Point::new(x, y);
        let rect = path(&rect_elements(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(2.0, 2.0))));
        let r = reverse(&rect);
        assert_eq!(
            r.elements(),
            [
                PathEl::MoveTo(p(0.0, 0.0)),
                PathEl::LineTo(p(0.0, 2.0)),
                PathEl::LineTo(p(2.0, 2.0)),
                PathEl::LineTo(p(2.0, 0.0)),
                PathEl::ClosePath,
                PathEl::MoveTo(p(0.0, 0.0)),
            ]
        );
        let open = path(&[
            PathEl::MoveTo(p(0.0, 0.0)),
            PathEl::LineTo(p(5.0, 0.0)),
            PathEl::CurveTo(p(7.0, 0.0), p(10.0, 3.0), p(10.0, 5.0)),
        ]);
        assert_eq!(
            reverse(&open).elements(),
            [
                PathEl::MoveTo(p(10.0, 5.0)),
                PathEl::CurveTo(p(10.0, 3.0), p(7.0, 0.0), p(5.0, 0.0)),
                PathEl::LineTo(p(0.0, 0.0)),
            ]
        );
    }

    #[test]
    fn open_subpaths_fill_as_if_closed() {
        let tri = path(&[
            PathEl::MoveTo(Point::ZERO),
            PathEl::LineTo((10.0, 0.0).into()),
            PathEl::LineTo((0.0, 10.0).into()),
        ]);
        assert_ne!(closed(&tri).winding((2.0, 2.0).into()), 0);
    }
}
