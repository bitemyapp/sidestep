//! libdispatch's C ABI, as dispatch2 0.3 calls it: queues (main, global,
//! serial, concurrent), `dispatch_after`, `dispatch_once`, groups,
//! semaphores and sources.
//!
//! dispatch2 links `-ldispatch` on Linux; `build.rs` writes an empty
//! `libdispatch.a` so the linker finds the library, and the symbols come
//! from here. Work travels as the (function, context) pairs libdispatch
//! uses; a queue that must remember something about an item (the global
//! queue it runs as, a concurrent queue's barrier flag, a group) boxes the
//! pair once more with it. The main queue is the main run loop's (it runs in
//! the common modes); the global queues share one pool of workers
//! (`pool`); serial and concurrent queues schedule their work onto a
//! target queue (`queue`); deadlines wait on one timer thread (`after`).
//!
//! Dispatch objects are Objective-C objects (`object`), so retain and
//! release are the runtime's and dispatch2's objc2 integration works.
//!
//! Client code (work, handlers, `apply` iterations, `dispatch_once`) runs
//! through [`callout`]: a callout that unwinds aborts the process, as it
//! does with libdispatch, rather than leaving a queue's bookkeeping half
//! done and the queue wedged.

mod after;
mod group;
mod object;
mod pool;
mod queue;
mod semaphore;
mod source;
mod time;

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Condvar, Mutex};

use block2::DynBlock;

pub use queue::dispatch_get_global_queue;

use crate::thread::lock;

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_retain(object: *mut c_void) {
    // SAFETY: the caller passes a live dispatch object.
    unsafe { objc2::ffi::objc_retain(object.cast()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_release(object: *mut c_void) {
    // SAFETY: the caller owns a reference to a live dispatch object.
    unsafe { objc2::ffi::objc_release(object.cast()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_get_context(object: *mut c_void) -> *mut c_void {
    object::context(object)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_set_context(object: *mut c_void, context: *mut c_void) {
    object::set_context(object, context);
}

/// The finalizer runs with the context when the object is freed.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_set_finalizer_f(
    object: *mut c_void,
    finalizer: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    object::set_finalizer(object, finalizer.map(queue::function));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_suspend(object: *mut c_void) {
    if source::is_source(object) { source::suspend(object) } else { queue::suspend(object) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_resume(object: *mut c_void) {
    if source::is_source(object) { source::resume(object) } else { queue::resume(object) }
}

/// Starts a source, or a queue made inactive.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_activate(object: *mut c_void) {
    if source::is_source(object) {
        source::activate(object);
    } else {
        queue::activate(object);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_set_target_queue(object: *mut c_void, target: *mut c_void) {
    let target = if target.is_null() { dispatch_get_global_queue(0, 0) } else { target };
    if !source::is_source(object) {
        queue::set_target(object, target);
    }
}

/// Quality of service has no effect on Linux.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_set_qos_class_floor(_object: *mut c_void, _qos: u32, _relative_priority: i32) {
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_get_qos_class(_queue: *mut c_void, relative_priority: *mut i32) -> u32 {
    if !relative_priority.is_null() {
        // SAFETY: the caller passes room for an int.
        unsafe { relative_priority.write(0) };
    }
    // QOS_CLASS_UNSPECIFIED
    0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn dispatch_allow_send_signals(_preserve_signum: i32) -> i32 {
    0
}

/// Run the main thread's run loop for the rest of the process, serving the
/// main queue.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn dispatch_main() -> ! {
    let main = crate::runloop::current();
    assert!(main.is_main(), "sidestep: dispatch_main must be called on the main thread");
    loop {
        main.run_mode(crate::runloop::Mode::DEFAULT, None, false);
    }
}

/// Waiters for `dispatch_once` calls in progress, woken all together.
static ONCE: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

const ONCE_DONE: isize = !0;
const ONCE_RUNNING: isize = 1;

/// Run `f(ctx)` exactly once for the predicate. The predicate settles at
/// `!0`, which dispatch2 checks with an acquire load before calling here.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_once_f(
    predicate: *mut isize,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    // SAFETY: the caller's predicate is a static (or otherwise pinned)
    // word only dispatch_once touches, and isize has the alignment of
    // AtomicIsize.
    let state = unsafe { AtomicIsize::from_ptr(predicate) };
    if state.load(Ordering::Acquire) == ONCE_DONE {
        return;
    }
    if state.compare_exchange(0, ONCE_RUNNING, Ordering::Acquire, Ordering::Acquire).is_ok() {
        struct Settle<'a>(&'a AtomicIsize);
        impl Drop for Settle<'_> {
            fn drop(&mut self) {
                let _guard = lock(&ONCE.0);
                self.0.store(ONCE_DONE, Ordering::Release);
                ONCE.1.notify_all();
            }
        }
        let _settle = Settle(state);
        // SAFETY: the caller's function and context.
        callout(|| unsafe { f(ctx) });
        return;
    }
    let mut guard = lock(&ONCE.0);
    while state.load(Ordering::Acquire) != ONCE_DONE {
        guard = ONCE.1.wait(guard).unwrap_or_else(|e| e.into_inner());
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_once(predicate: *mut isize, block: *const DynBlock<dyn Fn()>) {
    unsafe extern "C" fn call(ctx: *mut c_void) {
        // SAFETY: the caller's block, alive for the call.
        unsafe { &*ctx.cast::<DynBlock<dyn Fn()>>() }.call(());
    }
    // SAFETY: forwarded contract.
    unsafe { dispatch_once_f(predicate, block.cast_mut().cast(), call) };
}

/// Run a client callout (work, a handler, an iteration). libdispatch can't
/// carry on after one unwinds, since the queue's bookkeeping around it
/// would be left half done, and neither can Sidestep: the process aborts,
/// as on macOS. The panic has been reported by then.
pub(crate) fn callout<R>(f: impl FnOnce() -> R) -> R {
    struct Abort;
    impl Drop for Abort {
        fn drop(&mut self) {
            eprintln!("sidestep: a dispatch callout unwound; aborting, as libdispatch does");
            std::process::abort();
        }
    }
    let abort = Abort;
    let result = f();
    std::mem::forget(abort);
    result
}

/// Queue `work` on the default global queue, for Sidestep's own use.
pub(crate) fn global_async(work: crate::runloop::core::Work) {
    queue::enqueue(dispatch_get_global_queue(0, 0), work, false);
}
