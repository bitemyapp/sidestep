//! AppKit's event loop and window behaviour, checked on macOS and on Linux
//! alike without showing a window: run-loop modes, nested and main loops,
//! the event queue, application and window notifications and delegates,
//! termination, view frame and bounds notifications, responder rules and
//! the key view loop. Input is made with `NSEvent`'s constructors and sent
//! through `-[NSWindow sendEvent:]` or posted.
//!
//! AppKit belongs to the main thread, so this file has its own `main`. The
//! application runs with the accessory activation policy and is never
//! activated.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSNotification, NSNotificationCenter, NSPoint, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSSize, NSString, NSTimer, NSUserDefaults,
};

use sidestep as _;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn titled() -> NSWindowStyleMask {
    NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
}

/// A window that is never shown.
fn window(mtm: MainThreadMarker, style: NSWindowStyleMask) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            style,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

fn app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app
}

/// An application-defined event carrying `data1`.
fn app_event(data1: isize) -> Retained<NSEvent> {
    NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        data1,
        0,
    )
    .expect("an application-defined event")
}

fn post(data1: isize) {
    let mtm = MainThreadMarker::new().expect("main thread");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&app_event(data1), false);
}

/// A timer that calls `f` once after `seconds`, in `mode`.
fn timer_in(mode: &NSString, seconds: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(seconds, false, &block) };
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, mode) };
    timer
}

/// Run the current loop in `mode` until `done` or `seconds` pass.
fn run_mode_until(mode: &NSString, seconds: f64, done: impl Fn() -> bool) {
    let limit = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    while !done() && std::time::Instant::now() < limit {
        let date = NSDate::dateWithTimeIntervalSinceNow(0.02);
        NSRunLoop::currentRunLoop().runMode_beforeDate(mode, &date);
    }
}

/// Drop events tests left queued.
fn discard_app_events(app: &NSApplication) {
    app.discardEventsMatchingMask_beforeEvent(NSEventMask::ApplicationDefined, None);
}

fn counter() -> (Rc<Cell<u32>>, Rc<Cell<u32>>) {
    let c = Rc::new(Cell::new(0));
    (c.clone(), c)
}

// Run-loop modes.

fn mode_names(_: MainThreadMarker) {
    unsafe {
        assert_eq!(NSEventTrackingRunLoopMode.to_string(), "NSEventTrackingRunLoopMode");
        assert_eq!(NSModalPanelRunLoopMode.to_string(), "NSModalPanelRunLoopMode");
    }
}

fn the_application_adds_its_modes_to_the_common_modes(mtm: MainThreadMarker) {
    let _app = app(mtm);
    for mode in unsafe { [NSEventTrackingRunLoopMode, NSModalPanelRunLoopMode] } {
        let (fired, f) = counter();
        let timer = timer_in(unsafe { NSRunLoopCommonModes }, 0.01, move || f.set(f.get() + 1));
        run_mode_until(mode, 2.0, || fired.get() > 0);
        assert_eq!(fired.get(), 1, "a common-mode timer fires in {mode}");
        timer.invalidate();
    }
}

fn tracking_timers_fire_only_in_tracking_loops(mtm: MainThreadMarker) {
    let app = app(mtm);
    discard_app_events(&app);
    let (fired, f) = counter();
    let timer = timer_in(unsafe { NSEventTrackingRunLoopMode }, 0.02, move || {
        f.set(f.get() + 1);
        post(7);
    });
    // The default mode doesn't fire it.
    run_mode_until(unsafe { NSDefaultRunLoopMode }, 0.15, || false);
    assert_eq!(fired.get(), 0);
    // A loop looking for events in the tracking mode does, and takes the
    // event it posts.
    let until = NSDate::dateWithTimeIntervalSinceNow(2.0);
    let mode = unsafe { NSEventTrackingRunLoopMode };
    let event =
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, Some(&until), mode, true);
    assert_eq!(fired.get(), 1);
    assert_eq!(event.map(|e| e.data1()), Some(7));
    timer.invalidate();
}

fn default_timers_wait_while_tracking(mtm: MainThreadMarker) {
    let app = app(mtm);
    discard_app_events(&app);
    let (fired, f) = counter();
    let timer = timer_in(unsafe { NSDefaultRunLoopMode }, 0.01, move || f.set(f.get() + 1));
    let until = NSDate::dateWithTimeIntervalSinceNow(0.15);
    let mode = unsafe { NSEventTrackingRunLoopMode };
    let event =
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, Some(&until), mode, true);
    assert!(event.is_none());
    assert_eq!(fired.get(), 0);
    // It fires once the default mode runs.
    run_mode_until(unsafe { NSDefaultRunLoopMode }, 2.0, || fired.get() > 0);
    assert_eq!(fired.get(), 1);
    timer.invalidate();
}

fn a_window_tracks_events_in_the_tracking_mode(mtm: MainThreadMarker) {
    let app = app(mtm);
    discard_app_events(&app);
    let w = window(mtm, titled());
    // Whichever mode the window's loop runs in posts its number.
    let tracking = timer_in(unsafe { NSEventTrackingRunLoopMode }, 0.05, || post(1));
    let default = timer_in(unsafe { NSDefaultRunLoopMode }, 0.5, || post(2));
    let event = w.nextEventMatchingMask(NSEventMask::ApplicationDefined).expect("an event");
    assert_eq!(event.data1(), 1);
    tracking.invalidate();
    default.invalidate();
    discard_app_events(&app);
}

fn nested_loops_leave_other_events_queued(mtm: MainThreadMarker) {
    let app = app(mtm);
    discard_app_events(&app);
    let w = window(mtm, titled());
    let key = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        1.0,
        w.windowNumber(),
        None,
        &NSString::from_str("a"),
        &NSString::from_str("a"),
        false,
        0,
    )
    .unwrap();
    app.postEvent_atStart(&key, false);
    app.postEvent_atStart(&app_event(3), false);
    let mode = unsafe { NSEventTrackingRunLoopMode };
    // A loop asking for application-defined events skips the key down…
    let e = app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, None, mode, true);
    assert_eq!(e.map(|e| e.data1()), Some(3));
    // …which is still there for the next.
    let e = app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::KeyDown, None, mode, true);
    assert_eq!(e.map(|e| e.r#type()), Some(NSEventType::KeyDown));
    // Taking an event makes it the current event.
    assert!(app.currentEvent().is_some_and(|c| c.r#type() == NSEventType::KeyDown));
}

/// CoreFoundation observers and blocks on the main loop see AppKit's
/// loops: a program's observers keep working while AppKit tracks or runs
/// modally.
fn run_loop_observers_see_appkit_loops(mtm: MainThreadMarker) {
    use objc2_core_foundation::{CFRunLoop, CFRunLoopActivity, CFRunLoopObserver, kCFRunLoopCommonModes};
    let app = app(mtm);
    discard_app_events(&app);
    let seen: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    let handler = RcBlock::new(move |_: *mut CFRunLoopObserver, activity: CFRunLoopActivity| {
        let mode = NSRunLoop::currentRunLoop().currentMode().map(|m| m.to_string()).unwrap_or_default();
        log.borrow_mut().push(format!("{} {mode}", activity.0));
    });
    let entry_waiting_exit = 1 | 32 | 128;
    let observer =
        unsafe { CFRunLoopObserver::with_handler(None, entry_waiting_exit, true, 0, Some(&handler)) }.unwrap();
    let main = CFRunLoop::main().unwrap();
    let common = unsafe { kCFRunLoopCommonModes };
    main.add_observer(Some(&observer), common);
    let log = seen.clone();
    let block = RcBlock::new(move || log.borrow_mut().push("block".into()));
    unsafe { main.perform_block(common.map(|c| c.as_ref()), Some(&block)) };
    let until = NSDate::dateWithTimeIntervalSinceNow(0.1);
    let mode = unsafe { NSEventTrackingRunLoopMode };
    let _ =
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, Some(&until), mode, true);
    main.remove_observer(Some(&observer), common);
    let seen = seen.borrow();
    for expected in
        ["1 NSEventTrackingRunLoopMode", "32 NSEventTrackingRunLoopMode", "128 NSEventTrackingRunLoopMode", "block"]
    {
        assert!(seen.iter().any(|s| s == expected), "{expected} in {seen:?}");
    }
}

// Notifications and delegates.

thread_local!(static NOTES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn note(line: String) {
    if PRINT_NOTES.with(Cell::get) {
        println!("{line}");
    } else {
        NOTES.with(|n| n.borrow_mut().push(line));
    }
}

fn take_notes() -> Vec<String> {
    NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

define_class!(
    /// Observes notifications with `seen:`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsObserver"]
    struct Observer;

    impl Observer {
        #[unsafe(method(seen:))]
        fn seen(&self, n: &NSNotification) {
            note(format!("observer {}", n.name()));
        }
    }
);

impl Observer {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(())), init] }
    }

    fn watch(&self, name: &NSString, object: Option<&AnyObject>) {
        let center = NSNotificationCenter::defaultCenter();
        unsafe { center.addObserver_selector_name_object(self, sel!(seen:), Some(name), object) };
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(self) };
    }
}

define_class!(
    /// A window and application delegate that writes down what it hears.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsDelegate"]
    #[ivars = Cell<usize>]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, n: &NSNotification) {
            note(format!("delegate {}", n.name()));
        }

        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, n: &NSNotification) {
            note(format!("delegate {}", n.name()));
        }

        #[unsafe(method(windowDidMove:))]
        fn window_did_move(&self, n: &NSNotification) {
            note(format!("delegate {}", n.name()));
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, n: &NSNotification) {
            let object = n.object().map(|o| o.class().name().to_str().unwrap().to_string());
            note(format!("delegate {} from {}", n.name(), object.unwrap_or_default()));
        }
    }

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidBecomeActive:))]
        fn application_did_become_active(&self, n: &NSNotification) {
            note(format!("delegate {}", n.name()));
        }

        #[unsafe(method(applicationWillFinishLaunching:))]
        fn application_will_finish_launching(&self, n: &NSNotification) {
            let running = NSApplication::sharedApplication(self.mtm()).isRunning();
            note(format!("delegate {} running={running}", n.name()));
        }

        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, n: &NSNotification) {
            let running = NSApplication::sharedApplication(self.mtm()).isRunning();
            note(format!("delegate {} running={running}", n.name()));
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn application_should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            let mode = NSRunLoop::currentRunLoop().currentMode().map(|m| m.to_string()).unwrap_or_default();
            note(format!("delegate applicationShouldTerminate: in {mode:?}"));
            NSApplicationTerminateReply(self.ivars().get())
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, n: &NSNotification) {
            note(format!("delegate {}", n.name()));
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(Cell::new(1))), init] }
    }
}

macro_rules! names {
    ($($name:ident),* $(,)?) => {
        [$((unsafe { $name }, stringify!($name))),*]
    };
}

// Two names are deprecated, but still exported.
#[allow(deprecated)]
fn notification_names(_: MainThreadMarker) {
    let names = names![
        NSWindowDidBecomeKeyNotification,
        NSWindowDidBecomeMainNotification,
        NSWindowDidChangeBackingPropertiesNotification,
        NSWindowDidChangeOcclusionStateNotification,
        NSWindowDidChangeScreenNotification,
        NSWindowDidChangeScreenProfileNotification,
        NSWindowDidDeminiaturizeNotification,
        NSWindowDidEndLiveResizeNotification,
        NSWindowDidEndSheetNotification,
        NSWindowDidEnterFullScreenNotification,
        NSWindowDidEnterVersionBrowserNotification,
        NSWindowDidExitFullScreenNotification,
        NSWindowDidExitVersionBrowserNotification,
        NSWindowDidExposeNotification,
        NSWindowDidMiniaturizeNotification,
        NSWindowDidMoveNotification,
        NSWindowDidResignKeyNotification,
        NSWindowDidResignMainNotification,
        NSWindowDidResizeNotification,
        NSWindowDidUpdateNotification,
        NSWindowWillBeginSheetNotification,
        NSWindowWillCloseNotification,
        NSWindowWillEnterFullScreenNotification,
        NSWindowWillEnterVersionBrowserNotification,
        NSWindowWillExitFullScreenNotification,
        NSWindowWillExitVersionBrowserNotification,
        NSWindowWillMiniaturizeNotification,
        NSWindowWillMoveNotification,
        NSWindowWillStartLiveResizeNotification,
        NSApplicationDidBecomeActiveNotification,
        NSApplicationDidChangeOcclusionStateNotification,
        NSApplicationDidChangeScreenParametersNotification,
        NSApplicationDidFinishLaunchingNotification,
        NSApplicationDidHideNotification,
        NSApplicationDidResignActiveNotification,
        NSApplicationDidUnhideNotification,
        NSApplicationDidUpdateNotification,
        NSApplicationProtectedDataDidBecomeAvailableNotification,
        NSApplicationProtectedDataWillBecomeUnavailableNotification,
        NSApplicationWillBecomeActiveNotification,
        NSApplicationWillFinishLaunchingNotification,
        NSApplicationWillHideNotification,
        NSApplicationWillResignActiveNotification,
        NSApplicationWillTerminateNotification,
        NSApplicationWillUnhideNotification,
        NSApplicationWillUpdateNotification,
        NSViewFrameDidChangeNotification,
        NSViewBoundsDidChangeNotification,
        NSViewFocusDidChangeNotification,
        NSViewGlobalFrameDidChangeNotification,
        NSViewDidUpdateTrackingAreasNotification,
        NSBackingPropertyOldScaleFactorKey,
        NSBackingPropertyOldColorSpaceKey,
    ];
    for (value, name) in names {
        assert_eq!(value.to_string(), name);
    }
}

fn delegates_hear_through_the_notification_center(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    let center = NSNotificationCenter::defaultCenter();
    let observer = Observer::new(mtm);
    observer.watch(unsafe { NSWindowDidResizeNotification }, Some(&w));
    let delegate = Delegate::new(mtm);
    w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // Posting the window's notification by hand reaches its delegate, after
    // the observer registered before it.
    unsafe { center.postNotificationName_object(NSWindowDidResizeNotification, Some(&w)) };
    assert_eq!(take_notes(), ["observer NSWindowDidResizeNotification", "delegate NSWindowDidResizeNotification"]);
    unsafe { center.postNotificationName_object(NSWindowDidBecomeKeyNotification, Some(&w)) };
    assert_eq!(take_notes(), ["delegate NSWindowDidBecomeKeyNotification from NSWindow"]);
    // Only the window's own.
    unsafe { center.postNotificationName_object(NSWindowDidBecomeKeyNotification, None) };
    let other = window(mtm, titled());
    unsafe { center.postNotificationName_object(NSWindowDidBecomeKeyNotification, Some(&other)) };
    assert_eq!(take_notes(), Vec::<String>::new());
    // Setting the delegate again doesn't make it hear twice.
    w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    unsafe { center.postNotificationName_object(NSWindowWillCloseNotification, Some(&w)) };
    assert_eq!(take_notes(), ["delegate NSWindowWillCloseNotification"]);
    // Without a delegate, only the observer hears.
    w.setDelegate(None);
    unsafe { center.postNotificationName_object(NSWindowDidResizeNotification, Some(&w)) };
    assert_eq!(take_notes(), ["observer NSWindowDidResizeNotification"]);

    // The application's delegate the same way.
    let app = app(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    unsafe { center.postNotificationName_object(NSApplicationDidBecomeActiveNotification, Some(&app)) };
    unsafe { center.postNotificationName_object(NSApplicationDidBecomeActiveNotification, None) };
    assert_eq!(take_notes(), ["delegate NSApplicationDidBecomeActiveNotification"]);
    app.setDelegate(None);
    unsafe { center.postNotificationName_object(NSApplicationDidBecomeActiveNotification, Some(&app)) };
    assert_eq!(take_notes(), Vec::<String>::new());
}

fn application_delegates_are_weak(mtm: MainThreadMarker) {
    let app = app(mtm);
    objc2::rc::autoreleasepool(|_| {
        let delegate = Delegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        assert!(app.delegate().is_some());
    });
    assert!(app.delegate().is_none());
}

fn moving_and_resizing_windows(mtm: MainThreadMarker) {
    let w = window(mtm, titled());
    let delegate = Delegate::new(mtm);
    w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let resized = "delegate NSWindowDidResizeNotification";
    let moved = "delegate NSWindowDidMoveNotification";
    w.setContentSize(NSSize::new(250.0, 120.0));
    assert_eq!(take_notes(), [resized]);
    w.setFrameOrigin(NSPoint::new(10.0, 20.0));
    assert_eq!(take_notes(), [moved]);
    // A new size and origin at once is a resize only.
    w.setFrame_display(rect(30.0, 40.0, 300.0, 200.0), false);
    assert_eq!(take_notes(), [resized]);
    w.setFrame_display(rect(50.0, 60.0, 300.0, 200.0), false);
    assert_eq!(take_notes(), [moved]);
    w.setFrame_display(w.frame(), false);
    w.setFrameOrigin(w.frame().origin);
    assert_eq!(take_notes(), Vec::<String>::new());
    w.setFrameTopLeftPoint(NSPoint::new(0.0, 500.0));
    assert_eq!(take_notes(), [moved]);
    w.setDelegate(None);
}

fn closing_tells_and_releases(mtm: MainThreadMarker) {
    let delegate = Delegate::new(mtm);
    let observer = Observer::new(mtm);
    // A window released when closed (the default) gives up a reference.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 200.0, 100.0),
            titled(),
            NSBackingStoreType::Buffered,
            true,
        )
    };
    assert!(w.isReleasedWhenClosed());
    observer.watch(unsafe { NSWindowWillCloseNotification }, Some(&w));
    w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The reference the close gives up, which this test keeps for it.
    let given_up = w.clone();
    let before = w.retainCount();
    objc2::rc::autoreleasepool(|_| w.close());
    assert_eq!(w.retainCount(), before - 1);
    std::mem::forget(given_up);
    // Observers hear, even of a window that was never shown.
    assert_eq!(take_notes(), ["observer NSWindowWillCloseNotification", "delegate NSWindowWillCloseNotification"]);
    w.setDelegate(None);
    drop(observer);

    // One kept by the program isn't released.
    let kept = window(mtm, titled());
    let before = kept.retainCount();
    objc2::rc::autoreleasepool(|_| kept.close());
    assert_eq!(kept.retainCount(), before);
}

fn terminating_asks_the_delegate(mtm: MainThreadMarker) {
    let app = app(mtm);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // Cancel: terminate: returns.
    delegate.ivars().set(NSApplicationTerminateReply::TerminateCancel.0);
    app.terminate(None);
    assert_eq!(take_notes(), ["delegate applicationShouldTerminate: in \"\""]);
    // Later: terminate: waits, running the loop in the modal panel mode,
    // until the reply.
    delegate.ivars().set(NSApplicationTerminateReply::TerminateLater.0);
    let replier = timer_in(unsafe { NSRunLoopCommonModes }, 0.05, || {
        let mode = NSRunLoop::currentRunLoop().currentMode().map(|m| m.to_string()).unwrap_or_default();
        note(format!("replying in {mode:?}"));
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        app.replyToApplicationShouldTerminate(false);
    });
    app.terminate(None);
    note("terminate: returned".into());
    assert_eq!(
        take_notes(),
        [
            "delegate applicationShouldTerminate: in \"\"",
            "replying in \"NSModalPanelRunLoopMode\"",
            "terminate: returned",
        ]
    );
    replier.invalidate();
    // A reply nobody waits for does nothing.
    app.replyToApplicationShouldTerminate(true);
    app.setDelegate(None);
}

/// Run this test binary again with `child` as its argument, and return its
/// status and output.
fn in_child(child: &str) -> (std::process::ExitStatus, String) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .arg(child)
        .output()
        .expect("the test binary runs again");
    (out.status, String::from_utf8_lossy(&out.stdout).into_owned())
}

fn terminating_now_exits(_: MainThreadMarker) {
    let (status, out) = in_child("--child-terminate-now");
    assert!(status.success(), "{status:?} {out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        [
            "delegate applicationShouldTerminate: in \"\"",
            "observer NSApplicationWillTerminateNotification",
            "delegate NSApplicationWillTerminateNotification",
        ]
    );
}

fn child_terminate_now(mtm: MainThreadMarker) {
    let app = app(mtm);
    let observer = Observer::new(mtm);
    observer.watch(unsafe { NSApplicationWillTerminateNotification }, None);
    let delegate = Delegate::new(mtm);
    delegate.ivars().set(NSApplicationTerminateReply::TerminateNow.0);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // Notes are printed as they are made, as the process ends inside
    // terminate:.
    PRINT_NOTES.with(|p| p.set(true));
    app.terminate(None);
    println!("terminate: returned");
}

thread_local!(static PRINT_NOTES: Cell<bool> = const { Cell::new(false) });

fn launching_and_stopping(_: MainThreadMarker) {
    let (status, out) = in_child("--child-run");
    assert!(status.success(), "{status:?} {out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        [
            "observer NSApplicationWillFinishLaunchingNotification",
            "delegate NSApplicationWillFinishLaunchingNotification running=true",
            "observer NSApplicationDidFinishLaunchingNotification",
            "delegate NSApplicationDidFinishLaunchingNotification running=true",
            "stopping in \"kCFRunLoopDefaultMode\"",
            "stopped: running=false",
            "run returned",
        ]
    );
    // By hand: that launching finished is posted when the program first
    // looks for events.
    let (status, out) = in_child("--child-finish-launching");
    assert!(status.success(), "{status:?} {out}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        [
            "observer NSApplicationWillFinishLaunchingNotification",
            "delegate NSApplicationWillFinishLaunchingNotification running=false",
            "finishLaunching returned",
            "observer NSApplicationDidFinishLaunchingNotification",
            "delegate NSApplicationDidFinishLaunchingNotification running=false",
            "nextEvent returned",
        ]
    );
}

fn launch_observers(mtm: MainThreadMarker) -> (Retained<Observer>, Retained<Delegate>) {
    let app = app(mtm);
    let observer = Observer::new(mtm);
    observer.watch(unsafe { NSApplicationWillFinishLaunchingNotification }, None);
    observer.watch(unsafe { NSApplicationDidFinishLaunchingNotification }, None);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    PRINT_NOTES.with(|p| p.set(true));
    (observer, delegate)
}

fn child_run(mtm: MainThreadMarker) {
    let app = app(mtm);
    let _kept = launch_observers(mtm);
    let _stopper = timer_in(unsafe { NSDefaultRunLoopMode }, 0.1, || {
        let mode = NSRunLoop::currentRunLoop().currentMode().map(|m| m.to_string()).unwrap_or_default();
        note(format!("stopping in {mode:?}"));
        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        app.stop(None);
        note(format!("stopped: running={}", app.isRunning()));
        // macOS's loop notices a stop at its next event.
        post(0);
    });
    app.run();
    note("run returned".into());
}

fn child_finish_launching(mtm: MainThreadMarker) {
    let app = app(mtm);
    let _kept = launch_observers(mtm);
    app.finishLaunching();
    note("finishLaunching returned".into());
    // macOS finishes launching with an event of its own, which the loop
    // takes and sends.
    let mode = unsafe { NSDefaultRunLoopMode };
    let launched = Rc::new(Cell::new(false));
    let seen = launched.clone();
    let watcher = RcBlock::new(move |_: NonNull<NSNotification>| seen.set(true));
    let center = NSNotificationCenter::defaultCenter();
    let name = unsafe { NSApplicationDidFinishLaunchingNotification };
    let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &watcher) };
    for _ in 0..25 {
        let until = NSDate::dateWithTimeIntervalSinceNow(0.2);
        if let Some(event) =
            app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), mode, true)
        {
            app.sendEvent(&event);
        }
        if launched.get() {
            break;
        }
    }
    unsafe { center.removeObserver(token.as_ref()) };
    note("nextEvent returned".into());
}

// View frame and bounds notifications.

define_class!(
    /// A view with a tag, and an observer of its notifications.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsTagged"]
    #[ivars = &'static str]
    struct Tagged;

    impl Tagged {
        #[unsafe(method(changed:))]
        fn changed(&self, n: &NSNotification) {
            let name = n.name().to_string().replace("NSView", "").replace("DidChangeNotification", "");
            note(format!("{} {name}", self.ivars()));
        }
    }
);

impl Tagged {
    fn new(mtm: MainThreadMarker, name: &'static str, frame: NSRect) -> Retained<Self> {
        let this: Retained<Self> = unsafe { msg_send![super(Self::alloc(mtm).set_ivars(name)), initWithFrame: frame] };
        this.watch(&this);
        this
    }

    /// Hear of `view`'s frame and bounds changes.
    fn watch(&self, view: &NSView) {
        let center = NSNotificationCenter::defaultCenter();
        for name in unsafe { [NSViewFrameDidChangeNotification, NSViewBoundsDidChangeNotification] } {
            unsafe { center.addObserver_selector_name_object(self, sel!(changed:), Some(name), Some(view)) };
        }
    }
}

impl Drop for Tagged {
    fn drop(&mut self) {
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(self) };
    }
}

fn views_post_frame_and_bounds_changes(mtm: MainThreadMarker) {
    let outer = Tagged::new(mtm, "outer", rect(0.0, 0.0, 100.0, 100.0));
    let inner = Tagged::new(mtm, "inner", rect(10.0, 10.0, 50.0, 50.0));
    inner.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    outer.addSubview(&inner);
    // On by default, for every kind of view.
    assert!(outer.postsFrameChangedNotifications() && outer.postsBoundsChangedNotifications());
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 50.0, 50.0));
    let clip = scroll.contentView();
    assert!(scroll.postsFrameChangedNotifications() && scroll.postsBoundsChangedNotifications());
    assert!(clip.postsFrameChangedNotifications() && clip.postsBoundsChangedNotifications());
    take_notes();

    // Subviews resized with their superview post first; a new size alone
    // changes no bounds notification.
    outer.setFrame(rect(0.0, 0.0, 120.0, 100.0));
    assert_eq!(take_notes(), ["inner Frame", "outer Frame"]);
    outer.setFrame(rect(0.0, 0.0, 120.0, 100.0));
    assert_eq!(take_notes(), Vec::<String>::new());
    outer.setFrameOrigin(NSPoint::new(5.0, 5.0));
    assert_eq!(take_notes(), ["outer Frame"]);
    outer.setFrameSize(NSSize::new(130.0, 100.0));
    assert_eq!(take_notes(), ["inner Frame", "outer Frame"]);
    outer.setBoundsOrigin(NSPoint::new(3.0, 4.0));
    assert_eq!(take_notes(), ["outer Bounds"]);
    outer.setBoundsOrigin(NSPoint::new(3.0, 4.0));
    assert_eq!(take_notes(), Vec::<String>::new());

    // Turned off, changes wait; turned on, one notification says so.
    outer.setPostsFrameChangedNotifications(false);
    assert!(!outer.postsFrameChangedNotifications());
    outer.setFrame(rect(0.0, 0.0, 140.0, 100.0));
    outer.setFrame(rect(0.0, 0.0, 150.0, 100.0));
    assert_eq!(take_notes(), ["inner Frame", "inner Frame"]);
    outer.setPostsFrameChangedNotifications(true);
    assert_eq!(take_notes(), ["outer Frame"]);
    outer.setPostsFrameChangedNotifications(false);
    outer.setPostsFrameChangedNotifications(true);
    assert_eq!(take_notes(), Vec::<String>::new());
    outer.setPostsBoundsChangedNotifications(false);
    outer.setBoundsOrigin(NSPoint::new(7.0, 7.0));
    outer.setBoundsOrigin(NSPoint::new(8.0, 8.0));
    assert_eq!(take_notes(), Vec::<String>::new());
    outer.setPostsBoundsChangedNotifications(true);
    assert_eq!(take_notes(), ["outer Bounds"]);

    // Scrolling a clip view changes its bounds.
    let document = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 50.0, 500.0));
    scroll.setDocumentView(Some(&document));
    outer.watch(&clip);
    take_notes();
    clip.scrollToPoint(NSPoint::new(0.0, 10.0));
    clip.setBoundsOrigin(NSPoint::new(0.0, 20.0));
    assert_eq!(take_notes(), ["outer Bounds", "outer Bounds"]);
}

// Responders and the key view loop.

struct KeyedIvars {
    name: &'static str,
    accepts: Cell<bool>,
}

define_class!(
    /// A view that writes down keys, key equivalents, cancels and becoming
    /// first responder (with the window's selection direction then).
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsKeyed"]
    #[ivars = KeyedIvars]
    struct Keyed;

    impl Keyed {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().accepts.get()
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let direction = self.window().map_or(99, |w| w.keyViewSelectionDirection().0);
            note(format!("{} becomeFirstResponder {direction}", self.ivars().name));
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
            note(format!("{} keyDown: {chars:?}", self.ivars().name));
            unsafe { msg_send![super(self), keyDown: event] }
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let chars = event.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
            note(format!("{} performKeyEquivalent: {chars:?}", self.ivars().name));
            false
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            note(format!("{} cancelOperation:", self.ivars().name));
        }
    }
);

impl Keyed {
    fn new(mtm: MainThreadMarker, name: &'static str, accepts: bool, frame: NSRect) -> Retained<Self> {
        let ivars = KeyedIvars { name, accepts: Cell::new(accepts) };
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(ivars)), initWithFrame: frame] }
    }
}

/// The name of a key view, "nil" for none.
fn key_name(view: Option<Retained<NSView>>) -> String {
    match view {
        Some(v) => v
            .downcast_ref::<Keyed>()
            .map_or_else(|| v.class().name().to_str().unwrap().into(), |k| k.ivars().name.into()),
        None => "nil".into(),
    }
}

fn first_name(w: &NSWindow) -> String {
    match w.firstResponder() {
        Some(f) if f.downcast_ref::<NSWindow>().is_some() => "window".into(),
        Some(f) => key_name(f.downcast::<NSView>().ok()),
        None => "nil".into(),
    }
}

/// A window whose content holds four key views: a and c on the left, b in
/// the middle, d on the right, c and d in the top row.
struct KeyViews {
    window: Retained<NSWindow>,
    content: Retained<Keyed>,
    a: Retained<Keyed>,
    b: Retained<Keyed>,
    c: Retained<Keyed>,
    d: Retained<Keyed>,
}

fn key_views(mtm: MainThreadMarker) -> KeyViews {
    let window = window(mtm, titled());
    let content = Keyed::new(mtm, "content", false, rect(0.0, 0.0, 300.0, 200.0));
    let a = Keyed::new(mtm, "a", true, rect(10.0, 10.0, 50.0, 20.0));
    let b = Keyed::new(mtm, "b", true, rect(100.0, 50.0, 50.0, 20.0));
    let c = Keyed::new(mtm, "c", true, rect(10.0, 90.0, 50.0, 20.0));
    let d = Keyed::new(mtm, "d", true, rect(200.0, 90.0, 50.0, 20.0));
    window.setContentView(Some(&content));
    for view in [&a, &b, &c, &d] {
        content.addSubview(view);
    }
    KeyViews { window, content, a, b, c, d }
}

fn key_event_in(w: &NSWindow, chars: &str, flags: NSEventModifierFlags, code: u16) -> Retained<NSEvent> {
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        flags,
        1.0,
        w.windowNumber(),
        None,
        &NSString::from_str(chars),
        &NSString::from_str(chars),
        false,
        code,
    )
    .expect("a key event")
}

fn keys_go_to_the_key_window(mtm: MainThreadMarker) {
    let app = app(mtm);
    let v = key_views(mtm);
    v.window.makeFirstResponder(Some(&*v.a));
    take_notes();
    // No window is key: the application drops keys, key equivalents too.
    assert!(app.keyWindow().is_none());
    app.sendEvent(&key_event_in(&v.window, "x", NSEventModifierFlags::empty(), 7));
    app.sendEvent(&key_event_in(&v.window, "k", NSEventModifierFlags::Command, 40));
    assert_eq!(take_notes(), Vec::<String>::new());
}

fn the_window_ends_the_key_chain(mtm: MainThreadMarker) {
    let v = key_views(mtm);
    let w = &v.window;
    let none = NSEventModifierFlags::empty();
    // A new window is its own first responder.
    assert_eq!(first_name(w), "window");
    w.makeFirstResponder(Some(&*v.a));
    take_notes();
    // A key nobody takes reaches the window, which offers it to its views as
    // a key equivalent; Tab without key view links moves nothing.
    w.sendEvent(&key_event_in(w, "\t", none, 48));
    assert_eq!(
        take_notes(),
        ["a keyDown: \"\\t\"", "content keyDown: \"\\t\"", "content performKeyEquivalent: \"\\t\""]
    );
    assert_eq!(first_name(w), "a");
    unsafe { v.a.setNextKeyView(Some(&*v.b)) };
    w.sendEvent(&key_event_in(w, "\t", none, 48));
    assert_eq!(
        take_notes(),
        [
            "a keyDown: \"\\t\"",
            "content keyDown: \"\\t\"",
            "content performKeyEquivalent: \"\\t\"",
            "b becomeFirstResponder 1",
        ]
    );
    // Shift-Tab types a back tab.
    w.sendEvent(&key_event_in(w, "\u{19}", NSEventModifierFlags::Shift, 48));
    assert_eq!(
        take_notes(),
        [
            "b keyDown: \"\\u{19}\"",
            "content keyDown: \"\\u{19}\"",
            "content performKeyEquivalent: \"\\u{19}\"",
            "a becomeFirstResponder 2",
        ]
    );
    // Escape cancels, from the first responder up.
    w.sendEvent(&key_event_in(w, "\u{1b}", none, 53));
    assert_eq!(
        take_notes(),
        [
            "a keyDown: \"\\u{1b}\"",
            "content keyDown: \"\\u{1b}\"",
            "content performKeyEquivalent: \"\\u{1b}\"",
            "a cancelOperation:",
        ]
    );
    // Command-period cancels without being typed.
    w.sendEvent(&key_event_in(w, ".", NSEventModifierFlags::Command, 47));
    assert_eq!(take_notes(), ["a cancelOperation:"]);
}

define_class!(
    /// A responder that writes down cancels and keys nobody took.
    #[unsafe(super(NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsEnd"]
    struct ChainEnd;

    impl ChainEnd {
        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            note("end cancelOperation:".into());
        }

        #[unsafe(method(noResponderFor:))]
        fn no_responder_for(&self, selector: objc2::runtime::Sel) {
            note(format!("end noResponderFor: {}", selector.name().to_str().unwrap()));
        }
    }
);

fn responders_pass_commands_up(mtm: MainThreadMarker) {
    let plain: Retained<NSResponder> = unsafe { msg_send![NSResponder::alloc(mtm), init] };
    // Cancelling is up to responders that implement it.
    assert!(!plain.respondsToSelector(sel!(cancelOperation:)));
    assert!(plain.respondsToSelector(sel!(noResponderFor:)));
    let end: Retained<ChainEnd> = unsafe { msg_send![super(ChainEnd::alloc(mtm).set_ivars(())), init] };
    unsafe { plain.setNextResponder(Some(&end)) };
    assert!(unsafe { plain.tryToPerform_with(sel!(cancelOperation:), None) });
    unsafe { plain.doCommandBySelector(sel!(cancelOperation:)) };
    assert_eq!(take_notes(), ["end cancelOperation:", "end cancelOperation:"]);
    // A key that reaches the end of the chain is nobody's; the responder
    // there hears so, and doesn't pass that on.
    let w = window(mtm, titled());
    plain.keyDown(&key_event_in(&w, "q", NSEventModifierFlags::empty(), 12));
    unsafe { end.noResponderFor(sel!(keyDown:)) };
    assert_eq!(take_notes(), ["end noResponderFor: keyDown:", "end noResponderFor: keyDown:"]);
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsOpaque"]
    struct Opaque;

    impl Opaque {
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }
    }
);

fn clicks_that_move_windows(mtm: MainThreadMarker) {
    let plain = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 1.0, 1.0));
    let opaque: Retained<Opaque> =
        unsafe { msg_send![super(Opaque::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 1.0, 1.0)] };
    // Views that aren't opaque let a click move a window that moves by its
    // background.
    assert!(plain.mouseDownCanMoveWindow());
    assert!(!opaque.mouseDownCanMoveWindow());
    assert!(!plain.acceptsFirstMouse(None));
    let w = window(mtm, titled());
    assert!(!w.isMovableByWindowBackground() && w.isMovable());
}

fn key_view_links(mtm: MainThreadMarker) {
    let v = key_views(mtm);
    let (a, b, c, d) = (&v.a, &v.b, &v.c, &v.d);
    unsafe {
        assert!(a.canBecomeKeyView());
        assert!(!v.content.canBecomeKeyView());
        a.setHidden(true);
        assert!(!a.canBecomeKeyView());
        a.setHidden(false);
        let outside = Keyed::new(mtm, "outside", true, rect(0.0, 0.0, 1.0, 1.0));
        assert!(!outside.canBecomeKeyView());

        assert_eq!(key_name(a.nextKeyView()), "nil");
        assert_eq!(key_name(a.previousKeyView()), "nil");
        a.setNextKeyView(Some(b));
        assert_eq!((key_name(a.nextKeyView()), key_name(b.previousKeyView())), ("b".into(), "a".into()));
        // Relinking leaves the old next view's link back as it was…
        a.setNextKeyView(Some(c));
        assert_eq!((key_name(b.previousKeyView()), key_name(c.previousKeyView())), ("a".into(), "a".into()));
        // …and a view linked to from two places links back to the latest.
        d.setNextKeyView(Some(c));
        assert_eq!((key_name(c.previousKeyView()), key_name(a.nextKeyView())), ("d".into(), "c".into()));
        // Unlinking clears the link back, if it is to the view unlinked.
        a.setNextKeyView(None);
        assert_eq!(key_name(c.previousKeyView()), "d");
        d.setNextKeyView(None);
        assert_eq!(key_name(c.previousKeyView()), "nil");

        // Valid key views are the first along the links that can become key
        // views, short of the start.
        a.setNextKeyView(Some(b));
        b.setNextKeyView(Some(c));
        c.setNextKeyView(Some(a));
        assert_eq!(key_name(a.nextValidKeyView()), "b");
        assert_eq!(key_name(a.previousValidKeyView()), "c");
        b.ivars().accepts.set(false);
        assert_eq!(key_name(a.nextValidKeyView()), "c");
        assert_eq!(key_name(c.previousValidKeyView()), "a");
        b.ivars().accepts.set(true);
        c.setNextKeyView(None);
        assert_eq!(key_name(c.nextValidKeyView()), "nil");
        assert_eq!(key_name(a.previousValidKeyView()), "nil");
        d.setNextKeyView(Some(d));
        assert_eq!(key_name(d.nextValidKeyView()), "nil");
        // Taking a view out of its window keeps its links.
        b.removeFromSuperview();
        assert_eq!((key_name(a.nextKeyView()), key_name(b.nextKeyView())), ("b".into(), "c".into()));
    }
}

fn selecting_key_views(mtm: MainThreadMarker) {
    let v = key_views(mtm);
    let w = &v.window;
    unsafe {
        v.a.setNextKeyView(Some(&*v.b));
        v.b.setNextKeyView(Some(&*v.c));
    }
    take_notes();
    // With the window as first responder and no initial first responder,
    // nothing is selected.
    w.selectNextKeyView(None);
    w.selectPreviousKeyView(None);
    assert_eq!(first_name(w), "window");
    // With one, it is, either way.
    w.setInitialFirstResponder(Some(&*v.b));
    w.selectNextKeyView(None);
    assert_eq!(take_notes(), ["b becomeFirstResponder 1"]);
    w.makeFirstResponder(None);
    w.selectPreviousKeyView(None);
    assert_eq!(take_notes(), ["b becomeFirstResponder 2"]);
    // Along the links, stopping at the end.
    w.selectNextKeyView(None);
    assert_eq!(take_notes(), ["c becomeFirstResponder 1"]);
    w.selectNextKeyView(None);
    assert_eq!(first_name(w), "c");
    w.selectKeyViewFollowingView(&v.a);
    assert_eq!(take_notes(), ["b becomeFirstResponder 1"]);
    w.selectKeyViewPrecedingView(&v.c);
    assert_eq!(take_notes(), Vec::<String>::new());
    assert_eq!(first_name(w), "b");
    // The direction is direct outside a selection.
    assert_eq!(w.keyViewSelectionDirection(), NSSelectionDirection::DirectSelection);
}

fn recalculated_key_view_loops(mtm: MainThreadMarker) {
    let v = key_views(mtm);
    let w = &v.window;
    // Top to bottom, then left to right, from the content view round to it.
    w.recalculateKeyViewLoop();
    unsafe {
        let links: Vec<_> = [&v.content, &v.a, &v.b, &v.c, &v.d].iter().map(|x| key_name(x.nextKeyView())).collect();
        assert_eq!(links, ["c", "content", "a", "d", "b"]);
        for x in [&v.content, &v.a, &v.b, &v.c, &v.d] {
            x.setNextKeyView(None);
        }
    }
    // Recalculated before selecting, when asked to.
    w.setAutorecalculatesKeyViewLoop(true);
    let f = Keyed::new(mtm, "f", true, rect(150.0, 150.0, 20.0, 20.0));
    v.content.addSubview(&f);
    w.makeFirstResponder(Some(&*v.a));
    take_notes();
    w.selectNextKeyView(None);
    assert_eq!(take_notes(), ["f becomeFirstResponder 1"]);
    unsafe { assert_eq!(key_name(f.nextKeyView()), "c") };
}

define_class!(
    /// An application subclass that writes down what it is sent.
    #[unsafe(super(NSApplication, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsApplication"]
    struct Application;

    impl Application {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if event.r#type() == NSEventType::ApplicationDefined {
                note(format!("subclass sendEvent: {}", event.data1()));
            }
            unsafe { msg_send![super(self), sendEvent: event] }
        }
    }
);

fn a_subclass_is_the_application(_: MainThreadMarker) {
    let (status, out) = in_child("--child-subclass");
    assert!(status.success(), "{status:?} {out}");
    // (The event that wakes macOS's loop to see the stop may be sent too.)
    let lines: Vec<_> = out.lines().filter(|l| *l != "subclass sendEvent: 0").collect();
    assert_eq!(lines, ["ConformanceEventsApplication", "same", "subclass sendEvent: 5"]);
}

fn child_subclass(mtm: MainThreadMarker) {
    PRINT_NOTES.with(|p| p.set(true));
    let app: Retained<NSApplication> = unsafe { msg_send![Application::class(), sharedApplication] };
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    note(app.class().name().to_str().unwrap().into());
    let again = NSApplication::sharedApplication(mtm);
    note(if std::ptr::eq(&*again, &*app) { "same" } else { "different" }.into());
    post(5);
    let _stopper = timer_in(unsafe { NSDefaultRunLoopMode }, 0.2, || {
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).stop(None);
        post(0);
    });
    app.run();
}

// Panels.

fn panels(mtm: MainThreadMarker) {
    let panel = |style: NSWindowStyleMask| {
        NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect(0.0, 0.0, 100.0, 100.0),
            style,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // A utility panel floats.
    let utility = panel(NSWindowStyleMask::Titled | NSWindowStyleMask::UtilityWindow);
    assert!(utility.isFloatingPanel());
    assert_eq!(utility.level(), NSFloatingWindowLevel);
    assert!(!utility.becomesKeyOnlyIfNeeded() && !utility.worksWhenModal());
    assert!(utility.hidesOnDeactivate());
    assert!(!utility.isReleasedWhenClosed());
    // Panels are never main; borderless ones aren't key either.
    assert!(utility.canBecomeKeyWindow() && !utility.canBecomeMainWindow());
    let plain = panel(NSWindowStyleMask::Titled);
    assert!(plain.canBecomeKeyWindow() && !plain.canBecomeMainWindow());
    let borderless = panel(NSWindowStyleMask::Borderless);
    assert!(!borderless.canBecomeKeyWindow() && !borderless.canBecomeMainWindow());
    // Floating is a level.
    utility.setFloatingPanel(false);
    assert!(!utility.isFloatingPanel());
    assert_eq!(utility.level(), NSNormalWindowLevel);
    plain.setFloatingPanel(true);
    assert_eq!(plain.level(), NSFloatingWindowLevel);
    plain.setBecomesKeyOnlyIfNeeded(true);
    plain.setWorksWhenModal(true);
    assert!(plain.becomesKeyOnlyIfNeeded() && plain.worksWhenModal());
    // A plain window doesn't work when modal.
    assert!(!window(mtm, titled()).worksWhenModal());
}

// Tooltips.

fn tool_tips(mtm: MainThreadMarker) {
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
    let text = |v: &NSView| v.toolTip().map(|t| t.to_string());
    assert_eq!(text(&view), None);
    // A tooltip is a tracking area of its view.
    view.setToolTip(Some(&NSString::from_str("hello")));
    assert_eq!(text(&view).as_deref(), Some("hello"));
    assert_eq!(view.trackingAreas().count(), 1);
    view.setToolTip(Some(&NSString::from_str("")));
    assert_eq!(text(&view).as_deref(), Some(""));
    view.setToolTip(None);
    assert_eq!(text(&view), None);
    assert_eq!(view.trackingAreas().count(), 0);
    // Rectangles get tags, and their owners aren't retained.
    let owner = NSObject::new();
    let before = owner.retainCount();
    let first = unsafe { view.addToolTipRect_owner_userData(rect(0.0, 0.0, 10.0, 10.0), &owner, std::ptr::null_mut()) };
    let second =
        unsafe { view.addToolTipRect_owner_userData(rect(10.0, 0.0, 10.0, 10.0), &owner, std::ptr::null_mut()) };
    assert!(first != 0 && second != 0 && first != second);
    assert_eq!(owner.retainCount(), before);
    assert_eq!(view.trackingAreas().count(), 2);
    view.removeToolTip(first);
    assert_eq!(view.trackingAreas().count(), 1);
    // Removing them all takes the view's own tooltip too.
    view.setToolTip(Some(&NSString::from_str("again")));
    view.removeAllToolTips();
    assert_eq!(text(&view), None);
    assert_eq!(view.trackingAreas().count(), 0);
    assert_eq!(NSFont::toolTipsFontOfSize(0.0).pointSize(), 11.0);
}

// Frame autosave.

fn frame_autosave(mtm: MainThreadMarker) {
    let defaults = NSUserDefaults::standardUserDefaults();
    let name = NSString::from_str("SidestepConformanceAutosave");
    let key = NSString::from_str("NSWindow Frame SidestepConformanceAutosave");
    let stored = || defaults.stringForKey(&key).map(|s| s.to_string());
    // The first four numbers stored: the frame.
    let saved_frame = || {
        let numbers: Vec<f64> = stored()?.split_whitespace().take(4).map(|n| n.parse().unwrap()).collect();
        Some(rect(numbers[0], numbers[1], numbers[2], numbers[3]))
    };
    NSWindow::removeFrameUsingName(&name, mtm);
    let w = window(mtm, titled());
    // Naming saves nothing yet.
    assert!(w.setFrameAutosaveName(&name));
    assert_eq!(w.frameAutosaveName().to_string(), "SidestepConformanceAutosave");
    assert_eq!(stored(), None);
    // Moving and resizing save the frame, as whole numbers.
    w.setFrameOrigin(NSPoint::new(150.0, 160.0));
    assert_eq!(saved_frame(), Some(w.frame()));
    assert!(stored().unwrap().starts_with("150 160 "));
    w.setContentSize(NSSize::new(320.0, 210.0));
    assert_eq!(saved_frame(), Some(w.frame()));
    // Another window takes the saved frame's size.
    let other = window(mtm, titled());
    assert!(other.setFrameUsingName(&name));
    assert_eq!(other.frame().size, w.frame().size);
    assert!(!other.setFrameUsingName(&NSString::from_str("SidestepConformanceNothing")));
    // Four numbers are enough.
    unsafe { defaults.setObject_forKey(Some(&NSString::from_str("11 21 201 101")), &key) };
    assert!(other.setFrameUsingName(&name));
    assert_eq!(other.frame().size, NSSize::new(201.0, 101.0));
    // A window given the name takes the frame saved under it. (Whether two
    // windows may share a name isn't pinned: macOS has said both.)
    w.setFrameAutosaveName(&NSString::from_str(""));
    let third = window(mtm, titled());
    assert!(third.setFrameAutosaveName(&name));
    assert_eq!(third.frame().size, NSSize::new(201.0, 101.0));
    third.setFrameAutosaveName(&NSString::from_str(""));
    assert_eq!(third.frameAutosaveName().to_string(), "");
    other.setFrameAutosaveName(&NSString::from_str(""));
    NSWindow::removeFrameUsingName(&name, mtm);
    assert_eq!(stored(), None);
}

// Controllers.

define_class!(
    /// Loads a view of its own and says when.
    #[unsafe(super(NSViewController, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceEventsViewController"]
    struct ViewController;

    impl ViewController {
        #[unsafe(method(loadView))]
        fn load_view(&self) {
            note("loadView".into());
            let view = NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0.0, 0.0, 120.0, 80.0));
            self.setView(&view);
        }

        #[unsafe(method(viewDidLoad))]
        fn view_did_load(&self) {
            note("viewDidLoad".into());
            unsafe { msg_send![super(self), viewDidLoad] }
        }
    }
);

impl ViewController {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(())), init] }
    }
}

fn same<A: ?Sized, B: ?Sized>(a: &A, b: &B) -> bool {
    std::ptr::eq(a as *const A as *const u8, b as *const B as *const u8)
}

fn view_controllers(mtm: MainThreadMarker) {
    let vc = ViewController::new(mtm);
    assert!(!vc.isViewLoaded());
    assert!(vc.nibName().is_none() && vc.title().is_none());
    // Loaded when first asked for, once.
    let view = vc.view();
    assert_eq!(take_notes(), ["loadView", "viewDidLoad"]);
    assert!(vc.isViewLoaded() && view.frame().size == NSSize::new(120.0, 80.0));
    assert!(same(&*vc.view(), &*view));
    assert_eq!(take_notes(), Vec::<String>::new());
    // Between its view and the view's superview in the responder chain.
    let next = unsafe { view.nextResponder() }.unwrap();
    assert!(same(&*next, &*vc));
    let superview = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 300.0));
    superview.addSubview(&view);
    assert!(same(&*unsafe { view.nextResponder() }.unwrap(), &*vc));
    assert!(same(&*unsafe { vc.nextResponder() }.unwrap(), &*superview));
    // Without a nib or a view of its own: a plain view.
    let plain = NSViewController::new(mtm);
    let _ = plain.view();
    assert!(plain.isViewLoaded());
    // Children.
    let child = ViewController::new(mtm);
    vc.addChildViewController(&child);
    assert_eq!(vc.childViewControllers().count(), 1);
    assert!(child.parentViewController().is_some_and(|p| same(&*p, &*vc)));
    child.removeFromParentViewController();
    assert_eq!(vc.childViewControllers().count(), 0);
    assert!(child.parentViewController().is_none());
    vc.setTitle(Some(&NSString::from_str("Title")));
    assert_eq!(vc.title().map(|t| t.to_string()).as_deref(), Some("Title"));
    unsafe { vc.setRepresentedObject(Some(&NSString::from_str("R"))) };
    assert!(vc.representedObject().is_some());
    view.removeFromSuperview();

    // A view outliving its controller has its superview next again.
    let kept = objc2::rc::autoreleasepool(|_| {
        let vc = ViewController::new(mtm);
        let view = vc.view();
        superview.addSubview(&view);
        take_notes();
        view
    });
    assert!(same(&*unsafe { kept.nextResponder() }.unwrap(), &*superview));
}

fn window_controllers(mtm: MainThreadMarker) {
    let vc = ViewController::new(mtm);
    vc.setTitle(Some(&NSString::from_str("From the controller")));
    // A window for a view controller: its view, its size and its title.
    let w = NSWindow::windowWithContentViewController(&vc);
    unsafe { w.setReleasedWhenClosed(false) };
    assert!(w.contentView().is_some_and(|c| same(&*c, &*vc.view())));
    assert!(w.contentViewController().is_some_and(|c| same(&*c, &*vc)));
    assert_eq!(w.contentLayoutRect().size, NSSize::new(120.0, 80.0));
    assert_eq!(w.title().to_string(), "From the controller");
    assert_eq!(w.styleMask(), titled());
    take_notes();
    // A window controller is its window's controller and next responder.
    let wc = NSWindowController::initWithWindow(NSWindowController::alloc(mtm), Some(&w));
    assert!(wc.window().is_some_and(|x| same(&*x, &*w)));
    assert!(w.windowController().is_some_and(|c| same(&*c, &*wc)));
    assert!(unsafe { w.nextResponder() }.is_some_and(|n| same(&*n, &*wc)));
    assert!(wc.isWindowLoaded());
    assert!(wc.contentViewController().is_some_and(|c| same(&*c, &*vc)));
    assert!(wc.shouldCascadeWindows());
    assert_eq!(wc.windowFrameAutosaveName().to_string(), "");
    wc.setWindowFrameAutosaveName(&NSString::from_str("SidestepConformanceController"));
    assert_eq!(w.frameAutosaveName().to_string(), "SidestepConformanceController");
    w.setFrameAutosaveName(&NSString::from_str(""));
    NSWindow::removeFrameUsingName(&NSString::from_str("SidestepConformanceController"), mtm);
    let empty = NSWindowController::initWithWindow(NSWindowController::alloc(mtm), None);
    assert!(empty.window().is_none());
}

fn periodic_events(mtm: MainThreadMarker) {
    let app = app(mtm);
    discard_app_events(&app);
    let mode = unsafe { NSEventTrackingRunLoopMode };
    let next = |seconds: f64| {
        let until = NSDate::dateWithTimeIntervalSinceNow(seconds);
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Periodic, Some(&until), mode, true)
    };
    let start = std::time::Instant::now();
    NSEvent::startPeriodicEventsAfterDelay_withPeriod(0.05, 0.02);
    let first = next(2.0).expect("a periodic event");
    assert_eq!(first.r#type(), NSEventType::Periodic);
    assert!(start.elapsed().as_secs_f64() >= 0.04);
    for _ in 0..3 {
        assert!(next(2.0).is_some());
    }
    // Nobody taking them: they don't pile up.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut waiting = 0;
    while app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Periodic, None, mode, true).is_some() {
        waiting += 1;
        assert!(waiting < 5, "periodic events piled up");
    }
    assert!(waiting <= 1);
    NSEvent::stopPeriodicEvents();
    assert!(next(0.1).is_none());
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let children: &[Test] = &[
        ("--child-terminate-now", child_terminate_now),
        ("--child-run", child_run),
        ("--child-finish-launching", child_finish_launching),
        ("--child-subclass", child_subclass),
    ];
    if let Some(arg) = std::env::args().nth(1)
        && let Some((_, child)) = children.iter().find(|(name, _)| *name == arg)
    {
        child(mtm);
        return;
    }
    let tests: &[Test] = &[
        ("mode_names", mode_names),
        ("the_application_adds_its_modes_to_the_common_modes", the_application_adds_its_modes_to_the_common_modes),
        ("tracking_timers_fire_only_in_tracking_loops", tracking_timers_fire_only_in_tracking_loops),
        ("default_timers_wait_while_tracking", default_timers_wait_while_tracking),
        ("a_window_tracks_events_in_the_tracking_mode", a_window_tracks_events_in_the_tracking_mode),
        ("nested_loops_leave_other_events_queued", nested_loops_leave_other_events_queued),
        ("run_loop_observers_see_appkit_loops", run_loop_observers_see_appkit_loops),
        ("notification_names", notification_names),
        ("delegates_hear_through_the_notification_center", delegates_hear_through_the_notification_center),
        ("application_delegates_are_weak", application_delegates_are_weak),
        ("moving_and_resizing_windows", moving_and_resizing_windows),
        ("closing_tells_and_releases", closing_tells_and_releases),
        ("terminating_asks_the_delegate", terminating_asks_the_delegate),
        ("terminating_now_exits", terminating_now_exits),
        ("launching_and_stopping", launching_and_stopping),
        ("views_post_frame_and_bounds_changes", views_post_frame_and_bounds_changes),
        ("keys_go_to_the_key_window", keys_go_to_the_key_window),
        ("the_window_ends_the_key_chain", the_window_ends_the_key_chain),
        ("responders_pass_commands_up", responders_pass_commands_up),
        ("clicks_that_move_windows", clicks_that_move_windows),
        ("key_view_links", key_view_links),
        ("selecting_key_views", selecting_key_views),
        ("recalculated_key_view_loops", recalculated_key_view_loops),
        ("a_subclass_is_the_application", a_subclass_is_the_application),
        ("panels", panels),
        ("tool_tips", tool_tips),
        ("frame_autosave", frame_autosave),
        ("view_controllers", view_controllers),
        ("window_controllers", window_controllers),
        ("periodic_events", periodic_events),
    ];
    let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
    for (name, test) in tests {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
