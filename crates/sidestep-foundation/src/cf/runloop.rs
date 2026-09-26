//! `CFRunLoop`, `CFRunLoopTimer` and `CFRunLoopObserver`: thin functions
//! over `crate::runloop`, with CoreFoundation's conventions. "Get"
//! functions return objects without a reference, "Create" and "Copy" ones
//! with one. Modes may be the common pseudo-mode wherever CoreFoundation
//! accepts it, and a loop may be driven from other threads in the ways
//! CoreFoundation allows (stop, wake up, perform a block, add and remove
//! timers and observers).

use std::ffi::c_void;

use block2::DynBlock;
use objc2::DefinedClass;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{
    CFRunLoop, CFRunLoopActivity, CFRunLoopObserver, CFRunLoopObserverContext, CFRunLoopTimer, CFRunLoopTimerContext,
};
use objc2_foundation::NSString;

use crate::runloop::core::{self, BlockModes, Shared, with_state};
use crate::runloop::modes::Mode;
use crate::runloop::nsrunloop::{CfRunLoopImpl, block_modes, boxed_block, limit_in};
use crate::runloop::observers::{self, ObserverIvars, ObserverObject};
use crate::runloop::{RunResult, timers};
use crate::timer::{self as ns_timer, Action, NSTimerImpl, Payload};

/// # Safety
/// `rl` must be a run loop from this module or `-getCFRunLoop`.
unsafe fn shared<'a>(rl: *const CFRunLoop) -> &'a std::sync::Arc<Shared> {
    // SAFETY: guaranteed by the caller.
    unsafe { CfRunLoopImpl::shared_of(rl) }
}

/// # Safety
/// `mode` must be null or a live string.
unsafe fn mode(mode: *const NSString) -> Option<Mode> {
    // SAFETY: guaranteed by the caller.
    unsafe { mode.as_ref() }.map(Mode::from_ns)
}

/// # Safety
/// `timer` must be a live `CFRunLoopTimerRef` (an `NSTimer`).
unsafe fn timer<'a>(timer: *const CFRunLoopTimer) -> &'a NSTimerImpl {
    // SAFETY: guaranteed by the caller.
    unsafe { &*timer.cast::<NSTimerImpl>() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFRunLoopGetCurrent() -> *mut CFRunLoop {
    let cf = crate::runloop::nsrunloop::current_objects().1;
    // Owned by the thread's loop state, which outlives the caller's use.
    Retained::as_ptr(&cf).cast_mut().cast()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFRunLoopGetMain() -> *mut CFRunLoop {
    let cf = crate::runloop::nsrunloop::main_objects().1;
    // The main loop's objects are never released.
    Retained::as_ptr(&cf).cast_mut().cast()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFRunLoopRun() {
    loop {
        match core::run(Mode::DEFAULT, None, false) {
            None | Some(RunResult::Stopped | RunResult::Finished) => break,
            Some(_) => {}
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopRunInMode(mode_name: *const NSString, seconds: f64, return_after: u8) -> i32 {
    // SAFETY: the caller passes a mode string or null.
    let Some(mode) = (unsafe { mode(mode_name) }) else { return RunResult::Finished as i32 };
    core::run(mode, limit_in(seconds), return_after != 0).unwrap_or(RunResult::Finished) as i32
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopStop(rl: *const CFRunLoop) {
    // SAFETY: the caller passes a run loop.
    unsafe { shared(rl) }.stop();
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopWakeUp(rl: *const CFRunLoop) {
    // SAFETY: the caller passes a run loop.
    unsafe { shared(rl) }.wake.wake();
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopIsWaiting(rl: *const CFRunLoop) -> u8 {
    // SAFETY: the caller passes a run loop.
    unsafe { shared(rl) }.waiting.load(std::sync::atomic::Ordering::Acquire) as u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopCopyCurrentMode(rl: *const CFRunLoop) -> *mut NSString {
    // SAFETY: the caller passes a run loop.
    match unsafe { shared(rl) }.running_mode() {
        Some(mode) => Retained::into_raw(objc2::Message::retain(mode.name())),
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopAddCommonMode(rl: *const CFRunLoop, mode_name: *const NSString) {
    // SAFETY: the caller passes a run loop and a mode.
    if let Some(mode) = unsafe { mode(mode_name) } {
        crate::runloop::RunLoop(unsafe { shared(rl) }.clone()).add_common_mode(mode);
    }
}

/// The fire date of the next timer in `mode`, or 0 without one. Answered
/// from the loop's own thread; other threads get 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopGetNextTimerFireDate(rl: *const CFRunLoop, mode_name: *const NSString) -> f64 {
    // SAFETY: the caller passes a run loop and a mode.
    let (shared, mode) = unsafe { (shared(rl), mode(mode_name)) };
    match mode {
        Some(mode) if shared.is_current() => {
            with_state(|s| s.next_due(mode).map_or(0.0, |(_, entry)| entry.timer.fire_time()))
        }
        _ => 0.0,
    }
}

/// Queue a block for `mode`, a mode string or an array of them. Doesn't
/// wake the loop (`CFRunLoopWakeUp` does), as on macOS.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopPerformBlock(
    rl: *const CFRunLoop,
    modes: *const AnyObject,
    block: *const DynBlock<dyn Fn()>,
) {
    // SAFETY: the caller passes a run loop, modes and a block.
    let (shared, modes, block) = unsafe { (shared(rl), modes.as_ref(), block.as_ref()) };
    let (Some(modes), Some(block)) = (modes, block) else { return };
    let modes: BlockModes = block_modes(modes);
    core::enqueue(shared, modes, boxed_block(block), false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopAddTimer(rl: *const CFRunLoop, t: *const CFRunLoopTimer, m: *const NSString) {
    // SAFETY: the caller passes a run loop, a timer and a mode.
    if let (Some(mode), false) = (unsafe { mode(m) }, t.is_null()) {
        timers::add(unsafe { shared(rl) }, unsafe { timer(t) }, mode);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopRemoveTimer(
    rl: *const CFRunLoop,
    t: *const CFRunLoopTimer,
    m: *const NSString,
) {
    // SAFETY: as above.
    if let (Some(mode), false) = (unsafe { mode(m) }, t.is_null()) {
        timers::remove(unsafe { shared(rl) }, unsafe { timer(t) }, mode);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopContainsTimer(
    rl: *const CFRunLoop,
    t: *const CFRunLoopTimer,
    m: *const NSString,
) -> u8 {
    // SAFETY: as above.
    match (unsafe { mode(m) }, t.is_null()) {
        (Some(mode), false) => timers::contains(unsafe { shared(rl) }, unsafe { timer(t) }, mode) as u8,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopAddObserver(
    rl: *const CFRunLoop,
    o: *const CFRunLoopObserver,
    m: *const NSString,
) {
    // SAFETY: the caller passes a run loop, an observer and a mode.
    if let (Some(mode), false) = (unsafe { mode(m) }, o.is_null()) {
        observers::add(unsafe { shared(rl) }, unsafe { ObserverObject::from_cf(o) }, mode);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopRemoveObserver(
    rl: *const CFRunLoop,
    o: *const CFRunLoopObserver,
    m: *const NSString,
) {
    // SAFETY: as above.
    if let (Some(mode), false) = (unsafe { mode(m) }, o.is_null()) {
        observers::remove(unsafe { shared(rl) }, unsafe { ObserverObject::from_cf(o) }, mode);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopContainsObserver(
    rl: *const CFRunLoop,
    o: *const CFRunLoopObserver,
    m: *const NSString,
) -> u8 {
    // SAFETY: as above.
    match (unsafe { mode(m) }, o.is_null()) {
        (Some(mode), false) => {
            observers::contains(unsafe { shared(rl) }, unsafe { ObserverObject::from_cf(o) }, mode) as u8
        }
        _ => 0,
    }
}

// Timers.

/// CoreFoundation timers repeat when their interval is positive.
fn cf_timer(fire: f64, interval: f64, order: isize, payload: Payload) -> *mut CFRunLoopTimer {
    ns_timer::load();
    let timer = NSTimerImpl::make(fire, interval.max(0.0), order, payload);
    Retained::into_raw(timer).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerCreate(
    _allocator: *const c_void,
    fire_date: f64,
    interval: f64,
    _flags: usize,
    order: isize,
    callout: Option<unsafe extern "C-unwind" fn(*mut CFRunLoopTimer, *mut c_void)>,
    context: *mut CFRunLoopTimerContext,
) -> *mut CFRunLoopTimer {
    let Some(callout) = callout else { return std::ptr::null_mut() };
    // SAFETY: the caller passes a context or null.
    let (info, retain, release) = match unsafe { context.as_ref() } {
        None => (std::ptr::null_mut(), None, None),
        Some(context) => (retain_info(context.info, context.retain), context.retain, context.release),
    };
    let callback = std::sync::Arc::new(ns_timer::Callback { callout, info, retain, release });
    cf_timer(fire_date, interval, order, Payload { action: Some(Action::Callback(callback)), user_info: None })
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerCreateWithHandler(
    _allocator: *const c_void,
    fire_date: f64,
    interval: f64,
    _flags: usize,
    order: isize,
    block: *const DynBlock<dyn Fn(*mut CFRunLoopTimer)>,
) -> *mut CFRunLoopTimer {
    // SAFETY: the caller passes a block or null; a timer without one fires
    // and does nothing, as on macOS.
    let action = unsafe { block.as_ref() }.map(|block| Action::CfBlock(block.copy()));
    cf_timer(fire_date, interval, order, Payload { action, user_info: None })
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerInvalidate(t: *const CFRunLoopTimer) {
    // SAFETY: the caller passes a timer.
    timers::invalidate(unsafe { timer(t) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerIsValid(t: *const CFRunLoopTimer) -> u8 {
    // SAFETY: the caller passes a timer.
    unsafe { timer(t) }.valid() as u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerGetNextFireDate(t: *const CFRunLoopTimer) -> f64 {
    // SAFETY: the caller passes a timer.
    unsafe { timer(t) }.fire_time()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerSetNextFireDate(t: *const CFRunLoopTimer, fire_date: f64) {
    // SAFETY: the caller passes a timer.
    timers::set_fire_date(unsafe { timer(t) }, fire_date);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerGetInterval(t: *const CFRunLoopTimer) -> f64 {
    // SAFETY: the caller passes a timer.
    unsafe { timer(t) }.ivars().interval
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerDoesRepeat(t: *const CFRunLoopTimer) -> u8 {
    // SAFETY: the caller passes a timer.
    (unsafe { timer(t) }.ivars().interval > 0.0) as u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerGetOrder(t: *const CFRunLoopTimer) -> isize {
    // SAFETY: the caller passes a timer.
    unsafe { timer(t) }.ivars().order
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerGetTolerance(t: *const CFRunLoopTimer) -> f64 {
    // SAFETY: the caller passes a timer.
    unsafe { timer(t) }.tolerance_value()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopTimerSetTolerance(t: *const CFRunLoopTimer, tolerance: f64) {
    // SAFETY: the caller passes a timer.
    unsafe { timer(t) }.set_tolerance_value(tolerance);
}

// Observers.

fn cf_observer(activities: usize, repeats: u8, order: isize, callout: observers::Callout) -> *mut CFRunLoopObserver {
    observers::load();
    let observer = ObserverObject::new(ObserverIvars::new(activities, repeats != 0, order, callout));
    Retained::into_raw(observer).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopObserverCreate(
    _allocator: *const c_void,
    activities: usize,
    repeats: u8,
    order: isize,
    callout: Option<unsafe extern "C-unwind" fn(*mut CFRunLoopObserver, CFRunLoopActivity, *mut c_void)>,
    context: *mut CFRunLoopObserverContext,
) -> *mut CFRunLoopObserver {
    let Some(callout) = callout else { return std::ptr::null_mut() };
    // SAFETY: the caller passes a context or null.
    let (info, release) = match unsafe { context.as_ref() } {
        None => (std::ptr::null_mut(), None),
        Some(context) => (retain_info(context.info, context.retain), context.release),
    };
    let callback = observers::Callback { callout, info, release };
    cf_observer(activities, repeats, order, observers::Callout::Callback(callback))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopObserverCreateWithHandler(
    _allocator: *const c_void,
    activities: usize,
    repeats: u8,
    order: isize,
    block: *const DynBlock<dyn Fn(*mut CFRunLoopObserver, CFRunLoopActivity)>,
) -> *mut CFRunLoopObserver {
    // SAFETY: the caller passes a block or null.
    let Some(block) = (unsafe { block.as_ref() }) else { return std::ptr::null_mut() };
    cf_observer(activities, repeats, order, observers::Callout::Block(block.copy()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopObserverInvalidate(o: *const CFRunLoopObserver) {
    // SAFETY: the caller passes an observer.
    observers::invalidate(unsafe { ObserverObject::from_cf(o) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopObserverIsValid(o: *const CFRunLoopObserver) -> u8 {
    // SAFETY: the caller passes an observer.
    unsafe { ObserverObject::from_cf(o) }.is_valid() as u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopObserverGetActivities(o: *const CFRunLoopObserver) -> usize {
    // SAFETY: the caller passes an observer.
    unsafe { ObserverObject::from_cf(o) }.ivars().activities
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopObserverGetOrder(o: *const CFRunLoopObserver) -> isize {
    // SAFETY: the caller passes an observer.
    unsafe { ObserverObject::from_cf(o) }.ivars().order
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRunLoopObserverDoesRepeat(o: *const CFRunLoopObserver) -> u8 {
    // SAFETY: the caller passes an observer.
    unsafe { ObserverObject::from_cf(o) }.ivars().repeats as u8
}

/// A context's info after its own retain function, if it has one.
fn retain_info(
    info: *mut c_void,
    retain: Option<unsafe extern "C-unwind" fn(*const c_void) -> *const c_void>,
) -> *mut c_void {
    match retain {
        // SAFETY: the context's own retain function.
        Some(retain) => unsafe { retain(info) }.cast_mut(),
        None => info,
    }
}
