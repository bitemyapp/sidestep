//! NSSplitView, checked on macOS and on Linux alike: its defaults and
//! divider thicknesses, adjusting and resizing subviews, moving dividers
//! within the range the delegate allows, collapsing, the resize
//! notifications, arranged subviews, and autosaving to the user defaults.
//! On Linux, dragging a divider with mouse events too (AppKit follows a
//! drag in a loop of its own, which a test can't feed).
//!
//! Sizes are chosen so no frame needs rounding. AppKit belongs to the main
//! thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSSplitView, NSSplitViewDelegate, NSSplitViewDividerStyle, NSView};
use objc2_foundation::{NSArray, NSNotification, NSNumber, NSPoint, NSRect, NSSize, NSString, NSUserDefaults};

use sidestep as _;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// The resize notification as a log entry: its divider and whether the
/// user moved it.
fn note(kind: &str, n: &NSNotification) -> String {
    let info = n.userInfo().expect("user info");
    let number = |key: &str| {
        info.objectForKey(&NSString::from_str(key)).map(|v| v.downcast::<NSNumber>().unwrap().integerValue())
    };
    format!("{kind} {:?} {:?}", number("NSSplitViewDividerIndex"), number("NSSplitViewUserResizeKey"))
}

struct DelegateIvars {
    collapse: bool,
}

define_class!(
    /// Logs what the split view asks, allowing everything.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceSplitDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSSplitViewDelegate for Delegate {
        #[unsafe(method(splitView:constrainMinCoordinate:ofSubviewAt:))]
        fn constrain_min(&self, _split: &NSSplitView, p: f64, i: isize) -> f64 {
            log(format!("min {p} {i}"));
            p
        }

        #[unsafe(method(splitView:constrainMaxCoordinate:ofSubviewAt:))]
        fn constrain_max(&self, _split: &NSSplitView, p: f64, i: isize) -> f64 {
            log(format!("max {p} {i}"));
            p
        }

        #[unsafe(method(splitView:constrainSplitPosition:ofSubviewAt:))]
        fn constrain_split(&self, _split: &NSSplitView, p: f64, i: isize) -> f64 {
            log(format!("split {p} {i}"));
            p
        }

        #[unsafe(method(splitView:canCollapseSubview:))]
        fn can_collapse(&self, _split: &NSSplitView, _view: &NSView) -> bool {
            self.ivars().collapse
        }

        #[unsafe(method(splitViewWillResizeSubviews:))]
        fn will_resize(&self, n: &NSNotification) {
            log(note("will", n));
        }

        #[unsafe(method(splitViewDidResizeSubviews:))]
        fn did_resize(&self, n: &NSNotification) {
            log(note("did", n));
        }
    }
);

define_class!(
    /// Resizes the subviews itself.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceSplitResizer"]
    struct Resizer;

    unsafe impl NSObjectProtocol for Resizer {}

    unsafe impl NSSplitViewDelegate for Resizer {
        #[unsafe(method(splitView:resizeSubviewsWithOldSize:))]
        fn resize(&self, split: &NSSplitView, old: NSSize) {
            log(format!("resize {} {}", old.width, split.frame().size.width));
        }
    }
);

fn delegate(mtm: MainThreadMarker, collapse: bool) -> Retained<Delegate> {
    let this = Delegate::alloc(mtm).set_ivars(DelegateIvars { collapse });
    // SAFETY: the superclass's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn view(mtm: MainThreadMarker, w: f64, h: f64) -> Retained<NSView> {
    NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, w, h))
}

/// A thin-divided vertical split, 402 wide, of views 50, 100 and 50 wide.
fn split(mtm: MainThreadMarker) -> (Retained<NSSplitView>, [Retained<NSView>; 3]) {
    let s = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), rect(0.0, 0.0, 402.0, 100.0));
    s.setDividerStyle(NSSplitViewDividerStyle::Thin);
    s.setVertical(true);
    let views = [view(mtm, 50.0, 10.0), view(mtm, 100.0, 10.0), view(mtm, 50.0, 10.0)];
    for v in &views {
        s.addSubview(v);
    }
    (s, views)
}

fn frames(views: &[Retained<NSView>]) -> Vec<NSRect> {
    views.iter().map(|v| v.frame()).collect()
}

fn defaults(mtm: MainThreadMarker) {
    let s = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), rect(0.0, 0.0, 300.0, 100.0));
    assert!(!s.isVertical());
    assert_eq!(s.dividerStyle(), NSSplitViewDividerStyle::Thick);
    assert!(s.autosaveName().is_none());
    assert!(s.arrangesAllSubviews());
    assert!(s.isFlipped());
    assert_eq!(s.arrangedSubviews().count(), 0);
    assert!(s.delegate().is_none());
    let thickness = |style| {
        s.setDividerStyle(style);
        s.dividerThickness()
    };
    assert_eq!(thickness(NSSplitViewDividerStyle::Thick), 9.0);
    assert_eq!(thickness(NSSplitViewDividerStyle::Thin), 1.0);
    assert_eq!(thickness(NSSplitViewDividerStyle::PaneSplitter), 10.0);
    // Holding priorities belong to arranged views.
    assert_eq!(s.holdingPriorityForSubviewAtIndex(0), 250.0);
    s.setHoldingPriority_forSubviewAtIndex(260.0, 1);
    assert_eq!(s.holdingPriorityForSubviewAtIndex(1), 250.0);
    let (t, _) = split(mtm);
    t.setHoldingPriority_forSubviewAtIndex(260.0, 1);
    assert_eq!((t.holdingPriorityForSubviewAtIndex(0), t.holdingPriorityForSubviewAtIndex(1)), (250.0, 260.0));
}

fn adjusting(mtm: MainThreadMarker) {
    let (s, views) = split(mtm);
    // Adding doesn't place.
    assert_eq!(views[1].frame(), rect(0.0, 0.0, 100.0, 10.0));
    assert_eq!(s.arrangedSubviews().count(), 3);
    // The room less the dividers, in proportion to their sizes.
    s.adjustSubviews();
    assert_eq!(
        frames(&views),
        [rect(0.0, 0.0, 100.0, 100.0), rect(101.0, 0.0, 200.0, 100.0), rect(302.0, 0.0, 100.0, 100.0)]
    );
    // A collapsed (hidden) view takes no room; its dividers stay.
    views[1].setHidden(true);
    assert!(s.isSubviewCollapsed(&views[1]) && !s.isSubviewCollapsed(&views[0]));
    s.adjustSubviews();
    assert_eq!((views[0].frame(), views[2].frame()), (rect(0.0, 0.0, 200.0, 100.0), rect(202.0, 0.0, 200.0, 100.0)));
    assert_eq!(views[1].frame(), rect(101.0, 0.0, 200.0, 100.0));
    views[1].setHidden(false);

    // Stacked: horizontal dividers, from the top.
    let t = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), rect(0.0, 0.0, 400.0, 152.0));
    t.setDividerStyle(NSSplitViewDividerStyle::Thin);
    let rows = [view(mtm, 10.0, 50.0), view(mtm, 10.0, 50.0), view(mtm, 10.0, 50.0)];
    for r in &rows {
        t.addSubview(r);
    }
    t.adjustSubviews();
    assert_eq!(
        frames(&rows),
        [rect(0.0, 0.0, 400.0, 50.0), rect(0.0, 51.0, 400.0, 50.0), rect(0.0, 102.0, 400.0, 50.0)]
    );
    t.setPosition_ofDividerAtIndex(10.0, 0);
    assert_eq!(frames(&rows)[..2], [rect(0.0, 0.0, 400.0, 10.0), rect(0.0, 11.0, 400.0, 90.0)]);
}

fn moving_dividers(mtm: MainThreadMarker) {
    let (s, views) = split(mtm);
    s.adjustSubviews();
    // Between the view before and the one after.
    assert_eq!((s.minPossiblePositionOfDividerAtIndex(0), s.maxPossiblePositionOfDividerAtIndex(0)), (0.0, 300.0));
    assert_eq!((s.minPossiblePositionOfDividerAtIndex(1), s.maxPossiblePositionOfDividerAtIndex(1)), (101.0, 401.0));
    s.setPosition_ofDividerAtIndex(120.0, 0);
    assert_eq!(
        frames(&views),
        [rect(0.0, 0.0, 120.0, 100.0), rect(121.0, 0.0, 180.0, 100.0), rect(302.0, 0.0, 100.0, 100.0)]
    );
    // Kept in range.
    s.setPosition_ofDividerAtIndex(500.0, 0);
    assert_eq!(frames(&views)[..2], [rect(0.0, 0.0, 300.0, 100.0), rect(301.0, 0.0, 0.0, 100.0)]);
    s.setPosition_ofDividerAtIndex(-5.0, 0);
    assert_eq!(frames(&views)[..2], [rect(0.0, 0.0, 0.0, 100.0), rect(1.0, 0.0, 300.0, 100.0)]);
    assert!(!views[0].isHidden());
    s.setPosition_ofDividerAtIndex(100.0, 0);

    // The delegate bounds the range, then the position, around the
    // notifications.
    let d = delegate(mtm, false);
    s.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    take();
    s.setPosition_ofDividerAtIndex(120.0, 0);
    assert_eq!(take(), ["min 0 0", "max 300 0", "split 120 0", "will Some(0) Some(0)", "did Some(0) Some(0)"]);
    // Out of range, it's asked about the nearest position in range.
    s.setPosition_ofDividerAtIndex(1000.0, 1);
    assert_eq!(take(), ["min 121 1", "max 401 1", "split 401 1", "will Some(1) Some(0)", "did Some(1) Some(0)"]);
    assert_eq!(views[2].frame(), rect(402.0, 0.0, 0.0, 100.0));
    s.setDelegate(None);

    // A delegate that allows it collapses a view pushed past its edge.
    let (s, views) = split(mtm);
    s.adjustSubviews();
    let d = delegate(mtm, true);
    s.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    s.setPosition_ofDividerAtIndex(500.0, 0);
    assert!(views[1].isHidden() && s.isSubviewCollapsed(&views[1]));
    assert_eq!(views[0].frame(), rect(0.0, 0.0, 300.0, 100.0));
    s.setPosition_ofDividerAtIndex(100.0, 0);
    assert!(!views[1].isHidden());
    assert_eq!(frames(&views)[..2], [rect(0.0, 0.0, 100.0, 100.0), rect(101.0, 0.0, 200.0, 100.0)]);
    s.setDelegate(None);
    take();
}

fn resizing(mtm: MainThreadMarker) {
    let (s, views) = split(mtm);
    s.adjustSubviews();
    // The change is shared in proportion to their sizes.
    s.setFrameSize(NSSize::new(802.0, 100.0));
    assert_eq!(
        frames(&views),
        [rect(0.0, 0.0, 200.0, 100.0), rect(201.0, 0.0, 400.0, 100.0), rect(602.0, 0.0, 200.0, 100.0)]
    );
    s.setFrameSize(NSSize::new(402.0, 50.0));
    assert_eq!(
        frames(&views),
        [rect(0.0, 0.0, 100.0, 50.0), rect(101.0, 0.0, 200.0, 50.0), rect(302.0, 0.0, 100.0, 50.0)]
    );
    // A delegate can do it instead.
    // SAFETY: the class's initializer.
    let r: Retained<Resizer> = unsafe { msg_send![Resizer::alloc(mtm), init] };
    s.setDelegate(Some(ProtocolObject::from_ref(&*r)));
    s.setFrameSize(NSSize::new(502.0, 50.0));
    assert_eq!(take(), ["resize 402 502"]);
    assert_eq!(views[2].frame(), rect(302.0, 0.0, 100.0, 50.0));
    s.setDelegate(None);
}

fn arranged_subviews(mtm: MainThreadMarker) {
    let t = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), rect(0.0, 0.0, 301.0, 100.0));
    t.setVertical(true);
    t.setArrangesAllSubviews(false);
    let other = view(mtm, 10.0, 10.0);
    t.addSubview(&other);
    let (x, y) = (view(mtm, 10.0, 10.0), view(mtm, 10.0, 10.0));
    t.addArrangedSubview(&x);
    t.addArrangedSubview(&y);
    assert_eq!((t.arrangedSubviews().count(), t.subviews().count()), (2, 3));
    t.adjustSubviews();
    // Thick dividers are nine points.
    assert_eq!((x.frame(), y.frame()), (rect(0.0, 0.0, 146.0, 100.0), rect(155.0, 0.0, 146.0, 100.0)));
    assert_eq!(other.frame(), rect(0.0, 0.0, 10.0, 10.0));
    t.removeArrangedSubview(&x);
    assert_eq!((t.arrangedSubviews().count(), t.subviews().count()), (1, 3));
}

fn autosaving(mtm: MainThreadMarker) {
    let defaults = NSUserDefaults::standardUserDefaults();
    let name = NSString::from_str("ConformanceSplit");
    let key = NSString::from_str("NSSplitView Subview Frames ConformanceSplit");
    defaults.removeObjectForKey(&key);
    let (s, _) = split(mtm);
    s.adjustSubviews();
    s.setAutosaveName(Some(&name));
    assert_eq!(s.autosaveName().unwrap().to_string(), "ConformanceSplit");
    s.setPosition_ofDividerAtIndex(20.0, 0);
    let saved = defaults.arrayForKey(&key).expect("saved frames");
    let saved: Vec<String> = saved.iter().map(|e| e.downcast::<NSString>().unwrap().to_string()).collect();
    assert_eq!(
        saved,
        [
            "0.000000, 0.000000, 20.000000, 100.000000, NO, NO",
            "21.000000, 0.000000, 280.000000, 100.000000, NO, NO",
            "302.000000, 0.000000, 100.000000, 100.000000, NO, NO",
        ]
    );
    // Another split view with the name takes the frames.
    let (t, views) = split(mtm);
    t.setAutosaveName(Some(&name));
    assert_eq!(
        frames(&views),
        [rect(0.0, 0.0, 20.0, 100.0), rect(21.0, 0.0, 280.0, 100.0), rect(302.0, 0.0, 100.0, 100.0)]
    );
    defaults.removeObjectForKey(&key);
    s.setAutosaveName(None);
    let _ = NSArray::<NSString>::new();
}

/// Dragging a divider with mouse events, through the window.
#[cfg(not(target_vendor = "apple"))]
fn dragging(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSWindow, NSWindowStyleMask};
    // SAFETY: a plain window, never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 402.0, 100.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    let (s, views) = split(mtm);
    w.setContentView(Some(&s));
    s.adjustSubviews();
    let d = delegate(mtm, false);
    s.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    take();
    let mouse = |kind, x: f64| {
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            NSPoint::new(x, 50.0),
            NSEventModifierFlags::empty(),
            0.0,
            w.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .expect("an event");
        w.sendEvent(&event);
    };
    // Two points into a thin divider's five: still the divider.
    mouse(NSEventType::LeftMouseDown, 102.5);
    mouse(NSEventType::LeftMouseDragged, 152.5);
    assert_eq!(frames(&views)[..2], [rect(0.0, 0.0, 150.0, 100.0), rect(151.0, 0.0, 150.0, 100.0)]);
    mouse(NSEventType::LeftMouseUp, 152.5);
    // The user moved it.
    let log = take();
    assert!(log.contains(&"will Some(0) Some(1)".to_owned()), "{log:?}");
    // Elsewhere, the views get the clicks.
    mouse(NSEventType::LeftMouseDown, 50.0);
    mouse(NSEventType::LeftMouseDragged, 80.0);
    mouse(NSEventType::LeftMouseUp, 80.0);
    assert_eq!(views[0].frame().size.width, 150.0);
    s.setDelegate(None);
    w.setContentView(None);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    #[allow(unused_mut)] // Linux adds a test.
    let mut tests: Vec<Test> = vec![
        ("defaults", defaults),
        ("adjusting", adjusting),
        ("moving_dividers", moving_dividers),
        ("resizing", resizing),
        ("arranged_subviews", arranged_subviews),
        ("autosaving", autosaving),
    ];
    #[cfg(not(target_vendor = "apple"))]
    tests.push(("dragging", dragging));
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        take();
        println!("test {name} ... ok");
    }
}
