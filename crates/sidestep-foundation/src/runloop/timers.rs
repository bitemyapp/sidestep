//! Scheduling timers on a loop and firing them.
//!
//! Each loop keeps its timers in a `BTreeMap` keyed by (due instant,
//! sequence number), so adding, removing and re-arming are O(log n) and the
//! next deadline is the first entry in the running mode. A timer's own
//! [`Sched`](crate::timer::Sched) says where it should be; `resync` makes
//! the heap agree, on the loop's thread (other threads send it there).
//!
//! Firing takes the timers due in the running mode, then calls each one
//! still due, one fire per timer per pass. While its callout runs a timer
//! is marked firing, and nested runs skip it. A repeating timer then moves
//! to the first multiple of its interval after the current time, counted
//! from the fire date it just had: its phase is kept and missed fires are
//! dropped (a 100 ms timer stalled 350 ms in its first callout fires next
//! at 500 ms, as on macOS). A callout that moved the fire date later wins.

use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{DefinedClass, Message};

use super::core::{Dead, Shared, State, on_owner, with_state};
use super::modes::{Mode, ModeSet};
use crate::thread::lock;
use crate::timer::{LATEST_FIRE_DATE, NSTimerImpl, due_for};

/// A timer's place in its loop's heap.
pub(crate) type TimerKey = (Instant, u64);

pub(crate) struct TimerEntry {
    pub(crate) timer: Retained<NSTimerImpl>,
    /// A copy of the timer's modes, so looking for due timers takes no
    /// locks.
    pub(crate) modes: ModeSet,
}

/// A timer handed to its loop's thread.
struct SendTimer(Retained<NSTimerImpl>);

// SAFETY: a timer's cross-thread state is in atomics and locks, and
// Objective-C reference counting is thread-safe.
unsafe impl Send for SendTimer {}

/// Make `shared`'s heap agree with the timer's schedule.
fn resync(shared: &Arc<Shared>, timer: &NSTimerImpl) {
    let timer = SendTimer(timer.retain());
    on_owner(shared, move || {
        let timer = timer;
        with_state(|s| s.resync_timer(&timer.0));
    });
}

impl State {
    pub(crate) fn resync_timer(&mut self, timer: &Retained<NSTimerImpl>) {
        let mut sched = lock(&timer.ivars().sched);
        if let Some(key) = sched.key.take()
            && let Some(entry) = self.timers.remove(&key)
        {
            self.dead.push(Dead::Timer(entry.timer));
        }
        let mine = sched.owner.as_ref().is_some_and(|o| Arc::ptr_eq(o, &self.shared));
        if !mine || !timer.valid() {
            return;
        }
        if sched.reg.modes.is_empty() {
            sched.owner = None;
            return;
        }
        self.known.extend(&sched.reg.modes);
        let key = (sched.due, self.next_seq());
        sched.key = Some(key);
        self.timers.insert(key, TimerEntry { timer: timer.clone(), modes: sched.reg.modes.clone() });
    }
}

/// Schedule `timer` on `shared` in `mode`. A timer belongs to one loop at
/// a time; an invalid timer is ignored.
pub(crate) fn add(shared: &Arc<Shared>, timer: &NSTimerImpl, mode: Mode) {
    if !timer.valid() {
        return;
    }
    {
        let mut sched = lock(&timer.ivars().sched);
        match &sched.owner {
            Some(owner) if !Arc::ptr_eq(owner, shared) => {
                eprintln!("sidestep: a timer can be scheduled on only one run loop at a time");
                return;
            }
            _ => {}
        }
        sched.owner = Some(shared.clone());
        let common = lock(&shared.common).clone();
        sched.reg.add(mode, &common);
    }
    resync(shared, timer);
}

/// Take `timer` out of `mode` on `shared`; out of every common mode for
/// the common pseudo-mode. Leaves it valid.
pub(crate) fn remove(shared: &Arc<Shared>, timer: &NSTimerImpl, mode: Mode) {
    {
        let mut sched = lock(&timer.ivars().sched);
        if !sched.owner.as_ref().is_some_and(|o| Arc::ptr_eq(o, shared)) {
            return;
        }
        let common = lock(&shared.common).clone();
        sched.reg.remove(mode, &common);
    }
    resync(shared, timer);
}

/// Whether `timer` is scheduled on `shared` in `mode`.
pub(crate) fn contains(shared: &Arc<Shared>, timer: &NSTimerImpl, mode: Mode) -> bool {
    let sched = lock(&timer.ivars().sched);
    sched.owner.as_ref().is_some_and(|o| Arc::ptr_eq(o, shared)) && sched.reg.contains(mode)
}

/// Stop the timer for good and let go of its target, user info and
/// callout.
pub(crate) fn invalidate(timer: &NSTimerImpl) {
    if !timer.take_valid() {
        return;
    }
    let payload = timer.take_payload();
    let owner = {
        let mut sched = lock(&timer.ivars().sched);
        sched.reg = Default::default();
        sched.owner.take()
    };
    if let Some(owner) = owner {
        resync(&owner, timer);
    }
    drop(payload);
}

/// Move the timer's next fire to `fire` (seconds since 2001).
pub(crate) fn set_fire_date(timer: &NSTimerImpl, fire: f64) {
    if !timer.valid() {
        return;
    }
    let fire = fire.min(LATEST_FIRE_DATE);
    timer.set_fire_time(fire);
    let owner = {
        let mut sched = lock(&timer.ivars().sched);
        sched.due = due_for(fire);
        sched.retimed += 1;
        sched.owner.clone()
    };
    if let Some(owner) = owner {
        resync(&owner, timer);
    }
}

/// Fire the timers of `mode` due at `now`. Runs on the loop's thread.
pub(crate) fn fire_due(mode: Mode, now: Instant) {
    let due = with_state(|s| {
        let first = s.timers.keys().next()?;
        if first.0 > now {
            return None;
        }
        let mut due = std::mem::take(&mut s.scratch_timers);
        due.extend(
            s.timers
                .range(..=(now, u64::MAX))
                .filter(|(_, e)| e.modes.contains(mode) && !e.timer.is_firing())
                .map(|(key, e)| (*key, e.timer.clone())),
        );
        Some(due)
    });
    let Some(mut due) = due else { return };
    for (key, timer) in &due {
        let still_due = with_state(|s| {
            s.timers.get(key).is_some_and(|e| std::ptr::eq(&*e.timer, &**timer) && e.modes.contains(mode))
        });
        if still_due && timer.valid() && !timer.is_firing() {
            fire_scheduled(timer, key.0);
        }
    }
    due.clear();
    with_state(|s| {
        if s.scratch_timers.capacity() == 0 {
            s.scratch_timers = due;
        }
    });
}

/// Fire a scheduled timer that was due at `was_due`, then re-arm or
/// invalidate it.
fn fire_scheduled(timer: &NSTimerImpl, was_due: Instant) {
    let retimed = lock(&timer.ivars().sched).retimed;
    timer.set_firing(true);
    struct Unmark<'a>(&'a NSTimerImpl);
    impl Drop for Unmark<'_> {
        fn drop(&mut self) {
            self.0.set_firing(false);
        }
    }
    let unmark = Unmark(timer);
    timer.call();
    drop(unmark);

    let interval = timer.ivars().interval;
    if interval == 0.0 {
        invalidate(timer);
        return;
    }
    if !timer.valid() {
        return;
    }
    let owner = {
        let mut sched = lock(&timer.ivars().sched);
        if !(sched.retimed != retimed && sched.due > was_due) {
            // Next multiple of the interval after now, from the time it was
            // due. The fire date reported follows from that, as on macOS,
            // where a timer made with a past date starts its phase when it
            // was made.
            let now = Instant::now();
            let behind = now.saturating_duration_since(was_due).as_secs_f64();
            let steps = (behind / interval).floor() + 1.0;
            let ahead = Duration::from_secs_f64((steps * interval).min(LATEST_FIRE_DATE));
            sched.due = was_due.checked_add(ahead).unwrap_or(now);
            let until = sched.due.saturating_duration_since(now).as_secs_f64();
            timer.set_fire_time((crate::date::now() + until).min(LATEST_FIRE_DATE));
        }
        sched.owner.clone()
    };
    if let Some(owner) = owner {
        resync(&owner, timer);
    }
}
