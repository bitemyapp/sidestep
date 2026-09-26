//! `NSThread`, and the thread helpers the rest of the crate uses.
//!
//! An `NSThread` started here runs on a Rust thread, inside an autorelease
//! pool, with its object as `+currentThread`. Threads Sidestep didn't start
//! (the main thread, Rust threads) get an object the first time they ask,
//! named after their kernel name. As on macOS: the main thread is named
//! "main"; a started thread without a name reads as the empty string; the
//! first start posts `NSWillBecomeMultiThreadedNotification` on the
//! starting thread; a thread made with a target and selector posts
//! `NSThreadWillExitNotification` on itself as it ends (one made with a
//! block doesn't); and the target and argument are released when it ends.
//! Names longer than the kernel's 15 bytes are cut short only in the
//! kernel.
//!
//! A thread's run loop is made when it starts, so work can be handed to it
//! at once (`-performSelector:onThread:…` right after `-start`), and ends
//! when the thread does: requests still waiting are dropped, and a caller
//! waiting for one is told the thread exited.

use std::cell::{Cell, RefCell};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSCopying, NSDate, NSQualityOfService, NSString, NSThread, NSTimeInterval, NSUInteger};

use crate::runloop::core::Shared;
use crate::runloop::modes::constant;

/// The calling thread's kernel thread id.
pub(crate) fn current_tid() -> i32 {
    thread_local!(static TID: Cell<i32> = const { Cell::new(0) });
    TID.try_with(|tid| match tid.get() {
        0 => {
            // SAFETY: gettid has no preconditions.
            let id = unsafe { libc::gettid() };
            tid.set(id);
            id
        }
        id => id,
    })
    // SAFETY: as above; only reached while the thread is exiting.
    .unwrap_or_else(|_| unsafe { libc::gettid() })
}

/// The main thread is the process's initial thread, whose thread id equals
/// the process id.
pub(crate) fn is_main_thread() -> bool {
    // SAFETY: getpid has no preconditions.
    current_tid() == unsafe { libc::getpid() }
}

/// Lock a mutex, ignoring poisoning: the data these locks guard stays
/// consistent when a callout panics, since no callout runs under them.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// [`lock`] for reading a `RwLock`.
pub(crate) fn lock_read<T>(lock: &std::sync::RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

/// [`lock`] for writing a `RwLock`.
pub(crate) fn lock_write<T>(lock: &std::sync::RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

/// `NSThread`'s default stack size, which macOS reports for every thread.
const DEFAULT_STACK: usize = 512 * 1024;
/// Rust code wants more stack than Objective-C code; threads get at least
/// this much, whatever `stackSize` says.
const MIN_RUST_STACK: usize = 2 * 1024 * 1024;

const NOT_STARTED: u8 = 0;
const EXECUTING: u8 = 1;
const FINISHED: u8 = 2;

/// Unwinds a thread out of `+[NSThread exit]`.
struct ThreadExit;

enum Entry {
    Block(RcBlock<dyn Fn()>),
    Target { target: Retained<AnyObject>, selector: Sel, argument: Option<Retained<AnyObject>> },
}

pub(crate) struct ThreadIvars {
    main: bool,
    state: AtomicU8,
    cancelled: AtomicBool,
    name: Mutex<Option<Retained<NSString>>>,
    stack_size: AtomicUsize,
    qos: AtomicIsize,
    priority: AtomicU64,
    entry: Mutex<Option<Entry>>,
    /// Whether the thread posts `NSThreadWillExitNotification`.
    exit_notice: bool,
    /// A thread Sidestep didn't start (the main thread, Rust threads).
    foreign: bool,
    /// The thread's run loop: made by the first request for it or by
    /// `-start`, and adopted by the thread when it runs.
    run_loop: OnceLock<Arc<Shared>>,
}

// SAFETY: the entry and name are only touched under their locks, and
// reference counting is thread-safe.
unsafe impl Send for ThreadIvars {}
unsafe impl Sync for ThreadIvars {}

impl ThreadIvars {
    fn new(entry: Option<Entry>) -> Self {
        ThreadIvars {
            main: false,
            state: AtomicU8::new(NOT_STARTED),
            cancelled: AtomicBool::new(false),
            name: Mutex::new(None),
            stack_size: AtomicUsize::new(DEFAULT_STACK),
            qos: AtomicIsize::new(NSQualityOfService::Default.0),
            priority: AtomicU64::new(0.5f64.to_bits()),
            exit_notice: matches!(entry, Some(Entry::Target { .. })),
            foreign: false,
            entry: Mutex::new(entry),
            run_loop: OnceLock::new(),
        }
    }

    /// A thread that is already running and that Sidestep didn't start.
    fn running(main: bool, name: String) -> Self {
        let ivars = ThreadIvars { main, foreign: true, ..ThreadIvars::new(None) };
        ivars.state.store(EXECUTING, Ordering::Relaxed);
        if main {
            ivars.qos.store(NSQualityOfService::UserInteractive.0, Ordering::Relaxed);
        }
        *lock(&ivars.name) = Some(NSString::from_str(&name));
        ivars
    }
}

static MULTI: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// This thread's `NSThread`.
    static CURRENT: RefCell<Option<Retained<NSThreadImpl>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSThread"]
    #[ivars = ThreadIvars]
    pub(crate) struct NSThreadImpl;

    impl NSThreadImpl {
        #[unsafe(method(isMainThread))]
        fn class_is_main_thread() -> bool {
            is_main_thread()
        }

        #[unsafe(method(isMainThread))]
        fn is_main_thread(&self) -> bool {
            self.ivars().main
        }

        #[unsafe(method_id(currentThread))]
        fn current_thread() -> Retained<Self> {
            current()
        }

        #[unsafe(method_id(mainThread))]
        fn main_thread() -> Retained<Self> {
            main()
        }

        #[unsafe(method(isMultiThreaded))]
        fn is_multi_threaded() -> bool {
            MULTI.load(Ordering::Acquire)
        }

        #[unsafe(method(detachNewThreadWithBlock:))]
        fn detach_block(block: &DynBlock<dyn Fn()>) {
            start(&make(Some(Entry::Block(block.copy()))));
        }

        #[unsafe(method(detachNewThreadSelector:toTarget:withObject:))]
        fn detach_selector(selector: Sel, target: &AnyObject, argument: Option<&AnyObject>) {
            start(&make(Some(target_entry(target, selector, argument))));
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ThreadIvars::new(None));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithBlock:))]
        fn init_with_block(this: Allocated<Self>, block: &DynBlock<dyn Fn()>) -> Retained<Self> {
            let this = this.set_ivars(ThreadIvars::new(Some(Entry::Block(block.copy()))));
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithTarget:selector:object:))]
        fn init_with_target(
            this: Allocated<Self>,
            target: &AnyObject,
            selector: Sel,
            argument: Option<&AnyObject>,
        ) -> Retained<Self> {
            let this = this.set_ivars(ThreadIvars::new(Some(target_entry(target, selector, argument))));
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(start))]
        fn start_method(&self) {
            start(self);
        }

        /// Runs the block or sends the selector; subclasses override it.
        #[unsafe(method(main))]
        fn main_method(&self) {
            run_entry(self);
        }

        #[unsafe(method(cancel))]
        fn cancel(&self) {
            self.ivars().cancelled.store(true, Ordering::Release);
        }

        #[unsafe(method(isCancelled))]
        fn is_cancelled(&self) -> bool {
            self.ivars().cancelled.load(Ordering::Acquire)
        }

        #[unsafe(method(isExecuting))]
        fn is_executing(&self) -> bool {
            self.ivars().state.load(Ordering::Acquire) == EXECUTING
        }

        #[unsafe(method(isFinished))]
        fn is_finished(&self) -> bool {
            self.ivars().state.load(Ordering::Acquire) == FINISHED
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            lock(&self.ivars().name).clone()
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            let name = name.map(|n| n.copy());
            if self.is_current() {
                set_kernel_name(&name.as_ref().map(|n| n.to_string()).unwrap_or_default());
            }
            let old = std::mem::replace(&mut *lock(&self.ivars().name), name);
            drop(old);
        }

        #[unsafe(method(stackSize))]
        fn stack_size(&self) -> NSUInteger {
            self.ivars().stack_size.load(Ordering::Relaxed)
        }

        #[unsafe(method(setStackSize:))]
        fn set_stack_size(&self, size: NSUInteger) {
            self.ivars().stack_size.store(size, Ordering::Relaxed);
        }

        #[unsafe(method(qualityOfService))]
        fn quality_of_service(&self) -> NSQualityOfService {
            NSQualityOfService(self.ivars().qos.load(Ordering::Relaxed))
        }

        #[unsafe(method(setQualityOfService:))]
        fn set_quality_of_service(&self, qos: NSQualityOfService) {
            self.ivars().qos.store(qos.0, Ordering::Relaxed);
        }

        #[unsafe(method(threadPriority))]
        fn thread_priority(&self) -> f64 {
            f64::from_bits(self.ivars().priority.load(Ordering::Relaxed))
        }

        #[unsafe(method(setThreadPriority:))]
        fn set_thread_priority(&self, priority: f64) {
            self.ivars().priority.store(priority.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }

        #[unsafe(method(threadPriority))]
        fn class_thread_priority() -> f64 {
            f64::from_bits(current().ivars().priority.load(Ordering::Relaxed))
        }

        #[unsafe(method(setThreadPriority:))]
        fn class_set_thread_priority(priority: f64) -> bool {
            current().ivars().priority.store(priority.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
            true
        }

        #[unsafe(method(sleepForTimeInterval:))]
        fn sleep_for(seconds: NSTimeInterval) {
            sleep(seconds);
        }

        #[unsafe(method(sleepUntilDate:))]
        fn sleep_until(date: &NSDate) {
            sleep(date.timeIntervalSinceReferenceDate() - crate::date::now());
        }

        /// Ends the calling thread, which must be one `NSThread` started.
        #[unsafe(method(exit))]
        fn exit() {
            let started = CURRENT.try_with(|c| c.borrow().as_ref().is_some_and(|t| !t.ivars().foreign)).unwrap_or(false);
            assert!(started, "sidestep: +[NSThread exit] only ends threads NSThread started");
            std::panic::resume_unwind(Box::new(ThreadExit));
        }
    }

    unsafe impl NSObjectProtocol for NSThreadImpl {}
);

impl NSThreadImpl {
    fn is_current(&self) -> bool {
        CURRENT.try_with(|c| c.borrow().as_ref().is_some_and(|t| std::ptr::eq(&**t, self))).unwrap_or(false)
            || (self.ivars().main && is_main_thread())
    }

    /// The thread's run loop. One not started yet gets the loop it will
    /// run; one that has ended, its closed loop.
    pub(crate) fn run_loop(&self) -> Arc<Shared> {
        if self.ivars().main {
            return crate::runloop::core::main_shared().clone();
        }
        self.ivars().run_loop.get_or_init(Shared::for_new_thread).clone()
    }
}

fn target_entry(target: &AnyObject, selector: Sel, argument: Option<&AnyObject>) -> Entry {
    Entry::Target { target: target.retain(), selector, argument: argument.map(|a| a.retain()) }
}

/// A new thread object. Only called from the class's own methods.
fn make(entry: Option<Entry>) -> Retained<NSThreadImpl> {
    let this = NSThreadImpl::alloc().set_ivars(ThreadIvars::new(entry));
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn run_entry(thread: &NSThreadImpl) {
    enum Now {
        Block(RcBlock<dyn Fn()>),
        Target(Retained<AnyObject>, Sel, Option<Retained<AnyObject>>),
    }
    let now = match &*lock(&thread.ivars().entry) {
        None => return,
        Some(Entry::Block(block)) => Now::Block(block.clone()),
        Some(Entry::Target { target, selector, argument }) => Now::Target(target.clone(), *selector, argument.clone()),
    };
    match now {
        Now::Block(block) => block.call(()),
        // SAFETY: a thread's selector takes its argument.
        Now::Target(target, selector, argument) => unsafe {
            crate::perform::send_object(&target, selector, argument.as_deref())
        },
    }
}

fn sleep(seconds: f64) {
    if seconds > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(seconds.min(1e9)));
    }
}

/// A thread object handed to the thread it describes.
struct SendThread(Retained<NSThreadImpl>);

// SAFETY: a thread object's state is behind locks and atomics.
unsafe impl Send for SendThread {}

fn start(thread: &NSThreadImpl) {
    let ivars = thread.ivars();
    assert!(
        ivars.state.compare_exchange(NOT_STARTED, EXECUTING, Ordering::AcqRel, Ordering::Acquire).is_ok(),
        "sidestep: -[NSThread start]: attempt to start the thread again"
    );
    if !MULTI.swap(true, Ordering::AcqRel) {
        crate::notification_center::post(constant(&WILL_BECOME_MULTI_THREADED), None, None);
    }
    let name = {
        let mut name = lock(&ivars.name);
        name.get_or_insert_with(|| NSString::from_str("")).to_string()
    };
    let mut builder =
        std::thread::Builder::new().stack_size(ivars.stack_size.load(Ordering::Relaxed).max(MIN_RUST_STACK));
    if !name.is_empty() {
        builder = builder.name(kernel_name(&name).to_string());
    }
    // Made here, so work handed to the thread from now on waits for it.
    let run_loop = thread.run_loop();
    let thread = SendThread(thread.retain());
    builder
        .spawn(move || {
            let thread = thread;
            crate::runloop::core::adopt(run_loop);
            thread_main(&thread.0);
        })
        .expect("sidestep: couldn't start a thread");
}

fn thread_main(thread: &Retained<NSThreadImpl>) {
    CURRENT.with(|c| *c.borrow_mut() = Some(thread.clone()));
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: -main takes nothing.
        autoreleasepool(|_| {
            let _: () = unsafe { msg_send![&**thread, main] };
        })
    }));
    if thread.ivars().exit_notice {
        let name = constant(&WILL_EXIT);
        autoreleasepool(|_| crate::notification_center::post(name, Some(thread.as_ref()), None));
    }
    let entry = lock(&thread.ivars().entry).take();
    drop(entry);
    // The loop ends before the thread reads as finished, so a request made
    // once it does is refused at once.
    crate::runloop::core::teardown();
    thread.ivars().state.store(FINISHED, Ordering::Release);
    if let Err(payload) = result
        && !payload.is::<ThreadExit>()
    {
        std::panic::resume_unwind(payload);
    }
}

/// This thread's `NSThread`, made on first use for threads Sidestep didn't
/// start.
fn current() -> Retained<NSThreadImpl> {
    if let Ok(Some(thread)) = CURRENT.try_with(|c| c.borrow().clone()) {
        return thread;
    }
    let thread = if is_main_thread() {
        main()
    } else {
        let this = NSThreadImpl::alloc().set_ivars(ThreadIvars::running(false, foreign_name()));
        // SAFETY: NSObject's designated initializer.
        let thread: Retained<NSThreadImpl> = unsafe { msg_send![super(this), init] };
        let _ = thread.ivars().run_loop.set(crate::runloop::core::current_shared());
        thread
    };
    // Past the thread's thread-local destructors, the object isn't kept.
    CURRENT.try_with(|c| c.borrow_mut().get_or_insert_with(|| thread.clone()).clone()).unwrap_or(thread)
}

struct MainThread(*const NSThreadImpl);

// SAFETY: the main thread's object is never released; its state is behind
// locks and atomics.
unsafe impl Send for MainThread {}
unsafe impl Sync for MainThread {}

static MAIN_THREAD: OnceLock<MainThread> = OnceLock::new();

fn main() -> Retained<NSThreadImpl> {
    let main = MAIN_THREAD.get_or_init(|| {
        let this = NSThreadImpl::alloc().set_ivars(ThreadIvars::running(true, "main".into()));
        // SAFETY: NSObject's designated initializer.
        let thread: Retained<NSThreadImpl> = unsafe { msg_send![super(this), init] };
        MainThread(Retained::into_raw(thread))
    });
    // SAFETY: the main thread's object is immortal.
    unsafe { Retained::retain(main.0.cast_mut()) }.expect("the main thread object exists")
}

/// The `NSThreadImpl` behind an `NSThread`.
pub(crate) fn thread_impl(thread: &AnyObject) -> Option<&NSThreadImpl> {
    thread.downcast_ref::<NSThread>().map(|t| {
        // SAFETY: every NSThread is an instance of this class.
        unsafe { &*(t as *const NSThread).cast::<NSThreadImpl>() }
    })
}

/// Start a thread that sends `selector` to `target`, as
/// `-performSelectorInBackground:withObject:` does.
pub(crate) fn detach(target: &AnyObject, selector: Sel, argument: Option<&AnyObject>) {
    load();
    start(&make(Some(target_entry(target, selector, argument))));
}

/// Load the class the `NSThread` shell names.
pub(crate) fn load() {
    // SAFETY: +class takes nothing and returns the receiver.
    let _: *const objc2::runtime::AnyClass = unsafe { msg_send![NSThread::class(), class] };
}

/// The kernel keeps 15 bytes of a thread's name.
fn kernel_name(name: &str) -> &str {
    let mut end = name.len().min(15);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].split('\0').next().unwrap_or("")
}

fn set_kernel_name(name: &str) {
    let mut bytes = [0u8; 16];
    let short = kernel_name(name);
    bytes[..short.len()].copy_from_slice(short.as_bytes());
    // SAFETY: a NUL-terminated name of at most 15 bytes for this thread.
    unsafe { libc::prctl(libc::PR_SET_NAME, bytes.as_ptr()) };
}

/// The kernel name of the calling thread, or "" when it just repeats the
/// process's (the kernel's default for threads nobody named).
fn foreign_name() -> String {
    let mut buf = [0u8; 16];
    // SAFETY: PR_GET_NAME writes at most 16 bytes.
    unsafe { libc::prctl(libc::PR_GET_NAME, buf.as_mut_ptr()) };
    let name = String::from_utf8_lossy(buf.split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
    let process = std::fs::read_to_string("/proc/self/comm").unwrap_or_default();
    if name == process.trim_end() { String::new() } else { name }
}

crate::runloop::modes::exported_strings! {
    NSWillBecomeMultiThreadedNotification, WILL_BECOME_MULTI_THREADED = "NSWillBecomeMultiThreadedNotification";
    NSDidBecomeSingleThreadedNotification, DID_BECOME_SINGLE_THREADED = "NSDidBecomeSingleThreadedNotification";
    NSThreadWillExitNotification, WILL_EXIT = "NSThreadWillExitNotification";
}
