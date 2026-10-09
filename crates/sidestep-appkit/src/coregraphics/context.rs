//! `CGContext`: an object holding a `context::ContextState`, the state
//! AppKit's drawing uses too, and the functions that draw with it.
//!
//! Each function reaches the state through the object it's given and a
//! `RefCell` (no message sends), and maps onto it one to one: CoreGraphics'
//! CTM is the state's (user space to layer points) followed by the
//! context's device transform (layer points to device pixels, origin at
//! the bottom left), which `CGContextGetCTM` reports; fills, strokes,
//! clips and images become the same ops `NSBezierPath` and `NSImage`
//! record. What CoreGraphics keeps beyond AppKit ([`CgState`]: line
//! settings, the global alpha, fill and stroke color spaces, text
//! settings) is part of the graphics state, saved and restored with it.
//!
//! The current path is in layer points, as CoreGraphics keeps it in device
//! space: each point is transformed by the CTM when it's added, so the CTM
//! changing afterwards doesn't move it; queries map it back to user space.
//! A path added whole to an empty one (`CGContextAddPath`, the usual way to
//! fill a `CGPath`) is kept shared, with the CTM it was added under
//! ([`CtxPath::Shared`]), so filling it draws the path's own tiny-skia path
//! and copies nothing, as `-[NSBezierPath fill]` does. Strokes are made in
//! user space at the time of stroking (the line width scales with the CTM),
//! and a line width of 0 draws nothing, as in CoreGraphics.
//!
//! [`with_state`] and [`drawing_into`] are how other parts of AppKit (text
//! layout fragments drawing `inContext:`) draw into a CGContext they're
//! handed.

use std::cell::RefCell;
use std::ffi::{c_char, c_int};
use std::ptr::NonNull;
use std::sync::Arc;

use kurbo::{Affine, BezPath, Point};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFDictionary, CFTypeID, CGAffineTransform, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBlendMode, CGColor, CGColorRenderingIntent, CGColorSpace, CGContext, CGFont, CGGlyph, CGGradient,
    CGGradientDrawingOptions, CGImage, CGInterpolationQuality, CGLineCap, CGLineJoin, CGPath, CGPathDrawingMode,
    CGPattern, CGShading, CGTextDrawingMode,
};
use objc2_foundation::NSString;

use super::color::{CGColorSpaceImpl, color_imp, space_imp};
use super::geometry::{NULL, kurbo_of, rect as cg_rect, transform_of};
use super::path::{CgShape, Shape, new_path, path_imp};
use crate::context::{ContextState, to_skia};
use crate::protocol::{Blend, Color, Draw, Op, Paint, Rect, ShadowSpec, StrokeSpec};

/// CoreGraphics' part of the graphics state.
#[derive(Clone)]
pub(crate) struct CgState {
    /// The global alpha every drawing is faded by.
    pub alpha: f32,
    pub line: Line,
    /// The spaces `CGContextSetFillColor` and `…SetStrokeColor` take
    /// components in (`None`: device RGB, the default).
    pub fill_space: Option<Retained<CGColorSpaceImpl>>,
    pub stroke_space: Option<Retained<CGColorSpaceImpl>>,
    pub text: Arc<TextState>,
}

thread_local! {
    /// The text settings every fresh graphics state starts with, shared
    /// (each view's `drawRect:` starts from a fresh state, and shouldn't
    /// allocate for it).
    static DEFAULT_TEXT: Arc<TextState> = Arc::default();
}

impl Default for CgState {
    fn default() -> Self {
        let text = DEFAULT_TEXT.try_with(Arc::clone).unwrap_or_default();
        CgState { alpha: 1.0, line: Line::default(), fill_space: None, stroke_space: None, text }
    }
}

impl std::fmt::Debug for CgState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CgState").field("alpha", &self.alpha).field("line", &self.line).finish_non_exhaustive()
    }
}

/// How CoreGraphics strokes.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub width: f64,
    pub cap: CGLineCap,
    pub join: CGLineJoin,
    pub miter: f64,
    /// The dash lengths and phase; `None` for solid lines.
    pub dash: Option<(Arc<[f64]>, f64)>,
    pub flatness: f64,
}

impl Default for Line {
    fn default() -> Self {
        Line { width: 1.0, cap: CGLineCap::Butt, join: CGLineJoin::Miter, miter: 10.0, dash: None, flatness: 0.6 }
    }
}

impl Line {
    /// The width strokes are drawn with: a negative one draws a line 1
    /// wide, as CoreGraphics draws it (measured on macOS).
    pub(crate) fn drawn_width(&self) -> f64 {
        if self.width < 0.0 { 1.0 } else { self.width }
    }

    pub(crate) fn spec(&self) -> StrokeSpec {
        StrokeSpec {
            width: self.drawn_width() as f32,
            cap: self.cap.0 as u8,
            join: self.join.0 as u8,
            miter: self.miter.max(1.0) as f32,
            dash: self.dash.as_ref().map(|(l, p)| (l.iter().map(|&v| v as f32).collect(), *p as f32)),
        }
    }
}

/// CoreGraphics' text settings, part of the graphics state (the text
/// matrix isn't: it's the context state's). Text is drawn through
/// CoreText's glyph drawing (`coretext::draw`).
#[derive(Clone)]
pub(crate) struct TextState {
    pub font: Option<Retained<super::font::CGFontImpl>>,
    pub size: f64,
    pub spacing: f64,
    pub mode: CGTextDrawingMode,
    /// `CGContextSetShouldSmoothFonts`, `…SubpixelPositionFonts` and
    /// `…SubpixelQuantizeFonts`.
    pub smoothing: [bool; 3],
}

impl Default for TextState {
    fn default() -> Self {
        TextState { font: None, size: 12.0, spacing: 0.0, mode: CGTextDrawingMode::Fill, smoothing: [true; 3] }
    }
}

/// The current path, in layer points.
#[derive(Clone, Debug, Default)]
pub(crate) enum CtxPath {
    #[default]
    Empty,
    /// A path added whole to an empty one: its shape (shared with it) and
    /// the transform to layer points it was added under.
    Shared(Arc<Shape>, Affine),
    /// Elements built here.
    Built(Shape),
}

impl CtxPath {
    /// The elements, to build on.
    fn built(&mut self) -> &mut Shape {
        match self {
            CtxPath::Built(s) => s,
            _ => {
                let shape = match std::mem::take(self) {
                    CtxPath::Shared(s, xf) => s.transformed(xf),
                    _ => Shape::default(),
                };
                *self = CtxPath::Built(shape);
                match self {
                    CtxPath::Built(s) => s,
                    _ => unreachable!(),
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            CtxPath::Empty => true,
            CtxPath::Shared(s, _) => s.is_empty(),
            CtxPath::Built(s) => s.is_empty(),
        }
    }

    /// The path in layer points.
    fn layer(&self) -> Option<Shape> {
        match self {
            CtxPath::Empty => None,
            CtxPath::Shared(s, xf) => Some(s.transformed(*xf)),
            CtxPath::Built(s) => Some(s.clone()),
        }
    }

    /// The path under `ctm` (layer points back to user space), if the CTM
    /// can be undone.
    fn user(&self, ctm: Affine) -> Option<Shape> {
        match self {
            CtxPath::Empty => None,
            CtxPath::Shared(s, xf) if *xf == ctm => Some((**s).clone()),
            _ => {
                if ctm.determinant() == 0.0 || !ctm.determinant().is_finite() {
                    return None;
                }
                let layer = self.layer()?;
                Some(layer.transformed(ctm.inverse()))
            }
        }
    }
}

pub(crate) struct CgContextIvars {
    pub(crate) state: RefCell<ContextState>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a context is used
    // by one thread at a time, as CoreGraphics' are.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGContext"]
    #[ivars = CgContextIvars]
    pub(crate) struct CGContextImpl;

    impl CGContextImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let kind = match self.ivars().state.try_borrow().map(|s| matches!(s.target, crate::context::Target::Record)) {
                Ok(true) => "(kCGContextTypeWindow)",
                _ => "(kCGContextTypeBitmap)",
            };
            super::description("CGContext", self, kind)
        }
    }

    unsafe impl NSObjectProtocol for CGContextImpl {}
);

impl Drop for CGContextImpl {
    fn drop(&mut self) {
        let Ok(mut st) = self.ivars().state.try_borrow_mut() else { return };
        while !st.layers.is_empty() {
            end_layer(&mut st);
        }
        st.flush();
    }
}

/// A new context drawing with `state`.
pub(crate) fn new_context(state: ContextState) -> Retained<CGContextImpl> {
    let this = CGContextImpl::alloc().set_ivars(CgContextIvars { state: RefCell::new(state) });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

pub(crate) fn imp(c: &CGContext) -> &CGContextImpl {
    // SAFETY: every CGContext is a CGContextImpl.
    unsafe { &*(c as *const CGContext).cast::<CGContextImpl>() }
}

pub(crate) fn as_cg(c: &CGContextImpl) -> &CGContext {
    // SAFETY: CGContextImpl is what CGContext names.
    unsafe { &*(c as *const CGContextImpl).cast::<CGContext>() }
}

/// Run `f` on the graphics state `cg` draws with: what `drawRect:`'s
/// drawing uses when `cg` is the current context's, or a bitmap context's
/// own. `None` if the state is in use further up the stack. This is the
/// door for AppKit code handed a CGContext to draw into (text layout
/// fragments' `drawAtPoint:inContext:`): ops pushed on the state draw
/// there, in its CTM and clip.
pub(crate) fn with_state<R>(cg: &CGContext, f: impl FnOnce(&mut ContextState) -> R) -> Option<R> {
    let mut st = imp(cg).ivars().state.try_borrow_mut().ok()?;
    Some(f(&mut st))
}

/// Run `f` with `cg` the current graphics context (an `NSGraphicsContext`
/// wrapping it), so that AppKit's drawing (strings, images, paths) goes
/// into it; the context that was current before is current again after.
// For text layout fragments drawing `inContext:` (TextKit 2).
pub(crate) fn drawing_into<R>(cg: &CGContext, f: impl FnOnce() -> R) -> R {
    /// Puts the previous context back, however `f` ends.
    struct Current;
    impl Drop for Current {
        fn drop(&mut self) {
            crate::context::end_current();
        }
    }
    let ns = crate::context::wrap(imp(cg).retain(), None);
    crate::context::begin_current(ns);
    let _current = Current;
    f()
}

fn with<R>(c: Option<&CGContext>, f: impl FnOnce(&mut ContextState) -> R) -> Option<R> {
    with_state(c?, f)
}

fn pt(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

fn cg_point(p: Point) -> CGPoint {
    CGPoint { x: p.x, y: p.y }
}

fn blend_of(mode: CGBlendMode) -> Blend {
    match mode.0 {
        1 => Blend::Multiply,
        2 => Blend::Screen,
        3 => Blend::Overlay,
        4 => Blend::Darken,
        5 => Blend::Lighten,
        6 => Blend::ColorDodge,
        7 => Blend::ColorBurn,
        8 => Blend::SoftLight,
        9 => Blend::HardLight,
        10 => Blend::Difference,
        11 => Blend::Exclusion,
        12 => Blend::Hue,
        13 => Blend::Saturation,
        14 => Blend::Color,
        15 => Blend::Luminosity,
        16 => Blend::Clear,
        17 => Blend::Copy,
        18 => Blend::SourceIn,
        19 => Blend::SourceOut,
        20 => Blend::SourceAtop,
        21 => Blend::DestinationOver,
        22 => Blend::DestinationIn,
        23 => Blend::DestinationOut,
        24 => Blend::DestinationAtop,
        25 => Blend::Xor,
        26 => Blend::PlusDarker,
        27 => Blend::PlusLighter,
        _ => Blend::SourceOver,
    }
}

impl ContextState {
    /// CoreGraphics' CTM: user space to device space.
    pub(crate) fn cg_ctm(&self) -> Affine {
        self.device * self.gs.ctm
    }

    /// User space to device pixels: for a view drawing in a window, the
    /// pixels of its layer, as macOS gives them (the CTM scaled, turned
    /// over for a flipped view); elsewhere the context's pixels from their
    /// top left.
    pub(crate) fn user_to_device(&self) -> Affine {
        match self.view_device {
            Some(flipped) => {
                let s = self.scale;
                Affine::new([s, 0.0, 0.0, if flipped { -s } else { s }, 0.0, 0.0]) * self.cg_ctm()
            }
            None => Affine::scale(self.scale) * self.gs.ctm,
        }
    }

    /// Take the current path, leaving none.
    fn take_path(&mut self) -> CtxPath {
        std::mem::take(&mut self.path)
    }

    /// Fill `path` (as the context holds paths) with the fill color.
    fn fill_ctx_path(&mut self, path: &CtxPath, even_odd: bool) {
        let (shape, xf) = match path {
            CtxPath::Empty => return,
            CtxPath::Shared(s, xf) => (s.drawn(), to_skia(*xf)),
            CtxPath::Built(s) => (s.drawn(), tiny_skia::Transform::identity()),
        };
        let Some(path) = shape else { return };
        let mut draw = self.gs.draw();
        draw.xf = xf;
        self.push(Op::FillPath { path, even_odd, paint: Paint::Solid(self.gs.fill), draw });
    }

    /// Stroke `path` with the line settings and stroke color, in the
    /// current user space.
    fn stroke_ctx_path(&mut self, path: &CtxPath) {
        let line = &self.gs.cg.line;
        if line.drawn_width() == 0.0 || !line.width.is_finite() {
            return;
        }
        // A path added whole under the CTM in force is the path's own, and
        // so is its tiny-skia path.
        let skia = match path {
            CtxPath::Shared(s, xf) if *xf == self.gs.ctm => s.drawn(),
            _ => path.user(self.gs.ctm).and_then(|user| user.drawn()),
        };
        let Some(skia) = skia else { return };
        let stroke = Arc::new(line.spec());
        self.push(Op::StrokePath { path: skia, stroke, paint: Paint::Solid(self.gs.stroke), draw: self.gs.draw() });
    }

    /// What AppKit's drawing of a path does with CoreGraphics' current
    /// path: it adds its path to it and draws both, so what the program
    /// left there is filled (with the path's rule) and taken, as in AppKit
    /// (measured on macOS).
    pub(crate) fn fill_leftover(&mut self, even_odd: bool) {
        let path = self.take_path();
        self.fill_ctx_path(&path, even_odd);
    }

    /// The same for strokes: what's left is stroked with the path's
    /// stroke.
    pub(crate) fn stroke_leftover(&mut self, stroke: &Arc<StrokeSpec>) {
        let path = self.take_path();
        let Some(skia) = path.user(self.gs.ctm).and_then(|user| user.drawn()) else { return };
        let op = Op::StrokePath {
            path: skia,
            stroke: stroke.clone(),
            paint: Paint::Solid(self.gs.stroke),
            draw: self.gs.draw(),
        };
        self.push(op);
    }

    /// Fill a shape in user space.
    fn fill_user(&mut self, shape: &Shape, even_odd: bool) {
        let Some(path) = shape.drawn() else { return };
        self.push(Op::FillPath { path, even_odd, paint: Paint::Solid(self.gs.fill), draw: self.gs.draw() });
    }

    /// Stroke a shape in user space.
    fn stroke_user(&mut self, shape: &Shape) {
        let line = &self.gs.cg.line;
        if line.drawn_width() == 0.0 || !line.width.is_finite() {
            return;
        }
        let Some(path) = shape.drawn() else { return };
        let stroke = Arc::new(line.spec());
        self.push(Op::StrokePath { path, stroke, paint: Paint::Solid(self.gs.stroke), draw: self.gs.draw() });
    }

    /// The clip's bounds in user space; `None` if they're nowhere (clips
    /// that don't meet) or the CTM can't be undone. A clip of no area
    /// somewhere has bounds of no area there, as in CoreGraphics.
    pub(crate) fn clip_in_user_space(&self) -> Option<kurbo::Rect> {
        let c = self.gs.clip;
        if c.x1 < c.x0 || c.y1 < c.y0 || self.gs.ctm.determinant().abs() < 1e-12 {
            return None;
        }
        let inv = self.gs.ctm.inverse();
        // Clips of views start out unbounded (a window's recording).
        let clamp = |v: f32| f64::from(v.clamp(-1e7, 1e7));
        let r = kurbo::Rect::new(clamp(c.x0), clamp(c.y0), clamp(c.x1), clamp(c.y1));
        use kurbo::Shape as _;
        Some((inv * r.to_path(0.1)).bounding_box())
    }
}

/// The components the `set…Color` functions take into a color: in `space`
/// (device RGB for none), straight sRGB RGBA.
fn components_color(space: Option<&CGColorSpaceImpl>, comps: &[CGFloat]) -> Color {
    match space {
        Some(s) => s.rgba(comps),
        None => {
            let at = |i: usize| comps.get(i).copied().unwrap_or(if i == 3 { 1.0 } else { 0.0 });
            [at(0), at(1), at(2), at(3)].map(|v| v.clamp(0.0, 1.0) as f32)
        }
    }
}

/// A space's default color: black, opaque; nothing for a pattern space
/// (patterns aren't drawn).
fn default_color(space: &CGColorSpaceImpl) -> Color {
    if space.info().model == super::color::Model::Pattern {
        return [0.0; 4];
    }
    let n = space.info().components;
    let mut comps = vec![0.0; n + 1];
    if space.info().model == super::color::Model::Cmyk {
        comps[3] = 1.0;
    }
    comps[n] = 1.0;
    components_color(Some(space), &comps)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_CONTEXT
}

// The graphics state.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSaveGState(c: Option<&CGContext>) {
    with(c, ContextState::save);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextRestoreGState(c: Option<&CGContext>) {
    with(c, ContextState::restore);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextScaleCTM(c: Option<&CGContext>, sx: CGFloat, sy: CGFloat) {
    with(c, |st| st.set_ctm(st.gs.ctm * Affine::scale_non_uniform(sx, sy)));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextTranslateCTM(c: Option<&CGContext>, tx: CGFloat, ty: CGFloat) {
    with(c, |st| st.set_ctm(st.gs.ctm * Affine::translate((tx, ty))));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextRotateCTM(c: Option<&CGContext>, angle: CGFloat) {
    with(c, |st| st.set_ctm(st.gs.ctm * Affine::rotate(angle)));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextConcatCTM(c: Option<&CGContext>, transform: CGAffineTransform) {
    with(c, |st| st.set_ctm(st.gs.ctm * kurbo_of(transform)));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetCTM(c: Option<&CGContext>) -> CGAffineTransform {
    with(c, |st| transform_of(st.cg_ctm())).unwrap_or(super::geometry::IDENTITY)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetLineWidth(c: Option<&CGContext>, width: CGFloat) {
    with(c, |st| st.gs.cg.line.width = width);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetLineCap(c: Option<&CGContext>, cap: CGLineCap) {
    with(c, |st| st.gs.cg.line.cap = cap);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetLineJoin(c: Option<&CGContext>, join: CGLineJoin) {
    with(c, |st| st.gs.cg.line.join = join);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetMiterLimit(c: Option<&CGContext>, limit: CGFloat) {
    with(c, |st| st.gs.cg.line.miter = limit);
}

/// # Safety
///
/// `lengths` holds `count` lengths (or is null).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextSetLineDash(
    c: Option<&CGContext>,
    phase: CGFloat,
    lengths: *const CGFloat,
    count: usize,
) {
    // SAFETY: as the caller promises.
    let lengths = unsafe { super::slice(lengths, count) };
    // No lengths, or none positive, is a solid line.
    let dash = (!lengths.is_empty() && lengths.iter().all(|l| *l >= 0.0) && lengths.iter().sum::<f64>() > 0.0)
        .then(|| (Arc::from(lengths), phase));
    with(c, |st| st.gs.cg.line.dash = dash);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetFlatness(c: Option<&CGContext>, flatness: CGFloat) {
    with(c, |st| st.gs.cg.line.flatness = flatness);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetAlpha(c: Option<&CGContext>, alpha: CGFloat) {
    with(c, |st| st.gs.cg.alpha = alpha.clamp(0.0, 1.0) as f32);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetBlendMode(c: Option<&CGContext>, mode: CGBlendMode) {
    with(c, |st| st.gs.blend = blend_of(mode));
}

// Building the current path.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextBeginPath(c: Option<&CGContext>) {
    with(c, |st| st.path = CtxPath::Empty);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextMoveToPoint(c: Option<&CGContext>, x: CGFloat, y: CGFloat) {
    with(c, |st| {
        let p = st.gs.ctm * pt(x, y);
        st.path.built().move_to(p);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddLineToPoint(c: Option<&CGContext>, x: CGFloat, y: CGFloat) {
    with(c, |st| {
        let p = st.gs.ctm * pt(x, y);
        st.path.built().line_to(p);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddCurveToPoint(
    c: Option<&CGContext>,
    cp1x: CGFloat,
    cp1y: CGFloat,
    cp2x: CGFloat,
    cp2y: CGFloat,
    x: CGFloat,
    y: CGFloat,
) {
    with(c, |st| {
        let m = st.gs.ctm;
        st.path.built().curve_to(m * pt(cp1x, cp1y), m * pt(cp2x, cp2y), m * pt(x, y));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddQuadCurveToPoint(
    c: Option<&CGContext>,
    cpx: CGFloat,
    cpy: CGFloat,
    x: CGFloat,
    y: CGFloat,
) {
    with(c, |st| {
        let m = st.gs.ctm;
        st.path.built().quad_to(m * pt(cpx, cpy), m * pt(x, y));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextClosePath(c: Option<&CGContext>) {
    with(c, |st| st.path.built().close());
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        let m = st.gs.ctm;
        st.path.built().add_cg_rect(rect, Some(m));
    });
}

/// # Safety
///
/// `rects` holds `count` rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextAddRects(c: Option<&CGContext>, rects: *const CGRect, count: usize) {
    // SAFETY: as the caller promises.
    let rects = unsafe { super::slice(rects, count) };
    with(c, |st| {
        let m = st.gs.ctm;
        let shape = st.path.built();
        rects.iter().for_each(|r| shape.add_cg_rect(*r, Some(m)));
    });
}

/// # Safety
///
/// `points` holds `count` points.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextAddLines(c: Option<&CGContext>, points: *const CGPoint, count: usize) {
    // SAFETY: as the caller promises.
    let points: Vec<Point> = unsafe { super::slice(points, count) }.iter().map(|p| pt(p.x, p.y)).collect();
    with(c, |st| {
        let m = st.gs.ctm;
        st.path.built().add_lines(&points, Some(m));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddEllipseInRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        let m = st.gs.ctm;
        st.path.built().add_cg_ellipse(rect, Some(m));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddArc(
    c: Option<&CGContext>,
    x: CGFloat,
    y: CGFloat,
    radius: CGFloat,
    start_angle: CGFloat,
    end_angle: CGFloat,
    clockwise: c_int,
) {
    with(c, |st| {
        let m = st.gs.ctm;
        st.path.built().add_arc(pt(x, y), radius, start_angle, end_angle, clockwise != 0, Some(m));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddArcToPoint(
    c: Option<&CGContext>,
    x1: CGFloat,
    y1: CGFloat,
    x2: CGFloat,
    y2: CGFloat,
    radius: CGFloat,
) {
    with(c, |st| {
        let m = st.gs.ctm;
        st.path.built().add_arc_to(pt(x1, y1), pt(x2, y2), radius, Some(m));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextAddPath(c: Option<&CGContext>, path: Option<&CGPath>) {
    let Some(path) = path else { return };
    let shape = path_imp(path).snapshot();
    with(c, |st| {
        if st.path.is_empty() {
            st.path = CtxPath::Shared(shape, st.gs.ctm);
        } else {
            let m = st.gs.ctm;
            st.path.built().add_shape(&shape, Some(m));
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextReplacePathWithStrokedPath(c: Option<&CGContext>) {
    with(c, |st| {
        let line = st.gs.cg.line.clone();
        let Some(user) = st.path.user(st.gs.ctm) else { return };
        let width = line.drawn_width();
        let mut outline = user.stroked(width, line.cap, line.join, line.miter);
        if let Some((lengths, phase)) = &line.dash {
            let dashed = super::path::dash(&user.path, *phase, lengths);
            outline = Shape::from_path(dashed).stroked(width, line.cap, line.join, line.miter);
        }
        st.path = CtxPath::Built(Shape::from_path(outline).transformed(st.gs.ctm));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextIsPathEmpty(c: Option<&CGContext>) -> bool {
    with(c, |st| st.path.is_empty()).unwrap_or(true)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetPathCurrentPoint(c: Option<&CGContext>) -> CGPoint {
    with(c, |st| {
        let current = match &st.path {
            CtxPath::Empty => return CGPoint::ZERO,
            CtxPath::Shared(s, xf) => *xf * s.current(),
            CtxPath::Built(s) => s.current(),
        };
        let m = st.gs.ctm;
        if m.determinant() == 0.0 { CGPoint::ZERO } else { cg_point(m.inverse() * current) }
    })
    .unwrap_or(CGPoint::ZERO)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetPathBoundingBox(c: Option<&CGContext>) -> CGRect {
    with(c, |st| super::path::rect_of(st.path.user(st.gs.ctm).and_then(|s| s.tight_bounds()))).unwrap_or(NULL)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextCopyPath(c: Option<&CGContext>) -> Option<NonNull<CGPath>> {
    let shape = with(c, |st| if st.path.is_empty() { None } else { st.path.user(st.gs.ctm) })??;
    Some(super::owned(new_path(Arc::new(shape), false)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextPathContainsPoint(
    c: Option<&CGContext>,
    point: CGPoint,
    mode: CGPathDrawingMode,
) -> bool {
    with(c, |st| {
        let Some(user) = st.path.user(st.gs.ctm) else { return false };
        let p = pt(point.x, point.y);
        let fill = |even_odd| user.contains(p, even_odd);
        let stroke = || {
            let line = &st.gs.cg.line;
            Shape::from_path(user.stroked(line.drawn_width(), line.cap, line.join, line.miter)).contains(p, false)
        };
        match mode {
            CGPathDrawingMode::Fill => fill(false),
            CGPathDrawingMode::EOFill => fill(true),
            CGPathDrawingMode::Stroke => stroke(),
            CGPathDrawingMode::FillStroke => fill(false) || stroke(),
            _ => fill(true) || stroke(),
        }
    })
    .unwrap_or(false)
}

// Drawing paths.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextDrawPath(c: Option<&CGContext>, mode: CGPathDrawingMode) {
    with(c, |st| {
        let path = st.take_path();
        match mode {
            CGPathDrawingMode::Fill => st.fill_ctx_path(&path, false),
            CGPathDrawingMode::EOFill => st.fill_ctx_path(&path, true),
            CGPathDrawingMode::Stroke => st.stroke_ctx_path(&path),
            CGPathDrawingMode::FillStroke => {
                st.fill_ctx_path(&path, false);
                st.stroke_ctx_path(&path);
            }
            CGPathDrawingMode::EOFillStroke => {
                st.fill_ctx_path(&path, true);
                st.stroke_ctx_path(&path);
            }
            _ => {}
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextFillPath(c: Option<&CGContext>) {
    CGContextDrawPath(c, CGPathDrawingMode::Fill);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextEOFillPath(c: Option<&CGContext>) {
    CGContextDrawPath(c, CGPathDrawingMode::EOFill);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextStrokePath(c: Option<&CGContext>) {
    CGContextDrawPath(c, CGPathDrawingMode::Stroke);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextFillRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        st.path = CtxPath::Empty;
        st.fill_rect(super::geometry::standardize(rect), st.gs.fill, st.gs.blend);
    });
}

/// # Safety
///
/// `rects` holds `count` rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextFillRects(c: Option<&CGContext>, rects: *const CGRect, count: usize) {
    // SAFETY: as the caller promises.
    let rects = unsafe { super::slice(rects, count) };
    with(c, |st| {
        st.path = CtxPath::Empty;
        for r in rects {
            st.fill_rect(super::geometry::standardize(*r), st.gs.fill, st.gs.blend);
        }
    });
}

fn rect_shape(rect: CGRect) -> Shape {
    let mut s = Shape::default();
    s.add_cg_rect(rect, None);
    s
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextStrokeRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        st.path = CtxPath::Empty;
        st.stroke_user(&rect_shape(rect));
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextStrokeRectWithWidth(c: Option<&CGContext>, rect: CGRect, width: CGFloat) {
    with(c, |st| {
        st.path = CtxPath::Empty;
        let old = std::mem::replace(&mut st.gs.cg.line.width, width);
        st.stroke_user(&rect_shape(rect));
        st.gs.cg.line.width = old;
    });
}

/// Clear to transparent, whatever the alpha, blend mode and shadow.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextClearRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        st.path = CtxPath::Empty;
        let shadow = st.gs.shadow.take();
        st.fill_rect(super::geometry::standardize(rect), [0.0; 4], Blend::Clear);
        st.gs.shadow = shadow;
    });
}

fn ellipse_shape(rect: CGRect) -> Shape {
    let mut s = Shape::default();
    s.add_cg_ellipse(rect, None);
    s
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextFillEllipseInRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        st.path = CtxPath::Empty;
        st.fill_user(&ellipse_shape(rect), false);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextStrokeEllipseInRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        st.path = CtxPath::Empty;
        st.stroke_user(&ellipse_shape(rect));
    });
}

/// # Safety
///
/// `points` holds `count` points: pairs, each a segment.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextStrokeLineSegments(
    c: Option<&CGContext>,
    points: *const CGPoint,
    count: usize,
) {
    // SAFETY: as the caller promises.
    let points = unsafe { super::slice(points, count) };
    let mut path = BezPath::new();
    for [a, b] in points.as_chunks::<2>().0 {
        path.move_to(pt(a.x, a.y));
        path.line_to(pt(b.x, b.y));
    }
    with(c, |st| {
        st.path = CtxPath::Empty;
        st.stroke_user(&Shape::from_path(path));
    });
}

// Clipping.

/// Clip to the current path, consuming it. A path with no elements
/// leaves the clip as it was; one with elements but no area leaves nothing
/// to draw in (at its bounds), as in CoreGraphics.
fn clip_to_path(st: &mut ContextState, even_odd: bool) {
    let path = st.take_path();
    if path.is_empty() {
        return;
    }
    let (skia, xf) = match &path {
        CtxPath::Empty => return,
        CtxPath::Shared(s, xf) => (s.drawn(), to_skia(*xf)),
        CtxPath::Built(s) => (s.drawn(), tiny_skia::Transform::identity()),
    };
    match skia {
        Some(p) => st.clip_path_with(p, even_odd, false, xf),
        None => {
            // Nothing left, at the path's bounds (layer points).
            let at = path.layer().and_then(|s| s.control_bounds());
            st.gs.clip = match at {
                Some(b) => st.gs.clip.intersect(&Rect::new(b.x0 as f32, b.y0 as f32, b.x1 as f32, b.y1 as f32)),
                None => Rect::NOWHERE,
            };
            st.sync();
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextClip(c: Option<&CGContext>) {
    with(c, |st| clip_to_path(st, false));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextEOClip(c: Option<&CGContext>) {
    with(c, |st| clip_to_path(st, true));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextResetClip(c: &CGContext) {
    with_state(c, |st| {
        st.gs.clip = st.view_clip;
        st.gs.mask = None;
        st.sync();
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextClipToMask(c: Option<&CGContext>, rect: CGRect, mask: Option<&CGImage>) {
    let Some(mask) = mask.map(super::image::image_imp) else { return };
    // Worked out before the state is taken (a data provider's callbacks
    // may run).
    let data = mask.clip_pixels();
    with(c, |st| {
        st.path = CtxPath::Empty;
        if let Some(data) = data {
            super::image::clip_to_mask(st, rect, data);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetClipBoundingBox(c: Option<&CGContext>) -> CGRect {
    with(c, |st| match st.clip_in_user_space() {
        Some(r) => cg_rect(r.x0, r.y0, r.width(), r.height()),
        None => NULL,
    })
    .unwrap_or(NULL)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextClipToRect(c: Option<&CGContext>, rect: CGRect) {
    with(c, |st| {
        st.path = CtxPath::Empty;
        st.clip_rect(super::geometry::standardize(rect));
    });
}

/// # Safety
///
/// `rects` holds `count` rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextClipToRects(c: Option<&CGContext>, rects: NonNull<CGRect>, count: usize) {
    // SAFETY: as the caller promises.
    let rects = unsafe { super::slice(rects.as_ptr(), count) };
    with(c, |st| match rects {
        [] => {}
        [one] => st.clip_rect(super::geometry::standardize(*one)),
        _ => {
            // Their union, as one path.
            let mut shape = Shape::default();
            rects.iter().for_each(|r| shape.add_cg_rect(*r, None));
            match shape.drawn() {
                Some(p) => st.clip_path(p, false, false),
                None => st.clip_rect(NULL),
            }
        }
    });
}

// Colors.

#[unsafe(no_mangle)]
/// A color and its space as the `set…ColorWithColor` functions take them:
/// no color is opaque black, as in CoreGraphics.
fn color_and_space(color: Option<&CGColor>) -> (Color, Option<Retained<CGColorSpaceImpl>>) {
    match color.map(color_imp) {
        Some(color) => (color.resolve(), Some(color.space().retain())),
        None => ([0.0, 0.0, 0.0, 1.0], None),
    }
}

/// A space's default color and the space, as the `set…ColorSpace`
/// functions take them: no space is device RGB's black.
fn space_default(space: Option<&CGColorSpace>) -> (Color, Option<Retained<CGColorSpaceImpl>>) {
    match space.map(space_imp) {
        Some(space) => (default_color(space), Some(space.retain())),
        None => ([0.0, 0.0, 0.0, 1.0], None),
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetFillColorWithColor(c: Option<&CGContext>, color: Option<&CGColor>) {
    let (value, space) = color_and_space(color);
    with(c, |st| {
        st.gs.fill = value;
        st.gs.cg.fill_space = space;
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetStrokeColorWithColor(c: Option<&CGContext>, color: Option<&CGColor>) {
    let (value, space) = color_and_space(color);
    with(c, |st| {
        st.gs.stroke = value;
        st.gs.cg.stroke_space = space;
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetFillColorSpace(c: Option<&CGContext>, space: Option<&CGColorSpace>) {
    let (value, space) = space_default(space);
    with(c, |st| {
        st.gs.fill = value;
        st.gs.cg.fill_space = space;
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetStrokeColorSpace(c: Option<&CGContext>, space: Option<&CGColorSpace>) {
    let (value, space) = space_default(space);
    with(c, |st| {
        st.gs.stroke = value;
        st.gs.cg.stroke_space = space;
    });
}

fn space_components(space: Option<&CGColorSpaceImpl>) -> usize {
    space.map_or(3, |s| s.info().components) + 1
}

/// Whether `space` is a pattern space, whose colors (patterns) aren't
/// drawn.
fn is_pattern(space: Option<&CGColorSpaceImpl>) -> bool {
    space.is_some_and(|s| s.info().model == super::color::Model::Pattern)
}

/// # Safety
///
/// `components` holds the fill color space's components and alpha.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextSetFillColor(c: Option<&CGContext>, components: *const CGFloat) {
    with(c, |st| {
        let space = st.gs.cg.fill_space.clone();
        if is_pattern(space.as_deref()) {
            return;
        }
        // SAFETY: as the caller promises.
        let comps = unsafe { super::slice(components, space_components(space.as_deref())) };
        if !comps.is_empty() {
            st.gs.fill = components_color(space.as_deref(), comps);
        }
    });
}

/// # Safety
///
/// `components` holds the stroke color space's components and alpha.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextSetStrokeColor(c: Option<&CGContext>, components: *const CGFloat) {
    with(c, |st| {
        let space = st.gs.cg.stroke_space.clone();
        if is_pattern(space.as_deref()) {
            return;
        }
        // SAFETY: as the caller promises.
        let comps = unsafe { super::slice(components, space_components(space.as_deref())) };
        if !comps.is_empty() {
            st.gs.stroke = components_color(space.as_deref(), comps);
        }
    });
}

/// Patterns aren't drawn: setting one leaves the color as it was.
///
/// # Safety
///
/// Nothing is read.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextSetFillPattern(
    _c: Option<&CGContext>,
    _pattern: Option<&CGPattern>,
    _components: *const CGFloat,
) {
}

/// # Safety
///
/// Nothing is read.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextSetStrokePattern(
    _c: Option<&CGContext>,
    _pattern: Option<&CGPattern>,
    _components: *const CGFloat,
) {
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetPatternPhase(c: Option<&CGContext>, phase: CGSize) {
    with(c, |st| st.gs.phase = objc2_foundation::NSPoint::new(phase.width, phase.height));
}

fn gray_space() -> Retained<CGColorSpaceImpl> {
    super::color::shared(super::color::DEVICE_GRAY)
}

fn rgb_space() -> Retained<CGColorSpaceImpl> {
    super::color::shared(super::color::DEVICE_RGB)
}

fn cmyk_space() -> Retained<CGColorSpaceImpl> {
    super::color::shared(super::color::DEVICE_CMYK)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetGrayFillColor(c: Option<&CGContext>, gray: CGFloat, alpha: CGFloat) {
    let space = gray_space();
    let value = components_color(Some(&space), &[gray, alpha]);
    with(c, |st| {
        st.gs.fill = value;
        st.gs.cg.fill_space = Some(space);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetGrayStrokeColor(c: Option<&CGContext>, gray: CGFloat, alpha: CGFloat) {
    let space = gray_space();
    let value = components_color(Some(&space), &[gray, alpha]);
    with(c, |st| {
        st.gs.stroke = value;
        st.gs.cg.stroke_space = Some(space);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetRGBFillColor(
    c: Option<&CGContext>,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) {
    let value = components_color(None, &[red, green, blue, alpha]);
    with(c, |st| {
        st.gs.fill = value;
        st.gs.cg.fill_space = Some(rgb_space());
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetRGBStrokeColor(
    c: Option<&CGContext>,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) {
    let value = components_color(None, &[red, green, blue, alpha]);
    with(c, |st| {
        st.gs.stroke = value;
        st.gs.cg.stroke_space = Some(rgb_space());
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetCMYKFillColor(
    c: Option<&CGContext>,
    cyan: CGFloat,
    magenta: CGFloat,
    yellow: CGFloat,
    black: CGFloat,
    alpha: CGFloat,
) {
    let space = cmyk_space();
    let value = components_color(Some(&space), &[cyan, magenta, yellow, black, alpha]);
    with(c, |st| {
        st.gs.fill = value;
        st.gs.cg.fill_space = Some(space);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetCMYKStrokeColor(
    c: Option<&CGContext>,
    cyan: CGFloat,
    magenta: CGFloat,
    yellow: CGFloat,
    black: CGFloat,
    alpha: CGFloat,
) {
    let space = cmyk_space();
    let value = components_color(Some(&space), &[cyan, magenta, yellow, black, alpha]);
    with(c, |st| {
        st.gs.stroke = value;
        st.gs.cg.stroke_space = Some(space);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetRenderingIntent(c: Option<&CGContext>, intent: CGColorRenderingIntent) {
    with(c, |st| st.gs.intent = objc2_app_kit::NSColorRenderingIntent(intent.0 as isize));
}

// Images and shading.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextDrawImage(c: Option<&CGContext>, rect: CGRect, image: Option<&CGImage>) {
    let Some(image) = image.map(super::image::image_imp) else { return };
    // The pixels are worked out (a data provider's callbacks run) before
    // the state is taken, so the callbacks may use the context.
    let Some(data) = image.pixels() else { return };
    with(c, |st| super::image::draw(st, rect, image, data, false));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextDrawTiledImage(c: Option<&CGContext>, rect: CGRect, image: Option<&CGImage>) {
    let Some(image) = image.map(super::image::image_imp) else { return };
    let Some(data) = image.pixels() else { return };
    with(c, |st| super::image::draw(st, rect, image, data, true));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetInterpolationQuality(c: Option<&CGContext>) -> CGInterpolationQuality {
    with(c, |st| CGInterpolationQuality(st.gs.interpolation.0 as i32)).unwrap_or(CGInterpolationQuality::Default)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetInterpolationQuality(c: Option<&CGContext>, quality: CGInterpolationQuality) {
    with(c, |st| st.gs.interpolation = objc2_app_kit::NSImageInterpolation(quality.0 as usize));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetShadowWithColor(
    c: Option<&CGContext>,
    offset: CGSize,
    blur: CGFloat,
    color: Option<&CGColor>,
) {
    let color = color.map(|c| color_imp(c).resolve()).filter(|c| c[3] > 0.0);
    // Layer points run down; the offset's height runs up.
    let spec = color.map(|color| {
        Arc::new(ShadowSpec {
            dx: offset.width as f32,
            dy: -offset.height as f32,
            blur: blur.max(0.0) as f32,
            color,
            only: false,
        })
    });
    with(c, |st| st.gs.shadow = spec);
}

/// A shadow in black at a third of full opacity, as CoreGraphics' default.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetShadow(c: Option<&CGContext>, offset: CGSize, blur: CGFloat) {
    let spec = Arc::new(ShadowSpec {
        dx: offset.width as f32,
        dy: -offset.height as f32,
        blur: blur.max(0.0) as f32,
        color: [0.0, 0.0, 0.0, 1.0 / 3.0],
        only: false,
    });
    with(c, |st| st.gs.shadow = Some(spec));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextDrawLinearGradient(
    c: Option<&CGContext>,
    gradient: Option<&CGGradient>,
    start_point: CGPoint,
    end_point: CGPoint,
    options: CGGradientDrawingOptions,
) {
    let Some(gradient) = gradient.map(super::gradient::gradient_imp) else { return };
    let stops = gradient.stops();
    let extend = extend_of(options);
    with(c, |st| {
        crate::gradient::linear_band(st, stops, pt(start_point.x, start_point.y), pt(end_point.x, end_point.y), extend)
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextDrawRadialGradient(
    c: Option<&CGContext>,
    gradient: Option<&CGGradient>,
    start_center: CGPoint,
    start_radius: CGFloat,
    end_center: CGPoint,
    end_radius: CGFloat,
    options: CGGradientDrawingOptions,
) {
    let Some(gradient) = gradient.map(super::gradient::gradient_imp) else { return };
    let stops = gradient.stops();
    let extend = extend_of(options);
    with(c, |st| {
        crate::gradient::radial_band(
            st,
            stops,
            (pt(start_center.x, start_center.y), start_radius.max(0.0)),
            (pt(end_center.x, end_center.y), end_radius.max(0.0)),
            extend,
        )
    });
}

pub(crate) fn extend_of(options: CGGradientDrawingOptions) -> (bool, bool) {
    (
        options.contains(CGGradientDrawingOptions::DrawsBeforeStartLocation),
        options.contains(CGGradientDrawingOptions::DrawsAfterEndLocation),
    )
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextDrawShading(c: Option<&CGContext>, shading: Option<&CGShading>) {
    let Some(shading) = shading.map(super::gradient::shading_imp) else { return };
    // The program's function runs before the state is taken, so it may
    // use the context.
    let stops = shading.stops();
    with(c, |st| shading.draw(st, stops));
}

// Text: the settings, and glyphs drawn in the context's font.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetCharacterSpacing(c: Option<&CGContext>, spacing: CGFloat) {
    with(c, |st| Arc::make_mut(&mut st.gs.cg.text).spacing = spacing);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetTextPosition(c: Option<&CGContext>, x: CGFloat, y: CGFloat) {
    with(c, |st| {
        let [a, b, cc, d, _, _] = st.text_matrix.as_coeffs();
        st.text_matrix = Affine::new([a, b, cc, d, x, y]);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetTextPosition(c: Option<&CGContext>) -> CGPoint {
    with(c, |st| {
        let [.., x, y] = st.text_matrix.as_coeffs();
        CGPoint { x, y }
    })
    .unwrap_or(CGPoint::ZERO)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetTextMatrix(c: Option<&CGContext>, t: CGAffineTransform) {
    with(c, |st| st.text_matrix = kurbo_of(t));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetTextMatrix(c: Option<&CGContext>) -> CGAffineTransform {
    with(c, |st| transform_of(st.text_matrix)).unwrap_or(super::geometry::IDENTITY)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetTextDrawingMode(c: Option<&CGContext>, mode: CGTextDrawingMode) {
    with(c, |st| Arc::make_mut(&mut st.gs.cg.text).mode = mode);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetFont(c: Option<&CGContext>, font: Option<&CGFont>) {
    let font = font.map(|f| super::font::font_imp(f).retain());
    with(c, |st| Arc::make_mut(&mut st.gs.cg.text).font = font);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetFontSize(c: Option<&CGContext>, size: CGFloat) {
    with(c, |st| Arc::make_mut(&mut st.gs.cg.text).size = size);
}

/// # Safety
///
/// `name` is a NUL-terminated string.
#[allow(deprecated)]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextSelectFont(
    c: Option<&CGContext>,
    name: *const c_char,
    size: CGFloat,
    _text_encoding: objc2_core_graphics::CGTextEncoding,
) {
    if name.is_null() {
        return;
    }
    // SAFETY: as the caller promises.
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned();
    let font = super::font::named(&name);
    with(c, |st| {
        let text = Arc::make_mut(&mut st.gs.cg.text);
        if font.is_some() {
            text.font = font;
        }
        text.size = size;
    });
}

/// Glyphs of the context's font (`CGContextSetFont`) at its size, drawn
/// by CoreText's glyph drawing (`coretext::draw`): at `lpositions` in text
/// space, through the text matrix; the text position stays.
///
/// # Safety
///
/// `glyphs` and `lpositions` hold `count` items.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextShowGlyphsAtPositions(
    c: Option<&CGContext>,
    glyphs: *const CGGlyph,
    lpositions: *const CGPoint,
    count: usize,
) {
    // SAFETY: as the caller promises.
    let (glyphs, positions) = unsafe { (super::slice(glyphs, count), super::slice(lpositions, count)) };
    if glyphs.len() != positions.len() {
        return;
    }
    let positions: Vec<(f64, f64)> = positions.iter().map(|p| (p.x, p.y)).collect();
    with(c, |st| crate::coretext::draw::draw_cg_glyphs(st, glyphs, &positions));
}

/// Glyphs at the text position, one after another by their advances
/// (with the character spacing), moving the text position on.
fn show_advancing(st: &mut ContextState, glyphs: &[CGGlyph], advances: &[(f64, f64)]) {
    let mut positions = Vec::with_capacity(glyphs.len());
    let (mut x, mut y) = (0.0, 0.0);
    for &(dx, dy) in advances {
        positions.push((x, y));
        (x, y) = (x + dx, y + dy);
    }
    // Positions are from the text position (the text matrix's
    // translation), which then moves on by the advances, through the
    // matrix.
    let [a, b, cc, d, e, f] = st.text_matrix.as_coeffs();
    crate::coretext::draw::draw_cg_glyphs(st, glyphs, &positions);
    let moved = Affine::new([a, b, cc, d, 0.0, 0.0]) * Point::new(x, y);
    st.text_matrix = Affine::new([a, b, cc, d, e + moved.x, f + moved.y]);
}

/// # Safety
///
/// `glyphs` holds `count` glyphs.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextShowGlyphs(c: Option<&CGContext>, g: *const CGGlyph, count: usize) {
    // SAFETY: as the caller promises.
    let glyphs = unsafe { super::slice(g, count) };
    with(c, |st| {
        let advances: Vec<(f64, f64)> =
            crate::coretext::draw::cg_advances(st, glyphs).into_iter().map(|a| (a, 0.0)).collect();
        show_advancing(st, glyphs, &advances);
    });
}

/// # Safety
///
/// `glyphs` holds `count` glyphs.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextShowGlyphsAtPoint(
    c: Option<&CGContext>,
    x: CGFloat,
    y: CGFloat,
    glyphs: *const CGGlyph,
    count: usize,
) {
    CGContextSetTextPosition(c, x, y);
    // SAFETY: as the caller promises.
    unsafe { CGContextShowGlyphs(c, glyphs, count) };
}

/// # Safety
///
/// `glyphs` and `advances` hold `count` items.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextShowGlyphsWithAdvances(
    c: Option<&CGContext>,
    glyphs: *const CGGlyph,
    advances: *const CGSize,
    count: usize,
) {
    // SAFETY: as the caller promises.
    let (glyphs, advances) = unsafe { (super::slice(glyphs, count), super::slice(advances, count)) };
    if glyphs.len() != advances.len() {
        return;
    }
    let advances: Vec<(f64, f64)> = advances.iter().map(|a| (a.width, a.height)).collect();
    with(c, |st| show_advancing(st, glyphs, &advances));
}

/// Mac Roman text in the context's font, at the text position.
///
/// # Safety
///
/// `string` holds `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextShowText(c: Option<&CGContext>, string: *const c_char, length: usize) {
    // SAFETY: as the caller promises.
    let bytes = unsafe { super::slice(string.cast::<u8>(), length) };
    with(c, |st| {
        let glyphs = crate::coretext::draw::cg_glyphs_for_text(st, bytes);
        let advances: Vec<(f64, f64)> =
            crate::coretext::draw::cg_advances(st, &glyphs).into_iter().map(|a| (a, 0.0)).collect();
        show_advancing(st, &glyphs, &advances);
    });
}

/// # Safety
///
/// `string` holds `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextShowTextAtPoint(
    c: Option<&CGContext>,
    x: CGFloat,
    y: CGFloat,
    string: *const c_char,
    length: usize,
) {
    CGContextSetTextPosition(c, x, y);
    // SAFETY: as the caller promises.
    unsafe { CGContextShowText(c, string, length) };
}

// Antialiasing and fonts' settings.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetShouldAntialias(c: Option<&CGContext>, should_antialias: bool) {
    with(c, |st| st.gs.aa = should_antialias);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetAllowsAntialiasing(c: Option<&CGContext>, allows_antialiasing: bool) {
    with(c, |st| st.allows_aa = allows_antialiasing);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetShouldSmoothFonts(c: Option<&CGContext>, should_smooth_fonts: bool) {
    with(c, |st| Arc::make_mut(&mut st.gs.cg.text).smoothing[0] = should_smooth_fonts);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetAllowsFontSmoothing(c: Option<&CGContext>, allows_font_smoothing: bool) {
    with(c, |st| st.allows_fonts[0] = allows_font_smoothing);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetShouldSubpixelPositionFonts(
    c: Option<&CGContext>,
    should_subpixel_position_fonts: bool,
) {
    with(c, |st| Arc::make_mut(&mut st.gs.cg.text).smoothing[1] = should_subpixel_position_fonts);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetAllowsFontSubpixelPositioning(
    c: Option<&CGContext>,
    allows_font_subpixel_positioning: bool,
) {
    with(c, |st| st.allows_fonts[1] = allows_font_subpixel_positioning);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetShouldSubpixelQuantizeFonts(
    c: Option<&CGContext>,
    should_subpixel_quantize_fonts: bool,
) {
    with(c, |st| Arc::make_mut(&mut st.gs.cg.text).smoothing[2] = should_subpixel_quantize_fonts);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetAllowsFontSubpixelQuantization(
    c: Option<&CGContext>,
    allows_font_subpixel_quantization: bool,
) {
    with(c, |st| st.allows_fonts[2] = allows_font_subpixel_quantization);
}

// Transparency layers.

/// Begin a transparency layer: what's drawn until its end is composited
/// as one, with the alpha, blend mode and shadow in force now, which are
/// reset inside it. As in CoreGraphics, beginning a layer saves the
/// graphics state on the same stack `CGContextSaveGState` does, and ending
/// it restores whatever is on top; a layer's rectangle narrows the clip.
pub(crate) fn begin_layer(st: &mut ContextState, rect: Option<CGRect>) {
    st.save();
    st.layers.push(st.stack.len());
    let mut clip = st.gs.clip;
    if let Some(r) = rect.filter(|r| !super::geometry::is_null(*r)) {
        let r = crate::context::map_rect_any(st.gs.ctm, super::geometry::standardize(r));
        clip = clip.intersect(&r);
        st.gs.clip = clip;
    }
    let draw = Draw {
        xf: tiny_skia::Transform::identity(),
        blend: st.gs.blend,
        aa: true,
        clip,
        mask: st.gs.mask.clone(),
        shadow: st.gs.shadow.clone(),
    };
    st.push(Op::BeginGroup { alpha: 1.0, draw });
    st.gs.cg.alpha = 1.0;
    st.gs.blend = Blend::SourceOver;
    st.gs.shadow = None;
}

/// End the innermost transparency layer, restoring the graphics state from
/// before it.
pub(crate) fn end_layer(st: &mut ContextState) {
    if st.layers.pop().is_none() {
        return;
    }
    st.rec.ops.push(Op::EndGroup);
    if let Some(gs) = st.stack.pop() {
        st.gs = gs;
        st.sync();
    }
    st.flush();
}

/// # Safety
///
/// `auxiliary_info` is null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextBeginTransparencyLayer(
    c: Option<&CGContext>,
    _auxiliary_info: Option<&CFDictionary>,
) {
    with(c, |st| begin_layer(st, None));
}

/// # Safety
///
/// `aux_info` is null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextBeginTransparencyLayerWithRect(
    c: Option<&CGContext>,
    rect: CGRect,
    _aux_info: Option<&CFDictionary>,
) {
    with(c, |st| begin_layer(st, Some(rect)));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextEndTransparencyLayer(c: Option<&CGContext>) {
    with(c, end_layer);
}

// Output.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextFlush(c: Option<&CGContext>) {
    with(c, ContextState::flush);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSynchronize(c: Option<&CGContext>) {
    with(c, ContextState::flush);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSynchronizeAttributes(c: &CGContext) {
    with_state(c, ContextState::flush);
}

/// Pages are PDF contexts' (not here).
///
/// # Safety
///
/// `media_box` is null or points at a rectangle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGContextBeginPage(_c: Option<&CGContext>, _media_box: *const CGRect) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextEndPage(_c: Option<&CGContext>) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextSetEDRTargetHeadroom(_c: &CGContext, _headroom: f32) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetEDRTargetHeadroom(_c: &CGContext) -> f32 {
    1.0
}

// Coordinates.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextGetUserSpaceToDeviceSpaceTransform(c: Option<&CGContext>) -> CGAffineTransform {
    with(c, |st| transform_of(st.user_to_device())).unwrap_or(super::geometry::IDENTITY)
}

fn to_device(c: Option<&CGContext>, inverse: bool) -> Affine {
    with(c, |st| {
        let m = st.user_to_device();
        if !inverse {
            m
        } else if m.determinant() != 0.0 {
            m.inverse()
        } else {
            Affine::IDENTITY
        }
    })
    .unwrap_or(Affine::IDENTITY)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextConvertPointToDeviceSpace(c: Option<&CGContext>, point: CGPoint) -> CGPoint {
    cg_point(to_device(c, false) * pt(point.x, point.y))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextConvertPointToUserSpace(c: Option<&CGContext>, point: CGPoint) -> CGPoint {
    cg_point(to_device(c, true) * pt(point.x, point.y))
}

fn size_through(m: Affine, size: CGSize) -> CGSize {
    let [a, b, c, d, _, _] = m.as_coeffs();
    CGSize { width: a * size.width + c * size.height, height: b * size.width + d * size.height }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextConvertSizeToDeviceSpace(c: Option<&CGContext>, size: CGSize) -> CGSize {
    size_through(to_device(c, false), size)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextConvertSizeToUserSpace(c: Option<&CGContext>, size: CGSize) -> CGSize {
    size_through(to_device(c, true), size)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextConvertRectToDeviceSpace(c: Option<&CGContext>, rect: CGRect) -> CGRect {
    super::geometry::CGRectApplyAffineTransform(rect, transform_of(to_device(c, false)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGContextConvertRectToUserSpace(c: Option<&CGContext>, rect: CGRect) -> CGRect {
    super::geometry::CGRectApplyAffineTransform(rect, transform_of(to_device(c, true)))
}

#[cfg(test)]
mod tests {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{ClassType, msg_send};
    use objc2_app_kit::{NSBezierPath, NSColor, NSGraphicsContext};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    use super::*;
    use crate::graphics::Xf;

    /// What a display pass's drawing costs through CoreGraphics and through
    /// AppKit's equivalents, recording only (the ops a window records for
    /// the render thread), without rasterizing: the calls a real application makes
    /// most, 1000 times each way.
    #[test]
    #[ignore = "a benchmark; run in release mode"]
    fn timing_cg_calls_against_appkit_calls() {
        crate::load_shell::<NSColor>();
        crate::load_shell::<NSBezierPath>();
        const N: usize = 1000;
        let r = CGRect { origin: CGPoint { x: 2.0, y: 3.0 }, size: CGSize { width: 40.0, height: 20.0 } };
        // SAFETY: no transform.
        let rounded = unsafe { super::super::path::CGPathCreateWithRoundedRect(r, 4.0, 4.0, std::ptr::null()) };
        // SAFETY: a Create function's +1 reference, released at the end.
        let path = unsafe { &*rounded.expect("a path").as_ptr() };
        let fill = super::super::color::srgb_color([1.0, 0.0, 0.0, 1.0]);
        let stroke = super::super::color::srgb_color([0.0, 0.0, 1.0, 1.0]);
        let ns_rect = NSRect::new(NSPoint::new(2.0, 3.0), NSSize::new(40.0, 20.0));
        // SAFETY: class methods of AppKit's classes, as a program calls them.
        let bezier: Retained<NSBezierPath> = unsafe {
            msg_send![NSBezierPath::class(), bezierPathWithRoundedRect: ns_rect, xRadius: 4.0f64, yRadius: 4.0f64]
        };
        let (red, blue) = (NSColor::redColor(), NSColor::blueColor());
        let run = |f: &dyn Fn(&CGContext, &NSGraphicsContext)| {
            crate::backend::median(|| {
                crate::graphics::begin_recording();
                crate::graphics::set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, 800.0, 600.0));
                let ns = crate::context::current().expect("recording");
                // SAFETY: -[NSGraphicsContext CGContext] hands out the
                // context's CGContext.
                let cg: *mut CGContext = unsafe { msg_send![&*ns, CGContext] };
                // SAFETY: the context keeps it alive.
                let cg = unsafe { &*cg };
                for _ in 0..N {
                    f(cg, &ns);
                }
                drop(crate::graphics::end_recording());
            })
        };
        let cg_ms = run(&|c, _| {
            let c = Some(c);
            CGContextSaveGState(c);
            CGContextTranslateCTM(c, 1.0, 1.0);
            CGContextSetFillColorWithColor(c, Some(fill.as_cg()));
            CGContextAddPath(c, Some(path));
            CGContextFillPath(c);
            CGContextSetStrokeColorWithColor(c, Some(stroke.as_cg()));
            CGContextSetLineWidth(c, 2.0);
            CGContextAddPath(c, Some(path));
            CGContextStrokePath(c);
            CGContextFillRect(c, r);
            CGContextRestoreGState(c);
        });
        let ns_ms = run(&|_, ns| {
            // SAFETY: AppKit's methods, as a program calls them.
            unsafe {
                let _: () = msg_send![ns, saveGraphicsState];
                let t: Retained<AnyObject> = msg_send![objc2::class!(NSAffineTransform), transform];
                let _: () = msg_send![&*t, translateXBy: 1.0f64, yBy: 1.0f64];
                let _: () = msg_send![&*t, concat];
                let _: () = msg_send![&*red, setFill];
                let _: () = msg_send![&*bezier, fill];
                let _: () = msg_send![&*blue, setStroke];
                let _: () = msg_send![&*bezier, setLineWidth: 2.0f64];
                let _: () = msg_send![&*bezier, stroke];
                crate::context::NSRectFill(ns_rect);
                let _: () = msg_send![ns, restoreGraphicsState];
            }
        });
        // SAFETY: the Create function's reference.
        unsafe { objc2::ffi::objc_release((path as *const CGPath).cast_mut().cast()) };
        let per = |ms: f64| ms * 1e6 / N as f64;
        println!(
            "a display pass's calls, recorded ({N} times): CoreGraphics {:.0} ns, AppKit {:.0} ns an iteration",
            per(cg_ms),
            per(ns_ms)
        );
    }
}
