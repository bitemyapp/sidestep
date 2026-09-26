//! Dispatch queues.
//!
//! The main queue hands its work to the main run loop (see
//! `runloop::core::main_queue_push`), which runs it in the common modes.
//! The global queues are the pool's four priorities. A serial queue is a
//! FIFO that, when it goes from idle to having work, sends one drain job
//! to its target (a global queue unless set otherwise); the job runs up to
//! 64 items and then queues itself again, so a busy queue can't hog a
//! worker. A concurrent queue sends each item to its target as soon as no
//! barrier stands in the way; a barrier waits for the items before it and
//! runs alone.
//!
//! `dispatch_sync` runs the work on the calling thread, as libdispatch
//! does. An idle queue whose work goes straight to a global queue is taken
//! by the caller on the spot, with no other thread involved. Otherwise a
//! sync item waits its turn in the queue; when it comes, the queue is
//! handed to the waiting caller, which gives it back when its work is
//! done (a queue that targets another serial or concurrent queue instead
//! holds its place there while the caller runs). Sync on a global queue
//! simply runs; sync on the main queue from another thread runs on the
//! main thread and waits.
//!
//! A queue made with an initially-inactive attribute starts suspended once
//! more, until `dispatch_activate`. The attribute objects are immortal:
//! serial and concurrent, active and inactive.

use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use block2::{DynBlock, RcBlock};

use super::object::{self, Body, DISPATCH_CLASS, Function, Immortal, Kind, body};
use super::pool::{self, Priority};
use crate::runloop::core::{Work, main_queue_push};
use crate::thread::{is_main_thread, lock};

/// Most items a serial queue runs before letting other queues have the
/// worker.
const BATCH: usize = 64;

pub(crate) enum Label {
    Static(&'static CStr),
    Owned(CString),
}

impl Label {
    fn as_ptr(&self) -> *const c_char {
        match self {
            Label::Static(label) => label.as_ptr(),
            Label::Owned(label) => label.as_ptr(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum QueueKind {
    Main,
    Global(Priority),
    Serial,
    Concurrent,
}

struct Item {
    job: Job,
    barrier: bool,
}

/// What a queue item does: run work, or let a `dispatch_sync` caller run
/// its work on its own thread.
enum Job {
    Work(Work),
    Sync(Arc<Gate>),
}

struct State {
    items: VecDeque<Item>,
    /// Serial queues: a drain job is queued or running.
    scheduled: bool,
    /// Concurrent queues: items running, and whether one is a barrier.
    running: usize,
    barrier: bool,
}

struct Specific {
    key: usize,
    context: *mut c_void,
    destructor: Option<Function>,
}

pub(crate) struct Queue {
    kind: QueueKind,
    label: Label,
    state: Mutex<State>,
    suspended: AtomicUsize,
    /// Made inactive: suspended once more, until `dispatch_activate`.
    inactive: AtomicBool,
    /// Where the queue's work runs: a retained queue, or null for the
    /// default global queue.
    target: Mutex<Obj>,
    specifics: Mutex<Vec<Specific>>,
}

// SAFETY: queued work and specific contexts belong to whoever runs them, as
// libdispatch's contract says; everything else is behind locks.
unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Queue {
    const fn with(kind: QueueKind, label: Label, inactive: bool) -> Queue {
        Queue {
            kind,
            label,
            state: Mutex::new(State { items: VecDeque::new(), scheduled: false, running: 0, barrier: false }),
            suspended: AtomicUsize::new(inactive as usize),
            inactive: AtomicBool::new(inactive),
            target: Mutex::new(Obj::NULL),
            specifics: Mutex::new(Vec::new()),
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        for specific in self.specifics.get_mut().unwrap_or_else(|e| e.into_inner()).drain(..) {
            if let Some(destructor) = specific.destructor {
                // SAFETY: the destructor takes the context it was given with.
                unsafe { destructor(specific.context) };
            }
        }
    }
}

/// A counted reference to a dispatch object, or null.
pub(crate) struct Obj(*mut c_void);

// SAFETY: dispatch objects are thread-safe and their counts atomic.
unsafe impl Send for Obj {}
unsafe impl Sync for Obj {}

impl Obj {
    const NULL: Obj = Obj(std::ptr::null_mut());

    /// Take a new reference to `object`.
    pub(crate) fn retain(object: *const c_void) -> Obj {
        // SAFETY: callers pass live dispatch objects or null.
        Obj(unsafe { objc2::ffi::objc_retain(object.cast_mut().cast()) }.cast())
    }

    pub(crate) fn ptr(&self) -> *mut c_void {
        self.0
    }

    /// Give the reference up without releasing it.
    pub(crate) fn into_raw(self) -> *mut c_void {
        std::mem::ManuallyDrop::new(self).0
    }

    /// # Safety
    /// `object` must carry a reference the caller hands over.
    pub(crate) unsafe fn from_raw(object: *mut c_void) -> Obj {
        Obj(object)
    }
}

impl Drop for Obj {
    fn drop(&mut self) {
        // SAFETY: we own one reference (or hold null).
        unsafe { objc2::ffi::objc_release(self.0.cast()) };
    }
}

pub(crate) static MAIN: Immortal = Immortal::new(
    &DISPATCH_CLASS,
    Body::new(Kind::Queue(Queue::with(QueueKind::Main, Label::Static(c"com.apple.main-thread"), false))),
);

macro_rules! global {
    ($name:ident, $priority:ident, $label:literal) => {
        static $name: Immortal = Immortal::new(
            &DISPATCH_CLASS,
            Body::new(Kind::Queue(Queue::with(QueueKind::Global(Priority::$priority), Label::Static($label), false))),
        );
    };
}
global!(GLOBAL_HIGH, High, c"com.apple.root.user-initiated-qos");
global!(GLOBAL_DEFAULT, Default, c"com.apple.root.default-qos");
global!(GLOBAL_LOW, Low, c"com.apple.root.utility-qos");
global!(GLOBAL_BACKGROUND, Background, c"com.apple.root.background-qos");

macro_rules! attribute {
    ($name:ident, $concurrent:literal, $inactive:literal) => {
        pub(crate) static $name: Immortal = Immortal::new(
            &DISPATCH_CLASS,
            Body::new(Kind::Attribute { concurrent: $concurrent, inactive: $inactive }),
        );
    };
}
attribute!(CONCURRENT_ATTRIBUTE, true, false);
attribute!(SERIAL_ATTRIBUTE, false, false);
attribute!(SERIAL_INACTIVE_ATTRIBUTE, false, true);
attribute!(CONCURRENT_INACTIVE_ATTRIBUTE, true, true);

/// The attribute object with these properties.
fn attribute(concurrent: bool, inactive: bool) -> *mut c_void {
    object_of(match (concurrent, inactive) {
        (false, false) => &SERIAL_ATTRIBUTE,
        (true, false) => &CONCURRENT_ATTRIBUTE,
        (false, true) => &SERIAL_INACTIVE_ATTRIBUTE,
        (true, true) => &CONCURRENT_INACTIVE_ATTRIBUTE,
    })
}

/// Whether an attribute (null for `DISPATCH_QUEUE_SERIAL`) makes queues
/// concurrent, and inactive.
///
/// # Safety
/// `attr` must be null or a dispatch object.
unsafe fn attribute_flags(attr: *const c_void) -> (bool, bool) {
    if attr.is_null() {
        return (false, false);
    }
    // SAFETY: guaranteed by the caller.
    match unsafe { &body(attr).kind } {
        Kind::Attribute { concurrent, inactive } => (*concurrent, *inactive),
        _ => (false, false),
    }
}

// The ABI names the main queue and the concurrent attribute by their
// objects, so the symbols must be the objects themselves: the isa, just
// past each static's 16-byte header.
core::arch::global_asm!(
    ".globl _dispatch_main_q",
    ".set _dispatch_main_q, {main} + 16",
    ".globl _dispatch_queue_attr_concurrent",
    ".set _dispatch_queue_attr_concurrent, {concurrent} + 16",
    main = sym MAIN,
    concurrent = sym CONCURRENT_ATTRIBUTE,
);

fn object_of(object: &'static Immortal) -> *mut c_void {
    object.as_object().cast()
}

pub(crate) fn main_queue() -> *mut c_void {
    object_of(&MAIN)
}

fn global(priority: Priority) -> *mut c_void {
    object_of(match priority {
        Priority::High => &GLOBAL_HIGH,
        Priority::Default => &GLOBAL_DEFAULT,
        Priority::Low => &GLOBAL_LOW,
        Priority::Background => &GLOBAL_BACKGROUND,
    })
}

/// The queue state of a dispatch queue object.
///
/// # Safety
/// `queue` must be a live dispatch queue.
pub(crate) unsafe fn queue_of<'a>(queue: *const c_void) -> &'a Queue {
    // SAFETY: guaranteed by the caller.
    match unsafe { &body(queue).kind } {
        Kind::Queue(queue) => queue,
        _ => panic!("sidestep: a dispatch queue function was passed an object that isn't a queue"),
    }
}

thread_local! {
    /// The queue whose work this thread is running.
    static CURRENT: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

/// The queue the calling thread runs work for: the one it drains, else the
/// main queue on the main thread, else the default global queue.
pub(crate) fn current() -> *mut c_void {
    let current = CURRENT.with(|c| c.get());
    if !current.is_null() {
        current
    } else if is_main_thread() {
        main_queue()
    } else {
        global(Priority::Default)
    }
}

/// Run `f` as work of `queue` on this thread.
fn as_current<R>(queue: *mut c_void, f: impl FnOnce() -> R) -> R {
    struct Restore(*mut c_void);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.replace(queue)));
    f()
}

/// Queue `work` on `queue`.
pub(crate) fn enqueue(queue: *mut c_void, work: Work, barrier: bool) {
    // SAFETY: callers pass live queues.
    let q = unsafe { queue_of(queue) };
    match q.kind {
        QueueKind::Main => main_queue_push(work),
        QueueKind::Global(priority) => pool::submit(priority, tag(queue, work)),
        QueueKind::Serial | QueueKind::Concurrent => push_item(queue, q, Item { job: Job::Work(work), barrier }),
    }
}

/// Queue an item on a serial or concurrent queue.
fn push_item(queue: *mut c_void, q: &Queue, item: Item) {
    let mut state = lock(&q.state);
    state.items.push_back(item);
    if q.kind == QueueKind::Concurrent {
        pump(queue, q, &mut state);
    } else if !state.scheduled && q.suspended.load(Ordering::Acquire) == 0 {
        state.scheduled = true;
        drop(state);
        schedule_drain(queue, q);
    }
}

/// Whether the queue's work goes straight to a global queue.
fn targets_root(q: &Queue) -> bool {
    let target = lock(&q.target);
    // SAFETY: a queue's target is a live queue.
    target.ptr().is_null() || matches!(unsafe { queue_of(target.ptr()) }.kind, QueueKind::Global(_))
}

/// Work for a global queue, marked so it runs as that queue's work.
fn tag(queue: *mut c_void, work: Work) -> Work {
    struct Tagged {
        queue: *mut c_void,
        work: Work,
    }
    unsafe extern "C-unwind" fn run(ctx: *mut c_void) {
        // SAFETY: made from a Box below; runs once.
        let tagged = unsafe { Box::from_raw(ctx.cast::<Tagged>()) };
        // SAFETY: queued work pairs a function with its own context.
        as_current(tagged.queue, || unsafe { tagged.work.run() });
    }
    Work { f: run, ctx: Box::into_raw(Box::new(Tagged { queue, work })).cast() }
}

fn target_of(q: &Queue) -> Obj {
    let target = lock(&q.target);
    if target.ptr().is_null() { Obj::retain(global(Priority::Default)) } else { Obj::retain(target.ptr()) }
}

fn schedule_drain(queue: *mut c_void, q: &Queue) {
    unsafe extern "C-unwind" fn drain(ctx: *mut c_void) {
        // SAFETY: the job owns one reference to the queue.
        let queue = unsafe { Obj::from_raw(ctx) };
        // SAFETY: a live queue.
        let q = unsafe { queue_of(queue.ptr()) };
        // Whether the batch ended with items left for another drain job.
        let more = as_current(queue.ptr(), || {
            for _ in 0..BATCH {
                let item = {
                    let mut state = lock(&q.state);
                    if q.suspended.load(Ordering::Acquire) > 0 {
                        state.scheduled = false;
                        return false;
                    }
                    match state.items.pop_front() {
                        Some(item) => item,
                        None => {
                            state.scheduled = false;
                            return false;
                        }
                    }
                };
                match item.job {
                    // SAFETY: queued work pairs a function with its own context.
                    Job::Work(work) => objc2::rc::autoreleasepool(|_| unsafe { work.run() }),
                    // The caller now owns the queue (still marked
                    // scheduled) and restarts the drain when it is done.
                    Job::Sync(gate) if targets_root(q) => {
                        gate.set(OWNED);
                        return false;
                    }
                    // Hold this drain's place on the target meanwhile.
                    Job::Sync(gate) => {
                        gate.set(GO);
                        gate.wait_for(DONE);
                    }
                }
            }
            true
        });
        if more {
            schedule_drain(queue.ptr(), q);
        }
    }
    let target = target_of(q);
    let job = Work { f: drain, ctx: Obj::retain(queue).into_raw() };
    enqueue(target.ptr(), job, false);
}

/// Start the concurrent queue's runnable items. Called with its state
/// locked.
fn pump(queue: *mut c_void, q: &Queue, state: &mut State) {
    if q.suspended.load(Ordering::Acquire) > 0 {
        return;
    }
    while let Some(front) = state.items.front() {
        if state.barrier {
            break;
        }
        if front.barrier {
            if state.running == 0 {
                let item = state.items.pop_front().expect("the front item exists");
                state.barrier = true;
                start_item(queue, q, item);
            }
            break;
        }
        let item = state.items.pop_front().expect("the front item exists");
        state.running += 1;
        start_item(queue, q, item);
    }
}

/// Start a concurrent queue's item, counted as running (or as the running
/// barrier) by the caller. Called with the queue's state locked.
fn start_item(queue: *mut c_void, q: &Queue, item: Item) {
    struct Running {
        queue: Obj,
        work: Work,
        barrier: bool,
    }
    unsafe extern "C-unwind" fn run(ctx: *mut c_void) {
        // SAFETY: made from a Box below; runs once.
        let running = unsafe { Box::from_raw(ctx.cast::<Running>()) };
        let Running { queue, work, barrier } = *running;
        // SAFETY: queued work pairs a function with its own context.
        as_current(queue.ptr(), || unsafe { work.run() });
        // SAFETY: a live queue.
        finish_item(queue.ptr(), unsafe { queue_of(queue.ptr()) }, barrier);
    }
    let work = match item.job {
        Job::Work(work) => work,
        // The waiting caller runs it now, and finishes the item after.
        Job::Sync(gate) if targets_root(q) => return gate.set(OWNED),
        // Hold the item's place on the target while the caller runs.
        Job::Sync(gate) => Work { f: Gate::hold, ctx: Arc::into_raw(gate).cast_mut().cast() },
    };
    let running = Box::new(Running { queue: Obj::retain(queue), work, barrier: item.barrier });
    let target = target_of(q);
    enqueue(target.ptr(), Work { f: run, ctx: Box::into_raw(running).cast() }, false);
}

/// A concurrent queue's item is done: start what it held up.
fn finish_item(queue: *mut c_void, q: &Queue, barrier: bool) {
    let mut state = lock(&q.state);
    if barrier {
        state.barrier = false;
    } else {
        state.running -= 1;
    }
    pump(queue, q, &mut state);
}

/// Run `f(ctx)` on the calling thread as work of `queue`, once the queue
/// lets it, and return when it has run.
pub(crate) fn sync(queue: *mut c_void, ctx: *mut c_void, f: Function, barrier: bool) {
    // SAFETY: callers pass live queues.
    let q = unsafe { queue_of(queue) };
    // SAFETY: the caller's function and context.
    let run = || as_current(queue, || super::callout(|| unsafe { f(ctx) }));
    match q.kind {
        QueueKind::Global(_) => run(),
        QueueKind::Main if is_main_thread() => run(),
        QueueKind::Main => run_and_wait(queue, Work { f, ctx }),
        QueueKind::Serial | QueueKind::Concurrent => {
            if claim(q, barrier) {
                run();
                return give_back(queue, q, barrier);
            }
            // The gate is shared: the queue side may still be returning
            // from it when this side sees DONE and leaves.
            let gate = Arc::new(Gate::default());
            push_item(queue, q, Item { job: Job::Sync(gate.clone()), barrier });
            let handed_over = gate.wait_for(GO) == OWNED;
            run();
            if handed_over {
                give_back(queue, q, barrier);
            } else {
                gate.set(DONE);
            }
        }
    }
}

/// Take an idle queue for a `dispatch_sync` caller: a serial queue with
/// nothing queued or running, or a concurrent queue with nothing queued
/// and no barrier running (for a barrier, nothing running at all). Only a
/// queue whose work goes straight to a global queue qualifies; any other
/// has its target's order to keep too.
fn claim(q: &Queue, barrier: bool) -> bool {
    if !targets_root(q) {
        return false;
    }
    let mut state = lock(&q.state);
    if q.suspended.load(Ordering::Acquire) > 0 || !state.items.is_empty() {
        return false;
    }
    match q.kind {
        QueueKind::Serial if !state.scheduled => state.scheduled = true,
        QueueKind::Concurrent if !state.barrier && (!barrier || state.running == 0) => {
            if barrier {
                state.barrier = true;
            } else {
                state.running += 1;
            }
        }
        _ => return false,
    }
    true
}

/// Give back a queue a sync caller took or was handed, starting what
/// queued up meanwhile.
fn give_back(queue: *mut c_void, q: &Queue, barrier: bool) {
    if q.kind == QueueKind::Concurrent {
        return finish_item(queue, q, barrier);
    }
    let mut state = lock(&q.state);
    if state.items.is_empty() || q.suspended.load(Ordering::Acquire) > 0 {
        state.scheduled = false;
    } else {
        drop(state);
        schedule_drain(queue, q);
    }
}

/// Run `work` on `queue`'s own threads and wait for it.
pub(crate) fn run_and_wait(queue: *mut c_void, work: Work) {
    struct Waited {
        work: Work,
        gate: Arc<Gate>,
    }
    unsafe extern "C-unwind" fn run(ctx: *mut c_void) {
        // SAFETY: made from a Box below; runs once.
        let waited = unsafe { Box::from_raw(ctx.cast::<Waited>()) };
        struct Done(Arc<Gate>);
        impl Drop for Done {
            fn drop(&mut self) {
                self.0.set(DONE);
            }
        }
        let _done = Done(waited.gate.clone());
        // SAFETY: the caller's function and context; runs once.
        unsafe { waited.work.run() };
    }
    let gate = Arc::new(Gate::default());
    let waited = Box::new(Waited { work, gate: gate.clone() });
    enqueue(queue, Work { f: run, ctx: Box::into_raw(waited).cast() }, false);
    gate.wait_for(DONE);
}

/// The queue lets a sync caller go and holds its place until DONE.
const GO: u8 = 1;
const DONE: u8 = 2;
/// The queue is the sync caller's until it gives it back.
const OWNED: u8 = 3;

/// A handoff between a queue and a caller waiting on it.
#[derive(Default)]
struct Gate {
    state: Mutex<u8>,
    cv: Condvar,
}

impl Gate {
    fn set(&self, to: u8) {
        *lock(&self.state) = to;
        self.cv.notify_all();
    }

    /// Wait until the state is at least `at_least`, and return it.
    fn wait_for(&self, at_least: u8) -> u8 {
        let mut state = lock(&self.state);
        while *state < at_least {
            state = self.cv.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        *state
    }

    /// Queue side of a sync: let the caller go, and hold the queue until
    /// it is done.
    unsafe extern "C-unwind" fn hold(ctx: *mut c_void) {
        // SAFETY: one reference to the shared gate, handed over.
        let gate = unsafe { Arc::from_raw(ctx.cast_const().cast::<Gate>()) };
        gate.set(GO);
        gate.wait_for(DONE);
    }
}

/// `dispatch_function_t` is `extern "C"`; the same function unwinds no
/// differently when called through a `"C-unwind"` pointer, it aborts.
pub(crate) fn function(f: unsafe extern "C" fn(*mut c_void)) -> Function {
    // SAFETY: the two ABIs are the same apart from unwinding.
    unsafe { std::mem::transmute::<unsafe extern "C" fn(*mut c_void), Function>(f) }
}

/// A block as work: copied now, called and released once.
pub(crate) fn block_work(block: &DynBlock<dyn Fn()>) -> Work {
    unsafe extern "C-unwind" fn call(ctx: *mut c_void) {
        // SAFETY: the context is a copied block we own.
        let block = unsafe { RcBlock::<dyn Fn()>::from_raw(ctx.cast()) }.expect("a block");
        block.call(());
    }
    Work { f: call, ctx: RcBlock::into_raw(block.copy()).cast() }
}

// The C ABI.

#[unsafe(no_mangle)]
pub extern "C-unwind" fn dispatch_get_global_queue(identifier: isize, _flags: usize) -> *mut c_void {
    let priority = match identifier {
        // QOS_CLASS_USER_INTERACTIVE, QOS_CLASS_USER_INITIATED,
        // DISPATCH_QUEUE_PRIORITY_HIGH.
        0x21 | 0x19 | 2 => Priority::High,
        // QOS_CLASS_UTILITY, DISPATCH_QUEUE_PRIORITY_LOW.
        0x11 | -2 => Priority::Low,
        // QOS_CLASS_BACKGROUND, DISPATCH_QUEUE_PRIORITY_BACKGROUND.
        0x09 | -32768 => Priority::Background,
        _ => Priority::Default,
    };
    global(priority)
}

/// Quality of service and autorelease frequency don't change what a queue
/// does here (every item drains an autorelease pool of its own), so the
/// attribute stays serial or concurrent, active or inactive. Never null,
/// even for `DISPATCH_QUEUE_SERIAL`, as on macOS.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_attr_make_with_qos_class(
    attr: *mut c_void,
    _qos: u32,
    _relative_priority: i32,
) -> *mut c_void {
    // SAFETY: the caller passes an attribute or null.
    let (concurrent, inactive) = unsafe { attribute_flags(attr) };
    attribute(concurrent, inactive)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_attr_make_with_autorelease_frequency(
    attr: *mut c_void,
    _frequency: usize,
) -> *mut c_void {
    // SAFETY: the caller passes an attribute or null.
    let (concurrent, inactive) = unsafe { attribute_flags(attr) };
    attribute(concurrent, inactive)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_attr_make_initially_inactive(attr: *mut c_void) -> *mut c_void {
    // SAFETY: the caller passes an attribute or null.
    let (concurrent, _) = unsafe { attribute_flags(attr) };
    attribute(concurrent, true)
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void {
    // SAFETY: forwarded contract.
    unsafe { dispatch_queue_create_with_target(label, attr, std::ptr::null()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_create_with_target(
    label: *const c_char,
    attr: *const c_void,
    target: *const c_void,
) -> *mut c_void {
    // SAFETY: the caller passes an attribute or null.
    let (concurrent, inactive) = unsafe { attribute_flags(attr) };
    // SAFETY: the caller passes a C string or null.
    let label = match unsafe { label.as_ref() } {
        None => Label::Static(c""),
        Some(_) => Label::Owned(unsafe { CStr::from_ptr(label) }.to_owned()),
    };
    let kind = if concurrent { QueueKind::Concurrent } else { QueueKind::Serial };
    let queue = object::create(Kind::Queue(Queue::with(kind, label, inactive)));
    if !target.is_null() {
        // SAFETY: a new queue.
        *lock(&unsafe { queue_of(queue) }.target) = Obj::retain(target);
    }
    queue
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_get_label(queue: *const c_void) -> *const c_char {
    let queue = if queue.is_null() { current() } else { queue.cast_mut() };
    // SAFETY: a live queue.
    unsafe { queue_of(queue) }.label.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn dispatch_get_current_queue() -> *mut c_void {
    current()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_async_f(
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    enqueue(queue, Work { f: function(f), ctx }, false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_async(queue: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    // SAFETY: the caller passes a block.
    enqueue(queue, block_work(unsafe { &*block }), false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_barrier_async_f(
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    enqueue(queue, Work { f: function(f), ctx }, true);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_barrier_async(queue: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    // SAFETY: the caller passes a block.
    enqueue(queue, block_work(unsafe { &*block }), true);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_sync_f(
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    sync(queue, ctx, function(f), false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_sync(queue: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    let work = block_work(unsafe { &*block });
    sync(queue, work.ctx, work.f, false);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_barrier_sync_f(
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    sync(queue, ctx, function(f), true);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_barrier_sync(queue: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    let work = block_work(unsafe { &*block });
    sync(queue, work.ctx, work.f, true);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_async_and_wait_f(
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    run_and_wait(queue, Work { f: function(f), ctx });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_async_and_wait(queue: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    run_and_wait(queue, block_work(unsafe { &*block }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_barrier_async_and_wait_f(
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    // A barrier that runs on the queue and is waited for: the gate item
    // makes it wait for everything queued before.
    sync(queue, ctx, function(f), true);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_barrier_async_and_wait(queue: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    let work = block_work(unsafe { &*block });
    sync(queue, work.ctx, work.f, true);
}

/// Run `f(ctx, i)` for every `i` below `iterations`, spread over the pool,
/// and return when all have run. A serial queue runs them in order.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_apply_f(
    iterations: usize,
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C-unwind" fn(*mut c_void, usize),
) {
    // SAFETY: the caller passes a queue or null (automatic).
    let serial = !queue.is_null() && matches!(unsafe { queue_of(queue) }.kind, QueueKind::Serial | QueueKind::Main);
    if serial || iterations < 2 {
        super::callout(|| {
            for i in 0..iterations {
                // SAFETY: the caller's function and context.
                unsafe { f(ctx, i) };
            }
        });
        return;
    }
    struct Apply {
        next: AtomicUsize,
        left: AtomicUsize,
        iterations: usize,
        f: unsafe extern "C-unwind" fn(*mut c_void, usize),
        ctx: *mut c_void,
        gate: Gate,
    }
    // SAFETY: libdispatch's contract has the function called from any
    // thread with its context; the rest is atomics and a gate.
    unsafe impl Send for Apply {}
    unsafe impl Sync for Apply {}
    impl Apply {
        fn work(&self) {
            loop {
                let i = self.next.fetch_add(1, Ordering::Relaxed);
                if i >= self.iterations {
                    return;
                }
                // SAFETY: the caller's function and context, alive until
                // every iteration has run.
                unsafe { (self.f)(self.ctx, i) };
                if self.left.fetch_sub(1, Ordering::AcqRel) == 1 {
                    self.gate.set(DONE);
                }
            }
        }
    }
    unsafe extern "C-unwind" fn helper(ctx: *mut c_void) {
        // SAFETY: each helper owns one reference, made below. A helper that
        // starts after the last iteration finds nothing to do and doesn't
        // touch the caller's function.
        let apply = unsafe { Arc::from_raw(ctx.cast_const().cast::<Apply>()) };
        apply.work();
    }
    let apply = Arc::new(Apply {
        next: AtomicUsize::new(0),
        left: AtomicUsize::new(iterations),
        iterations,
        f,
        ctx,
        gate: Gate::default(),
    });
    let helpers = std::thread::available_parallelism().map_or(4, |n| n.get()).min(iterations) - 1;
    let target = if queue.is_null() { global(Priority::Default) } else { queue };
    for _ in 0..helpers {
        let ctx = Arc::into_raw(apply.clone()).cast_mut().cast();
        enqueue(target, Work { f: helper, ctx }, false);
    }
    super::callout(|| apply.work());
    apply.gate.wait_for(DONE);
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_apply(
    iterations: usize,
    queue: *mut c_void,
    block: *const DynBlock<dyn Fn(usize)>,
) {
    unsafe extern "C-unwind" fn call(ctx: *mut c_void, i: usize) {
        // SAFETY: the caller's block, alive for the whole apply.
        unsafe { &*ctx.cast::<DynBlock<dyn Fn(usize)>>() }.call((i,));
    }
    // SAFETY: forwarded contract.
    unsafe { dispatch_apply_f(iterations, queue, block.cast_mut().cast(), call) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_set_specific(
    queue: *mut c_void,
    key: *const c_void,
    context: *mut c_void,
    destructor: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    // SAFETY: a live queue.
    let q = unsafe { queue_of(queue) };
    let old = {
        let mut specifics = lock(&q.specifics);
        let old = specifics.iter().position(|s| s.key == key as usize).map(|at| specifics.remove(at));
        if !context.is_null() {
            specifics.push(Specific { key: key as usize, context, destructor: destructor.map(function) });
        }
        old
    };
    if let Some(Specific { context, destructor: Some(destructor), .. }) = old {
        // SAFETY: the destructor takes the context it was given with.
        unsafe { destructor(context) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_queue_get_specific(queue: *mut c_void, key: *const c_void) -> *mut c_void {
    // SAFETY: a live queue.
    let q = unsafe { queue_of(queue) };
    lock(&q.specifics).iter().find(|s| s.key == key as usize).map_or(std::ptr::null_mut(), |s| s.context)
}

/// The value for `key` on the current queue or, failing that, the queues
/// it targets.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_get_specific(key: *const c_void) -> *mut c_void {
    let mut queue = Obj::retain(current());
    loop {
        // SAFETY: a live queue.
        let value = unsafe { dispatch_queue_get_specific(queue.ptr(), key) };
        if !value.is_null() {
            return value;
        }
        // SAFETY: a live queue.
        let next = Obj::retain(lock(&unsafe { queue_of(queue.ptr()) }.target).ptr());
        if next.ptr().is_null() {
            return std::ptr::null_mut();
        }
        queue = next;
    }
}

fn targets(from: *mut c_void, queue: *const c_void) -> bool {
    let mut at = Obj::retain(from);
    loop {
        if std::ptr::eq(at.ptr(), queue) {
            return true;
        }
        // SAFETY: a live queue.
        let next = Obj::retain(lock(&unsafe { queue_of(at.ptr()) }.target).ptr());
        if next.ptr().is_null() {
            return false;
        }
        at = next;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_assert_queue(queue: *const c_void) {
    assert!(targets(current(), queue), "sidestep: dispatch_assert_queue: running on another queue");
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_assert_queue_barrier(queue: *const c_void) {
    // SAFETY: forwarded contract.
    unsafe { dispatch_assert_queue(queue) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_assert_queue_not(queue: *const c_void) {
    assert!(!targets(current(), queue), "sidestep: dispatch_assert_queue_not: running on that queue");
}

/// Suspend a queue (or source); work already running goes on.
pub(crate) fn suspend(queue: *mut c_void) {
    // SAFETY: a live queue.
    let q = unsafe { queue_of(queue) };
    if matches!(q.kind, QueueKind::Serial | QueueKind::Concurrent) {
        q.suspended.fetch_add(1, Ordering::AcqRel);
    }
}

pub(crate) fn resume(queue: *mut c_void) {
    // SAFETY: a live queue.
    let q = unsafe { queue_of(queue) };
    if !matches!(q.kind, QueueKind::Serial | QueueKind::Concurrent) {
        return;
    }
    // macOS crashes here too: only dispatch_activate starts such a queue.
    assert!(
        !(q.inactive.load(Ordering::Acquire) && q.suspended.load(Ordering::Acquire) <= 1),
        "sidestep: dispatch_resume: the queue is inactive (dispatch_activate starts it)"
    );
    let was = q.suspended.fetch_sub(1, Ordering::AcqRel);
    assert!(was > 0, "sidestep: dispatch_resume: over-resumed queue");
    if was == 1 {
        let mut state = lock(&q.state);
        match q.kind {
            QueueKind::Serial => {
                if !state.scheduled && !state.items.is_empty() {
                    state.scheduled = true;
                    drop(state);
                    schedule_drain(queue, q);
                }
            }
            _ => pump(queue, q, &mut state),
        }
    }
}

/// `dispatch_activate` on an object: starts a queue made inactive, once.
/// Other queues and other objects are always active.
pub(crate) fn activate(object: *mut c_void) {
    // SAFETY: callers pass live dispatch objects.
    if let Kind::Queue(q) = unsafe { &body(object).kind }
        && q.inactive.swap(false, Ordering::AcqRel)
    {
        resume(object);
    }
}

pub(crate) fn set_target(queue: *mut c_void, target: *mut c_void) {
    // SAFETY: a live queue.
    let q = unsafe { queue_of(queue) };
    if matches!(q.kind, QueueKind::Serial | QueueKind::Concurrent) {
        let old = std::mem::replace(&mut *lock(&q.target), Obj::retain(target));
        drop(old);
    }
}
