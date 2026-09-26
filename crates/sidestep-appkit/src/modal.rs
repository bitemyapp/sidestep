//! Modal loops, modal sessions, sheets, and the wait for a reply to
//! `applicationShouldTerminate:`.
//!
//! Modal loops (`runModalForWindow:`) and sessions (`beginModalSessionForWindow:`,
//! run a pass at a time with `runModalSession:`) share one stack, innermost
//! last. A loop runs the main loop in `NSModalPanelRunLoopMode` until the
//! session is stopped; `stopModal`, `stopModalWithCode:` and `abortModal`
//! decide the innermost and stop the run loop's innermost run, so a timer's
//! callout ends it at once. A session's passes answer
//! `NSModalResponseContinue` until it is stopped and its response from
//! then on, until `endModalSession:` takes it off the stack. While any is
//! on the stack, input goes only to the innermost modal window, the windows
//! and sheets it holds, and windows that work when modal.
//!
//! Sheets are window-modal and asynchronous: `beginSheet:completionHandler:`
//! attaches the sheet to its parent and returns; `endSheet:returnCode:`
//! takes it off, calls the handler with the code, posts
//! `NSWindowDidEndSheetNotification` and attaches the next sheet waiting,
//! if any (checked on macOS by `conformance/tests/appkit_windows.rs`).
//! While a sheet is attached its parent refuses mouse input and the sheet
//! is key in its place. On screen, the sheet is part of its parent: a
//! subsurface placed top-centre under the title bar (see `backend::sheet`).
//! A parent that isn't on screen can't hold a subsurface, so the sheet then
//! shows as a window of its own; to the program it is the parent's sheet
//! all the same, with the same notifications, as on macOS. An ended sheet
//! has no parent any more, though it stays `isSheet`.
//!
//! `terminate:` asks the delegate's `applicationShouldTerminate:`: now
//! posts `NSApplicationWillTerminateNotification` and exits, cancel
//! returns, and later runs the loop in `NSModalPanelRunLoopMode` until
//! `replyToApplicationShouldTerminate:` (`conformance/tests/appkit_events.rs`).

use std::cell::{Cell, RefCell};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, Message, msg_send, sel};
use objc2_app_kit::{
    NSApplicationTerminateReply, NSEvent, NSEventType, NSModalResponse, NSModalResponseContinue, NSModalSession,
    NSWindow,
};

use crate::app::{self, NSApplicationImpl};
use crate::event_loop;
use crate::notifications::name;
use crate::window;
use crate::window_events::{self, SheetEntry};

/// A modal loop or session on the stack.
struct Modal {
    window: Retained<NSWindow>,
    /// Decided by `stopModal` and its kin.
    response: Option<NSModalResponse>,
    /// The session's handle, for a session; a loop has none.
    session: Option<Box<Session>>,
    /// The session lent the window a parent to be placed over.
    lent: bool,
}

/// What `beginModalSessionForWindow:` hands out: an address only compared.
struct Session(#[allow(dead_code)] u8);

thread_local! {
    static MODALS: RefCell<Vec<Modal>> = const { RefCell::new(Vec::new()) };
    /// Inside `terminate:`, and the reply to a later answer once it comes.
    static TERMINATING: Cell<bool> = const { Cell::new(false) };
    static REPLY: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Put `window` on the modal stack and show it over the key window.
fn push(window: &NSWindow, session: Option<Box<Session>>) -> usize {
    // Over the window the session came from, where compositors center it.
    let modal = window::imp(window);
    let lent = modal.transient().is_none();
    if lent && let Some(key) = app::key_window().filter(|k| !std::ptr::eq(&**k, window)) {
        modal.set_transient(Some(&key));
    }
    let depth = MODALS.with(|m| {
        let mut m = m.borrow_mut();
        m.push(Modal { window: window.retain(), response: None, session, lent });
        m.len()
    });
    window.makeKeyAndOrderFront(None);
    depth
}

/// Take the entry at `depth` (1-based) off the stack, and those above it.
fn pop_to(depth: usize) {
    let gone: Vec<Modal> = MODALS.with(|m| {
        let mut m = m.borrow_mut();
        let at = depth.saturating_sub(1).min(m.len());
        m.drain(at..).collect()
    });
    for modal in &gone {
        if modal.lent {
            window::imp(&modal.window).set_transient(None);
        }
    }
    // Dropped outside the borrow: releasing may run arbitrary code.
    drop(gone);
}

fn decided(depth: usize) -> Option<NSModalResponse> {
    MODALS.with(|m| m.borrow().get(depth - 1).and_then(|e| e.response))
}

/// Run the loop in `NSModalPanelRunLoopMode` for `window` alone until the
/// modal session ends.
pub(crate) fn run_modal(window: &NSWindow) -> NSModalResponse {
    let depth = push(window, None);
    event_loop::run_while(event_loop::modal_mode(), &|| decided(depth).is_none());
    let response = decided(depth).expect("the modal loop ended with a response");
    pop_to(depth);
    response
}

/// `beginModalSessionForWindow:`.
pub(crate) fn begin_session(window: &NSWindow) -> NSModalSession {
    let session = Box::new(Session(0));
    let handle: *const Session = &*session;
    push(window, Some(session));
    handle.cast_mut().cast()
}

fn session_depth(session: NSModalSession) -> Option<usize> {
    let handle = session.cast::<Session>().cast_const();
    MODALS.with(|m| {
        m.borrow().iter().position(|e| e.session.as_deref().is_some_and(|s| std::ptr::eq(s, handle))).map(|i| i + 1)
    })
}

/// `runModalSession:`: one pass of the loop in the modal panel mode,
/// without waiting, then the session's response, or
/// `NSModalResponseContinue` while it goes on.
pub(crate) fn run_session(session: NSModalSession) -> NSModalResponse {
    let Some(depth) = session_depth(session) else { return NSModalResponseContinue };
    if decided(depth).is_none() {
        event_loop::run_once(event_loop::modal_mode());
    }
    decided(depth).unwrap_or(NSModalResponseContinue)
}

/// `endModalSession:`.
pub(crate) fn end_session(session: NSModalSession) {
    if let Some(depth) = session_depth(session) {
        pop_to(depth);
    }
}

/// End the innermost modal loop or session with `response`.
pub(crate) fn stop_modal(response: NSModalResponse) {
    MODALS.with(|m| {
        if let Some(modal) = m.borrow_mut().last_mut() {
            modal.response.get_or_insert(response);
        }
    });
    event_loop::stop_innermost();
}

/// End the modal loop or session for `window` with `response`, wherever
/// it is on the stack (a save panel answered by the desktop while another
/// modal loop runs above it ends when that one does).
pub(crate) fn stop_window(window: &NSWindow, response: NSModalResponse) {
    MODALS.with(|m| {
        if let Some(modal) = m.borrow_mut().iter_mut().rev().find(|e| std::ptr::eq(&*e.window, window)) {
            modal.response.get_or_insert(response);
        }
    });
    event_loop::stop_innermost();
}

pub(crate) fn modal_window() -> Option<Retained<NSWindow>> {
    MODALS.with(|m| m.borrow().last().map(|e| e.window.clone()))
}

/// During a modal loop or session, input goes only to the modal window,
/// the windows and sheets it holds, and windows that work when modal.
pub(crate) fn allows(event: &NSEvent) -> bool {
    let Some(modal) = modal_window() else { return true };
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
    !input || event.window(MainThreadMarker::from(&*modal)).is_some_and(|w| within(&w, &modal) || w.worksWhenModal())
}

/// `window` is `modal`, or a window or sheet it holds.
pub(crate) fn within(window: &NSWindow, modal: &NSWindow) -> bool {
    let mut window = Some(window.retain());
    while let Some(w) = window {
        if std::ptr::eq(&*w, modal) {
            return true;
        }
        window = w.parentWindow().or_else(|| w.sheetParent());
    }
    false
}

// Sheets.

/// `beginSheet:completionHandler:` and `beginCriticalSheet:…`.
pub(crate) fn begin_sheet(parent: &NSWindow, sheet: &NSWindow, handler: Option<RcBlock<dyn Fn(NSModalResponse)>>) {
    window_events::became_sheet(window::imp(sheet), parent);
    let first = window_events::attached_sheet(window::imp(parent)).is_none();
    window_events::sheets_of(window::imp(parent)).borrow_mut().push(SheetEntry { window: sheet.retain(), handler });
    if first {
        attach(parent, sheet, parent.isKeyWindow());
    }
}

/// Put `sheet` on screen, attached to `parent` (inside it if the parent is
/// on screen, else as a window of its own), giving it the keyboard if
/// `key`.
fn attach(parent: &NSWindow, sheet: &NSWindow, key: bool) {
    crate::notifications::post(name!(NSWindowWillBeginSheetNotification), parent);
    window_events::set_attached(window::imp(sheet), true);
    sheet.orderFront(None);
    if key {
        app::make_key(sheet);
    }
}

/// `endSheet:` and `endSheet:returnCode:`.
pub(crate) fn end_sheet(parent: &NSWindow, sheet: &NSWindow, code: NSModalResponse) {
    let entries = window_events::sheets_of(window::imp(parent));
    let at = entries.borrow().iter().position(|e| std::ptr::eq(&*e.window, sheet));
    let Some(at) = at else { return };
    let was_attached = window_events::is_attached(window::imp(sheet));
    let entry = entries.borrow_mut().remove(at);
    let was_key = sheet.isKeyWindow();
    window_events::set_attached(window::imp(sheet), false);
    sheet.orderOut(None);
    window_events::sheet_ended(window::imp(sheet));
    if let Some(handler) = &entry.handler {
        handler.call((code,));
    }
    if was_attached {
        crate::notifications::post(name!(NSWindowDidEndSheetNotification), parent);
    }
    drop(entry);
    let next = entries.borrow().first().map(|e| e.window.clone());
    match next {
        Some(next) if was_attached => attach(parent, &next, was_key),
        _ if was_key => app::make_key(parent),
        _ => {}
    }
}

/// `terminate:`.
pub(crate) fn terminate(app: &NSApplicationImpl) {
    if TERMINATING.with(|t| t.replace(true)) {
        // Already deciding.
        return;
    }
    struct Decided;
    impl Drop for Decided {
        fn drop(&mut self) {
            TERMINATING.with(|t| t.set(false));
            REPLY.with(|r| r.set(None));
        }
    }
    let _decided = Decided;
    REPLY.with(|r| r.set(None));
    let quit = match should_terminate(app) {
        NSApplicationTerminateReply::TerminateCancel => false,
        NSApplicationTerminateReply::TerminateLater => {
            let replied = || REPLY.with(Cell::get);
            event_loop::run_while(event_loop::modal_mode(), &|| replied().is_none());
            replied() == Some(true)
        }
        _ => true,
    };
    if quit {
        app::tell(app, name!(NSApplicationWillTerminateNotification));
        std::process::exit(0);
    }
}

fn should_terminate(app: &NSApplicationImpl) -> NSApplicationTerminateReply {
    let Some(delegate) = app.delegate_object() else { return NSApplicationTerminateReply::TerminateNow };
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(applicationShouldTerminate:)] };
    if !responds {
        return NSApplicationTerminateReply::TerminateNow;
    }
    let sender: &AnyObject = app;
    // SAFETY: the delegate method takes the application and returns an
    // NSApplicationTerminateReply.
    unsafe { msg_send![&*delegate, applicationShouldTerminate: sender] }
}

/// `replyToApplicationShouldTerminate:`: ends the wait `terminate:` is in,
/// if it is in one.
pub(crate) fn reply_to_terminate(terminate: bool) {
    if TERMINATING.with(Cell::get) {
        REPLY.with(|r| r.set(Some(terminate)));
        event_loop::stop_innermost();
    }
}
