//! `NSGraphicsContext` and the graphics state everything draws with.
//!
//! A context owns a [`ContextState`]: where drawing goes (its target), the
//! graphics state in force and the ones saved. There are two targets:
//!
//! - **Record**, for a window's display pass: ops are collected for the
//!   render thread, in the coordinates of the layer being drawn.
//! - **Bitmap**, for `+graphicsContextWithBitmapImageRep:`,
//!   `cacheDisplayInRect:toBitmapImageRep:` and image drawing handlers:
//!   each op is rasterized at once, on the calling thread, into the
//!   representation's pixels. This is also how tests read pixels on Linux
//!   without a compositor.
//!
//! The graphics state ([`GState`]) is what a CGContext keeps too, so that
//! a CGContext shim can map its calls onto it one to one: the CTM (user
//! space to layer points, a full affine), the clip (its bounds as a
//! rectangle, plus paths when it isn't one), fill and stroke colors
//! already resolved to RGBA, the compositing operation, antialiasing,
//! image interpolation, the shadow and the pattern phase.
//!
//! The current context is per thread, as in AppKit. Drawing methods
//! (`-[NSColor setFill]`, `-[NSBezierPath fill]`, `NSRectFill`) reach the
//! current state through a thread local and a `RefCell`, with no message
//! sends. Clipping is analytic where it can be: with an axis-aligned CTM a
//! rectangle clip narrows the clip rectangle, and only rotated rectangles
//! and paths (`addClip`, `setClip`) produce clip paths, which the
//! rasterizer turns into masks.
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
use objc2_foundation::{NSInteger, NSPoint, NSRect, NSSize};

use crate::graphics::{Recorder, Xf};
use crate::protocol::{Blend, ClipPath, Color, Draw, Op, Rect, ShadowSpec};

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
fn map_rect(a: Affine, r: NSRect) -> Rect {
    let p0 = a * kurbo::Point::new(r.origin.x, r.origin.y);
    let p1 = a * kurbo::Point::new(r.origin.x + r.size.width, r.origin.y + r.size.height);
    Rect::new(p0.x.min(p1.x) as f32, p0.y.min(p1.y) as f32, p0.x.max(p1.x) as f32, p0.y.max(p1.y) as f32)
}

/// Where a context's drawing goes.
pub(crate) enum Target {
    /// Ops for the render thread.
    Record,
    /// Ops rasterized at once into a bitmap's pixels.
    Bitmap(Retained<NSBitmapImageRep>),
}

pub(crate) struct ContextState {
    pub target: Target,
    pub flipped: bool,
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
    /// The ops, and what string drawing reads of the state.
    pub rec: Recorder,
}

impl ContextState {
    fn new(target: Target, flipped: bool, base: Affine, clip: Rect, scale: f64) -> ContextState {
        let rec = Recorder::new(matches!(target, Target::Bitmap(_)));
        let gs = GState::fresh(base, clip);
        let mut st = ContextState { target, flipped, base, gs, stack: Vec::new(), view_clip: clip, scale, rec };
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
    fn sync(&mut self) {
        self.rec.xf = nearest_xf(self.gs.ctm);
        self.rec.clip = self.gs.clip;
    }

    pub fn push(&mut self, op: Op) {
        self.rec.ops.push(op);
        self.flush();
    }

    /// Rasterize what a bitmap context has recorded.
    pub fn flush(&mut self) {
        if let Target::Bitmap(rep) = &self.target
            && !self.rec.ops.is_empty()
        {
            let ops = std::mem::take(&mut self.rec.ops);
            crate::bitmap::rasterize(rep, &ops);
            let mut ops = ops;
            ops.clear();
            self.rec.ops = ops;
        }
    }

    pub fn save(&mut self) {
        self.stack.push(self.gs.clone());
    }

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
            if whole || !gs.aa {
                let rect = rect.intersect(&gs.clip);
                if rect.is_empty() {
                    return;
                }
                let op = match blend {
                    Blend::SourceOver => Op::Fill { rect, color },
                    Blend::Copy if color[3] >= 1.0 => Op::Fill { rect, color },
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
        self.push(Op::FillPath {
            path: Arc::new(path),
            even_odd: false,
            paint: crate::protocol::Paint::Solid(color),
            draw,
        });
    }

    /// Narrow the clip to `r` (user space).
    pub fn clip_rect(&mut self, r: NSRect) {
        if self.gs.axis_aligned() {
            let rect = map_rect(self.gs.ctm, r);
            self.gs.clip = self.gs.clip.intersect(&rect);
            self.sync();
        } else if let Some(path) =
            tiny_skia::Rect::from_xywh(r.origin.x as f32, r.origin.y as f32, r.size.width as f32, r.size.height as f32)
                .map(tiny_skia::PathBuilder::from_rect)
        {
            self.clip_path(Arc::new(path), false, true);
        }
    }

    /// Narrow the clip to a path (user space).
    pub fn clip_path(&mut self, path: Arc<tiny_skia::Path>, even_odd: bool, replace: bool) {
        let xf = to_skia(self.gs.ctm);
        let bounds = path.bounds().transform(xf);
        let bounds = bounds.map_or(Rect::default(), |b| Rect::new(b.left(), b.top(), b.right(), b.bottom()));
        let clip = ClipPath { path, even_odd, xf, aa: self.gs.aa };
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
    state: RefCell<ContextState>,
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
                imp(c).state().save();
            }
            CLASS_STACK.with(|s| s.borrow_mut().push(current));
        }

        #[unsafe(method(restoreGraphicsState))]
        fn restore_graphics_state_class() {
            let Some(saved) = CLASS_STACK.with(|s| s.borrow_mut().pop()) else { return };
            if let Some(c) = &saved {
                imp(c).state().restore();
            }
            set_current(saved);
        }

        #[unsafe(method_id(graphicsContextWithBitmapImageRep:))]
        fn with_bitmap(rep: &NSBitmapImageRep) -> Option<Retained<NSGraphicsContext>> {
            bitmap_context(rep)
        }

        #[unsafe(method(saveGraphicsState))]
        fn save_graphics_state(&self) {
            self.state().save();
        }

        #[unsafe(method(restoreGraphicsState))]
        fn restore_graphics_state(&self) {
            self.state().restore();
        }

        #[unsafe(method(flushGraphics))]
        fn flush_graphics(&self) {
            self.state().flush();
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.state().flipped
        }

        #[unsafe(method(isDrawingToScreen))]
        fn is_drawing_to_screen(&self) -> bool {
            true
        }

        #[unsafe(method(shouldAntialias))]
        fn should_antialias(&self) -> bool {
            self.state().gs.aa
        }

        #[unsafe(method(setShouldAntialias:))]
        fn set_should_antialias(&self, flag: bool) {
            self.state().gs.aa = flag;
        }

        #[unsafe(method(imageInterpolation))]
        fn image_interpolation(&self) -> NSImageInterpolation {
            self.state().gs.interpolation
        }

        #[unsafe(method(setImageInterpolation:))]
        fn set_image_interpolation(&self, value: NSImageInterpolation) {
            self.state().gs.interpolation = value;
        }

        #[unsafe(method(compositingOperation))]
        fn compositing_operation(&self) -> NSCompositingOperation {
            NSCompositingOperation(self.state().gs.blend as usize)
        }

        #[unsafe(method(setCompositingOperation:))]
        fn set_compositing_operation(&self, op: NSCompositingOperation) {
            self.state().gs.blend = Blend::from_raw(op.0);
        }

        #[unsafe(method(patternPhase))]
        fn pattern_phase(&self) -> NSPoint {
            self.state().gs.phase
        }

        #[unsafe(method(setPatternPhase:))]
        fn set_pattern_phase(&self, phase: NSPoint) {
            self.state().gs.phase = phase;
        }

        #[unsafe(method(colorRenderingIntent))]
        fn color_rendering_intent(&self) -> NSColorRenderingIntent {
            self.state().gs.intent
        }

        #[unsafe(method(setColorRenderingIntent:))]
        fn set_color_rendering_intent(&self, intent: NSColorRenderingIntent) {
            self.state().gs.intent = intent;
        }
    }

    unsafe impl NSObjectProtocol for NSGraphicsContextImpl {}
);

impl NSGraphicsContextImpl {
    fn state(&self) -> std::cell::RefMut<'_, ContextState> {
        self.ivars().state.borrow_mut()
    }
}

impl Drop for NSGraphicsContextImpl {
    fn drop(&mut self) {
        self.ivars().state.borrow_mut().flush();
    }
}

fn imp(c: &NSGraphicsContext) -> &NSGraphicsContextImpl {
    // SAFETY: every NSGraphicsContext is an NSGraphicsContextImpl.
    unsafe { &*(c as *const NSGraphicsContext).cast::<NSGraphicsContextImpl>() }
}

fn make(state: ContextState) -> Retained<NSGraphicsContext> {
    crate::load_shell::<NSGraphicsContext>();
    let this: Allocated<NSGraphicsContextImpl> = NSGraphicsContextImpl::alloc();
    let this = this.set_ivars(ContextIvars { state: RefCell::new(state) });
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
    let old = CURRENT.with(|c| std::mem::replace(&mut *c.borrow_mut(), context));
    // What a bitmap context drew is in its pixels once it stops being
    // current.
    if let Some(old) = old
        && let Ok(mut st) = imp(&old).ivars().state.try_borrow_mut()
    {
        st.flush();
    }
}

/// Run `f` on the current context's state, if there is one (and it isn't
/// already in use further up the stack).
pub(crate) fn with_state<R>(f: impl FnOnce(&mut ContextState) -> R) -> Option<R> {
    CURRENT.with(|c| {
        let c = c.borrow();
        let mut st = imp(c.as_ref()?).ivars().state.try_borrow_mut().ok()?;
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
    Some(make(ContextState::new(Target::Bitmap(rep.retain()), false, base, clip, scale)))
}

/// Make a fresh bitmap context flipped: user space's origin at the top
/// left, y growing down, which is the layer's own orientation.
pub(crate) fn flip(context: &NSGraphicsContext) {
    with_state_of(context, |st| {
        st.flipped = true;
        st.base = Affine::IDENTITY;
        st.set_ctm(Affine::IDENTITY);
    });
}

/// Run `f` on `context`'s state.
pub(crate) fn with_state_of<R>(context: &NSGraphicsContext, f: impl FnOnce(&mut ContextState) -> R) -> Option<R> {
    let mut st = imp(context).ivars().state.try_borrow_mut().ok()?;
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
    begin_current(make(ContextState::new(Target::Record, false, base, all, scale)));
}

/// The ops recorded since [`begin_recording`], and the recorder with them.
pub(crate) fn end_recording() -> Recorder {
    match end_current() {
        Some(c) => std::mem::replace(&mut imp(&c).state().rec, Recorder::new(false)),
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
        let old_clip = std::mem::replace(&mut st.view_clip, clip);
        let old_flipped = std::mem::replace(&mut st.flipped, flipped);
        st.sync();
        (depth, old_clip, old_flipped)
    });
    ViewMark { saved, dirty: prior, _appearance: appearance }
}

/// What [`begin_view`] replaced.
pub(crate) struct ViewMark {
    saved: Option<(usize, Rect, bool)>,
    dirty: NSRect,
    _appearance: crate::appearance::Drawing,
}

/// The view's `drawRect:` returned: drop what it saved and didn't
/// restore, and put back the state from before it.
pub(crate) fn end_view(mark: ViewMark) {
    DIRTY.with(|d| d.set(mark.dirty));
    let Some((depth, clip, flipped)) = mark.saved else { return };
    with_state(|st| {
        st.stack.truncate(depth + 1);
        if let Some(gs) = st.stack.pop() {
            st.gs = gs;
        }
        st.view_clip = clip;
        st.flipped = flipped;
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
        imp(&ctx).state().flush();
    }
}

/// A bitmap for caching `rect` of `view`: 8-bit RGBA, the rectangle's size
/// in points, at the backing scale of the view's window (1 outside one).
pub(crate) fn bitmap_for(view: &crate::views::NSViewImpl, rect: NSRect) -> Option<Retained<NSBitmapImageRep>> {
    let scale = crate::views::window_of(view).map_or(1.0, crate::window::backing_scale);
    let (w, h) = ((rect.size.width * scale).ceil() as usize, (rect.size.height * scale).ceil() as usize);
    crate::image_rep::new_bitmap(w, h, rect.size)
}
