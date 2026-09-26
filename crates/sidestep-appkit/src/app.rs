//! `NSApplication` and the main thread's event loop.
//!
//! The loop sleeps until the render thread sends something or the next
//! timer is due, handles what arrived, fires due timers, then gives each
//! window a display pass. Nothing on this thread waits for rendering.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventType, NSResponder, NSWindow};
use objc2_foundation::NSString;
use sidestep_foundation::{fire_due_timers, next_timer_deadline, notification};

use crate::backend::{self, Backend};
use crate::protocol::{FromRender, ToRender};
use crate::{graphics, window};

thread_local! {
    static BACKEND: OnceCell<Backend> = const { OnceCell::new() };
    static SHARED: OnceCell<Retained<NSApplication>> = const { OnceCell::new() };
    /// Windows on screen, in the order they were shown.
    static WINDOWS: RefCell<Vec<Retained<NSWindow>>> = const { RefCell::new(Vec::new()) };
}

/// Send to the render thread, starting it with the first message.
pub(crate) fn send(msg: ToRender) {
    BACKEND.with(|b| {
        let _ = b.get_or_init(backend::start).tx.send(msg);
    });
}

pub(crate) fn add_window(window: &NSWindow) {
    WINDOWS.with(|w| w.borrow_mut().push(window.retain()));
}

pub(crate) fn remove_window(window: &NSWindow) {
    // Dropped outside the borrow: releasing may run arbitrary code.
    let removed: Vec<_> = WINDOWS.with(|w| {
        let mut windows = w.borrow_mut();
        let (gone, kept) = windows.drain(..).partition(|x| std::ptr::eq(&**x, window));
        *windows = kept;
        gone
    });
    drop(removed);
}

fn windows() -> Vec<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().clone())
}

/// Make sure the classes this crate instantiates directly are loaded from
/// their shells before their `define_class!` types are used.
pub(crate) fn load_shells() {
    thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    // SAFETY: +class takes nothing and returns the receiver.
    let _: &objc2::runtime::AnyClass = unsafe { msg_send![NSEvent::class(), class] };
    graphics::install_string_drawing();
}

pub(crate) struct AppIvars {
    delegate: RefCell<Option<Retained<AnyObject>>>,
    policy: Cell<NSApplicationActivationPolicy>,
    running: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSApplication"]
    #[ivars = AppIvars]
    pub(crate) struct NSApplicationImpl;

    impl NSApplicationImpl {
        #[unsafe(method_id(sharedApplication))]
        fn shared_application() -> Retained<NSApplication> {
            shared()
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().clone()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            self.ivars().delegate.replace(delegate.map(|d| d.retain()));
        }

        #[unsafe(method(activationPolicy))]
        fn activation_policy(&self) -> NSApplicationActivationPolicy {
            self.ivars().policy.get()
        }

        #[unsafe(method(setActivationPolicy:))]
        fn set_activation_policy(&self, policy: NSApplicationActivationPolicy) -> bool {
            self.ivars().policy.set(policy);
            true
        }

        #[unsafe(method(activateIgnoringOtherApps:))]
        fn activate_ignoring_other_apps(&self, _flag: bool) {}

        #[unsafe(method(activate))]
        fn activate(&self) {}

        #[unsafe(method(isRunning))]
        fn is_running(&self) -> bool {
            self.ivars().running.get()
        }

        #[unsafe(method(finishLaunching))]
        fn finish_launching(&self) {
            tell_delegate(self, Launch::Will);
        }

        #[unsafe(method(run))]
        fn run(&self) {
            run(self);
        }

        #[unsafe(method(stop:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            self.ivars().running.set(false);
        }

        #[unsafe(method(terminate:))]
        fn terminate(&self, _sender: Option<&AnyObject>) {
            terminate(self);
        }
    }

    unsafe impl NSObjectProtocol for NSApplicationImpl {}
);

fn shared() -> Retained<NSApplication> {
    SHARED.with(|s| {
        s.get_or_init(|| {
            let mtm = MainThreadMarker::new().expect("sidestep: NSApplication belongs to the main thread");
            load_shells();
            let this = NSApplicationImpl::alloc(mtm).set_ivars(AppIvars {
                delegate: RefCell::new(None),
                policy: Cell::new(NSApplicationActivationPolicy::Regular),
                running: Cell::new(false),
            });
            // SAFETY: NSResponder's designated initializer.
            let app: Retained<NSApplicationImpl> = unsafe { msg_send![super(this), init] };
            // SAFETY: NSApplicationImpl is the class NSApplication names.
            unsafe { Retained::cast_unchecked(app) }
        })
        .clone()
    })
}

fn app_impl(app: &NSApplication) -> &NSApplicationImpl {
    // SAFETY: NSApplication is NSApplicationImpl's class.
    unsafe { &*(app as *const NSApplication).cast::<NSApplicationImpl>() }
}

enum Launch {
    Will,
    Did,
}

fn tell_delegate(app: &NSApplicationImpl, when: Launch) {
    let Some(delegate) = app.ivars().delegate.borrow().clone() else { return };
    let (sel, name) = match when {
        Launch::Will => (sel!(applicationWillFinishLaunching:), "NSApplicationWillFinishLaunchingNotification"),
        Launch::Did => (sel!(applicationDidFinishLaunching:), "NSApplicationDidFinishLaunchingNotification"),
    };
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel] };
    if !responds {
        return;
    }
    let note = notification(&NSString::from_str(name), Some(app));
    // SAFETY: both delegate methods take the notification.
    unsafe {
        match when {
            Launch::Will => msg_send![&*delegate, applicationWillFinishLaunching: &*note],
            Launch::Did => msg_send![&*delegate, applicationDidFinishLaunching: &*note],
        }
    }
}

fn run(app: &NSApplicationImpl) {
    if app.ivars().running.replace(true) {
        return;
    }
    // SAFETY: finishLaunching takes nothing.
    let _: () = unsafe { msg_send![app, finishLaunching] };
    tell_delegate(app, Launch::Did);
    while app.ivars().running.get() {
        autoreleasepool(|_| turn());
    }
}

/// One turn of the loop: wait, handle input, fire timers, display.
fn turn() {
    for msg in wait() {
        handle(msg);
    }
    fire_due_timers(Instant::now());
    for window in windows() {
        window::display_if_needed(window::imp(&window));
    }
}

/// Wait for the render thread or the next timer, whichever comes first.
fn wait() -> Vec<FromRender> {
    let timeout = next_timer_deadline().map(|d| d.saturating_duration_since(Instant::now()));
    BACKEND.with(|b| {
        let Some(backend) = b.get() else {
            // Nothing on screen yet: only timers can happen.
            match timeout {
                Some(t) => std::thread::sleep(t),
                None => std::thread::park(),
            }
            return Vec::new();
        };
        let first = match timeout {
            Some(t) => backend.rx.recv_timeout(t),
            None => backend.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match first {
            Ok(msg) => std::iter::once(msg).chain(backend.rx.try_iter()).collect(),
            Err(RecvTimeoutError::Timeout) => Vec::new(),
            Err(RecvTimeoutError::Disconnected) => {
                eprintln!("sidestep: the render thread stopped");
                std::process::exit(1);
            }
        }
    })
}

fn find_window(id: u32) -> Option<Retained<NSWindow>> {
    windows().into_iter().find(|w| window::imp(w).id() == id)
}

fn handle(msg: FromRender) {
    match msg {
        FromRender::Configure { window, width, height } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).configure(width, height);
            }
        }
        FromRender::Frame { window } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).frame_done();
            }
        }
        FromRender::Button { window, x, y, button, pressed } => {
            let Some(w) = find_window(window) else { return };
            // Linux input codes: BTN_LEFT, BTN_RIGHT, then the rest.
            let (number, kind) = match (button, pressed) {
                (0x110, true) => (0, NSEventType::LeftMouseDown),
                (0x110, false) => (0, NSEventType::LeftMouseUp),
                (0x111, true) => (1, NSEventType::RightMouseDown),
                (0x111, false) => (1, NSEventType::RightMouseUp),
                (b, true) => (b as isize - 0x110, NSEventType::OtherMouseDown),
                (b, false) => (b as isize - 0x110, NSEventType::OtherMouseUp),
            };
            window::imp(&w).pointer(kind, x, y, number, 0.0);
        }
        FromRender::Motion { window, x, y } => {
            let Some(w) = find_window(window) else { return };
            let w = window::imp(&w);
            if w.wants_drags() {
                w.pointer(NSEventType::LeftMouseDragged, x, y, 0, 0.0);
            }
        }
        FromRender::Scroll { window, x, y, dy } => {
            if let Some(w) = find_window(window) {
                // Wayland counts toward the bottom; AppKit toward the top.
                window::imp(&w).pointer(NSEventType::ScrollWheel, x, y, 0, -dy);
            }
        }
        FromRender::CloseRequested { window } => {
            if let Some(w) = find_window(window) {
                w.performClose(None);
            }
        }
    }
}

/// After a window closes: quit if it was the last and the delegate says so.
pub(crate) fn window_closed() {
    if !windows().is_empty() {
        return;
    }
    let app = shared();
    let Some(delegate) = app_impl(&app).ivars().delegate.borrow().clone() else { return };
    let sel = sel!(applicationShouldTerminateAfterLastWindowClosed:);
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel] };
    // SAFETY: the delegate method takes the application and returns BOOL.
    if responds && unsafe { msg_send![&*delegate, applicationShouldTerminateAfterLastWindowClosed: &*app] } {
        app.terminate(None);
    }
}

fn terminate(app: &NSApplicationImpl) {
    if let Some(delegate) = app.ivars().delegate.borrow().clone() {
        // SAFETY: respondsToSelector: takes a selector and returns BOOL.
        let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(applicationWillTerminate:)] };
        if responds {
            let note = notification(&NSString::from_str("NSApplicationWillTerminateNotification"), Some(app));
            // SAFETY: the delegate method takes the notification.
            let _: () = unsafe { msg_send![&*delegate, applicationWillTerminate: &*note] };
        }
    }
    std::process::exit(0);
}
