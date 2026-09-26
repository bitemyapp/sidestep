//! Following the mouse: `-[NSControl mouseDown:]`, `-[NSCell
//! trackMouse:inRect:ofView:untilMouseUp:]` and `performClick:`.
//!
//! The order of calls is AppKit's, as `conformance/tests/control_events.rs`
//! records it on macOS:
//!
//! - The control highlights the cell (`highlight:withFrame:inView:`) and
//!   hands it the mouse-down with its bounds; afterwards it takes the
//!   highlight away.
//! - The cell asks `startTrackingAt:inView:` whether to follow the mouse
//!   (plain cells do only when continuous). If so, each drag inside goes
//!   to `continueTracking:at:inView:`, and the end, inside or not, to
//!   `stopTracking:at:inView:mouseIsUp:`.
//! - Released inside, the cell takes its next state and sends its action,
//!   still highlighted, and says the mouse went up. Dragged outside (unless
//!   it tracks until the mouse is up), it says the mouse left; the control
//!   then waits for the mouse to come back, and tracks again from there, or
//!   for the release.
//! - A cell that acts on the mouse-down acts at once and stops, leaving the
//!   release in the queue.
//! - Continuous cells act again on periodic ticks while the mouse is held
//!   inside.
//!
//! Events come from `-[NSApplication nextEventMatchingMask:untilDate:
//! inMode:dequeue:]` in the event-tracking run loop mode, so timers that
//! only run in the default mode wait, and the events the loop doesn't take
//! wait for the main loop. A periodic tick is a wait that times out.

use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, Message, msg_send};
use objc2_app_kit::{NSApplication, NSCell, NSControl, NSEvent, NSEventMask, NSEventType, NSView};
use objc2_foundation::{NSDate, NSPoint, NSRect, NSString};

use super::cell::{Flags, imp as cell_imp};
use super::control;

/// A continuous cell's first periodic action and the time between the
/// rest, in seconds (`getPeriodicDelay:interval:`; AppKit's defaults).
pub(crate) const PERIODIC: (f32, f32) = (0.4, 0.075);

/// The run loop mode tracking loops wait in.
fn tracking_mode() -> Retained<NSString> {
    thread_local!(static MODE: Retained<NSString> = NSString::from_str("NSEventTrackingRunLoopMode"));
    MODE.with(Clone::clone)
}

/// The next event `mask` takes, waiting until `until` (forever if none).
pub(crate) fn next_event(
    mtm: MainThreadMarker,
    mask: NSEventMask,
    until: Option<Instant>,
) -> Option<Retained<NSEvent>> {
    let app = NSApplication::sharedApplication(mtm);
    let date = match until {
        Some(t) => NSDate::dateWithTimeIntervalSinceNow(t.saturating_duration_since(Instant::now()).as_secs_f64()),
        None => NSDate::distantFuture(),
    };
    app.nextEventMatchingMask_untilDate_inMode_dequeue(mask, Some(&date), &tracking_mode(), true)
}

/// Whether `p` is in `r`, as `-[NSView mouse:inRect:]` decides: the edge
/// nearer the origin counts, the far one doesn't, whichever way the view is
/// flipped.
pub(crate) fn mouse_in_rect(p: NSPoint, r: NSRect, flipped: bool) -> bool {
    let (x0, x1) = (r.origin.x, r.origin.x + r.size.width);
    let (y0, y1) = (r.origin.y, r.origin.y + r.size.height);
    let y_in = if flipped { p.y >= y0 && p.y < y1 } else { p.y > y0 && p.y <= y1 };
    p.x >= x0 && p.x < x1 && y_in
}

fn drag_or_up() -> NSEventMask {
    NSEventMask::LeftMouseUp | NSEventMask::LeftMouseDragged
}

/// Send the cell's action through its view, as a control sends its own.
pub(crate) fn send_cell_action(cell: &NSCell, view: &NSView) {
    let action = cell.action();
    let target = cell.target();
    if control::as_control(view).is_some() {
        // SAFETY: sendAction:to: takes an action and a target, both
        // optional.
        let _: bool = unsafe { msg_send![view, sendAction: action, to: target.as_deref()] };
    } else if let Some(action) = action {
        let app = NSApplication::sharedApplication(MainThreadMarker::from(view));
        let sender: &AnyObject = cell;
        // SAFETY: the action takes the sender.
        let _ = unsafe { app.sendAction_to_from(action, target.as_deref(), Some(sender)) };
    }
}

/// `-[NSControl mouseDown:]`.
pub(crate) fn control_mouse_down(control: &NSControl, event: &NSEvent) {
    let Some(cell) = control.cell() else { return };
    if !control.isEnabled() {
        return;
    }
    let view: &NSView = control;
    let mtm = MainThreadMarker::from(control);
    let until_up: bool = {
        // SAFETY: +prefersTrackingUntilMouseUp takes nothing and returns
        // BOOL.
        unsafe { msg_send![cell.class(), prefersTrackingUntilMouseUp] }
    };
    let mut event = event.retain();
    loop {
        let bounds = view.bounds();
        cell.highlight_withFrame_inView(true, bounds, view);
        let up = cell.trackMouse_inRect_ofView_untilMouseUp(&event, bounds, view, until_up);
        cell.highlight_withFrame_inView(false, bounds, view);
        if up {
            return;
        }
        // Outside: wait for the mouse to come back, or for the release.
        loop {
            let Some(next) = next_event(mtm, drag_or_up(), None) else { return };
            if next.r#type() == NSEventType::LeftMouseUp {
                return;
            }
            let at = control::event_point(view, &next);
            if mouse_in_rect(at, view.bounds(), view.isFlipped()) {
                event = next;
                break;
            }
        }
    }
}

/// `-[NSCell trackMouse:inRect:ofView:untilMouseUp:]`: true when the mouse
/// went up, false when it left `frame` first.
pub(crate) fn track_mouse(cell: &NSCell, event: &NSEvent, frame: NSRect, view: &NSView, until_up: bool) -> bool {
    let imp = cell_imp(cell);
    let mtm = MainThreadMarker::from(view);
    let flipped = view.isFlipped();
    let start = control::event_point(view, event);
    let following = cell.startTrackingAt_inView(start, view);
    let mask = imp.action_mask();
    if mask & NSEventMask::LeftMouseDown.0 != 0 {
        cell.setNextState();
        send_cell_action(cell, view);
        cell.stopTracking_at_inView_mouseIsUp(start, start, view, true);
        return true;
    }
    let continuous = imp.has(Flags::CONTINUOUS);
    let mut events = drag_or_up();
    let mut tick = None;
    if continuous {
        events |= NSEventMask::Periodic;
        let (delay, _) = periodic(cell);
        tick = Some(Instant::now() + std::time::Duration::from_secs_f32(delay));
    }
    let mut last = start;
    loop {
        let next = next_event(mtm, events, tick);
        let Some(next) = next else {
            // A periodic tick: act again while held inside.
            if let Some(t) = tick {
                let (_, interval) = periodic(cell);
                tick = Some(t.max(Instant::now()) + std::time::Duration::from_secs_f32(interval));
                if mouse_in_rect(last, frame, flipped) {
                    send_cell_action(cell, view);
                }
                continue;
            }
            return true;
        };
        match next.r#type() {
            NSEventType::LeftMouseUp => {
                let at = control::event_point(view, &next);
                if following {
                    cell.stopTracking_at_inView_mouseIsUp(at, at, view, true);
                }
                if mouse_in_rect(at, frame, flipped) {
                    cell.setNextState();
                    if mask & NSEventMask::LeftMouseUp.0 != 0 {
                        send_cell_action(cell, view);
                    }
                }
                return true;
            }
            NSEventType::LeftMouseDragged => {
                let at = control::event_point(view, &next);
                if !until_up && !mouse_in_rect(at, frame, flipped) {
                    if following {
                        cell.stopTracking_at_inView_mouseIsUp(at, at, view, false);
                    }
                    return false;
                }
                if following && !cell.continueTracking_at_inView(last, at, view) {
                    // The cell has seen enough: wait for the release.
                    cell.stopTracking_at_inView_mouseIsUp(at, at, view, false);
                    return until_up_or_leave(mtm, frame, view, until_up);
                }
                last = at;
                if continuous && mask & NSEventMask::LeftMouseDragged.0 != 0 {
                    send_cell_action(cell, view);
                }
            }
            NSEventType::Periodic if mouse_in_rect(last, frame, flipped) => send_cell_action(cell, view),
            _ => {}
        }
    }
}

/// After a cell stopped following: wait for the release (true) or, unless
/// tracking until the mouse is up, for the mouse to leave (false).
fn until_up_or_leave(mtm: MainThreadMarker, frame: NSRect, view: &NSView, until_up: bool) -> bool {
    loop {
        let Some(next) = next_event(mtm, drag_or_up(), None) else { return true };
        if next.r#type() == NSEventType::LeftMouseUp {
            return true;
        }
        if !until_up && !mouse_in_rect(control::event_point(view, &next), frame, view.isFlipped()) {
            return false;
        }
    }
}

/// A cell's periodic delay and interval.
fn periodic(cell: &NSCell) -> (f32, f32) {
    let (mut delay, mut interval) = PERIODIC;
    // SAFETY: getPeriodicDelay:interval: writes two floats.
    unsafe {
        let _: () = msg_send![cell, getPeriodicDelay: &mut delay as *mut f32, interval: &mut interval as *mut f32];
    }
    (delay.max(0.0), interval.max(0.001))
}

/// `-[NSCell performClick:]`: as a click would, without the mouse: the
/// next state, then the action, while highlighted. Nothing when disabled.
pub(crate) fn perform_click(cell: &NSCell) {
    let imp = cell_imp(cell);
    if !imp.has(Flags::ENABLED) {
        return;
    }
    cell.setHighlighted(true);
    cell.setNextState();
    if let Some(view) = imp.view() {
        send_cell_action(cell, &view);
    }
    cell.setHighlighted(false);
}
