//! Dispatch groups: a count of unfinished work, waits for it to reach
//! zero, and work queued when it does.

use std::ffi::c_void;
use std::sync::{Condvar, Mutex};

use block2::DynBlock;

use super::object::{self, Kind, body};
use super::queue::{Obj, block_work, enqueue, function};
use crate::runloop::core::Work;
use crate::thread::lock;

#[derive(Default)]
struct State {
    count: isize,
    notify: Vec<(Obj, Work)>,
}

#[derive(Default)]
pub(crate) struct Group {
    state: Mutex<State>,
    cv: Condvar,
}

/// # Safety
/// `group` must be a live dispatch group.
unsafe fn group<'a>(group: *const c_void) -> &'a Group {
    // SAFETY: guaranteed by the caller.
    match unsafe { &body(group).kind } {
        Kind::Group(group) => group,
        _ => panic!("sidestep: a dispatch group function was passed an object that isn't a group"),
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn dispatch_group_create() -> *mut c_void {
    object::create(Kind::Group(Group::default()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_group_enter(g: *mut c_void) {
    // SAFETY: the caller passes a group.
    lock(&unsafe { group(g) }.state).count += 1;
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_group_leave(g: *mut c_void) {
    // SAFETY: the caller passes a group.
    let group = unsafe { group(g) };
    let ready = {
        let mut state = lock(&group.state);
        state.count -= 1;
        assert!(state.count >= 0, "sidestep: unbalanced call to dispatch_group_leave()");
        if state.count > 0 {
            return;
        }
        std::mem::take(&mut state.notify)
    };
    group.cv.notify_all();
    for (queue, work) in ready {
        enqueue(queue.ptr(), work, false);
    }
}

/// 0 when the group emptied, non-zero on timeout.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_group_wait(g: *mut c_void, timeout: u64) -> isize {
    // SAFETY: the caller passes a group.
    let group = unsafe { group(g) };
    let deadline = super::time::deadline(timeout);
    let mut state = lock(&group.state);
    while state.count > 0 {
        state = match deadline {
            None => group.cv.wait(state).unwrap_or_else(|e| e.into_inner()),
            Some(deadline) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return 49;
                }
                group.cv.wait_timeout(state, deadline - now).unwrap_or_else(|e| e.into_inner()).0
            }
        };
    }
    0
}

fn notify(g: *mut c_void, queue: *mut c_void, work: Work) {
    // SAFETY: the caller passes a group.
    let group = unsafe { group(g) };
    let mut state = lock(&group.state);
    if state.count == 0 {
        drop(state);
        enqueue(queue, work, false);
    } else {
        state.notify.push((Obj::retain(queue), work));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_group_notify_f(
    g: *mut c_void,
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    notify(g, queue, Work { f: function(f), ctx });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_group_notify(
    g: *mut c_void,
    queue: *mut c_void,
    block: *const DynBlock<dyn Fn()>,
) {
    // SAFETY: the caller passes a block.
    notify(g, queue, block_work(unsafe { &*block }));
}

/// Queue `work` on `queue` as part of the group.
fn group_async(g: *mut c_void, queue: *mut c_void, work: Work) {
    struct Member {
        group: Obj,
        work: Work,
    }
    unsafe extern "C-unwind" fn run(ctx: *mut c_void) {
        // SAFETY: made from a Box below; runs once.
        let member = unsafe { Box::from_raw(ctx.cast::<Member>()) };
        struct Leave(Obj);
        impl Drop for Leave {
            fn drop(&mut self) {
                // SAFETY: the group entered below.
                unsafe { dispatch_group_leave(self.0.ptr()) };
            }
        }
        let Member { group, work } = *member;
        let _leave = Leave(group);
        // SAFETY: queued work pairs a function with its own context.
        unsafe { work.run() };
    }
    // SAFETY: the caller passes a group.
    unsafe { dispatch_group_enter(g) };
    let member = Box::new(Member { group: Obj::retain(g), work });
    enqueue(queue, Work { f: run, ctx: Box::into_raw(member).cast() }, false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_group_async_f(
    g: *mut c_void,
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    group_async(g, queue, Work { f: function(f), ctx });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_group_async(
    g: *mut c_void,
    queue: *mut c_void,
    block: *const DynBlock<dyn Fn()>,
) {
    // SAFETY: the caller passes a block.
    group_async(g, queue, block_work(unsafe { &*block }));
}
