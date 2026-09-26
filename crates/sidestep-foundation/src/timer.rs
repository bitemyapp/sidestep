//! `NSTimer` and the run loop that fires timers. Each thread has its own
//! list of scheduled timers; AppKit's event loop asks for the next deadline
//! and fires what is due, and `-[NSRunLoop run]` does the same on its own.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSString, NSTimeInterval, NSTimer};

type TimerBlock = RcBlock<dyn Fn(NonNull<NSTimer>)>;

pub(crate) struct TimerIvars {
    interval: Cell<f64>,
    repeats: Cell<bool>,
    block: RefCell<Option<TimerBlock>>,
    next_fire: Cell<Instant>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTimer"]
    #[ivars = TimerIvars]
    pub(crate) struct NSTimerImpl;

    impl NSTimerImpl {
        #[unsafe(method_id(scheduledTimerWithTimeInterval:repeats:block:))]
        fn scheduled(interval: NSTimeInterval, repeats: bool, block: &DynBlock<dyn Fn(NonNull<NSTimer>)>) -> Retained<Self> {
            let timer = new_timer(interval, repeats, block);
            schedule(&timer);
            timer
        }

        #[unsafe(method_id(timerWithTimeInterval:repeats:block:))]
        fn unscheduled(interval: NSTimeInterval, repeats: bool, block: &DynBlock<dyn Fn(NonNull<NSTimer>)>) -> Retained<Self> {
            new_timer(interval, repeats, block)
        }

        #[unsafe(method(invalidate))]
        fn invalidate(&self) {
            self.ivars().block.replace(None);
        }

        #[unsafe(method(isValid))]
        fn is_valid(&self) -> bool {
            self.ivars().block.borrow().is_some()
        }

        #[unsafe(method(timeInterval))]
        fn time_interval(&self) -> NSTimeInterval {
            self.ivars().interval.get()
        }

        #[unsafe(method(fire))]
        fn fire_now(&self) {
            fire(self);
        }
    }

    unsafe impl NSObjectProtocol for NSTimerImpl {}
);

fn new_timer(interval: f64, repeats: bool, block: &DynBlock<dyn Fn(NonNull<NSTimer>)>) -> Retained<NSTimerImpl> {
    // Foundation substitutes 0.1 ms for non-positive intervals.
    let interval = if interval > 0.0 { interval } else { 0.0001 };
    let ivars = TimerIvars {
        interval: Cell::new(interval),
        repeats: Cell::new(repeats),
        block: RefCell::new(Some(block.copy())),
        next_fire: Cell::new(Instant::now() + Duration::from_secs_f64(interval)),
    };
    let this = <NSTimerImpl as objc2::AnyThread>::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn fire(timer: &NSTimerImpl) {
    let block = timer.ivars().block.borrow().clone();
    if let Some(block) = block {
        block.call((NonNull::from(timer).cast::<NSTimer>(),));
    }
    if !timer.ivars().repeats.get() {
        timer.ivars().block.replace(None);
    }
}

thread_local!(static TIMERS: RefCell<Vec<Retained<NSTimerImpl>>> = const { RefCell::new(Vec::new()) });

fn schedule(timer: &Retained<NSTimerImpl>) {
    TIMERS.with(|t| t.borrow_mut().push(timer.clone()));
}

/// When the next timer on this thread is due, if any.
pub fn next_timer_deadline() -> Option<Instant> {
    TIMERS.with(|t| {
        let mut timers = t.borrow_mut();
        timers.retain(|timer| timer.ivars().block.borrow().is_some());
        timers.iter().map(|timer| timer.ivars().next_fire.get()).min()
    })
}

/// Fire every timer on this thread that is due at `now`.
pub fn fire_due_timers(now: Instant) {
    let due: Vec<Retained<NSTimerImpl>> =
        TIMERS.with(|t| t.borrow().iter().filter(|timer| timer.ivars().next_fire.get() <= now).cloned().collect());
    for timer in due {
        let ivars = timer.ivars();
        if ivars.repeats.get() {
            // Skip missed firings rather than bursting to catch up.
            let interval = Duration::from_secs_f64(ivars.interval.get());
            let mut next = ivars.next_fire.get() + interval;
            if next <= now {
                next = now + interval;
            }
            ivars.next_fire.set(next);
        }
        autoreleasepool(|_| fire(&timer));
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSRunLoop"]
    pub(crate) struct NSRunLoopImpl;

    impl NSRunLoopImpl {
        #[unsafe(method_id(currentRunLoop))]
        fn current() -> Retained<Self> {
            run_loop()
        }

        #[unsafe(method_id(mainRunLoop))]
        fn main() -> Retained<Self> {
            run_loop()
        }

        #[unsafe(method(addTimer:forMode:))]
        fn add_timer(&self, timer: &NSTimerImpl, _mode: &NSString) {
            schedule(&timer.retain());
        }

        /// Runs until no timers remain.
        #[unsafe(method(run))]
        fn run(&self) {
            while let Some(deadline) = next_timer_deadline() {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                fire_due_timers(Instant::now());
            }
        }
    }

    unsafe impl NSObjectProtocol for NSRunLoopImpl {}
);

fn run_loop() -> Retained<NSRunLoopImpl> {
    thread_local!(static RUN_LOOP: Retained<NSRunLoopImpl> = {
        let this = <NSRunLoopImpl as objc2::AnyThread>::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    });
    RUN_LOOP.with(|r| r.clone())
}
