//! `NSApplication` and the main thread's event loop.
//!
//! The loop sleeps until the render thread sends something or the next
//! timer is due, handles what arrived, fires due timers, then gives each
//! window a display pass. Nothing on this thread waits for rendering.
//!
//! Input becomes `NSEvent`s sent through `-[NSApplication sendEvent:]`, which
//! programs may override, to the event's window. Keys go to the window with
//! the keyboard; the application is active while one of its windows has it.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType, NSResponder, NSWindow,
};
use objc2_foundation::NSString;
use sidestep_foundation::{fire_due_timers, next_timer_deadline, notification};

use crate::backend::{self, Backend};
use crate::protocol::{Cursor, FromRender, ToRender, WindowRequest};
use crate::{event, graphics, window};

thread_local! {
    static BACKEND: OnceCell<Backend> = const { OnceCell::new() };
    static SHARED: OnceCell<Retained<NSApplication>> = const { OnceCell::new() };
    /// Windows on screen, in the order they were shown.
    static WINDOWS: RefCell<Vec<Retained<NSWindow>>> = const { RefCell::new(Vec::new()) };
    /// The event being dispatched, for `currentEvent`.
    static CURRENT_EVENT: RefCell<Option<Retained<NSEvent>>> = const { RefCell::new(None) };
    /// Whether a window of ours had the keyboard after the last batch of input.
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// When to decide the application went inactive, if no window of ours
    /// gets the keyboard back by then.
    static RESIGN_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The cursor set with `-[NSCursor set]`, shown over every window.
    static CURSOR: Cell<Cursor> = const { Cell::new(Cursor::Default) };
}

/// Show `cursor` over every window's content, now and in windows shown
/// later.
pub(crate) fn set_cursor_everywhere(cursor: Cursor) {
    CURSOR.with(|c| c.set(cursor));
    for w in windows() {
        window::imp(&w).set_cursor(cursor);
    }
}

pub(crate) fn cursor() -> Cursor {
    CURSOR.with(Cell::get)
}

/// Send to the render thread, starting it with the first message.
pub(crate) fn send(msg: ToRender) {
    BACKEND.with(|b| {
        let backend = b.get_or_init(|| {
            let backend = backend::start();
            let _ = ANY_THREAD.set(backend.tx.clone());
            backend
        });
        let _ = backend.tx.send(msg);
    });
}

/// The render thread's inbox, for threads other than the main one.
static ANY_THREAD: std::sync::OnceLock<smithay_client_toolkit::reexports::calloop::channel::Sender<ToRender>> =
    std::sync::OnceLock::new();

/// Send to the render thread from any thread, if it's running; if it
/// isn't (no window was ever shown), the message is dropped: without a
/// window there's no Wayland focus to act with.
pub(crate) fn send_if_running(msg: ToRender) {
    if let Some(tx) = ANY_THREAD.get() {
        let _ = tx.send(msg);
    }
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

/// The window on screen with this `windowNumber`.
pub(crate) fn window_by_number(number: isize) -> Option<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().iter().find(|w| window::imp(w).id() as isize == number).cloned())
}

/// The window with the keyboard.
fn key_window() -> Option<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().iter().find(|w| w.isKeyWindow()).cloned())
}

/// Send an event through `-[NSApplication sendEvent:]`.
pub(crate) fn dispatch(event: &NSEvent) {
    let previous = CURRENT_EVENT.with(|c| c.replace(Some(event.retain())));
    shared().sendEvent(event);
    // Dropped outside the borrow: releasing may run arbitrary code.
    let done = CURRENT_EVENT.with(|c| c.replace(previous));
    drop(done);
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
        fn activate_ignoring_other_apps(&self, _flag: bool) {
            activate();
        }

        #[unsafe(method(activate))]
        fn activate(&self) {
            activate();
        }

        #[unsafe(method(isActive))]
        fn is_active(&self) -> bool {
            ACTIVE.with(Cell::get)
        }

        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            let mtm = MainThreadMarker::from(self);
            let Some(window) = event.window(mtm) else { return };
            // Command-key presses are key equivalents first: a view that
            // performs one consumes the key.
            let equivalent = event.r#type() == NSEventType::KeyDown
                && event.modifierFlags().contains(NSEventModifierFlags::Command);
            if !(equivalent && window.performKeyEquivalent(event)) {
                window.sendEvent(event);
            }
        }

        #[unsafe(method_id(currentEvent))]
        fn current_event(&self) -> Option<Retained<NSEvent>> {
            CURRENT_EVENT.with(|c| c.borrow().clone())
        }

        #[unsafe(method_id(keyWindow))]
        fn key_window(&self) -> Option<Retained<NSWindow>> {
            key_window()
        }

        #[unsafe(method_id(mainWindow))]
        fn main_window(&self) -> Option<Retained<NSWindow>> {
            key_window()
        }

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
        autoreleasepool(|_| turn(app));
    }
}

/// One turn of the loop: wait, handle input, fire timers, display.
fn turn(app: &NSApplicationImpl) {
    let batch = wait();
    let resign_due = RESIGN_AT.with(Cell::get).is_some_and(|t| t <= Instant::now());
    if !batch.is_empty() || resign_due {
        let mut batch = batch.into_iter().peekable();
        while let Some(msg) = batch.next() {
            // Moves that another move of the same window follows are
            // coalesced, as AppKit coalesces mouse events: a busy main
            // thread catches up instead of falling behind.
            if let (FromRender::Motion { window, .. }, Some(FromRender::Motion { window: next, .. })) =
                (&msg, batch.peek())
                && window == next
            {
                continue;
            }
            handle(msg);
        }
        update_active(app);
    }
    fire_due_timers(Instant::now());
    for window in windows() {
        window::display_if_needed(window::imp(&window));
    }
}

/// Wait for the render thread or the next timer, whichever comes first.
fn wait() -> Vec<FromRender> {
    let deadline = match (next_timer_deadline(), RESIGN_AT.with(Cell::get)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let timeout = deadline.map(|d| d.saturating_duration_since(Instant::now()));
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
    WINDOWS.with(|w| w.borrow().iter().find(|w| window::imp(w).id() == id).cloned())
}

/// Ask for the keyboard for the key window, or else the first window.
fn activate() {
    let window = key_window().or_else(|| windows().into_iter().next());
    if let Some(w) = window {
        send(ToRender::Request { window: window::imp(&w).id(), request: WindowRequest::Activate });
    }
}

fn handle(msg: FromRender) {
    match msg {
        FromRender::Configure { window, width, height, scale, titlebar, state } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).configure(width, height, scale, titlebar, state);
            }
        }
        FromRender::Frame { window } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).frame_done();
            }
        }
        FromRender::Focus { window, focused } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).set_key(focused);
            }
        }
        FromRender::Key { window, key } => {
            let Some(w) = find_window(window) else { return };
            let event = event::key_event(&w, key);
            dispatch(&event);
        }
        FromRender::Modifiers { window, modifiers, code } => {
            event::set_current_flags(modifiers);
            let Some(w) = find_window(window) else { return };
            let event = event::flags_changed_event(&w, modifiers, code);
            dispatch(&event);
        }
        FromRender::Enter { window, x, y } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).motion(x, y, event::current_flags());
            }
        }
        FromRender::Leave { window } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).pointer_left();
            }
        }
        FromRender::Button { window, x, y, button, pressed, clicks, modifiers } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).button(button, pressed, x, y, clicks, modifiers);
            }
        }
        FromRender::Motion { window, x, y, modifiers } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).motion(x, y, modifiers);
            }
        }
        FromRender::Scroll { window, x, y, dx, dy, wheel, modifiers } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).scroll(x, y, (dx, dy), wheel, modifiers);
            }
        }
        FromRender::CloseRequested { window } => {
            if let Some(w) = find_window(window) {
                w.performClose(None);
            }
        }
        FromRender::PopupDone { window } => {
            if let Some(w) = find_window(window) {
                w.orderOut(None);
            }
        }
    }
}

/// After a batch of input: tell the delegate if the application became
/// active (a window of ours got the keyboard) or stopped being active.
fn update_active(app: &NSApplicationImpl) {
    let active = key_window().is_some();
    if ACTIVE.with(Cell::get) == active {
        RESIGN_AT.with(|r| r.set(None));
        return;
    }
    if !active {
        // Focus moving between two of our windows arrives as a leave and an
        // enter, which can come a moment apart: wait before deciding.
        let now = Instant::now();
        match RESIGN_AT.with(Cell::get) {
            None => {
                RESIGN_AT.with(|r| r.set(Some(now + Duration::from_millis(50))));
                return;
            }
            Some(at) if now < at => return,
            Some(_) => {}
        }
    }
    RESIGN_AT.with(|r| r.set(None));
    ACTIVE.with(|a| a.set(active));
    let Some(delegate) = app.ivars().delegate.borrow().clone() else { return };
    let (sel, name) = if active {
        (sel!(applicationDidBecomeActive:), "NSApplicationDidBecomeActiveNotification")
    } else {
        (sel!(applicationDidResignActive:), "NSApplicationDidResignActiveNotification")
    };
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel] };
    if !responds {
        return;
    }
    let note = notification(&NSString::from_str(name), Some(app));
    // SAFETY: both delegate methods take the notification.
    unsafe {
        if active {
            msg_send![&*delegate, applicationDidBecomeActive: &*note]
        } else {
            msg_send![&*delegate, applicationDidResignActive: &*note]
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
