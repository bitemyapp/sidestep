//! `@synchronized`: a recursive lock per object.

use std::collections::HashMap;
use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use crate::object::Object;
use crate::util::{lock, thread_token};

const SUCCESS: c_int = 0;
const NOT_OWNING_THREAD: c_int = -1;

#[derive(Default)]
struct RecursiveLock {
    /// Owning thread token and recursion depth.
    state: Mutex<(usize, usize)>,
    released: Condvar,
}

static LOCKS: LazyLock<Mutex<HashMap<usize, Arc<RecursiveLock>>>> = LazyLock::new(Default::default);

#[unsafe(no_mangle)]
pub extern "C-unwind" fn objc_sync_enter(obj: *mut Object) -> c_int {
    if obj.is_null() {
        return SUCCESS;
    }
    USED.store(true, Ordering::Relaxed);
    let lk = lock(&LOCKS).entry(obj as usize).or_default().clone();
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
    let Some(lk) = lock(&LOCKS).get(&(obj as usize)).cloned() else {
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
    if USED.load(Ordering::Relaxed) {
        lock(&LOCKS).remove(&(obj as usize));
    }
}

/// Set once any object has been locked, so freeing objects in programs that
/// never use `@synchronized` skips the table.
static USED: AtomicBool = AtomicBool::new(false);
