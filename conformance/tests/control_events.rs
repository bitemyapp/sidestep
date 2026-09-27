//! Controls tracking the mouse and taking keys, checked on macOS and on
//! Linux alike with an application and a window that is never shown:
//! events are posted to the application's queue, and a control's
//! `mouseDown:` takes the rest of the click from there, as a real click's
//! would.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

type Test = (&'static str, fn(MainThreadMarker, &Setup));

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

// A cell that logs the tracking protocol as it's called.
define_class!(
    #[unsafe(super(NSActionCell, NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTrackingCell"]
    struct TrackingCell;

    impl TrackingCell {
        #[unsafe(method(startTrackingAt:inView:))]
        fn start(&self, at: NSPoint, view: &NSView) -> bool {
            log(format!("start {} {}", at.x, at.y));
            // SAFETY: NSCell's method, with its arguments.
            unsafe { msg_send![super(self), startTrackingAt: at, inView: view] }
        }

        #[unsafe(method(continueTracking:at:inView:))]
        fn continue_tracking(&self, last: NSPoint, at: NSPoint, view: &NSView) -> bool {
            log(format!("continue {} {} {} {}", last.x, last.y, at.x, at.y));
            // SAFETY: as above.
            unsafe { msg_send![super(self), continueTracking: last, at: at, inView: view] }
        }

        #[unsafe(method(stopTracking:at:inView:mouseIsUp:))]
        fn stop(&self, last: NSPoint, at: NSPoint, view: &NSView, up: bool) {
            log(format!("stop {} {} up={up}", at.x, at.y));
            // SAFETY: as above.
            unsafe { msg_send![super(self), stopTracking: last, at: at, inView: view, mouseIsUp: up] }
        }

        #[unsafe(method(highlight:withFrame:inView:))]
        fn highlight(&self, flag: bool, frame: NSRect, view: &NSView) {
            log(format!("highlight {flag}"));
            // SAFETY: as above.
            unsafe { msg_send![super(self), highlight: flag, withFrame: frame, inView: view] }
        }

        #[unsafe(method(setNextState))]
        fn set_next_state(&self) {
            log("next".into());
            // SAFETY: as above.
            unsafe { msg_send![super(self), setNextState] }
        }

        #[unsafe(method(trackMouse:inRect:ofView:untilMouseUp:))]
        fn track(&self, event: &NSEvent, frame: NSRect, view: &NSView, until_up: bool) -> bool {
            let kind = match event.r#type() {
                NSEventType::LeftMouseDown => "down",
                NSEventType::LeftMouseDragged => "drag",
                _ => "other",
            };
            log(format!("track {kind} {} {} until={until_up}", frame.size.width, frame.size.height));
            // SAFETY: as above.
            let up: bool =
                unsafe { msg_send![super(self), trackMouse: event, inRect: frame, ofView: view, untilMouseUp: until_up] };
            log(format!("tracked {up}"));
            up
        }
    }
);

fn tracking_cell(mtm: MainThreadMarker) -> Retained<TrackingCell> {
    // SAFETY: NSCell's initializer for text cells.
    unsafe { msg_send![TrackingCell::alloc(mtm), initTextCell: &*NSString::from_str("cell")] }
}

// A control that logs what reaches it besides the mouse-down.
define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceLoggingControl"]
    struct LoggingControl;

    impl LoggingControl {
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            log("control mouseUp:".into());
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, _event: &NSEvent) {
            log("control mouseDragged:".into());
        }
    }
);

// A responder that logs the keys that reach it.
define_class!(
    #[unsafe(super(NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceKeyCatcher"]
    struct KeyCatcher;

    impl KeyCatcher {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
            log(format!("next responder keyDown: {chars}"));
        }
    }
);

// A button subclass that overrides mouseDown: and calls super, as apps do.
define_class!(
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceSubclassedButton"]
    struct SubclassedButton;

    impl SubclassedButton {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            log("button mouseDown:".into());
            // SAFETY: NSButton's mouseDown: takes the event.
            unsafe { msg_send![super(self), mouseDown: event] }
            log("button mouseDown: returned".into());
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(act:))]
        fn act(&self, sender: &NSControl) {
            let state = sender.cell().map_or(-9, |c| c.state());
            log(format!("action state={state} highlighted={}", sender.isHighlighted()));
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

fn target(mtm: MainThreadMarker) -> Retained<Target> {
    // SAFETY: NSObject's initializer.
    unsafe { msg_send![Target::alloc(mtm), init] }
}

/// The application, a window never shown, and a target.
struct Setup {
    app: Retained<NSApplication>,
    window: Retained<NSWindow>,
    target: Retained<Target>,
}

fn setup(mtm: MainThreadMarker) -> Setup {
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: a titled window, never shown.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 300.0, 200.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    Setup { app, window, target: target(mtm) }
}

impl Setup {
    fn mouse(&self, kind: NSEventType, x: f64, y: f64) -> Retained<NSEvent> {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            NSPoint::new(x, y),
            NSEventModifierFlags::empty(),
            0.0,
            self.window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .expect("a mouse event")
    }

    fn post(&self, events: &[(NSEventType, f64, f64)]) {
        for &(kind, x, y) in events {
            self.app.postEvent_atStart(&self.mouse(kind, x, y), false);
        }
    }

    /// The types of the events still queued, which it takes.
    fn drain(&self) -> Vec<NSEventType> {
        let mut left = Vec::new();
        // SAFETY: the mode is a constant string.
        let mode = unsafe { NSDefaultRunLoopMode };
        while let Some(e) = self.app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, None, mode, true)
        {
            // AppKit and the system post events of their own whenever they
            // like (CI's macOS runner posted an AppKit-defined one mid-test):
            // only what a test posts counts.
            let t = e.r#type();
            if !matches!(t, NSEventType::AppKitDefined | NSEventType::SystemDefined) {
                left.push(t);
            }
        }
        left
    }

    /// A control with a tracking cell at (10, 20, 80, 30) in the content.
    fn control(&self, mtm: MainThreadMarker) -> (Retained<NSControl>, Retained<TrackingCell>) {
        let control = NSControl::initWithFrame(NSControl::alloc(mtm), rect(10.0, 20.0, 80.0, 30.0));
        let cell = tracking_cell(mtm);
        control.setCell(Some(&cell));
        // SAFETY: the target outlives the control's use of it.
        unsafe {
            control.setTarget(Some(&self.target));
            control.setAction(Some(sel!(act:)));
        }
        self.window.contentView().expect("a content view").addSubview(&control);
        (control, cell)
    }
}

const DOWN: NSEventType = NSEventType::LeftMouseDown;
const DRAG: NSEventType = NSEventType::LeftMouseDragged;
const UP: NSEventType = NSEventType::LeftMouseUp;

fn click(s: &Setup, control: &NSControl, events: &[(NSEventType, f64, f64)]) -> Vec<String> {
    s.drain();
    s.post(events);
    take_log();
    control.mouseDown(&s.mouse(DOWN, 30.0, 30.0));
    let left = s.drain();
    assert!(left.is_empty(), "events left over: {left:?}");
    take_log()
}

fn tracks_a_click(mtm: MainThreadMarker, s: &Setup) {
    let (control, cell) = s.control(mtm);
    // A plain control isn't flipped: (30, 30) in the window is (20, 10) in
    // it. The control highlights the cell, the cell tracks with the
    // control's bounds, and a release inside advances the state and sends
    // the action, still highlighted.
    assert_eq!(
        click(s, &control, &[(UP, 30.0, 30.0)]),
        [
            "highlight true",
            "track down 80 30 until=false",
            "start 20 10",
            "next",
            "action state=1 highlighted=true",
            "tracked true",
            "highlight false",
        ]
    );
    // Drags inside change nothing unless the cell is continuous.
    assert_eq!(
        click(s, &control, &[(DRAG, 31.0, 30.0), (UP, 32.0, 30.0)]),
        [
            "highlight true",
            "track down 80 30 until=false",
            "start 20 10",
            "next",
            "action state=0 highlighted=true",
            "tracked true",
            "highlight false",
        ]
    );
    // Continuous cells hear about every drag and the release.
    cell.setContinuous(true);
    assert_eq!(
        click(s, &control, &[(DRAG, 31.0, 30.0), (UP, 32.0, 30.0)]),
        [
            "highlight true",
            "track down 80 30 until=false",
            "start 20 10",
            "continue 20 10 21 10",
            "stop 22 10 up=true",
            "next",
            "action state=1 highlighted=true",
            "tracked true",
            "highlight false",
        ]
    );
    cell.setContinuous(false);
}

fn tracks_leaving(mtm: MainThreadMarker, s: &Setup) {
    let (control, cell) = s.control(mtm);
    // Leaving ends the cell's tracking; a release outside sends nothing.
    assert_eq!(
        click(s, &control, &[(DRAG, 200.0, 150.0), (UP, 200.0, 150.0)]),
        ["highlight true", "track down 80 30 until=false", "start 20 10", "tracked false", "highlight false"]
    );
    assert_eq!(cell.state(), 0);
    // Coming back in starts tracking again, from the drag.
    assert_eq!(
        click(s, &control, &[(DRAG, 200.0, 150.0), (DRAG, 31.0, 30.0), (UP, 31.0, 30.0)]),
        [
            "highlight true",
            "track down 80 30 until=false",
            "start 20 10",
            "tracked false",
            "highlight false",
            "highlight true",
            "track drag 80 30 until=false",
            "start 21 10",
            "next",
            "action state=1 highlighted=true",
            "tracked true",
            "highlight false",
        ]
    );
    // A continuous cell is told it stopped when the mouse leaves.
    cell.setContinuous(true);
    assert_eq!(
        click(s, &control, &[(DRAG, 200.0, 150.0), (UP, 200.0, 150.0)]),
        [
            "highlight true",
            "track down 80 30 until=false",
            "start 20 10",
            "stop 190 130 up=false",
            "tracked false",
            "highlight false",
        ]
    );
}

fn actions_on_mouse_down(mtm: MainThreadMarker, s: &Setup) {
    let (control, cell) = s.control(mtm);
    // A cell that acts on the mouse-down acts at once and stops tracking,
    // leaving the release for whoever wants it.
    cell.sendActionOn(NSEventMask::LeftMouseDown);
    s.drain();
    s.post(&[(UP, 30.0, 30.0)]);
    take_log();
    control.mouseDown(&s.mouse(DOWN, 30.0, 30.0));
    assert_eq!(
        take_log(),
        [
            "highlight true",
            "track down 80 30 until=false",
            "start 20 10",
            "next",
            "action state=1 highlighted=true",
            "stop 20 10 up=true",
            "tracked true",
            "highlight false",
        ]
    );
    assert_eq!(s.drain(), [UP]);
}

fn disabled_buttons_ignore_clicks(mtm: MainThreadMarker, s: &Setup) {
    let button = NSButton::initWithFrame(NSButton::alloc(mtm), rect(10.0, 20.0, 80.0, 30.0));
    button.setButtonType(NSButtonType::Switch);
    // SAFETY: the target outlives the button's use of it.
    unsafe {
        button.setTarget(Some(&s.target));
        button.setAction(Some(sel!(act:)));
    }
    s.window.contentView().expect("a content view").addSubview(&button);
    button.setEnabled(false);
    s.drain();
    s.post(&[(UP, 30.0, 30.0)]);
    take_log();
    button.mouseDown(&s.mouse(DOWN, 30.0, 30.0));
    assert!(take_log().is_empty());
    // The release is left for whoever wants it.
    assert_eq!(s.drain(), [UP]);
    assert_eq!(button.state(), 0);
}

fn buttons_track_and_keep_their_mouse_up(mtm: MainThreadMarker, s: &Setup) {
    // SAFETY: NSButton's designated initializer.
    let button: Retained<SubclassedButton> =
        unsafe { msg_send![SubclassedButton::alloc(mtm), initWithFrame: rect(10.0, 20.0, 80.0, 30.0)] };
    button.setButtonType(NSButtonType::Switch);
    // SAFETY: the target outlives the button's use of it.
    unsafe {
        button.setTarget(Some(&s.target));
        button.setAction(Some(sel!(act:)));
    }
    s.window.contentView().expect("a content view").addSubview(&button);
    assert_eq!(
        click(s, &button, &[(DRAG, 31.0, 30.0), (UP, 31.0, 30.0)]),
        ["button mouseDown:", "action state=1 highlighted=true", "button mouseDown: returned"]
    );
    assert_eq!(button.state(), 1);
    assert!(!button.isHighlighted());
    // Released outside: no action, no change.
    assert_eq!(
        click(s, &button, &[(DRAG, 200.0, 150.0), (UP, 200.0, 150.0)]),
        ["button mouseDown:", "button mouseDown: returned"]
    );
    assert_eq!(button.state(), 1);
}

fn huge_periodic_delays(mtm: MainThreadMarker, s: &Setup) {
    // A continuous button whose periodic delay is too long to be a time
    // takes a click as any other does, and ends it unhighlighted. (What
    // else happens differs: macOS 26 ends the tracking at once and leaves
    // the click's events queued; Sidestep repeats never and acts on the
    // release.)
    // SAFETY: the target outlives the button.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("OK"), Some(&s.target), Some(sel!(act:)), mtm)
    };
    button.setFrame(rect(10.0, 20.0, 80.0, 24.0));
    s.window.contentView().expect("a content view").addSubview(&button);
    button.setContinuous(true);
    for delay in [f32::MAX, f32::INFINITY] {
        button.setPeriodicDelay_interval(delay, f32::MAX);
        s.drain();
        s.post(&[(DRAG, 31.0, 30.0), (UP, 31.0, 30.0)]);
        button.mouseDown(&s.mouse(DOWN, 30.0, 30.0));
        s.drain();
        take_log();
        assert!(!button.isHighlighted(), "{delay}");
    }
}

fn radio_buttons_are_exclusive(mtm: MainThreadMarker, s: &Setup) {
    let content = s.window.contentView().expect("a content view");
    let radio = |y: f64, action| {
        // SAFETY: the target outlives the buttons.
        let b = unsafe {
            NSButton::radioButtonWithTitle_target_action(&NSString::from_str("R"), Some(&s.target), Some(action), mtm)
        };
        b.setFrame(rect(150.0, y, 80.0, 20.0));
        content.addSubview(&b);
        b
    };
    let a = radio(10.0, sel!(act:));
    let b = radio(40.0, sel!(act:));
    let c = radio(70.0, sel!(act:));
    // Same superview, different action: another group.
    let other = radio(100.0, sel!(other:));
    a.setState(1);
    other.setState(1);
    take_log();
    unsafe { b.performClick(None) };
    assert_eq!([a.state(), b.state(), c.state(), other.state()], [0, 1, 0, 1]);
    assert_eq!(take_log(), ["action state=1 highlighted=true"]);
    // Clicking the one that's on leaves it on.
    unsafe { b.performClick(None) };
    assert_eq!([a.state(), b.state(), c.state(), other.state()], [0, 1, 0, 1]);
    // So does turning one on directly.
    c.setState(1);
    assert_eq!([a.state(), b.state(), c.state(), other.state()], [0, 0, 1, 1]);
    // Turning one off leaves the group with none on.
    c.setState(0);
    assert_eq!([a.state(), b.state(), c.state(), other.state()], [0, 0, 0, 1]);
    // Radio buttons without an action belong to no group.
    // SAFETY: no target or action.
    let loose = |y: f64| {
        let b = unsafe { NSButton::radioButtonWithTitle_target_action(&NSString::from_str("L"), None, None, mtm) };
        b.setFrame(rect(10.0, y, 80.0, 20.0));
        content.addSubview(&b);
        b
    };
    let (x, y) = (loose(10.0), loose(40.0));
    x.setState(1);
    y.setState(1);
    assert_eq!([x.state(), y.state(), other.state()], [1, 1, 1]);
    y.setState(0);
    unsafe { y.performClick(None) };
    assert_eq!([x.state(), y.state()], [1, 1]);
}

#[cfg(not(target_vendor = "apple"))]
fn place(s: &Setup, view: &NSView, frame: NSRect) {
    view.setFrame(frame);
    s.window.contentView().expect("a content view").addSubview(view);
}

#[cfg(not(target_vendor = "apple"))]
fn target_control(s: &Setup, control: &NSControl) {
    // SAFETY: the target outlives the control's use of it.
    unsafe {
        control.setTarget(Some(&s.target));
        control.setAction(Some(sel!(act:)));
    }
}

// macOS 26's segmented controls, steppers, sliders and switches don't take
// the rest of a click from the event queue (they track the real mouse), so
// what follows can't be observed there. It checks what AppKit documents:
// a click chooses a segment by the tracking mode, steps a stepper, moves a
// slider's knob, turns a switch over; each sends the action.

#[cfg(not(target_vendor = "apple"))]
fn click_at(s: &Setup, control: &NSControl, x: f64, y: f64, rest: &[(NSEventType, f64, f64)]) -> Vec<String> {
    s.drain();
    s.post(rest);
    take_log();
    control.mouseDown(&s.mouse(DOWN, x, y));
    let left = s.drain();
    assert!(left.is_empty(), "events left over: {left:?}");
    take_log()
}

#[cfg(not(target_vendor = "apple"))]
fn segments_track(mtm: MainThreadMarker, s: &Setup) {
    let labels: Vec<Retained<NSString>> = ["One", "Two", "Three"].iter().map(|l| NSString::from_str(l)).collect();
    let labels = objc2_foundation::NSArray::from_retained_slice(&labels);
    // SAFETY: the target outlives the control.
    let seg = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &labels,
            NSSegmentSwitchTracking::SelectOne,
            Some(&s.target),
            Some(sel!(act:)),
            mtm,
        )
    };
    let size = seg.frame().size;
    place(s, &seg, NSRect::new(NSPoint::new(10.0, 20.0), size));
    // Each segment's middle in the window: the control is sized to fit.
    let font = seg.font().expect("a font");
    // SAFETY: the key is a constant string.
    let attrs = objc2_foundation::NSDictionary::from_slices(
        &[unsafe { NSFontAttributeName }],
        &[&*font as &objc2::runtime::AnyObject],
    );
    let widths: Vec<f64> = (0..3)
        .map(|i| {
            unsafe { seg.labelForSegment(i).expect("a label").sizeWithAttributes(Some(&attrs)) }.width.ceil() + 20.0
        })
        .collect();
    let middle = |i: usize| 10.0 + widths[..i].iter().sum::<f64>() + i as f64 + widths[i] / 2.0;
    let y = 20.0 + size.height / 2.0;
    let selected = |seg: &NSSegmentedControl| (0..3).map(|i| seg.isSelectedForSegment(i)).collect::<Vec<_>>();
    assert_eq!(click_at(s, &seg, middle(1), y, &[(UP, middle(1), y)]), ["action state=0 highlighted=false"]);
    assert_eq!(seg.selectedSegment(), 1);
    // A release on another segment chooses nothing.
    assert!(click_at(s, &seg, middle(2), y, &[(DRAG, middle(0), y), (UP, middle(0), y)]).is_empty());
    assert_eq!(seg.selectedSegment(), 1);
    // Select-any toggles.
    seg.setTrackingMode(NSSegmentSwitchTracking::SelectAny);
    click_at(s, &seg, middle(2), y, &[(UP, middle(2), y)]);
    assert_eq!(selected(&seg), [false, true, true]);
    click_at(s, &seg, middle(2), y, &[(UP, middle(2), y)]);
    assert_eq!(selected(&seg), [false, true, false]);
    // Momentary: selected for the action, then not.
    seg.setTrackingMode(NSSegmentSwitchTracking::Momentary);
    seg.setSelectedSegment(-1);
    assert_eq!(click_at(s, &seg, middle(0), y, &[(UP, middle(0), y)]).len(), 1);
    assert_eq!(seg.selectedSegment(), -1);
    // A disabled segment can't be chosen.
    seg.setTrackingMode(NSSegmentSwitchTracking::SelectOne);
    seg.setEnabled_forSegment(false, 0);
    assert!(click_at(s, &seg, middle(0), y, &[(UP, middle(0), y)]).is_empty());
    assert_eq!(seg.selectedSegment(), -1);
}

#[cfg(not(target_vendor = "apple"))]
fn steppers_track(mtm: MainThreadMarker, s: &Setup) {
    let st = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 20.0, 26.0));
    place(s, &st, rect(10.0, 20.0, 20.0, 26.0));
    target_control(s, &st);
    // No repeats in these clicks.
    st.setAutorepeat(false);
    st.setDoubleValue(5.0);
    // The upper arrow counts up (the window's y goes up).
    assert_eq!(click_at(s, &st, 20.0, 40.0, &[(UP, 20.0, 40.0)]).len(), 1);
    assert_eq!(st.doubleValue(), 6.0);
    click_at(s, &st, 20.0, 25.0, &[(UP, 20.0, 25.0)]);
    assert_eq!(st.doubleValue(), 5.0);
    // Past the top: round to the bottom, or stay.
    st.setDoubleValue(59.0);
    click_at(s, &st, 20.0, 40.0, &[(UP, 20.0, 40.0)]);
    assert_eq!(st.doubleValue(), 0.0);
    st.setValueWraps(false);
    st.setDoubleValue(59.0);
    click_at(s, &st, 20.0, 40.0, &[(UP, 20.0, 40.0)]);
    assert_eq!(st.doubleValue(), 59.0);
    st.setDoubleValue(0.0);
    click_at(s, &st, 20.0, 25.0, &[(UP, 20.0, 25.0)]);
    assert_eq!(st.doubleValue(), 0.0);
}

#[cfg(not(target_vendor = "apple"))]
fn sliders_track(mtm: MainThreadMarker, s: &Setup) {
    let sl = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(0.0, 0.0, 200.0, 21.0));
    place(s, &sl, rect(10.0, 20.0, 200.0, 21.0));
    target_control(s, &sl);
    // The knob's center goes to the mouse: along the travel, the track
    // less the 20-point knob.
    let at = |x: f64| (x - 10.0 - 10.0) / 180.0;
    let log = click_at(s, &sl, 110.0, 30.0, &[(UP, 110.0, 30.0)]);
    assert_eq!(sl.doubleValue(), 0.5);
    assert!(!log.is_empty());
    // Dragging follows the mouse, even off the track, until the release; a
    // continuous slider acts at each change.
    let log = click_at(s, &sl, 60.0, 30.0, &[(DRAG, 160.0, 30.0), (DRAG, 170.0, 60.0), (UP, 170.0, 60.0)]);
    assert_eq!(sl.doubleValue(), at(170.0));
    assert_eq!(log.len(), 3);
    click_at(s, &sl, 400.0, 30.0, &[(UP, 400.0, 30.0)]);
    assert_eq!(sl.doubleValue(), 1.0);
    // Others act once, at the release.
    sl.setContinuous(false);
    let log = click_at(s, &sl, 60.0, 30.0, &[(DRAG, 160.0, 30.0), (UP, 160.0, 30.0)]);
    assert_eq!(log.len(), 1);
    assert_eq!(sl.doubleValue(), at(160.0));
    // Asking for the release alone is the same.
    sl.setContinuous(true);
    sl.sendActionOn(NSEventMask::LeftMouseUp);
    let log = click_at(s, &sl, 60.0, 30.0, &[(DRAG, 160.0, 30.0), (DRAG, 170.0, 30.0), (UP, 170.0, 30.0)]);
    assert_eq!(log.len(), 1);
    assert_eq!(sl.doubleValue(), at(170.0));
}

#[cfg(not(target_vendor = "apple"))]
fn switches_track(mtm: MainThreadMarker, s: &Setup) {
    let sw = NSSwitch::initWithFrame(NSSwitch::alloc(mtm), rect(0.0, 0.0, 54.0, 24.0));
    place(s, &sw, rect(10.0, 20.0, 54.0, 24.0));
    target_control(s, &sw);
    assert_eq!(click_at(s, &sw, 30.0, 30.0, &[(UP, 30.0, 30.0)]).len(), 1);
    assert_eq!(sw.state(), 1);
    click_at(s, &sw, 30.0, 30.0, &[(UP, 30.0, 30.0)]);
    assert_eq!(sw.state(), 0);
    // Dragging the knob across turns it to that side.
    click_at(s, &sw, 15.0, 30.0, &[(DRAG, 60.0, 30.0), (UP, 60.0, 30.0)]);
    assert_eq!(sw.state(), 1);
    // Dragging it back past halfway turns it off; a release off it after a
    // drag still counts.
    click_at(s, &sw, 50.0, 30.0, &[(DRAG, 5.0, 30.0), (UP, 5.0, 60.0)]);
    assert_eq!(sw.state(), 0);
}

/// With no window on screen, tracking takes what was posted and ends when
/// it runs out, rather than waiting for input that can't come.
#[cfg(not(target_vendor = "apple"))]
fn tracking_ends_headless(mtm: MainThreadMarker, s: &Setup) {
    // SAFETY: the target outlives the button.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("OK"), Some(&s.target), Some(sel!(act:)), mtm)
    };
    place(s, &button, rect(10.0, 20.0, 80.0, 24.0));
    assert!(click_at(s, &button, 30.0, 30.0, &[]).is_empty());
    assert!(!button.isHighlighted());
    // A held stepper steps once, and doesn't repeat.
    let st = NSStepper::initWithFrame(NSStepper::alloc(mtm), rect(0.0, 0.0, 20.0, 26.0));
    place(s, &st, rect(100.0, 20.0, 20.0, 26.0));
    target_control(s, &st);
    assert!(st.autorepeat());
    assert_eq!(click_at(s, &st, 110.0, 40.0, &[]).len(), 1);
    assert_eq!(st.doubleValue(), 1.0);
    // A continuous button doesn't repeat either.
    button.setContinuous(true);
    assert!(click_at(s, &button, 30.0, 30.0, &[]).is_empty());
    let sw = NSSwitch::initWithFrame(NSSwitch::alloc(mtm), rect(0.0, 0.0, 54.0, 24.0));
    place(s, &sw, rect(150.0, 20.0, 54.0, 24.0));
    target_control(s, &sw);
    assert!(click_at(s, &sw, 170.0, 30.0, &[]).is_empty());
    assert_eq!(sw.state(), 0);
}

fn key(s: &Setup, chars: &str, flags: NSEventModifierFlags, code: u16) -> Retained<NSEvent> {
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        flags,
        0.0,
        s.window.windowNumber(),
        None,
        &NSString::from_str(chars),
        &NSString::from_str(chars),
        false,
        code,
    )
    .expect("a key event")
}

fn key_equivalents(mtm: MainThreadMarker, s: &Setup) {
    let content = s.window.contentView().expect("a content view");
    // SAFETY: the target outlives the buttons.
    let (ok, cancel) = unsafe {
        (
            NSButton::buttonWithTitle_target_action(&NSString::from_str("OK"), Some(&s.target), Some(sel!(act:)), mtm),
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Cancel"),
                Some(&s.target),
                Some(sel!(act:)),
                mtm,
            ),
        )
    };
    ok.setKeyEquivalent(&NSString::from_str("\r"));
    cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    content.addSubview(&ok);
    content.addSubview(&cancel);
    let none = NSEventModifierFlags::empty();
    take_log();
    // A button performs its own key equivalent, clicking itself.
    assert!(ok.performKeyEquivalent(&key(s, "\r", none, 36)));
    assert_eq!(take_log(), ["action state=1 highlighted=true"]);
    assert!(!ok.performKeyEquivalent(&key(s, "x", none, 7)));
    // The modifiers must match.
    assert!(!ok.performKeyEquivalent(&key(s, "\r", NSEventModifierFlags::Command, 36)));
    assert!(take_log().is_empty());
    // The window finds it in its views.
    assert!(s.window.performKeyEquivalent(&key(s, "\u{1b}", none, 53)));
    assert_eq!(take_log(), ["action state=1 highlighted=true"]);
    // Not when disabled or hidden.
    cancel.setEnabled(false);
    assert!(!cancel.performKeyEquivalent(&key(s, "\u{1b}", none, 53)));
    cancel.setEnabled(true);
    cancel.setHidden(true);
    assert!(!s.window.performKeyEquivalent(&key(s, "\u{1b}", none, 53)));
    assert!(take_log().is_empty());
    // A key equivalent with a modifier.
    let save = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("Save"), Some(&s.target), Some(sel!(act:)), mtm)
    };
    save.setKeyEquivalent(&NSString::from_str("s"));
    save.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
    content.addSubview(&save);
    assert!(!save.performKeyEquivalent(&key(s, "s", none, 1)));
    assert!(save.performKeyEquivalent(&key(s, "s", NSEventModifierFlags::Command, 1)));
    assert_eq!(take_log().len(), 1);
    // Shift needn't match: the characters say it was down. An uppercase
    // key equivalent is how a Shift equivalent is written.
    let shift = NSEventModifierFlags::Shift;
    assert!(ok.performKeyEquivalent(&key(s, "\r", shift, 36)));
    // (Clicked again, the button's state goes back.)
    ok.setState(1);
    save.setKeyEquivalent(&NSString::from_str("S"));
    assert!(save.performKeyEquivalent(&key(s, "S", NSEventModifierFlags::Command | shift, 1)));
    assert!(!save.performKeyEquivalent(&key(
        s,
        "S",
        NSEventModifierFlags::Command | NSEventModifierFlags::Option | shift,
        1
    )));
    // Nor does a Shift in the mask.
    save.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | shift);
    assert!(save.performKeyEquivalent(&key(s, "S", NSEventModifierFlags::Command, 1)));
    assert!(save.performKeyEquivalent(&key(s, "S", NSEventModifierFlags::Command | shift, 1)));
    assert_eq!(take_log().len(), 4);
    // The window's default button is the one Return clicks.
    let default = s.window.defaultButtonCell().expect("a default button");
    assert!(std::ptr::eq(&*default as &NSCell, &*ok.cell().expect("a cell")));
    // Return reaching the window clicks it, whoever's first responder;
    // Escape clicks the button whose key equivalent it is.
    take_log();
    s.window.keyDown(&key(s, "\r", none, 36));
    assert_eq!(take_log(), ["action state=0 highlighted=true"]);
    s.window.makeFirstResponder(None);
    s.window.sendEvent(&key(s, "\r", none, 36));
    assert_eq!(take_log(), ["action state=1 highlighted=true"]);
    cancel.setHidden(false);
    s.window.keyDown(&key(s, "\u{1b}", none, 53));
    assert_eq!(take_log(), ["action state=0 highlighted=true"]);
    // Shift-Return is Return to the window, too.
    s.window.keyDown(&key(s, "\r", NSEventModifierFlags::Shift, 36));
    assert_eq!(take_log(), ["action state=0 highlighted=true"]);
    // Keys the window's buttons don't take go on up the responder chain.
    let next: Retained<KeyCatcher> = unsafe { msg_send![KeyCatcher::alloc(mtm), init] };
    // SAFETY: the responder outlives its place in the chain.
    unsafe { s.window.setNextResponder(Some(&next)) };
    s.window.keyDown(&key(s, "x", none, 7));
    assert_eq!(take_log(), ["next responder keyDown: x"]);
    // SAFETY: nil ends the chain.
    unsafe { s.window.setNextResponder(None) };
}

fn first_responders(mtm: MainThreadMarker, s: &Setup) {
    let content = s.window.contentView().expect("a content view");
    let label = NSTextField::labelWithString(&NSString::from_str("Label"), mtm);
    let field = NSTextField::textFieldWithString(&NSString::from_str("Field"), mtm);
    content.addSubview(&label);
    content.addSubview(&field);
    // Labels don't take the keyboard of their own accord; fields do. (A
    // field that takes it hands it to its field editor, so which object
    // is then first responder isn't checked.)
    assert!(!label.acceptsFirstResponder());
    assert!(s.window.makeFirstResponder(Some(&label)));
    assert!(field.acceptsFirstResponder());
    assert!(s.window.makeFirstResponder(Some(&field)));
    let disabled = NSTextField::textFieldWithString(&NSString::from_str("Off"), mtm);
    disabled.setEnabled(false);
    assert!(!disabled.acceptsFirstResponder());
    // Views draw the default focus ring unless told not to.
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert_eq!(v.focusRingType(), NSFocusRingType::Default);
    v.setFocusRingType(NSFocusRingType::None);
    assert_eq!(v.focusRingType(), NSFocusRingType::None);
    assert_eq!(v.focusRingMaskBounds(), NSRect::ZERO);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("tracks_a_click", tracks_a_click),
        ("tracks_leaving", tracks_leaving),
        ("actions_on_mouse_down", actions_on_mouse_down),
        ("disabled_buttons_ignore_clicks", disabled_buttons_ignore_clicks),
        ("buttons_track_and_keep_their_mouse_up", buttons_track_and_keep_their_mouse_up),
        ("huge_periodic_delays", huge_periodic_delays),
        ("radio_buttons_are_exclusive", radio_buttons_are_exclusive),
        ("key_equivalents", key_equivalents),
        ("first_responders", first_responders),
        #[cfg(not(target_vendor = "apple"))]
        ("segments_track", segments_track),
        #[cfg(not(target_vendor = "apple"))]
        ("steppers_track", steppers_track),
        #[cfg(not(target_vendor = "apple"))]
        ("sliders_track", sliders_track),
        #[cfg(not(target_vendor = "apple"))]
        ("switches_track", switches_track),
        #[cfg(not(target_vendor = "apple"))]
        ("tracking_ends_headless", tracking_ends_headless),
    ];
    // One window for every test: AppKit finds a window that was never
    // shown by its number only while it's the first such window.
    let s = setup(mtm);
    for (name, test) in tests {
        // Each test starts with an empty window.
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        s.window.setContentView(Some(&content));
        objc2::rc::autoreleasepool(|_| test(mtm, &s));
        println!("test {name} ... ok");
    }
}
