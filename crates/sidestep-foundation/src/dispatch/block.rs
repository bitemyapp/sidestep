//! Cancellable work items. Copies of a heap block share cancellation and
//! completion state; cancellation never interrupts an invocation in progress.
use std::collections::HashMap;
use std::ffi::{c_ulong, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, Weak};

use super::queue::{Obj, block_work, enqueue};
use crate::thread::lock;
use block2::{DynBlock, RcBlock};

type Block = DynBlock<dyn Fn()>;
// Dispatch's caller is responsible for making the captured state safe to
// execute on the destination queue, just as for dispatch_async.
struct Owned(RcBlock<dyn Fn()>);
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}
#[derive(Default)]
struct Completion {
    done: bool,
    notifications: Vec<(Obj, Owned)>,
}
struct State {
    barrier: bool,
    key: AtomicUsize,
    cancelled: AtomicBool,
    body: Owned,
    completion: Mutex<Completion>,
    cv: Condvar,
}
static BLOCKS: LazyLock<Mutex<HashMap<usize, Weak<State>>>> = LazyLock::new(Mutex::default);
impl Drop for State {
    fn drop(&mut self) {
        lock(&BLOCKS).remove(&self.key.load(Ordering::Relaxed));
    }
}
impl State {
    fn invoke(&self) {
        if !self.cancelled.load(Ordering::Acquire) {
            super::callout(|| self.body.0.call(()));
        }
        let notifications = {
            let mut completion = lock(&self.completion);
            completion.done = true;
            std::mem::take(&mut completion.notifications)
        };
        self.cv.notify_all();
        for (queue, block) in notifications {
            enqueue(queue.ptr(), block_work(&block.0), false);
        }
    }
}
fn state(block: *const Block) -> Arc<State> {
    lock(&BLOCKS)
        .get(&(block as usize))
        .and_then(Weak::upgrade)
        .expect("sidestep: expected a block created by dispatch_block_create")
}
pub(super) fn is_barrier(block: *const Block) -> bool {
    let state = lock(&BLOCKS).get(&(block as usize)).and_then(Weak::upgrade);
    state.is_some_and(|s| s.barrier)
}
const FLAGS: c_ulong = 0x3f;
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_block_create(flags: c_ulong, block: *const Block) -> *mut Block {
    if flags & !FLAGS != 0 || block.is_null() {
        return std::ptr::null_mut();
    }
    let state = Arc::new(State {
        barrier: flags & 1 != 0,
        key: AtomicUsize::new(0),
        cancelled: AtomicBool::new(false),
        // SAFETY: the caller supplies a live block; copy owns its captures.
        body: Owned(unsafe { &*block }.copy()),
        completion: Mutex::default(),
        cv: Condvar::new(),
    });
    let capture = state.clone();
    let result = RcBlock::into_raw(RcBlock::new(move || capture.invoke()));
    state.key.store(result as usize, Ordering::Relaxed);
    lock(&BLOCKS).insert(result as usize, Arc::downgrade(&state));
    result
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_block_create_with_qos_class(
    flags: c_ulong,
    qos: u32,
    priority: i32,
    block: *const Block,
) -> *mut Block {
    if !matches!(qos, 0 | 0x09 | 0x11 | 0x15 | 0x19 | 0x21)
        || !(-15..=0).contains(&priority)
        || (qos == 0 && priority != 0)
    {
        return std::ptr::null_mut();
    }
    // QoS follows the destination queue, as with other Sidestep dispatch work.
    unsafe { dispatch_block_create(flags, block) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_block_cancel(block: *const Block) {
    state(block).cancelled.store(true, Ordering::Release);
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_block_testcancel(block: *const Block) -> isize {
    state(block).cancelled.load(Ordering::Acquire) as isize
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_block_wait(block: *const Block, timeout: u64) -> isize {
    let state = state(block);
    let deadline = super::time::deadline(timeout);
    let mut completion = lock(&state.completion);
    while !completion.done {
        completion = match deadline {
            None => state.cv.wait(completion).unwrap_or_else(|e| e.into_inner()),
            Some(deadline) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return 49;
                }
                state.cv.wait_timeout(completion, deadline - now).unwrap_or_else(|e| e.into_inner()).0
            }
        };
    }
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_block_notify(
    block: *const Block,
    queue: *mut c_void,
    notification: *const Block,
) {
    let state = state(block);
    let mut completion = lock(&state.completion);
    // SAFETY: the caller supplies a live notification block.
    let notification = unsafe { &*notification };
    if completion.done {
        drop(completion);
        enqueue(queue, block_work(notification), false);
    } else {
        completion.notifications.push((Obj::retain(queue), Owned(notification.copy())));
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_block_perform(flags: c_ulong, block: *const Block) {
    // SAFETY: forwarded caller's block; the returned reference is ours.
    let Some(block) = (unsafe { RcBlock::from_raw(dispatch_block_create(flags, block)) }) else {
        return;
    };
    block.call(());
}
