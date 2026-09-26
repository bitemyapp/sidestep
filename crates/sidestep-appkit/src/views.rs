//! The view hierarchy: `NSView` (the responder chain it sits in is
//! `responder`; clip and scroll views are `scroll`).
//!
//! Views keep their geometry in ivars. Everything that subclasses may
//! override (`isFlipped`, `drawRect:`, `hitTest:`, the event methods) is
//! reached by message; the rest is plain Rust.
//!
//! A window has layers: its own surface, and one per clip view that
//! `layers` promotes, holding that clip view's document. A view's
//! placement is where its bounds land in its layer, found by composing
//! each view's map to its superview: `x' = x + tx` and `y' = a·y + ty`,
//! with `a = -1` where a view and its superview disagree about flipping.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSCursor, NSEvent, NSResponder, NSTextInputContext, NSTrackingArea, NSView, NSWindow,
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
    /// Copy-on-write, so snapshots (`subviews`, the display pass) cost a
    /// reference count rather than a copy.
    subviews: RefCell<Rc<Vec<Retained<NSView>>>>,
    window: Unretained<NSWindow>,
    autoresizing: Cell<NSAutoresizingMaskOptions>,
    hidden: Cell<bool>,
    /// An NSClipView: its document is drawn into a layer of its own.
    is_clip: Cell<bool>,
    tracking: RefCell<crate::tracking::ViewTracking>,
    /// Made when first asked for, for views that are text input clients.
    input_context: RefCell<Option<Retained<NSTextInputContext>>>,
    /// Drawing: the view's appearance, and its opacity.
    appearance: crate::appearance::ViewAppearance,
    alpha: Cell<f64>,
    /// Frame and bounds notification state (see `changed`).
    notes: Cell<u8>,
    /// The view's place in its window's key view loop (see `keyloop`).
    key_links: crate::keyloop::KeyLinks,
    /// `toolTip` and the view's tooltip areas (see `tooltip`).
    tool_tips: crate::tooltip::ViewTips,
    /// `focusRingType` (see `controls::focus`).
    focus_ring: Cell<objc2_app_kit::NSFocusRingType>,
    /// The view's accessibility record (see `controls::a11y`).
    a11y: crate::controls::a11y::Node,
    /// The layout pass's flags and the rest of the view contract's state
    /// (see `view_layout`).
    pub(crate) state: crate::view_layout::ViewState,
}

impl ViewIvars {
    fn new(frame: NSRect) -> Self {
        ViewIvars {
            frame: Cell::new(frame),
            bounds_origin: Cell::new(NSPoint::ZERO),
            superview: Cell::new(None),
            subviews: RefCell::default(),
            window: Cell::new(None),
            autoresizing: Cell::new(NSAutoresizingMaskOptions::ViewNotSizable),
            hidden: Cell::new(false),
            is_clip: Cell::new(false),
            tracking: RefCell::default(),
            input_context: RefCell::new(None),
            appearance: Default::default(),
            alpha: Cell::new(1.0),
            notes: Cell::new(0),
            key_links: Default::default(),
            tool_tips: Default::default(),
            focus_ring: Cell::new(objc2_app_kit::NSFocusRingType::Default),
            a11y: Default::default(),
            state: crate::view_layout::ViewState::default(),
        }
    }
}

impl Drop for ViewIvars {
    fn drop(&mut self) {
        // As when a view goes on macOS: subviews that outlive it no longer
        // point at it, as their superview or (their controller's) next
        // responder. Only their links are read, never the view going away.
        for sub in self.subviews.get_mut().iter() {
            if let Some(gone) = imp(sub).ivars().superview.take() {
                crate::responder::unlink_next(sub, gone.cast());
            }
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
            crate::view_layout::bounds(self)
        }

        #[unsafe(method(setBoundsOrigin:))]
        fn set_bounds_origin(&self, origin: NSPoint) {
            change_bounds_origin(self, origin);
        }

        #[unsafe(method(visibleRect))]
        fn visible_rect(&self) -> NSRect {
            crate::view_layout::visible_rect(self)
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
            if self.ivars().hidden.get() == hidden {
                return;
            }
            invalidate(self, bounds(self));
            self.ivars().hidden.set(hidden);
            invalidate(self, bounds(self));
            moved(self);
            crate::view_layout::hidden_changed(self, hidden);
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
            crate::view_layout::add_subview(self, view, crate::view_layout::Place::Top);
        }

        #[unsafe(method(removeFromSuperview))]
        fn remove_from_superview(&self) {
            crate::view_layout::remove_from_superview(self, true);
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
            for view in subviews(self) {
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

        // Drawing: appearance, snapshots, opacity (see `crate::context`).

        #[unsafe(method_id(appearance))]
        fn appearance(&self) -> Option<Retained<objc2_app_kit::NSAppearance>> {
            self.ivars().appearance.own.get().map(crate::appearance::get)
        }

        #[unsafe(method(setAppearance:))]
        fn set_appearance(&self, appearance: Option<&objc2_app_kit::NSAppearance>) {
            self.ivars().appearance.own.set(appearance.map(crate::appearance::id_of));
            crate::appearance::refresh(as_view(self));
        }

        #[unsafe(method_id(effectiveAppearance))]
        fn effective_appearance(&self) -> Retained<objc2_app_kit::NSAppearance> {
            crate::appearance::get(crate::appearance::effective(self))
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn view_did_change_effective_appearance(&self) {}

        #[unsafe(method(cacheDisplayInRect:toBitmapImageRep:))]
        fn cache_display_in_rect(&self, rect: NSRect, rep: &objc2_app_kit::NSBitmapImageRep) {
            crate::context::cache_display(self, rect, rep);
        }

        #[unsafe(method_id(bitmapImageRepForCachingDisplayInRect:))]
        fn bitmap_image_rep_for_caching(&self, rect: NSRect) -> Option<Retained<objc2_app_kit::NSBitmapImageRep>> {
            crate::context::bitmap_for(self, rect)
        }

        #[unsafe(method(displayRectIgnoringOpacity:inContext:))]
        fn display_rect_ignoring_opacity(&self, rect: NSRect, context: &objc2_app_kit::NSGraphicsContext) {
            crate::context::display_in(self, rect, context);
        }

        #[unsafe(method(needsToDrawRect:))]
        fn needs_to_draw_rect(&self, rect: NSRect) -> bool {
            let d = crate::context::dirty();
            rect.origin.x < d.origin.x + d.size.width
                && d.origin.x < rect.origin.x + rect.size.width
                && rect.origin.y < d.origin.y + d.size.height
                && d.origin.y < rect.origin.y + rect.size.height
        }

        #[unsafe(method(getRectsBeingDrawn:count:))]
        fn get_rects_being_drawn(&self, rects: *mut *const NSRect, count: *mut isize) {
            // SAFETY: each pointer is null or writable; the rectangle lives
            // in a thread local that outlasts the drawRect: asking.
            unsafe {
                if !rects.is_null() {
                    *rects = crate::context::dirty_ptr();
                }
                if !count.is_null() {
                    *count = 1;
                }
            }
        }

        #[unsafe(method(alphaValue))]
        fn alpha_value(&self) -> f64 {
            self.ivars().alpha.get()
        }

        #[unsafe(method(setAlphaValue:))]
        fn set_alpha_value(&self, alpha: f64) {
            // Kept as given, as AppKit keeps it; drawing clamps it.
            if self.ivars().alpha.replace(alpha) != alpha {
                invalidate(self, bounds(self));
            }
        }

        #[unsafe(method_id(animator))]
        fn animator(&self) -> Retained<NSView> {
            // Changes apply at once (see `crate::animation`).
            as_view(self).retain()
        }

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
            self.ivars().tool_tips.text()
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

        // Sizing, which controls override (see `controls`).

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            NSSize::new(crate::controls::control::NO_METRIC, crate::controls::control::NO_METRIC)
        }

        #[unsafe(method(invalidateIntrinsicContentSize))]
        fn invalidate_intrinsic_content_size(&self) {
            // Auto Layout measures it again (`fittingSize` is its too).
            crate::autolayout::intrinsic_size_changed(self);
        }

        // Focus rings (see `controls::focus`).

        #[unsafe(method(focusRingType))]
        fn focus_ring_type(&self) -> objc2_app_kit::NSFocusRingType {
            self.ivars().focus_ring.get()
        }

        #[unsafe(method(setFocusRingType:))]
        fn set_focus_ring_type(&self, kind: objc2_app_kit::NSFocusRingType) {
            if self.ivars().focus_ring.replace(kind) != kind {
                crate::controls::focus::ring_changed(as_view(self));
            }
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {}

        #[unsafe(method(focusRingMaskBounds))]
        fn focus_ring_mask_bounds(&self) -> NSRect {
            NSRect::ZERO
        }

        #[unsafe(method(noteFocusRingMaskChanged))]
        fn note_focus_ring_mask_changed(&self) {
            crate::controls::focus::ring_changed(as_view(self));
        }

        #[unsafe(method(setKeyboardFocusRingNeedsDisplayInRect:))]
        fn set_keyboard_focus_ring_needs_display_in_rect(&self, _rect: NSRect) {
            crate::controls::focus::ring_changed(as_view(self));
        }
    }

    unsafe impl NSObjectProtocol for NSViewImpl {}
);

/// Any view as the implementation class, which every view inherits from.
pub(crate) fn imp(view: &NSView) -> &NSViewImpl {
    // SAFETY: NSView is NSViewImpl's class; subclasses share its layout.
    unsafe { &*(view as *const NSView).cast::<NSViewImpl>() }
}

pub(crate) fn a11y_node(view: &NSView) -> &crate::controls::a11y::Node {
    &imp(view).ivars().a11y
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

/// A view's subviews as they are now, bottom to top: a snapshot that later
/// changes to the view don't touch.
pub(crate) fn subviews(view: &NSViewImpl) -> Subviews {
    Subviews(view.ivars().subviews.borrow().clone())
}

/// The `i`th subview, if there is one.
pub(crate) fn subview_at(view: &NSViewImpl, i: usize) -> Option<Retained<NSView>> {
    view.ivars().subviews.borrow().get(i).cloned()
}

/// A snapshot of a view's subviews (see [`subviews`]). It derefs to a
/// slice, and iterating it by value yields each subview retained.
pub(crate) struct Subviews(Rc<Vec<Retained<NSView>>>);

impl std::ops::Deref for Subviews {
    type Target = [Retained<NSView>];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl IntoIterator for Subviews {
    type Item = Retained<NSView>;
    type IntoIter = SubviewsIter;

    fn into_iter(self) -> SubviewsIter {
        SubviewsIter(self.0, 0)
    }
}

pub(crate) struct SubviewsIter(Rc<Vec<Retained<NSView>>>, usize);

impl Iterator for SubviewsIter {
    type Item = Retained<NSView>;

    fn next(&mut self) -> Option<Retained<NSView>> {
        let view = self.0.get(self.1)?.clone();
        self.1 += 1;
        Some(view)
    }
}

/// Where `view` is among `this`'s subviews.
pub(crate) fn index_of(this: &NSViewImpl, view: &NSViewImpl) -> Option<usize> {
    this.ivars().subviews.borrow().iter().position(|v| std::ptr::eq(imp(v), view))
}

/// Put `view`, which has no superview, among `this`'s subviews at `index`
/// (on top if it's past the end). Only the links: callbacks, the window
/// and drawing the view are `view_layout`'s business.
pub(crate) fn link(this: &NSViewImpl, view: &NSView, index: usize) {
    let v = imp(view);
    debug_assert!(superview(v).is_none());
    v.ivars().superview.set(Some(NonNull::from(as_view(this))));
    // SAFETY: the superview owns the view, so outlives the link.
    unsafe { view.setNextResponder(Some(this)) };
    let mut subviews = this.ivars().subviews.borrow_mut();
    let list = Rc::make_mut(&mut subviews);
    list.insert(index.min(list.len()), view.retain());
}

/// Take `view` out of its superview's subviews, redrawing where it was if
/// `display`. Only the links, as for [`link`]; the caller keeps the view
/// alive.
pub(crate) fn unlink(view: &NSViewImpl, display: bool) {
    let Some(sup) = superview(view) else { return };
    if display {
        invalidate(sup, frame(view));
    }
    // Released outside the borrow: releasing may run arbitrary code.
    let removed = {
        let mut subviews = sup.ivars().subviews.borrow_mut();
        let list = Rc::make_mut(&mut subviews);
        list.iter().position(|v| std::ptr::eq(imp(v), view)).map(|i| list.remove(i))
    };
    view.ivars().superview.set(None);
    // SAFETY: clearing the link.
    unsafe { as_view(view).setNextResponder(None) };
    drop(removed);
}

/// Give `this` the subviews it has, in a new order.
pub(crate) fn reorder(this: &NSViewImpl, order: Vec<Retained<NSView>>) {
    debug_assert_eq!(order.len(), this.ivars().subviews.borrow().len());
    let old = this.ivars().subviews.replace(Rc::new(order));
    drop(old);
    invalidate(this, bounds(this));
}

pub(crate) fn superview_of(view: &NSViewImpl) -> Option<&NSViewImpl> {
    superview(view)
}

pub(crate) fn is_clip(view: &NSViewImpl) -> bool {
    view.ivars().is_clip.get()
}

/// Mark a new view as an NSClipView (see `scroll`).
pub(crate) fn mark_clip(view: &NSViewImpl) {
    view.ivars().is_clip.set(true);
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

pub(crate) fn tool_tips(view: &NSViewImpl) -> &crate::tooltip::ViewTips {
    &view.ivars().tool_tips
}

/// The view moved in its window, or showed or hid: tracking areas and
/// cursor rectangles are due for an update, and scroll layers are placed
/// again.
fn moved(view: &NSViewImpl) {
    if let Some(window) = window_of(view) {
        crate::tracking::views_moved(window, view);
        window.layers_moved();
    }
}

pub(crate) fn frame(view: &NSViewImpl) -> NSRect {
    view.ivars().frame.get()
}

pub(crate) fn bounds(view: &NSViewImpl) -> NSRect {
    NSRect::new(view.ivars().bounds_origin.get(), view.ivars().frame.get().size)
}

pub(crate) fn appearance_slot(view: &NSViewImpl) -> &crate::appearance::ViewAppearance {
    &view.ivars().appearance
}

pub(crate) fn alpha_of(view: &NSViewImpl) -> f64 {
    view.ivars().alpha.get()
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
    /// The view is drawn in a scroll layer's overlay (it or an ancestor in
    /// its layer is one of the overlay's views), not in `layer` itself.
    pub overlay: bool,
}

/// A clip view's layer, named by the clip view.
pub(crate) fn layer_id(clip: &NSViewImpl) -> LayerId {
    clip as *const NSViewImpl as LayerId
}

/// The map from a window's content view to the window's surface.
pub(crate) fn root_xf(root: &NSViewImpl, content_height: f64) -> Xf {
    step(root, false, frame(root)).then(&Xf { tx: 0.0, a: -1.0, ty: content_height })
}

pub(crate) fn placement(view: &NSViewImpl) -> Option<Placement> {
    let window = window_of(view)?;
    // From the view up to the root of its layer: the window's content view,
    // or a subview of a clip view with a layer of its own.
    let mut chain = vec![view];
    let mut layer = ROOT_LAYER;
    let mut overlay = false;
    loop {
        let cur = *chain.last().expect("chain");
        if is_hidden(cur) {
            return None;
        }
        overlay |= crate::view_layout::in_overlay(cur);
        match superview(cur) {
            Some(sup) if is_clip(sup) && crate::layers::promoted(sup) => {
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
    let mut xf = match superview(root) {
        Some(clip) if layer != ROOT_LAYER => {
            step(root, is_flipped(clip), frame(root)).then(&crate::layers::layer_xf(clip))
        }
        _ => root_xf(root, window.content_height()),
    };
    let mut clip = xf.rect(bounds(root));
    for pair in chain.windows(2).rev() {
        let (child, parent) = (pair[0], pair[1]);
        xf = step(child, is_flipped(parent), frame(child)).then(&xf);
        clip = clip.intersect(&xf.rect(bounds(child)));
    }
    Some(Placement { layer, xf, clip, overlay })
}

/// Mark part of a view (in its coordinates) for redrawing.
pub(crate) fn invalidate(view: &NSViewImpl, rect: NSRect) {
    let Some(window) = window_of(view) else { return };
    let Some(p) = placement(view) else { return };
    let r = p.xf.rect(rect).intersect(&p.clip).round_out();
    if !r.is_empty() {
        window.invalidate(crate::layers::damage_key(p.layer, p.overlay), r);
    }
}

fn change_frame(view: &NSViewImpl, new: NSRect) {
    let old = frame(view);
    if old == new {
        return;
    }
    // A document in a clip view with a layer that only grows or shrinks
    // keeps its pixels, as a layer-backed view whose layer contents redraw
    // on demand does (see `layers`): only what it gains is drawn, and its
    // clip view, drawn behind the layer, needn't be.
    let in_layer = superview(view).is_some_and(|s| is_clip(s) && crate::layers::promoted(s));
    let keeps = in_layer && old.origin == new.origin && crate::view_layout::keeps_content_on_resize(view);
    if let Some(sup) = superview(view)
        && !in_layer
    {
        invalidate(sup, old);
    }
    view.ivars().frame.set(new);
    // Bounds scaling, the layout flag, autoresizing and Auto Layout.
    crate::view_layout::frame_changed(view, old);
    // A clip view keeps its bounds over its document (see `scroll`).
    crate::scroll::frame_changed(view, old);
    if keeps {
        let b = bounds(view);
        let (w, h) = (old.size.width.min(b.size.width), old.size.height.min(b.size.height));
        let right = NSRect::new(NSPoint::new(b.origin.x + w, b.origin.y), NSSize::new(b.size.width - w, b.size.height));
        let rest = NSRect::new(NSPoint::new(b.origin.x, b.origin.y + h), NSSize::new(w, b.size.height - h));
        for gained in [right, rest] {
            if gained.size.width > 0.0 && gained.size.height > 0.0 {
                invalidate(view, gained);
            }
        }
    } else {
        invalidate(view, bounds(view));
    }
    moved(view);
    changed(view, Change::Frame);
}

fn change_bounds_origin(view: &NSViewImpl, origin: NSPoint) {
    if view.ivars().bounds_origin.get() == origin {
        return;
    }
    view.ivars().bounds_origin.set(origin);
    if is_clip(view) {
        crate::view_layout::clip_moved(view);
    }
    // Scrolling a clip view with a layer moves the layer (`moved` below);
    // nothing is redrawn.
    if !(is_clip(view) && crate::layers::promoted(view)) {
        invalidate(view, bounds(view));
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
    if notes & change.missed() != 0 {
        // A clip view follows its document as an observer would.
        if let Change::Frame = change {
            crate::scroll::frame_posted(view);
        }
        if observed(change) {
            sidestep_foundation::notification_center::post(change.name(), Some(view), None);
        }
    }
}

/// Whether a view posts NSViewFrameDidChangeNotification now.
pub(crate) fn posts_frame_changes(view: &NSViewImpl) -> bool {
    view.ivars().notes.get() & Change::Frame.off() == 0
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
    if w < 0.0 || h < 0.0 {
        as_view(view).setFrame(rect);
    } else {
        // Where Auto Layout's constraints for the mask put it too.
        crate::autolayout::autoresize(view, rect, || as_view(view).setFrame(rect));
    }
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

/// Move a view and its subviews into a window, or out of one, telling
/// each as AppKit does: `viewWillMoveToWindow:` before its subviews move,
/// `viewDidMoveToWindow` after. A view moving within its window hears both
/// too. The callbacks may move views: each view joins the window its place
/// puts it in once its own callback has run, and subviews taken elsewhere
/// are left to the move that took them.
pub(crate) fn set_window(view: &NSViewImpl, window: Option<NonNull<NSWindow>>) {
    let this = as_view(view).retain();
    // SAFETY: the caller holds the window.
    let to = window.map(|w| unsafe { w.as_ref() });
    this.viewWillMoveToWindow(to);
    let window = match superview(view) {
        Some(sup) => sup.ivars().window.get(),
        // SAFETY: as above.
        None => window.filter(|w| window::imp(unsafe { w.as_ref() }).is_content_view(view)),
    };
    relink_window(view, window);
    for sub in subviews(view) {
        let s = imp(&sub);
        if superview(s).is_some_and(|p| std::ptr::eq(p, view)) {
            set_window(s, view.ivars().window.get());
        }
    }
    this.viewDidMoveToWindow();
}

/// Take a view and its subviews out of a window that is going away,
/// telling none of them: the window can't be reached any more.
pub(crate) fn leave_dying_window(view: &NSViewImpl) {
    relink_window(view, None);
    for sub in subviews(view) {
        leave_dying_window(imp(&sub));
    }
}

/// Update one view's link to its window and what the window keeps about
/// it.
fn relink_window(view: &NSViewImpl, window: Option<NonNull<NSWindow>>) {
    if view.ivars().window.get() == window {
        return;
    }
    if let Some(old) = window_of(view) {
        old.view_left(view);
        // Its old window's overlays no longer draw it.
        crate::view_layout::set_in_overlay(view, false);
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
