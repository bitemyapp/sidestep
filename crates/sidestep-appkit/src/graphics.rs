//! Drawing during `drawRect:`. Nothing here touches pixels: `NSColor`,
//! `NSBezierPath` and string drawing (`string_drawing`) append [`Op`]s to
//! the recorder of the view being drawn, in its layer's coordinates, for
//! the render thread.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::NSColor;
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::protocol::{Color, Op, Rect};

/// Maps a view's coordinates to its layer's: `x' = x + tx`, `y' = a·y + ty`
/// with `a = ±1` (views may be flipped relative to each other).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Xf {
    pub tx: f64,
    pub a: f64,
    pub ty: f64,
}

impl Xf {
    pub const IDENTITY: Xf = Xf { tx: 0.0, a: 1.0, ty: 0.0 };

    pub fn point(&self, x: f64, y: f64) -> (f64, f64) {
        (x + self.tx, self.a * y + self.ty)
    }

    pub fn rect(&self, r: NSRect) -> Rect {
        let (x0, y0) = self.point(r.origin.x, r.origin.y);
        let (x1, y1) = self.point(r.origin.x + r.size.width, r.origin.y + r.size.height);
        Rect::new(x0.min(x1) as f32, y0.min(y1) as f32, x0.max(x1) as f32, y0.max(y1) as f32)
    }

    pub fn inverse(&self) -> Xf {
        // y = a·y' ... solved for y: y = a·(y' - ty), since a = ±1.
        Xf { tx: -self.tx, a: self.a, ty: -self.a * self.ty }
    }

    pub fn inverse_rect(&self, r: Rect) -> NSRect {
        let inv = self.inverse();
        let (x0, y0) = inv.point(r.x0 as f64, r.y0 as f64);
        let (x1, y1) = inv.point(r.x1 as f64, r.y1 as f64);
        NSRect::new(NSPoint::new(x0.min(x1), y0.min(y1)), NSSize::new((x1 - x0).abs(), (y1 - y0).abs()))
    }

    /// `outer ∘ self`: first this map, then `outer`.
    pub fn then(&self, outer: &Xf) -> Xf {
        Xf { tx: self.tx + outer.tx, a: self.a * outer.a, ty: outer.a * self.ty + outer.ty }
    }
}

/// What the view being drawn records into.
pub(crate) struct Recorder {
    pub ops: Vec<Op>,
    pub xf: Xf,
    pub clip: Rect,
    fill: Color,
    stroke: Color,
    /// Text drawn before being laid out, to lay out together at the end.
    pub pending: crate::string_drawing::Pending,
}

thread_local!(static CURRENT: RefCell<Option<Recorder>> = const { RefCell::new(None) });

pub(crate) fn begin_recording() {
    CURRENT.with(|c| {
        *c.borrow_mut() = Some(Recorder {
            ops: Vec::new(),
            xf: Xf::IDENTITY,
            clip: Rect::default(),
            fill: [0.0, 0.0, 0.0, 1.0],
            stroke: [0.0, 0.0, 0.0, 1.0],
            pending: Default::default(),
        })
    });
}

pub(crate) fn end_recording() -> Vec<Op> {
    let Some(rec) = CURRENT.with(|c| c.borrow_mut().take()) else { return Vec::new() };
    crate::string_drawing::finish(rec.ops, rec.pending)
}

/// Whether a view is being drawn.
pub(crate) fn recording() -> bool {
    CURRENT.with(|c| c.borrow().is_some())
}

/// Position the recorder for the view about to draw.
pub(crate) fn set_view(xf: Xf, clip: Rect) {
    with_recorder(|r| {
        r.xf = xf;
        r.clip = clip;
    });
}

/// Record directly, outside any view (the window background).
pub(crate) fn push(op: Op) {
    with_recorder(|r| r.ops.push(op));
}

pub(crate) fn with_recorder(f: impl FnOnce(&mut Recorder)) {
    CURRENT.with(|c| {
        if let Some(r) = c.borrow_mut().as_mut() {
            f(r)
        }
    });
}

// NSColor

pub(crate) struct ColorIvars {
    rgba: [f64; 4],
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSColor"]
    #[ivars = ColorIvars]
    pub(crate) struct NSColorImpl;

    impl NSColorImpl {
        #[unsafe(method_id(colorWithSRGBRed:green:blue:alpha:))]
        fn srgb(r: f64, g: f64, b: f64, a: f64) -> Retained<Self> {
            color(r, g, b, a)
        }

        #[unsafe(method_id(colorWithRed:green:blue:alpha:))]
        fn rgb(r: f64, g: f64, b: f64, a: f64) -> Retained<Self> {
            color(r, g, b, a)
        }

        #[unsafe(method_id(colorWithCalibratedRed:green:blue:alpha:))]
        fn calibrated(r: f64, g: f64, b: f64, a: f64) -> Retained<Self> {
            color(r, g, b, a)
        }

        #[unsafe(method_id(colorWithDeviceRed:green:blue:alpha:))]
        fn device(r: f64, g: f64, b: f64, a: f64) -> Retained<Self> {
            color(r, g, b, a)
        }

        #[unsafe(method_id(colorWithWhite:alpha:))]
        fn white_alpha(w: f64, a: f64) -> Retained<Self> {
            color(w, w, w, a)
        }

        #[unsafe(method_id(blackColor))]
        fn black() -> Retained<Self> {
            color(0.0, 0.0, 0.0, 1.0)
        }

        #[unsafe(method_id(whiteColor))]
        fn white() -> Retained<Self> {
            color(1.0, 1.0, 1.0, 1.0)
        }

        #[unsafe(method_id(clearColor))]
        fn clear() -> Retained<Self> {
            color(0.0, 0.0, 0.0, 0.0)
        }

        #[unsafe(method_id(redColor))]
        fn red() -> Retained<Self> {
            color(1.0, 0.0, 0.0, 1.0)
        }

        #[unsafe(method_id(textColor))]
        fn text() -> Retained<Self> {
            color(0.0, 0.0, 0.0, 0.85)
        }

        #[unsafe(method_id(windowBackgroundColor))]
        fn window_background() -> Retained<Self> {
            color(0.925, 0.925, 0.925, 1.0)
        }

        #[unsafe(method(set))]
        fn set(&self) {
            let c = self.color();
            with_recorder(|r| {
                r.fill = c;
                r.stroke = c;
            });
        }

        #[unsafe(method(setFill))]
        fn set_fill(&self) {
            let c = self.color();
            with_recorder(|r| r.fill = c);
        }

        #[unsafe(method(setStroke))]
        fn set_stroke(&self) {
            let c = self.color();
            with_recorder(|r| r.stroke = c);
        }

        #[unsafe(method(redComponent))]
        fn red_component(&self) -> f64 {
            self.ivars().rgba[0]
        }

        #[unsafe(method(greenComponent))]
        fn green_component(&self) -> f64 {
            self.ivars().rgba[1]
        }

        #[unsafe(method(blueComponent))]
        fn blue_component(&self) -> f64 {
            self.ivars().rgba[2]
        }

        #[unsafe(method(alphaComponent))]
        fn alpha_component(&self) -> f64 {
            self.ivars().rgba[3]
        }
    }

    unsafe impl NSObjectProtocol for NSColorImpl {}
);

impl NSColorImpl {
    fn color(&self) -> Color {
        self.ivars().rgba.map(|v| v as f32)
    }
}

fn color(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColorImpl> {
    crate::load_shell::<objc2_app_kit::NSColor>();
    let this = NSColorImpl::alloc().set_ivars(ColorIvars { rgba: [r, g, b, a] });
    unsafe { msg_send![super(this), init] }
}

pub(crate) fn color_of(c: &NSColor) -> Color {
    // SAFETY: every NSColor is an instance of NSColorImpl.
    unsafe { &*(c as *const NSColor).cast::<NSColorImpl>() }.color()
}

// NSBezierPath

#[derive(Default)]
pub(crate) struct PathIvars {
    subpaths: RefCell<Vec<Vec<[f64; 2]>>>,
    line_width: Cell<f64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSBezierPath"]
    #[ivars = PathIvars]
    pub(crate) struct NSBezierPathImpl;

    impl NSBezierPathImpl {
        #[unsafe(method_id(bezierPath))]
        fn bezier_path() -> Retained<Self> {
            path()
        }

        #[unsafe(method_id(bezierPathWithRect:))]
        fn with_rect(r: NSRect) -> Retained<Self> {
            let p = path();
            p.ivars().subpaths.borrow_mut().push(rect_points(r));
            p
        }

        #[unsafe(method(fillRect:))]
        fn fill_rect(r: NSRect) {
            with_recorder(|rec| {
                let rect = rec.xf.rect(r).intersect(&rec.clip);
                if !rect.is_empty() {
                    rec.ops.push(Op::Fill { rect, color: rec.fill });
                }
            });
        }

        #[unsafe(method(moveToPoint:))]
        fn move_to(&self, p: NSPoint) {
            self.ivars().subpaths.borrow_mut().push(vec![[p.x, p.y]]);
        }

        #[unsafe(method(lineToPoint:))]
        fn line_to(&self, p: NSPoint) {
            let mut subpaths = self.ivars().subpaths.borrow_mut();
            match subpaths.last_mut() {
                Some(last) => last.push([p.x, p.y]),
                None => subpaths.push(vec![[p.x, p.y]]),
            }
        }

        #[unsafe(method(closePath))]
        fn close_path(&self) {}

        #[unsafe(method(appendBezierPathWithRect:))]
        fn append_rect(&self, r: NSRect) {
            self.ivars().subpaths.borrow_mut().push(rect_points(r));
        }

        #[unsafe(method(setLineWidth:))]
        fn set_line_width(&self, w: f64) {
            self.ivars().line_width.set(w);
        }

        #[unsafe(method(lineWidth))]
        fn line_width(&self) -> f64 {
            self.ivars().line_width.get()
        }

        #[unsafe(method(fill))]
        fn fill(&self) {
            let subpaths = self.ivars().subpaths.borrow();
            with_recorder(|rec| {
                for sub in subpaths.iter().filter(|s| s.len() >= 3) {
                    let points: Vec<[f32; 2]> = sub
                        .iter()
                        .map(|p| {
                            let (x, y) = rec.xf.point(p[0], p[1]);
                            [x as f32, y as f32]
                        })
                        .collect();
                    rec.ops.push(Op::Path { points, color: rec.fill, clip: rec.clip });
                }
            });
        }
    }

    unsafe impl NSObjectProtocol for NSBezierPathImpl {}
);

fn path() -> Retained<NSBezierPathImpl> {
    crate::load_shell::<objc2_app_kit::NSBezierPath>();
    let this = NSBezierPathImpl::alloc().set_ivars(PathIvars { line_width: Cell::new(1.0), ..Default::default() });
    unsafe { msg_send![super(this), init] }
}

fn rect_points(r: NSRect) -> Vec<[f64; 2]> {
    let (x0, y0) = (r.origin.x, r.origin.y);
    let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}
