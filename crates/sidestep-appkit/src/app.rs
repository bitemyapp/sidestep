//! `NSApplication` and the main thread's event loop.
//!
//! The loop sleeps until the render thread sends something or the next
//! timer is due, handles what arrived, fires due timers, then gives each
//! window a display pass. Nothing on this thread waits for rendering.
//!
//! Input becomes `NSEvent`s sent through `-[NSApplication sendEvent:]`, which
//! programs may override, to the event's window, after the local event
//! monitors. Keys go to the key window: the window with the keyboard, if it
//! can become key; one that can't (a borderless window) leaves the key
//! window as it was, and its keys go there, as on macOS. The application is
//! active while it has a key window.
//!
//! A nested loop (`nextEventMatchingMask:untilDate:inMode:dequeue:`, as a
//! view tracking a drag runs) handles the render thread's messages the same
//! way, but the events input makes wait in a queue: the loop takes the ones
//! it asked for and the main loop sends the rest when it gets back. A modal
//! loop (`runModalForWindow:`) is the main loop with input for other windows
//! dropped.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSModalResponse, NSModalResponseAbort, NSModalResponseStop, NSRequestUserAttentionType, NSResponder, NSWindow,
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
    /// Every window the program has, on screen or not, in the order they
    /// were made: `windows` and `windowWithWindowNumber:`.
    static ALL_WINDOWS: RefCell<Vec<Weak<NSWindow>>> = const { RefCell::new(Vec::new()) };
    /// The window with the keyboard, as the render thread names it.
    static FOCUSED: Cell<Option<u32>> = const { Cell::new(None) };
    /// The event being dispatched, for `currentEvent`.
    static CURRENT_EVENT: RefCell<Option<Retained<NSEvent>>> = const { RefCell::new(None) };
    /// Whether a window of ours had the keyboard after the last batch of input.
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    /// When to decide the application went inactive, if no window of ours
    /// gets the keyboard back by then.
    static RESIGN_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// The cursor set with `-[NSCursor set]`, shown over every window.
    static CURSOR: Cell<Cursor> = const { Cell::new(Cursor::Default) };
    /// Events waiting to be sent or taken: made while a nested loop looked
    /// for others, or posted.
    static QUEUE: RefCell<VecDeque<Retained<NSEvent>>> = const { RefCell::new(VecDeque::new()) };
    /// Nested loops looking for events; while there are any, input is
    /// queued rather than sent.
    static PUMPING: Cell<u32> = const { Cell::new(0) };
    /// Modal loops, innermost last: the window and, once it's decided, the
    /// response.
    static MODALS: RefCell<Vec<(Retained<NSWindow>, Option<NSModalResponse>)>> = const { RefCell::new(Vec::new()) };
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

/// The application is active: one of its windows has the keyboard.
pub(crate) fn is_active() -> bool {
    ACTIVE.with(Cell::get)
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

/// A window was made: the application has it until it's freed.
pub(crate) fn window_made(window: &NSWindow) {
    ALL_WINDOWS.with(|all| {
        let mut all = all.borrow_mut();
        // Forget windows that are gone, now and then.
        if all.len() >= 16 && all.len().is_power_of_two() {
            all.retain(|w| w.load().is_some());
        }
        all.push(Weak::new(window));
    });
}

/// Every window the program has, in the order they were made.
fn all_windows() -> Vec<Retained<NSWindow>> {
    ALL_WINDOWS.with(|all| all.borrow().iter().filter_map(Weak::load).collect())
}

pub(crate) fn add_window(window: &NSWindow) {
    WINDOWS.with(|w| w.borrow_mut().push(window.retain()));
}

pub(crate) fn remove_window(window: &NSWindow) {
    crate::momentum::window_closed(window);
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

/// The window with this `windowNumber`, on screen or not.
pub(crate) fn window_by_number(number: isize) -> Option<Retained<NSWindow>> {
    if number <= 0 {
        return None;
    }
    ALL_WINDOWS.with(|all| all.borrow().iter().filter_map(Weak::load).find(|w| window::imp(w).number() == number))
}

/// The key window: the one keys go to.
pub(crate) fn key_window() -> Option<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().iter().find(|w| window::imp(w).is_key()).cloned())
}

fn main_window() -> Option<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().iter().find(|w| window::imp(w).is_main()).cloned())
}

/// Make `window` the key window (and the main window, if it can be), or
/// have no key window.
fn focus_moved(window: Option<&NSWindow>) {
    let old = key_window();
    let same = match (&old, window) {
        (Some(a), Some(b)) => std::ptr::eq(&**a, b),
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    if let Some(old) = &old {
        window::imp(old).set_key(false);
    }
    match window {
        Some(new) => {
            window::imp(new).set_key(true);
            if new.canBecomeMainWindow() {
                if let Some(main) = main_window().filter(|m| !std::ptr::eq(&**m, new)) {
                    window::imp(&main).set_main(false);
                }
                window::imp(new).set_main(true);
            }
        }
        None => {
            if let Some(main) = main_window() {
                window::imp(&main).set_main(false);
            }
        }
    }
}

/// The window with the keyboard got it or lost it.
fn keyboard_focus(id: u32, focused: bool) {
    if !focused {
        // Whether another window of ours gets it is settled once the batch
        // is handled (see `settle_focus`).
        if FOCUSED.with(Cell::get) == Some(id) {
            FOCUSED.with(|f| f.set(None));
        }
        return;
    }
    FOCUSED.with(|f| f.set(Some(id)));
    let Some(w) = find_window(id) else { return };
    if w.canBecomeKeyWindow() {
        focus_moved(Some(&w));
    }
    // The compositor gave another window the keyboard during a modal
    // session: ask for it back.
    if let Some(modal) = modal_window()
        && !within(&w, &modal)
    {
        modal.makeKeyAndOrderFront(None);
    }
}

/// After a batch of input: without a window of ours holding the keyboard,
/// there's no key window.
fn settle_focus() {
    let held = FOCUSED.with(Cell::get).and_then(find_window);
    if held.is_none() {
        focus_moved(None);
    }
}

/// A window is leaving the screen: it's no longer key or main.
pub(crate) fn window_hidden(window: &NSWindow) {
    let w = window::imp(window);
    if FOCUSED.with(Cell::get) == Some(w.id()) {
        FOCUSED.with(|f| f.set(None));
    }
    if w.is_key() {
        focus_moved(None);
    }
    w.set_main(false);
}

/// Send an event through `-[NSApplication sendEvent:]`, or queue it for
/// the nested loop that's looking for events.
pub(crate) fn dispatch(event: &NSEvent) {
    if PUMPING.with(Cell::get) > 0 {
        QUEUE.with(|q| q.borrow_mut().push_back(event.retain()));
    } else {
        send_now(event);
    }
}

fn send_now(event: &NSEvent) {
    if !allowed_by_modal(event) {
        return;
    }
    let previous = CURRENT_EVENT.with(|c| c.replace(Some(event.retain())));
    shared().sendEvent(event);
    // Dropped outside the borrow: releasing may run arbitrary code.
    let done = CURRENT_EVENT.with(|c| c.replace(previous));
    drop(done);
}

/// During a modal loop, input goes only to the modal window and the
/// windows it holds.
fn allowed_by_modal(event: &NSEvent) -> bool {
    let Some(modal) = MODALS.with(|m| m.borrow().last().map(|(w, _)| w.clone())) else { return true };
    let input = matches!(
        event.r#type(),
        NSEventType::KeyDown
            | NSEventType::KeyUp
            | NSEventType::FlagsChanged
            | NSEventType::LeftMouseDown
            | NSEventType::LeftMouseUp
            | NSEventType::RightMouseDown
            | NSEventType::RightMouseUp
            | NSEventType::OtherMouseDown
            | NSEventType::OtherMouseUp
            | NSEventType::LeftMouseDragged
            | NSEventType::RightMouseDragged
            | NSEventType::OtherMouseDragged
            | NSEventType::MouseMoved
            | NSEventType::ScrollWheel
    );
    !input || event.window(MainThreadMarker::from(&*modal)).is_some_and(|w| within(&w, &modal))
}

/// `window` is `modal` or a window it holds.
fn within(window: &NSWindow, modal: &NSWindow) -> bool {
    let mut window = Some(window.retain());
    while let Some(w) = window {
        if std::ptr::eq(&*w, modal) {
            return true;
        }
        window = w.parentWindow();
    }
    false
}

/// Send the events nested loops left, in order.
fn send_queued() {
    while let Some(event) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        send_now(&event);
    }
}

pub(crate) fn post_event(event: &NSEvent, at_start: bool) {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if at_start { q.push_front(event.retain()) } else { q.push_back(event.retain()) }
    });
}

/// Drop queued events `mask` matches, up to `last` if given.
pub(crate) fn discard_events(mask: NSEventMask, last: Option<&NSEvent>) {
    let before = last.map(|e| e.timestamp());
    let gone: VecDeque<_> = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let (gone, kept): (VecDeque<_>, VecDeque<_>) =
            q.drain(..).partition(|e| matches(mask, e) && before.is_none_or(|t| e.timestamp() <= t));
        *q = kept;
        gone
    });
    drop(gone);
}

fn matches(mask: NSEventMask, event: &NSEvent) -> bool {
    let kind = event.r#type().0;
    kind < 64 && mask.0 & (1 << kind) != 0
}

/// Take (or look at) the first queued event `mask` matches, handling the
/// render thread's messages until one comes or `deadline` passes (None:
/// never). Events it doesn't want wait for the main loop. Timers fire only
/// when `timers` is set, as in the default run loop mode.
pub(crate) fn next_event(
    mask: NSEventMask,
    deadline: Option<Instant>,
    dequeue: bool,
    timers: bool,
) -> Option<Retained<NSEvent>> {
    let app = shared();
    let app = app_impl(&app);
    let mut looked = false;
    loop {
        let found = QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            let at = q.iter().position(|e| matches(mask, e))?;
            if dequeue { q.remove(at) } else { q.get(at).cloned() }
        });
        if let Some(event) = found {
            if dequeue {
                let previous = CURRENT_EVENT.with(|c| c.replace(Some(event.clone())));
                drop(previous);
            }
            return Some(event);
        }
        let now = Instant::now();
        let expired = deadline.is_some_and(|d| d <= now);
        if expired && looked {
            return None;
        }
        let until = if expired {
            Some(now)
        } else {
            let timer = if timers { next_timer_deadline() } else { None };
            earliest(earliest(deadline, timer), RESIGN_AT.with(Cell::get))
        };
        struct Pumping;
        impl Drop for Pumping {
            fn drop(&mut self) {
                PUMPING.with(|p| p.set(p.get() - 1));
            }
        }
        PUMPING.with(|p| p.set(p.get() + 1));
        let pumping = Pumping;
        crate::pasteboard::offer_changes();
        take_batch(app, wait(until));
        if timers {
            fire_due_timers(Instant::now());
        }
        refresh_windows();
        drop(pumping);
        looked = true;
    }
}

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Run the main loop for `window` alone until the modal session ends.
fn run_modal(app: &NSApplicationImpl, window: &NSWindow) -> NSModalResponse {
    MODALS.with(|m| m.borrow_mut().push((window.retain(), None)));
    // Over the window the session came from, where compositors center it.
    let modal = window::imp(window);
    let lent = modal.transient().is_none();
    if lent && let Some(key) = key_window().filter(|k| !std::ptr::eq(&**k, window)) {
        modal.set_transient(Some(&key));
    }
    window.makeKeyAndOrderFront(None);
    let response = loop {
        let decided = MODALS.with(|m| m.borrow().last().and_then(|(_, r)| *r));
        if let Some(response) = decided {
            break response;
        }
        autoreleasepool(|_| turn(app));
    };
    let ended = MODALS.with(|m| m.borrow_mut().pop());
    drop(ended);
    if lent {
        modal.set_transient(None);
    }
    response
}

/// End the innermost modal loop with `response`.
fn stop_modal(response: NSModalResponse) {
    MODALS.with(|m| {
        if let Some((_, decided)) = m.borrow_mut().last_mut() {
            decided.get_or_insert(response);
        }
    });
}

fn modal_window() -> Option<Retained<NSWindow>> {
    MODALS.with(|m| m.borrow().last().map(|(w, _)| w.clone()))
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
    hidden: Cell<bool>,
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
            // Local monitors see it first, and may change or swallow it.
            let Some(event) = event::monitor(event) else { return };
            let event = &*event;
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
            main_window()
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

        /// Inside a modal loop, ends that loop instead.
        #[unsafe(method(stop:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            if modal_window().is_some() {
                stop_modal(NSModalResponseStop);
            } else {
                self.ivars().running.set(false);
            }
        }

        #[unsafe(method(sendAction:to:from:))]
        fn send_action(&self, action: Sel, target: Option<&AnyObject>, sender: Option<&AnyObject>) -> bool {
            send_action(self, action, target, sender)
        }

        #[unsafe(method_id(targetForAction:))]
        fn target_for_action(&self, action: Sel) -> Option<Retained<AnyObject>> {
            target_for_action(self, action, None)
        }

        #[unsafe(method_id(targetForAction:to:from:))]
        fn target_for_action_to_from(
            &self,
            action: Sel,
            target: Option<&AnyObject>,
            _sender: Option<&AnyObject>,
        ) -> Option<Retained<AnyObject>> {
            target_for_action(self, action, target)
        }

        /// The application, then its delegate.
        #[unsafe(method(tryToPerform:with:))]
        fn try_to_perform(&self, action: Sel, object: Option<&AnyObject>) -> bool {
            let delegate = self.ivars().delegate.borrow().clone();
            let this: &AnyObject = self;
            [Some(this.retain()), delegate].into_iter().flatten().any(|o| perform(&o, action, object))
        }

        #[unsafe(method_id(windows))]
        fn windows(&self) -> Retained<AnyObject> {
            array_of(&all_windows())
        }

        #[unsafe(method_id(windowWithWindowNumber:))]
        fn window_with_window_number(&self, number: isize) -> Option<Retained<NSWindow>> {
            window_by_number(number)
        }

        /// Wayland can't hide windows; they're minimized, and the
        /// application asks to be activated when unhidden.
        #[unsafe(method(hide:))]
        fn hide(&self, _sender: Option<&AnyObject>) {
            if self.ivars().hidden.replace(true) {
                return;
            }
            tell(self, sel!(applicationWillHide:), "NSApplicationWillHideNotification");
            for window in windows() {
                send(ToRender::Request { window: window::imp(&window).id(), request: WindowRequest::Minimize });
            }
            tell(self, sel!(applicationDidHide:), "NSApplicationDidHideNotification");
        }

        #[unsafe(method(unhide:))]
        fn unhide(&self, _sender: Option<&AnyObject>) {
            unhide(self, true);
        }

        #[unsafe(method(unhideWithoutActivation))]
        fn unhide_without_activation(&self) {
            unhide(self, false);
        }

        #[unsafe(method(isHidden))]
        fn is_hidden(&self) -> bool {
            self.ivars().hidden.get()
        }

        // Other programs' windows aren't ours to hide or show.
        #[unsafe(method(hideOtherApplications:))]
        fn hide_other_applications(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(unhideAllApplications:))]
        fn unhide_all_applications(&self, _sender: Option<&AnyObject>) {}

        /// An activation request, which compositors show as a call for
        /// attention when they don't grant it.
        #[unsafe(method(requestUserAttention:))]
        fn request_user_attention(&self, _kind: NSRequestUserAttentionType) -> isize {
            if ACTIVE.with(Cell::get) {
                return 0;
            }
            activate();
            1
        }

        #[unsafe(method(cancelUserAttentionRequest:))]
        fn cancel_user_attention_request(&self, _request: isize) {}

        #[unsafe(method(runModalForWindow:))]
        fn run_modal_for_window(&self, window: &NSWindow) -> NSModalResponse {
            run_modal(self, window)
        }

        #[unsafe(method(stopModal))]
        fn stop_modal(&self) {
            stop_modal(NSModalResponseStop);
        }

        #[unsafe(method(stopModalWithCode:))]
        fn stop_modal_with_code(&self, code: NSModalResponse) {
            stop_modal(code);
        }

        #[unsafe(method(abortModal))]
        fn abort_modal(&self) {
            stop_modal(NSModalResponseAbort);
        }

        #[unsafe(method_id(modalWindow))]
        fn modal_window(&self) -> Option<Retained<NSWindow>> {
            modal_window()
        }

        #[unsafe(method_id(nextEventMatchingMask:untilDate:inMode:dequeue:))]
        fn next_event_matching_mask(
            &self,
            mask: NSEventMask,
            until: Option<&AnyObject>,
            mode: &NSString,
            dequeue: bool,
        ) -> Option<Retained<NSEvent>> {
            next_event(mask, deadline(until), dequeue, default_mode(mode))
        }

        #[unsafe(method(postEvent:atStart:))]
        fn post_event(&self, event: &NSEvent, at_start: bool) {
            post_event(event, at_start);
        }

        #[unsafe(method(discardEventsMatchingMask:beforeEvent:))]
        fn discard_events_matching_mask(&self, mask: NSEventMask, last: Option<&NSEvent>) {
            discard_events(mask, last);
        }

        #[unsafe(method(terminate:))]
        fn terminate(&self, _sender: Option<&AnyObject>) {
            terminate(self);
        }
    }

    unsafe impl NSObjectProtocol for NSApplicationImpl {}
);

/// When a nested loop given `date` stops waiting: at once for none, never
/// for a date too far off to matter.
pub(crate) fn deadline(date: Option<&AnyObject>) -> Option<Instant> {
    let now = Instant::now();
    let Some(date) = date else { return Some(now) };
    // SAFETY: NSDate's timeIntervalSinceNow returns seconds.
    let seconds: f64 = unsafe { msg_send![date, timeIntervalSinceNow] };
    if seconds.is_nan() || seconds > 1e8 { None } else { Some(now + Duration::from_secs_f64(seconds.max(0.0))) }
}

/// Timers fire in nested loops run in the default mode (or the common
/// modes), not in the tracking or modal-panel modes.
pub(crate) fn default_mode(mode: &NSString) -> bool {
    let mode = mode.to_string();
    mode == "kCFRunLoopDefaultMode" || mode == "kCFRunLoopCommonModes"
}

/// Objects as an `NSArray`, which Foundation provides when it has one.
pub(crate) fn array_of<T: objc2::Message>(objects: &[Retained<T>]) -> Retained<AnyObject> {
    let Some(class) = objc2::runtime::AnyClass::get(c"NSArray") else {
        panic!("sidestep: this AppKit method answers with an NSArray, which isn't implemented yet");
    };
    let pointers: Vec<*const T> = objects.iter().map(Retained::as_ptr).collect();
    // SAFETY: arrayWithObjects:count: takes that many object pointers,
    // alive for the call.
    unsafe { msg_send![class, arrayWithObjects: pointers.as_ptr(), count: pointers.len()] }
}

/// Whom an action goes to: `target` if it has the action, else, for no
/// target, the first in the key window's responder chain (ending with the
/// window and its delegate), the application and its delegate that has it.
fn target_for_action(app: &NSApplicationImpl, action: Sel, target: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
    if let Some(target) = target {
        return responds(target, action).then(|| target.retain());
    }
    if let Some(window) = key_window() {
        let mut responder = window.firstResponder();
        while let Some(r) = responder {
            if responds(&r, action) {
                return Some(Retained::into_super(Retained::into_super(r)));
            }
            // SAFETY: nextResponder returns a responder or nil.
            responder = unsafe { r.nextResponder() };
        }
        if let Some(delegate) = window::imp(&window).delegate_object()
            && responds(&delegate, action)
        {
            return Some(delegate);
        }
    }
    let this: &AnyObject = app;
    if responds(this, action) {
        return Some(this.retain());
    }
    app.ivars().delegate.borrow().clone().filter(|d| responds(d, action))
}

fn send_action(app: &NSApplicationImpl, action: Sel, target: Option<&AnyObject>, sender: Option<&AnyObject>) -> bool {
    let Some(target) = target_for_action(app, action, target) else { return false };
    // SAFETY: action methods take the sender and return nothing.
    unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(&*target, action, (sender,)) };
    true
}

fn responds(object: &AnyObject, action: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: action] }
}

/// Perform `action` with `object` if `target` has it.
pub(crate) fn perform(target: &AnyObject, action: Sel, object: Option<&AnyObject>) -> bool {
    if !responds(target, action) {
        return false;
    }
    // SAFETY: as in `send_action`.
    unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(target, action, (object,)) };
    true
}

fn unhide(app: &NSApplicationImpl, activating: bool) {
    if !app.ivars().hidden.replace(false) {
        return;
    }
    tell(app, sel!(applicationWillUnhide:), "NSApplicationWillUnhideNotification");
    if activating {
        activate();
    }
    tell(app, sel!(applicationDidUnhide:), "NSApplicationDidUnhideNotification");
}

/// Tell the delegate, if it listens, with a notification from the
/// application.
fn tell(app: &NSApplicationImpl, selector: Sel, name: &str) {
    let Some(delegate) = app.ivars().delegate.borrow().clone() else { return };
    if !responds(&delegate, selector) {
        return;
    }
    let note = notification(&NSString::from_str(name), Some(app));
    // SAFETY: application delegate notifications take the notification.
    unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(&*delegate, selector, (&*note,)) };
}

fn shared() -> Retained<NSApplication> {
    SHARED.with(|s| {
        s.get_or_init(|| {
            let mtm = MainThreadMarker::new().expect("sidestep: NSApplication belongs to the main thread");
            load_shells();
            let this = NSApplicationImpl::alloc(mtm).set_ivars(AppIvars {
                delegate: RefCell::new(None),
                policy: Cell::new(NSApplicationActivationPolicy::Regular),
                running: Cell::new(false),
                hidden: Cell::new(false),
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

/// One turn of the loop: send what nested loops left, wait, handle input,
/// fire timers, display.
fn turn(app: &NSApplicationImpl) {
    send_queued();
    crate::pasteboard::offer_changes();
    let deadline = earliest(earliest(next_timer_deadline(), RESIGN_AT.with(Cell::get)), crate::momentum::deadline());
    let batch = wait(deadline);
    take_batch(app, batch);
    crate::momentum::tick(Instant::now());
    fire_due_timers(Instant::now());
    refresh_windows();
}

fn take_batch(app: &NSApplicationImpl, batch: Vec<FromRender>) {
    let resign_due = RESIGN_AT.with(Cell::get).is_some_and(|t| t <= Instant::now());
    if !batch.is_empty() || resign_due {
        // Where each window's last key message is: repeats before another
        // key message (a newer repeat, or the release) are dropped, so keys
        // held while the main thread was busy don't pile up and go on
        // acting after they're released.
        let mut last_keys: Vec<(u32, usize)> = Vec::new();
        for (i, msg) in batch.iter().enumerate() {
            if let FromRender::Key { window, .. } = msg {
                match last_keys.iter_mut().find(|(w, _)| w == window) {
                    Some(last) => last.1 = i,
                    None => last_keys.push((*window, i)),
                }
            }
        }
        let mut batch = batch.into_iter().enumerate().peekable();
        while let Some((i, msg)) = batch.next() {
            // Moves that another move of the same window follows are
            // coalesced, as AppKit coalesces mouse events: a busy main
            // thread catches up instead of falling behind.
            if let (FromRender::Motion { window, .. }, Some((_, FromRender::Motion { window: next, .. }))) =
                (&msg, batch.peek())
                && window == next
            {
                continue;
            }
            if let FromRender::Key { window, key } = &msg
                && key.repeat
                && last_keys.iter().any(|(w, last)| w == window && *last > i)
            {
                continue;
            }
            handle(msg);
        }
        settle_focus();
        update_active(app);
    }
}

fn refresh_windows() {
    for window in windows() {
        // Views that moved this turn may have moved under the pointer.
        crate::tracking::refresh(window::imp(&window));
        window::display_if_needed(window::imp(&window));
    }
}

/// Wait for the render thread until `deadline` (None: as long as it takes).
fn wait(deadline: Option<Instant>) -> Vec<FromRender> {
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

/// The window on screen the render thread calls `id`; none for messages
/// about an earlier showing.
fn find_window(id: u32) -> Option<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().iter().find(|w| window::imp(w).id() == id).cloned())
}

/// Where keys typed with the keyboard on window `id` go: the key window,
/// which is that window unless it can't become key.
fn keys_window(id: u32) -> Option<Retained<NSWindow>> {
    key_window().or_else(|| find_window(id).filter(|w| w.canBecomeKeyWindow()))
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
                crate::momentum::frame(&w);
            }
        }
        FromRender::Focus { window, focused } => keyboard_focus(window, focused),
        FromRender::Key { window, key } => {
            let Some(w) = keys_window(window) else { return };
            let event = event::key_event(&w, key);
            dispatch(&event);
            // Typing moves the caret an input method places its window by.
            crate::inputcontext::update(window::imp(&w));
        }
        FromRender::Modifiers { window, modifiers, code } => {
            event::set_current_flags(modifiers);
            let Some(w) = keys_window(window) else { return };
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
        FromRender::Scroll { window, x, y, dx, dy, wheel, modifiers, phase, velocity, inverted } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).scroll((x, y), (dx, dy), wheel, modifiers, (phase, velocity, inverted));
            }
        }
        FromRender::Pinch { window, x, y, phase, magnification, rotation, modifiers } => {
            if let Some(w) = find_window(window) {
                window::imp(&w).pinch((x, y), phase, magnification, rotation, modifiers);
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
        FromRender::TextInput { window, commit, preedit } => {
            if let Some(w) = find_window(window) {
                crate::inputcontext::apply(window::imp(&w), commit, preedit);
            }
        }
        // Pasteboards and drag and drop (pasteboard.rs, drag.rs).
        FromRender::ProvideSelection { mime, token } => crate::pasteboard::provide_for_render(&mime, token),
        FromRender::DndEnter { window, x, y, mimes, actions } => {
            if let Some(w) = find_window(window) {
                crate::drag::enter(&w, x, y, mimes, actions);
            }
        }
        FromRender::DndMotion { x, y } => crate::drag::motion(x, y),
        FromRender::DndActions { actions } => crate::drag::actions(actions),
        FromRender::DndLeave => crate::drag::leave(),
        FromRender::DndDrop => crate::drag::dropped(),
        // Screens (screen.rs).
        FromRender::ScreensChanged => crate::screen::changed(MainThreadMarker::new().expect("the main thread")),
        FromRender::WindowOutputs { window, outputs } => {
            if let Some(w) = find_window(window) {
                crate::screen::window_outputs(&w, outputs);
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
