//! Scrolling that coasts after a touchpad flick, as AppKit's does.
//!
//! Wayland compositors report touchpad scrolls only while fingers move, so
//! toolkits make the coasting: when a gesture ends fast enough, scroll
//! events with a momentum phase (began, changed, ended) follow it, their
//! speed decaying exponentially. A [`Coast`] is the physics; the toolkit
//! steps it once per frame the window shows (each step as long as the time
//! since the last, so the distance per frame changes smoothly with the
//! display's pace), or after [`FALLBACK`] when no frame comes, and stops it
//! at a new scroll, a click or the window going away.

use std::time::{Duration, Instant};

/// Speed kept per millisecond: after a second, about 5% is left.
pub const DECAY_PER_MS: f64 = 0.997;
/// Flicks slower than this, in points per second, don't coast.
pub const START_SPEED: f64 = 60.0;
/// Coasting slower than this ends.
pub const STOP_SPEED: f64 = 12.0;
/// Time after a step before the next comes without a frame.
pub const FALLBACK: Duration = Duration::from_millis(50);

/// A flick coasting: its speed, and when it last moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coast {
    /// Points per second, in Wayland's directions.
    pub velocity: (f64, f64),
    pub last: Instant,
    /// The first step was taken (its event was the momentum's start).
    pub began: bool,
}

/// What a step moves: points in Wayland's directions, whether it's the
/// first, and whether the coast is now too slow to go on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    pub delta: (f64, f64),
    pub first: bool,
    pub slow: bool,
}

impl Coast {
    /// A gesture ended at `velocity` points per second: a coast, if that's
    /// fast enough.
    pub fn start(velocity: (f64, f64), now: Instant) -> Option<Coast> {
        (velocity.0.hypot(velocity.1) >= START_SPEED).then_some(Coast { velocity, last: now, began: false })
    }

    /// When the next step is due if no frame comes first.
    pub fn deadline(&self) -> Instant {
        self.last + FALLBACK
    }

    /// Move on to `now`: the distance a decaying speed covers in the time
    /// since the last step, however long the main thread was away.
    pub fn step(&mut self, now: Instant) -> Step {
        let ms = now.duration_since(self.last).as_secs_f64() * 1000.0;
        self.last = now;
        let keep = DECAY_PER_MS.powf(ms);
        let reach = (1.0 - keep) / -DECAY_PER_MS.ln() / 1000.0;
        let delta = (self.velocity.0 * reach, self.velocity.1 * reach);
        self.velocity = (self.velocity.0 * keep, self.velocity.1 * keep);
        let slow = self.velocity.0.hypot(self.velocity.1) < STOP_SPEED;
        let first = !std::mem::replace(&mut self.began, true);
        Step { delta, first, slow }
    }
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

    #[test]
    fn steps_cover_what_the_speed_does() {
        let t = Instant::now();
        assert!(Coast::start((30.0, 40.0), t).is_none(), "too slow to coast");
        let mut coast = Coast::start((0.0, 1000.0), t).expect("a coast");
        let a = coast.step(t + Duration::from_millis(16));
        assert!(a.first && !a.slow);
        // About 16 ms at nearly 1000 pt/s.
        assert!((15.0..16.5).contains(&a.delta.1), "{a:?}");
        let b = coast.step(t + Duration::from_millis(32));
        assert!(!b.first && b.delta.1 < a.delta.1);
        // One long step covers what many short ones would.
        let mut one = Coast::start((0.0, 1000.0), t).expect("a coast");
        let mut many = one;
        let long = one.step(t + Duration::from_millis(400)).delta.1;
        let short: f64 = (1..=100).map(|i| many.step(t + Duration::from_millis(4 * i)).delta.1).sum();
        assert!((long - short).abs() < 0.01, "{long} {short}");
        assert!(coast.step(t + Duration::from_secs(3)).slow);
    }
}
