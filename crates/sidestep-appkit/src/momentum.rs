//! Scrolling that coasts after a touchpad flick, as AppKit's does.
//!
//! Wayland compositors report touchpad scrolls only while fingers move, so
//! the coasting is made here: when a gesture ends fast enough, scroll
//! events with a `momentumPhase` (began, changed, ended) follow it at the
//! window's last pointer position, their speed decaying exponentially. A
//! new scroll, a click or the window going away stops it.
//!
//! Coasting moves once per frame the window shows: each frame callback
//! brings a step as long as the time since the last one, so the distance
//! per frame changes smoothly with the display's pace. When no frames come
//! (the scroll reached an end and nothing changed), a timer steps instead.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use objc2::Message;
use objc2::rc::Retained;
use objc2_app_kit::{NSEventPhase, NSWindow};
use objc2_foundation::NSPoint;

use crate::app;
use crate::protocol::Modifiers;

/// Speed kept per millisecond: after a second, about 5% is left.
const DECAY_PER_MS: f64 = 0.997;
/// Flicks slower than this, in points per second, don't coast.
const START_SPEED: f64 = 60.0;
/// Coasting slower than this ends.
const STOP_SPEED: f64 = 12.0;
/// Time after a step before the next comes without a frame.
const FALLBACK: Duration = Duration::from_millis(50);

struct Coast {
    window: Retained<NSWindow>,
    location: NSPoint,
    modifiers: Modifiers,
    /// Points per second, in Wayland's directions.
    velocity: (f64, f64),
    /// The gesture's `isDirectionInvertedFromDevice`.
    inverted: bool,
    last: Instant,
    began: bool,
}

thread_local!(static COAST: RefCell<Option<Coast>> = const { RefCell::new(None) });

/// A gesture ended at `velocity` points per second: coast if that's fast.
pub(crate) fn start(window: &NSWindow, location: NSPoint, velocity: (f64, f64), modifiers: Modifiers, inverted: bool) {
    stop();
    if velocity.0.hypot(velocity.1) < START_SPEED {
        return;
    }
    let last = Instant::now();
    let coast = Coast { window: window.retain(), location, modifiers, velocity, inverted, last, began: false };
    COAST.with(|c| c.replace(Some(coast)));
}

/// Stop coasting, telling the window it ended.
pub(crate) fn stop() {
    let Some(coast) = COAST.with(|c| c.take()) else { return };
    if coast.began {
        send(&coast, (0.0, 0.0), NSEventPhase::Ended);
    }
}

/// When a coasting event is due if no frame comes first.
pub(crate) fn deadline() -> Option<Instant> {
    COAST.with(|c| c.borrow().as_ref().map(|c| c.last + FALLBACK))
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
    let step = COAST.with(|c| {
        let mut slot = c.borrow_mut();
        let coast = slot.as_mut()?;
        let ms = now.duration_since(coast.last).as_secs_f64() * 1000.0;
        coast.last = now;
        let keep = DECAY_PER_MS.powf(ms);
        // The distance a decaying speed covers in `ms`, however long the
        // main thread was away.
        let reach = (1.0 - keep) / -DECAY_PER_MS.ln() / 1000.0;
        let delta = (coast.velocity.0 * reach, coast.velocity.1 * reach);
        coast.velocity = (coast.velocity.0 * keep, coast.velocity.1 * keep);
        let slow = coast.velocity.0.hypot(coast.velocity.1) < STOP_SPEED;
        let phase = if !coast.began {
            coast.began = true;
            NSEventPhase::Began
        } else {
            NSEventPhase::Changed
        };
        Some((delta, phase, slow))
    });
    let Some((delta, phase, slow)) = step else { return };
    // Sent without holding the state: handlers may stop coasting.
    let coast = COAST.with(|c| c.borrow().as_ref().map(|c| (c.window.clone(), c.location, c.modifiers, c.inverted)));
    if let Some((window, location, modifiers, inverted)) = coast {
        let phases = (NSEventPhase::None, phase);
        let event = crate::event::scroll_event(location, &window, delta, false, modifiers, phases, inverted);
        app::dispatch(&event);
    }
    if slow {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coasting_slows_to_a_stop() {
        // A second of decay leaves about 5%: a 2000 pt/s flick coasts about
        // 660 points before it's slower than the stop speed.
        let keep = DECAY_PER_MS.powf(1000.0);
        assert!((0.04..0.06).contains(&keep));
        let (mut v, mut travelled, mut ms) = (2000.0_f64, 0.0, 0.0);
        while v >= STOP_SPEED {
            v *= DECAY_PER_MS.powf(8.0);
            travelled += v * 0.008;
            ms += 8.0;
        }
        assert!((600.0..700.0).contains(&travelled), "{travelled}");
        assert!(ms < 2000.0, "{ms}");
    }
}
