//! AppKit behaviour that needs windows on screen: sheets, modal loops and
//! modal sessions. Opt-in, as the windows show: `SIDESTEP_CONFORMANCE_WINDOWS=1`
//! runs them. On macOS the application uses the accessory activation
//! policy and is never activated, the windows are small and ordered out at
//! the end. On Linux they run under the null render thread unless a
//! Wayland compositor is there (`scripts/headless-wayland`).
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSModalResponse, NSModalResponseContinue,
    NSModalResponseStop, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSNotification, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString,
    NSTimer,
};

use sidestep as _;

thread_local!(static NOTES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn note(line: String) {
    NOTES.with(|n| n.borrow_mut().push(line));
}

fn take_notes() -> Vec<String> {
    NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

fn window(mtm: MainThreadMarker, width: f64, height: f64) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(40.0, 40.0), NSSize::new(width, height)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

/// Run the default mode for a moment, so the window server catches up.
fn pump() {
    let until = NSDate::dateWithTimeIntervalSinceNow(0.1);
    unsafe { NSRunLoop::currentRunLoop().runMode_beforeDate(NSDefaultRunLoopMode, &until) };
}

fn timer_in(mode: &NSString, seconds: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(seconds, false, &block) };
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, mode) };
    timer
}

define_class!(
    /// Hears of sheets beginning and ending.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceWindowsDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillBeginSheet:))]
        fn will_begin_sheet(&self, n: &NSNotification) {
            note(format!("delegate {}", n.name()));
        }

        #[unsafe(method(windowDidEndSheet:))]
        fn did_end_sheet(&self, n: &NSNotification) {
            note(format!("delegate {}", n.name()));
        }
    }
);

fn is(a: Option<Retained<NSWindow>>, b: &NSWindow) -> bool {
    a.is_some_and(|a| std::ptr::eq(&*a, b))
}

fn sheets(mtm: MainThreadMarker) {
    let parent = window(mtm, 240.0, 160.0);
    parent.orderFrontRegardless();
    pump();
    let delegate: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc(mtm).set_ivars(())), init] };
    parent.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let first = window(mtm, 160.0, 80.0);
    let second = window(mtm, 160.0, 80.0);
    let handler = |name: &'static str| RcBlock::new(move |code: NSModalResponse| note(format!("{name} ended {code}")));
    let (first_done, second_done) = (handler("first"), handler("second"));

    // Beginning tells the parent's observers at once, and attaches.
    parent.beginSheet_completionHandler(&first, Some(&first_done));
    assert_eq!(take_notes(), ["delegate NSWindowWillBeginSheetNotification"]);
    assert!(is(parent.attachedSheet(), &first));
    assert!(first.isSheet() && is(first.sheetParent(), &parent));
    assert!(first.isVisible());
    assert_eq!(parent.sheets().count(), 1);
    // A second waits its turn.
    parent.beginSheet_completionHandler(&second, Some(&second_done));
    pump();
    assert_eq!(take_notes(), Vec::<String>::new());
    assert!(is(parent.attachedSheet(), &first));
    assert!(second.isSheet());
    assert_eq!(parent.sheets().count(), 2);
    // Ending one calls its handler with the code, tells the observers, and
    // begins the next.
    parent.endSheet_returnCode(&first, 5);
    assert_eq!(
        take_notes(),
        ["first ended 5", "delegate NSWindowDidEndSheetNotification", "delegate NSWindowWillBeginSheetNotification"]
    );
    assert!(!first.isVisible());
    assert!(is(parent.attachedSheet(), &second));
    // Ending without a code ends with "stop".
    parent.endSheet(&second);
    assert_eq!(
        take_notes(),
        [format!("second ended {NSModalResponseStop}"), "delegate NSWindowDidEndSheetNotification".into()]
    );
    assert!(parent.attachedSheet().is_none());
    assert_eq!(parent.sheets().count(), 0);
    // Critical sheets begin and end the same way.
    parent.beginCriticalSheet_completionHandler(&first, Some(&first_done));
    assert!(is(parent.attachedSheet(), &first));
    parent.endSheet(&first);
    assert_eq!(
        take_notes(),
        [
            "delegate NSWindowWillBeginSheetNotification".to_string(),
            format!("first ended {NSModalResponseStop}"),
            "delegate NSWindowDidEndSheetNotification".into(),
        ]
    );
    parent.setDelegate(None);
    parent.orderOut(None);
    first.orderOut(None);
    second.orderOut(None);
}

fn modal_sessions(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let modal = window(mtm, 160.0, 80.0);
    let session = app.beginModalSessionForWindow(&modal);
    assert!(!session.is_null());
    assert!(is(app.modalWindow(), &modal));
    assert!(modal.isVisible());
    // Each pass goes on until the session is stopped, then answers its
    // response until it ends.
    assert_eq!(unsafe { app.runModalSession(session) }, NSModalResponseContinue);
    app.stopModalWithCode(7);
    assert_eq!(unsafe { app.runModalSession(session) }, 7);
    assert_eq!(unsafe { app.runModalSession(session) }, 7);
    unsafe { app.endModalSession(session) };
    assert!(app.modalWindow().is_none());
    // The window stays on screen.
    assert!(modal.isVisible());
    modal.orderOut(None);
}

fn modal_loops(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let modal = window(mtm, 160.0, 80.0);
    let mode = std::rc::Rc::new(RefCell::new(String::new()));
    let seen = mode.clone();
    // A common-mode timer runs inside the modal loop and ends it.
    let stopper = timer_in(unsafe { NSRunLoopCommonModes }, 0.05, move || {
        let current = NSRunLoop::currentRunLoop().currentMode().map(|m| m.to_string()).unwrap_or_default();
        seen.replace(current);
        NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).stopModalWithCode(3);
    });
    let fired = std::rc::Rc::new(Cell::new(false));
    let default_fired = fired.clone();
    // A default-mode timer waits for the loop to end.
    let waiting = timer_in(unsafe { NSDefaultRunLoopMode }, 0.01, move || default_fired.set(true));
    let response = app.runModalForWindow(&modal);
    assert_eq!(response, 3);
    assert_eq!(mode.borrow().as_str(), "NSModalPanelRunLoopMode");
    assert!(!fired.get());
    assert!(app.modalWindow().is_none());
    stopper.invalidate();
    waiting.invalidate();
    modal.orderOut(None);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    if std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_none() {
        println!("appkit_windows: skipped (SIDESTEP_CONFORMANCE_WINDOWS=1 runs these tests, which show windows)");
        return;
    }
    #[cfg(not(target_vendor = "apple"))]
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        // SAFETY: nothing else runs yet to read the environment.
        unsafe { std::env::set_var("SIDESTEP_BACKEND", "null") };
    }
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let tests: &[Test] = &[("sheets", sheets), ("modal_sessions", modal_sessions), ("modal_loops", modal_loops)];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
