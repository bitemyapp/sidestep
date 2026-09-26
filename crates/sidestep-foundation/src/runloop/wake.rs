//! Putting a run loop's thread to sleep and waking it.
//!
//! A loop sleeps on its own condition variable rather than the thread's park
//! token, so programs that park the main thread themselves neither steal
//! the loop's wake-ups nor have theirs stolen. Waking is one atomic
//! operation unless the loop is actually asleep, and wake-ups while it runs
//! coalesce into one: the next sleep returns at once.
//!
//! Producers put their work where the loop will find it (the inbox, a
//! source's flag) before calling [`Wake::wake`], so a wake-up can never
//! arrive before the work it announces.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use crate::thread::lock;

const SLEEPING: u8 = 1;
const PENDING: u8 = 2;

#[derive(Default)]
pub(crate) struct Wake {
    state: AtomicU8,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Wake {
    /// Make the loop's current or next sleep return.
    pub(crate) fn wake(&self) {
        let prev = self.state.fetch_or(PENDING, Ordering::SeqCst);
        if prev & SLEEPING != 0 && prev & PENDING == 0 {
            // The sleeper holds the lock from announcing SLEEPING until it
            // waits, so this notification can't fall between the two.
            let _guard = lock(&self.lock);
            self.cv.notify_one();
        }
    }

    /// Sleep until woken or until `deadline`. Only the loop's own thread
    /// calls this.
    pub(crate) fn sleep(&self, deadline: Option<Instant>) {
        let mut guard = lock(&self.lock);
        if self.state.fetch_or(SLEEPING, Ordering::SeqCst) & PENDING == 0 {
            loop {
                guard = match deadline {
                    None => self.cv.wait(guard).unwrap_or_else(|e| e.into_inner()),
                    Some(deadline) => {
                        let now = Instant::now();
                        if now >= deadline {
                            break;
                        }
                        self.cv.wait_timeout(guard, deadline - now).unwrap_or_else(|e| e.into_inner()).0
                    }
                };
                if self.state.load(Ordering::SeqCst) & PENDING != 0 {
                    break;
                }
            }
        }
        // Whatever the wake-up announced is in place by now; the loop looks
        // for it next.
        self.state.store(0, Ordering::SeqCst);
    }
}
