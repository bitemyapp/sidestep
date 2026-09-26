//! Scroll views, clip views and scrollers, checked on macOS and on Linux
//! alike: scrollers' widths, parts and hit testing at each style and size;
//! clip views' document rectangles, constraints, insets, the difference
//! between `scrollToPoint:` and `setBoundsOrigin:`, following and losing
//! their documents; views' redraw policies; scroll views' defaults,
//! tiling (styles, borders, insets), reflecting, autohiding, replacing
//! their parts, the class size helpers, paging, magnification and the
//! messages and notifications scrolling sends. Every test sets the
//! scroller style it uses, since the preferred one follows the Mac's
//! settings.
//!
//! Windows are never shown. AppKit belongs to the main thread, so this
//! file has its own `main`. On macOS, `wheel_probe` scrolls with wheel
//! events built from CGEvents and `tracking_probe` drags a scroller's knob
//! with events AppKit's tracking loop takes from the queue; both wait for
//! AppKit, which applies some scrolls later, animated.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBorderType, NSClipView, NSColor, NSControl, NSControlSize, NSEvent, NSEventModifierFlags,
    NSEventType, NSResponder, NSScrollElasticity, NSScrollView, NSScroller, NSScrollerKnobStyle, NSScrollerPart,
    NSScrollerStyle, NSUsableScrollerParts, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSEdgeInsets, NSNotification, NSNotificationCenter, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

mod common;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

/// What the logging views and observer heard since the last call.
fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn pt(x: f64, y: f64) -> NSPoint {
    NSPoint::new(x, y)
}

fn insets(top: f64, left: f64, bottom: f64, right: f64) -> NSEdgeInsets {
    NSEdgeInsets { top, left, bottom, right }
}

struct DocIvars {
    flipped: bool,
}

define_class!(
    /// A document, flipped or not.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceScrollDocument"]
    #[ivars = DocIvars]
    struct Document;

    impl Document {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().flipped
        }
    }
);

fn document(mtm: MainThreadMarker, frame: NSRect, flipped: bool) -> Retained<NSView> {
    let this = Document::alloc(mtm).set_ivars(DocIvars { flipped });
    // SAFETY: NSView's designated initializer.
    let view: Retained<Document> = unsafe { msg_send![super(this), initWithFrame: frame] };
    Retained::into_super(view)
}

define_class!(
    /// A scroll view that logs `tile` and `reflectScrolledClipView:`.
    #[unsafe(super(NSScrollView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceLoggingScrollView"]
    struct Logging;

    impl Logging {
        #[unsafe(method(reflectScrolledClipView:))]
        fn reflect(&self, clip: &NSClipView) {
            let o = clip.bounds().origin;
            log(format!("reflect {} {}", o.x, o.y));
            // SAFETY: the superclass's method, with the clip view it was given.
            unsafe { msg_send![super(self), reflectScrolledClipView: clip] }
        }

        #[unsafe(method(tile))]
        fn tile(&self) {
            log("tile".into());
            // SAFETY: the superclass's method.
            unsafe { msg_send![super(self), tile] }
        }
    }
);

define_class!(
    /// A clip view that logs `viewFrameChanged:`.
    #[unsafe(super(NSClipView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceLoggingClipView"]
    struct LoggingClip;

    impl LoggingClip {
        #[unsafe(method(viewFrameChanged:))]
        fn view_frame_changed(&self, note: &NSNotification) {
            log(format!("viewFrameChanged {}", note.name()));
            // SAFETY: the superclass's method, with the notification.
            unsafe { msg_send![super(self), viewFrameChanged: note] }
        }
    }
);

define_class!(
    /// A view with a `drawRect:` of its own.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceScrollDrawing"]
    struct Drawing;

    impl Drawing {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {}
    }
);

fn scroll_view(mtm: MainThreadMarker, frame: NSRect, style: NSScrollerStyle) -> Retained<NSScrollView> {
    // SAFETY: NSScrollView's designated initializer.
    let view: Retained<Logging> = unsafe { msg_send![Logging::alloc(mtm), initWithFrame: frame] };
    let view = Retained::into_super(view);
    view.setScrollerStyle(style);
    view.tile();
    take();
    view
}

define_class!(
    /// Logs the notifications it observes.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceScrollObserver"]
    struct Observer;

    impl Observer {
        #[unsafe(method(seen:))]
        fn seen(&self, note: &NSNotification) {
            log(format!("note {}", note.name()));
        }
    }

    unsafe impl NSObjectProtocol for Observer {}
);

define_class!(
    /// A scroller that draws its own knob.
    #[unsafe(super(NSScroller, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceCustomScroller"]
    struct Custom;

    impl Custom {
        #[unsafe(method(drawKnob))]
        fn draw_knob(&self) {}
    }
);

/// A window that is never shown.
fn window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    // SAFETY: a plain window, made later (defer), never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

fn scroller(mtm: MainThreadMarker, frame: NSRect, style: NSScrollerStyle) -> Retained<NSScroller> {
    let s = NSScroller::initWithFrame(NSScroller::alloc(mtm), frame);
    s.setScrollerStyle(style);
    s
}

fn frame_of(s: Option<Retained<NSScroller>>) -> NSRect {
    s.expect("a scroller").frame()
}

fn scroller_classes(mtm: MainThreadMarker) {
    use NSControlSize as S;
    use NSScrollerStyle as St;
    let width = |size, style| NSScroller::scrollerWidthForControlSize_scrollerStyle(size, style, mtm);
    assert_eq!([S::Regular, S::Small, S::Mini, S::Large].map(|s| width(s, St::Legacy)), [17.0, 13.0, 13.0, 17.0]);
    assert_eq!([S::Regular, S::Small, S::Mini, S::Large].map(|s| width(s, St::Overlay)), [17.0, 15.0, 15.0, 17.0]);
    #[allow(deprecated)]
    {
        assert_eq!(NSScroller::scrollerWidth(mtm), 17.0);
        assert_eq!(NSScroller::scrollerWidthForControlSize(S::Small, mtm), 13.0);
    }
    assert!(NSScroller::isCompatibleWithOverlayScrollers(mtm));
    // A subclass that draws its own knob isn't.
    // SAFETY: the class method takes nothing and returns BOOL.
    let custom: bool = unsafe { msg_send![Custom::class(), isCompatibleWithOverlayScrollers] };
    assert!(!custom);
    // No cell.
    // SAFETY: NSControl's class method returns a class or nil.
    let cell: Option<&objc2::runtime::AnyClass> = unsafe { msg_send![NSScroller::class(), cellClass] };
    assert!(cell.is_none());
    let style = NSScroller::preferredScrollerStyle(mtm);
    assert!(style == St::Legacy || style == St::Overlay);
}

fn scroller_defaults(mtm: MainThreadMarker) {
    let s = NSScroller::initWithFrame(NSScroller::alloc(mtm), rect(0.0, 0.0, 17.0, 200.0));
    assert_eq!((s.doubleValue(), s.knobProportion()), (0.0, 0.0));
    assert_eq!(s.scrollerStyle(), NSScrollerStyle::Legacy);
    assert_eq!(s.knobStyle(), NSScrollerKnobStyle::Default);
    assert_eq!(s.controlSize(), NSControlSize::Regular);
    assert_eq!(s.usableParts(), NSUsableScrollerParts::NoScrollerParts);
    assert_eq!(s.hitPart(), NSScrollerPart::NoPart);
    assert!(!s.isEnabled() && s.isFlipped() && !s.isOpaque());
    assert!(s.cell().is_none());
    // Values and proportions stay between 0 and 1.
    s.setKnobProportion(1.5);
    assert_eq!(s.knobProportion(), 1.0);
    s.setDoubleValue(1.5);
    assert_eq!(s.doubleValue(), 1.0);
    s.setDoubleValue(-0.5);
    assert_eq!(s.doubleValue(), 0.0);
    // NaN makes either whole: nothing to scroll.
    s.setKnobProportion(0.3);
    s.setDoubleValue(0.3);
    s.setKnobProportion(f64::NAN);
    s.setDoubleValue(f64::NAN);
    assert_eq!((s.doubleValue(), s.knobProportion()), (1.0, 1.0));
    s.setEnabled(true);
    assert_eq!(s.usableParts(), NSUsableScrollerParts::AllScrollerParts);
}

fn scroller_parts(mtm: MainThreadMarker) {
    use NSScrollerPart as P;
    // Vertical, legacy: the slot inset 3 from the ends and the outer edge.
    let s = scroller(mtm, rect(0.0, 0.0, 17.0, 206.0), NSScrollerStyle::Legacy);
    s.setEnabled(true);
    s.setDoubleValue(0.5);
    s.setKnobProportion(0.25);
    assert_eq!(s.rectForPart(P::KnobSlot), rect(3.0, 3.0, 11.0, 200.0));
    assert_eq!(s.rectForPart(P::Knob), rect(3.0, 78.0, 11.0, 50.0));
    assert_eq!(s.rectForPart(P::DecrementPage), rect(3.0, 3.0, 11.0, 75.0));
    assert_eq!(s.rectForPart(P::IncrementPage), rect(3.0, 128.0, 11.0, 75.0));
    assert_eq!(s.rectForPart(P::NoPart), NSRect::ZERO);
    // testPart: takes window coordinates: y up from the scroller's bottom
    // here, as it's in no window.
    let at = |y: f64| s.testPart(pt(8.0, y));
    assert_eq!(
        [0.0, 2.0, 5.0, 60.0, 80.0, 100.0, 130.0, 150.0, 204.0].map(at),
        [
            P::NoPart,
            P::NoPart,
            P::IncrementPage,
            P::IncrementPage,
            P::Knob,
            P::Knob,
            P::DecrementPage,
            P::DecrementPage,
            P::NoPart
        ]
    );
    assert_eq!([0.0, 16.0, 20.0].map(|x| s.testPart(pt(x, 100.0))), [P::NoPart; 3]);
    // The shortest knob, and a knob filling the slot.
    s.setKnobProportion(0.0);
    assert_eq!(s.rectForPart(P::Knob), rect(3.0, 93.0, 11.0, 20.0));
    s.setKnobProportion(1.0);
    assert_eq!(s.rectForPart(P::Knob), rect(3.0, 3.0, 11.0, 200.0));
    assert_eq!(at(100.0), P::Knob);
    s.setEnabled(false);
    assert_eq!(at(100.0), P::NoPart);
    // Overlay: a thinner slot at the outer edge, a longer shortest knob.
    let s = scroller(mtm, rect(0.0, 0.0, 17.0, 206.0), NSScrollerStyle::Overlay);
    s.setEnabled(true);
    s.setDoubleValue(0.5);
    s.setKnobProportion(0.25);
    assert_eq!(s.rectForPart(P::KnobSlot), rect(8.0, 3.0, 6.0, 200.0));
    assert_eq!(s.rectForPart(P::Knob), rect(8.0, 78.0, 6.0, 50.0));
    assert_eq!(s.testPart(pt(2.0, 100.0)), P::NoPart);
    assert_eq!(s.testPart(pt(10.0, 100.0)), P::Knob);
    // Sizes: the slot's thickness and the shortest knob.
    for (style, size, x, thick, shortest) in [
        (NSScrollerStyle::Legacy, NSControlSize::Regular, 6.0, 11.0, 20.0),
        (NSScrollerStyle::Legacy, NSControlSize::Small, 10.0, 7.0, 16.0),
        (NSScrollerStyle::Legacy, NSControlSize::Mini, 10.0, 7.0, 16.0),
        (NSScrollerStyle::Legacy, NSControlSize::Large, 6.0, 11.0, 20.0),
        (NSScrollerStyle::Overlay, NSControlSize::Regular, 11.0, 6.0, 26.0),
        (NSScrollerStyle::Overlay, NSControlSize::Small, 13.0, 4.0, 26.0),
        (NSScrollerStyle::Overlay, NSControlSize::Mini, 13.0, 4.0, 26.0),
        (NSScrollerStyle::Overlay, NSControlSize::Large, 11.0, 6.0, 26.0),
    ] {
        let s = scroller(mtm, rect(0.0, 0.0, 20.0, 206.0), style);
        s.setEnabled(true);
        s.setControlSize(size);
        s.setDoubleValue(0.5);
        s.setKnobProportion(0.01);
        assert_eq!(s.rectForPart(P::KnobSlot), rect(x, 3.0, thick, 200.0), "{style:?} {size:?}");
        let y = 3.0 + (200.0 - shortest) / 2.0;
        assert_eq!(s.rectForPart(P::Knob), rect(x, y, thick, shortest), "{style:?} {size:?}");
    }
    // Horizontal when made wider than tall.
    let h = scroller(mtm, rect(0.0, 0.0, 206.0, 17.0), NSScrollerStyle::Legacy);
    h.setEnabled(true);
    h.setDoubleValue(0.5);
    h.setKnobProportion(0.25);
    assert_eq!(h.rectForPart(P::KnobSlot), rect(3.0, 3.0, 200.0, 11.0));
    assert_eq!(h.rectForPart(P::Knob), rect(78.0, 3.0, 50.0, 11.0));
    assert_eq!(h.rectForPart(P::DecrementPage), rect(3.0, 3.0, 75.0, 11.0));
    assert_eq!(h.rectForPart(P::IncrementPage), rect(128.0, 3.0, 75.0, 11.0));
    h.setScrollerStyle(NSScrollerStyle::Overlay);
    assert_eq!(h.rectForPart(P::KnobSlot), rect(3.0, 8.0, 200.0, 6.0));
    // Made square or empty: vertical, and it stays so when resized.
    let square = scroller(mtm, rect(0.0, 0.0, 17.0, 17.0), NSScrollerStyle::Legacy);
    square.setEnabled(true);
    square.setKnobProportion(0.5);
    assert_eq!(square.rectForPart(P::Knob), rect(3.0, 3.0, 11.0, 20.0));
    let later = scroller(mtm, NSRect::ZERO, NSScrollerStyle::Legacy);
    later.setEnabled(true);
    later.setKnobProportion(0.25);
    later.setFrame(rect(0.0, 0.0, 206.0, 17.0));
    assert_eq!(later.rectForPart(P::Knob), rect(192.0, 3.0, 11.0, 20.0));
    // Too short for the shortest knob: nothing is usable, and no part is
    // hit (a knob is still placed, and longer than the slot).
    for (style, size, height, fits) in [
        (NSScrollerStyle::Legacy, NSControlSize::Regular, 25.0, false),
        (NSScrollerStyle::Legacy, NSControlSize::Regular, 26.0, true),
        (NSScrollerStyle::Legacy, NSControlSize::Small, 21.0, false),
        (NSScrollerStyle::Legacy, NSControlSize::Small, 22.0, true),
        (NSScrollerStyle::Overlay, NSControlSize::Regular, 31.0, false),
        (NSScrollerStyle::Overlay, NSControlSize::Regular, 32.0, true),
    ] {
        let s = scroller(mtm, rect(0.0, 0.0, 17.0, 100.0), style);
        s.setControlSize(size);
        s.setEnabled(true);
        s.setKnobProportion(0.5);
        s.setDoubleValue(0.5);
        s.setFrameSize(NSSize::new(17.0, height));
        let (usable, part) = if fits {
            (NSUsableScrollerParts::AllScrollerParts, P::Knob)
        } else {
            (NSUsableScrollerParts::NoScrollerParts, P::NoPart)
        };
        assert_eq!(s.usableParts(), usable, "{style:?} {size:?} {height}");
        assert_eq!(s.testPart(pt(12.0, height / 2.0)), part, "{style:?} {size:?} {height}");
    }
    let s = scroller(mtm, rect(0.0, 0.0, 17.0, 20.0), NSScrollerStyle::Legacy);
    s.setEnabled(true);
    s.setKnobProportion(0.5);
    s.setDoubleValue(0.5);
    assert_eq!(s.rectForPart(P::Knob), rect(3.0, 0.0, 11.0, 20.0));
    // A scroll view makes it horizontal when it takes it as such.
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHorizontalScroller(Some(&later));
    sv.setHasHorizontalScroller(true);
    assert_eq!(later.frame(), rect(0.0, 133.0, 200.0, 17.0));
    assert_eq!(later.rectForPart(P::KnobSlot), rect(3.0, 3.0, 194.0, 11.0));
}

fn clip_view_defaults(mtm: MainThreadMarker) {
    let c = NSClipView::initWithFrame(NSClipView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
    assert!(c.drawsBackground());
    assert!(c.backgroundColor().isEqual(Some(&NSColor::controlBackgroundColor())));
    assert_eq!(c.contentInsets(), insets(0.0, 0.0, 0.0, 0.0));
    assert!(c.automaticallyAdjustsContentInsets());
    #[allow(deprecated)]
    {
        assert!(c.copiesOnScroll());
    }
    assert!(c.documentCursor().is_none() && c.documentView().is_none());
    assert!(!c.isFlipped() && !c.isOpaque());
    c.setDrawsBackground(false);
    assert!(!c.isOpaque());
    assert!(c.postsBoundsChangedNotifications() && c.postsFrameChangedNotifications());
    // Without a document, bounds stay at the origin, and there is no
    // document rectangle, insets or not.
    assert_eq!(c.constrainBoundsRect(rect(33.0, 44.0, 100.0, 100.0)), rect(0.0, 0.0, 100.0, 100.0));
    assert_eq!(c.documentRect(), NSRect::ZERO);
    c.setContentInsets(insets(10.0, 5.0, 20.0, 7.0));
    assert_eq!(c.documentRect(), NSRect::ZERO);
    assert_eq!(c.constrainBoundsRect(rect(33.0, 44.0, 100.0, 100.0)).origin, pt(-5.0, -20.0));
}

fn redraw_policies(mtm: MainThreadMarker) {
    use objc2_app_kit::NSViewLayerContentsRedrawPolicy as Policy;
    // A view redraws on setNeedsDisplay: unless its class draws, which
    // then draws again on every resize; AppKit's clip and scroll views
    // redraw on setNeedsDisplay:, scrollers never.
    let plain = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    let flipped = document(mtm, rect(0.0, 0.0, 10.0, 10.0), true);
    // SAFETY: NSView's designated initializer.
    let drawing: Retained<Drawing> =
        unsafe { msg_send![Drawing::alloc(mtm), initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] };
    let drawing: Retained<NSView> = Retained::into_super(drawing);
    assert_eq!(plain.layerContentsRedrawPolicy(), Policy::OnSetNeedsDisplay);
    assert_eq!(flipped.layerContentsRedrawPolicy(), Policy::OnSetNeedsDisplay);
    assert_eq!(drawing.layerContentsRedrawPolicy(), Policy::DuringViewResize);
    let clip = NSClipView::initWithFrame(NSClipView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert_eq!(clip.layerContentsRedrawPolicy(), Policy::OnSetNeedsDisplay);
    let sv = scroll_view(mtm, rect(0.0, 0.0, 100.0, 100.0), NSScrollerStyle::Legacy);
    assert_eq!(sv.layerContentsRedrawPolicy(), Policy::OnSetNeedsDisplay);
    let s = scroller(mtm, rect(0.0, 0.0, 17.0, 100.0), NSScrollerStyle::Legacy);
    assert_eq!(s.layerContentsRedrawPolicy(), Policy::Never);
    // In a window too; and as set.
    let w = window(mtm);
    w.setContentView(Some(&plain));
    plain.addSubview(&drawing);
    assert_eq!(drawing.layerContentsRedrawPolicy(), Policy::DuringViewResize);
    drawing.setLayerContentsRedrawPolicy(Policy::OnSetNeedsDisplay);
    assert_eq!(drawing.layerContentsRedrawPolicy(), Policy::OnSetNeedsDisplay);
    w.setContentView(None);
}

fn documents_and_constraints(mtm: MainThreadMarker) {
    let c = NSClipView::initWithFrame(NSClipView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
    // A small document: the rectangle is at least the clip view's.
    let small = document(mtm, rect(0.0, 0.0, 50.0, 40.0), false);
    c.setDocumentView(Some(&small));
    assert!(!c.isFlipped());
    assert_eq!(c.documentRect(), rect(0.0, 0.0, 100.0, 100.0));
    assert_eq!(c.documentVisibleRect(), rect(0.0, 0.0, 100.0, 100.0));
    assert_eq!(c.constrainBoundsRect(rect(20.0, 30.0, 100.0, 100.0)), rect(0.0, 0.0, 100.0, 100.0));
    let flipped = document(mtm, rect(0.0, 0.0, 50.0, 40.0), true);
    c.setDocumentView(Some(&flipped));
    assert!(c.isFlipped());
    assert_eq!(c.constrainBoundsRect(rect(20.0, 30.0, 100.0, 100.0)), rect(0.0, 0.0, 100.0, 100.0));
    // SAFETY: reading the old document's superview.
    assert!(unsafe { small.superview() }.is_none(), "the old document leaves");
    // A document away from the origin: the clip view starts at its corner.
    let offset = document(mtm, rect(10.0, 20.0, 300.0, 400.0), false);
    c.setDocumentView(Some(&offset));
    assert_eq!(c.documentRect(), rect(10.0, 20.0, 300.0, 400.0));
    assert_eq!(c.bounds(), rect(10.0, 20.0, 100.0, 100.0));
    assert_eq!(c.constrainBoundsRect(rect(-50.0, -50.0, 100.0, 100.0)), rect(10.0, 20.0, 100.0, 100.0));
    assert_eq!(c.constrainBoundsRect(rect(0.3, 30.7, 100.0, 100.0)), rect(10.0, 30.7, 100.0, 100.0));
    // scrollToPoint: goes where it's told; setBoundsOrigin: is constrained.
    c.scrollToPoint(pt(1000.0, 1000.0));
    assert_eq!(c.bounds().origin, pt(1000.0, 1000.0));
    c.scrollToPoint(pt(-50.0, -50.0));
    assert_eq!(c.bounds().origin, pt(-50.0, -50.0));
    c.setBoundsOrigin(pt(500.0, 500.0));
    assert_eq!(c.bounds().origin, pt(210.0, 320.0));
    // Content insets: the document can scroll past its edges by them (an
    // unflipped document's bottom inset is at its low y).
    c.setContentInsets(insets(10.0, 5.0, 20.0, 7.0));
    assert_eq!(c.documentRect(), rect(10.0, 20.0, 300.0, 400.0));
    assert_eq!(c.constrainBoundsRect(rect(-100.0, -100.0, 100.0, 100.0)).origin, pt(5.0, 0.0));
    assert_eq!(c.constrainBoundsRect(rect(1000.0, 1000.0, 100.0, 100.0)).origin, pt(217.0, 330.0));
    c.setDocumentView(Some(&flipped));
    assert_eq!(c.documentRect(), rect(0.0, 0.0, 88.0, 70.0));
    assert_eq!(c.constrainBoundsRect(rect(-100.0, -100.0, 100.0, 100.0)).origin, pt(-5.0, -10.0));
    assert_eq!(c.constrainBoundsRect(rect(1000.0, 1000.0, 100.0, 100.0)).origin, pt(-5.0, -10.0));
    #[allow(deprecated)]
    {
        assert_eq!(c.constrainScrollPoint(pt(40.0, 40.0)), pt(-5.0, -10.0));
    }
}

fn scrolling_messages(mtm: MainThreadMarker) {
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    let d = document(mtm, rect(0.0, 0.0, 400.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    let clip = sv.contentView();
    // SAFETY: the observer outlives its registrations, removed below.
    let observer: Retained<Observer> = unsafe { msg_send![Observer::alloc(mtm), init] };
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: seen: takes the notification.
    unsafe { center.addObserver_selector_name_object(&observer, sel!(seen:), None, Some(&clip)) };
    take();
    // scrollToPoint: posts the bounds notification and tells no one else.
    clip.scrollToPoint(pt(0.0, 100.0));
    assert_eq!(take(), ["note NSViewBoundsDidChangeNotification"]);
    // setBoundsOrigin: has the scroll view reflect.
    clip.setBoundsOrigin(pt(10.0, 200.0));
    assert_eq!(take(), ["note NSViewBoundsDidChangeNotification", "reflect 10 200"]);
    // scrollPoint: too.
    d.scrollPoint(pt(0.0, 300.0));
    assert_eq!(take(), ["note NSViewBoundsDidChangeNotification", "reflect 0 300"]);
    // A document that shrinks keeps the clip view over it.
    d.setFrameSize(NSSize::new(400.0, 350.0));
    assert_eq!(take(), ["note NSViewBoundsDidChangeNotification", "reflect 0 200"]);
    assert_eq!(clip.bounds().origin, pt(0.0, 200.0));
    // SAFETY: removing the observer's registrations.
    unsafe { center.removeObserver(&observer) };
    // The document's frame, the clip view's size: constrained again.
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    let d = document(mtm, rect(0.0, 0.0, 183.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    let clip = sv.contentView();
    clip.scrollToPoint(pt(0.0, 850.0));
    take();
    sv.setFrameSize(NSSize::new(200.0, 300.0));
    assert_eq!(take(), ["tile", "reflect 0 700"]);
    assert_eq!(clip.bounds().origin, pt(0.0, 700.0));
    // A document that fits: nothing to scroll.
    d.setFrameSize(NSSize::new(183.0, 100.0));
    let v = sv.verticalScroller().expect("a vertical scroller");
    assert!(!v.isEnabled());
    assert_eq!(v.usableParts(), NSUsableScrollerParts::NoScrollerParts);
    assert_eq!(clip.bounds().origin, pt(0.0, 0.0));
}

fn following_documents(mtm: MainThreadMarker) {
    let sv = scroll_view(mtm, rect(0.0, 0.0, 100.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    // SAFETY: NSClipView's designated initializer.
    let clip: Retained<LoggingClip> =
        unsafe { msg_send![LoggingClip::alloc(mtm), initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] };
    let clip: Retained<NSClipView> = Retained::into_super(clip);
    sv.setContentView(&clip);
    let d = document(mtm, rect(0.0, 0.0, 83.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    let v = sv.verticalScroller().expect("a vertical scroller");
    clip.scrollToPoint(pt(0.0, 300.0));
    sv.reflectScrolledClipView(&clip);
    take();
    // The clip view hears of its document's frame through
    // viewFrameChanged:, and the scrollers follow even when it stays.
    d.setFrameSize(NSSize::new(83.0, 2000.0));
    assert_eq!(take(), ["viewFrameChanged NSViewFrameDidChangeNotification", "reflect 0 300"]);
    assert_eq!(v.knobProportion(), 0.075);
    d.setFrameSize(NSSize::new(83.0, 350.0));
    assert_eq!(take(), ["viewFrameChanged NSViewFrameDidChangeNotification", "reflect 0 200"]);
    assert_eq!(clip.bounds().origin, pt(0.0, 200.0));
    // Not while the document posts no frame notifications: it catches up
    // when it posts again.
    d.setFrameSize(NSSize::new(83.0, 1000.0));
    clip.scrollToPoint(pt(0.0, 750.0));
    sv.reflectScrolledClipView(&clip);
    take();
    d.setPostsFrameChangedNotifications(false);
    d.setFrameSize(NSSize::new(83.0, 0.0));
    d.setFrameSize(NSSize::new(83.0, 3000.0));
    assert!(take().is_empty());
    assert_eq!(clip.bounds().origin, pt(0.0, 750.0));
    assert_eq!(v.knobProportion(), 0.15);
    d.setPostsFrameChangedNotifications(true);
    assert_eq!(take(), ["viewFrameChanged NSViewFrameDidChangeNotification", "reflect 0 750"]);
    assert_eq!(v.knobProportion(), 0.05);
    // Cleared and filled again while not posting: the place is kept.
    d.setPostsFrameChangedNotifications(false);
    d.setFrameSize(NSSize::new(83.0, 0.0));
    d.setFrameSize(NSSize::new(83.0, 1000.0));
    d.setPostsFrameChangedNotifications(true);
    assert_eq!(clip.bounds().origin, pt(0.0, 750.0));
    // A shrink while not posting moves it only then.
    d.setPostsFrameChangedNotifications(false);
    d.setFrameSize(NSSize::new(83.0, 300.0));
    assert_eq!(clip.bounds().origin, pt(0.0, 750.0));
    d.setPostsFrameChangedNotifications(true);
    assert_eq!(clip.bounds().origin, pt(0.0, 150.0));
    take();
    // Its own size changing isn't its document's: no viewFrameChanged:.
    d.setFrameSize(NSSize::new(83.0, 1000.0));
    clip.scrollToPoint(pt(0.0, 850.0));
    take();
    sv.setFrameSize(NSSize::new(100.0, 300.0));
    assert!(!take().iter().any(|l| l.starts_with("viewFrameChanged")));
    assert_eq!(clip.bounds().origin, pt(0.0, 700.0));
}

fn losing_documents(mtm: MainThreadMarker) {
    let sv = scroll_view(mtm, rect(0.0, 0.0, 100.0, 100.0), NSScrollerStyle::Legacy);
    let d = document(mtm, rect(10.0, 20.0, 300.0, 400.0), true);
    sv.setDocumentView(Some(&d));
    let clip = sv.contentView();
    clip.scrollToPoint(pt(50.0, 60.0));
    take();
    // A document that leaves its clip view is no longer its document; the
    // bounds origin stays where its corner was, and the scroll view
    // reflects.
    d.removeFromSuperview();
    assert!(clip.documentView().is_none() && sv.documentView().is_none());
    assert_eq!(clip.bounds().origin, pt(10.0, 20.0));
    assert_eq!(take(), ["reflect 10 20"]);
    // It can come back.
    sv.setDocumentView(Some(&d));
    assert!(sv.documentView().is_some_and(|v| std::ptr::eq(&*v, &*d)));
    // SAFETY: reading the document's superview.
    assert!(unsafe { d.superview() }.is_some_and(|s| std::ptr::eq(&*s, &**clip)));
    assert_eq!(clip.bounds().origin, pt(10.0, 20.0));
    // Set to none: the same.
    clip.scrollToPoint(pt(50.0, 60.0));
    take();
    sv.setDocumentView(None);
    assert!(clip.documentView().is_none());
    assert_eq!(clip.bounds().origin, pt(10.0, 20.0));
    // Moved among the clip view's subviews by addSubview:positioned:…, it
    // stays its document; by addSubview:, which takes it out and puts it
    // back, it doesn't.
    sv.setDocumentView(Some(&d));
    let extra = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    clip.addSubview(&extra);
    let is_document = |c: &NSClipView| c.documentView().is_some_and(|v| std::ptr::eq(&*v, &*d));
    clip.addSubview_positioned_relativeTo(&d, objc2_app_kit::NSWindowOrderingMode::Above, Some(&extra));
    clip.addSubview_positioned_relativeTo(&d, objc2_app_kit::NSWindowOrderingMode::Below, Some(&extra));
    assert!(is_document(&clip));
    clip.addSubview(&d);
    assert!(clip.documentView().is_none());
    extra.removeFromSuperview();
    // Moved to another view, it isn't.
    sv.setDocumentView(Some(&d));
    let other = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    other.addSubview(&d);
    assert!(clip.documentView().is_none());
    take();
}

fn scroll_view_defaults(mtm: MainThreadMarker) {
    // SAFETY: NSScrollView's designated initializer.
    let logging: Retained<Logging> =
        unsafe { msg_send![Logging::alloc(mtm), initWithFrame: rect(0.0, 0.0, 200.0, 150.0)] };
    assert!(take().is_empty(), "made without tiling");
    let sv = Retained::into_super(logging);
    assert_eq!(sv.borderType(), NSBorderType::NoBorder);
    assert!(sv.drawsBackground() && sv.contentView().drawsBackground());
    assert!(sv.backgroundColor().isEqual(Some(&NSColor::controlBackgroundColor())));
    assert!(!sv.hasVerticalScroller() && !sv.hasHorizontalScroller());
    assert!(sv.verticalScroller().is_none() && sv.horizontalScroller().is_none());
    assert!(!sv.autohidesScrollers());
    assert_eq!(sv.scrollerKnobStyle(), NSScrollerKnobStyle::Default);
    assert_eq!(sv.scrollerStyle(), NSScroller::preferredScrollerStyle(mtm));
    #[allow(deprecated)]
    {
        assert_eq!((sv.lineScroll(), sv.verticalLineScroll(), sv.horizontalLineScroll()), (10.0, 10.0, 10.0));
        assert_eq!((sv.pageScroll(), sv.verticalPageScroll(), sv.horizontalPageScroll()), (10.0, 10.0, 10.0));
    }
    assert!(sv.scrollsDynamically() && sv.usesPredominantAxisScrolling());
    assert_eq!(sv.horizontalScrollElasticity(), NSScrollElasticity::Automatic);
    assert_eq!(sv.verticalScrollElasticity(), NSScrollElasticity::Automatic);
    assert_eq!(sv.contentInsets(), insets(0.0, 0.0, 0.0, 0.0));
    assert_eq!(sv.scrollerInsets(), insets(0.0, 0.0, 0.0, 0.0));
    assert!(sv.automaticallyAdjustsContentInsets());
    assert_eq!((sv.magnification(), sv.minMagnification(), sv.maxMagnification()), (1.0, 0.25, 4.0));
    assert!(!sv.allowsMagnification());
    assert!(sv.isFlipped() && sv.isOpaque());
    assert_eq!(sv.subviews().len(), 1);
    assert_eq!(sv.contentSize(), NSSize::new(200.0, 150.0));
    assert_eq!(sv.contentView().frame(), rect(0.0, 0.0, 200.0, 150.0));
    // Line and page amounts: both at once, or one axis.
    #[allow(deprecated)]
    {
        sv.setLineScroll(7.0);
        assert_eq!((sv.verticalLineScroll(), sv.horizontalLineScroll()), (7.0, 7.0));
        sv.setVerticalLineScroll(3.0);
        assert_eq!((sv.lineScroll(), sv.horizontalLineScroll()), (3.0, 7.0));
        sv.setPageScroll(11.0);
        assert_eq!((sv.verticalPageScroll(), sv.horizontalPageScroll()), (11.0, 11.0));
    }
    // The background is the clip view's.
    sv.setDrawsBackground(false);
    assert!(!sv.contentView().drawsBackground() && !sv.isOpaque());
    sv.setDrawsBackground(true);
    sv.setBackgroundColor(&NSColor::redColor());
    assert!(sv.contentView().backgroundColor().isEqual(Some(&NSColor::redColor())));
    sv.setBackgroundColor(&NSColor::clearColor());
    assert!(sv.isOpaque(), "opaque as long as it draws a background");
    // Magnification stays within its limits.
    sv.setMagnification(10.0);
    assert_eq!(sv.magnification(), 4.0);
    sv.setMagnification(0.1);
    assert_eq!(sv.magnification(), 0.25);
    take();
}

fn tiling(mtm: MainThreadMarker) {
    let frames = |sv: &NSScrollView| {
        (sv.contentView().frame(), sv.verticalScroller().map(|s| s.frame()), sv.horizontalScroller().map(|s| s.frame()))
    };
    // Legacy scrollers take their room from the clip view.
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    assert_eq!(take(), ["tile"]);
    assert_eq!(frames(&sv), (rect(0.0, 0.0, 183.0, 150.0), Some(rect(183.0, 0.0, 17.0, 150.0)), None));
    assert_eq!(sv.contentSize(), NSSize::new(183.0, 150.0));
    let v = sv.verticalScroller().expect("a vertical scroller");
    assert_eq!((v.scrollerStyle(), v.controlSize()), (NSScrollerStyle::Legacy, NSControlSize::Regular));
    assert!(!v.isEnabled(), "nothing to scroll yet");
    let index = |view: &NSView| sv.subviews().iter().position(|s| std::ptr::eq(&*s, view));
    assert!(index(&v).expect("a subview") > index(&sv.contentView()).expect("a subview"));
    sv.setHasHorizontalScroller(true);
    let both = (rect(0.0, 0.0, 183.0, 133.0), Some(rect(183.0, 0.0, 17.0, 133.0)), Some(rect(0.0, 133.0, 183.0, 17.0)));
    assert_eq!(frames(&sv), both);
    // Borders inset everything.
    for (border, edge) in [
        (NSBorderType::LineBorder, 1.0),
        (NSBorderType::BezelBorder, 1.0),
        (NSBorderType::GrooveBorder, 2.0),
        (NSBorderType::NoBorder, 0.0),
    ] {
        sv.setBorderType(border);
        let (w, h) = (200.0 - 2.0 * edge, 150.0 - 2.0 * edge);
        assert_eq!(
            frames(&sv),
            (
                rect(edge, edge, w - 17.0, h - 17.0),
                Some(rect(edge + w - 17.0, edge, 17.0, h - 17.0)),
                Some(rect(edge, edge + h - 17.0, w - 17.0, 17.0))
            ),
            "{border:?}"
        );
    }
    // Content insets move the scrollers in, not the clip view.
    sv.setAutomaticallyAdjustsContentInsets(false);
    sv.setContentInsets(insets(10.0, 5.0, 20.0, 7.0));
    assert_eq!(sv.contentView().contentInsets(), insets(10.0, 5.0, 20.0, 7.0));
    assert_eq!(
        frames(&sv),
        (rect(0.0, 0.0, 183.0, 133.0), Some(rect(176.0, 10.0, 17.0, 103.0)), Some(rect(5.0, 113.0, 171.0, 17.0)))
    );
    sv.setContentInsets(insets(0.0, 0.0, 0.0, 0.0));
    // So do scroller insets, once tiled.
    sv.setScrollerInsets(insets(3.0, 4.0, 5.0, 6.0));
    sv.tile();
    assert_eq!(
        frames(&sv),
        (rect(0.0, 0.0, 183.0, 133.0), Some(rect(177.0, 3.0, 17.0, 125.0)), Some(rect(4.0, 128.0, 173.0, 17.0)))
    );
    sv.setScrollerInsets(insets(0.0, 0.0, 0.0, 0.0));
    sv.tile();
    // A document: reflected, tiled, reflected.
    let d = document(mtm, rect(0.0, 0.0, 400.0, 1000.0), true);
    take();
    sv.setDocumentView(Some(&d));
    assert_eq!(take(), ["reflect 0 0", "tile", "reflect 0 0"]);
    let (v, h) = (sv.verticalScroller().expect("vertical"), sv.horizontalScroller().expect("horizontal"));
    assert_eq!((v.doubleValue(), v.knobProportion()), (0.0, 0.133));
    assert_eq!((h.doubleValue(), h.knobProportion()), (0.0, 0.4575));
    assert!(v.isEnabled() && h.isEnabled());
    // The scrollers follow once reflected.
    sv.contentView().scrollToPoint(pt(50.0, 300.0));
    assert_eq!(v.doubleValue(), 0.0);
    sv.reflectScrolledClipView(&sv.contentView());
    assert_eq!((v.doubleValue(), h.doubleValue()), (300.0 / 867.0, 50.0 / 217.0));
    // A new size tiles, and the clip view's new size reflects.
    take();
    sv.setFrameSize(NSSize::new(300.0, 250.0));
    assert_eq!(take(), ["tile", "reflect 50 300"]);
    assert_eq!(
        frames(&sv),
        (rect(0.0, 0.0, 283.0, 233.0), Some(rect(283.0, 0.0, 17.0, 233.0)), Some(rect(0.0, 233.0, 283.0, 17.0)))
    );
    // A scroller no longer had is hidden and moved out of the way.
    sv.setHasVerticalScroller(false);
    let v = sv.verticalScroller().expect("kept");
    // SAFETY: reading the scroller's superview.
    assert!(v.isHidden() && unsafe { v.superview() }.is_some());
    assert_eq!(v.frame().origin, pt(-100.0, -100.0));
    assert_eq!(sv.contentView().frame(), rect(0.0, 0.0, 300.0, 233.0));

    // Overlay scrollers lie over the clip view, full length.
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay);
    sv.setHasVerticalScroller(true);
    sv.setHasHorizontalScroller(true);
    assert_eq!(sv.verticalScroller().map(|s| s.scrollerStyle()), Some(NSScrollerStyle::Overlay));
    assert_eq!(
        frames(&sv),
        (rect(0.0, 0.0, 200.0, 150.0), Some(rect(183.0, 0.0, 17.0, 150.0)), Some(rect(0.0, 133.0, 200.0, 17.0)))
    );
    assert_eq!(sv.contentSize(), NSSize::new(200.0, 150.0));
    sv.setBorderType(NSBorderType::LineBorder);
    assert_eq!(
        frames(&sv),
        (rect(1.0, 1.0, 198.0, 148.0), Some(rect(182.0, 1.0, 17.0, 148.0)), Some(rect(1.0, 132.0, 198.0, 17.0)))
    );
    sv.setBorderType(NSBorderType::NoBorder);
    sv.setContentInsets(insets(10.0, 5.0, 20.0, 7.0));
    assert_eq!(frame_of(sv.verticalScroller()), rect(176.0, 10.0, 17.0, 120.0));
    assert_eq!(frame_of(sv.horizontalScroller()), rect(5.0, 113.0, 188.0, 17.0));
    sv.setContentInsets(insets(0.0, 0.0, 0.0, 0.0));
    let d = document(mtm, rect(0.0, 0.0, 400.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    let (v, h) = (sv.verticalScroller().expect("vertical"), sv.horizontalScroller().expect("horizontal"));
    assert_eq!((v.knobProportion(), h.knobProportion()), (0.15, 0.5));
    // A style set later applies at the next layout.
    sv.setScrollerStyle(NSScrollerStyle::Legacy);
    assert_eq!(v.scrollerStyle(), NSScrollerStyle::Legacy);
    sv.layoutSubtreeIfNeeded();
    assert_eq!(sv.contentView().frame(), rect(0.0, 0.0, 183.0, 133.0));
    take();
}

fn reflecting(mtm: MainThreadMarker) {
    // Content insets lengthen what the scrollers cover.
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay);
    sv.setHasVerticalScroller(true);
    let d = document(mtm, rect(0.0, 0.0, 400.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    sv.setAutomaticallyAdjustsContentInsets(false);
    sv.setContentInsets(insets(10.0, 5.0, 20.0, 7.0));
    let clip = sv.contentView();
    assert_eq!(clip.bounds().origin, pt(-5.0, -10.0));
    assert_eq!(clip.documentVisibleRect(), rect(-5.0, -10.0, 200.0, 150.0));
    let v = sv.verticalScroller().expect("vertical");
    clip.scrollToPoint(pt(0.0, 100.0));
    sv.reflectScrolledClipView(&clip);
    assert_eq!((v.doubleValue(), v.knobProportion()), (0.125, 150.0 / 1030.0));
    assert_eq!(sv.documentVisibleRect(), rect(0.0, 100.0, 200.0, 150.0));
    // An unflipped document: 0 is its top, where its highest y shows.
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    let d = document(mtm, rect(0.0, 0.0, 183.0, 1000.0), false);
    sv.setDocumentView(Some(&d));
    let clip = sv.contentView();
    let v = sv.verticalScroller().expect("vertical");
    assert_eq!(clip.bounds().origin, pt(0.0, 0.0));
    assert_eq!((v.doubleValue(), v.knobProportion()), (1.0, 0.15));
    clip.scrollToPoint(pt(0.0, 850.0));
    sv.reflectScrolledClipView(&clip);
    assert_eq!(v.doubleValue(), 0.0);
    take();
}

fn autohiding(mtm: MainThreadMarker) {
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    sv.setHasHorizontalScroller(true);
    let d = document(mtm, rect(0.0, 0.0, 100.0, 50.0), true);
    sv.setDocumentView(Some(&d));
    take();
    sv.setAutohidesScrollers(true);
    assert!(take().contains(&"reflect 0 0".to_string()));
    let (v, h) = (sv.verticalScroller().expect("vertical"), sv.horizontalScroller().expect("horizontal"));
    assert!(v.isHidden() && h.isHidden());
    assert_eq!(sv.contentView().frame(), rect(0.0, 0.0, 200.0, 150.0));
    // A tall document brings the vertical one back.
    d.setFrameSize(NSSize::new(100.0, 1000.0));
    assert!(!v.isHidden() && h.isHidden());
    assert_eq!(sv.contentView().frame(), rect(0.0, 0.0, 183.0, 150.0));
    assert_eq!(v.frame(), rect(183.0, 0.0, 17.0, 150.0));
    d.setFrameSize(NSSize::new(1000.0, 1000.0));
    assert!(!v.isHidden() && !h.isHidden());
    assert_eq!(sv.contentView().frame(), rect(0.0, 0.0, 183.0, 133.0));
    take();
}

fn size_helpers(mtm: MainThreadMarker) {
    let scroller_class = Some(NSScroller::class());
    for (border, edges) in [
        (NSBorderType::NoBorder, 0.0),
        (NSBorderType::LineBorder, 2.0),
        (NSBorderType::BezelBorder, 2.0),
        (NSBorderType::GrooveBorder, 2.0),
    ] {
        for (style, bar) in [(NSScrollerStyle::Legacy, 17.0), (NSScrollerStyle::Overlay, 0.0)] {
            // SAFETY: the scroller classes are NSScroller.
            let frame = unsafe {
                NSScrollView::frameSizeForContentSize_horizontalScrollerClass_verticalScrollerClass_borderType_controlSize_scrollerStyle(
                    NSSize::new(100.0, 100.0),
                    scroller_class,
                    scroller_class,
                    border,
                    NSControlSize::Regular,
                    style,
                    mtm,
                )
            };
            assert_eq!(frame, NSSize::new(100.0 + edges + bar, 100.0 + edges + bar), "{border:?} {style:?}");
            let small = if style == NSScrollerStyle::Legacy { 13.0 } else { 0.0 };
            // SAFETY: as above.
            let content = unsafe {
                NSScrollView::contentSizeForFrameSize_horizontalScrollerClass_verticalScrollerClass_borderType_controlSize_scrollerStyle(
                    NSSize::new(100.0, 100.0),
                    scroller_class,
                    None,
                    border,
                    NSControlSize::Small,
                    style,
                    mtm,
                )
            };
            assert_eq!(content, NSSize::new(100.0 - edges, 100.0 - edges - small), "{border:?} {style:?}");
        }
        // The old ones, without scrollers (with them, they follow the Mac's
        // preferred style).
        #[allow(deprecated)]
        {
            let frame = NSScrollView::frameSizeForContentSize_hasHorizontalScroller_hasVerticalScroller_borderType(
                NSSize::new(100.0, 100.0),
                false,
                false,
                border,
                mtm,
            );
            assert_eq!(frame, NSSize::new(100.0 + edges, 100.0 + edges));
            let content = NSScrollView::contentSizeForFrameSize_hasHorizontalScroller_hasVerticalScroller_borderType(
                NSSize::new(100.0, 100.0),
                false,
                false,
                border,
                mtm,
            );
            assert_eq!(content, NSSize::new(100.0 - edges, 100.0 - edges));
        }
    }
    // SAFETY: as above.
    let mini = unsafe {
        NSScrollView::frameSizeForContentSize_horizontalScrollerClass_verticalScrollerClass_borderType_controlSize_scrollerStyle(
            NSSize::new(100.0, 100.0),
            None,
            scroller_class,
            NSBorderType::NoBorder,
            NSControlSize::Mini,
            NSScrollerStyle::Legacy,
            mtm,
        )
    };
    assert_eq!(mini, NSSize::new(113.0, 100.0));
}

fn replacing_parts(mtm: MainThreadMarker) {
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    let d = document(mtm, rect(0.0, 0.0, 400.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    let old_clip = sv.contentView();
    let clip = NSClipView::initWithFrame(NSClipView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    take();
    sv.setContentView(&clip);
    assert_eq!(take(), ["tile"]);
    assert!(std::ptr::eq(&*sv.contentView(), &*clip));
    assert_eq!(clip.frame(), rect(0.0, 0.0, 183.0, 150.0));
    assert!(sv.documentView().is_none());
    // SAFETY: reading superviews.
    unsafe {
        assert!(old_clip.superview().is_none());
        assert!(d.superview().is_some_and(|s| std::ptr::eq(&*s, &**old_clip)), "the document stays with it");
    }
    // A new scroller takes the old one's place, laid out at the next tile.
    let old = sv.verticalScroller().expect("vertical");
    let new = NSScroller::initWithFrame(NSScroller::alloc(mtm), rect(0.0, 0.0, 5.0, 5.0));
    sv.setVerticalScroller(Some(&new));
    assert!(take().is_empty());
    // SAFETY: reading superviews.
    unsafe {
        assert!(old.superview().is_none());
        assert!(new.superview().is_some_and(|s| std::ptr::eq(&*s, &**sv)));
    }
    assert_eq!(new.frame(), rect(0.0, 0.0, 5.0, 5.0));
    sv.tile();
    assert_eq!(new.frame(), rect(183.0, 0.0, 17.0, 150.0));
    assert!(sv.respondsToSelector(sel!(flashScrollers)));
    sv.flashScrollers();
    take();
}

fn paging_and_keys(mtm: MainThreadMarker) {
    let w = window(mtm);
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    let d = document(mtm, rect(0.0, 0.0, 400.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    w.setContentView(Some(&sv));
    let clip = sv.contentView();
    assert_eq!(clip.frame().size, NSSize::new(383.0, 300.0));
    // Only the page actions are the scroll view's own.
    for action in [sel!(pageDown:), sel!(pageUp:)] {
        assert!(sv.respondsToSelector(action));
    }
    for action in [
        sel!(scrollPageDown:),
        sel!(scrollPageUp:),
        sel!(scrollLineDown:),
        sel!(scrollLineUp:),
        sel!(scrollToBeginningOfDocument:),
        sel!(scrollToEndOfDocument:),
    ] {
        assert!(!sv.respondsToSelector(action), "{action:?}");
    }
    // A page is the visible height less the page overlap (AppKit animates
    // it; wait for it to arrive).
    sv.setVerticalPageScroll(50.0);
    clip.scrollToPoint(pt(0.0, 300.0));
    let page = |down: bool| {
        // SAFETY: the actions take a sender and return nothing.
        unsafe {
            if down {
                let _: () = msg_send![&*sv, pageDown: None::<&AnyObject>];
            } else {
                let _: () = msg_send![&*sv, pageUp: None::<&AnyObject>];
            }
        }
    };
    page(true);
    wait_for(|| clip.bounds().origin.y == 550.0);
    assert_eq!(clip.bounds().origin, pt(0.0, 550.0));
    page(false);
    wait_for(|| clip.bounds().origin.y == 300.0);
    assert_eq!(clip.bounds().origin, pt(0.0, 300.0));
    // A key sent to the scroll view doesn't scroll it.
    for (character, code) in [(0xF72Du32, 121u16), (0xF72B, 119), (0xF701, 125), (0x20, 49)] {
        let text = NSString::from_str(&char::from_u32(character).expect("a character").to_string());
        let key = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown,
            NSPoint::ZERO,
            NSEventModifierFlags::empty(),
            0.0,
            w.windowNumber(),
            None,
            &text,
            &text,
            false,
            code,
        )
        .expect("a key event");
        sv.keyDown(&key);
        assert_eq!(clip.bounds().origin, pt(0.0, 300.0));
    }
    w.setContentView(None);
    take();
}

/// Run the main loop until `done`, for at most five seconds.
fn wait_for(done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() && std::time::Instant::now() < deadline {
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.02);
        objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&until);
    }
}

fn magnification(mtm: MainThreadMarker) {
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay);
    let d = document(mtm, rect(0.0, 0.0, 800.0, 600.0), true);
    sv.setDocumentView(Some(&d));
    sv.setAllowsMagnification(true);
    assert!(sv.allowsMagnification());
    sv.setMinMagnification(0.1);
    sv.setMaxMagnification(8.0);
    assert_eq!((sv.minMagnification(), sv.maxMagnification()), (0.1, 8.0));
    // Magnifying scales the clip view's bounds about their center.
    sv.setMagnification(2.0);
    assert_eq!(sv.magnification(), 2.0);
    assert_eq!(sv.contentView().bounds(), rect(50.0, 37.5, 100.0, 75.0));
    assert_eq!(sv.contentView().frame(), rect(0.0, 0.0, 200.0, 150.0));
    sv.setMagnification(1.0);
    // Fitting a rectangle shows it whole.
    sv.magnifyToFitRect(rect(0.0, 0.0, 400.0, 300.0));
    assert_eq!(sv.magnification(), 0.5);
    assert_eq!(sv.contentView().bounds(), rect(0.0, 0.0, 400.0, 300.0));
    sv.setMagnification_centeredAtPoint(1.0, pt(0.0, 0.0));
    assert_eq!(sv.magnification(), 1.0);
    assert_eq!(sv.contentView().bounds(), rect(0.0, 0.0, 200.0, 150.0));
    // The point stays where it is on screen.
    sv.contentView().scrollToPoint(pt(100.0, 100.0));
    sv.setMagnification_centeredAtPoint(2.0, pt(150.0, 150.0));
    assert_eq!(sv.contentView().bounds(), rect(125.0, 125.0, 100.0, 75.0));
    sv.setMagnification_centeredAtPoint(1.0, pt(150.0, 150.0));
    assert_eq!(sv.contentView().bounds(), rect(100.0, 100.0, 200.0, 150.0));
    // Without a point, the middle stays.
    sv.setMagnification(2.0);
    assert_eq!(sv.contentView().bounds(), rect(150.0, 137.5, 100.0, 75.0));
    sv.setMagnification(1.0);
    // A rectangle fitted is centered, as far as the document goes.
    sv.magnifyToFitRect(rect(200.0, 200.0, 200.0, 50.0));
    assert_eq!(sv.magnification(), 1.0);
    assert_eq!(sv.contentView().bounds(), rect(200.0, 150.0, 200.0, 150.0));
    sv.magnifyToFitRect(rect(100.0, 100.0, 400.0, 100.0));
    assert_eq!(sv.magnification(), 0.5);
    assert_eq!(sv.contentView().bounds(), rect(100.0, 0.0, 400.0, 300.0));
    sv.magnifyToFitRect(rect(700.0, 500.0, 200.0, 150.0));
    assert_eq!(sv.contentView().bounds(), rect(600.0, 450.0, 200.0, 150.0));
    take();
}

fn snapshots_draw_documents(mtm: MainThreadMarker) {
    // A document red at its top and blue below, in a scroll view without
    // a background.
    let doc = common::draw_view(mtm, rect(0.0, 0.0, 100.0, 400.0), true, |_, _| {
        NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0).setFill();
        objc2_app_kit::NSBezierPath::fillRect(rect(0.0, 0.0, 100.0, 200.0));
        NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0).setFill();
        objc2_app_kit::NSBezierPath::fillRect(rect(0.0, 200.0, 100.0, 200.0));
    });
    let sv = scroll_view(mtm, rect(0.0, 0.0, 100.0, 100.0), NSScrollerStyle::Overlay);
    sv.setDrawsBackground(false);
    sv.setDocumentView(Some(&doc));
    let shot = common::snapshot(&sv, 1.0);
    common::assert_px(&shot, 50, 50, common::RED);
    // Scrolled: what the clip view shows now.
    sv.contentView().scrollToPoint(pt(0.0, 250.0));
    let shot = common::snapshot(&sv, 1.0);
    common::assert_px(&shot, 50, 50, common::BLUE);
    // Below a short document, the clip view's background.
    let short = common::draw_view(mtm, rect(0.0, 0.0, 100.0, 40.0), true, |_, _| {
        NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 1.0, 1.0).setFill();
        objc2_app_kit::NSBezierPath::fillRect(rect(0.0, 0.0, 100.0, 40.0));
    });
    let sv = scroll_view(mtm, rect(0.0, 0.0, 100.0, 100.0), NSScrollerStyle::Overlay);
    sv.setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0));
    sv.setDocumentView(Some(&short));
    let shot = common::snapshot(&sv, 1.0);
    common::assert_px(&shot, 50, 20, common::BLUE);
    common::assert_px(&shot, 50, 80, common::RED);
    take();
}

fn notification_names(_mtm: MainThreadMarker) {
    // SAFETY: the names are AppKit's constants.
    let names = unsafe {
        [
            objc2_app_kit::NSScrollViewWillStartLiveScrollNotification,
            objc2_app_kit::NSScrollViewDidLiveScrollNotification,
            objc2_app_kit::NSScrollViewDidEndLiveScrollNotification,
            objc2_app_kit::NSScrollViewWillStartLiveMagnifyNotification,
            objc2_app_kit::NSScrollViewDidEndLiveMagnifyNotification,
            objc2_app_kit::NSPreferredScrollerStyleDidChangeNotification,
        ]
    };
    let expected = [
        "NSScrollViewWillStartLiveScrollNotification",
        "NSScrollViewDidLiveScrollNotification",
        "NSScrollViewDidEndLiveScrollNotification",
        "NSScrollViewWillStartLiveMagnifyNotification",
        "NSScrollViewDidEndLiveMagnifyNotification",
        "NSPreferredScrollerStyleDidChangeNotification",
    ];
    for (name, value) in names.iter().zip(expected) {
        assert_eq!(name.to_string(), value);
    }
}

/// What scroll wheel events do to a scroll view on macOS: one line of
/// wheel is `verticalLineScroll` points, precise deltas are points, and a
/// scroll posts `NSScrollViewDidLiveScrollNotification`, without a start
/// or an end once the scroll view has scrolled once (the first scroll is
/// bracketed, and only printed).
#[cfg(target_os = "macos")]
fn wheel_probe(mtm: MainThreadMarker) {
    #[repr(C)]
    struct CGEvent {
        _private: [u8; 0],
    }
    // SAFETY: CGEventRef is a pointer to this opaque struct.
    unsafe impl objc2::encode::RefEncode for CGEvent {
        const ENCODING_REF: objc2::encode::Encoding =
            objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("__CGEvent", &[]));
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateScrollWheelEvent2(
            source: *const std::ffi::c_void,
            units: u32,
            count: u32,
            wheel1: i32,
            wheel2: i32,
            wheel3: i32,
        ) -> *mut CGEvent;
        fn CFRelease(object: *const std::ffi::c_void);
    }
    let wheel = |dy: i32, dx: i32, pixels: bool| -> Retained<NSEvent> {
        // SAFETY: a new scroll wheel event, released after NSEvent took it.
        unsafe {
            let e = CGEventCreateScrollWheelEvent2(std::ptr::null(), if pixels { 0 } else { 1 }, 2, dy, dx, 0);
            let event: Option<Retained<NSEvent>> = msg_send![NSEvent::class(), eventWithCGEvent: e];
            CFRelease(e as *const _);
            event.expect("an event")
        }
    };
    let w = window(mtm);
    let sv = scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy);
    let d = document(mtm, rect(0.0, 0.0, 400.0, 1000.0), true);
    sv.setDocumentView(Some(&d));
    w.setContentView(Some(&sv));
    let clip = sv.contentView();
    // SAFETY: the observer outlives its registration, removed below.
    let observer: Retained<Observer> = unsafe { msg_send![Observer::alloc(mtm), init] };
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: seen: takes the notification.
    unsafe { center.addObserver_selector_name_object(&observer, sel!(seen:), None, Some(&sv)) };
    // The first scroll of a scroll view comes later, bracketed by the
    // start and end of a live scroll; then each moves at once, live.
    clip.scrollToPoint(pt(0.0, 300.0));
    sv.scrollWheel(&wheel(-1, 0, false));
    wait_for(|| clip.bounds().origin.y != 300.0);
    wait_for(|| LOG.with(|l| l.borrow().iter().any(|n| n.contains("DidEndLiveScroll"))));
    println!("wheel_probe: the first: {:?}", take());
    // A line is verticalLineScroll points; precise deltas are points.
    sv.setVerticalLineScroll(25.0);
    for (name, dy, pixels, moved) in
        [("1 line down", -1, false, 25.0), ("3 lines down", -3, false, 75.0), ("40 points down", -40, true, 40.0)]
    {
        clip.scrollToPoint(pt(0.0, 300.0));
        take();
        let e = wheel(dy, 0, pixels);
        sv.scrollWheel(&e);
        wait_for(|| clip.bounds().origin.y == 300.0 + moved);
        let heard = take();
        println!(
            "wheel_probe: {name}: precise {} delta {}: y 300 -> {}, {heard:?}",
            e.hasPreciseScrollingDeltas(),
            e.scrollingDeltaY(),
            clip.bounds().origin.y
        );
        assert_eq!(clip.bounds().origin.y, 300.0 + moved, "{name}");
        assert!(heard.contains(&"note NSScrollViewDidLiveScrollNotification".to_string()), "{name}: {heard:?}");
    }
    // SAFETY: removing the observer's registrations.
    unsafe { center.removeObserver(&observer) };
    w.setContentView(None);
}

/// A scroller's knob dragged on macOS, by events posted for AppKit's
/// tracking loop: a live scroll, moved through the scroll view.
#[cfg(target_os = "macos")]
fn tracking_probe(mtm: MainThreadMarker) {
    let w = window(mtm);
    let sv = scroll_view(mtm, rect(0.0, 0.0, 400.0, 300.0), NSScrollerStyle::Legacy);
    sv.setHasVerticalScroller(true);
    let d = document(mtm, rect(0.0, 0.0, 383.0, 1500.0), true);
    sv.setDocumentView(Some(&d));
    w.setContentView(Some(&sv));
    let clip = sv.contentView();
    // SAFETY: the observer outlives its registration, removed below.
    let observer: Retained<Observer> = unsafe { msg_send![Observer::alloc(mtm), init] };
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: seen: takes the notification.
    unsafe { center.addObserver_selector_name_object(&observer, sel!(seen:), None, Some(&sv)) };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let v = sv.verticalScroller().expect("a vertical scroller");
    // Window coordinates, y up from the content's bottom; x on the scroller.
    let event = |kind: NSEventType, y_down: f64| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            pt(391.0, 300.0 - y_down),
            NSEventModifierFlags::empty(),
            0.0,
            w.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .expect("a mouse event")
    };
    take();
    // Down on the knob (it starts 3 points down), dragged, up.
    app.postEvent_atStart(&event(NSEventType::LeftMouseDragged, 150.0), false);
    app.postEvent_atStart(&event(NSEventType::LeftMouseUp, 150.0), false);
    v.mouseDown(&event(NSEventType::LeftMouseDown, 13.0));
    wait_for(|| LOG.with(|l| l.borrow().iter().any(|n| n.contains("DidEndLiveScroll"))));
    let mut heard: Vec<String> =
        take().into_iter().filter(|n| n.contains("LiveScroll") || n.starts_with("reflect")).collect();
    println!("tracking_probe: knob drag: y {}, {heard:?}", clip.bounds().origin.y);
    // (AppKit posts the start and the end twice.)
    heard.dedup();
    let live: Vec<&str> = heard.iter().map(String::as_str).filter(|n| n.contains("LiveScroll")).collect();
    assert_eq!(
        live,
        [
            "note NSScrollViewWillStartLiveScrollNotification",
            "note NSScrollViewDidLiveScrollNotification",
            "note NSScrollViewDidEndLiveScrollNotification"
        ]
    );
    // The scrollers follow after the live scroll is posted.
    let at = |n: &str| heard.iter().position(|h| h.starts_with(n));
    assert!(at("note NSScrollViewDidLiveScrollNotification") < at("reflect"), "{heard:?}");
    assert_eq!(clip.bounds().origin.y, 698.0);
    // SAFETY: removing the observer's registrations.
    unsafe { center.removeObserver(&observer) };
    w.setContentView(None);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let tests: &[Test] = &[
        ("scroller_classes", scroller_classes),
        ("scroller_defaults", scroller_defaults),
        ("scroller_parts", scroller_parts),
        ("clip_view_defaults", clip_view_defaults),
        ("redraw_policies", redraw_policies),
        ("documents_and_constraints", documents_and_constraints),
        ("scrolling_messages", scrolling_messages),
        ("following_documents", following_documents),
        ("losing_documents", losing_documents),
        ("scroll_view_defaults", scroll_view_defaults),
        ("tiling", tiling),
        ("reflecting", reflecting),
        ("autohiding", autohiding),
        ("size_helpers", size_helpers),
        ("replacing_parts", replacing_parts),
        ("paging_and_keys", paging_and_keys),
        ("magnification", magnification),
        ("snapshots_draw_documents", snapshots_draw_documents),
        ("notification_names", notification_names),
        #[cfg(target_os = "macos")]
        ("wheel_probe", wheel_probe),
        #[cfg(target_os = "macos")]
        ("tracking_probe", tracking_probe),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        take();
        println!("test {name} ... ok");
    }
}
