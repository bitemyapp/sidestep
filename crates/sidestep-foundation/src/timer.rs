//! `NSTimer`, which is also CoreFoundation's `CFRunLoopTimer`: one object,
//! toll-free, as on macOS.
//!
//! A timer keeps what any thread may ask about in atomics (validity, fire
//! date, tolerance) and its callout and scheduling behind small locks; the
//! loop it is scheduled on keeps it in a heap ordered by monotonic due time
//! (see `runloop::timers`). Fire dates are wall-clock times at the API edge
//! and `Instant`s inside, converted when they are set.
//!
//! The semantics are pinned by `conformance/tests/runloop.rs`: a
//! non-repeating timer reports an interval of 0 and a repeating one an
//! interval of at least 0.1 ms; fire dates are clamped to CoreFoundation's
//! latest date; a repeating timer keeps its phase and drops the fires it
//! missed; invalidating releases the target and user info at once.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::CFRunLoopTimer;
use objc2_foundation::{NSDate, NSString, NSTimeInterval, NSTimer};

use crate::runloop::modes::{Mode, Registration};
use crate::runloop::{self, core::Shared, timers::TimerKey};
use crate::thread::lock;

/// CoreFoundation clamps fire dates to this, in seconds since 2001.
pub(crate) const LATEST_FIRE_DATE: f64 = 4_039_289_856.0;

/// The interval a repeating timer uses when asked for none.
const SHORTEST_REPEAT: f64 = 0.0001;

/// A CoreFoundation timer callback and its context.
pub(crate) struct Callback {
    pub(crate) callout: unsafe extern "C-unwind" fn(*mut CFRunLoopTimer, *mut c_void),
    pub(crate) info: *mut c_void,
    pub(crate) retain: Option<unsafe extern "C-unwind" fn(*const c_void) -> *const c_void>,
    pub(crate) release: Option<unsafe extern "C-unwind" fn(*const c_void)>,
}

// SAFETY: the context belongs to the timer, which CoreFoundation lets any
// thread invalidate and release (running the context's release there);
// the callout itself runs on the timer's loop's thread, as on macOS.
unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

impl Drop for Callback {
    fn drop(&mut self) {
        if let Some(release) = self.release {
            // SAFETY: the context's own release function, called once.
            unsafe { release(self.info) };
        }
    }
}

impl Callback {
    /// Call the callout. The timer may be invalidated while it runs (by the
    /// callout itself, typically), which releases the timer's reference to
    /// the context; so the callout gets a reference of its own, as on
    /// macOS, taken with the context's retain function and released after.
    /// A context without one stays alive through `callback` until then.
    fn call(callback: std::sync::Arc<Callback>, timer: *mut CFRunLoopTimer) {
        let Some(retain) = callback.retain else {
            // SAFETY: the callback's own callout and context, which the Arc
            // keeps alive until it returns.
            unsafe { (callback.callout)(timer, callback.info) };
            return;
        };
        // SAFETY: the context's own retain function, on a context the Arc
        // keeps alive until this reference is taken.
        let info = unsafe { retain(callback.info) }.cast_mut();
        let (callout, release) = (callback.callout, callback.release);
        drop(callback);
        struct Release(Option<unsafe extern "C-unwind" fn(*const c_void)>, *mut c_void);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(release) = self.0 {
                    // SAFETY: gives back the reference taken above, once.
                    unsafe { release(self.1) };
                }
            }
        }
        let _release = Release(release, info);
        // SAFETY: the callout with the context it was made with, which the
        // reference taken above keeps alive.
        unsafe { callout(timer, info) };
    }
}

/// What a timer does when it fires.
pub(crate) enum Action {
    Block(RcBlock<dyn Fn(NonNull<NSTimer>)>),
    CfBlock(RcBlock<dyn Fn(*mut CFRunLoopTimer)>),
    Target {
        target: Retained<AnyObject>,
        selector: Sel,
    },
    Callback(std::sync::Arc<Callback>),
    /// A delayed `performSelector:`: target, selector and argument.
    Perform {
        target: Retained<AnyObject>,
        selector: Sel,
        argument: Option<Retained<AnyObject>>,
    },
}

/// A clone of an [`Action`]'s callable, taken so the lock isn't held
/// during the callout.
enum Callout {
    Block(RcBlock<dyn Fn(NonNull<NSTimer>)>),
    CfBlock(RcBlock<dyn Fn(*mut CFRunLoopTimer)>),
    Target(Retained<AnyObject>, Sel),
    Callback(std::sync::Arc<Callback>),
    Perform(Retained<AnyObject>, Sel, Option<Retained<AnyObject>>),
}

/// What a timer holds on to until it is invalidated.
pub(crate) struct Payload {
    pub(crate) action: Option<Action>,
    pub(crate) user_info: Option<Retained<AnyObject>>,
}

/// Where a timer is scheduled. Written by whoever changes the schedule;
/// the loop's heap follows it on the loop's thread.
pub(crate) struct Sched {
    pub(crate) owner: Option<std::sync::Arc<Shared>>,
    pub(crate) reg: Registration,
    /// When it next fires, on the monotonic clock.
    pub(crate) due: Instant,
    /// Its place in the owner's heap, if it has one.
    pub(crate) key: Option<TimerKey>,
    /// Bumped by every change of fire date, so firing can tell whether its
    /// callout moved the timer.
    pub(crate) retimed: u64,
}

pub(crate) struct TimerIvars {
    /// 0 for a timer that fires once.
    pub(crate) interval: f64,
    pub(crate) order: isize,
    valid: AtomicBool,
    firing: AtomicBool,
    /// Fire date in seconds since 2001, as f64 bits.
    fire: AtomicU64,
    tolerance: AtomicU64,
    payload: Mutex<Payload>,
    pub(crate) sched: Mutex<Sched>,
}

impl TimerIvars {
    pub(crate) fn new(fire: f64, interval: f64, order: isize, payload: Payload) -> Self {
        let fire = fire.min(LATEST_FIRE_DATE);
        TimerIvars {
            interval,
            order,
            valid: AtomicBool::new(true),
            firing: AtomicBool::new(false),
            fire: AtomicU64::new(fire.to_bits()),
            tolerance: AtomicU64::new(0f64.to_bits()),
            payload: Mutex::new(payload),
            sched: Mutex::new(Sched {
                owner: None,
                reg: Registration::default(),
                due: due_for(fire),
                key: None,
                retimed: 0,
            }),
        }
    }
}

/// The interval an `NSTimer` keeps: 0 when it doesn't repeat.
pub(crate) fn ns_interval(interval: f64, repeats: bool) -> f64 {
    match repeats {
        false => 0.0,
        true if interval > 0.0 => interval,
        true => SHORTEST_REPEAT,
    }
}

/// The monotonic time matching wall-clock `fire` (seconds since 2001).
pub(crate) fn due_for(fire: f64) -> Instant {
    let now = Instant::now();
    let ahead = fire - crate::date::now();
    if ahead.is_nan() || ahead <= 0.0 {
        // Overdue: as due as now, however long ago it was.
        return now;
    }
    // Clamped fire dates lie at most a few billion seconds ahead, which an
    // Instant holds; the fallback only guards against a broken clock.
    now.checked_add(Duration::from_secs_f64(ahead.min(LATEST_FIRE_DATE))).unwrap_or(now)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTimer"]
    #[ivars = TimerIvars]
    pub(crate) struct NSTimerImpl;

    impl NSTimerImpl {
        #[unsafe(method_id(timerWithTimeInterval:target:selector:userInfo:repeats:))]
        fn timer_with_target(
            interval: NSTimeInterval,
            target: &AnyObject,
            selector: Sel,
            user_info: Option<&AnyObject>,
            repeats: bool,
        ) -> Retained<Self> {
            with_target(interval, target, selector, user_info, repeats)
        }

        #[unsafe(method_id(scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:))]
        fn scheduled_with_target(
            interval: NSTimeInterval,
            target: &AnyObject,
            selector: Sel,
            user_info: Option<&AnyObject>,
            repeats: bool,
        ) -> Retained<Self> {
            scheduled(with_target(interval, target, selector, user_info, repeats))
        }

        #[unsafe(method_id(timerWithTimeInterval:repeats:block:))]
        fn timer_with_block(
            interval: NSTimeInterval,
            repeats: bool,
            block: &DynBlock<dyn Fn(NonNull<NSTimer>)>,
        ) -> Retained<Self> {
            with_block(interval, repeats, block)
        }

        #[unsafe(method_id(scheduledTimerWithTimeInterval:repeats:block:))]
        fn scheduled_with_block(
            interval: NSTimeInterval,
            repeats: bool,
            block: &DynBlock<dyn Fn(NonNull<NSTimer>)>,
        ) -> Retained<Self> {
            scheduled(with_block(interval, repeats, block))
        }

        #[unsafe(method_id(initWithFireDate:interval:repeats:block:))]
        fn init_with_block(
            this: Allocated<Self>,
            date: &NSDate,
            interval: NSTimeInterval,
            repeats: bool,
            block: &DynBlock<dyn Fn(NonNull<NSTimer>)>,
        ) -> Retained<Self> {
            let payload = Payload { action: Some(Action::Block(block.copy())), user_info: None };
            let fire = date.timeIntervalSinceReferenceDate();
            let this = this.set_ivars(TimerIvars::new(fire, ns_interval(interval, repeats), 0, payload));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithFireDate:interval:target:selector:userInfo:repeats:))]
        fn init_with_target(
            this: Allocated<Self>,
            date: &NSDate,
            interval: NSTimeInterval,
            target: &AnyObject,
            selector: Sel,
            user_info: Option<&AnyObject>,
            repeats: bool,
        ) -> Retained<Self> {
            let payload = Payload {
                action: Some(Action::Target { target: target.retain(), selector }),
                user_info: user_info.map(|u| u.retain()),
            };
            let fire = date.timeIntervalSinceReferenceDate();
            let this = this.set_ivars(TimerIvars::new(fire, ns_interval(interval, repeats), 0, payload));
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(fire))]
        fn fire(&self) {
            self.call();
            if self.ivars().interval == 0.0 {
                runloop::timers::invalidate(self);
            }
        }

        #[unsafe(method_id(fireDate))]
        fn fire_date(&self) -> Retained<NSDate> {
            NSDate::dateWithTimeIntervalSinceReferenceDate(self.fire_time())
        }

        #[unsafe(method(setFireDate:))]
        fn set_fire_date(&self, date: &NSDate) {
            runloop::timers::set_fire_date(self, date.timeIntervalSinceReferenceDate());
        }

        #[unsafe(method(timeInterval))]
        fn time_interval(&self) -> NSTimeInterval {
            self.ivars().interval
        }

        #[unsafe(method(tolerance))]
        fn tolerance(&self) -> NSTimeInterval {
            self.tolerance_value()
        }

        #[unsafe(method(setTolerance:))]
        fn set_tolerance(&self, tolerance: NSTimeInterval) {
            self.set_tolerance_value(tolerance);
        }

        #[unsafe(method(invalidate))]
        fn invalidate(&self) {
            runloop::timers::invalidate(self);
        }

        #[unsafe(method(isValid))]
        fn is_valid(&self) -> bool {
            self.valid()
        }

        #[unsafe(method_id(userInfo))]
        fn user_info(&self) -> Option<Retained<AnyObject>> {
            lock(&self.ivars().payload).user_info.clone()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let text = format!(
                "<NSTimer: {:p}> fire date: {}, interval: {}, valid: {}",
                self,
                self.fire_time(),
                self.ivars().interval,
                if self.valid() { "YES" } else { "NO" }
            );
            NSString::from_str(&text)
        }
    }

    unsafe impl NSObjectProtocol for NSTimerImpl {}
);

fn with_target(
    interval: f64,
    target: &AnyObject,
    selector: Sel,
    user_info: Option<&AnyObject>,
    repeats: bool,
) -> Retained<NSTimerImpl> {
    let payload = Payload {
        action: Some(Action::Target { target: target.retain(), selector }),
        user_info: user_info.map(|u| u.retain()),
    };
    NSTimerImpl::make(crate::date::now() + interval.max(0.0), ns_interval(interval, repeats), 0, payload)
}

fn with_block(interval: f64, repeats: bool, block: &DynBlock<dyn Fn(NonNull<NSTimer>)>) -> Retained<NSTimerImpl> {
    let payload = Payload { action: Some(Action::Block(block.copy())), user_info: None };
    NSTimerImpl::make(crate::date::now() + interval.max(0.0), ns_interval(interval, repeats), 0, payload)
}

/// Schedule on the current thread's loop in the default mode.
fn scheduled(timer: Retained<NSTimerImpl>) -> Retained<NSTimerImpl> {
    runloop::timers::add(&runloop::core::current_shared(), &timer, Mode::DEFAULT);
    timer
}

impl NSTimerImpl {
    /// A new timer. Only called from `NSTimer`'s own methods or after
    /// [`load`], so the class is the one the shell loaded.
    pub(crate) fn make(fire: f64, interval: f64, order: isize, payload: Payload) -> Retained<Self> {
        let this = Self::alloc().set_ivars(TimerIvars::new(fire, interval, order, payload));
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    pub(crate) fn valid(&self) -> bool {
        self.ivars().valid.load(Ordering::Acquire)
    }

    pub(crate) fn is_firing(&self) -> bool {
        self.ivars().firing.load(Ordering::Relaxed)
    }

    pub(crate) fn set_firing(&self, firing: bool) {
        self.ivars().firing.store(firing, Ordering::Relaxed);
    }

    /// Mark invalid; true if it was valid. The fire date then reads as the
    /// reference date, as on macOS.
    pub(crate) fn take_valid(&self) -> bool {
        let was = self.ivars().valid.swap(false, Ordering::AcqRel);
        if was {
            self.ivars().fire.store(0f64.to_bits(), Ordering::Release);
        }
        was
    }

    /// Give up the callout and user info, for the caller to drop outside
    /// any lock.
    pub(crate) fn take_payload(&self) -> Payload {
        let mut payload = lock(&self.ivars().payload);
        Payload { action: payload.action.take(), user_info: payload.user_info.take() }
    }

    /// Whether the timer's callout is a delayed perform matching these.
    /// `argument` of `None` matches any argument.
    pub(crate) fn is_perform(
        &self,
        target: &AnyObject,
        selector: Option<Sel>,
        argument: Option<Option<&AnyObject>>,
    ) -> bool {
        let payload = lock(&self.ivars().payload);
        let Some(Action::Perform { target: t, selector: s, argument: a }) = &payload.action else { return false };
        if !std::ptr::eq(&**t, target) || selector.is_some_and(|sel| sel != *s) {
            return false;
        }
        match argument {
            None => true,
            Some(None) => a.is_none(),
            Some(Some(arg)) => a.as_deref().is_some_and(|a| {
                // SAFETY: -isEqual: takes an object and returns BOOL.
                std::ptr::eq(a, arg) || unsafe { msg_send![a, isEqual: arg] }
            }),
        }
    }

    pub(crate) fn fire_time(&self) -> f64 {
        f64::from_bits(self.ivars().fire.load(Ordering::Acquire))
    }

    pub(crate) fn set_fire_time(&self, fire: f64) {
        self.ivars().fire.store(fire.to_bits(), Ordering::Release);
    }

    pub(crate) fn tolerance_value(&self) -> f64 {
        f64::from_bits(self.ivars().tolerance.load(Ordering::Relaxed))
    }

    pub(crate) fn set_tolerance_value(&self, tolerance: f64) {
        self.ivars().tolerance.store(tolerance.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn as_cf(&self) -> *mut CFRunLoopTimer {
        (self as *const Self).cast_mut().cast()
    }

    /// Run the callout, without holding any lock while it runs.
    pub(crate) fn call(&self) {
        let callout = {
            let payload = lock(&self.ivars().payload);
            match &payload.action {
                None => return,
                Some(Action::Block(b)) => Callout::Block(b.clone()),
                Some(Action::CfBlock(b)) => Callout::CfBlock(b.clone()),
                Some(Action::Target { target, selector }) => Callout::Target(target.clone(), *selector),
                Some(Action::Callback(c)) => Callout::Callback(c.clone()),
                Some(Action::Perform { target, selector, argument }) => {
                    Callout::Perform(target.clone(), *selector, argument.clone())
                }
            }
        };
        match callout {
            Callout::Block(block) => block.call((NonNull::from(self).cast::<NSTimer>(),)),
            Callout::CfBlock(block) => block.call((self.as_cf(),)),
            Callout::Target(target, selector) => {
                let timer: &AnyObject = self.as_ref();
                // SAFETY: a timer's action method takes the timer.
                unsafe { crate::perform::send_object(&target, selector, Some(timer)) };
            }
            Callout::Callback(callback) => Callback::call(callback, self.as_cf()),
            Callout::Perform(target, selector, argument) => {
                // SAFETY: a delayed perform's method takes one object.
                unsafe { crate::perform::send_object(&target, selector, argument.as_deref()) };
            }
        }
    }
}

/// Make sure the class the `NSTimer` shell names is loaded, so the
/// `define_class!` type can be used directly.
pub(crate) fn load() {
    // SAFETY: +class takes nothing and returns the receiver.
    let _: *const objc2::runtime::AnyClass = unsafe { msg_send![NSTimer::class(), class] };
}
