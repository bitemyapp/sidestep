//! Scrolling that coasts after a touchpad flick, as AppKit's does.
//!
//! Wayland compositors report touchpad scrolls only while fingers move, so
//! the coasting is made here: when a gesture ends fast enough, scroll
//! events with a `momentumPhase` (began, changed, ended) follow it at the
//! window's last pointer position, their speed decaying exponentially
//! (`sidestep_engine::momentum`, which the native toolkit shares). A new
//! scroll, a click or the window going away stops it.
//!
//! Coasting moves once per frame the window shows: each frame callback
//! brings a step as long as the time since the last, so the distance per
//! frame changes smoothly with the display's pace. When no frames come
//! (the scroll reached an end and nothing changed), a timer steps instead.

use std::cell::RefCell;
use std::time::Instant;

use objc2::Message;
use objc2::rc::Retained;
use objc2_app_kit::{NSEventPhase, NSWindow};
use objc2_foundation::NSPoint;
use sidestep_engine::momentum;

use crate::app;
use crate::protocol::Modifiers;

struct Coast {
    window: Retained<NSWindow>,
    location: NSPoint,
    modifiers: Modifiers,
    /// The gesture's `isDirectionInvertedFromDevice`.
    inverted: bool,
    physics: momentum::Coast,
}

thread_local!(static COAST: RefCell<Option<Coast>> = const { RefCell::new(None) });

/// A gesture ended at `velocity` points per second: coast if that's fast.
pub(crate) fn start(window: &NSWindow, location: NSPoint, velocity: (f64, f64), modifiers: Modifiers, inverted: bool) {
    stop();
    let Some(physics) = momentum::Coast::start(velocity, Instant::now()) else { return };
    let coast = Coast { window: window.retain(), location, modifiers, inverted, physics };
    COAST.with(|c| c.replace(Some(coast)));
}

/// Stop coasting, telling the window it ended.
pub(crate) fn stop() {
    let Some(coast) = COAST.with(|c| c.take()) else { return };
    if coast.physics.began {
        send(&coast, (0.0, 0.0), NSEventPhase::Ended);
    }
}

/// When a coasting event is due if no frame comes first.
pub(crate) fn deadline() -> Option<Instant> {
    COAST.with(|c| c.borrow().as_ref().map(|c| c.physics.deadline()))
}

/// A frame of `window` was shown: coasting there moves on.
pub(crate) fn frame(window: &NSWindow) {
    let coasting = COAST.with(|c| c.borrow().as_ref().is_some_and(|c| std::ptr::eq(&*c.window, window)));
    if coasting {
        step(Instant::now());
    }
}

/// No frame came for a while: step anyway, if it's time.
pub(crate) fn tick(now: Instant) {
    if deadline().is_some_and(|d| now >= d) {
        step(now);
    }
}

/// Send the coasting event for the time since the last.
fn step(now: Instant) {
    let step = COAST.with(|c| c.borrow_mut().as_mut().map(|coast| coast.physics.step(now)));
    let Some(step) = step else { return };
    let phase = if step.first { NSEventPhase::Began } else { NSEventPhase::Changed };
    // Sent without holding the state: handlers may stop coasting.
    let coast = COAST.with(|c| c.borrow().as_ref().map(|c| (c.window.clone(), c.location, c.modifiers, c.inverted)));
    if let Some((window, location, modifiers, inverted)) = coast {
        let phases = (NSEventPhase::None, phase);
        let event = crate::event::scroll_event(location, &window, step.delta, false, modifiers, phases, inverted);
        app::dispatch(&event);
    }
    if step.slow {
        stop();
    }
}

fn send(coast: &Coast, delta: (f64, f64), phase: NSEventPhase) {
    let event = crate::event::scroll_event(
        coast.location,
        &coast.window,
        delta,
        false,
        coast.modifiers,
        (NSEventPhase::None, phase),
        coast.inverted,
    );
    app::dispatch(&event);
}

/// The window closed: its coasting ends without a word.
pub(crate) fn window_closed(window: &NSWindow) {
    let gone = COAST.with(|c| {
        let mut slot = c.borrow_mut();
        if slot.as_ref().is_some_and(|c| std::ptr::eq(&*c.window, window)) { slot.take() } else { None }
    });
    drop(gone);
}
