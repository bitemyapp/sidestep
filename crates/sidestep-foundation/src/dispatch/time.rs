//! `dispatch_time_t`: nanoseconds on the monotonic clock since a
//! per-process origin, or, with the top bit set, a wall-clock time stored
//! negated, as libdispatch encodes them. 0 is now and all ones is never.

use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const NOW: u64 = 0;
pub(crate) const FOREVER: u64 = u64::MAX;
/// `DISPATCH_WALLTIME_NOW`.
const WALLTIME_NOW: u64 = !1;

/// The monotonic clock's origin, a moment before any dispatch time.
static ORIGIN: LazyLock<Instant> =
    LazyLock::new(|| Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now));

fn monotonic_now() -> u64 {
    ORIGIN.elapsed().as_nanos() as u64
}

fn wall_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_nanos() as i64,
        Err(before) => -(before.duration().as_nanos() as i64),
    }
}

fn is_wall(when: u64) -> bool {
    (when as i64) < 0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn dispatch_time(when: u64, delta: i64) -> u64 {
    match when {
        FOREVER => FOREVER,
        WALLTIME_NOW => wall(wall_now(), delta),
        when if is_wall(when) => wall(-(when as i64), delta),
        when => {
            let base = if when == NOW { monotonic_now() } else { when };
            match base.checked_add_signed(delta) {
                // Keep clear of 0 (now) and of the wall-clock half.
                Some(t) if (t as i64) >= 0 => t.max(1),
                Some(_) => FOREVER,
                None if delta < 0 => 1,
                None => FOREVER,
            }
        }
    }
}

fn wall(nanos: i64, delta: i64) -> u64 {
    match nanos.checked_add(delta) {
        Some(t) if t > 1 => (-t) as u64,
        Some(_) => (-2i64) as u64,
        None => FOREVER,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_walltime(when: *const libc::timespec, delta: i64) -> u64 {
    // SAFETY: the caller passes a timespec or null (now).
    let nanos = match unsafe { when.as_ref() } {
        None => wall_now(),
        Some(ts) => ts.tv_sec.saturating_mul(1_000_000_000).saturating_add(ts.tv_nsec),
    };
    wall(nanos, delta)
}

/// The instant a dispatch time means; `None` for never.
pub(crate) fn deadline(when: u64) -> Option<Instant> {
    let now = Instant::now();
    match when {
        FOREVER => None,
        NOW => Some(now),
        when if is_wall(when) => {
            let target = if when == WALLTIME_NOW { wall_now() } else { -(when as i64) };
            let ahead = target.saturating_sub(wall_now());
            if ahead <= 0 { Some(now) } else { now.checked_add(Duration::from_nanos(ahead as u64)) }
        }
        when => ORIGIN.checked_add(Duration::from_nanos(when)),
    }
}
