//! `NSRunLoop` and the object behind `CFRunLoopRef`. Both wrap the same
//! per-thread loop; macOS also gives them different addresses. A thread's
//! objects live as long as the thread (the main thread's, as long as the
//! process), which is what lets `CFRunLoopGetCurrent` return them without
//! a retain. An `NSRunLoop` keeps its CoreFoundation object, so
//! `-getCFRunLoop` answers on any thread for as long as the `NSRunLoop`
//! lives, and the cross-thread CoreFoundation functions (stop, wake up)
//! work on the result.
//!
//! `-runMode:beforeDate:` runs until a source is handled, the date passes,
//! the run is stopped or the mode empties, and answers NO only when the
//! mode had nothing in it; `-run` and `-runUntilDate:` repeat it. Timers
//! alone don't end a run: on the main thread, where the main dispatch
//! queue keeps the common modes non-empty, a timer firing doesn't make
//! `-runMode:beforeDate:` return early (see `conformance/tests/runloop.rs`).

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::CFRunLoop;
use objc2_foundation::{NSDate, NSRunLoop, NSString, NSTimer};

use super::core::{self, BlockModes, Shared, with_state};
use super::modes::Mode;
use crate::timer::NSTimerImpl;

pub(crate) struct LoopIvars {
    pub(crate) shared: Arc<Shared>,
}

pub(crate) struct NsLoopIvars {
    shared: Arc<Shared>,
    cf: Retained<CfRunLoopImpl>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSRunLoop"]
    #[ivars = NsLoopIvars]
    pub(crate) struct NSRunLoopImpl;

    impl NSRunLoopImpl {
        #[unsafe(method_id(currentRunLoop))]
        fn current_run_loop() -> Retained<Self> {
            current_objects().0
        }

        #[unsafe(method_id(mainRunLoop))]
        fn main_run_loop() -> Retained<Self> {
            main_objects().0
        }

        #[unsafe(method_id(currentMode))]
        fn current_mode(&self) -> Option<Retained<NSString>> {
            self.shared().running_mode().map(|m| m.name().retain())
        }

        /// Not retained: this object keeps it. A raw pointer, because objc2
        /// checks the CoreFoundation encoding.
        #[unsafe(method(getCFRunLoop))]
        fn get_cf_run_loop(&self) -> *mut CFRunLoop {
            Retained::as_ptr(&self.ivars().cf).cast_mut().cast()
        }

        #[unsafe(method(addTimer:forMode:))]
        fn add_timer(&self, timer: &NSTimer, mode: &NSString) {
            super::timers::add(self.shared(), timer_impl(timer), Mode::from_ns(mode));
        }

        #[unsafe(method(run))]
        fn run(&self) {
            self.assert_current("-run");
            while core::run(Mode::DEFAULT, None, true).is_some() {}
        }

        #[unsafe(method(runUntilDate:))]
        fn run_until_date(&self, date: &NSDate) {
            self.assert_current("-runUntilDate:");
            let limit = limit_for(date);
            while limit.is_none_or(|l| Instant::now() < l) {
                if core::run(Mode::DEFAULT, limit, true).is_none() {
                    break;
                }
            }
        }

        #[unsafe(method(runMode:beforeDate:))]
        fn run_mode_before_date(&self, mode: &NSString, date: &NSDate) -> bool {
            self.assert_current("-runMode:beforeDate:");
            core::run(Mode::from_ns(mode), limit_for(date), true).is_some()
        }

        #[unsafe(method_id(limitDateForMode:))]
        fn limit_date_for_mode(&self, mode: &NSString) -> Option<Retained<NSDate>> {
            self.assert_current("-limitDateForMode:");
            limit_date(Mode::from_ns(mode))
        }

        #[unsafe(method(acceptInputForMode:beforeDate:))]
        fn accept_input(&self, mode: &NSString, date: &NSDate) {
            self.assert_current("-acceptInputForMode:beforeDate:");
            core::run(Mode::from_ns(mode), limit_for(date), true);
        }

        /// Like `CFRunLoopPerformBlock`, this doesn't wake the loop.
        #[unsafe(method(performBlock:))]
        fn perform_block(&self, block: &DynBlock<dyn Fn()>) {
            let mut modes = BlockModes::new();
            modes.add(Mode::DEFAULT);
            core::enqueue(self.shared(), modes, boxed_block(block), false);
        }

        #[unsafe(method(performInModes:block:))]
        fn perform_in_modes(&self, modes: &AnyObject, block: &DynBlock<dyn Fn()>) {
            core::enqueue(self.shared(), block_modes(modes), boxed_block(block), false);
        }
    }

    unsafe impl NSObjectProtocol for NSRunLoopImpl {}
);

impl NSRunLoopImpl {
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.ivars().shared
    }

    fn assert_current(&self, what: &str) {
        assert!(self.shared().is_current(), "sidestep: -[NSRunLoop {what}] sent from another thread than the loop's");
    }
}

// The object `CFRunLoopRef` points to. On macOS this is a CoreFoundation
// object; here, a private class.
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCFRunLoop"]
    #[ivars = LoopIvars]
    pub(crate) struct CfRunLoopImpl;

    unsafe impl NSObjectProtocol for CfRunLoopImpl {}
);

impl CfRunLoopImpl {
    /// The loop behind a `CFRunLoopRef`.
    ///
    /// # Safety
    /// `ptr` must come from `CFRunLoopGetCurrent`, `CFRunLoopGetMain` or
    /// `-getCFRunLoop`.
    pub(crate) unsafe fn shared_of<'a>(ptr: *const CFRunLoop) -> &'a Arc<Shared> {
        // SAFETY: guaranteed by the caller.
        unsafe { &(*ptr.cast::<CfRunLoopImpl>()).ivars().shared }
    }
}

/// `-limitDateForMode:`: one pass without sleeping, then the next timer's
/// fire date; the distant future if only sources remain, nil if nothing
/// does.
fn limit_date(mode: Mode) -> Option<Retained<NSDate>> {
    core::run(mode, Some(Instant::now()), true)?;
    let next = with_state(|s| match s.next_due(mode) {
        Some((_, entry)) => Some(Some(entry.timer.fire_time())),
        None if s.is_empty(mode) => None,
        None => Some(None),
    });
    next.map(|fire| match fire {
        Some(fire) => NSDate::dateWithTimeIntervalSinceReferenceDate(fire),
        None => NSDate::distantFuture(),
    })
}

/// A loop's Objective-C objects.
pub(crate) type Objects = (Retained<NSRunLoopImpl>, Retained<CfRunLoopImpl>);

fn make_objects(shared: &Arc<Shared>) -> Objects {
    // Load the class the NSRunLoop shell names before using its type.
    // SAFETY: +class takes nothing and returns the receiver.
    let _: *const objc2::runtime::AnyClass = unsafe { msg_send![NSRunLoop::class(), class] };
    let cf = CfRunLoopImpl::alloc().set_ivars(LoopIvars { shared: shared.clone() });
    // SAFETY: NSObject's designated initializer.
    let cf: Retained<CfRunLoopImpl> = unsafe { msg_send![super(cf), init] };
    let ns = NSRunLoopImpl::alloc().set_ivars(NsLoopIvars { shared: shared.clone(), cf: cf.clone() });
    // SAFETY: as above.
    let ns: Retained<NSRunLoopImpl> = unsafe { msg_send![super(ns), init] };
    (ns, cf)
}

/// The main loop's objects, which live as long as the process.
struct MainObjects(*const NSRunLoopImpl, *const CfRunLoopImpl);

// SAFETY: the objects are never released, and their state is thread-safe
// where other threads may reach it.
unsafe impl Send for MainObjects {}
unsafe impl Sync for MainObjects {}

static MAIN_OBJECTS: OnceLock<MainObjects> = OnceLock::new();

pub(crate) fn main_objects() -> Objects {
    let objects = MAIN_OBJECTS.get_or_init(|| {
        let (ns, cf) = make_objects(core::main_shared());
        MainObjects(Retained::into_raw(ns), Retained::into_raw(cf))
    });
    // SAFETY: the pointers are the main loop's immortal objects.
    unsafe { (Retained::retain(objects.0.cast_mut()).unwrap(), Retained::retain(objects.1.cast_mut()).unwrap()) }
}

/// This thread's loop objects.
pub(crate) fn current_objects() -> Objects {
    if let Some(objects) = with_state(|s| s.objects.clone()) {
        return objects;
    }
    let shared = core::current_shared();
    let objects = if shared.is_main() { main_objects() } else { make_objects(&shared) };
    with_state(|s| s.objects.get_or_insert(objects).clone())
}

/// The `NSTimerImpl` behind an `NSTimer`.
pub(crate) fn timer_impl(timer: &NSTimer) -> &NSTimerImpl {
    // SAFETY: every NSTimer is an instance of the class NSTimerImpl
    // defines, or of a subclass of it.
    unsafe { &*(timer as *const NSTimer).cast::<NSTimerImpl>() }
}

/// The deadline an `NSDate` limit means: `None` for dates too far away to
/// matter, now for dates already past.
pub(crate) fn limit_for(date: &NSDate) -> Option<Instant> {
    limit_in(date.timeIntervalSinceReferenceDate() - crate::date::now())
}

/// The deadline `seconds` from now, as `CFRunLoopRunInMode` takes it.
pub(crate) fn limit_in(seconds: f64) -> Option<Instant> {
    let now = Instant::now();
    if seconds.is_nan() || seconds <= 0.0 {
        return Some(now);
    }
    if seconds >= 1e18 {
        return None;
    }
    now.checked_add(Duration::from_secs_f64(seconds))
}

/// A block copied for running once on a loop's thread.
struct SendBlock(RcBlock<dyn Fn()>);

// SAFETY: a block is invoked on the loop's thread only, as CoreFoundation
// does; what it captures is the caller's business, as there. Reference
// counting blocks is thread-safe.
unsafe impl Send for SendBlock {}

pub(crate) fn boxed_block(block: &DynBlock<dyn Fn()>) -> Box<dyn FnOnce() + Send> {
    let block = SendBlock(block.copy());
    Box::new(move || {
        let block = block;
        block.0.call(());
    })
}

/// The modes a mode string or an array of them names.
pub(crate) fn block_modes(modes: &AnyObject) -> BlockModes {
    let mut set = BlockModes::new();
    if let Some(mode) = Mode::from_object(modes) {
        set.add(mode);
        return set;
    }
    // SAFETY: anything else passed as modes is an array of strings.
    let count: usize = unsafe { msg_send![modes, count] };
    for i in 0..count {
        // SAFETY: as above.
        let mode: *mut AnyObject = unsafe { msg_send![modes, objectAtIndex: i] };
        // SAFETY: arrays hold live objects.
        if let Some(mode) = unsafe { mode.as_ref() }.and_then(Mode::from_object) {
            set.add(mode);
        }
    }
    set
}
