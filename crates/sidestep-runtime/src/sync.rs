//! `@synchronized`: a recursive lock per object.

use std::ffi::c_int;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};

use crate::object::{IMMORTAL, Kind, Object, SYNCHRONIZED, header, kind};
use crate::util::{Sharded, lock, thread_token};

const SUCCESS: c_int = 0;
const NOT_OWNING_THREAD: c_int = -1;

#[derive(Default)]
struct RecursiveLock {
    /// Owning thread token and recursion depth.
    state: Mutex<(usize, usize)>,
    released: Condvar,
}

/// Each object's lock, sharded by address like the other side tables.
static LOCKS: Sharded<Arc<RecursiveLock>> = Sharded::new();

#[unsafe(no_mangle)]
pub extern "C-unwind" fn objc_sync_enter(obj: *mut Object) -> c_int {
    if obj.is_null() {
        return SUCCESS;
    }
    // SAFETY: the caller passes a live object.
    match unsafe { kind(obj) } {
        Kind::Counted => {
            // Tells object_dispose to drop the lock with the object. The
            // bit is set while the caller holds a reference, so the final
            // release, a later change of the same word, carries it to
            // object_dispose.
            // SAFETY: as above.
            let rc = unsafe { &header(obj).rc };
            if rc.load(Ordering::Relaxed) & (IMMORTAL | SYNCHRONIZED) == 0 {
                rc.fetch_or(SYNCHRONIZED, Ordering::Relaxed);
            }
        }
        // The same for a heap block, which `_Block_release` frees.
        // SAFETY: as above.
        Kind::Block if unsafe { crate::blocks::is_heap(obj) } => unsafe { crate::blocks::mark(obj, SYNCHRONIZED) },
        _ => {}
    }
    let lk = LOCKS.lock(obj as usize).entry(obj as usize).or_default().clone();
    let me = thread_token();
    let mut state = lock(&lk.state);
    while state.1 > 0 && state.0 != me {
        state = lk.released.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    *state = (me, state.1 + 1);
    SUCCESS
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn objc_sync_exit(obj: *mut Object) -> c_int {
    if obj.is_null() {
        return SUCCESS;
    }
    let Some(lk) = LOCKS.lock(obj as usize).get(&(obj as usize)).cloned() else {
        return NOT_OWNING_THREAD;
    };
    let mut state = lock(&lk.state);
    if state.0 != thread_token() || state.1 == 0 {
        return NOT_OWNING_THREAD;
    }
    state.1 -= 1;
    if state.1 == 0 {
        state.0 = 0;
        lk.released.notify_one();
    }
    SUCCESS
}

/// Drop the lock of an object being freed.
pub(crate) fn forget(obj: *mut Object) {
    LOCKS.lock(obj as usize).remove(&(obj as usize));
}
