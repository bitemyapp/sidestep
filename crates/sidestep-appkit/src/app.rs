//! `NSApplication`: its windows, the key and main window, the event queue
//! and modal loops. The loops themselves run on the main thread's run loop
//! (see `event_loop`); nothing on this thread waits for rendering.
//!
//! Input becomes `NSEvent`s sent through `-[NSApplication sendEvent:]`, which
//! programs may override (`+sharedApplication` sent to a subclass makes an
//! instance of it), after the local event monitors: keys to the key window,
//! the rest to the event's window. The key window is the window with the
//! keyboard, if it can become key (or its sheet); one that can't (a
//! borderless window) leaves the key window as it was, and its keys go
//! there, as on macOS. The application is active while it has a key window,
//! and posts its notifications (and so tells its delegate) as that changes.
//!
//! Input and posted events wait in one queue, in order, for AppKit's loops
//! (see `event_loop`): the main loop and modal loops send them, and a
//! nested loop (`nextEventMatchingMask:untilDate:inMode:dequeue:`, as a
//! view tracking a drag runs) takes the ones it asked for, leaving the rest
//! for the loop outside. A modal loop (`runModalForWindow:`) runs in
//! `NSModalPanelRunLoopMode` with input for other windows dropped.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType,
    NSModalResponse, NSModalResponseAbort, NSModalResponseStop, NSModalSession, NSRequestUserAttentionType,
    NSResponder, NSWindow,
};
use objc2_foundation::NSString;
use sidestep_foundation::runloop::Mode;

use crate::backend::{self, Backend};
use crate::event_loop;
use crate::notifications::{Owner, name};
use crate::protocol::{Cursor, FromRender, ToRender, WindowRequest};
use crate::{event, window};

// What holds windows (and through them views, menus and the rest) is
// never dropped when the main thread exits, as on macOS, where a program
// that exits doesn't deallocate its windows: tearing them down then runs
// program code against thread-locals already gone.
thread_local! {
    static BACKEND: OnceCell<Backend> = const { OnceCell::new() };
    static SHARED: ManuallyDrop<OnceCell<Retained<NSApplication>>> = const { ManuallyDrop::new(OnceCell::new()) };
    /// Windows on screen, in the order they were shown.
    static WINDOWS: ManuallyDrop<RefCell<Vec<Retained<NSWindow>>>> = const { ManuallyDrop::new(RefCell::new(Vec::new())) };
    /// Every window the program has, on screen or not, in the order they
    /// were made: `windows` and `windowWithWindowNumber:`.
    static ALL_WINDOWS: RefCell<Vec<Weak<NSWindow>>> = const { RefCell::new(Vec::new()) };
    /// The window with the keyboard, as the render thread names it.
    static FOCUSED: Cell<Option<u32>> = const { Cell::new(None) };
    /// The event being dispatched, for `currentEvent`.
    static CURRENT_EVENT: ManuallyDrop<RefCell<Option<Retained<NSEvent>>>> = const { ManuallyDrop::new(RefCell::new(None)) };
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

/// Send to the render thread, starting it with the first message. Once
/// the main thread is exiting and its connection is gone, there is nobody
/// to tell.
pub(crate) fn send(msg: ToRender) {
    let _ = BACKEND.try_with(|b| {
        let backend = b.get_or_init(|| {
            let backend = backend::start(event_loop::install());
            let _ = ANY_THREAD.set(backend.tx.clone());
            backend
        });
        backend::null::sending();
        let _ = backend.tx.send(msg);
    });
}

/// Call `f` with the render thread's channel to the main thread, if the
/// render thread has started.
pub(crate) fn with_receiver<R>(f: impl FnOnce(&std::sync::mpsc::Receiver<FromRender>) -> R) -> Option<R> {
    BACKEND.with(|b| b.get().map(|backend| f(&backend.rx)))
}

/// The render thread's inbox, for threads other than the main one.
static ANY_THREAD: std::sync::OnceLock<smithay_client_toolkit::reexports::calloop::channel::Sender<ToRender>> =
    std::sync::OnceLock::new();

/// Send to the render thread from any thread, if it's running; if it
/// isn't (no window was ever shown), the message is dropped: without a
/// window there's no Wayland focus to act with.
pub(crate) fn send_if_running(msg: ToRender) {
    if let Some(tx) = ANY_THREAD.get() {
        backend::null::sending();
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
    crate::screen::window_closed(window);
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

/// Every window, for an appearance change to reach.
pub(crate) fn windows_for_appearance() -> Vec<Retained<NSWindow>> {
    all_windows()
}

/// The `i`th window on screen, if there are that many.
fn window_at(i: usize) -> Option<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().get(i).cloned())
}

/// Whether any window is on screen, without making objects: controls'
/// tracking loops ask on every event (see `controls::track`).
pub(crate) fn any_window_on_screen() -> bool {
    WINDOWS.with(|w| !w.borrow().is_empty())
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

pub(crate) fn main_window() -> Option<Retained<NSWindow>> {
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

/// Make `window` the key window (and main, if it can be), as focus moving
/// inside one toplevel does: to a sheet and back.
pub(crate) fn make_key(window: &NSWindow) {
    focus_moved(Some(window));
}

/// The window with the keyboard got it or lost it.
pub(crate) fn keyboard_focus(id: u32, focused: bool) {
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
    // A window with a sheet gives the keyboard to the sheet.
    let w = crate::window_events::deepest_sheet(&w);
    if w.canBecomeKeyWindow() && crate::panel::takes_key_on_focus(&w) {
        focus_moved(Some(&w));
    }
    // The compositor gave another window the keyboard during a modal
    // session: ask for it back, unless that window works when modal (a
    // panel the modal window uses), as the modal filter lets its input
    // through.
    if let Some(modal) = crate::modal::modal_window()
        && !crate::modal::within(&w, &modal)
        && !w.worksWhenModal()
    {
        modal.makeKeyAndOrderFront(None);
    }
}

/// After a batch of input: without a window of ours holding the keyboard,
/// there's no key window.
pub(crate) fn settle_focus() {
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

/// Queue an event input made, for the AppKit loop running to send through
/// `-[NSApplication sendEvent:]` or take.
pub(crate) fn dispatch(event: &NSEvent) {
    QUEUE.with(|q| q.borrow_mut().push_back(event.retain()));
    event_loop::queued();
}

pub(crate) fn has_queued() -> bool {
    QUEUE.with(|q| !q.borrow().is_empty())
}

/// Whether an event `mask` matches waits in the queue.
pub(crate) fn has_queued_matching(mask: NSEventMask) -> bool {
    QUEUE.with(|q| q.borrow().iter().any(|e| matches(mask, e)))
}

fn send_now(event: &NSEvent) {
    if !crate::modal::allows(event) {
        return;
    }
    let previous = CURRENT_EVENT.with(|c| c.replace(Some(event.retain())));
    shared().sendEvent(event);
    // Dropped outside the borrow: releasing may run arbitrary code.
    let done = CURRENT_EVENT.with(|c| c.replace(previous));
    drop(done);
}

/// Send the events nested loops left, in order.
pub(crate) fn send_queued() {
    while let Some(event) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        send_now(&event);
    }
}

pub(crate) fn post_event(event: &NSEvent, at_start: bool) {
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if at_start { q.push_front(event.retain()) } else { q.push_back(event.retain()) }
    });
    // Sent by the AppKit loop running now, or taken by a nested one.
    event_loop::queued();
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

/// Take (or look at) the first queued event `mask` matches; taking it makes
/// it the current event.
pub(crate) fn take_queued(mask: NSEventMask, dequeue: bool) -> Option<Retained<NSEvent>> {
    let found = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        let at = q.iter().position(|e| matches(mask, e))?;
        if dequeue { q.remove(at) } else { q.get(at).cloned() }
    })?;
    if dequeue {
        let previous = CURRENT_EVENT.with(|c| c.replace(Some(found.clone())));
        drop(previous);
    }
    Some(found)
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
}

pub(crate) struct AppIvars {
    /// Weak, as AppKit's is.
    delegate: RefCell<Option<Weak<AnyObject>>>,
    /// The notifications the delegate is registered for.
    registered: crate::notifications::Registered,
    policy: Cell<NSApplicationActivationPolicy>,
    running: Cell<bool>,
    hidden: Cell<bool>,
    /// `finishLaunching` ran, and the first look for events is yet to post
    /// that launching finished.
    launching: Cell<bool>,
    /// The menu whose key equivalents come after the key window's.
    main_menu: RefCell<Option<Retained<AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSApplication"]
    #[ivars = AppIvars]
    pub(crate) struct NSApplicationImpl;

    impl NSApplicationImpl {
        /// Made by `+sharedApplication` (see `load`), as the class it was
        /// sent to.
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(AppIvars {
                delegate: RefCell::new(None),
                registered: Default::default(),
                policy: Cell::new(NSApplicationActivationPolicy::Regular),
                running: Cell::new(false),
                hidden: Cell::new(false),
                launching: Cell::new(false),
                main_menu: RefCell::new(None),
            });
            // SAFETY: NSResponder's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        /// Kept for the key equivalent phase of `sendEvent:`; showing it is
        /// the menus' business.
        #[unsafe(method_id(mainMenu))]
        fn main_menu(&self) -> Option<Retained<AnyObject>> {
            self.ivars().main_menu.borrow().clone()
        }

        #[unsafe(method(setMainMenu:))]
        fn set_main_menu(&self, menu: Option<&AnyObject>) {
            let old = self.ivars().main_menu.replace(menu.map(|m| m.retain()));
            drop(old);
            // Windows show it as a bar (see `menubar`).
            crate::menubar::visibility_changed();
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.delegate_object()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            let old = self.delegate_object();
            let gone = self.ivars().delegate.replace(delegate.map(Weak::new));
            drop(gone);
            let this: &AnyObject = self;
            let registered = &self.ivars().registered;
            crate::notifications::set_delegate(Owner::Application, this, registered, old.as_deref(), delegate);
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
            // Keys go to the key window, if there is one, as on macOS.
            let kind = event.r#type();
            if is_press(kind) {
                crate::tooltip::input();
            }
            let keys = matches!(kind, NSEventType::KeyDown | NSEventType::KeyUp | NSEventType::FlagsChanged);
            let window = if keys { key_window() } else { event.window(mtm) };
            let Some(window) = window else { return };
            // Command-key presses are key equivalents first, the key window's
            // views' and then the main menu's: one that performs it
            // consumes the key. Control keys and function keys are the main
            // menu's first too (see `keyequiv::goes_to_menu`).
            let down = kind == NSEventType::KeyDown;
            let equivalent = down && event.modifierFlags().contains(NSEventModifierFlags::Command);
            let menu = down && crate::keyequiv::goes_to_menu(event);
            if !((equivalent && window.performKeyEquivalent(event)) || (menu && menu_key_equivalent(self, event))) {
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

        /// Posts that launching will finish; that it did is posted when the
        /// program first looks for events, as on macOS.
        #[unsafe(method(finishLaunching))]
        fn finish_launching(&self) {
            self.ivars().launching.set(true);
            tell(self, name!(NSApplicationWillFinishLaunchingNotification));
        }

        #[unsafe(method(run))]
        fn run(&self) {
            run(self);
        }

        /// Inside a modal loop, ends that loop instead.
        #[unsafe(method(stop:))]
        fn stop(&self, _sender: Option<&AnyObject>) {
            if crate::modal::modal_window().is_some() {
                crate::modal::stop_modal(NSModalResponseStop);
            } else if self.ivars().running.replace(false) {
                event_loop::stop_innermost();
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
            let delegate = self.delegate_object();
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
            tell(self, name!(NSApplicationWillHideNotification));
            for window in windows() {
                send(ToRender::Request { window: window::imp(&window).id(), request: WindowRequest::Minimize });
            }
            tell(self, name!(NSApplicationDidHideNotification));
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
            crate::modal::run_modal(window)
        }

        #[unsafe(method(stopModal))]
        fn stop_modal(&self) {
            crate::modal::stop_modal(NSModalResponseStop);
        }

        #[unsafe(method(stopModalWithCode:))]
        fn stop_modal_with_code(&self, code: NSModalResponse) {
            crate::modal::stop_modal(code);
        }

        #[unsafe(method(abortModal))]
        fn abort_modal(&self) {
            crate::modal::stop_modal(NSModalResponseAbort);
        }

        #[unsafe(method(beginModalSessionForWindow:))]
        fn begin_modal_session_for_window(&self, window: &NSWindow) -> NSModalSession {
            crate::modal::begin_session(window)
        }

        #[unsafe(method(runModalSession:))]
        fn run_modal_session(&self, session: NSModalSession) -> NSModalResponse {
            crate::modal::run_session(session)
        }

        #[unsafe(method(endModalSession:))]
        fn end_modal_session(&self, session: NSModalSession) {
            crate::modal::end_session(session);
        }

        #[unsafe(method_id(modalWindow))]
        fn modal_window(&self) -> Option<Retained<NSWindow>> {
            crate::modal::modal_window()
        }

        #[unsafe(method_id(nextEventMatchingMask:untilDate:inMode:dequeue:))]
        fn next_event_matching_mask(
            &self,
            mask: NSEventMask,
            until: Option<&AnyObject>,
            mode: &NSString,
            dequeue: bool,
        ) -> Option<Retained<NSEvent>> {
            event_loop::next_event(mask, deadline(until), dequeue, Mode::from_ns(mode))
        }

        #[unsafe(method(postEvent:atStart:))]
        fn post_event(&self, event: &NSEvent, at_start: bool) {
            post_event(event, at_start);
        }

        #[unsafe(method(discardEventsMatchingMask:beforeEvent:))]
        fn discard_events_matching_mask(&self, mask: NSEventMask, last: Option<&NSEvent>) {
            discard_events(mask, last);
        }

        // NSAppearanceCustomization.

        #[unsafe(method_id(appearance))]
        fn appearance(&self) -> Option<Retained<objc2_app_kit::NSAppearance>> {
            crate::appearance::app_appearance().map(crate::appearance::get)
        }

        #[unsafe(method(setAppearance:))]
        fn set_appearance(&self, appearance: Option<&objc2_app_kit::NSAppearance>) {
            crate::appearance::set_app_appearance(appearance.map(crate::appearance::id_of));
            crate::appearance::refresh_all();
        }

        #[unsafe(method_id(effectiveAppearance))]
        fn effective_appearance(&self) -> Retained<objc2_app_kit::NSAppearance> {
            crate::appearance::get(crate::appearance::app_effective())
        }

        #[unsafe(method(terminate:))]
        fn terminate(&self, _sender: Option<&AnyObject>) {
            crate::modal::terminate(self);
        }

        #[unsafe(method(replyToApplicationShouldTerminate:))]
        fn reply_to_application_should_terminate(&self, terminate: bool) {
            crate::modal::reply_to_terminate(terminate);
        }

        /// On, as on GNOME, unless `SIDESTEP_FULL_KEYBOARD_ACCESS=0` (see
        /// `controls::focus`).
        #[unsafe(method(isFullKeyboardAccessEnabled))]
        fn is_full_keyboard_access_enabled(&self) -> bool {
            crate::controls::focus::full_keyboard_access()
        }
    }

    unsafe impl NSObjectProtocol for NSApplicationImpl {}
);

/// A click or a key going down.
fn is_press(kind: NSEventType) -> bool {
    matches!(
        kind,
        NSEventType::KeyDown | NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown
    )
}

/// The main menu's turn at a key equivalent, once there is a main menu.
fn menu_key_equivalent(app: &NSApplicationImpl, event: &NSEvent) -> bool {
    let this: &AnyObject = app;
    // SAFETY: mainMenu takes nothing and returns a menu or nil.
    let menu: Option<Retained<AnyObject>> = unsafe { msg_send![this, mainMenu] };
    // SAFETY: performKeyEquivalent: takes an event and returns BOOL.
    menu.is_some_and(|m| {
        responds(&m, sel!(performKeyEquivalent:)) && unsafe { msg_send![&*m, performKeyEquivalent: event] }
    })
}

/// Give `NSApplication` its `+sharedApplication`: a method taking the class
/// it was sent to, so a subclass's makes an instance of that subclass. A
/// `define_class!` class method doesn't see its receiver, so it is added by
/// hand when the class loads.
pub(crate) fn load() {
    let class = NSApplicationImpl::class();
    /// `+sharedApplication`.
    unsafe extern "C-unwind" fn shared_application(class: &AnyClass, _cmd: Sel) -> *mut NSApplication {
        // The application lives as long as the program, so a reference that
        // isn't counted stays good.
        Retained::as_ptr(&shared_as(class)).cast_mut()
    }
    // SAFETY: the implementation takes the receiver and selector and
    // returns an object, as the type encoding says; it is added to the
    // metaclass, so it is a class method.
    let added = unsafe {
        let imp: objc2::runtime::Imp = std::mem::transmute(
            shared_application as unsafe extern "C-unwind" fn(&AnyClass, Sel) -> *mut NSApplication,
        );
        let meta = (class.metaclass() as *const AnyClass).cast_mut();
        objc2::ffi::class_addMethod(meta, sel!(sharedApplication), imp, c"@@:".as_ptr())
    };
    assert!(added.as_bool(), "sidestep: +[NSApplication sharedApplication] was already defined");
}

/// When a nested loop given `date` stops waiting: at once for none, never
/// for a date too far off to matter.
pub(crate) fn deadline(date: Option<&AnyObject>) -> Option<Instant> {
    let now = Instant::now();
    let Some(date) = date else { return Some(now) };
    // SAFETY: NSDate's timeIntervalSinceNow returns seconds.
    let seconds: f64 = unsafe { msg_send![date, timeIntervalSinceNow] };
    if seconds.is_nan() || seconds > 1e8 { None } else { Some(now + Duration::from_secs_f64(seconds.max(0.0))) }
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
/// target, the first that has it among the key window's responder chain
/// (the window and its controller at its end) and the window's delegate,
/// then the same of the main window when it isn't the key window (a panel
/// is key over a document window), then the application and its delegate.
fn target_for_action(app: &NSApplicationImpl, action: Sel, target: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
    if let Some(target) = target {
        return responds(target, action).then(|| target.retain());
    }
    let key = key_window();
    if let Some(found) = key.as_deref().and_then(|w| target_in_window(w, action)) {
        return Some(found);
    }
    let main = main_window().filter(|m| key.as_deref().is_none_or(|k| !std::ptr::eq(k, &**m)));
    if let Some(found) = main.as_deref().and_then(|w| target_in_window(w, action)) {
        return Some(found);
    }
    let this: &AnyObject = app;
    if responds(this, action) {
        return Some(this.retain());
    }
    app.delegate_object().filter(|d| responds(d, action))
}

/// The first in `window`'s responder chain that has `action`, else the
/// window's delegate if it has it.
fn target_in_window(window: &NSWindow, action: Sel) -> Option<Retained<AnyObject>> {
    let mut responder = window.firstResponder();
    while let Some(r) = responder {
        if responds(&r, action) {
            return Some(Retained::into_super(Retained::into_super(r)));
        }
        // SAFETY: nextResponder returns a responder or nil.
        responder = unsafe { r.nextResponder() };
    }
    window::imp(window).delegate_object().filter(|d| responds(d, action))
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
    tell(app, name!(NSApplicationWillUnhideNotification));
    if activating {
        activate();
    }
    tell(app, name!(NSApplicationDidUnhideNotification));
}

/// Post a notification from the application; its delegate is among the
/// observers (see `notifications`).
pub(crate) fn tell(app: &NSApplicationImpl, name: &NSString) {
    crate::notifications::post(name, app);
}

impl NSApplicationImpl {
    pub(crate) fn delegate_object(&self) -> Option<Retained<AnyObject>> {
        self.ivars().delegate.borrow().as_ref().and_then(Weak::load)
    }
}

/// The first look for events after `finishLaunching`: launching finished.
pub(crate) fn launched() {
    let Some(app) = SHARED.with(|s| s.get().cloned()) else { return };
    let app = app_impl(&app);
    if app.ivars().launching.replace(false) {
        // The desktop's portal, ready for the first panel or URL.
        crate::portal::prewarm();
        tell(app, name!(NSApplicationDidFinishLaunchingNotification));
    }
}

/// The application, if the program made it.
pub(crate) fn existing() -> Option<Retained<NSApplication>> {
    SHARED.with(|s| s.get().cloned())
}

fn shared() -> Retained<NSApplication> {
    if let Some(app) = SHARED.with(|s| s.get().cloned()) {
        return app;
    }
    crate::load_shell::<NSApplication>();
    shared_as(NSApplication::class())
}

/// The application, made as an instance of `class` (NSApplication or a
/// subclass) by the first call.
fn shared_as(class: &AnyClass) -> Retained<NSApplication> {
    if let Some(app) = SHARED.with(|s| s.get().cloned()) {
        return app;
    }
    MainThreadMarker::new().expect("sidestep: NSApplication belongs to the main thread");
    load_shells();
    event_loop::install();
    // Ask the desktop for its appearance while the program starts.
    crate::settings::start();
    // SAFETY: alloc and init make an instance of the class, an
    // NSApplication; its initializer (NSApplicationImpl's, inherited or
    // called by the subclass's) sets up the ivars.
    let app: Retained<NSApplication> = unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        let made: Option<Retained<AnyObject>> = msg_send![allocated, init];
        Retained::cast_unchecked(made.expect("sidestep: NSApplication's initializer returned nil"))
    };
    SHARED.with(|s| s.get_or_init(|| app).clone())
}

fn app_impl(app: &NSApplication) -> &NSApplicationImpl {
    // SAFETY: NSApplication is NSApplicationImpl's class.
    unsafe { &*(app as *const NSApplication).cast::<NSApplicationImpl>() }
}

fn run(app: &NSApplicationImpl) {
    if app.ivars().running.replace(true) {
        return;
    }
    // SAFETY: finishLaunching takes nothing.
    let _: () = unsafe { msg_send![app, finishLaunching] };
    launched();
    event_loop::run_while(Mode::DEFAULT, &|| app.ivars().running.get());
}

/// Give each window on screen its display pass, after having its tracking
/// areas look again at views that moved under the pointer.
pub(crate) fn refresh_windows() {
    // By index, not over a copy of the list: a display may show or hide
    // windows, and the pass allocates nothing when there is nothing to do.
    let mut i = 0;
    while let Some(window) = window_at(i) {
        crate::tracking::refresh(window::imp(&window));
        window::display_if_needed(window::imp(&window));
        i += 1;
    }
}

/// The window on screen the render thread calls `id`; none for messages
/// about an earlier showing.
pub(crate) fn find_window(id: u32) -> Option<Retained<NSWindow>> {
    WINDOWS.with(|w| w.borrow().iter().find(|w| window::imp(w).id() == id).cloned())
}

/// Where keys typed with the keyboard on window `id` go: the key window,
/// which is that window unless it can't become key.
pub(crate) fn keys_window(id: u32) -> Option<Retained<NSWindow>> {
    key_window().or_else(|| find_window(id).filter(|w| w.canBecomeKeyWindow()))
}

/// Ask for the keyboard for the key window, or else the first window.
fn activate() {
    let window = key_window().or_else(|| windows().into_iter().next());
    if let Some(w) = window {
        send(ToRender::Request { window: window::imp(&w).id(), request: WindowRequest::Activate });
    }
}

/// When to decide whether the application went inactive, if a window of
/// ours lost the keyboard and none has it back yet.
pub(crate) fn resign_deadline() -> Option<Instant> {
    RESIGN_AT.with(Cell::get)
}

/// The pause after losing the keyboard is over: decide.
pub(crate) fn resign_if_due() {
    if RESIGN_AT.with(Cell::get).is_some_and(|t| t <= Instant::now()) {
        update_active();
    }
}

/// After a batch of input: tell the delegate if the application became
/// active (a window of ours got the keyboard) or stopped being active.
pub(crate) fn update_active() {
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
    let app = shared();
    let app = app_impl(&app);
    let (will, did) = if active {
        (name!(NSApplicationWillBecomeActiveNotification), name!(NSApplicationDidBecomeActiveNotification))
    } else {
        (name!(NSApplicationWillResignActiveNotification), name!(NSApplicationDidResignActiveNotification))
    };
    tell(app, will);
    ACTIVE.with(|a| a.set(active));
    crate::panel::activity_changed(active, &windows());
    tell(app, did);
}

/// A window on screen closed. Once the event being handled is done, if no
/// window is left on screen, the delegate is asked whether to quit.
pub(crate) fn window_closed() {
    sidestep_foundation::runloop::main().perform(&[Mode::DEFAULT], last_window_check);
}

fn last_window_check() {
    if !windows().is_empty() {
        return;
    }
    let app = shared();
    let Some(delegate) = app_impl(&app).delegate_object() else { return };
    let sel = sel!(applicationShouldTerminateAfterLastWindowClosed:);
    // SAFETY: the delegate method takes the application and returns BOOL.
    if responds(&delegate, sel)
        && unsafe { msg_send![&*delegate, applicationShouldTerminateAfterLastWindowClosed: &*app] }
    {
        app.terminate(None);
    }
}
