//! `NSLock`, `NSRecursiveLock`, `NSCondition` and `NSConditionLock`.
//!
//! Objective-C locks are taken and released by separate messages, so none
//! of them can hold a Rust guard across calls: each keeps its state (held
//! or not, the owner and depth for the recursive lock, the condition
//! value) in a `std::sync::Mutex` and parks waiters on a `Condvar`.
//! Waits with a date turn it into a monotonic deadline once.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSDate, NSInteger, NSLocking, NSString};

sidestep_runtime::static_class!(pub(crate) NSLOCK, NSLOCK_META = "NSLock", || {
    let _ = NSLockImpl::class();
    crate::perform::install();
});

sidestep_runtime::static_class!(pub(crate) NSRECURSIVELOCK, NSRECURSIVELOCK_META = "NSRecursiveLock", || {
    let _ = NSRecursiveLockImpl::class();
    crate::perform::install();
});

sidestep_runtime::static_class!(pub(crate) NSCONDITION, NSCONDITION_META = "NSCondition", || {
    let _ = NSConditionImpl::class();
    crate::perform::install();
});

sidestep_runtime::static_class!(pub(crate) NSCONDITIONLOCK, NSCONDITIONLOCK_META = "NSConditionLock", || {
    let _ = NSConditionLockImpl::class();
    crate::perform::install();
});

/// A deadline from a date, or none for dates beyond `Instant`'s range.
fn deadline(date: &NSDate) -> Option<Instant> {
    let seconds = crate::date::time_of(date) - crate::date::now();
    if seconds <= 0.0 {
        return Some(Instant::now());
    }
    Instant::now().checked_add(Duration::try_from_secs_f64(seconds).ok()?)
}

/// Wait on `cv` until `ready` holds or the deadline passes; whether it
/// holds.
fn wait_until<'a, T>(
    cv: &Condvar,
    mut guard: MutexGuard<'a, T>,
    deadline: Option<Instant>,
    ready: impl Fn(&T) -> bool,
) -> (MutexGuard<'a, T>, bool) {
    while !ready(&guard) {
        match deadline {
            None => guard = cv.wait(guard).unwrap_or_else(|e| e.into_inner()),
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    return (guard, false);
                }
                guard = cv.wait_timeout(guard, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }
    (guard, true)
}

#[derive(Default)]
pub(crate) struct LockIvars {
    held: Mutex<bool>,
    released: Condvar,
    name: Mutex<Option<String>>,
}

impl LockIvars {
    fn lock(&self, deadline: Option<Instant>) -> bool {
        let (mut held, acquired) = wait_until(&self.released, crate::thread::lock(&self.held), deadline, |h| !*h);
        if acquired {
            *held = true;
        }
        acquired
    }

    fn unlock(&self) {
        *crate::thread::lock(&self.held) = false;
        self.released.notify_one();
    }
}

fn name_of(name: &Mutex<Option<String>>) -> Option<Retained<NSString>> {
    crate::thread::lock(name).as_deref().map(NSString::from_str)
}

fn set_name(name: &Mutex<Option<String>>, value: Option<&NSString>) {
    *crate::thread::lock(name) = value.map(|v| v.to_string());
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSLock"]
    #[ivars = LockIvars]
    pub(crate) struct NSLockImpl;

    impl NSLockImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(LockIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(lock))]
        fn lock(&self) {
            self.ivars().lock(None);
        }

        #[unsafe(method(unlock))]
        fn unlock(&self) {
            self.ivars().unlock();
        }

        #[unsafe(method(tryLock))]
        fn try_lock(&self) -> bool {
            self.ivars().lock(Some(Instant::now()))
        }

        #[unsafe(method(lockBeforeDate:))]
        fn lock_before_date(&self, limit: &NSDate) -> bool {
            self.ivars().lock(deadline(limit))
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            name_of(&self.ivars().name)
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            set_name(&self.ivars().name, name);
        }
    }

    unsafe impl NSObjectProtocol for NSLockImpl {}
    unsafe impl NSLocking for NSLockImpl {}
);

#[derive(Default)]
struct Recursion {
    owner: i32,
    depth: usize,
}

#[derive(Default)]
pub(crate) struct RecursiveIvars {
    state: Mutex<Recursion>,
    released: Condvar,
    name: Mutex<Option<String>>,
}

impl RecursiveIvars {
    fn lock(&self, deadline: Option<Instant>) -> bool {
        let me = crate::thread::current_tid();
        let (mut state, acquired) =
            wait_until(&self.released, crate::thread::lock(&self.state), deadline, |s| s.depth == 0 || s.owner == me);
        if acquired {
            state.owner = me;
            state.depth += 1;
        }
        acquired
    }

    fn unlock(&self) {
        let mut state = crate::thread::lock(&self.state);
        if state.depth > 0 && state.owner == crate::thread::current_tid() {
            state.depth -= 1;
            if state.depth == 0 {
                drop(state);
                self.released.notify_one();
            }
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSRecursiveLock"]
    #[ivars = RecursiveIvars]
    pub(crate) struct NSRecursiveLockImpl;

    impl NSRecursiveLockImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(RecursiveIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(lock))]
        fn lock(&self) {
            self.ivars().lock(None);
        }

        #[unsafe(method(unlock))]
        fn unlock(&self) {
            self.ivars().unlock();
        }

        #[unsafe(method(tryLock))]
        fn try_lock(&self) -> bool {
            self.ivars().lock(Some(Instant::now()))
        }

        #[unsafe(method(lockBeforeDate:))]
        fn lock_before_date(&self, limit: &NSDate) -> bool {
            self.ivars().lock(deadline(limit))
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            name_of(&self.ivars().name)
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            set_name(&self.ivars().name, name);
        }
    }

    unsafe impl NSObjectProtocol for NSRecursiveLockImpl {}
    unsafe impl NSLocking for NSRecursiveLockImpl {}
);

#[derive(Default)]
struct ConditionState {
    held: bool,
    /// Signals not yet taken by a waiter.
    signals: usize,
    waiters: usize,
}

#[derive(Default)]
pub(crate) struct ConditionIvars {
    state: Mutex<ConditionState>,
    released: Condvar,
    signalled: Condvar,
    name: Mutex<Option<String>>,
}

impl ConditionIvars {
    fn lock(&self) {
        let (mut state, _) = wait_until(&self.released, crate::thread::lock(&self.state), None, |s| !s.held);
        state.held = true;
    }

    fn unlock(&self) {
        crate::thread::lock(&self.state).held = false;
        self.released.notify_one();
    }

    /// Release the lock, wait for a signal, and take the lock again.
    fn wait(&self, deadline: Option<Instant>) -> bool {
        let mut state = crate::thread::lock(&self.state);
        state.held = false;
        state.waiters += 1;
        self.released.notify_one();
        let (mut state, signalled) = wait_until(&self.signalled, state, deadline, |s| s.signals > 0);
        state.waiters -= 1;
        if signalled {
            state.signals -= 1;
        }
        let (mut state, _) = wait_until(&self.released, state, None, |s| !s.held);
        state.held = true;
        signalled
    }

    fn signal(&self, all: bool) {
        let mut state = crate::thread::lock(&self.state);
        let waiting = state.waiters.saturating_sub(state.signals);
        if waiting == 0 {
            return;
        }
        state.signals += if all { waiting } else { 1 };
        drop(state);
        if all {
            self.signalled.notify_all();
        } else {
            self.signalled.notify_one();
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSCondition"]
    #[ivars = ConditionIvars]
    pub(crate) struct NSConditionImpl;

    impl NSConditionImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ConditionIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(lock))]
        fn lock(&self) {
            self.ivars().lock();
        }

        #[unsafe(method(unlock))]
        fn unlock(&self) {
            self.ivars().unlock();
        }

        #[unsafe(method(wait))]
        fn wait(&self) {
            self.ivars().wait(None);
        }

        #[unsafe(method(waitUntilDate:))]
        fn wait_until_date(&self, limit: &NSDate) -> bool {
            self.ivars().wait(deadline(limit))
        }

        #[unsafe(method(signal))]
        fn signal(&self) {
            self.ivars().signal(false);
        }

        #[unsafe(method(broadcast))]
        fn broadcast(&self) {
            self.ivars().signal(true);
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            name_of(&self.ivars().name)
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            set_name(&self.ivars().name, name);
        }
    }

    unsafe impl NSObjectProtocol for NSConditionImpl {}
    unsafe impl NSLocking for NSConditionImpl {}
);

#[derive(Default)]
struct ConditionLockState {
    held: bool,
    condition: NSInteger,
}

#[derive(Default)]
pub(crate) struct ConditionLockIvars {
    state: Mutex<ConditionLockState>,
    released: Condvar,
    name: Mutex<Option<String>>,
}

impl ConditionLockIvars {
    fn lock(&self, condition: Option<NSInteger>, deadline: Option<Instant>) -> bool {
        let (mut state, acquired) = wait_until(&self.released, crate::thread::lock(&self.state), deadline, |s| {
            !s.held && condition.is_none_or(|c| c == s.condition)
        });
        if acquired {
            state.held = true;
        }
        acquired
    }

    fn unlock(&self, condition: Option<NSInteger>) {
        let mut state = crate::thread::lock(&self.state);
        state.held = false;
        if let Some(condition) = condition {
            state.condition = condition;
        }
        drop(state);
        // Waiters may want different conditions: wake them all to look.
        self.released.notify_all();
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSConditionLock"]
    #[ivars = ConditionLockIvars]
    pub(crate) struct NSConditionLockImpl;

    impl NSConditionLockImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_condition_lock(this, 0)
        }

        #[unsafe(method_id(initWithCondition:))]
        fn init_with_condition(this: Allocated<Self>, condition: NSInteger) -> Retained<Self> {
            init_condition_lock(this, condition)
        }

        #[unsafe(method(condition))]
        fn condition(&self) -> NSInteger {
            crate::thread::lock(&self.ivars().state).condition
        }

        #[unsafe(method(lock))]
        fn lock(&self) {
            self.ivars().lock(None, None);
        }

        #[unsafe(method(unlock))]
        fn unlock(&self) {
            self.ivars().unlock(None);
        }

        #[unsafe(method(lockWhenCondition:))]
        fn lock_when_condition(&self, condition: NSInteger) {
            self.ivars().lock(Some(condition), None);
        }

        #[unsafe(method(unlockWithCondition:))]
        fn unlock_with_condition(&self, condition: NSInteger) {
            self.ivars().unlock(Some(condition));
        }

        #[unsafe(method(tryLock))]
        fn try_lock(&self) -> bool {
            self.ivars().lock(None, Some(Instant::now()))
        }

        #[unsafe(method(tryLockWhenCondition:))]
        fn try_lock_when_condition(&self, condition: NSInteger) -> bool {
            self.ivars().lock(Some(condition), Some(Instant::now()))
        }

        #[unsafe(method(lockBeforeDate:))]
        fn lock_before_date(&self, limit: &NSDate) -> bool {
            self.ivars().lock(None, deadline(limit))
        }

        #[unsafe(method(lockWhenCondition:beforeDate:))]
        fn lock_when_condition_before_date(&self, condition: NSInteger, limit: &NSDate) -> bool {
            self.ivars().lock(Some(condition), deadline(limit))
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            name_of(&self.ivars().name)
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            set_name(&self.ivars().name, name);
        }
    }

    unsafe impl NSObjectProtocol for NSConditionLockImpl {}
    unsafe impl NSLocking for NSConditionLockImpl {}
);

fn init_condition_lock(this: Allocated<NSConditionLockImpl>, condition: NSInteger) -> Retained<NSConditionLockImpl> {
    let ivars = ConditionLockIvars {
        state: Mutex::new(ConditionLockState { held: false, condition }),
        ..ConditionLockIvars::default()
    };
    let this = this.set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}
