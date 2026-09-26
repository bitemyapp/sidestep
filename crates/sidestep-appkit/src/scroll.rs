//! Scroll views: `NSClipView`, which shows part of its document and
//! scrolls it, and `NSScrollView`, which holds a clip view, its scrollers
//! (`scroller`) and a border. `layers` decides which clip views draw into
//! layers of their own; this module is AppKit's semantics, which
//! `conformance/tests/appkit_scroll.rs` pins against macOS.
//!
//! **Clip views.** `scrollToPoint:` moves the bounds origin as it is told,
//! unconstrained, and posts the bounds notification; `setBoundsOrigin:`
//! first constrains the origin (`constrainBoundsRect:`, which subclasses
//! may override), moves it, then sends the superview
//! `reflectScrolledClipView:` (NSView's does nothing). The document may be
//! scrolled past its edges by the content insets, and is at least as big
//! as the clip view less them (`documentRect`, empty without a document).
//! A clip view that changes size constrains its origin again the same way.
//! It follows its document's frame as AppKit's observes the document's
//! frame notification, through a hook in `views` rather than the
//! notification center: not while the document posts none (it catches up
//! when the document posts again), through `viewFrameChanged:` when a
//! subclass overrides it, constraining again, moving through the
//! superview's `scrollClipView:toPoint:` if it must, and reflecting. A
//! document that leaves the clip view (or is replaced by none) leaves the
//! bounds origin where the document's corner was.
//!
//! **Scroll views.** `tile` lays the clip view and scrollers out from the
//! border, the scroller style and the scrollers shown: legacy scrollers
//! take room from the clip view, overlay ones sit over it; content insets
//! and scroller insets move the scrollers in. `reflectScrolledClipView:`
//! gives each scroller its value and knob proportion over the document
//! and its insets, enables it while there's somewhere to scroll, and,
//! when the scroll view hides its scrollers automatically, shows or hides
//! them (tiling again when that changed). The scroller style is applied
//! at the next layout, as AppKit does.
//!
//! **Input.** `scrollWheel:` moves touchpads' precise deltas as points and
//! wheels' as lines of `verticalLineScroll` and `horizontalLineScroll`,
//! only along the predominant axis while `usesPredominantAxisScrolling`;
//! an event along no axis the document can move along goes to the next
//! responder, which is how a scroll view inside another hands the outer
//! one what it can't use. Each move posts
//! `NSScrollViewDidLiveScrollNotification` (before the scrollers
//! follow); a touchpad gesture's start posts `…WillStartLiveScroll…` and
//! its end (after its momentum, if any) `…DidEndLiveScroll…`, as a press
//! on a scroller does when it starts dragging the knob or paging and when
//! it ends. A wheel's moves aren't bracketed, as on macOS once a scroll
//! view has scrolled once. `pageUp:` and `pageDown:` move by the
//! visible height less `verticalPageScroll`; AppKit animates them,
//! Sidestep doesn't. Keys pass through, as on macOS: a scroll view
//! doesn't scroll on `keyDown:`. Linux doesn't rubber-band, so the
//! elasticity settings are only kept.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezierPath, NSBorderType, NSClipView, NSColor, NSControlSize, NSCursor, NSEvent, NSEventGestureAxis,
    NSEventPhase, NSResponder, NSScrollElasticity, NSScrollView, NSScroller, NSScrollerKnobStyle, NSScrollerPart,
    NSScrollerStyle, NSView, NSViewLayerContentsRedrawPolicy,
};
use objc2_foundation::{NSEdgeInsets, NSNotification, NSPoint, NSRect, NSSize};

use crate::protocol::Color;
use crate::views::{self, NSViewImpl};

// Posted by scroll views as they scroll and magnify, and when the desktop's
// scroller style changes; the values are macOS's.
sidestep_foundation::constant_string!(
    NSScrollViewWillStartLiveMagnifyNotification = "NSScrollViewWillStartLiveMagnifyNotification"
);
sidestep_foundation::constant_string!(
    NSScrollViewDidEndLiveMagnifyNotification = "NSScrollViewDidEndLiveMagnifyNotification"
);
sidestep_foundation::constant_string!(
    NSScrollViewWillStartLiveScrollNotification = "NSScrollViewWillStartLiveScrollNotification"
);
sidestep_foundation::constant_string!(NSScrollViewDidLiveScrollNotification = "NSScrollViewDidLiveScrollNotification");
sidestep_foundation::constant_string!(
    NSScrollViewDidEndLiveScrollNotification = "NSScrollViewDidEndLiveScrollNotification"
);
sidestep_foundation::constant_string!(
    NSPreferredScrollerStyleDidChangeNotification = "NSPreferredScrollerStyleDidChangeNotification"
);

fn no_insets() -> NSEdgeInsets {
    NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }
}

fn add_insets(a: NSEdgeInsets, b: NSEdgeInsets) -> NSEdgeInsets {
    NSEdgeInsets { top: a.top + b.top, left: a.left + b.left, bottom: a.bottom + b.bottom, right: a.right + b.right }
}

// NSClipView

pub(crate) struct ClipIvars {
    document: RefCell<Option<Retained<NSView>>>,
    /// None until set: `controlBackgroundColor`.
    background: RefCell<Option<Retained<NSColor>>>,
    draws_background: Cell<bool>,
    insets: Cell<NSEdgeInsets>,
    automatic_insets: Cell<bool>,
    copies_on_scroll: Cell<bool>,
    cursor: RefCell<Option<Retained<NSCursor>>>,
    /// Whether it has a layer now (see `layers`).
    layer: crate::layers::ClipLayer,
}

impl Default for ClipIvars {
    fn default() -> Self {
        ClipIvars {
            document: RefCell::new(None),
            background: RefCell::new(None),
            draws_background: Cell::new(true),
            insets: Cell::new(no_insets()),
            automatic_insets: Cell::new(true),
            copies_on_scroll: Cell::new(true),
            cursor: RefCell::new(None),
            layer: Default::default(),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
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
            views::mark_clip(views::imp(this.view()));
            // Though it draws, as macOS answers.
            crate::view_layout::set_redraw_policy(
                views::imp(this.view()),
                NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay,
            );
            this
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            // Asked outside the borrow: the document's answer is program code.
            self.document().is_some_and(|d| views::is_flipped(views::imp(&d)))
        }

        #[unsafe(method_id(documentView))]
        fn document_view(&self) -> Option<Retained<NSView>> {
            self.ivars().document.borrow().clone()
        }

        #[unsafe(method(setDocumentView:))]
        fn set_document_view(&self, document: Option<&NSView>) {
            set_document(self, document);
        }

        #[unsafe(method(documentRect))]
        fn document_rect(&self) -> NSRect {
            document_rect(self)
        }

        #[unsafe(method(documentVisibleRect))]
        fn document_visible_rect(&self) -> NSRect {
            document_visible_rect(self)
        }

        #[unsafe(method(scrollToPoint:))]
        fn scroll_to_point(&self, point: NSPoint) {
            // SAFETY: NSView's setBoundsOrigin: takes a point.
            let _: () = unsafe { msg_send![super(self), setBoundsOrigin: point] };
        }

        #[unsafe(method(setBoundsOrigin:))]
        fn set_bounds_origin(&self, origin: NSPoint) {
            scroll_constrained(self, origin);
        }

        #[unsafe(method(constrainBoundsRect:))]
        fn constrain_bounds_rect(&self, proposed: NSRect) -> NSRect {
            constrain(self, proposed)
        }

        #[unsafe(method(constrainScrollPoint:))]
        fn constrain_scroll_point(&self, point: NSPoint) -> NSPoint {
            let size = crate::view_layout::bounds(views::imp(self.view())).size;
            self.clip().constrainBoundsRect(NSRect::new(point, size)).origin
        }

        #[unsafe(method(contentInsets))]
        fn content_insets(&self) -> NSEdgeInsets {
            self.ivars().insets.get()
        }

        /// The document keeps its place on screen: the origin moves by as
        /// much as the inset at its low side does.
        #[unsafe(method(setContentInsets:))]
        fn set_content_insets(&self, insets: NSEdgeInsets) {
            let old = self.ivars().insets.replace(insets);
            let flipped = views::is_flipped(views::imp(self.view()));
            let low = |i: NSEdgeInsets| if flipped { i.top } else { i.bottom };
            let o = views::bounds(views::imp(self.view())).origin;
            let origin = NSPoint::new(o.x - (insets.left - old.left), o.y - (low(insets) - low(old)));
            self.view().setBoundsOrigin(origin);
        }

        #[unsafe(method(automaticallyAdjustsContentInsets))]
        fn automatically_adjusts_content_insets(&self) -> bool {
            self.ivars().automatic_insets.get()
        }

        #[unsafe(method(setAutomaticallyAdjustsContentInsets:))]
        fn set_automatically_adjusts_content_insets(&self, flag: bool) {
            self.ivars().automatic_insets.set(flag);
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Retained<NSColor> {
            background(self)
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: &NSColor) {
            let old = self.ivars().background.replace(Some(color.retain()));
            drop(old);
            self.view().setNeedsDisplay(true);
        }

        #[unsafe(method(drawsBackground))]
        fn draws_background(&self) -> bool {
            self.ivars().draws_background.get()
        }

        #[unsafe(method(setDrawsBackground:))]
        fn set_draws_background(&self, flag: bool) {
            if self.ivars().draws_background.replace(flag) != flag {
                self.view().setNeedsDisplay(true);
            }
        }

        #[unsafe(method(copiesOnScroll))]
        fn copies_on_scroll(&self) -> bool {
            self.ivars().copies_on_scroll.get()
        }

        #[unsafe(method(setCopiesOnScroll:))]
        fn set_copies_on_scroll(&self, flag: bool) {
            self.ivars().copies_on_scroll.set(flag);
        }

        #[unsafe(method_id(documentCursor))]
        fn document_cursor(&self) -> Option<Retained<NSCursor>> {
            self.ivars().cursor.borrow().clone()
        }

        #[unsafe(method(setDocumentCursor:))]
        fn set_document_cursor(&self, cursor: Option<&NSCursor>) {
            let old = self.ivars().cursor.replace(cursor.map(|c| c.retain()));
            drop(old);
            if let Some(window) = self.view().window() {
                window.invalidateCursorRectsForView(self.view());
            }
        }

        /// The document cursor over the clip view, as a cursor rectangle.
        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let cursor = self.ivars().cursor.borrow().clone();
            if let Some(cursor) = cursor {
                let view = self.view();
                view.addCursorRect_cursor(view.visibleRect(), &cursor);
            }
        }

        /// The document's frame changed (see `frame_changed`).
        #[unsafe(method(viewFrameChanged:))]
        fn view_frame_changed(&self, _notification: &NSNotification) {
            follow_document(self);
        }

        #[unsafe(method(viewBoundsChanged:))]
        fn view_bounds_changed(&self, _notification: &NSNotification) {
            follow_document(self);
        }

        /// The document leaving is no longer the document (as on macOS,
        /// also when `addSubview:` moves it up among the subviews, which
        /// sends this; `addSubview:positioned:relativeTo:` doesn't).
        #[unsafe(method(willRemoveSubview:))]
        fn will_remove_subview(&self, subview: &NSView) {
            // SAFETY: NSView's method, with the view it was given.
            let _: () = unsafe { msg_send![super(self), willRemoveSubview: subview] };
            let leaving = self.ivars().document.borrow().as_deref().is_some_and(|d| std::ptr::eq(d, subview));
            if leaving {
                let old = self.ivars().document.take();
                document_gone(self, old.as_deref());
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            if self.ivars().draws_background.get() {
                background(self).setFill();
                NSBezierPath::fillRect(dirty);
            }
        }
    }

    unsafe impl NSObjectProtocol for NSClipViewImpl {}
);

impl NSClipViewImpl {
    fn view(&self) -> &NSView {
        // SAFETY: NSClipView is a subclass of NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn clip(&self) -> &NSClipView {
        // SAFETY: this is an NSClipView.
        unsafe { &*(self as *const Self).cast::<NSClipView>() }
    }

    fn document(&self) -> Option<Retained<NSView>> {
        self.ivars().document.borrow().clone()
    }
}

/// A clip view as its implementation, if `view` is one.
fn clip_impl(view: &NSViewImpl) -> Option<&NSClipViewImpl> {
    // SAFETY: views marked as clip views are NSClipViews, whose instances
    // (subclasses' too) have NSClipViewImpl's layout.
    views::is_clip(view).then(|| unsafe { &*(view as *const NSViewImpl).cast::<NSClipViewImpl>() })
}

/// A clip view's layer state (see `layers`).
pub(crate) fn clip_layer(clip: &NSViewImpl) -> Option<&crate::layers::ClipLayer> {
    clip_impl(clip).map(|c| &c.ivars().layer)
}

/// A clip view's document, if it has one.
pub(crate) fn document_of(clip: &NSViewImpl) -> Option<Retained<NSView>> {
    clip_impl(clip).and_then(NSClipViewImpl::document)
}

pub(crate) fn has_document(clip: &NSViewImpl) -> bool {
    clip_impl(clip).is_some_and(|c| c.ivars().document.borrow().is_some())
}

fn background(clip: &NSClipViewImpl) -> Retained<NSColor> {
    let set = clip.ivars().background.borrow().clone();
    set.unwrap_or_else(NSColor::controlBackgroundColor)
}

/// The color a clip view fills its whole viewport with, if it draws an
/// opaque background, in its appearance: what its layer's tiles start as.
pub(crate) fn opaque_background(clip: &NSViewImpl) -> Option<Color> {
    let c = clip_impl(clip)?;
    if !c.ivars().draws_background.get() {
        return None;
    }
    let color = background(c);
    let _look = crate::appearance::Drawing::push(crate::appearance::effective(clip));
    let rgba = crate::color::resolve(&color);
    (rgba[3] >= 1.0).then_some(rgba)
}

fn set_document(clip: &NSClipViewImpl, document: Option<&NSView>) {
    let same = match (&*clip.ivars().document.borrow(), document) {
        (Some(a), Some(b)) => std::ptr::eq(&**a, b),
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    // Out of the ivar while views move, so `willRemoveSubview:` takes
    // neither for the document leaving.
    let old = clip.ivars().document.take();
    if let Some(old) = &old
        && views::superview_of(views::imp(old)).is_some_and(|s| std::ptr::eq(s, views::imp(clip.view())))
    {
        old.removeFromSuperview();
    }
    let Some(document) = document else {
        document_gone(clip, old.as_deref());
        return;
    };
    clip.view().addSubview(document);
    let replaced = clip.ivars().document.replace(Some(document.retain()));
    drop(replaced);
    drop(old);
    // The document's corner at the clip view's, as far as it may go.
    clip.view().setBoundsOrigin(document.frame().origin);
}

/// The document left (`old`, if there was one): the bounds origin stays
/// where its corner was, unconstrained, and the superview reflects.
fn document_gone(clip: &NSClipViewImpl, old: Option<&NSView>) {
    if let Some(old) = old {
        set_origin(clip, views::frame(views::imp(old)).origin);
    }
    if let Some(sup) = views::superview_of(views::imp(clip.view())).map(|s| views::as_view(s).retain()) {
        sup.reflectScrolledClipView(clip.clip());
    }
}

/// `documentRect`: the document's frame, at least as big as the clip view
/// less its content insets; empty without a document.
fn document_rect(clip: &NSClipViewImpl) -> NSRect {
    let Some(document) = clip.document() else { return NSRect::ZERO };
    let b = crate::view_layout::bounds(views::imp(clip.view()));
    let i = clip.ivars().insets.get();
    let visible = NSSize::new(b.size.width - i.left - i.right, b.size.height - i.top - i.bottom);
    let f = views::frame(views::imp(&document));
    NSRect::new(f.origin, NSSize::new(f.size.width.max(visible.width), f.size.height.max(visible.height)))
}

/// Keep proposed bounds over the document, which the content insets extend
/// on every side.
fn constrain(clip: &NSClipViewImpl, proposed: NSRect) -> NSRect {
    let r = clip.clip().documentRect();
    let i = clip.ivars().insets.get();
    let flipped = views::is_flipped(views::imp(clip.view()));
    let (low_y, high_y) = if flipped { (i.top, i.bottom) } else { (i.bottom, i.top) };
    let (o, size) = (proposed.origin, proposed.size);
    let x_min = r.origin.x - i.left;
    let x_max = (r.origin.x + r.size.width + i.right - size.width).max(x_min);
    let y_min = r.origin.y - low_y;
    let y_max = (r.origin.y + r.size.height + high_y - size.height).max(y_min);
    NSRect::new(NSPoint::new(o.x.clamp(x_min, x_max), o.y.clamp(y_min, y_max)), size)
}

/// NSView's `setBoundsOrigin:`, which moves the origin as it is told.
fn set_origin(clip: &NSClipViewImpl, origin: NSPoint) {
    // SAFETY: NSView's setBoundsOrigin: takes a point.
    let _: () = unsafe { msg_send![super(clip), setBoundsOrigin: origin] };
}

/// `setBoundsOrigin:`: constrained, moved, and the superview told.
fn scroll_constrained(clip: &NSClipViewImpl, origin: NSPoint) {
    let this = clip.clip();
    let size = crate::view_layout::bounds(views::imp(clip.view())).size;
    let target = this.constrainBoundsRect(NSRect::new(origin, size)).origin;
    set_origin(clip, target);
    if let Some(sup) = views::superview_of(views::imp(clip.view())).map(|s| views::as_view(s).retain()) {
        sup.reflectScrolledClipView(this);
    }
}

/// Constrain a clip view's origin again, after its size or insets changed.
fn reconstrain(clip: &NSClipViewImpl) {
    let origin = views::bounds(views::imp(clip.view())).origin;
    clip.view().setBoundsOrigin(origin);
}

/// The document's frame changed: constrained again, moved through the
/// superview when that changes the origin, and reflected.
fn follow_document(clip: &NSClipViewImpl) {
    let this = clip.clip();
    let b = crate::view_layout::bounds(views::imp(clip.view()));
    let target = this.constrainBoundsRect(b).origin;
    let sup = views::superview_of(views::imp(clip.view())).map(|s| views::as_view(s).retain());
    if target != b.origin {
        match &sup {
            Some(sup) => sup.scrollClipView_toPoint(this, target),
            None => this.scrollToPoint(target),
        }
    }
    if let Some(sup) = sup {
        sup.reflectScrolledClipView(this);
    }
}

/// A view's frame changed: a clip view with a document that changed size
/// constrains its origin again; the clip view whose document it is
/// follows it, unless the document posts no frame notifications now.
pub(crate) fn frame_changed(view: &NSViewImpl, old: NSRect) {
    if let Some(clip) = clip_impl(view) {
        let has_document = clip.ivars().document.borrow().is_some();
        if has_document && old.size != views::frame(view).size {
            reconstrain(clip);
        }
    }
    if views::posts_frame_changes(view) {
        document_changed(view);
    }
}

/// A view posts its frame notification again, and its frame changed
/// while it didn't: the clip view whose document it is catches up.
pub(crate) fn frame_posted(view: &NSViewImpl) {
    document_changed(view);
}

/// If `view` is a clip view's document, the clip view follows it: through
/// `viewFrameChanged:` when a subclass overrides that, as AppKit's clip
/// view hears of it.
fn document_changed(view: &NSViewImpl) {
    let Some(clip) = views::superview_of(view).and_then(clip_impl) else { return };
    let document = clip.ivars().document.borrow().clone();
    let Some(document) = document.filter(|d| std::ptr::eq(views::imp(d), view)) else { return };
    let class = clip.view().class();
    let own = |c: &AnyClass| c.instance_method(sel!(viewFrameChanged:)).map(|m| m.implementation());
    let overridden = !own(class).zip(own(NSClipViewImpl::class())).is_some_and(|(a, b)| std::ptr::fn_addr_eq(a, b));
    if overridden {
        let name = crate::notifications::name!(NSViewFrameDidChangeNotification);
        let object: &AnyObject = &document;
        // SAFETY: the notification's object is the view whose frame changed.
        let note = unsafe { NSNotification::notificationWithName_object(name, Some(object)) };
        let clip_view = clip.clip().retain();
        clip_view.viewFrameChanged(&note);
    } else {
        follow_document(clip);
    }
}

fn document_visible_rect(clip: &NSClipViewImpl) -> NSRect {
    let Some(document) = clip.document() else { return NSRect::ZERO };
    document.convertRect_fromView(views::bounds(views::imp(clip.view())), Some(clip.view()))
}

// NSView's part in scrolling, added by a category.

define_class!(
    /// NSView's scrolling methods. Receivers are NSViews.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "_SidestepViewScrolling"]
    struct ViewScrolling;

    impl ViewScrolling {
        /// A clip view scrolled: nothing, unless a scroll view says.
        #[unsafe(method(reflectScrolledClipView:))]
        fn reflect_scrolled_clip_view(&self, _clip: &NSClipView) {}

        #[unsafe(method(scrollClipView:toPoint:))]
        fn scroll_clip_view_to_point(&self, clip: &NSClipView, point: NSPoint) {
            clip.scrollToPoint(point);
        }
    }
);

sidestep_runtime::category!("NSView"(SidestepViewScrolling), |category| {
    // SAFETY: the helper's methods don't touch their receiver.
    unsafe { category.add_methods_of(ViewScrolling::class()) };
});

// NSScrollView

/// Boolean settings, bits of `ScrollIvars::flags`.
const HAS_VERTICAL: u32 = 1 << 0;
const HAS_HORIZONTAL: u32 = 1 << 1;
const AUTOHIDES: u32 = 1 << 2;
const DYNAMIC: u32 = 1 << 3;
const PREDOMINANT: u32 = 1 << 4;
const AUTOMATIC_INSETS: u32 = 1 << 5;
const MAGNIFIES: u32 = 1 << 6;
/// `tile` is running.
const TILING: u32 = 1 << 7;
/// The scrollers autohiding leaves out.
const HIDES_VERTICAL: u32 = 1 << 8;
const HIDES_HORIZONTAL: u32 = 1 << 9;
/// Tile at the next layout (a new scroller style).
const NEEDS_TILE: u32 = 1 << 10;
/// A touchpad gesture (or its momentum) is scrolling: live scroll
/// notifications are due.
const LIVE: u32 = 1 << 11;
/// A gesture ended; its live scroll ends unless momentum follows.
const ENDING: u32 = 1 << 12;
const RULERS_VISIBLE: u32 = 1 << 13;
const HAS_HORIZONTAL_RULER: u32 = 1 << 14;
const HAS_VERTICAL_RULER: u32 = 1 << 15;
/// The scrollers the last `tile` laid out as shown.
const LAID_VERTICAL: u32 = 1 << 16;
const LAID_HORIZONTAL: u32 = 1 << 17;

pub(crate) struct ScrollIvars {
    clip: RefCell<Option<Retained<NSClipView>>>,
    vertical: RefCell<Option<Retained<NSScroller>>>,
    horizontal: RefCell<Option<Retained<NSScroller>>>,
    flags: Cell<u32>,
    border: Cell<NSBorderType>,
    style: Cell<NSScrollerStyle>,
    knob_style: Cell<NSScrollerKnobStyle>,
    /// Horizontal, then vertical.
    line: Cell<[f64; 2]>,
    page: Cell<[f64; 2]>,
    elasticity: Cell<[NSScrollElasticity; 2]>,
    insets: Cell<NSEdgeInsets>,
    scroller_insets: Cell<NSEdgeInsets>,
    /// Magnification, its minimum and its maximum.
    magnification: Cell<[f64; 3]>,
    /// Where the clip view was at the last reflect, to flash overlay
    /// scrollers when it moves.
    reflected: Cell<Option<NSPoint>>,
}

impl ScrollIvars {
    fn new(style: NSScrollerStyle) -> Self {
        ScrollIvars {
            clip: RefCell::new(None),
            vertical: RefCell::new(None),
            horizontal: RefCell::new(None),
            flags: Cell::new(DYNAMIC | PREDOMINANT | AUTOMATIC_INSETS),
            border: Cell::new(NSBorderType::NoBorder),
            style: Cell::new(style),
            knob_style: Cell::new(NSScrollerKnobStyle::Default),
            line: Cell::new([10.0; 2]),
            page: Cell::new([10.0; 2]),
            elasticity: Cell::new([NSScrollElasticity::Automatic; 2]),
            insets: Cell::new(no_insets()),
            scroller_insets: Cell::new(no_insets()),
            magnification: Cell::new([1.0, 0.25, 4.0]),
            reflected: Cell::new(None),
        }
    }

    fn has(&self, flag: u32) -> bool {
        self.flags.get() & flag != 0
    }

    fn set(&self, flag: u32, on: bool) -> bool {
        let old = self.flags.get();
        self.flags.set(if on { old | flag } else { old & !flag });
        old & flag != 0
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSScrollView"]
    #[ivars = ScrollIvars]
    pub(crate) struct NSScrollViewImpl;

    impl NSScrollViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let mtm = MainThreadMarker::new().expect("AppKit on the main thread");
            crate::load_shell::<NSScroller>();
            let this = this.set_ivars(ScrollIvars::new(NSScroller::preferredScrollerStyle(mtm)));
            // SAFETY: NSView's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            // Though it draws, as macOS answers.
            crate::view_layout::set_redraw_policy(
                views::imp(this.view()),
                NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay,
            );
            let bounds = NSRect::new(NSPoint::ZERO, frame.size);
            let clip = NSClipView::initWithFrame(NSClipView::alloc(mtm), bounds);
            this.view().addSubview(&clip);
            this.ivars().clip.replace(Some(clip));
            this
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            self.clip().drawsBackground()
        }

        #[unsafe(method_id(contentView))]
        fn content_view(&self) -> Retained<NSClipView> {
            self.clip()
        }

        #[unsafe(method(setContentView:))]
        fn set_content_view(&self, clip: &NSClipView) {
            let old = self.clip();
            if std::ptr::eq(&*old, clip) {
                return;
            }
            let this = self.view();
            if clip.contentInsets() != self.ivars().insets.get() {
                clip.setContentInsets(self.ivars().insets.get());
            }
            this.addSubview_positioned_relativeTo(clip, objc2_app_kit::NSWindowOrderingMode::Above, Some(&old));
            old.removeFromSuperview();
            self.ivars().clip.replace(Some(clip.retain()));
            self.scroll_view().tile();
            drop(old);
        }

        #[unsafe(method_id(documentView))]
        fn document_view(&self) -> Option<Retained<NSView>> {
            self.clip().documentView()
        }

        #[unsafe(method(setDocumentView:))]
        fn set_document_view(&self, document: Option<&NSView>) {
            let clip = self.clip();
            clip.setDocumentView(document);
            let this = self.scroll_view();
            this.tile();
            this.reflectScrolledClipView(&clip);
        }

        #[unsafe(method(documentVisibleRect))]
        fn document_visible_rect(&self) -> NSRect {
            self.clip().documentVisibleRect()
        }

        #[unsafe(method(contentSize))]
        fn content_size(&self) -> NSSize {
            self.clip().frame().size
        }

        #[unsafe(method_id(documentCursor))]
        fn document_cursor(&self) -> Option<Retained<NSCursor>> {
            self.clip().documentCursor()
        }

        #[unsafe(method(setDocumentCursor:))]
        fn set_document_cursor(&self, cursor: Option<&NSCursor>) {
            self.clip().setDocumentCursor(cursor);
        }

        #[unsafe(method(borderType))]
        fn border_type(&self) -> NSBorderType {
            self.ivars().border.get()
        }

        #[unsafe(method(setBorderType:))]
        fn set_border_type(&self, border: NSBorderType) {
            if self.ivars().border.replace(border) != border {
                self.scroll_view().tile();
                self.view().setNeedsDisplay(true);
            }
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Retained<NSColor> {
            self.clip().backgroundColor()
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: &NSColor) {
            self.clip().setBackgroundColor(color);
        }

        #[unsafe(method(drawsBackground))]
        fn draws_background(&self) -> bool {
            self.clip().drawsBackground()
        }

        #[unsafe(method(setDrawsBackground:))]
        fn set_draws_background(&self, flag: bool) {
            self.clip().setDrawsBackground(flag);
        }

        #[unsafe(method(hasVerticalScroller))]
        fn has_vertical_scroller(&self) -> bool {
            self.ivars().has(HAS_VERTICAL)
        }

        #[unsafe(method(setHasVerticalScroller:))]
        fn set_has_vertical_scroller(&self, flag: bool) {
            self.set_has(true, flag);
        }

        #[unsafe(method(hasHorizontalScroller))]
        fn has_horizontal_scroller(&self) -> bool {
            self.ivars().has(HAS_HORIZONTAL)
        }

        #[unsafe(method(setHasHorizontalScroller:))]
        fn set_has_horizontal_scroller(&self, flag: bool) {
            self.set_has(false, flag);
        }

        #[unsafe(method_id(verticalScroller))]
        fn vertical_scroller(&self) -> Option<Retained<NSScroller>> {
            self.ivars().vertical.borrow().clone()
        }

        #[unsafe(method(setVerticalScroller:))]
        fn set_vertical_scroller(&self, scroller: Option<&NSScroller>) {
            self.replace_scroller(true, scroller);
        }

        #[unsafe(method_id(horizontalScroller))]
        fn horizontal_scroller(&self) -> Option<Retained<NSScroller>> {
            self.ivars().horizontal.borrow().clone()
        }

        #[unsafe(method(setHorizontalScroller:))]
        fn set_horizontal_scroller(&self, scroller: Option<&NSScroller>) {
            self.replace_scroller(false, scroller);
        }

        #[unsafe(method(autohidesScrollers))]
        fn autohides_scrollers(&self) -> bool {
            self.ivars().has(AUTOHIDES)
        }

        #[unsafe(method(setAutohidesScrollers:))]
        fn set_autohides_scrollers(&self, flag: bool) {
            if self.ivars().set(AUTOHIDES, flag) != flag {
                self.scroll_view().reflectScrolledClipView(&self.clip());
            }
        }

        #[unsafe(method(horizontalLineScroll))]
        fn horizontal_line_scroll(&self) -> f64 {
            self.ivars().line.get()[0]
        }

        #[unsafe(method(setHorizontalLineScroll:))]
        fn set_horizontal_line_scroll(&self, amount: f64) {
            let [_, v] = self.ivars().line.get();
            self.ivars().line.set([amount, v]);
        }

        #[unsafe(method(verticalLineScroll))]
        fn vertical_line_scroll(&self) -> f64 {
            self.ivars().line.get()[1]
        }

        #[unsafe(method(setVerticalLineScroll:))]
        fn set_vertical_line_scroll(&self, amount: f64) {
            let [h, _] = self.ivars().line.get();
            self.ivars().line.set([h, amount]);
        }

        /// The vertical amount, as AppKit answers.
        #[unsafe(method(lineScroll))]
        fn line_scroll(&self) -> f64 {
            self.ivars().line.get()[1]
        }

        #[unsafe(method(setLineScroll:))]
        fn set_line_scroll(&self, amount: f64) {
            self.ivars().line.set([amount; 2]);
        }

        #[unsafe(method(horizontalPageScroll))]
        fn horizontal_page_scroll(&self) -> f64 {
            self.ivars().page.get()[0]
        }

        #[unsafe(method(setHorizontalPageScroll:))]
        fn set_horizontal_page_scroll(&self, amount: f64) {
            let [_, v] = self.ivars().page.get();
            self.ivars().page.set([amount, v]);
        }

        #[unsafe(method(verticalPageScroll))]
        fn vertical_page_scroll(&self) -> f64 {
            self.ivars().page.get()[1]
        }

        #[unsafe(method(setVerticalPageScroll:))]
        fn set_vertical_page_scroll(&self, amount: f64) {
            let [h, _] = self.ivars().page.get();
            self.ivars().page.set([h, amount]);
        }

        #[unsafe(method(pageScroll))]
        fn page_scroll(&self) -> f64 {
            self.ivars().page.get()[1]
        }

        #[unsafe(method(setPageScroll:))]
        fn set_page_scroll(&self, amount: f64) {
            self.ivars().page.set([amount; 2]);
        }

        #[unsafe(method(scrollsDynamically))]
        fn scrolls_dynamically(&self) -> bool {
            self.ivars().has(DYNAMIC)
        }

        #[unsafe(method(setScrollsDynamically:))]
        fn set_scrolls_dynamically(&self, flag: bool) {
            self.ivars().set(DYNAMIC, flag);
        }

        #[unsafe(method(tile))]
        fn tile(&self) {
            tile(self);
        }

        #[unsafe(method(reflectScrolledClipView:))]
        fn reflect_scrolled_clip_view(&self, clip: &NSClipView) {
            if std::ptr::eq(&*self.clip(), clip) {
                reflect(self);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            if !scroll_by_wheel(self, event) {
                // SAFETY: NSResponder's scrollWheel: passes the event on.
                let _: () = unsafe { msg_send![super(self), scrollWheel: event] };
            }
        }

        #[unsafe(method(scrollerStyle))]
        fn scroller_style(&self) -> NSScrollerStyle {
            self.ivars().style.get()
        }

        #[unsafe(method(setScrollerStyle:))]
        fn set_scroller_style(&self, style: NSScrollerStyle) {
            if self.ivars().style.replace(style) != style {
                for scroller in self.scrollers() {
                    scroller.setScrollerStyle(style);
                }
                // Laid out again at the next layout, as AppKit does.
                self.ivars().set(NEEDS_TILE, true);
                self.view().setNeedsLayout(true);
            }
        }

        #[unsafe(method(scrollerKnobStyle))]
        fn scroller_knob_style(&self) -> NSScrollerKnobStyle {
            self.ivars().knob_style.get()
        }

        #[unsafe(method(setScrollerKnobStyle:))]
        fn set_scroller_knob_style(&self, style: NSScrollerKnobStyle) {
            self.ivars().knob_style.set(style);
            for scroller in self.scrollers() {
                scroller.setKnobStyle(style);
            }
        }

        #[unsafe(method(flashScrollers))]
        fn flash_scrollers(&self) {
            if self.ivars().style.get() == NSScrollerStyle::Overlay {
                for scroller in self.scrollers() {
                    crate::scroller::flash(&scroller);
                }
            }
        }

        #[unsafe(method(horizontalScrollElasticity))]
        fn horizontal_scroll_elasticity(&self) -> NSScrollElasticity {
            self.ivars().elasticity.get()[0]
        }

        #[unsafe(method(setHorizontalScrollElasticity:))]
        fn set_horizontal_scroll_elasticity(&self, elasticity: NSScrollElasticity) {
            let [_, v] = self.ivars().elasticity.get();
            self.ivars().elasticity.set([elasticity, v]);
        }

        #[unsafe(method(verticalScrollElasticity))]
        fn vertical_scroll_elasticity(&self) -> NSScrollElasticity {
            self.ivars().elasticity.get()[1]
        }

        #[unsafe(method(setVerticalScrollElasticity:))]
        fn set_vertical_scroll_elasticity(&self, elasticity: NSScrollElasticity) {
            let [h, _] = self.ivars().elasticity.get();
            self.ivars().elasticity.set([h, elasticity]);
        }

        #[unsafe(method(usesPredominantAxisScrolling))]
        fn uses_predominant_axis_scrolling(&self) -> bool {
            self.ivars().has(PREDOMINANT)
        }

        #[unsafe(method(setUsesPredominantAxisScrolling:))]
        fn set_uses_predominant_axis_scrolling(&self, flag: bool) {
            self.ivars().set(PREDOMINANT, flag);
        }

        #[unsafe(method(automaticallyAdjustsContentInsets))]
        fn automatically_adjusts_content_insets(&self) -> bool {
            self.ivars().has(AUTOMATIC_INSETS)
        }

        #[unsafe(method(setAutomaticallyAdjustsContentInsets:))]
        fn set_automatically_adjusts_content_insets(&self, flag: bool) {
            self.ivars().set(AUTOMATIC_INSETS, flag);
            self.clip().setAutomaticallyAdjustsContentInsets(flag);
        }

        #[unsafe(method(contentInsets))]
        fn content_insets(&self) -> NSEdgeInsets {
            self.ivars().insets.get()
        }

        #[unsafe(method(setContentInsets:))]
        fn set_content_insets(&self, insets: NSEdgeInsets) {
            self.ivars().insets.set(insets);
            self.clip().setContentInsets(insets);
            self.scroll_view().tile();
        }

        #[unsafe(method(scrollerInsets))]
        fn scroller_insets(&self) -> NSEdgeInsets {
            self.ivars().scroller_insets.get()
        }

        #[unsafe(method(setScrollerInsets:))]
        fn set_scroller_insets(&self, insets: NSEdgeInsets) {
            self.ivars().scroller_insets.set(insets);
        }

        // Magnification: kept and clamped; drawing doesn't scale yet.

        #[unsafe(method(allowsMagnification))]
        fn allows_magnification(&self) -> bool {
            self.ivars().has(MAGNIFIES)
        }

        #[unsafe(method(setAllowsMagnification:))]
        fn set_allows_magnification(&self, flag: bool) {
            self.ivars().set(MAGNIFIES, flag);
        }

        #[unsafe(method(magnification))]
        fn magnification(&self) -> f64 {
            self.ivars().magnification.get()[0]
        }

        #[unsafe(method(setMagnification:))]
        fn set_magnification(&self, magnification: f64) {
            let b = self.clip().bounds();
            let center = NSPoint::new(b.origin.x + b.size.width / 2.0, b.origin.y + b.size.height / 2.0);
            magnify(self, magnification, center);
        }

        #[unsafe(method(setMagnification:centeredAtPoint:))]
        fn set_magnification_centered_at_point(&self, magnification: f64, point: NSPoint) {
            let clip = self.clip();
            let center = clip.convertPoint_fromView(point, clip.documentView().as_deref());
            magnify(self, magnification, center);
        }

        #[unsafe(method(minMagnification))]
        fn min_magnification(&self) -> f64 {
            self.ivars().magnification.get()[1]
        }

        #[unsafe(method(setMinMagnification:))]
        fn set_min_magnification(&self, min: f64) {
            let [m, _, max] = self.ivars().magnification.get();
            self.ivars().magnification.set([m, min, max]);
        }

        #[unsafe(method(maxMagnification))]
        fn max_magnification(&self) -> f64 {
            self.ivars().magnification.get()[2]
        }

        #[unsafe(method(setMaxMagnification:))]
        fn set_max_magnification(&self, max: f64) {
            let [m, min, _] = self.ivars().magnification.get();
            self.ivars().magnification.set([m, min, max]);
        }

        /// The magnification that fits `rect` (document points) in the
        /// visible area, within the limits, with `rect` centered.
        #[unsafe(method(magnifyToFitRect:))]
        fn magnify_to_fit_rect(&self, rect: NSRect) {
            let clip = self.clip();
            let size = clip.frame().size;
            if rect.size.width > 0.0 && rect.size.height > 0.0 {
                let fit = (size.width / rect.size.width).min(size.height / rect.size.height);
                let b = clip.bounds();
                magnify(self, fit, b.origin);
                let at = clip.convertRect_fromView(rect, clip.documentView().as_deref());
                let shown = clip.bounds().size;
                clip.setBoundsOrigin(NSPoint::new(
                    at.origin.x + (at.size.width - shown.width) / 2.0,
                    at.origin.y + (at.size.height - shown.height) / 2.0,
                ));
            }
        }

        /// A pinch magnifies when the scroll view allows it, as a live
        /// magnification from its start to its end.
        #[unsafe(method(magnifyWithEvent:))]
        fn magnify_with_event(&self, event: &NSEvent) {
            if !self.ivars().has(MAGNIFIES) {
                // SAFETY: NSResponder's magnifyWithEvent: passes it on.
                let _: () = unsafe { msg_send![super(self), magnifyWithEvent: event] };
                return;
            }
            let view = self.view();
            let phase = event.phase();
            if phase.contains(NSEventPhase::Began) {
                notify(view, crate::notifications::name!(NSScrollViewWillStartLiveMagnifyNotification));
            }
            // About the pointer.
            let clip = self.clip();
            let at = clip.convertPoint_fromView(event.locationInWindow(), None);
            let m = self.ivars().magnification.get()[0] * (1.0 + event.magnification());
            magnify(self, m, at);
            if phase.contains(NSEventPhase::Ended) || phase.contains(NSEventPhase::Cancelled) {
                notify(view, crate::notifications::name!(NSScrollViewDidEndLiveMagnifyNotification));
            }
        }

        #[unsafe(method(addFloatingSubview:forAxis:))]
        fn add_floating_subview(&self, view: &NSView, _axis: NSEventGestureAxis) {
            self.view().addSubview(view);
        }

        // Rulers: kept, none shown.

        #[unsafe(method(rulersVisible))]
        fn rulers_visible(&self) -> bool {
            self.ivars().has(RULERS_VISIBLE)
        }

        #[unsafe(method(setRulersVisible:))]
        fn set_rulers_visible(&self, flag: bool) {
            self.ivars().set(RULERS_VISIBLE, flag);
        }

        #[unsafe(method(hasHorizontalRuler))]
        fn has_horizontal_ruler(&self) -> bool {
            self.ivars().has(HAS_HORIZONTAL_RULER)
        }

        #[unsafe(method(setHasHorizontalRuler:))]
        fn set_has_horizontal_ruler(&self, flag: bool) {
            self.ivars().set(HAS_HORIZONTAL_RULER, flag);
        }

        #[unsafe(method(hasVerticalRuler))]
        fn has_vertical_ruler(&self) -> bool {
            self.ivars().has(HAS_VERTICAL_RULER)
        }

        #[unsafe(method(setHasVerticalRuler:))]
        fn set_has_vertical_ruler(&self, flag: bool) {
            self.ivars().set(HAS_VERTICAL_RULER, flag);
        }

        #[unsafe(method(pageUp:))]
        fn page_up(&self, _sender: Option<&AnyObject>) {
            page(self, -1.0);
        }

        #[unsafe(method(pageDown:))]
        fn page_down(&self, _sender: Option<&AnyObject>) {
            page(self, 1.0);
        }

        /// A scroller moved (its action): scroll where it says.
        #[unsafe(method(_sidestepScrollerMoved:))]
        fn scroller_moved(&self, sender: Option<&AnyObject>) {
            if let Some(scroller) = sender.and_then(|s| s.downcast_ref::<NSScroller>()) {
                follow_scroller(self, scroller);
            }
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews_with_old_size(&self, old: NSSize) {
            // SAFETY: NSView's method, with the size it was given.
            let _: () = unsafe { msg_send![super(self), resizeSubviewsWithOldSize: old] };
            self.scroll_view().tile();
        }

        #[unsafe(method(layout))]
        fn layout(&self) {
            // SAFETY: NSView's layout.
            let _: () = unsafe { msg_send![super(self), layout] };
            if self.ivars().set(NEEDS_TILE, false) {
                self.scroll_view().tile();
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_frame(self);
        }

        // Sizes for a content size, and the other way round.

        #[unsafe(method(frameSizeForContentSize:horizontalScrollerClass:verticalScrollerClass:borderType:controlSize:scrollerStyle:))]
        fn frame_size_for_content_size(
            size: NSSize,
            horizontal: Option<&AnyClass>,
            vertical: Option<&AnyClass>,
            border: NSBorderType,
            control_size: NSControlSize,
            style: NSScrollerStyle,
        ) -> NSSize {
            let (w, h) = extra(horizontal, vertical, border, control_size, style);
            NSSize::new(size.width + w, size.height + h)
        }

        #[unsafe(method(contentSizeForFrameSize:horizontalScrollerClass:verticalScrollerClass:borderType:controlSize:scrollerStyle:))]
        fn content_size_for_frame_size(
            size: NSSize,
            horizontal: Option<&AnyClass>,
            vertical: Option<&AnyClass>,
            border: NSBorderType,
            control_size: NSControlSize,
            style: NSScrollerStyle,
        ) -> NSSize {
            let (w, h) = extra(horizontal, vertical, border, control_size, style);
            NSSize::new(size.width - w, size.height - h)
        }

        #[unsafe(method(frameSizeForContentSize:hasHorizontalScroller:hasVerticalScroller:borderType:))]
        fn frame_size_for_content_size_old(size: NSSize, horizontal: bool, vertical: bool, border: NSBorderType) -> NSSize {
            let (w, h) = extra_preferred(horizontal, vertical, border);
            NSSize::new(size.width + w, size.height + h)
        }

        #[unsafe(method(contentSizeForFrameSize:hasHorizontalScroller:hasVerticalScroller:borderType:))]
        fn content_size_for_frame_size_old(size: NSSize, horizontal: bool, vertical: bool, border: NSBorderType) -> NSSize {
            let (w, h) = extra_preferred(horizontal, vertical, border);
            NSSize::new(size.width - w, size.height - h)
        }
    }

    unsafe impl NSObjectProtocol for NSScrollViewImpl {}
);

impl NSScrollViewImpl {
    fn view(&self) -> &NSView {
        // SAFETY: NSScrollView is a subclass of NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn scroll_view(&self) -> &NSScrollView {
        // SAFETY: this is an NSScrollView.
        unsafe { &*(self as *const Self).cast::<NSScrollView>() }
    }

    fn clip(&self) -> Retained<NSClipView> {
        self.ivars().clip.borrow().clone().expect("NSScrollView without a clip view")
    }

    fn scroller(&self, vertical: bool) -> Option<Retained<NSScroller>> {
        let slot = if vertical { &self.ivars().vertical } else { &self.ivars().horizontal };
        slot.borrow().clone()
    }

    fn scrollers(&self) -> impl Iterator<Item = Retained<NSScroller>> {
        self.scroller(true).into_iter().chain(self.scroller(false))
    }

    /// `setHasVerticalScroller:` and `setHasHorizontalScroller:`: make the
    /// scroller the first time, above the clip view.
    fn set_has(&self, vertical: bool, flag: bool) {
        let bit = if vertical { HAS_VERTICAL } else { HAS_HORIZONTAL };
        if self.ivars().set(bit, flag) == flag {
            return;
        }
        if flag && self.scroller(vertical).is_none() {
            let mtm = MainThreadMarker::from(self);
            let width = NSScroller::scrollerWidthForControlSize_scrollerStyle(
                NSControlSize::Regular,
                self.ivars().style.get(),
                mtm,
            );
            let frame = if vertical {
                NSRect::new(NSPoint::ZERO, NSSize::new(width, width * 2.0))
            } else {
                NSRect::new(NSPoint::ZERO, NSSize::new(width * 2.0, width))
            };
            let scroller = NSScroller::initWithFrame(NSScroller::alloc(mtm), frame);
            self.adopt(vertical, &scroller);
        }
        self.scroll_view().tile();
    }

    /// `setVerticalScroller:` and `setHorizontalScroller:`: the new one
    /// takes the old one's place, laid out at the next `tile`.
    fn replace_scroller(&self, vertical: bool, scroller: Option<&NSScroller>) {
        let slot = if vertical { &self.ivars().vertical } else { &self.ivars().horizontal };
        let old = slot.replace(None);
        if let Some(old) = &old {
            old.removeFromSuperview();
        }
        if let Some(scroller) = scroller {
            self.adopt(vertical, scroller);
        }
        drop(old);
    }

    /// Make `scroller` this scroll view's vertical or horizontal scroller.
    fn adopt(&self, vertical: bool, scroller: &NSScroller) {
        crate::scroller::set_vertical(scroller, vertical);
        scroller.setScrollerStyle(self.ivars().style.get());
        scroller.setKnobStyle(self.ivars().knob_style.get());
        let target: &AnyObject = self.view();
        // SAFETY: a control's target is weak, so the scroll view going away
        // leaves the scroller no dangling target; the action takes the
        // sender.
        unsafe {
            scroller.setTarget(Some(target));
            scroller.setAction(Some(sel!(_sidestepScrollerMoved:)));
        }
        self.view().addSubview(scroller);
        let slot = if vertical { &self.ivars().vertical } else { &self.ivars().horizontal };
        slot.replace(Some(scroller.retain()));
    }
}

/// Set the magnification, within its limits: the clip view's bounds scale,
/// keeping `anchor` (clip view coordinates) where it is on screen, and
/// stay over the document. Drawing doesn't scale yet (bounds scaling is
/// stored and reported only, see `view_layout`).
fn magnify(sv: &NSScrollViewImpl, magnification: f64, anchor: NSPoint) {
    let [_, min, max] = sv.ivars().magnification.get();
    let m = magnification.clamp(min, max.max(min));
    sv.ivars().magnification.set([m, min, max]);
    let clip = sv.clip();
    let b = clip.bounds();
    let frame = clip.frame().size;
    let size = NSSize::new(frame.width / m, frame.height / m);
    clip.setBoundsSize(size);
    let kept = |at: f64, origin: f64, new: f64, old: f64| if old > 0.0 { at - (at - origin) * new / old } else { at };
    let origin = NSPoint::new(
        kept(anchor.x, b.origin.x, size.width, b.size.width),
        kept(anchor.y, b.origin.y, size.height, b.size.height),
    );
    clip.setBoundsOrigin(origin);
}

/// The width a border takes on each side.
fn border_width(border: NSBorderType) -> f64 {
    match border {
        NSBorderType::NoBorder => 0.0,
        NSBorderType::GrooveBorder => 2.0,
        _ => 1.0,
    }
}

/// A scroller class's width at a size and style.
fn scroller_width(class: &AnyClass, size: NSControlSize, style: NSScrollerStyle) -> f64 {
    // SAFETY: NSScroller's class method, which subclasses may override,
    // takes a control size and a style and returns a CGFloat.
    unsafe { msg_send![class, scrollerWidthForControlSize: size, scrollerStyle: style] }
}

/// What a scroll view adds to its content's width and height: legacy
/// scrollers and the border (one point a side for any border here).
fn extra(
    horizontal: Option<&AnyClass>,
    vertical: Option<&AnyClass>,
    border: NSBorderType,
    size: NSControlSize,
    style: NSScrollerStyle,
) -> (f64, f64) {
    let b = if border == NSBorderType::NoBorder { 0.0 } else { 2.0 };
    let legacy = style == NSScrollerStyle::Legacy;
    let width = |class: Option<&AnyClass>| class.filter(|_| legacy).map_or(0.0, |c| scroller_width(c, size, style));
    (b + width(vertical), b + width(horizontal))
}

/// The same, for the preferred style and regular scrollers.
fn extra_preferred(horizontal: bool, vertical: bool, border: NSBorderType) -> (f64, f64) {
    let mtm = MainThreadMarker::new().expect("AppKit on the main thread");
    crate::load_shell::<NSScroller>();
    let class = NSScroller::class();
    let pick = |on: bool| on.then_some(class);
    extra(pick(horizontal), pick(vertical), border, NSControlSize::Regular, NSScroller::preferredScrollerStyle(mtm))
}

/// Which scrollers show: had, and not hidden by autohiding.
fn shown(sv: &NSScrollViewImpl) -> (bool, bool) {
    let i = sv.ivars();
    (
        i.has(HAS_VERTICAL) && !(i.has(AUTOHIDES) && i.has(HIDES_VERTICAL)),
        i.has(HAS_HORIZONTAL) && !(i.has(AUTOHIDES) && i.has(HIDES_HORIZONTAL)),
    )
}

/// `tile`: the clip view and the scrollers, inside the border. Legacy
/// scrollers take their width from the clip view; overlay scrollers lie
/// over it, full length. Both are moved in by the content and scroller
/// insets.
fn tile(sv: &NSScrollViewImpl) {
    let ivars = sv.ivars();
    if ivars.set(TILING, true) {
        return;
    }
    let b = views::bounds(views::imp(sv.view()));
    let edge = border_width(ivars.border.get());
    let r = NSRect::new(
        NSPoint::new(b.origin.x + edge, b.origin.y + edge),
        NSSize::new((b.size.width - 2.0 * edge).max(0.0), (b.size.height - 2.0 * edge).max(0.0)),
    );
    let style = ivars.style.get();
    let legacy = style == NSScrollerStyle::Legacy;
    let (show_v, show_h) = shown(sv);
    ivars.set(LAID_VERTICAL, show_v);
    ivars.set(LAID_HORIZONTAL, show_h);
    let width_of = |s: &Option<Retained<NSScroller>>| {
        s.as_ref().map_or(0.0, |s| scroller_width(s.class(), s.controlSize(), style))
    };
    let (vertical, horizontal) = (sv.scroller(true), sv.scroller(false));
    let vw = if show_v { width_of(&vertical) } else { 0.0 };
    let hh = if show_h { width_of(&horizontal) } else { 0.0 };
    let i = add_insets(ivars.insets.get(), ivars.scroller_insets.get());
    let (room_w, room_h) = if legacy { (r.size.width - vw, r.size.height - hh) } else { (r.size.width, r.size.height) };
    let place = |scroller: &Option<Retained<NSScroller>>, has: bool, show: bool, frame: NSRect| {
        let Some(s) = scroller else { return };
        if !has {
            // Out of the way, as AppKit leaves a scroller no longer had.
            s.setHidden(true);
            s.setFrameOrigin(NSPoint::new(-100.0, -100.0));
        } else if !show {
            s.setHidden(true);
        } else {
            s.setFrame(frame);
            s.setHidden(false);
        }
    };
    place(
        &vertical,
        ivars.has(HAS_VERTICAL),
        show_v,
        NSRect::new(
            NSPoint::new(r.origin.x + r.size.width - vw - i.right, r.origin.y + i.top),
            NSSize::new(vw, (room_h - i.top - i.bottom).max(0.0)),
        ),
    );
    place(
        &horizontal,
        ivars.has(HAS_HORIZONTAL),
        show_h,
        NSRect::new(
            NSPoint::new(r.origin.x + i.left, r.origin.y + r.size.height - hh - i.bottom),
            NSSize::new((room_w - i.left - i.right).max(0.0), hh),
        ),
    );
    let clip = sv.clip();
    clip.setFrame(if legacy { NSRect::new(r.origin, NSSize::new(room_w.max(0.0), room_h.max(0.0))) } else { r });
    ivars.set(TILING, false);
}

/// Along one axis, what the clip view shows of its document: the knob's
/// proportion and the scroller's value (0 at the top or left).
fn axis(visible: f64, doc_min: f64, doc_len: f64, low: f64, high: f64, at: f64, from_end: bool) -> (f64, f64) {
    let total = doc_len + low + high;
    let proportion = if total > 0.0 { (visible / total).min(1.0) } else { 1.0 };
    let range = total - visible;
    let value = if range > 0.0 {
        let along = (at - (doc_min - low)) / range;
        if from_end { 1.0 - along } else { along }
    } else {
        0.0
    };
    (proportion, value.clamp(0.0, 1.0))
}

/// `reflectScrolledClipView:`: the scrollers follow the clip view, and
/// autohiding shows or hides them.
fn reflect(sv: &NSScrollViewImpl) {
    let clip = sv.clip();
    let c = views::imp(&clip);
    let bounds = crate::view_layout::bounds(c);
    let doc = clip.documentRect();
    let i = clip.contentInsets();
    let flipped = views::is_flipped(c);
    let (ph, vh) = axis(bounds.size.width, doc.origin.x, doc.size.width, i.left, i.right, bounds.origin.x, false);
    let (low, high) = if flipped { (i.top, i.bottom) } else { (i.bottom, i.top) };
    let (pv, vv) = axis(bounds.size.height, doc.origin.y, doc.size.height, low, high, bounds.origin.y, !flipped);
    let has_document = clip.documentView().is_some();
    for (scroller, proportion, value) in [(sv.scroller(true), pv, vv), (sv.scroller(false), ph, vh)] {
        if let Some(s) = scroller {
            s.setKnobProportion(proportion);
            s.setDoubleValue(value);
            s.setEnabled(has_document && proportion < 1.0);
        }
    }
    let ivars = sv.ivars();
    // Overlay scrollers show while the clip view moves.
    let moved = ivars.reflected.replace(Some(bounds.origin)).is_some_and(|o| o != bounds.origin);
    if moved && ivars.style.get() == NSScrollerStyle::Overlay {
        for (scroller, show) in [(sv.scroller(true), shown(sv).0), (sv.scroller(false), shown(sv).1)] {
            if let Some(s) = scroller.filter(|_| show) {
                crate::scroller::flash(&s);
            }
        }
    }
    if ivars.has(TILING) {
        return;
    }
    ivars.set(HIDES_VERTICAL, pv >= 1.0);
    ivars.set(HIDES_HORIZONTAL, ph >= 1.0);
    let laid = (ivars.has(LAID_VERTICAL), ivars.has(LAID_HORIZONTAL));
    if ivars.has(AUTOHIDES) && shown(sv) != laid {
        sv.scroll_view().tile();
    }
}

/// Whether the clip view can move along each axis at all.
fn scrollable(clip: &NSClipView) -> (bool, bool) {
    let b = clip.bounds();
    let far = clip.constrainBoundsRect(NSRect::new(NSPoint::new(f64::MAX / 4.0, f64::MAX / 4.0), b.size)).origin;
    let near = clip.constrainBoundsRect(NSRect::new(NSPoint::new(f64::MIN / 4.0, f64::MIN / 4.0), b.size)).origin;
    (far.x > near.x, far.y > near.y)
}

/// Scroll the clip view to `origin` (constrained), post
/// `NSScrollViewDidLiveScrollNotification` if `live`, and have the
/// scrollers follow. Returns whether it moved.
fn scroll_to(sv: &NSScrollViewImpl, clip: &NSClipView, origin: NSPoint, live: bool) -> bool {
    let b = clip.bounds();
    let target = clip.constrainBoundsRect(NSRect::new(origin, b.size)).origin;
    if target == b.origin {
        return false;
    }
    clip.scrollToPoint(target);
    if live {
        notify(sv.view(), crate::notifications::name!(NSScrollViewDidLiveScrollNotification));
    }
    sv.scroll_view().reflectScrolledClipView(clip);
    true
}

/// Handle a scroll event; false when it's for the next responder.
fn scroll_by_wheel(sv: &NSScrollViewImpl, event: &NSEvent) -> bool {
    let ivars = sv.ivars();
    let clip = sv.clip();
    let (mut dx, mut dy) = (event.scrollingDeltaX(), event.scrollingDeltaY());
    if !event.hasPreciseScrollingDeltas() {
        let [h, v] = ivars.line.get();
        (dx, dy) = (dx * h, dy * v);
    }
    if ivars.has(PREDOMINANT) {
        if dx.abs() > dy.abs() {
            dy = 0.0;
        } else if dy.abs() > dx.abs() {
            dx = 0.0;
        }
    }
    let (can_x, can_y) = scrollable(&clip);
    let (phase, momentum) = (event.phase(), event.momentumPhase());
    let moving = dx != 0.0 || dy != 0.0;
    if moving && !(dx != 0.0 && can_x) && !(dy != 0.0 && can_y) {
        return false;
    }
    if phase.contains(NSEventPhase::Began) || phase.contains(NSEventPhase::MayBegin) {
        begin_live(sv);
    }
    if momentum.contains(NSEventPhase::Began) {
        // Momentum carries the gesture on.
        ivars.set(ENDING, false);
    }
    if moving {
        let b = clip.bounds();
        let flipped = views::is_flipped(views::imp(&clip));
        // Positive deltas scroll toward the top and the left.
        let x = if can_x { b.origin.x - dx } else { b.origin.x };
        let y = match (can_y, flipped) {
            (false, _) => b.origin.y,
            (true, true) => b.origin.y - dy,
            (true, false) => b.origin.y + dy,
        };
        scroll_to(sv, &clip, NSPoint::new(x, y), true);
    }
    if momentum.contains(NSEventPhase::Ended) || momentum.contains(NSEventPhase::Cancelled) {
        end_live(sv);
    } else if phase.contains(NSEventPhase::Ended) || phase.contains(NSEventPhase::Cancelled) {
        // Unless momentum follows, which is known once the event is handled.
        ivars.set(ENDING, true);
        ending_later(sv);
    }
    true
}

/// A gesture or a scroller's tracking starts a live scroll.
fn begin_live(sv: &NSScrollViewImpl) {
    sv.ivars().set(ENDING, false);
    if !sv.ivars().set(LIVE, true) {
        notify(sv.view(), crate::notifications::name!(NSScrollViewWillStartLiveScrollNotification));
    }
}

/// A press on `scroller` started tracking (dragging its knob or paging) or
/// ended: its scroll view's live scroll starts or ends.
pub(crate) fn scroller_tracking(scroller: &NSView, started: bool) {
    let Some(sup) = views::superview_of(views::imp(scroller)).map(|s| views::as_view(s).retain()) else { return };
    let Some(sv) = sup.downcast_ref::<NSScrollView>() else { return };
    let sv = scroll_impl(sv);
    if started {
        begin_live(sv);
    } else {
        end_live(sv);
    }
}

/// The live scroll is over.
fn end_live(sv: &NSScrollViewImpl) {
    sv.ivars().set(ENDING, false);
    if sv.ivars().set(LIVE, false) {
        notify(sv.view(), crate::notifications::name!(NSScrollViewDidEndLiveScrollNotification));
    }
}

thread_local! {
    /// Scroll views whose gesture ended, to end their live scroll once the
    /// event is handled unless momentum took over.
    static ENDING_VIEWS: RefCell<Vec<Weak<NSScrollView>>> = const { RefCell::new(Vec::new()) };
}

fn ending_later(sv: &NSScrollViewImpl) {
    let first = ENDING_VIEWS.with(|e| {
        let mut e = e.borrow_mut();
        e.push(Weak::new(sv.scroll_view()));
        e.len() == 1
    });
    if first {
        let block = block2::RcBlock::new(|_: NonNull<objc2_foundation::NSTimer>| {
            let views: Vec<Weak<NSScrollView>> = ENDING_VIEWS.with(|e| std::mem::take(&mut *e.borrow_mut()));
            let coasting = crate::momentum::deadline().is_some();
            for sv in views.iter().filter_map(Weak::load) {
                let imp = scroll_impl(&sv);
                if imp.ivars().has(ENDING) && !coasting {
                    end_live(imp);
                }
            }
        });
        // SAFETY: the block runs on the main thread, where the timer is
        // scheduled, in the common modes (NSRunLoopCommonModes is
        // Foundation's constant).
        unsafe {
            let timer = objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(0.0, false, &block);
            objc2_foundation::NSRunLoop::mainRunLoop().addTimer_forMode(&timer, objc2_foundation::NSRunLoopCommonModes);
        }
    }
}

fn scroll_impl(sv: &NSScrollView) -> &NSScrollViewImpl {
    // SAFETY: every NSScrollView (subclasses too) has NSScrollViewImpl's
    // layout.
    unsafe { &*(sv as *const NSScrollView).cast::<NSScrollViewImpl>() }
}

fn notify(view: &NSView, name: &objc2_foundation::NSString) {
    let object: &AnyObject = view;
    sidestep_foundation::notification_center::post(name, Some(object), None);
}

/// `pageUp:` and `pageDown:`: a visible height less the page overlap.
fn page(sv: &NSScrollViewImpl, direction: f64) {
    let clip = sv.clip();
    let b = clip.bounds();
    let step = (b.size.height - sv.ivars().page.get()[1]).max(1.0) * direction;
    let flipped = views::is_flipped(views::imp(&clip));
    let y = if flipped { b.origin.y + step } else { b.origin.y - step };
    scroll_to(sv, &clip, NSPoint::new(b.origin.x, y), false);
}

/// A scroller's knob moved or its slot was clicked: scroll to its value,
/// or by a page, through `scrollClipView:toPoint:`, live while the
/// scroller tracks a press.
fn follow_scroller(sv: &NSScrollViewImpl, scroller: &NSScroller) {
    let vertical = sv.scroller(true).is_some_and(|v| std::ptr::eq(&*v, scroller));
    let clip = sv.clip();
    let b = clip.bounds();
    let part = scroller.hitPart();
    let [ph, pv] = sv.ivars().page.get();
    let flipped = views::is_flipped(views::imp(&clip));
    let target = match part {
        NSScrollerPart::DecrementPage | NSScrollerPart::IncrementPage => {
            let sign = if part == NSScrollerPart::IncrementPage { 1.0 } else { -1.0 };
            if vertical {
                let step = (b.size.height - pv).max(1.0) * sign;
                NSPoint::new(b.origin.x, if flipped { b.origin.y + step } else { b.origin.y - step })
            } else {
                NSPoint::new(b.origin.x + (b.size.width - ph).max(1.0) * sign, b.origin.y)
            }
        }
        _ => {
            let doc = clip.documentRect();
            let i = clip.contentInsets();
            let value = scroller.doubleValue();
            if vertical {
                let (low, high) = if flipped { (i.top, i.bottom) } else { (i.bottom, i.top) };
                let min = doc.origin.y - low;
                let range = (doc.size.height + low + high - b.size.height).max(0.0);
                let along = if flipped { value } else { 1.0 - value };
                NSPoint::new(b.origin.x, min + along * range)
            } else {
                let min = doc.origin.x - i.left;
                let range = (doc.size.width + i.left + i.right - b.size.width).max(0.0);
                NSPoint::new(min + value * range, b.origin.y)
            }
        }
    };
    let target = clip.constrainBoundsRect(NSRect::new(target, b.size)).origin;
    if target == b.origin {
        return;
    }
    let this = sv.scroll_view();
    this.scrollClipView_toPoint(&clip, target);
    if sv.ivars().has(LIVE) {
        notify(sv.view(), crate::notifications::name!(NSScrollViewDidLiveScrollNotification));
    }
    this.reflectScrolledClipView(&clip);
}

/// The border, and the corner between two legacy scrollers.
fn draw_frame(sv: &NSScrollViewImpl) {
    if !crate::theme::paint::recording() {
        return;
    }
    let bounds = views::bounds(views::imp(sv.view()));
    let palette = crate::theme::palette();
    let ivars = sv.ivars();
    let (v, h) = shown(sv);
    if ivars.style.get() == NSScrollerStyle::Legacy
        && v
        && h
        && let (Some(vs), Some(hs)) = (sv.scroller(true), sv.scroller(false))
    {
        let (vf, hf) = (vs.frame(), hs.frame());
        let corner = NSRect::new(NSPoint::new(vf.origin.x, hf.origin.y), NSSize::new(vf.size.width, hf.size.height));
        crate::theme::paint::fill_rect(corner, crate::scroller::track_color());
    }
    let width = border_width(ivars.border.get());
    if width > 0.0 {
        frame_rect(bounds, palette.outline);
        if ivars.border.get() == NSBorderType::GrooveBorder {
            let inner = NSRect::new(
                NSPoint::new(bounds.origin.x + 1.0, bounds.origin.y + 1.0),
                NSSize::new(bounds.size.width - 2.0, bounds.size.height - 2.0),
            );
            frame_rect(inner, palette.separator);
        }
    }
}

/// A one-point line along each edge of `r`.
fn frame_rect(r: NSRect, color: Color) {
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    for edge in [
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, 1.0)),
        NSRect::new(NSPoint::new(x, y + h - 1.0), NSSize::new(w, 1.0)),
        NSRect::new(NSPoint::new(x, y), NSSize::new(1.0, h)),
        NSRect::new(NSPoint::new(x + w - 1.0, y), NSSize::new(1.0, h)),
    ] {
        crate::theme::paint::fill_rect(edge, color);
    }
}
