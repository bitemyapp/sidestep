//! A window's behaviour toward the program: its delegate and
//! notifications, closing, moving and resizing as the program sees it, and
//! live resizes.
//!
//! `NSWindow` keeps this area's state in one ivar, [`WindowEvents`]; its
//! methods forward here in a line or two.
//!
//! What is posted when follows macOS (see
//! `conformance/tests/appkit_events.rs`): a frame change that changes the
//! size posts `NSWindowDidResizeNotification` only, one that moves the
//! window without resizing it `NSWindowDidMoveNotification`, whether the
//! window is on screen or not; `close` posts `NSWindowWillCloseNotification`
//! (on screen or not), orders the window out and, for a window released
//! when closed (the default), gives up one reference to the current
//! autorelease pool, as AppKit does. Whether the program should quit after
//! its last window closed is asked once the event being handled is done,
//! not inside `close`.

use std::cell::{Cell, RefCell};

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{Message, msg_send, sel};
use objc2_app_kit::{
    NSBackingPropertyOldScaleFactorKey, NSEvent, NSEventModifierFlags, NSEventType, NSModalResponse, NSView,
    NSViewController, NSWindow, NSWindowController, NSWindowDidChangeBackingPropertiesNotification,
    NSWindowDidEndLiveResizeNotification, NSWindowDidMoveNotification, NSWindowDidResizeNotification,
    NSWindowStyleMask, NSWindowWillCloseNotification, NSWindowWillStartLiveResizeNotification,
};
use objc2_foundation::{NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString, NSUserDefaults};

use crate::notifications::{self, Owner, Registered};
use crate::window::NSWindowImpl;

/// What a window keeps for this area.
#[derive(Default)]
pub(crate) struct WindowEvents {
    /// The notifications the delegate is registered for.
    registered: Registered,
    /// The window's address, to remove its delegate's registrations when it
    /// goes (while it goes, it can't be messaged).
    this: Cell<usize>,
    /// The user is resizing the window.
    live_resize: Cell<bool>,
    /// Its sheets, and its place as one.
    sheets: Sheets,
    /// Weak: the controller holds the window.
    window_controller: RefCell<Option<Weak<NSWindowController>>>,
    content_view_controller: RefCell<Option<Retained<NSViewController>>>,
}

/// The window was made.
pub(crate) fn made(window: &NSWindowImpl) {
    window.events().this.set(window as *const NSWindowImpl as usize);
}

/// The window is going away: its delegate's registrations go with it.
pub(crate) fn gone(events: &WindowEvents, delegate: Option<&Weak<AnyObject>>) {
    let Some(delegate) = delegate.and_then(Weak::load) else { return };
    // SAFETY: the window's memory is still allocated while its ivars are
    // dropped; the center only compares its address.
    let this = unsafe { &*(events.this.get() as *const AnyObject) };
    notifications::set_delegate(Owner::Window, this, &events.registered, Some(&delegate), None);
}

/// `setDelegate:`, after the window has stored `delegate` in place of `old`.
pub(crate) fn delegate_changed(window: &NSWindowImpl, old: Option<&AnyObject>, delegate: Option<&AnyObject>) {
    notifications::set_delegate(Owner::Window, window.as_object(), &window.events().registered, old, delegate);
}

/// The frame changed from `before` (the program moved or resized the
/// window, or the compositor resized it).
pub(crate) fn frame_changed(window: &NSWindowImpl, before: NSRect, after: NSRect) {
    let name = if before.size != after.size {
        // SAFETY: the name is a constant string this crate exports.
        unsafe { NSWindowDidResizeNotification }
    } else if before.origin != after.origin {
        // SAFETY: as above.
        unsafe { NSWindowDidMoveNotification }
    } else {
        return;
    };
    autosave(window);
    notifications::post(name, window.as_object());
}

/// The compositor resized the window.
pub(crate) fn resized_by_compositor(window: &NSWindowImpl) {
    autosave(window);
    // SAFETY: the name is a constant string this crate exports.
    notifications::post(unsafe { NSWindowDidResizeNotification }, window.as_object());
}

/// The compositor changed the window's scale from `old`.
pub(crate) fn scale_changed(window: &NSWindowImpl, old: f64) {
    // SAFETY: the names are constant strings this crate exports.
    let (name, key) = unsafe { (NSWindowDidChangeBackingPropertiesNotification, NSBackingPropertyOldScaleFactorKey) };
    if !notifications::observed(name) {
        return;
    }
    let old = NSNumber::new_f64(old);
    let info = NSDictionary::from_slices(&[key], &[&*old]);
    notifications::post_with(name, window.as_object(), &info);
}

/// The compositor says whether the user is resizing the window.
pub(crate) fn resizing(window: &NSWindowImpl, resizing: bool) {
    if window.events().live_resize.replace(resizing) == resizing {
        return;
    }
    let name = if resizing {
        // SAFETY: the name is a constant string this crate exports.
        unsafe { NSWindowWillStartLiveResizeNotification }
    } else {
        // SAFETY: as above.
        unsafe { NSWindowDidEndLiveResizeNotification }
    };
    notifications::post(name, window.as_object());
}

/// `inLiveResize`.
pub(crate) fn in_live_resize(window: &NSWindowImpl) -> bool {
    window.events().live_resize.get()
}

/// `performClose:`: the close button's click. Nothing happens to a window
/// without one, or one its delegate (or, without a delegate that answers,
/// the window itself) won't let close.
pub(crate) fn perform_close(window: &NSWindowImpl) {
    if !window.style().contains(NSWindowStyleMask::Closable) || !should_close(window) {
        return;
    }
    window.as_window().close();
}

fn should_close(window: &NSWindowImpl) -> bool {
    let this = window.as_window();
    let ask = sel!(windowShouldClose:);
    let delegate = window.delegate_object().filter(|d| responds(d, ask));
    let asked: &AnyObject = match &delegate {
        Some(d) => d,
        None if responds(this, ask) => this,
        None => return true,
    };
    // SAFETY: windowShouldClose: takes the window and returns BOOL.
    unsafe { msg_send![asked, windowShouldClose: this] }
}

fn responds(object: &AnyObject, selector: objc2::runtime::Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// `close`: tell the observers, take the window off screen, and give up the
/// reference a window released when closed holds on itself.
pub(crate) fn close(window: &NSWindowImpl) {
    // Kept alive through the notification and ordering out, whatever the
    // observers release.
    let this: Retained<NSWindow> = window.as_window().retain();
    // SAFETY: the name is a constant string this crate exports.
    notifications::post(unsafe { NSWindowWillCloseNotification }, window.as_object());
    let was_visible = this.isVisible();
    this.orderOut(None);
    if was_visible {
        crate::app::window_closed();
    }
    if window.released_when_closed() {
        // SAFETY: as on macOS, a window released when closed gives up the
        // reference it was made with, to the current pool. A program that
        // keeps its own reference turns this off (objc2 asks programs to),
        // so the reference given up is one nobody else counts on.
        unsafe { objc2::ffi::objc_autorelease(Retained::as_ptr(&this).cast_mut().cast()) };
    }
    drop(this);
}

/// Whether a mouse down in `view` reaches it (the caller then sends
/// `mouseDown:` and its kin), or only activated the window, or moves it.
///
/// The render thread tags the press that gave its window the keyboard (the
/// compositor hands it over before the click arrives, so the window is key
/// by now): such a first click reaches the view only if it
/// `acceptsFirstMouse:`, and otherwise neither it nor the release after it
/// does. A left press on a window that moves by its background, in a view
/// that lets the window move (`mouseDownCanMoveWindow`: views that aren't
/// opaque), or in the title bar strip of a window whose content runs under
/// it, starts an interactive move instead.
pub(crate) fn mouse_down_reaches(window: &NSWindowImpl, view: &NSView, event: &NSEvent) -> bool {
    if crate::event::is_activating(event) && !view.acceptsFirstMouse(Some(event)) {
        return false;
    }
    if event.r#type() == NSEventType::LeftMouseDown && moves_window(window, view, event) {
        window.as_window().performWindowDragWithEvent(event);
        return false;
    }
    true
}

fn moves_window(window: &NSWindowImpl, view: &NSView, event: &NSEvent) -> bool {
    let this = window.as_window();
    if !this.isMovable() {
        return false;
    }
    let in_titlebar = window.style().contains(NSWindowStyleMask::FullSizeContentView) && {
        let layout = this.contentLayoutRect();
        event.locationInWindow().y >= layout.origin.y + layout.size.height
    };
    (this.isMovableByWindowBackground() || in_titlebar) && view.mouseDownCanMoveWindow()
}

/// Command-period: cancel, as Escape does.
pub(crate) fn is_cancel(event: &NSEvent) -> bool {
    let modifiers = event.modifierFlags()
        & (NSEventModifierFlags::Command | NSEventModifierFlags::Option | NSEventModifierFlags::Control);
    modifiers == NSEventModifierFlags::Command
        && event.charactersIgnoringModifiers().is_some_and(|c| c.to_string() == ".")
}

// Sheets (see `modal`, which begins and ends them).

/// A sheet begun on a window: attached (the first that isn't detached),
/// waiting its turn, or shown on its own because the parent wasn't on
/// screen.
pub(crate) struct SheetEntry {
    pub(crate) window: Retained<NSWindow>,
    pub(crate) handler: Option<RcBlock<dyn Fn(NSModalResponse)>>,
    pub(crate) detached: bool,
}

/// What a window keeps about sheets: its own, and its place as one.
#[derive(Default)]
pub(crate) struct Sheets {
    entries: RefCell<Vec<SheetEntry>>,
    /// Weak: a parent holds its sheets, not the other way round.
    parent: RefCell<Option<Weak<NSWindow>>>,
    is_sheet: Cell<bool>,
    /// On screen as a sheet, inside its parent.
    attached: Cell<bool>,
}

pub(crate) fn sheets_of(window: &NSWindowImpl) -> &RefCell<Vec<SheetEntry>> {
    &window.events().sheets.entries
}

/// `window` is to be a sheet of `parent`.
pub(crate) fn became_sheet(window: &NSWindowImpl, parent: &NSWindow) {
    let sheets = &window.events().sheets;
    sheets.is_sheet.set(true);
    let old = sheets.parent.replace(Some(Weak::new(parent)));
    drop(old);
}

pub(crate) fn set_attached(window: &NSWindowImpl, attached: bool) {
    window.events().sheets.attached.set(attached);
}

pub(crate) fn is_attached(window: &NSWindowImpl) -> bool {
    window.events().sheets.attached.get()
}

/// `isSheet`.
pub(crate) fn is_sheet(window: &NSWindowImpl) -> bool {
    window.events().sheets.is_sheet.get()
}

/// `sheetParent`.
pub(crate) fn sheet_parent(window: &NSWindowImpl) -> Option<Retained<NSWindow>> {
    window.events().sheets.parent.borrow().as_ref().and_then(Weak::load)
}

/// `attachedSheet`: the sheet on screen in the window.
pub(crate) fn attached_sheet(window: &NSWindowImpl) -> Option<Retained<NSWindow>> {
    let entries = sheets_of(window).borrow();
    let first = entries.iter().find(|e| !e.detached)?;
    is_attached(crate::window::imp(&first.window)).then(|| first.window.clone())
}

/// `sheets`: the attached sheet and those waiting their turn.
pub(crate) fn sheets(window: &NSWindowImpl) -> Vec<Retained<NSWindow>> {
    sheets_of(window).borrow().iter().filter(|e| !e.detached).map(|e| e.window.clone()).collect()
}

/// The window's attached sheet's attached sheet, and so on: the window keys
/// go to while `window` has the keyboard.
pub(crate) fn deepest_sheet(window: &NSWindow) -> Retained<NSWindow> {
    let mut at = window.retain();
    while let Some(sheet) = attached_sheet(crate::window::imp(&at)) {
        at = sheet;
    }
    at
}

/// The parent a window is attached to as a sheet, if it is on screen as
/// one: the render thread makes it part of the parent.
pub(crate) fn attached_to(window: &NSWindowImpl) -> Option<Retained<NSWindow>> {
    if !is_attached(window) {
        return None;
    }
    sheet_parent(window).filter(|p| p.isVisible())
}

/// While a sheet is attached, its parent refuses mouse input.
pub(crate) fn blocked_by_sheet(window: &NSWindowImpl, kind: NSEventType) -> bool {
    let mouse = matches!(
        kind,
        NSEventType::LeftMouseDown
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
            | NSEventType::Magnify
            | NSEventType::Rotate
            | NSEventType::Swipe
            | NSEventType::SmartMagnify
    );
    mouse && attached_sheet(window).is_some()
}

// Frame autosave.
//
// As on macOS: a frame is saved in the user defaults under
// "NSWindow Frame <name>" as the frame's x, y, width and height (macOS adds
// its screen's; Sidestep, which doesn't know where its windows are, writes
// the frame alone), each whole number without decimals, followed by a
// space. A window with an autosave name saves its frame whenever it moves
// or resizes, and takes the frame saved under the name when it gets one.
// Restoring asks for the size (the compositor decides) and keeps the
// origin the window reports; macOS reads four numbers or more.

fn defaults_key(name: &NSString) -> Retained<NSString> {
    NSString::from_str(&format!("NSWindow Frame {name}"))
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v}") }
}

/// `saveFrameUsingName:`.
pub(crate) fn save_frame(window: &NSWindowImpl, name: &NSString) {
    if name.length() == 0 {
        return;
    }
    let f = window.as_window().frame();
    let value =
        format!("{} {} {} {} ", number(f.origin.x), number(f.origin.y), number(f.size.width), number(f.size.height));
    let defaults = NSUserDefaults::standardUserDefaults();
    // SAFETY: a string is a property-list value.
    unsafe { defaults.setObject_forKey(Some(&NSString::from_str(&value)), &defaults_key(name)) };
}

/// `setFrameUsingName:`: whether a frame was saved under the name.
pub(crate) fn restore_frame(window: &NSWindowImpl, name: &NSString) -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    let Some(value) = defaults.stringForKey(&defaults_key(name)) else { return false };
    let numbers: Vec<f64> = value.to_string().split_whitespace().map_while(|n| n.parse().ok()).collect();
    let [x, y, width, height, ..] = numbers[..] else { return false };
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    window.as_window().setFrame_display(frame, true);
    true
}

/// `removeFrameUsingName:`.
pub(crate) fn remove_frame(name: &NSString) {
    NSUserDefaults::standardUserDefaults().removeObjectForKey(&defaults_key(name));
}

/// `setFrameAutosaveName:`: a frame saved under the name is the window's.
pub(crate) fn autosave_named(window: &NSWindowImpl, name: &NSString) {
    if name.length() > 0 {
        restore_frame(window, name);
    }
}

/// The frame moved or resized: save it, if the window has an autosave
/// name.
fn autosave(window: &NSWindowImpl) {
    let name = window.as_window().frameAutosaveName();
    if name.length() > 0 {
        save_frame(window, &name);
    }
}

// Controllers (see `controllers`).

/// `windowController`.
pub(crate) fn window_controller(window: &NSWindowImpl) -> Option<Retained<NSWindowController>> {
    window.events().window_controller.borrow().as_ref().and_then(Weak::load)
}

/// `setWindowController:`: the controller is also the window's next
/// responder.
pub(crate) fn set_window_controller(window: &NSWindowImpl, controller: Option<&NSWindowController>) {
    let old = window.events().window_controller.replace(controller.map(Weak::new));
    drop(old);
    crate::controllers::window_controller_changed(window.as_window(), controller);
}

/// `contentViewController`.
pub(crate) fn content_view_controller(window: &NSWindowImpl) -> Option<Retained<NSViewController>> {
    window.events().content_view_controller.borrow().clone()
}

/// `setContentViewController:`: its view is the content view.
pub(crate) fn set_content_view_controller(window: &NSWindowImpl, controller: Option<&NSViewController>) {
    let old = window.events().content_view_controller.replace(controller.map(|c| c.retain()));
    let view = controller.map(|c| c.view());
    window.as_window().setContentView(view.as_deref());
    drop(old);
}
