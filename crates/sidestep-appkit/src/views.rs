//! The view hierarchy: `NSView`, `NSClipView` and `NSScrollView` (the
//! responder chain they sit in is `responder`).
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
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSClipView, NSCursor, NSEvent, NSResponder, NSTextInputContext, NSTrackingArea, NSView,
    NSWindow,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::graphics::Xf;
use crate::protocol::{LayerId, ROOT_LAYER, Rect};
use crate::window::{self, NSWindowImpl};

/// A reference that doesn't retain, as AppKit's back pointers are.
type Unretained<T> = Cell<Option<NonNull<T>>>;

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
    tracking: RefCell<crate::tracking::ViewTracking>,
    /// Made when first asked for, for views that are text input clients.
    input_context: RefCell<Option<Retained<NSTextInputContext>>>,
    /// Frame and bounds notification state (see `changed`).
    notes: Cell<u8>,
    /// The view's place in its window's key view loop (see `keyloop`).
    key_links: crate::keyloop::KeyLinks,
    /// `toolTip` (see `tooltip`).
    tool_tip: RefCell<Option<Retained<objc2_foundation::NSString>>>,
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
            tracking: RefCell::default(),
            input_context: RefCell::new(None),
            notes: Cell::new(0),
            key_links: Default::default(),
            tool_tip: RefCell::new(None),
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
            moved(self);
        }

        #[unsafe(method(isHiddenOrHasHiddenAncestor))]
        fn is_hidden_or_has_hidden_ancestor(&self) -> bool {
            is_hidden_or_has_hidden_ancestor(self)
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

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            // Depth first, subviews in order, until one performs it.
            subviews(self).iter().any(|sub| !is_hidden(imp(sub)) && sub.performKeyEquivalent(event))
        }

        // Tracking areas and cursor rectangles (see `tracking`).

        #[unsafe(method(addTrackingArea:))]
        fn add_tracking_area(&self, area: &NSTrackingArea) {
            crate::tracking::add_area(self, area);
        }

        #[unsafe(method(removeTrackingArea:))]
        fn remove_tracking_area(&self, area: &NSTrackingArea) {
            crate::tracking::remove_area(self, area);
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {}

        #[unsafe(method_id(trackingAreas))]
        fn tracking_areas(&self) -> Retained<AnyObject> {
            crate::tracking::areas_array(self)
        }

        #[unsafe(method(addTrackingRect:owner:userData:assumeInside:))]
        fn add_tracking_rect(&self, rect: NSRect, owner: &AnyObject, data: *mut c_void, inside: bool) -> isize {
            crate::tracking::add_tracking_rect(self, rect, owner, data, inside)
        }

        #[unsafe(method(removeTrackingRect:))]
        fn remove_tracking_rect(&self, tag: isize) {
            crate::tracking::remove_tracking_rect(self, tag);
        }

        #[unsafe(method(addCursorRect:cursor:))]
        fn add_cursor_rect(&self, rect: NSRect, cursor: &NSCursor) {
            crate::tracking::add_cursor_rect(self, rect, cursor);
        }

        #[unsafe(method(removeCursorRect:cursor:))]
        fn remove_cursor_rect(&self, rect: NSRect, cursor: &NSCursor) {
            crate::tracking::remove_cursor_rect(self, rect, cursor);
        }

        #[unsafe(method(discardCursorRects))]
        fn discard_cursor_rects(&self) {
            crate::tracking::discard_cursor_rects(self);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {}

        #[unsafe(method_id(inputContext))]
        fn input_context(&self) -> Option<Retained<NSTextInputContext>> {
            crate::inputcontext::for_view(self)
        }

        // The key view loop (see `keyloop`).

        #[unsafe(method_id(nextKeyView))]
        fn next_key_view(&self) -> Option<Retained<NSView>> {
            crate::keyloop::next(self)
        }

        #[unsafe(method(setNextKeyView:))]
        fn set_next_key_view(&self, next: Option<&NSView>) {
            crate::keyloop::set_next(self, next);
        }

        #[unsafe(method_id(previousKeyView))]
        fn previous_key_view(&self) -> Option<Retained<NSView>> {
            crate::keyloop::previous(self)
        }

        #[unsafe(method_id(nextValidKeyView))]
        fn next_valid_key_view(&self) -> Option<Retained<NSView>> {
            crate::keyloop::next_valid(self)
        }

        #[unsafe(method_id(previousValidKeyView))]
        fn previous_valid_key_view(&self) -> Option<Retained<NSView>> {
            crate::keyloop::previous_valid(self)
        }

        // Tooltips (see `tooltip`).

        #[unsafe(method_id(toolTip))]
        fn tool_tip(&self) -> Option<Retained<objc2_foundation::NSString>> {
            self.ivars().tool_tip.borrow().clone()
        }

        #[unsafe(method(setToolTip:))]
        fn set_tool_tip(&self, text: Option<&objc2_foundation::NSString>) {
            crate::tooltip::set_tool_tip(as_view(self), text);
        }

        #[unsafe(method(addToolTipRect:owner:userData:))]
        fn add_tool_tip_rect(&self, rect: NSRect, owner: &AnyObject, data: *mut c_void) -> isize {
            crate::tooltip::add_tool_tip_rect(as_view(self), rect, owner, data)
        }

        #[unsafe(method(removeToolTip:))]
        fn remove_tool_tip(&self, tag: isize) {
            crate::tooltip::remove_tool_tip(as_view(self), tag);
        }

        #[unsafe(method(removeAllToolTips))]
        fn remove_all_tool_tips(&self) {
            crate::tooltip::remove_all(as_view(self));
        }

        /// Views that take typing (text views and fields) say yes.
        #[unsafe(method(needsPanelToBecomeKey))]
        fn needs_panel_to_become_key(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeKeyView))]
        fn can_become_key_view(&self) -> bool {
            crate::keyloop::can_become_key_view(self)
        }

        /// A click in a view that isn't opaque can move a window that moves
        /// by its background, as on macOS.
        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            // SAFETY: isOpaque takes nothing and returns BOOL.
            let opaque: bool = unsafe { msg_send![self, isOpaque] };
            !opaque
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            false
        }

        #[unsafe(method(postsFrameChangedNotifications))]
        fn posts_frame_changed_notifications(&self) -> bool {
            self.ivars().notes.get() & Change::Frame.off() == 0
        }

        #[unsafe(method(setPostsFrameChangedNotifications:))]
        fn set_posts_frame_changed_notifications(&self, flag: bool) {
            set_posts(self, Change::Frame, flag);
        }

        #[unsafe(method(postsBoundsChangedNotifications))]
        fn posts_bounds_changed_notifications(&self) -> bool {
            self.ivars().notes.get() & Change::Bounds.off() == 0
        }

        #[unsafe(method(setPostsBoundsChangedNotifications:))]
        fn set_posts_bounds_changed_notifications(&self, flag: bool) {
            set_posts(self, Change::Bounds, flag);
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

/// The `i`th subview, if there is one.
pub(crate) fn subview_at(view: &NSViewImpl, i: usize) -> Option<Retained<NSView>> {
    view.ivars().subviews.borrow().get(i).cloned()
}

pub(crate) fn superview_of(view: &NSViewImpl) -> Option<&NSViewImpl> {
    superview(view)
}

pub(crate) fn is_clip(view: &NSViewImpl) -> bool {
    view.ivars().is_clip.get()
}

pub(crate) fn is_hidden(view: &NSViewImpl) -> bool {
    view.ivars().hidden.get()
}

pub(crate) fn is_hidden_or_has_hidden_ancestor(view: &NSViewImpl) -> bool {
    is_hidden(view) || superview(view).is_some_and(is_hidden_or_has_hidden_ancestor)
}

/// How many superviews a view has.
pub(crate) fn depth(view: &NSViewImpl) -> usize {
    superview(view).map_or(0, |s| 1 + depth(s))
}

pub(crate) fn tracking(view: &NSViewImpl) -> &RefCell<crate::tracking::ViewTracking> {
    &view.ivars().tracking
}

pub(crate) fn input_context(view: &NSViewImpl) -> &RefCell<Option<Retained<NSTextInputContext>>> {
    &view.ivars().input_context
}

pub(crate) fn key_links(view: &NSViewImpl) -> &crate::keyloop::KeyLinks {
    &view.ivars().key_links
}

pub(crate) fn tool_tip(view: &NSViewImpl) -> &RefCell<Option<Retained<objc2_foundation::NSString>>> {
    &view.ivars().tool_tip
}

/// The view moved in its window, or showed or hid: tracking areas and
/// cursor rectangles are due for an update.
fn moved(view: &NSViewImpl) {
    if let Some(window) = window_of(view) {
        crate::tracking::views_moved(window, view);
    }
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
    if let Some(window) = window_of(view)
        && (is_clip(view) || superview(view).is_some_and(is_clip))
    {
        window.layers_moved();
    }
    invalidate(view, bounds(view));
    moved(view);
    changed(view, Change::Frame);
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
    moved(view);
    changed(view, Change::Bounds);
}

// Frame and bounds notifications.
//
// A view posts NSViewFrameDidChangeNotification when its frame changes
// (after its subviews were resized, so theirs come first) and
// NSViewBoundsDidChangeNotification when its bounds origin does, a clip
// view's scrolling included; a new frame size alone changes no bounds
// notification, as on macOS. Posting is on by default and a program may
// turn it off; turning it back on posts once if anything changed meanwhile
// (conformance/tests/appkit_events.rs pins all of this). A change nobody
// observes costs a load or two: whether anyone observes each name is
// remembered until the notification center's registrations change.

#[derive(Clone, Copy)]
enum Change {
    Frame,
    Bounds,
}

impl Change {
    /// The view's flag: posting is off.
    fn off(self) -> u8 {
        match self {
            Change::Frame => 1,
            Change::Bounds => 2,
        }
    }

    /// The view's flag: it changed while posting was off.
    fn missed(self) -> u8 {
        self.off() << 2
    }

    fn name(self) -> &'static objc2_foundation::NSString {
        match self {
            Change::Frame => crate::notifications::name!(NSViewFrameDidChangeNotification),
            Change::Bounds => crate::notifications::name!(NSViewBoundsDidChangeNotification),
        }
    }
}

thread_local! {
    /// The notification center's generation when last asked, and whether
    /// each name was observed then.
    static OBSERVED: Cell<(u64, bool, bool)> = const { Cell::new((0, false, false)) };
}

fn observed(change: Change) -> bool {
    use sidestep_foundation::notification_center::{generation, has_observers};
    let now = generation();
    let (seen, mut frame, mut bounds) = OBSERVED.with(Cell::get);
    if seen != now {
        (frame, bounds) = (has_observers(Change::Frame.name()), has_observers(Change::Bounds.name()));
        OBSERVED.with(|o| o.set((now, frame, bounds)));
    }
    match change {
        Change::Frame => frame,
        Change::Bounds => bounds,
    }
}

fn changed(view: &NSViewImpl, change: Change) {
    let notes = view.ivars().notes.get();
    if notes & change.off() != 0 {
        view.ivars().notes.set(notes | change.missed());
    } else if observed(change) {
        sidestep_foundation::notification_center::post(change.name(), Some(view), None);
    }
}

fn set_posts(view: &NSViewImpl, change: Change, on: bool) {
    let notes = view.ivars().notes.get();
    if !on {
        view.ivars().notes.set(notes | change.off());
        return;
    }
    view.ivars().notes.set(notes & !(change.off() | change.missed()));
    if notes & change.missed() != 0 && observed(change) {
        sidestep_foundation::notification_center::post(change.name(), Some(view), None);
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
    if let Some(old) = window_of(view) {
        old.view_left(view);
    }
    // A content view leaving its window (or the window going away) keeps
    // no link to it as its next responder.
    if let Some(old) = view.ivars().window.get() {
        crate::responder::unlink_next(as_view(view), old.cast::<NSResponder>());
    }
    if is_clip(view)
        && let Some(old) = window_of(view)
    {
        old.remove_clip(view);
    }
    view.ivars().window.set(window);
    if is_clip(view)
        && let Some(new) = window_of(view)
    {
        new.add_clip(as_view(view));
    }
    if let Some(new) = window_of(view) {
        crate::tracking::view_joined(new, view);
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

/// A line, for wheels, which scroll by lines.
const LINE_SCROLL: f64 = 16.0;

fn scroll_by_wheel(clip: &NSClipView, event: &NSEvent) {
    let (mut dx, mut dy) = (event.scrollingDeltaX(), event.scrollingDeltaY());
    if !event.hasPreciseScrollingDeltas() {
        (dx, dy) = (dx * LINE_SCROLL, dy * LINE_SCROLL);
    }
    let bounds = clip.bounds();
    // Positive deltas scroll toward the top and the left of the document.
    let y = if clip.isFlipped() { bounds.origin.y - dy } else { bounds.origin.y + dy };
    let x = bounds.origin.x - dx;
    let target = clip.constrainBoundsRect(NSRect::new(NSPoint::new(x, y), bounds.size));
    clip.scrollToPoint(target.origin);
}
