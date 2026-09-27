//! `NSGraphicsContext` and the graphics state everything draws with.
//!
//! The state lives in a CGContext (`coregraphics::context`), and an
//! `NSGraphicsContext` wraps one: drawing through `-[NSGraphicsContext
//! CGContext]` and through AppKit's drawing methods is the same drawing,
//! and their saves and restores interleave, as on macOS. A context's
//! [`ContextState`] holds where drawing goes (its target), the graphics
//! state in force and the ones saved. There are three targets:
//!
//! - **Record**, for a window's display pass: ops are collected for the
//!   render thread, in the coordinates of the layer being drawn.
//! - **Bitmap**, for `+graphicsContextWithBitmapImageRep:`,
//!   `cacheDisplayInRect:toBitmapImageRep:` and image drawing handlers:
//!   each op is rasterized at once, on the calling thread, into the
//!   representation's pixels. This is also how tests read pixels on Linux
//!   without a compositor.
//! - **Surface**, for `CGBitmapContextCreate`: the same, into memory in
//!   the layout the program asked for (`coregraphics::bitmap`).
//!
//! The graphics state ([`GState`]) is what a CGContext keeps, and CGContext
//! calls map onto it one to one: the CTM (user space to layer points, a
//! full affine), the clip (its bounds as a rectangle, plus paths when it
//! isn't one), fill and stroke colors already resolved to RGBA, the
//! compositing operation, antialiasing, image interpolation, the shadow and
//! the pattern phase, and CoreGraphics' own part ([`CgState`]: line
//! settings, the global alpha, color spaces, text settings). What
//! CoreGraphics keeps outside the graphics state, the current path and the
//! text matrix, is in the context state.
//!
//! The current context is per thread, as in AppKit. Drawing methods
//! (`-[NSColor setFill]`, `-[NSBezierPath fill]`, `NSRectFill`) reach the
//! current state through a thread local and a `RefCell`, with no message
//! sends; CoreGraphics' functions reach their context's the same way,
//! through the object they're given. Clipping is analytic where it can be:
//! with an axis-aligned CTM a rectangle clip narrows the clip rectangle,
//! and only rotated rectangles and paths (`addClip`, `setClip`,
//! `CGContextClip`) produce clip paths, which the rasterizer turns into
//! masks.
//!
//! Each view's `drawRect:` starts from a fresh state ([`begin_view`]), so
//! a color or clip one view leaves set doesn't reach the next, and saves
//! it didn't restore are discarded ([`end_view`]).

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::Arc;

use kurbo::Affine;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBitmapImageRep, NSColor, NSColorRenderingIntent, NSCompositingOperation, NSGraphicsContext, NSImageInterpolation,
};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSInteger, NSPoint, NSRect, NSSize};

use crate::coregraphics::context::{CGContextImpl, CgState, CtxPath};
use crate::graphics::{Recorder, Xf};
use crate::protocol::{Blend, ClipPath, Color, Draw, Op, Paint, Rect, ShadowSpec};

/// The drawing state `saveGraphicsState` saves and `restoreGraphicsState`
/// restores.
#[derive(Clone, Debug)]
pub(crate) struct GState {
    /// User space to layer points.
    pub ctm: Affine,
    /// The clip's bounds, in layer points.
    pub clip: Rect,
    /// Paths the clip is the intersection of, when it isn't `clip` alone.
    pub mask: Option<Arc<[ClipPath]>>,
    pub fill: Color,
    pub stroke: Color,
    pub blend: Blend,
    pub aa: bool,
    pub interpolation: NSImageInterpolation,
    pub shadow: Option<Arc<ShadowSpec>>,
    pub phase: NSPoint,
    pub intent: NSColorRenderingIntent,
    /// The compositing operation `-[NSGraphicsContext
    /// compositingOperation]` reports: what AppKit set, which
    /// `CGContextSetBlendMode` doesn't change (it sets `blend` alone).
    pub operation: Blend,
    /// What only CoreGraphics' calls set.
    pub cg: CgState,
}

impl GState {
    fn fresh(ctm: Affine, clip: Rect) -> GState {
        GState {
            ctm,
            clip,
            mask: None,
            fill: [0.0, 0.0, 0.0, 1.0],
            stroke: [0.0, 0.0, 0.0, 1.0],
            blend: Blend::SourceOver,
            aa: true,
            interpolation: NSImageInterpolation::Default,
            shadow: None,
            phase: NSPoint::ZERO,
            intent: NSColorRenderingIntent::RelativeColorimetric,
            operation: Blend::SourceOver,
            cg: CgState::default(),
        }
    }

    /// What an op drawn now carries.
    pub fn draw(&self) -> Draw {
        Draw {
            xf: to_skia(self.ctm),
            blend: self.blend,
            aa: self.aa,
            clip: self.clip,
            mask: self.mask.clone(),
            shadow: self.shadow.clone(),
        }
    }

    /// Whether the CTM keeps rectangles rectangles, with no path in the
    /// clip: then fills and clips are worked out here, as rectangles.
    pub fn axis_aligned(&self) -> bool {
        let [_, b, c, _, _, _] = self.ctm.as_coeffs();
        b == 0.0 && c == 0.0
    }
}

pub(crate) fn to_skia(a: Affine) -> tiny_skia::Transform {
    let [a, b, c, d, e, f] = a.as_coeffs().map(|v| v as f32);
    tiny_skia::Transform::from_row(a, b, c, d, e, f)
}

pub(crate) fn xf_affine(xf: Xf) -> Affine {
    Affine::new([1.0, 0.0, 0.0, xf.a, xf.tx, xf.ty])
}

/// The translate-and-flip nearest `a`, for string drawing, which places
/// text with one.
fn nearest_xf(a: Affine) -> Xf {
    let [_, _, _, d, e, f] = a.as_coeffs();
    Xf { tx: e, a: if d < 0.0 { -1.0 } else { 1.0 }, ty: f }
}

/// A rectangle's image under an axis-aligned transform.
pub(crate) fn map_rect(a: Affine, r: NSRect) -> Rect {
    let p0 = a * kurbo::Point::new(r.origin.x, r.origin.y);
    let p1 = a * kurbo::Point::new(r.origin.x + r.size.width, r.origin.y + r.size.height);
    Rect::new(p0.x.min(p1.x) as f32, p0.y.min(p1.y) as f32, p0.x.max(p1.x) as f32, p0.y.max(p1.y) as f32)
}

/// The bounds of a rectangle's image under any transform.
pub(crate) fn map_rect_any(a: Affine, r: NSRect) -> Rect {
    let (x0, y0) = (r.origin.x, r.origin.y);
    let (x1, y1) = (x0 + r.size.width, y0 + r.size.height);
    let pts = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| a * kurbo::Point::new(x, y));
    let (mut lo, mut hi) = (pts[0], pts[0]);
    for p in &pts[1..] {
        lo = kurbo::Point::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = kurbo::Point::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    Rect::new(lo.x as f32, lo.y as f32, hi.x as f32, hi.y as f32)
}

/// Where a context's drawing goes.
pub(crate) enum Target {
    /// Ops for the render thread.
    Record,
    /// Ops rasterized at once into a bitmap's pixels.
    Bitmap(Retained<NSBitmapImageRep>),
    /// Ops rasterized at once into a CoreGraphics bitmap context's memory.
    Surface(Box<crate::coregraphics::bitmap::Surface>),
}

pub(crate) struct ContextState {
    pub target: Target,
    /// Whether AppKit's drawing takes the context as flipped now: the
    /// current `NSGraphicsContext`'s own flippedness, which for the
    /// contexts AppKit makes is `view_flipped`.
    pub flipped: bool,
    /// Whether the view drawing (or the bitmap) is flipped: what
    /// `NSGraphicsContext`s AppKit makes report, and what `flipped` goes
    /// back to when one of them is current again.
    pub view_flipped: bool,
    /// Where a fresh user space (a view's base coordinates: the window's,
    /// or the bitmap's with its origin at the bottom left) maps in the
    /// layer; `-[NSAffineTransform set]` sets the CTM relative to it.
    pub base: Affine,
    pub gs: GState,
    pub stack: Vec<GState>,
    /// The clip a view started with, which `-[NSBezierPath setClip]`
    /// replaces the clip within.
    pub view_clip: Rect,
    /// Device pixels per layer point, as far as the main thread knows (the
    /// window's backing scale, or a bitmap's pixels per point).
    pub scale: f64,
    /// Layer points to CoreGraphics' device space: pixels, the origin at
    /// the bottom left of the window or bitmap, y up. `CGContextGetCTM` is
    /// this after the CTM. While a view draws in a window, it's the view's
    /// own space instead, as macOS draws views into layers of their own:
    /// the CTM starts out as the identity (measured on macOS).
    pub device: Affine,
    /// While a view draws in a window (a recording): whether it's flipped,
    /// for the user-to-device transform (its layer's pixels).
    pub view_device: Option<bool>,
    /// The current path, in layer points, as CoreGraphics builds it (not
    /// part of the graphics state).
    pub path: CtxPath,
    /// CoreGraphics' text matrix, the text position its translation (not
    /// part of the graphics state).
    pub text_matrix: Affine,
    /// `CGContextSetAllowsAntialiasing` and its kin for fonts (smoothing,
    /// subpixel positioning, subpixel quantization): context settings the
    /// graphics state's flags can't override.
    pub allows_aa: bool,
    pub allows_fonts: [bool; 3],
    /// Transparency layers (and their groups) open: a bitmap waits for the
    /// outermost to end before drawing them.
    pub layers: Vec<usize>,
    /// The ops, and what string drawing reads of the state.
    pub rec: Recorder,
}

impl ContextState {
    pub(crate) fn new(target: Target, flipped: bool, base: Affine, clip: Rect, scale: f64, device: Affine) -> Self {
        let immediate = !matches!(target, Target::Record);
        let rec = Recorder::new(immediate);
        let gs = GState::fresh(base, clip);
        let mut st = ContextState {
            target,
            flipped,
            view_flipped: flipped,
            base,
            gs,
            stack: Vec::new(),
            view_clip: clip,
            scale,
            device,
            view_device: None,
            path: CtxPath::default(),
            text_matrix: Affine::IDENTITY,
            allows_aa: true,
            allows_fonts: [true; 3],
            layers: Vec::new(),
            rec,
        };
        st.sync();
        st
    }

    pub fn pixels_per_point(&self) -> f64 {
        self.scale
    }

    /// A fresh state drawing with `xf` into `clip`.
    pub fn reset(&mut self, xf: Xf, clip: Rect) {
        self.gs = GState::fresh(xf_affine(xf), clip);
        self.view_clip = clip;
        self.sync();
    }

    /// Keep what string drawing reads in step with the state.
    pub(crate) fn sync(&mut self) {
        self.rec.xf = nearest_xf(self.gs.ctm);
        self.rec.clip = self.gs.clip;
    }

    pub fn push(&mut self, mut op: Op) {
        if self.gs.cg.alpha < 1.0 || !self.allows_aa {
            self.adjust(&mut op);
        }
        self.rec.ops.push(op);
        self.flush();
    }

    /// Apply what applies to every op but the ops don't carry themselves:
    /// CoreGraphics' global alpha, and antialiasing the context doesn't
    /// allow.
    pub(crate) fn adjust(&self, op: &mut Op) {
        let alpha = self.gs.cg.alpha.clamp(0.0, 1.0);
        let fade = |c: &mut Color| c[3] *= alpha;
        let paint = |p: &mut Paint| match p {
            Paint::Solid(c) => c[3] *= alpha,
            Paint::Gradient(g) => {
                let mut spec = (**g).clone();
                spec.stops.iter_mut().for_each(|(_, c)| c[3] *= alpha);
                *g = Arc::new(spec);
            }
        };
        // A shape's shadow is as opaque as its paint, faded already; an
        // image's and a group's shadows are faded here.
        let draw = |d: &mut Draw, fade_shadow: bool| {
            d.aa &= self.allows_aa;
            if fade_shadow
                && alpha < 1.0
                && let Some(s) = &mut d.shadow
            {
                let mut spec = (**s).clone();
                spec.color[3] *= alpha;
                *s = Arc::new(spec);
            }
        };
        match op {
            Op::Fill { color, .. } => fade(color),
            Op::FillWith { color, blend, .. } => {
                if *blend != Blend::Clear {
                    fade(color);
                }
            }
            Op::FillPath { paint: p, draw: d, .. } | Op::StrokePath { paint: p, draw: d, .. } => {
                paint(p);
                draw(d, false);
            }
            Op::Image { alpha: a, draw: d, .. } => {
                *a *= alpha;
                draw(d, true);
            }
            Op::BeginGroup { alpha: a, draw: d } => {
                *a *= alpha;
                draw(d, true);
            }
            Op::Glyphs(run) => fade(&mut run.color),
            Op::EndGroup => {}
        }
    }

    /// Rasterize what a bitmap context has recorded (once no transparency
    /// layer is open).
    pub fn flush(&mut self) {
        if self.rec.ops.is_empty() || !self.layers.is_empty() {
            return;
        }
        match &mut self.target {
            Target::Record => return,
            Target::Bitmap(rep) => crate::bitmap::rasterize(rep, &self.rec.ops),
            Target::Surface(s) => s.rasterize(&self.rec.ops),
        }
        self.rec.ops.clear();
    }

    pub fn save(&mut self) {
        self.stack.push(self.gs.clone());
    }

    /// Restore the graphics state saved last: a transparency layer's
    /// save too, as in CoreGraphics (whose layers save on the same stack).
    pub fn restore(&mut self) {
        if let Some(gs) = self.stack.pop() {
            self.gs = gs;
            self.sync();
        }
    }

    pub fn set_ctm(&mut self, ctm: Affine) {
        self.gs.ctm = ctm;
        self.sync();
    }

    /// Fill `r` (user space) with `color` composited by `blend`.
    pub fn fill_rect(&mut self, r: NSRect, color: Color, blend: Blend) {
        if r.size.width <= 0.0 || r.size.height <= 0.0 {
            return;
        }
        let gs = &self.gs;
        if gs.axis_aligned() && gs.mask.is_none() && gs.shadow.is_none() {
            let rect = map_rect(gs.ctm, r);
            // Rectangles on whole points (all of them, usually) keep to the
            // pixels whose centers they cover, so neighbors meet without
            // seams at any scale; so do ones on whole device pixels (half
            // points at 2×), which antialiasing would cover the same;
            // other fractional edges are antialiased.
            let s = self.scale as f32;
            let edges = [rect.x0, rect.y0, rect.x1, rect.y1];
            let whole = edges.iter().all(|v| (v - v.round()).abs() < 1e-3)
                || edges.iter().all(|v| (v * s - (v * s).round()).abs() < 1e-3);
            if whole || !(gs.aa && self.allows_aa) {
                let rect = rect.intersect(&gs.clip);
                if rect.is_empty() {
                    return;
                }
                let op = match blend {
                    Blend::SourceOver => Op::Fill { rect, color },
                    Blend::Copy if color[3] >= 1.0 && self.gs.cg.alpha >= 1.0 => Op::Fill { rect, color },
                    blend => Op::FillWith { rect, color, blend },
                };
                self.push(op);
                return;
            }
        }
        let Some(path) =
            tiny_skia::Rect::from_xywh(r.origin.x as f32, r.origin.y as f32, r.size.width as f32, r.size.height as f32)
                .map(tiny_skia::PathBuilder::from_rect)
        else {
            return;
        };
        let mut draw = self.gs.draw();
        draw.blend = blend;
        self.push(Op::FillPath { path: Arc::new(path), even_odd: false, paint: Paint::Solid(color), draw });
    }

    /// Narrow the clip to `r` (user space): analytically when it keeps to
    /// whole points or pixels (as fills do), with an antialiased clip path
    /// when its edges fall inside pixels or the CTM turns it.
    pub fn clip_rect(&mut self, r: NSRect) {
        if self.gs.axis_aligned() {
            let rect = map_rect(self.gs.ctm, r);
            let s = self.scale as f32;
            let edges = [rect.x0, rect.y0, rect.x1, rect.y1];
            let whole = edges.iter().all(|v| (v - v.round()).abs() < 1e-3)
                || edges.iter().all(|v| (v * s - (v * s).round()).abs() < 1e-3);
            if whole || rect.is_empty() || !(self.gs.aa && self.allows_aa) {
                self.gs.clip = self.gs.clip.intersect(&rect);
                self.sync();
                return;
            }
        }
        match tiny_skia::Rect::from_xywh(
            r.origin.x as f32,
            r.origin.y as f32,
            r.size.width as f32,
            r.size.height as f32,
        )
        .map(tiny_skia::PathBuilder::from_rect)
        {
            Some(path) => self.clip_path(Arc::new(path), false, false),
            None => {
                // An empty rectangle leaves nothing to draw in, where it is.
                self.gs.clip = self.gs.clip.intersect(&map_rect_any(self.gs.ctm, r));
                self.sync();
            }
        }
    }

    /// Narrow the clip to a path (user space).
    pub fn clip_path(&mut self, path: Arc<tiny_skia::Path>, even_odd: bool, replace: bool) {
        let xf = to_skia(self.gs.ctm);
        self.clip_path_with(path, even_odd, replace, xf);
    }

    /// Narrow the clip to a path whose points `xf` maps to layer points.
    pub fn clip_path_with(
        &mut self,
        path: Arc<tiny_skia::Path>,
        even_odd: bool,
        replace: bool,
        xf: tiny_skia::Transform,
    ) {
        let aa = self.gs.aa && self.allows_aa;
        let bounds = path.bounds().transform(xf);
        let bounds = bounds.map_or(Rect::NOWHERE, |b| Rect::new(b.left(), b.top(), b.right(), b.bottom()));
        let clip = ClipPath { path, even_odd, xf, aa, image: None };
        if replace {
            self.gs.clip = self.view_clip.intersect(&bounds);
            self.gs.mask = Some(Arc::from([clip]));
        } else {
            self.gs.clip = self.gs.clip.intersect(&bounds);
            let mut paths: Vec<ClipPath> = self.gs.mask.as_deref().map(<[ClipPath]>::to_vec).unwrap_or_default();
            paths.push(clip);
            self.gs.mask = Some(paths.into());
        }
        self.sync();
    }
}

pub(crate) struct ContextIvars {
    /// The CGContext holding the state.
    cg: Retained<CGContextImpl>,
    /// Flipped or not as `+graphicsContextWithCGContext:flipped:` said;
    /// `None` for the contexts AppKit makes, which views flip as they draw.
    flipped: Option<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the state is used
    // by one thread at a time, as AppKit's contexts are.
    #[unsafe(super(NSObject))]
    #[name = "NSGraphicsContext"]
    #[ivars = ContextIvars]
    pub(crate) struct NSGraphicsContextImpl;

    impl NSGraphicsContextImpl {
        #[unsafe(method_id(currentContext))]
        fn current_context() -> Option<Retained<NSGraphicsContext>> {
            current()
        }

        #[unsafe(method(setCurrentContext:))]
        fn set_current_context(context: Option<&NSGraphicsContext>) {
            set_current(context.map(|c| c.retain()));
        }

        #[unsafe(method(currentContextDrawingToScreen))]
        fn current_context_drawing_to_screen() -> bool {
            // AppKit says so for bitmap contexts too.
            true
        }

        #[unsafe(method(saveGraphicsState))]
        fn save_graphics_state_class() {
            let current = current();
            if let Some(c) = &current {
                imp(c).with(ContextState::save);
            }
            CLASS_STACK.with(|s| s.borrow_mut().push(current));
        }

        #[unsafe(method(restoreGraphicsState))]
        fn restore_graphics_state_class() {
            let Some(saved) = CLASS_STACK.with(|s| s.borrow_mut().pop()) else { return };
            if let Some(c) = &saved {
                imp(c).with(ContextState::restore);
            }
            set_current(saved);
        }

        #[unsafe(method_id(graphicsContextWithBitmapImageRep:))]
        fn with_bitmap(rep: &NSBitmapImageRep) -> Option<Retained<NSGraphicsContext>> {
            bitmap_context(rep)
        }

        #[unsafe(method_id(graphicsContextWithCGContext:flipped:))]
        fn with_cg_context(cg: &CGContext, flipped: bool) -> Retained<NSGraphicsContext> {
            let cg = crate::coregraphics::context::imp(cg).retain();
            wrap(cg, Some(flipped))
        }

        // CoreGraphics' types go out as pointers (their own encodings).
        #[unsafe(method(CGContext))]
        fn cg_context(&self) -> *mut CGContext {
            Retained::autorelease_ptr(crate::coregraphics::context::as_cg(&self.ivars().cg).retain())
        }

        #[unsafe(method(saveGraphicsState))]
        fn save_graphics_state(&self) {
            self.with(ContextState::save);
        }

        #[unsafe(method(restoreGraphicsState))]
        fn restore_graphics_state(&self) {
            self.with(ContextState::restore);
        }

        #[unsafe(method(flushGraphics))]
        fn flush_graphics(&self) {
            self.with(ContextState::flush);
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().flipped.unwrap_or_else(|| self.with(|st| st.view_flipped).unwrap_or(false))
        }

        #[unsafe(method(isDrawingToScreen))]
        fn is_drawing_to_screen(&self) -> bool {
            true
        }

        #[unsafe(method(shouldAntialias))]
        fn should_antialias(&self) -> bool {
            self.with(|st| st.gs.aa).unwrap_or(true)
        }

        #[unsafe(method(setShouldAntialias:))]
        fn set_should_antialias(&self, flag: bool) {
            self.with(|st| st.gs.aa = flag);
        }

        #[unsafe(method(imageInterpolation))]
        fn image_interpolation(&self) -> NSImageInterpolation {
            self.with(|st| st.gs.interpolation).unwrap_or(NSImageInterpolation::Default)
        }

        #[unsafe(method(setImageInterpolation:))]
        fn set_image_interpolation(&self, value: NSImageInterpolation) {
            self.with(|st| st.gs.interpolation = value);
        }

        #[unsafe(method(compositingOperation))]
        fn compositing_operation(&self) -> NSCompositingOperation {
            NSCompositingOperation(self.with(|st| st.gs.operation).unwrap_or(Blend::SourceOver) as usize)
        }

        #[unsafe(method(setCompositingOperation:))]
        fn set_compositing_operation(&self, op: NSCompositingOperation) {
            self.with(|st| {
                st.gs.blend = Blend::from_raw(op.0);
                st.gs.operation = st.gs.blend;
            });
        }

        #[unsafe(method(patternPhase))]
        fn pattern_phase(&self) -> NSPoint {
            self.with(|st| st.gs.phase).unwrap_or(NSPoint::ZERO)
        }

        #[unsafe(method(setPatternPhase:))]
        fn set_pattern_phase(&self, phase: NSPoint) {
            self.with(|st| st.gs.phase = phase);
        }

        #[unsafe(method(colorRenderingIntent))]
        fn color_rendering_intent(&self) -> NSColorRenderingIntent {
            self.with(|st| st.gs.intent).unwrap_or(NSColorRenderingIntent::Default)
        }

        #[unsafe(method(setColorRenderingIntent:))]
        fn set_color_rendering_intent(&self, intent: NSColorRenderingIntent) {
            self.with(|st| st.gs.intent = intent);
        }
    }

    unsafe impl NSObjectProtocol for NSGraphicsContextImpl {}
);

impl NSGraphicsContextImpl {
    /// Run `f` on the state; nothing if it's in use further up the stack
    /// (a data provider's or shading's callback reaching the context it's
    /// drawing into), rather than panicking out of a method.
    fn with<R>(&self, f: impl FnOnce(&mut ContextState) -> R) -> Option<R> {
        let mut st = self.ivars().cg.ivars().state.try_borrow_mut().ok()?;
        Some(f(&mut st))
    }
}

fn imp(c: &NSGraphicsContext) -> &NSGraphicsContextImpl {
    // SAFETY: every NSGraphicsContext is an NSGraphicsContextImpl.
    unsafe { &*(c as *const NSGraphicsContext).cast::<NSGraphicsContextImpl>() }
}

/// A new context drawing with `state`: a CGContext holding it and the
/// `NSGraphicsContext` wrapping that.
fn make(state: ContextState) -> Retained<NSGraphicsContext> {
    wrap(crate::coregraphics::context::new_context(state), None)
}

/// An `NSGraphicsContext` for `cg`'s drawing, flipped as `flipped` says
/// (or as the views drawing say, for `None`) while it's the current
/// context: each wrapper has its own flippedness, as in AppKit.
pub(crate) fn wrap(cg: Retained<CGContextImpl>, flipped: Option<bool>) -> Retained<NSGraphicsContext> {
    crate::load_shell::<NSGraphicsContext>();
    let this: Allocated<NSGraphicsContextImpl> = NSGraphicsContextImpl::alloc();
    let this = this.set_ivars(ContextIvars { cg, flipped });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSGraphicsContextImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSGraphicsContextImpl is the class NSGraphicsContext names.
    unsafe { Retained::cast_unchecked(this) }
}

thread_local! {
    static CURRENT: RefCell<Option<Retained<NSGraphicsContext>>> = const { RefCell::new(None) };
    static CLASS_STACK: RefCell<Vec<Option<Retained<NSGraphicsContext>>>> = const { RefCell::new(Vec::new()) };
    /// Contexts the display pass and view snapshots replaced, to put back.
    static SAVED: RefCell<Vec<Option<Retained<NSGraphicsContext>>>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn current() -> Option<Retained<NSGraphicsContext>> {
    CURRENT.with(|c| c.borrow().clone())
}

fn set_current(context: Option<Retained<NSGraphicsContext>>) {
    // AppKit's drawing takes the state as flipped as the current context
    // is: one made from a CGContext as it was made, one AppKit made as the
    // view drawing is.
    if let Some(c) = &context {
        let own = imp(c).ivars().flipped;
        imp(c).with(|st| st.flipped = own.unwrap_or(st.view_flipped));
    }
    let old = CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), context));
    // What a bitmap context drew is in its pixels once it stops being
    // current.
    if let Some(old) = old
        && let Ok(mut st) = imp(&old).ivars().cg.ivars().state.try_borrow_mut()
    {
        st.flush();
    }
}

/// Run `f` on the current context's state, if there is one (and it isn't
/// already in use further up the stack).
pub(crate) fn with_state<R>(f: impl FnOnce(&mut ContextState) -> R) -> Option<R> {
    CURRENT.with(|c| {
        let c = c.borrow();
        let mut st = imp(c.as_ref()?).ivars().cg.ivars().state.try_borrow_mut().ok()?;
        Some(f(&mut st))
    })
}

/// A context drawing into `rep`, if its pixels are a format contexts
/// draw into: 8-bit RGBA, alpha last and premultiplied.
pub(crate) fn bitmap_context(rep: &NSBitmapImageRep) -> Option<Retained<NSGraphicsContext>> {
    let ((pw, _), points) = crate::bitmap::drawable(rep)?;
    let base = Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, points.height]);
    let clip = Rect::new(0.0, 0.0, points.width as f32, points.height as f32);
    let scale = if points.width > 0.0 { pw as f64 / points.width } else { 1.0 };
    let device = Affine::new([scale, 0.0, 0.0, -scale, 0.0, points.height * scale]);
    Some(make(ContextState::new(Target::Bitmap(rep.retain()), false, base, clip, scale, device)))
}

/// Make a fresh bitmap context flipped: user space's origin at the top
/// left, y growing down, which is the layer's own orientation.
pub(crate) fn flip(context: &NSGraphicsContext) {
    with_state_of(context, |st| {
        st.flipped = true;
        st.view_flipped = true;
        st.base = Affine::IDENTITY;
        st.set_ctm(Affine::IDENTITY);
    });
}

/// Run `f` on `context`'s state.
pub(crate) fn with_state_of<R>(context: &NSGraphicsContext, f: impl FnOnce(&mut ContextState) -> R) -> Option<R> {
    let mut st = imp(context).ivars().cg.ivars().state.try_borrow_mut().ok()?;
    Some(f(&mut st))
}

/// Make `context` current until [`end_current`], saving the one that was.
pub(crate) fn begin_current(context: Retained<NSGraphicsContext>) {
    let old = current();
    SAVED.with(|s| s.borrow_mut().push(old));
    set_current(Some(context));
}

/// Put back the context [`begin_current`] replaced, returning the one it
/// made current.
pub(crate) fn end_current() -> Option<Retained<NSGraphicsContext>> {
    let this = current();
    let old = SAVED.with(|s| s.borrow_mut().pop()).flatten();
    set_current(old);
    this
}

/// A context recording ops for the render thread, current until
/// [`end_recording`]: `base` maps the window's (or the document's)
/// coordinates to the layer, which shows at `scale` pixels a point.
pub(crate) fn begin_recording(base: Xf, scale: f64) {
    let base = xf_affine(base);
    let all = Rect::new(f32::MIN, f32::MIN, f32::MAX, f32::MAX);
    // CoreGraphics' device space: the window's base coordinates in pixels.
    let device = Affine::scale(scale) * base.inverse();
    begin_current(make(ContextState::new(Target::Record, false, base, all, scale, device)));
}

/// The ops recorded since [`begin_recording`], and the recorder with them.
pub(crate) fn end_recording() -> Recorder {
    match end_current() {
        Some(c) => imp(&c)
            .with(|st| std::mem::replace(&mut st.rec, Recorder::new(false)))
            .unwrap_or_else(|| Recorder::new(false)),
        None => Recorder::new(false),
    }
}

/// A view's `drawRect:` is about to run: give it a fresh state whose user
/// space is its bounds (`xf` to the layer), clipped to `clip` (layer
/// points: what's visible of it and being drawn), flipped as it is, in its
/// effective appearance. Returns what [`end_view`] needs to put things
/// back.
pub(crate) fn begin_view(view: &crate::views::NSViewImpl, xf: Xf, clip: Rect) -> ViewMark {
    let flipped = crate::views::is_flipped(view);
    let appearance = crate::appearance::Drawing::push(crate::appearance::effective(view));
    let prior = DIRTY.with(|d| d.replace(xf.inverse_rect(clip)));
    let saved = with_state(|st| {
        let depth = st.stack.len();
        st.stack.push(st.gs.clone());
        st.gs = GState::fresh(xf_affine(xf), clip);
        let saved = Saved {
            depth,
            clip: std::mem::replace(&mut st.view_clip, clip),
            flipped: std::mem::replace(&mut st.flipped, flipped),
            view_flipped: std::mem::replace(&mut st.view_flipped, flipped),
            device: st.device,
            view_device: st.view_device,
        };
        // In a window, CoreGraphics sees the view's own space, as macOS
        // draws views into layers of their own.
        if matches!(st.target, Target::Record) {
            let to_layer = xf_affine(xf);
            if to_layer.determinant() != 0.0 {
                st.device = to_layer.inverse();
                st.view_device = Some(flipped);
            }
        }
        st.path = CtxPath::default();
        st.sync();
        saved
    });
    ViewMark { saved, dirty: prior, _appearance: appearance }
}

/// What [`begin_view`] replaced.
pub(crate) struct ViewMark {
    saved: Option<Saved>,
    dirty: NSRect,
    _appearance: crate::appearance::Drawing,
}

/// The state a view's drawing replaced, to put back.
struct Saved {
    /// How deep the stack was.
    depth: usize,
    clip: Rect,
    flipped: bool,
    view_flipped: bool,
    device: Affine,
    view_device: Option<bool>,
}

/// The view's `drawRect:` returned: drop what it saved and didn't
/// restore, and put back the state from before it.
pub(crate) fn end_view(mark: ViewMark) {
    DIRTY.with(|d| d.set(mark.dirty));
    let Some(saved) = mark.saved else { return };
    let depth = saved.depth;
    with_state(|st| {
        // Transparency layers it left open end here.
        while st.layers.last().is_some_and(|&d| d > depth) {
            crate::coregraphics::context::end_layer(st);
        }
        st.stack.truncate(depth + 1);
        if let Some(gs) = st.stack.pop() {
            st.gs = gs;
        }
        st.view_clip = saved.clip;
        (st.flipped, st.view_flipped) = (saved.flipped, saved.view_flipped);
        (st.device, st.view_device) = (saved.device, saved.view_device);
        st.path = CtxPath::default();
        st.sync();
        st.flush();
    });
}

/// A view's opacity over it and its subviews (`alphaValue`), from
/// [`opacity`]: closes the group it opened when dropped.
pub(crate) struct Opacity(bool);

/// `view` is about to be drawn with its subviews into `clip` (layer
/// points): `None` if it's fully transparent and shouldn't be; otherwise,
/// below full opacity, they're drawn as one group faded by its alpha until
/// the returned guard drops. Drawing into a bitmap (a view snapshot)
/// leaves partial opacity out, and draws the view being snapshotted even
/// at 0, as AppKit does.
pub(crate) fn opacity(view: &crate::views::NSViewImpl, clip: Rect) -> Option<Opacity> {
    let alpha = crate::views::alpha_of(view).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        let root = SNAPSHOT_ROOT.with(Cell::get) == view as *const crate::views::NSViewImpl as usize;
        return root.then_some(Opacity(false));
    }
    if alpha >= 1.0 {
        return Some(Opacity(false));
    }
    let opened = with_state(|st| {
        if !matches!(st.target, Target::Record) {
            return false;
        }
        let draw = Draw {
            xf: tiny_skia::Transform::identity(),
            blend: Blend::SourceOver,
            aa: true,
            clip,
            mask: None,
            shadow: None,
        };
        st.push(Op::BeginGroup { alpha: alpha as f32, draw });
        true
    });
    Some(Opacity(opened == Some(true)))
}

impl Drop for Opacity {
    fn drop(&mut self) {
        if self.0 {
            with_state(|st| st.push(Op::EndGroup));
        }
    }
}

thread_local! {
    static DIRTY: Cell<NSRect> = const { Cell::new(NSRect::ZERO) };
    /// The view `cacheDisplayInRect:` and its kin are drawing, which is
    /// drawn even at no opacity.
    static SNAPSHOT_ROOT: Cell<usize> = const { Cell::new(0) };
}

/// The rectangle being drawn of the view whose `drawRect:` is running, in
/// its coordinates (empty outside drawing).
pub(crate) fn dirty() -> NSRect {
    DIRTY.with(Cell::get)
}

/// Where [`dirty`] lives, for `getRectsBeingDrawn:count:`: valid while the
/// thread runs.
pub(crate) fn dirty_ptr() -> *const NSRect {
    DIRTY.with(|d| d.as_ptr().cast_const())
}

// The drawing functions AppKit exports.

fn fill_list(rects: NonNull<NSRect>, count: NSInteger, color: impl Fn(usize) -> Option<Color>, blend: Blend) {
    let count = usize::try_from(count).unwrap_or(0);
    // SAFETY: the caller passes `count` rectangles.
    let rects = unsafe { std::slice::from_raw_parts(rects.as_ptr(), count) };
    let colors: Vec<Option<Color>> = (0..count).map(&color).collect();
    with_state(|st| {
        for (r, c) in rects.iter().zip(colors) {
            let c = c.unwrap_or(st.gs.fill);
            st.fill_rect(*r, c, blend);
        }
    });
}

fn fill_with(r: NSRect, blend: Blend) {
    with_state(|st| st.fill_rect(r, st.gs.fill, blend));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRectFill(rect: NSRect) {
    fill_with(rect, Blend::Copy);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRectFillUsingOperation(rect: NSRect, op: NSCompositingOperation) {
    fill_with(rect, Blend::from_raw(op.0));
}

/// # Safety
///
/// `rects` points to `count` rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSRectFillList(rects: NonNull<NSRect>, count: NSInteger) {
    fill_list(rects, count, |_| None, Blend::Copy);
}

/// # Safety
///
/// `rects` points to `count` rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSRectFillListUsingOperation(
    rects: NonNull<NSRect>,
    count: NSInteger,
    op: NSCompositingOperation,
) {
    fill_list(rects, count, |_| None, Blend::from_raw(op.0));
}

/// # Safety
///
/// `rects` and `grays` point to `count` rectangles and gray levels.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSRectFillListWithGrays(rects: NonNull<NSRect>, grays: NonNull<f64>, count: NSInteger) {
    let grays = grays.as_ptr();
    // SAFETY: the caller passes `count` gray levels.
    fill_list(rects, count, |i| Some(unsafe { *grays.add(i) } as f32).map(|g| [g, g, g, 1.0]), Blend::Copy);
}

/// # Safety
///
/// `rects` and `colors` point to `count` rectangles and colors.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSRectFillListWithColors(
    rects: NonNull<NSRect>,
    colors: NonNull<NonNull<NSColor>>,
    count: NSInteger,
) {
    // SAFETY: as documented.
    unsafe { NSRectFillListWithColorsUsingOperation(rects, colors, count, NSCompositingOperation::Copy) }
}

/// # Safety
///
/// `rects` and `colors` point to `count` rectangles and colors.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSRectFillListWithColorsUsingOperation(
    rects: NonNull<NSRect>,
    colors: NonNull<NonNull<NSColor>>,
    count: NSInteger,
    op: NSCompositingOperation,
) {
    let colors = colors.as_ptr();
    // Colors resolve before the state is borrowed: a dynamic color runs
    // its provider.
    let resolve = |i: usize| {
        // SAFETY: the caller passes `count` colors, each a live NSColor.
        let color = unsafe { (*colors.add(i)).as_ref() };
        Some(crate::color::resolve(color))
    };
    fill_list(rects, count, resolve, Blend::from_raw(op.0));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRectClip(rect: NSRect) {
    with_state(|st| st.clip_rect(rect));
}

/// # Safety
///
/// `rects` points to `count` rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSRectClipList(rects: NonNull<NSRect>, count: NSInteger) {
    let count = usize::try_from(count).unwrap_or(0);
    // SAFETY: the caller passes `count` rectangles.
    let rects = unsafe { std::slice::from_raw_parts(rects.as_ptr(), count) };
    // The union of the rectangles, as one path.
    let mut pb = tiny_skia::PathBuilder::new();
    for r in rects {
        if let Some(r) =
            tiny_skia::Rect::from_xywh(r.origin.x as f32, r.origin.y as f32, r.size.width as f32, r.size.height as f32)
        {
            pb.push_rect(r);
        }
    }
    match (rects, pb.finish()) {
        // None leave the clip as it was, as in AppKit.
        ([], _) => {}
        ([one], _) => NSRectClip(*one),
        (_, Some(path)) => {
            with_state(|st| st.clip_path(Arc::new(path), false, false));
        }
        // Only empty rectangles: nothing is left to draw in.
        (_, None) => NSRectClip(NSRect::ZERO),
    }
}

fn frame(rect: NSRect, width: f64, blend: Blend) {
    let (o, s) = (rect.origin, rect.size);
    let (w, h) = (width.min(s.width / 2.0), width.min(s.height / 2.0));
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let side = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    with_state(|st| {
        let c = st.gs.fill;
        st.fill_rect(side(o.x, o.y, s.width, h), c, blend);
        st.fill_rect(side(o.x, o.y + s.height - h, s.width, h), c, blend);
        st.fill_rect(side(o.x, o.y + h, w, s.height - 2.0 * h), c, blend);
        st.fill_rect(side(o.x + s.width - w, o.y + h, w, s.height - 2.0 * h), c, blend);
    });
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSFrameRect(rect: NSRect) {
    frame(rect, 1.0, Blend::Copy);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSFrameRectWithWidth(rect: NSRect, width: f64) {
    frame(rect, width, Blend::Copy);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSFrameRectWithWidthUsingOperation(rect: NSRect, width: f64, op: NSCompositingOperation) {
    frame(rect, width, Blend::from_raw(op.0));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSEraseRect(rect: NSRect) {
    with_state(|st| st.fill_rect(rect, [1.0, 1.0, 1.0, 1.0], Blend::Copy));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSDrawWindowBackground(rect: NSRect) {
    let color = crate::color::resolve(&NSColor::windowBackgroundColor());
    with_state(|st| st.fill_rect(rect, color, Blend::Copy));
}

/// A one-point frame; current AppKit draws it solid, not dotted.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSDottedFrameRect(rect: NSRect) {
    frame(rect, 1.0, Blend::Copy);
}

/// Deprecated, and AppKit leaves the pixels as they were.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSHighlightRect(_rect: NSRect) {}

// Drawing views into bitmaps: `cacheDisplayInRect:toBitmapImageRep:` and
// its kin, which run the display pass's recording into a bitmap context.

/// Draw `rect` of `view` and its subviews into `rep`, the rectangle
/// filling it.
pub(crate) fn cache_display(view: &crate::views::NSViewImpl, rect: NSRect, rep: &NSBitmapImageRep) {
    let Some(ctx) = bitmap_context(rep) else { return };
    draw_view_into(view, rect, ctx);
}

/// `displayRectIgnoringOpacity:inContext:`: draw `rect` of `view` into
/// `context` as if it were the whole of it.
pub(crate) fn display_in(view: &crate::views::NSViewImpl, rect: NSRect, context: &NSGraphicsContext) {
    draw_view_into(view, rect, context.retain());
}

fn draw_view_into(view: &crate::views::NSViewImpl, rect: NSRect, ctx: Retained<NSGraphicsContext>) {
    crate::view_layout::prepare_to_draw(view);
    // The view's rectangle to the context's layer (points, top-left
    // origin): the rectangle's corner nearest the view's origin goes to
    // the layer's corner.
    let (w, h) = (rect.size.width as f32, rect.size.height as f32);
    let xf = if crate::views::is_flipped(view) {
        Xf { tx: -rect.origin.x, a: 1.0, ty: -rect.origin.y }
    } else {
        Xf { tx: -rect.origin.x, a: -1.0, ty: rect.origin.y + rect.size.height }
    };
    let area = Rect::new(0.0, 0.0, w, h);
    begin_current(ctx);
    let outer = SNAPSHOT_ROOT.with(|r| r.replace(view as *const crate::views::NSViewImpl as usize));
    // Snapshots show no focus ring, as on macOS.
    crate::window::record(view, xf, area, area, None);
    SNAPSHOT_ROOT.with(|r| r.set(outer));
    if let Some(ctx) = end_current() {
        imp(&ctx).with(ContextState::flush);
    }
}

/// A bitmap for caching `rect` of `view`: 8-bit RGBA, the rectangle's size
/// in points, at the backing scale of the view's window (1 outside one).
pub(crate) fn bitmap_for(view: &crate::views::NSViewImpl, rect: NSRect) -> Option<Retained<NSBitmapImageRep>> {
    let scale = crate::views::window_of(view).map_or(1.0, crate::window::backing_scale);
    let (w, h) = ((rect.size.width * scale).ceil() as usize, (rect.size.height * scale).ceil() as usize);
    crate::image_rep::new_bitmap(w, h, rect.size)
}
