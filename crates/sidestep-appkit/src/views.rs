//! The responder chain and the view hierarchy: `NSResponder`, `NSView`,
//! `NSClipView` and `NSScrollView`.
//!
//! Views keep their geometry in ivars. Everything that subclasses may
//! override (`isFlipped`, `drawRect:`, `hitTest:`, the event methods) is
//! reached by message; the rest is plain Rust.
//!
//! A window has layers: its own surface, and one per clip view, holding that
//! clip view's document. A view's placement is where its bounds land in its
//! layer, found by composing each view's map to its superview:
//! `x' = x + tx` and `y' = a·y + ty`, with `a = -1` where a view and its
//! superview disagree about flipping.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSClipView, NSEvent, NSResponder, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::graphics::Xf;
use crate::protocol::{LayerId, ROOT_LAYER, Rect};
use crate::window::{self, NSWindowImpl};

/// A reference that doesn't retain, as AppKit's back pointers are.
type Unretained<T> = Cell<Option<NonNull<T>>>;

// NSResponder

#[derive(Default)]
pub(crate) struct ResponderIvars {
    next: Unretained<NSResponder>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSResponder"]
    #[ivars = ResponderIvars]
    pub(crate) struct NSResponderImpl;

    impl NSResponderImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ResponderIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(nextResponder))]
        fn next_responder(&self) -> Option<Retained<NSResponder>> {
            // SAFETY: the next responder outlives the link (it is the
            // superview or window, which own this responder).
            self.ivars().next.get().map(|p| unsafe { p.as_ref() }.retain())
        }

        #[unsafe(method(setNextResponder:))]
        fn set_next_responder(&self, next: Option<&NSResponder>) {
            self.ivars().next.set(next.map(NonNull::from));
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            forward(self, |next| next.mouseDown(event));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            forward(self, |next| next.mouseUp(event));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            forward(self, |next| next.mouseDragged(event));
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            forward(self, |next| next.mouseMoved(event));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            forward(self, |next| next.rightMouseDown(event));
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            forward(self, |next| next.rightMouseUp(event));
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            forward(self, |next| next.otherMouseDown(event));
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            forward(self, |next| next.otherMouseUp(event));
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            forward(self, |next| next.scrollWheel(event));
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            forward(self, |next| next.keyDown(event));
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            forward(self, |next| next.keyUp(event));
        }
    }

    unsafe impl NSObjectProtocol for NSResponderImpl {}
);

/// Pass an event a responder doesn't handle up the chain.
fn forward(this: &NSResponderImpl, send: impl FnOnce(&NSResponder)) {
    if let Some(next) = this.ivars().next.get() {
        // SAFETY: as in `nextResponder`.
        send(unsafe { next.as_ref() });
    }
}

// NSView

pub(crate) struct ViewIvars {
    frame: Cell<NSRect>,
    bounds_origin: Cell<NSPoint>,
    superview: Unretained<NSView>,
    subviews: RefCell<Vec<Retained<NSView>>>,
    window: Unretained<NSWindow>,
    autoresizing: Cell<NSAutoresizingMaskOptions>,
    hidden: Cell<bool>,
    /// An NSClipView: its document is drawn into a layer of its own.
    is_clip: Cell<bool>,
}

impl ViewIvars {
    fn new(frame: NSRect) -> Self {
        ViewIvars {
            frame: Cell::new(frame),
            bounds_origin: Cell::new(NSPoint::ZERO),
            superview: Cell::new(None),
            subviews: RefCell::new(Vec::new()),
            window: Cell::new(None),
            autoresizing: Cell::new(NSAutoresizingMaskOptions::ViewNotSizable),
            hidden: Cell::new(false),
            is_clip: Cell::new(false),
        }
    }
}

define_class!(
    #[unsafe(super(NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSView"]
    #[ivars = ViewIvars]
    pub(crate) struct NSViewImpl;

    impl NSViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ViewIvars::new(frame));
            // SAFETY: NSResponder's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithFrame: NSRect::ZERO] }
        }

        #[unsafe(method(frame))]
        fn frame(&self) -> NSRect {
            self.ivars().frame.get()
        }

        #[unsafe(method(setFrame:))]
        fn set_frame(&self, frame: NSRect) {
            change_frame(self, frame);
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            change_frame(self, NSRect::new(self.ivars().frame.get().origin, size));
        }

        #[unsafe(method(setFrameOrigin:))]
        fn set_frame_origin(&self, origin: NSPoint) {
            change_frame(self, NSRect::new(origin, self.ivars().frame.get().size));
        }

        #[unsafe(method(bounds))]
        fn bounds(&self) -> NSRect {
            bounds(self)
        }

        #[unsafe(method(setBoundsOrigin:))]
        fn set_bounds_origin(&self, origin: NSPoint) {
            change_bounds_origin(self, origin);
        }

        #[unsafe(method(visibleRect))]
        fn visible_rect(&self) -> NSRect {
            visible_rect(self)
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            false
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            false
        }

        #[unsafe(method(isHidden))]
        fn is_hidden(&self) -> bool {
            self.ivars().hidden.get()
        }

        #[unsafe(method(setHidden:))]
        fn set_hidden(&self, hidden: bool) {
            invalidate(self, bounds(self));
            self.ivars().hidden.set(hidden);
            invalidate(self, bounds(self));
        }

        #[unsafe(method_id(superview))]
        fn superview(&self) -> Option<Retained<NSView>> {
            superview(self).map(|s| as_view(s).retain())
        }

        #[unsafe(method_id(window))]
        fn window(&self) -> Option<Retained<NSWindow>> {
            // SAFETY: a window detaches its views before it goes away.
            self.ivars().window.get().map(|w| unsafe { w.as_ref() }.retain())
        }

        #[unsafe(method(addSubview:))]
        fn add_subview(&self, view: &NSView) {
            add_subview(self, view);
        }

        #[unsafe(method(removeFromSuperview))]
        fn remove_from_superview(&self) {
            remove_from_superview(self);
        }

        #[unsafe(method(autoresizingMask))]
        fn autoresizing_mask(&self) -> NSAutoresizingMaskOptions {
            self.ivars().autoresizing.get()
        }

        #[unsafe(method(setAutoresizingMask:))]
        fn set_autoresizing_mask(&self, mask: NSAutoresizingMaskOptions) {
            self.ivars().autoresizing.set(mask);
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews(&self, old: NSSize) {
            let subviews = self.ivars().subviews.borrow().clone();
            for view in subviews {
                view.resizeWithOldSuperviewSize(old);
            }
        }

        #[unsafe(method(resizeWithOldSuperviewSize:))]
        fn resize_with_old_superview_size(&self, old: NSSize) {
            autoresize(self, old);
        }

        #[unsafe(method(setNeedsDisplay:))]
        fn set_needs_display(&self, flag: bool) {
            if flag {
                invalidate(self, bounds(self));
            }
        }

        #[unsafe(method(setNeedsDisplayInRect:))]
        fn set_needs_display_in_rect(&self, rect: NSRect) {
            invalidate(self, rect);
        }

        #[unsafe(method(needsDisplay))]
        fn needs_display(&self) -> bool {
            false
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {}

        #[unsafe(method(convertPoint:fromView:))]
        fn convert_point_from_view(&self, point: NSPoint, view: Option<&NSView>) -> NSPoint {
            let (x, y) = from_window(self).point_xy(to_window_of(view).point(point.x, point.y));
            NSPoint::new(x, y)
        }

        #[unsafe(method(convertPoint:toView:))]
        fn convert_point_to_view(&self, point: NSPoint, view: Option<&NSView>) -> NSPoint {
            let window = to_window(self).point(point.x, point.y);
            let (x, y) = match view {
                Some(v) => from_window(imp(v)).point_xy(window),
                None => window,
            };
            NSPoint::new(x, y)
        }

        #[unsafe(method(convertRect:fromView:))]
        fn convert_rect_from_view(&self, rect: NSRect, view: Option<&NSView>) -> NSRect {
            map_rect(&to_window_of(view).then(&from_window(self)), rect)
        }

        #[unsafe(method(convertRect:toView:))]
        fn convert_rect_to_view(&self, rect: NSRect, view: Option<&NSView>) -> NSRect {
            let to = view.map_or(Xf::IDENTITY, |v| from_window(imp(v)));
            map_rect(&to_window(self).then(&to), rect)
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            hit_test(self, point)
        }
    }

    unsafe impl NSObjectProtocol for NSViewImpl {}
);

/// Any view as the implementation class, which every view inherits from.
pub(crate) fn imp(view: &NSView) -> &NSViewImpl {
    // SAFETY: NSView is NSViewImpl's class; subclasses share its layout.
    unsafe { &*(view as *const NSView).cast::<NSViewImpl>() }
}

pub(crate) fn as_view(view: &NSViewImpl) -> &NSView {
    // SAFETY: as in `imp`.
    unsafe { &*(view as *const NSViewImpl).cast::<NSView>() }
}

fn superview(view: &NSViewImpl) -> Option<&NSViewImpl> {
    // SAFETY: a superview owns its subviews, so it outlives the link.
    view.ivars().superview.get().map(|p| imp(unsafe { p.as_ref() }))
}

pub(crate) fn window_of(view: &NSViewImpl) -> Option<&NSWindowImpl> {
    // SAFETY: a window detaches its views before it goes away.
    view.ivars().window.get().map(|p| window::imp(unsafe { p.as_ref() }))
}

pub(crate) fn subviews(view: &NSViewImpl) -> Vec<Retained<NSView>> {
    view.ivars().subviews.borrow().clone()
}

pub(crate) fn is_clip(view: &NSViewImpl) -> bool {
    view.ivars().is_clip.get()
}

pub(crate) fn is_hidden(view: &NSViewImpl) -> bool {
    view.ivars().hidden.get()
}

pub(crate) fn frame(view: &NSViewImpl) -> NSRect {
    view.ivars().frame.get()
}

pub(crate) fn bounds(view: &NSViewImpl) -> NSRect {
    NSRect::new(view.ivars().bounds_origin.get(), view.ivars().frame.get().size)
}

pub(crate) fn is_flipped(view: &NSViewImpl) -> bool {
    // SAFETY: isFlipped takes nothing and returns BOOL.
    unsafe { msg_send![view, isFlipped] }
}

fn contains(r: NSRect, (x, y): (f64, f64)) -> bool {
    x >= r.origin.x && y >= r.origin.y && x < r.origin.x + r.size.width && y < r.origin.y + r.size.height
}

fn map_rect(xf: &Xf, r: NSRect) -> NSRect {
    let (x0, y0) = xf.point(r.origin.x, r.origin.y);
    let (x1, y1) = xf.point(r.origin.x + r.size.width, r.origin.y + r.size.height);
    NSRect::new(NSPoint::new(x0.min(x1), y0.min(y1)), NSSize::new((x1 - x0).abs(), (y1 - y0).abs()))
}

impl Xf {
    fn point_xy(&self, (x, y): (f64, f64)) -> (f64, f64) {
        self.point(x, y)
    }
}

/// The map from `view`'s bounds to its superview's coordinates, the view
/// sitting at `frame` in a superview flipped or not.
pub(crate) fn step(view: &NSViewImpl, super_flipped: bool, frame: NSRect) -> Xf {
    let b = view.ivars().bounds_origin.get();
    let tx = frame.origin.x - b.x;
    if is_flipped(view) == super_flipped {
        Xf { tx, a: 1.0, ty: frame.origin.y - b.y }
    } else {
        Xf { tx, a: -1.0, ty: frame.origin.y + frame.size.height + b.y }
    }
}

/// A view's map to window coordinates (unflipped, origin at the bottom left
/// of the content area). Views outside a window map to their topmost
/// ancestor's frame.
pub(crate) fn to_window(view: &NSViewImpl) -> Xf {
    let mut xf = Xf::IDENTITY;
    let mut cur = view;
    loop {
        match superview(cur) {
            Some(sup) => {
                xf = xf.then(&step(cur, is_flipped(sup), frame(cur)));
                cur = sup;
            }
            None => return xf.then(&step(cur, false, frame(cur))),
        }
    }
}

fn to_window_of(view: Option<&NSView>) -> Xf {
    view.map_or(Xf::IDENTITY, |v| to_window(imp(v)))
}

fn from_window(view: &NSViewImpl) -> Xf {
    to_window(view).inverse()
}

/// Where a view draws: its layer, the map from its bounds to the layer's
/// pixels (top-left origin), and the part of the layer its ancestors let it
/// draw in.
pub(crate) struct Placement {
    pub layer: LayerId,
    pub xf: Xf,
    pub clip: Rect,
}

/// A clip view's layer, named by the clip view.
pub(crate) fn layer_id(clip: &NSViewImpl) -> LayerId {
    clip as *const NSViewImpl as LayerId
}

/// The map from the root view of a layer to the layer: the window's content
/// view to the window surface, or a document to its clip view's layer (whose
/// top is the document's top).
pub(crate) fn root_xf(root: &NSViewImpl, layer: LayerId, content_height: f64) -> Xf {
    if layer == ROOT_LAYER {
        step(root, false, frame(root)).then(&Xf { tx: 0.0, a: -1.0, ty: content_height })
    } else {
        let size = frame(root).size;
        step(root, false, NSRect::new(NSPoint::ZERO, size)).then(&Xf { tx: 0.0, a: -1.0, ty: size.height })
    }
}

pub(crate) fn placement(view: &NSViewImpl) -> Option<Placement> {
    let window = window_of(view)?;
    // From the view up to the root of its layer.
    let mut chain = vec![view];
    let mut layer = ROOT_LAYER;
    loop {
        let cur = *chain.last().expect("chain");
        if is_hidden(cur) {
            return None;
        }
        match superview(cur) {
            Some(sup) if is_clip(sup) => {
                layer = layer_id(sup);
                break;
            }
            Some(sup) => chain.push(sup),
            None => break,
        }
    }
    let root = *chain.last().expect("chain");
    if layer == ROOT_LAYER && !window.is_content_view(root) {
        return None;
    }
    let mut xf = root_xf(root, layer, window.content_height());
    let mut clip = xf.rect(bounds(root));
    for pair in chain.windows(2).rev() {
        let (child, parent) = (pair[0], pair[1]);
        xf = step(child, is_flipped(parent), frame(child)).then(&xf);
        clip = clip.intersect(&xf.rect(bounds(child)));
    }
    Some(Placement { layer, xf, clip })
}

/// Mark part of a view (in its coordinates) for redrawing.
pub(crate) fn invalidate(view: &NSViewImpl, rect: NSRect) {
    let Some(window) = window_of(view) else { return };
    let Some(p) = placement(view) else { return };
    let r = p.xf.rect(rect).intersect(&p.clip).round_out();
    if !r.is_empty() {
        window.invalidate(p.layer, r);
    }
}

fn visible_rect(view: &NSViewImpl) -> NSRect {
    match placement(view) {
        Some(p) if !p.clip.is_empty() => p.xf.inverse_rect(p.clip),
        _ => NSRect::ZERO,
    }
}

fn change_frame(view: &NSViewImpl, new: NSRect) {
    let old = frame(view);
    if old == new {
        return;
    }
    if let Some(sup) = superview(view) {
        invalidate(sup, old);
    }
    view.ivars().frame.set(new);
    if old.size != new.size {
        // SAFETY: resizeSubviewsWithOldSize: takes an NSSize.
        unsafe { msg_send![view, resizeSubviewsWithOldSize: old.size] }
    }
    if let Some(window) = window_of(view) {
        if is_clip(view) || superview(view).is_some_and(is_clip) {
            window.layers_moved();
        }
    }
    invalidate(view, bounds(view));
}

fn change_bounds_origin(view: &NSViewImpl, origin: NSPoint) {
    if view.ivars().bounds_origin.get() == origin {
        return;
    }
    view.ivars().bounds_origin.set(origin);
    match window_of(view) {
        // Scrolling a clip view moves its layer; nothing is redrawn.
        Some(window) if is_clip(view) => window.layers_moved(),
        _ => invalidate(view, bounds(view)),
    }
}

/// Share a change in the superview's size among a view's flexible margins
/// and size, in proportion to their current extents.
fn autoresize(view: &NSViewImpl, old: NSSize) {
    let Some(sup) = superview(view) else { return };
    let mask = view.ivars().autoresizing.get();
    let new = frame(sup).size;
    let f = frame(view);
    let has = |m: NSAutoresizingMaskOptions| mask.contains(m);
    let (x, w) = share(
        f.origin.x,
        f.size.width,
        old.width - f.origin.x - f.size.width,
        new.width - old.width,
        [
            has(NSAutoresizingMaskOptions::ViewMinXMargin),
            has(NSAutoresizingMaskOptions::ViewWidthSizable),
            has(NSAutoresizingMaskOptions::ViewMaxXMargin),
        ],
    );
    let (y, h) = share(
        f.origin.y,
        f.size.height,
        old.height - f.origin.y - f.size.height,
        new.height - old.height,
        [
            has(NSAutoresizingMaskOptions::ViewMinYMargin),
            has(NSAutoresizingMaskOptions::ViewHeightSizable),
            has(NSAutoresizingMaskOptions::ViewMaxYMargin),
        ],
    );
    let rect = NSRect::new(NSPoint::new(x, y), NSSize::new(w.max(0.0), h.max(0.0)));
    as_view(view).setFrame(rect);
}

fn share(pos: f64, size: f64, max_margin: f64, delta: f64, flexible: [bool; 3]) -> (f64, f64) {
    let extents = [pos, size, max_margin];
    let count = flexible.iter().filter(|f| **f).count();
    if count == 0 || delta == 0.0 {
        return (pos, size);
    }
    let total: f64 = (0..3).filter(|&i| flexible[i]).map(|i| extents[i].max(0.0)).sum();
    let part = |i: usize| {
        if !flexible[i] {
            0.0
        } else if total > 0.0 {
            delta * extents[i].max(0.0) / total
        } else {
            delta / count as f64
        }
    };
    (pos + part(0), size + part(1))
}

fn add_subview(this: &NSViewImpl, view: &NSView) {
    let v = imp(view);
    if superview(v).is_some() {
        remove_from_superview(v);
    }
    v.ivars().superview.set(Some(NonNull::from(as_view(this))));
    // SAFETY: the superview owns the view, so outlives the link.
    unsafe { view.setNextResponder(Some(this)) };
    this.ivars().subviews.borrow_mut().push(view.retain());
    set_window(v, this.ivars().window.get());
    invalidate(v, bounds(v));
}

fn remove_from_superview(view: &NSViewImpl) {
    let Some(sup) = superview(view) else { return };
    // Keep the view alive until it is fully detached.
    let this = as_view(view).retain();
    invalidate(sup, frame(view));
    sup.ivars().subviews.borrow_mut().retain(|v| !std::ptr::eq(&**v, &*this));
    view.ivars().superview.set(None);
    // SAFETY: clearing the link.
    unsafe { this.setNextResponder(None) };
    set_window(view, None);
}

/// Move a view and its subviews into a window, or out of one.
pub(crate) fn set_window(view: &NSViewImpl, window: Option<NonNull<NSWindow>>) {
    if view.ivars().window.get() == window {
        return;
    }
    if is_clip(view) {
        if let Some(old) = window_of(view) {
            old.remove_clip(view);
        }
    }
    view.ivars().window.set(window);
    if is_clip(view) {
        if let Some(new) = window_of(view) {
            new.add_clip(as_view(view));
        }
    }
    for sub in subviews(view) {
        set_window(imp(&sub), window);
    }
}

fn hit_test(view: &NSViewImpl, point: NSPoint) -> Option<Retained<NSView>> {
    if is_hidden(view) {
        return None;
    }
    let super_flipped = superview(view).is_some_and(is_flipped);
    let local = step(view, super_flipped, frame(view)).inverse().point(point.x, point.y);
    if !contains(bounds(view), local) {
        return None;
    }
    let local = NSPoint::new(local.0, local.1);
    for sub in subviews(view).iter().rev() {
        if let Some(hit) = sub.hitTest(local) {
            return Some(hit);
        }
    }
    Some(as_view(view).retain())
}

// NSClipView

#[derive(Default)]
pub(crate) struct ClipIvars {
    document: RefCell<Option<Retained<NSView>>>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSClipView"]
    #[ivars = ClipIvars]
    pub(crate) struct NSClipViewImpl;

    impl NSClipViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ClipIvars::default());
            // SAFETY: NSView's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            imp(&this).ivars().is_clip.set(true);
            this
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().document.borrow().as_ref().is_some_and(|d| is_flipped(imp(d)))
        }

        #[unsafe(method_id(documentView))]
        fn document_view(&self) -> Option<Retained<NSView>> {
            self.ivars().document.borrow().clone()
        }

        #[unsafe(method(setDocumentView:))]
        fn set_document_view(&self, document: Option<&NSView>) {
            set_document(self, document);
        }

        #[unsafe(method(scrollToPoint:))]
        fn scroll_to_point(&self, point: NSPoint) {
            self.setBoundsOrigin(point);
        }

        #[unsafe(method(constrainBoundsRect:))]
        fn constrain_bounds_rect(&self, proposed: NSRect) -> NSRect {
            constrain(self, proposed)
        }

        #[unsafe(method(documentVisibleRect))]
        fn document_visible_rect(&self) -> NSRect {
            document_visible_rect(self)
        }
    }

    unsafe impl NSObjectProtocol for NSClipViewImpl {}
);

fn set_document(clip: &NSClipViewImpl, document: Option<&NSView>) {
    let old = clip.ivars().document.replace(document.map(|d| d.retain()));
    if let Some(old) = old {
        old.removeFromSuperview();
    }
    if let Some(document) = document {
        clip.addSubview(document);
    }
    clip.setBoundsOrigin(NSPoint::ZERO);
    if let Some(window) = window_of(imp(clip)) {
        window.layers_moved();
    }
}

/// Keep proposed clip view bounds within the document.
fn constrain(clip: &NSClipViewImpl, proposed: NSRect) -> NSRect {
    let Some(document) = clip.ivars().document.borrow().clone() else { return proposed };
    let doc = frame(imp(&document));
    let (o, size) = (proposed.origin, proposed.size);
    let x = o.x.min(doc.origin.x + doc.size.width - size.width).max(doc.origin.x);
    let y = o.y.min(doc.origin.y + doc.size.height - size.height).max(doc.origin.y);
    NSRect::new(NSPoint::new(x, y), size)
}

fn document_visible_rect(clip: &NSClipViewImpl) -> NSRect {
    let Some(document) = clip.ivars().document.borrow().clone() else { return NSRect::ZERO };
    let d = imp(&document);
    let to_doc = step(d, is_flipped(imp(clip)), frame(d)).inverse();
    map_rect(&to_doc, bounds(imp(clip)))
}

// NSScrollView

#[derive(Default)]
pub(crate) struct ScrollIvars {
    clip: RefCell<Option<Retained<NSClipView>>>,
    vertical: Cell<bool>,
    horizontal: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSScrollView"]
    #[ivars = ScrollIvars]
    pub(crate) struct NSScrollViewImpl;

    impl NSScrollViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ScrollIvars::default());
            // SAFETY: NSView's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            let bounds = NSRect::new(NSPoint::ZERO, frame.size);
            // SAFETY: NSClipView's designated initializer.
            let clip: Retained<NSClipView> = unsafe { msg_send![NSClipView::alloc(this.mtm()), initWithFrame: bounds] };
            clip.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            this.addSubview(&clip);
            this.ivars().clip.replace(Some(clip));
            this
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method_id(contentView))]
        fn content_view(&self) -> Retained<NSClipView> {
            self.clip()
        }

        #[unsafe(method_id(documentView))]
        fn document_view(&self) -> Option<Retained<NSView>> {
            self.clip().documentView()
        }

        #[unsafe(method(setDocumentView:))]
        fn set_document_view(&self, document: Option<&NSView>) {
            self.clip().setDocumentView(document);
        }

        #[unsafe(method(documentVisibleRect))]
        fn document_visible_rect(&self) -> NSRect {
            self.clip().documentVisibleRect()
        }

        #[unsafe(method(contentSize))]
        fn content_size(&self) -> NSSize {
            self.clip().frame().size
        }

        #[unsafe(method(hasVerticalScroller))]
        fn has_vertical_scroller(&self) -> bool {
            self.ivars().vertical.get()
        }

        #[unsafe(method(setHasVerticalScroller:))]
        fn set_has_vertical_scroller(&self, flag: bool) {
            self.ivars().vertical.set(flag);
        }

        #[unsafe(method(hasHorizontalScroller))]
        fn has_horizontal_scroller(&self) -> bool {
            self.ivars().horizontal.get()
        }

        #[unsafe(method(setHasHorizontalScroller:))]
        fn set_has_horizontal_scroller(&self, flag: bool) {
            self.ivars().horizontal.set(flag);
        }

        #[unsafe(method(reflectScrolledClipView:))]
        fn reflect_scrolled_clip_view(&self, _clip: &NSClipView) {}

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            scroll_by_wheel(&self.clip(), event);
        }
    }

    unsafe impl NSObjectProtocol for NSScrollViewImpl {}
);

impl NSScrollViewImpl {
    fn clip(&self) -> Retained<NSClipView> {
        self.ivars().clip.borrow().clone().expect("NSScrollView without a clip view")
    }
}

fn scroll_by_wheel(clip: &NSClipView, event: &NSEvent) {
    let dy = event.scrollingDeltaY();
    let bounds = clip.bounds();
    // Positive deltas scroll toward the top of the document.
    let y = if clip.isFlipped() { bounds.origin.y - dy } else { bounds.origin.y + dy };
    let target = clip.constrainBoundsRect(NSRect::new(NSPoint::new(bounds.origin.x, y), bounds.size));
    clip.scrollToPoint(target.origin);
}
