//! Dispatch semaphores: a count, waits that take one, and signals that
//! add one and wake a waiter.

use std::ffi::c_void;
use std::sync::{Condvar, Mutex};

use super::object::{self, Kind, body};
use crate::thread::lock;

struct State {
    value: isize,
    waiters: usize,
}

pub(crate) struct Semaphore {
    state: Mutex<State>,
    cv: Condvar,
}

/// # Safety
/// `semaphore` must be a live dispatch semaphore.
unsafe fn semaphore<'a>(semaphore: *const c_void) -> &'a Semaphore {
    // SAFETY: guaranteed by the caller.
    match unsafe { &body(semaphore).kind } {
        Kind::Semaphore(semaphore) => semaphore,
        _ => panic!("sidestep: a dispatch semaphore function was passed an object that isn't a semaphore"),
    }
}

/// Null for a negative value, as libdispatch does.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn dispatch_semaphore_create(value: isize) -> *mut c_void {
    if value < 0 {
        return std::ptr::null_mut();
    }
    object::create(Kind::Semaphore(Semaphore { state: Mutex::new(State { value, waiters: 0 }), cv: Condvar::new() }))
}

/// 0 once a unit was taken, non-zero on timeout.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_semaphore_wait(s: *mut c_void, timeout: u64) -> isize {
    // SAFETY: the caller passes a semaphore.
    let semaphore = unsafe { semaphore(s) };
    let deadline = super::time::deadline(timeout);
    let mut state = lock(&semaphore.state);
    state.waiters += 1;
    while state.value <= 0 {
        state = match deadline {
            None => semaphore.cv.wait(state).unwrap_or_else(|e| e.into_inner()),
            Some(deadline) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    state.waiters -= 1;
                    return 49;
                }
                semaphore.cv.wait_timeout(state, deadline - now).unwrap_or_else(|e| e.into_inner()).0
            }
        };
    }
    state.waiters -= 1;
    state.value -= 1;
    0
}

/// Non-zero if a waiter was woken.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_semaphore_signal(s: *mut c_void) -> isize {
    // SAFETY: the caller passes a semaphore.
    let semaphore = unsafe { semaphore(s) };
    let mut state = lock(&semaphore.state);
    state.value += 1;
    if state.waiters > 0 {
        semaphore.cv.notify_one();
        1
    } else {
        0
    }
}
