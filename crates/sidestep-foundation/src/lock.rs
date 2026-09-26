//! `NSLock`, `NSRecursiveLock`, `NSCondition` and `NSConditionLock`.
//!
//! Objective-C locks are taken and released by separate messages, so none
//! of them can hold a Rust guard across calls. `NSLock`, `NSRecursiveLock`
//! and `NSCondition`'s lock are a [`RawLock`]: a futex word, so taking a
//! free lock and giving back one nobody waits for are one atomic operation
//! each, with no system call, as with the pthread mutexes they are on
//! macOS. The recursive lock adds its owner and depth, which only the
//! owner touches. `NSCondition` counts its waiters and signals under a
//! small mutex of their own, and `NSConditionLock` keeps its held flag and
//! condition value in a `Mutex` and wakes its `Condvar` only when someone
//! waits. Waits with a date turn it into a monotonic deadline once.

use std::sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};
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

/// A lock taken and released by separate calls, on a futex word: 0 when
/// free, 1 when held, 2 when held and someone may be waiting for it.
#[derive(Default)]
struct RawLock {
    state: AtomicU32,
}

const FREE: u32 = 0;
const HELD: u32 = 1;
const CONTENDED: u32 = 2;

impl RawLock {
    /// Take the lock, waiting until the deadline at most; whether it was
    /// taken.
    fn lock(&self, deadline: Option<Instant>) -> bool {
        if self.state.compare_exchange(FREE, HELD, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            return true;
        }
        self.lock_contended(deadline)
    }

    #[cold]
    fn lock_contended(&self, deadline: Option<Instant>) -> bool {
        // A lock held briefly is often free again after a short spin.
        for _ in 0..100 {
            match self.state.load(Ordering::Relaxed) {
                FREE => {
                    if self.state.compare_exchange(FREE, HELD, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                        return true;
                    }
                }
                HELD => std::hint::spin_loop(),
                _ => break,
            }
        }
        loop {
            // Marked contended, so whoever gives it back wakes a waiter.
            if self.state.swap(CONTENDED, Ordering::Acquire) == FREE {
                return true;
            }
            let timeout = match deadline {
                None => None,
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => Some(left),
                    _ => return false,
                },
            };
            futex_wait(&self.state, CONTENDED, timeout);
        }
    }

    fn unlock(&self) {
        if self.state.swap(FREE, Ordering::Release) == CONTENDED {
            futex_wake(&self.state);
        }
    }
}

/// Sleep while `word` holds `expected`, for `timeout` at most. Returns on
/// a wake-up, a timeout, a signal or a changed word alike: callers look
/// again.
fn futex_wait(word: &AtomicU32, expected: u32, timeout: Option<Duration>) {
    let timeout = timeout.map(|t| libc::timespec {
        tv_sec: t.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
        tv_nsec: t.subsec_nanos() as libc::c_long,
    });
    let timeout = timeout.as_ref().map_or(std::ptr::null(), |t| t as *const libc::timespec);
    // SAFETY: the word is a live, aligned u32 and the timeout null or a
    // live timespec; FUTEX_WAIT only reads them.
    unsafe {
        libc::syscall(libc::SYS_futex, word.as_ptr(), libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG, expected, timeout)
    };
}

/// Wake one thread sleeping on `word`.
fn futex_wake(word: &AtomicU32) {
    // SAFETY: FUTEX_WAKE only uses the word's address.
    unsafe { libc::syscall(libc::SYS_futex, word.as_ptr(), libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG, 1) };
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
    raw: RawLock,
    name: Mutex<Option<String>>,
}

impl LockIvars {
    fn lock(&self, deadline: Option<Instant>) -> bool {
        self.raw.lock(deadline)
    }

    fn unlock(&self) {
        self.raw.unlock();
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
pub(crate) struct RecursiveIvars {
    raw: RawLock,
    /// The holder's kernel thread id, 0 when free. Only the holder sets
    /// it to its own id, so a thread that reads its own id holds the lock.
    owner: AtomicI32,
    /// How many times the holder took it; only the holder touches it.
    depth: AtomicUsize,
    name: Mutex<Option<String>>,
}

impl RecursiveIvars {
    fn lock(&self, deadline: Option<Instant>) -> bool {
        let me = crate::thread::current_tid();
        if self.owner.load(Ordering::Relaxed) == me {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if !self.raw.lock(deadline) {
            return false;
        }
        self.owner.store(me, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
        true
    }

    /// Unlocking a lock the thread doesn't hold does nothing.
    fn unlock(&self) {
        if self.owner.load(Ordering::Relaxed) != crate::thread::current_tid() {
            return;
        }
        if self.depth.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.owner.store(0, Ordering::Relaxed);
            self.raw.unlock();
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
    /// Signals not yet taken by a waiter.
    signals: usize,
    waiters: usize,
}

#[derive(Default)]
pub(crate) struct ConditionIvars {
    raw: RawLock,
    state: Mutex<ConditionState>,
    signalled: Condvar,
    name: Mutex<Option<String>>,
}

impl ConditionIvars {
    fn lock(&self) {
        self.raw.lock(None);
    }

    fn unlock(&self) {
        self.raw.unlock();
    }

    /// Release the lock, wait for a signal, and take the lock again. The
    /// waiter counts itself before it lets the lock go, so a signal sent
    /// by whoever takes the lock next is never lost.
    fn wait(&self, deadline: Option<Instant>) -> bool {
        let mut state = crate::thread::lock(&self.state);
        state.waiters += 1;
        self.raw.unlock();
        let (mut state, signalled) = wait_until(&self.signalled, state, deadline, |s| s.signals > 0);
        state.waiters -= 1;
        if signalled {
            state.signals -= 1;
        }
        drop(state);
        self.raw.lock(None);
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
    /// Threads waiting to take the lock.
    waiters: usize,
}

#[derive(Default)]
pub(crate) struct ConditionLockIvars {
    state: Mutex<ConditionLockState>,
    released: Condvar,
    name: Mutex<Option<String>>,
}

impl ConditionLockIvars {
    fn lock(&self, condition: Option<NSInteger>, deadline: Option<Instant>) -> bool {
        let ready = |s: &ConditionLockState| !s.held && condition.is_none_or(|c| c == s.condition);
        let mut state = crate::thread::lock(&self.state);
        if !ready(&state) {
            state.waiters += 1;
            let (waited, acquired) = wait_until(&self.released, state, deadline, ready);
            state = waited;
            state.waiters -= 1;
            if !acquired {
                return false;
            }
        }
        state.held = true;
        true
    }

    fn unlock(&self, condition: Option<NSInteger>) {
        let mut state = crate::thread::lock(&self.state);
        state.held = false;
        if let Some(condition) = condition {
            state.condition = condition;
        }
        let waiters = state.waiters;
        drop(state);
        if waiters > 0 {
            // Waiters may want different conditions: wake them all to look.
            self.released.notify_all();
        }
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
        state: Mutex::new(ConditionLockState { held: false, condition, waiters: 0 }),
        ..ConditionLockIvars::default()
    };
    let this = this.set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}
