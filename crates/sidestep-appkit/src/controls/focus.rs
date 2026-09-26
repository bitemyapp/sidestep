//! Controls and the keyboard: key equivalents, the default button, the
//! keys a focused control takes, and focus rings.
//!
//! - A button performs its key equivalent (`performKeyEquivalent:`) when
//!   the key's characters, ignoring modifiers, are its key equivalent and
//!   the modifiers it asks for are down: it clicks itself, as
//!   `performClick:` does. Control, Option and Command must match exactly;
//!   Shift doesn't count, since the characters already carry it (an "S"
//!   equivalent with Command is Command-Shift-S).
//!   Return and Escape are ordinary key equivalents.
//! - A window's default button is the one set with `setDefaultButtonCell:`
//!   or else the first button whose key equivalent is Return. Return
//!   reaching the window (`keyDown:`) clicks it; Escape clicks the button
//!   whose key equivalent is Escape (`conformance/tests/control_events.rs`,
//!   `key_equivalents`).
//! - A focused control takes the keys that work it: Space clicks a button
//!   or turns a switch over; the arrows move between segments and between
//!   the radio buttons of a group, and step sliders and steppers through
//!   the action methods key bindings send (`moveUp:`, `pageDown:` …), so a
//!   subclass that overrides one sees the key. Keys a window's controls
//!   don't take go on up the responder chain.
//! - The key window's first responder, when it's a control, draws a focus
//!   ring: the accent at half strength, 2 points outside the part it
//!   belongs to (a button's bezel, a field's entry, a check box's box).
//!   The window draws it after the control's subtree, clipped only by the
//!   control's ancestors, and a change of focus redraws the area the ring
//!   covers.
//! - `isFullKeyboardAccessEnabled` is on (every control takes the
//!   keyboard, as on GNOME), unless `SIDESTEP_FULL_KEYBOARD_ACCESS=0`. A
//!   click never gives the keyboard to a button-like control either way.

use std::cell::RefCell;
use std::sync::OnceLock;

use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{ClassType, sel};
use objc2_app_kit::{NSButton, NSButtonCell, NSCell, NSControl, NSEvent, NSEventModifierFlags, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::control;
use crate::theme::{self, paint, parts};
use crate::views;

/// `NSUpArrowFunctionKey` and its siblings, as key events carry them.
const UP: u16 = 0xF700;
const DOWN: u16 = 0xF701;
const LEFT: u16 = 0xF702;
const RIGHT: u16 = 0xF703;
const PAGE_UP: u16 = 0xF72C;
const PAGE_DOWN: u16 = 0xF72D;

/// Whether every control takes the keyboard: on, unless
/// `SIDESTEP_FULL_KEYBOARD_ACCESS=0`.
pub(crate) fn full_keyboard_access() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("SIDESTEP_FULL_KEYBOARD_ACCESS").map_or(true, |v| v != "0"))
}

/// Whether a key event's modifiers match a key equivalent's mask:
/// Control, Option and Command exactly. Shift doesn't count, on either
/// side: the key's characters already say whether it was down
/// (`control_events.rs`, `key_equivalents`).
fn modifiers_match(event: NSEventModifierFlags, mask: NSEventModifierFlags) -> bool {
    let exact = NSEventModifierFlags::Control | NSEventModifierFlags::Option | NSEventModifierFlags::Command;
    event & exact == mask & exact
}

/// `-[NSButton performKeyEquivalent:]`.
pub(crate) fn button_key_equivalent(button: &NSButton, event: &NSEvent) -> bool {
    let key = button.keyEquivalent();
    if key.length() == 0 || !button.isEnabled() || button.isHiddenOrHasHiddenAncestor() {
        return false;
    }
    let Some(characters) = event.charactersIgnoringModifiers() else { return false };
    if !characters.isEqualToString(&key) {
        return false;
    }
    if !modifiers_match(event.modifierFlags(), button.keyEquivalentModifierMask()) {
        return false;
    }
    let sender = control::imp(button);
    // SAFETY: performClick: takes a sender.
    unsafe { button.performClick(Some(sender.as_view())) };
    true
}

thread_local! {
    /// Default buttons set with `setDefaultButtonCell:`, by window.
    static DEFAULTS: RefCell<Vec<(Weak<NSWindow>, Weak<NSButtonCell>)>> = const { RefCell::new(Vec::new()) };
}

/// `-[NSWindow defaultButtonCell]`: the one set, else the first button in
/// the window whose key equivalent is Return.
pub(crate) fn default_button_cell(window: &NSWindow) -> Option<Retained<NSButtonCell>> {
    let set = DEFAULTS.with(|d| {
        d.borrow().iter().find(|(w, _)| w.load().is_some_and(|w| std::ptr::eq(&*w, window))).map(|(_, c)| c.load())
    });
    if let Some(set) = set {
        return set;
    }
    let content = crate::window::imp(window).content()?;
    find_default(&content)
}

fn find_default(view: &NSView) -> Option<Retained<NSButtonCell>> {
    if let Some(control) = control::as_control(view)
        && let Some(cell) = control.as_control().cell()
        && let Some(button) = super::button::as_button_cell(&cell)
        && button.is_default()
    {
        // SAFETY: checked to be a button cell just above.
        return Some(unsafe { Retained::cast_unchecked(cell) });
    }
    views::subviews(views::imp(view)).iter().find_map(|sub| find_default(sub))
}

/// `-[NSWindow setDefaultButtonCell:]`.
pub(crate) fn set_default_button_cell(window: &NSWindow, cell: Option<&NSButtonCell>) {
    let gone = DEFAULTS.with(|d| {
        let mut defaults = d.borrow_mut();
        let (gone, mut kept): (Vec<_>, Vec<_>) =
            defaults.drain(..).partition(|(w, _)| w.load().is_none_or(|w| std::ptr::eq(&*w, window)));
        if let Some(cell) = cell {
            kept.push((Weak::new(window), Weak::new(cell)));
        }
        *defaults = kept;
        gone
    });
    drop(gone);
}

/// `-[NSWindow keyDown:]`'s part for controls: Return clicks the default
/// button. True if the key was used. Shift may be down, as for any key
/// equivalent without it. (Escape, and the rest, go on to the window's key
/// equivalents and the key view loop: `keyloop::window_key_down`.)
pub(crate) fn window_key_down(window: &NSWindow, event: &NSEvent) -> bool {
    let Some(characters) = event.charactersIgnoringModifiers() else { return false };
    let key = characters.to_string();
    if !modifiers_match(event.modifierFlags(), NSEventModifierFlags::empty()) {
        return false;
    }
    match key.as_str() {
        "\r" | "\u{3}" => {
            let Some(cell) = default_button_cell(window) else { return false };
            let cell: &NSCell = &cell;
            if !cell.isEnabled() {
                return false;
            }
            let view = super::cell::imp(cell).view();
            if view.as_deref().is_some_and(|v| v.isHiddenOrHasHiddenAncestor()) {
                return false;
            }
            // SAFETY: performClick: takes a sender.
            unsafe { cell.performClick(view.as_deref().map(|v| v as &AnyObject)) };
            true
        }
        // Escape reaches a button whose key equivalent it is through the
        // window's key equivalents (`keyloop::window_key_down`).
        _ => false,
    }
}

/// The key a key event is about: its first character, ignoring modifiers.
fn key_of(event: &NSEvent) -> Option<u16> {
    let characters = event.charactersIgnoringModifiers()?;
    (characters.length() > 0).then(|| characters.characterAtIndex(0))
}

/// `keyDown:` for a focused button: Space clicks it; the arrows move
/// between the radio buttons of its group. True if the key was used.
pub(crate) fn button_key_down(button: &NSButton, event: &NSEvent) -> bool {
    if !button.isEnabled() {
        return false;
    }
    match key_of(event) {
        Some(0x20) => {
            // SAFETY: performClick: takes a sender.
            unsafe { button.performClick(None) };
            true
        }
        Some(k @ (UP | DOWN | LEFT | RIGHT)) => radio_arrow(button, k == DOWN || k == RIGHT),
        _ => false,
    }
}

/// Move from a radio button to the next or previous enabled one of its
/// group (same superview, same action), click it and give it the keyboard.
/// A radio button without an action has no group.
fn radio_arrow(button: &NSButton, forward: bool) -> bool {
    let Some(cell) = (button as &NSControl).cell() else { return false };
    let Some(b) = super::button::as_button_cell(&cell) else { return false };
    if b.look() != super::button::Look::Radio {
        return false;
    }
    let Some(action) = cell.action() else { return false };
    // SAFETY: superview takes nothing and returns a view or nil.
    let Some(superview) = (unsafe { button.superview() }) else { return false };
    let action = Some(action);
    let group: Vec<Retained<NSView>> = views::subviews(views::imp(&superview))
        .into_iter()
        .filter(|v| {
            control::as_control(v).and_then(|c| c.as_control().cell()).is_some_and(|c| {
                super::button::as_button_cell(&c).is_some_and(|b| b.look() == super::button::Look::Radio)
                    && c.action() == action
                    && c.isEnabled()
            })
        })
        .collect();
    let me: &NSView = button;
    let Some(at) = group.iter().position(|v| std::ptr::eq(&**v, me)) else { return false };
    let n = group.len();
    let next = &group[if forward { (at + 1) % n } else { (at + n - 1) % n }];
    let next_control = control::as_control(next).expect("a control").as_control();
    // SAFETY: performClick: takes a sender.
    unsafe { next_control.performClick(None) };
    if let Some(window) = next.window() {
        window.makeFirstResponder(Some(next));
    }
    true
}

/// `keyDown:` for other focused controls: segments, sliders, steppers and
/// switches. True if the key was used.
pub(crate) fn control_key_down(view: &NSView, event: &NSEvent) -> bool {
    let Some(control) = control::as_control(view) else { return false };
    let control = control.as_control();
    if !control.isEnabled() {
        return false;
    }
    let key = key_of(event);
    let is = |class: &objc2::runtime::AnyClass| super::kind_of(view, class);
    if is(objc2_app_kit::NSSegmentedControl::class()) {
        return match key {
            Some(LEFT | UP) => super::segmented::arrow(control, false),
            Some(RIGHT | DOWN) => super::segmented::arrow(control, true),
            _ => false,
        };
    }
    // Sliders and steppers take the keys through their action methods.
    let action = if is(objc2_app_kit::NSSlider::class()) {
        match key {
            Some(LEFT) => Some(sel!(moveLeft:)),
            Some(RIGHT) => Some(sel!(moveRight:)),
            Some(UP) => Some(sel!(moveUp:)),
            Some(DOWN) => Some(sel!(moveDown:)),
            Some(PAGE_UP) => Some(sel!(pageUp:)),
            Some(PAGE_DOWN) => Some(sel!(pageDown:)),
            _ => None,
        }
    } else if is(objc2_app_kit::NSStepper::class()) {
        match key {
            Some(UP) => Some(sel!(moveUp:)),
            Some(DOWN) => Some(sel!(moveDown:)),
            _ => None,
        }
    } else {
        None
    };
    if let Some(action) = action {
        // SAFETY: the move methods take a sender and return nothing.
        unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(view, action, (None::<&AnyObject>,)) };
        return true;
    }
    if is(objc2_app_kit::NSSwitch::class()) && key == Some(0x20) {
        return super::switch::toggle(view);
    }
    false
}

/// A click on `view` doesn't give it the keyboard: buttons, segments,
/// sliders, steppers and switches keep it where it was.
pub(crate) fn click_keeps_focus(view: &NSView) -> bool {
    [
        NSButton::class(),
        objc2_app_kit::NSSegmentedControl::class(),
        objc2_app_kit::NSSlider::class(),
        objc2_app_kit::NSStepper::class(),
        objc2_app_kit::NSSwitch::class(),
    ]
    .into_iter()
    .any(|c| super::kind_of(view, c))
}

// Focus rings.

/// What a ring can spill over, beyond a view's frame.
const RING_OUTSET: f64 = 4.0;

/// The focus moved from `old` to `new` (either may be absent): redraw
/// where their rings are or were, which reaches outside their frames.
pub(crate) fn focus_moved(old: Option<&NSView>, new: Option<&NSView>) {
    for view in [old, new].into_iter().flatten() {
        ring_changed(view);
    }
}

/// Redraw the area of `view`'s ring, in its superview.
pub(crate) fn ring_changed(view: &NSView) {
    let v = views::imp(view);
    let frame = views::frame(v);
    let area = NSRect::new(
        NSPoint::new(frame.origin.x - RING_OUTSET, frame.origin.y - RING_OUTSET),
        NSSize::new(frame.size.width + 2.0 * RING_OUTSET, frame.size.height + 2.0 * RING_OUTSET),
    );
    match views::superview_of(v) {
        Some(sup) => views::invalidate(sup, area),
        None => view.setNeedsDisplay(true),
    }
}

/// The view whose focus ring `window` shows, if any: its first responder,
/// if the window is key and the responder is a control that doesn't turn
/// rings off. Asked once per display pass.
pub(crate) fn ring_view(window: &crate::window::NSWindowImpl) -> Option<Retained<NSView>> {
    if !window.is_key() {
        return None;
    }
    let first = window.as_window().firstResponder()?;
    let view = first.downcast::<NSView>().ok()?;
    let shows = control::as_control(&view).is_some() && view.focusRingType() != objc2_app_kit::NSFocusRingType::None;
    shows.then_some(view)
}

/// The outline of the part a control's ring follows, in its coordinates.
pub(crate) fn outline(view: &NSView) -> Option<parts::Outline> {
    let control = control::as_control(view)?.as_control();
    let bounds = view.bounds();
    let round = |r: NSRect, radius: f64| Some(parts::Outline::RoundRect(r, paint::radii(radius)));
    if let Some(cell) = control.cell() {
        if let Some(b) = super::button::as_button_cell(&cell) {
            return match b.look() {
                super::button::Look::Check => round(cell.imageRectForBounds(bounds), 4.0),
                super::button::Look::Radio => Some(parts::Outline::Ellipse(cell.imageRectForBounds(bounds))),
                super::button::Look::Circular | super::button::Look::Help | super::button::Look::PushDisclosure => {
                    let side = bounds.size.width.min(bounds.size.height);
                    Some(parts::Outline::Ellipse(parts::centered_square(bounds, side)))
                }
                _ => {
                    let d = cell.drawingRectForBounds(bounds);
                    round(
                        NSRect::new(
                            NSPoint::new(bounds.origin.x, d.origin.y),
                            NSSize::new(bounds.size.width, d.size.height),
                        ),
                        parts::RADIUS,
                    )
                }
            };
        }
        if super::kind_of(view, objc2_app_kit::NSSearchField::class()) {
            return round(bounds, bounds.size.height / 2.0);
        }
        if super::text_field::field_cell(&cell).is_some() {
            return if cell.isBezeled() || cell.isBordered() {
                round(bounds, parts::RADIUS)
            } else {
                round(bounds, 2.0)
            };
        }
    }
    if super::kind_of(view, objc2_app_kit::NSSwitch::class()) {
        return round(bounds, bounds.size.height / 2.0);
    }
    round(bounds, parts::RADIUS)
}

/// Draw `view`'s focus ring, in its coordinates, into what's being
/// recorded.
pub(crate) fn draw_ring(view: &NSView) {
    if let Some(outline) = outline(view) {
        parts::focus_ring(theme::palette(), outline);
    }
}
