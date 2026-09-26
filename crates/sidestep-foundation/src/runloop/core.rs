//! One run loop per thread: what other threads may touch ([`Shared`]) and
//! what only the loop's own thread touches ([`State`], thread-local).
//!
//! Other threads reach a loop only through its inbox (blocks to perform and
//! operations to apply on the owner thread), its stop flag and its wake
//! cell. An `NSThread`'s loop is made when the thread starts, on the
//! starting thread, so work can be handed to it before it runs; the new
//! thread adopts it. Everything else, the timer heap, observers, sources, the queue of
//! blocks and the stack of running modes, belongs to the owner thread and
//! sits in a `RefCell`. No borrow of it is ever held across a callout:
//! work is copied out into scratch vectors first, because any callout may
//! run the loop again (a modal alert, menu tracking, app code calling
//! `-runMode:beforeDate:`). Objects the state lets go of are dropped only
//! after the borrow ends, since releasing one can run arbitrary code.
//!
//! One pass of [`run`] follows the sequence Apple documents for run loops
//! and that `CFRunLoopRunInMode` shows on macOS (see
//! `conformance/tests/runloop.rs`): Entry; then repeatedly BeforeTimers,
//! BeforeSources, blocks, signalled sources, then, unless a source was
//! handled or the call only polls, BeforeWaiting, sleep until the next
//! timer in the mode, the limit or a wake-up, AfterWaiting; due timers, the
//! main dispatch queue, blocks; until a source was handled and the caller
//! asked to return then, the limit passed, the loop was stopped or the mode
//! ran out of timers, sources and blocks; then Exit.
//!
//! A loop ends with its thread: [`teardown`] closes its inbox (work handed
//! to it later is dropped, so a caller waiting for it is told) and drops
//! its state while the thread can still run code, since releasing a timer's
//! target or a request's argument may use a run loop again. Past that point
//! (inside the thread's own thread-local destructors) such code gets a
//! throwaway loop rather than aborting the process.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use objc2::DefinedClass;
use objc2::rc::{Retained, autoreleasepool};

use super::modes::{Mode, ModeSet, Registration};
use super::observers::{ObserverEntry, ObserverTarget};
use super::timers::{TimerEntry, TimerKey};
use super::wake::Wake;
use super::{Activity, RunResult};
use crate::thread::{current_tid, is_main_thread, lock};
use crate::timer::NSTimerImpl;

/// Mode id meaning "not running", in [`Shared::mode`].
const NOT_RUNNING: u32 = u32::MAX;

/// The part of a run loop any thread may use.
pub(crate) struct Shared {
    /// Kernel thread id of the loop's thread; 0 until a thread adopts a
    /// loop made for it in advance.
    tid: AtomicI32,
    main: bool,
    pub(crate) wake: Wake,
    inbox: Mutex<Vec<Remote>>,
    inbox_ready: AtomicBool,
    /// The loop's thread has ended: work handed to it is dropped. Changed
    /// under the inbox lock.
    closed: AtomicBool,
    /// True while the loop sleeps (`CFRunLoopIsWaiting`).
    pub(crate) waiting: AtomicBool,
    /// The innermost running mode, for other threads' questions.
    mode: AtomicU32,
    /// A stop request for the innermost run.
    stop: AtomicBool,
    /// The loop's common modes. The owner keeps a copy in its state; this
    /// one lets other threads register items in the common modes.
    pub(crate) common: Mutex<ModeSet>,
}

/// Work other threads leave for a loop.
enum Remote {
    Block(BlockModes, Box<dyn FnOnce() + Send>),
    Op(Box<dyn FnOnce() + Send>),
}

impl Shared {
    fn new(tid: i32, main: bool) -> Shared {
        Shared {
            tid: AtomicI32::new(tid),
            main,
            wake: Wake::default(),
            inbox: Mutex::new(Vec::new()),
            inbox_ready: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            waiting: AtomicBool::new(false),
            mode: AtomicU32::new(NOT_RUNNING),
            stop: AtomicBool::new(false),
            common: Mutex::new(ModeSet::of(Mode::DEFAULT)),
        }
    }

    /// A loop for a thread that hasn't started yet, which [`adopt`]s it.
    pub(crate) fn for_new_thread() -> Arc<Shared> {
        Arc::new(Shared::new(0, false))
    }

    pub(crate) fn is_current(&self) -> bool {
        current_tid() == self.tid.load(Ordering::Acquire)
    }

    pub(crate) fn is_main(&self) -> bool {
        self.main
    }

    /// The innermost running mode, from any thread.
    pub(crate) fn running_mode(&self) -> Option<Mode> {
        match self.mode.load(Ordering::Acquire) {
            NOT_RUNNING => None,
            id => Some(Mode(id)),
        }
    }

    /// Leave `item` for the loop; a loop whose thread has ended drops it
    /// (outside the lock: dropping work can run any code).
    fn push(&self, item: Remote) {
        let mut inbox = lock(&self.inbox);
        if self.closed.load(Ordering::Relaxed) {
            drop(inbox);
            drop(item);
            return;
        }
        inbox.push(item);
        self.inbox_ready.store(true, Ordering::Release);
    }

    /// Refuse work from now on and hand back what is waiting.
    fn close(&self) -> Vec<Remote> {
        let mut inbox = lock(&self.inbox);
        self.closed.store(true, Ordering::Relaxed);
        self.inbox_ready.store(false, Ordering::Relaxed);
        std::mem::take(&mut *inbox)
    }

    /// Stop the innermost run. A loop that isn't running ignores it, as
    /// on macOS, where each run starts with a fresh stop flag.
    pub(crate) fn stop(&self) {
        if self.mode.load(Ordering::Acquire) != NOT_RUNNING {
            self.stop.store(true, Ordering::Release);
            self.wake.wake();
        }
    }
}

static MAIN: OnceLock<Arc<Shared>> = OnceLock::new();

/// The main thread's loop, from any thread.
pub(crate) fn main_shared() -> &'static Arc<Shared> {
    MAIN.get_or_init(|| {
        // SAFETY: getpid has no preconditions.
        let pid = unsafe { libc::getpid() };
        Arc::new(Shared::new(pid, true))
    })
}

/// Run `op` on the loop's thread: now if that is this thread, otherwise
/// the next time the loop looks at its inbox, which this wakes it for.
pub(crate) fn on_owner(shared: &Arc<Shared>, op: impl FnOnce() + Send + 'static) {
    if shared.is_current() {
        op();
    } else {
        shared.push(Remote::Op(Box::new(op)));
        shared.wake.wake();
    }
}

/// Which modes a block may run in. Unlike timers, blocks for the common
/// modes follow the common set as it is when they run.
pub(crate) struct BlockModes {
    pub(crate) modes: ModeSet,
    pub(crate) common: bool,
}

impl BlockModes {
    pub(crate) fn new() -> BlockModes {
        BlockModes { modes: ModeSet::default(), common: false }
    }

    pub(crate) fn add(&mut self, mode: Mode) {
        if mode == Mode::COMMON {
            self.common = true;
        } else {
            self.modes.insert(mode);
        }
    }

    /// The modes named, the common pseudo-mode left out.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Mode> + '_ {
        self.modes.iter()
    }

    fn matches(&self, mode: Mode, common_set: &ModeSet) -> bool {
        self.modes.contains(mode) || (self.common && common_set.contains(mode))
    }
}

pub(crate) struct Block {
    pub(crate) modes: BlockModes,
    pub(crate) work: Box<dyn FnOnce()>,
}

/// Queue a block. `wake` decides whether a sleeping loop notices it now:
/// `CFRunLoopPerformBlock` and `-performBlock:` don't wake it, as on macOS;
/// Sidestep's own handoffs do.
pub(crate) fn enqueue(shared: &Arc<Shared>, modes: BlockModes, work: Box<dyn FnOnce() + Send>, wake: bool) {
    if shared.is_current() {
        drain_inbox(shared);
        with_state(|s| s.blocks.push_back(Block { modes, work }));
    } else {
        shared.push(Remote::Block(modes, work));
    }
    if wake {
        shared.wake.wake();
    }
}

/// Run `work` once on `shared`'s thread, the next time it runs in one of
/// `modes`, from the loop's perform source: performing it ends a run that
/// asked to return after a source, which is how
/// `-performSelectorOnMainThread:…` behaves on macOS. Requests run in the
/// order they were made, every waiting one for the running mode in one
/// go; one made meanwhile waits for the next time the source is looked at.
pub(crate) fn perform_as_source(shared: &Arc<Shared>, modes: Vec<Mode>, work: Box<dyn FnOnce() + Send>) {
    on_owner(shared, move || with_state(|s| s.queue_perform(&modes, work)));
    shared.wake.wake();
}

/// A request waiting for the loop's perform source.
pub(crate) struct PerformRequest {
    reg: Registration,
    work: Box<dyn FnOnce()>,
}

/// The perform source's callout: run the requests waiting for the running
/// mode. If one of them unwinds, those after it go back in the queue.
fn run_performs() {
    struct Requeue(VecDeque<PerformRequest>);
    impl Drop for Requeue {
        fn drop(&mut self) {
            if !self.0.is_empty() {
                let rest = std::mem::take(&mut self.0);
                with_state(|s| s.requeue_performs(rest));
            }
        }
    }
    let mut batch = Requeue(with_state(State::take_performs));
    while let Some(request) = batch.0.pop_front() {
        (request.work)();
    }
}

/// A version-0 source's flags, shared with [`super::SourceSignal`].
pub(crate) struct SourceFlag {
    pub(crate) signalled: AtomicBool,
    pub(crate) valid: AtomicBool,
}

pub(crate) struct SourceEntry {
    pub(crate) order: isize,
    pub(crate) seq: u64,
    pub(crate) reg: Registration,
    pub(crate) flag: Arc<SourceFlag>,
    pub(crate) perform: Rc<dyn Fn()>,
}

/// A signalled source about to be performed.
type SourceCallout = (Arc<SourceFlag>, Rc<dyn Fn()>);

/// Something the state has let go of, dropped once the borrow has ended.
pub(crate) enum Dead {
    Timer(#[allow(dead_code)] Retained<NSTimerImpl>),
    Observer(#[allow(dead_code)] ObserverTarget),
    Source(#[allow(dead_code)] Rc<dyn Fn()>),
}

/// The owner thread's side of a loop.
pub(crate) struct State {
    /// `performSelector…` requests, oldest first, for the perform source.
    performs: VecDeque<PerformRequest>,
    /// The perform source's flag, once the loop has made the source.
    perform_source: Option<Arc<SourceFlag>>,
    pub(crate) shared: Arc<Shared>,
    /// The owner's copy of the common set.
    pub(crate) common: ModeSet,
    /// Every mode something was registered in (`CFRunLoopCopyAllModes`).
    pub(crate) known: ModeSet,
    pub(crate) timers: BTreeMap<TimerKey, TimerEntry>,
    /// Sorted by (order, seq).
    pub(crate) observers: Vec<ObserverEntry>,
    /// Sorted by (order, seq).
    pub(crate) sources: Vec<SourceEntry>,
    blocks: VecDeque<Block>,
    /// Running modes, innermost last.
    stack: Vec<Mode>,
    seq: u64,
    /// Inside a main-queue callout, which the main queue doesn't re-enter.
    in_main_queue: bool,
    pub(crate) dead: Vec<Dead>,
    pub(crate) scratch_timers: Vec<(TimerKey, Retained<NSTimerImpl>)>,
    pub(crate) scratch_observers: Vec<ObserverTarget>,
    scratch_sources: Vec<SourceCallout>,
    /// The loop's Objective-C objects, made on first use.
    pub(crate) objects: Option<super::nsrunloop::Objects>,
}

impl State {
    fn new() -> State {
        let shared = if is_main_thread() { main_shared().clone() } else { Arc::new(Shared::new(current_tid(), false)) };
        State::with_shared(shared)
    }

    fn with_shared(shared: Arc<Shared>) -> State {
        State {
            performs: VecDeque::new(),
            perform_source: None,
            shared,
            common: ModeSet::of(Mode::DEFAULT),
            known: ModeSet::of(Mode::DEFAULT),
            timers: BTreeMap::new(),
            observers: Vec::new(),
            sources: Vec::new(),
            blocks: VecDeque::new(),
            stack: Vec::new(),
            seq: 0,
            in_main_queue: false,
            dead: Vec::new(),
            scratch_timers: Vec::new(),
            scratch_observers: Vec::new(),
            scratch_sources: Vec::new(),
            objects: None,
        }
    }

    pub(crate) fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Whether the main dispatch queue drains in `mode` right now.
    fn main_queue_runs_in(&self, mode: Mode) -> bool {
        self.shared.main && !self.in_main_queue && self.common.contains(mode)
    }

    /// Whether `mode` has nothing that could make a run in it end on its
    /// own: no timers, sources or blocks. Observers don't count. On the
    /// main thread the main dispatch queue counts as a source of the common
    /// modes, as on macOS.
    pub(crate) fn is_empty(&self, mode: Mode) -> bool {
        if mode == Mode::COMMON {
            return true;
        }
        if self.main_queue_runs_in(mode) {
            return false;
        }
        !(self.timers.values().any(|e| e.modes.contains(mode))
            || self.sources.iter().any(|s| s.flag.valid.load(Ordering::Relaxed) && s.reg.modes.contains(mode))
            || self.blocks.iter().any(|b| b.modes.matches(mode, &self.common)))
    }

    pub(crate) fn depth(&self) -> usize {
        self.stack.len()
    }

    pub(crate) fn current_mode(&self) -> Option<Mode> {
        self.stack.last().copied()
    }

    /// Add `mode` to the common set and every common item to it.
    pub(crate) fn add_common_mode(&mut self, mode: Mode) {
        if mode == Mode::COMMON || self.common.contains(mode) {
            return;
        }
        self.common.insert(mode);
        self.known.insert(mode);
        for entry in self.timers.values_mut() {
            let mut sched = lock(&entry.timer.ivars().sched);
            if sched.reg.common {
                sched.reg.modes.insert(mode);
                entry.modes.insert(mode);
            }
        }
        for entry in &mut self.observers {
            if entry.reg.common {
                entry.reg.modes.insert(mode);
                entry.mirror_registration();
            }
        }
        for source in &mut self.sources {
            if source.reg.common {
                source.reg.modes.insert(mode);
            }
        }
        for request in &mut self.performs {
            if request.reg.common {
                request.reg.modes.insert(mode);
            }
        }
    }

    /// Queue a perform request, making the perform source on first use,
    /// and signal the source.
    fn queue_perform(&mut self, modes: &[Mode], work: Box<dyn FnOnce()>) {
        let mut reg = Registration::default();
        for &mode in modes {
            reg.add(mode, &self.common);
        }
        self.known.extend(&reg.modes);
        let flag = match &self.perform_source {
            Some(flag) => flag.clone(),
            None => {
                let flag = Arc::new(SourceFlag { signalled: AtomicBool::new(false), valid: AtomicBool::new(true) });
                let seq = self.next_seq();
                let at = self.sources.partition_point(|e| (e.order, e.seq) <= (0, seq));
                let entry = SourceEntry {
                    order: 0,
                    seq,
                    reg: Registration::default(),
                    flag: flag.clone(),
                    perform: Rc::new(run_performs),
                };
                self.sources.insert(at, entry);
                self.perform_source = Some(flag.clone());
                flag
            }
        };
        if let Some(source) = self.sources.iter_mut().find(|e| Arc::ptr_eq(&e.flag, &flag)) {
            source.reg.modes.extend(&reg.modes);
            source.reg.common |= reg.common;
        }
        self.performs.push_back(PerformRequest { reg, work });
        flag.signalled.store(true, Ordering::Release);
    }

    /// Take the requests for the running mode out of the queue. The perform
    /// source stays in the modes of those left, signalled.
    fn take_performs(&mut self) -> VecDeque<PerformRequest> {
        let Some(mode) = self.current_mode() else { return VecDeque::new() };
        if self.performs.iter().all(|p| p.reg.modes.contains(mode)) {
            let batch = std::mem::take(&mut self.performs);
            self.narrow_perform_source();
            return batch;
        }
        let (batch, rest) = std::mem::take(&mut self.performs).into_iter().partition(|p| p.reg.modes.contains(mode));
        self.performs = rest;
        self.narrow_perform_source();
        batch
    }

    /// Put requests taken for running back at the front of the queue.
    fn requeue_performs(&mut self, mut requests: VecDeque<PerformRequest>) {
        requests.append(&mut self.performs);
        self.performs = requests;
        self.narrow_perform_source();
    }

    /// Make the perform source's modes those of the waiting requests, and
    /// keep it signalled while any wait.
    fn narrow_perform_source(&mut self) {
        let Some(flag) = self.perform_source.clone() else { return };
        let mut reg = Registration::default();
        for request in &self.performs {
            reg.modes.extend(&request.reg.modes);
            reg.common |= request.reg.common;
        }
        if let Some(source) = self.sources.iter_mut().find(|e| Arc::ptr_eq(&e.flag, &flag)) {
            source.reg = reg;
        }
        if !self.performs.is_empty() {
            flag.signalled.store(true, Ordering::Release);
        }
    }

    /// The earliest time a timer in `mode` wants to fire, leaving out those
    /// firing right now (a nested run inside a timer's callout must not
    /// spin on it).
    pub(crate) fn next_due(&self, mode: Mode) -> Option<(TimerKey, &TimerEntry)> {
        self.timers.iter().find(|(_, e)| e.modes.contains(mode) && !e.timer.is_firing()).map(|(k, e)| (*k, e))
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Ends the loop of a thread Sidestep didn't start, as the thread
    /// exits: its destructor runs before `STATE`'s, since it is first used
    /// after it.
    static TEARDOWN: Teardown = const { Teardown };
}

struct Teardown;

impl Drop for Teardown {
    fn drop(&mut self) {
        teardown();
    }
}

/// Borrow this thread's loop state, creating it on first use. Whatever the
/// state let go of is dropped after the borrow ends.
///
/// Inside the thread's thread-local destructors, once the state itself is
/// being dropped, `f` gets a throwaway state instead: releasing what a loop
/// held can run code that asks for the loop again.
pub(crate) fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut f = Some(f);
    let attempt = STATE.try_with(|cell| {
        let mut slot = cell.borrow_mut();
        let made = slot.is_none();
        let state = slot.get_or_insert_with(State::new);
        let result = (f.take().expect("called once"))(state);
        (result, std::mem::take(&mut state.dead), made)
    });
    match attempt {
        Ok((result, dead, made)) => {
            drop(dead);
            if made && !is_main_thread() {
                // Register the teardown now, after `STATE`'s destructor.
                let _ = TEARDOWN.try_with(|_| {});
            }
            result
        }
        Err(_) => {
            let mut state = State::new();
            let result = (f.take().expect("called once"))(&mut state);
            drop(state);
            result
        }
    }
}

/// Make `shared`, made for this thread before it started, the thread's
/// loop. The first thing an `NSThread` does.
pub(crate) fn adopt(shared: Arc<Shared>) {
    shared.tid.store(current_tid(), Ordering::Release);
    STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(State::with_shared(shared));
        }
    });
    let _ = TEARDOWN.try_with(|_| {});
}

/// End this thread's loop: close its inbox, then drop what was waiting in
/// it and the loop's state (timers, sources, observers, blocks and perform
/// requests), outside any borrow. A request someone waits for is dropped
/// unperformed, which tells the waiter. Code that uses the run loop
/// afterwards on this thread gets a new one. The main loop is never ended.
pub(crate) fn teardown() {
    if is_main_thread() {
        return;
    }
    let Ok(Some(state)) = STATE.try_with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.take())) else {
        return;
    };
    let waiting = state.shared.close();
    drop(waiting);
    drop(state);
}

/// This thread's loop.
pub(crate) fn current_shared() -> Arc<Shared> {
    with_state(|s| s.shared.clone())
}

/// Move what other threads left into the state; run their operations.
pub(crate) fn drain_inbox(shared: &Shared) {
    if !shared.inbox_ready.swap(false, Ordering::Acquire) {
        return;
    }
    let items = std::mem::take(&mut *lock(&shared.inbox));
    for item in items {
        match item {
            Remote::Block(modes, work) => with_state(|s| s.blocks.push_back(Block { modes, work })),
            Remote::Op(op) => op(),
        }
    }
}

// ---------------------------------------------------------------------------
// The main dispatch queue: work other code wants run on the main thread,
// drained by the main loop in its common modes.

/// A function and its argument, as libdispatch passes work around.
pub(crate) struct Work {
    pub(crate) f: unsafe extern "C-unwind" fn(*mut c_void),
    pub(crate) ctx: *mut c_void,
}

// SAFETY: whoever makes a `Work` hands its context over to the thread that
// runs it, as libdispatch's contract requires.
unsafe impl Send for Work {}

impl Work {
    /// A Rust closure as a function and context pair.
    pub(crate) fn boxed<F: FnOnce() + Send + 'static>(f: F) -> Work {
        unsafe extern "C-unwind" fn call<F: FnOnce()>(ctx: *mut c_void) {
            // SAFETY: `ctx` came from Box::into_raw below and runs once.
            let f = unsafe { Box::from_raw(ctx.cast::<F>()) };
            f();
        }
        let ctx = Box::into_raw(Box::new(f)).cast::<c_void>();
        Work { f: call::<F>, ctx }
    }

    /// Run the work. It is a dispatch client callout: unwinding out of it
    /// aborts (see `dispatch::callout`).
    ///
    /// # Safety
    /// The function must accept the context, which is used up.
    pub(crate) unsafe fn run(self) {
        // SAFETY: guaranteed by the caller.
        crate::dispatch::callout(|| unsafe { (self.f)(self.ctx) })
    }
}

struct MainQueue {
    items: Mutex<VecDeque<Work>>,
    ready: AtomicBool,
}

static MAIN_QUEUE: MainQueue = MainQueue { items: Mutex::new(VecDeque::new()), ready: AtomicBool::new(false) };

/// Queue work for the main thread and wake its loop.
pub(crate) fn main_queue_push(work: Work) {
    lock(&MAIN_QUEUE.items).push_back(work);
    MAIN_QUEUE.ready.store(true, Ordering::Release);
    main_shared().wake.wake();
}

fn main_queue_pending() -> bool {
    MAIN_QUEUE.ready.load(Ordering::Acquire)
}

/// Run what is on the main queue now; work queued meanwhile waits for the
/// next pass. Returns whether anything ran.
fn drain_main_queue() -> bool {
    if !MAIN_QUEUE.ready.swap(false, Ordering::Acquire) {
        return false;
    }
    let batch = std::mem::take(&mut *lock(&MAIN_QUEUE.items));
    if batch.is_empty() {
        return false;
    }
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            with_state(|s| s.in_main_queue = false);
        }
    }
    with_state(|s| s.in_main_queue = true);
    let _leave = Leave;
    for work in batch {
        // SAFETY: queued work pairs a function with its own context.
        autoreleasepool(|_| unsafe { work.run() });
    }
    true
}

// ---------------------------------------------------------------------------
// Running.

/// The frame of one run: the mode it pushed and the stop request of the
/// run it interrupts, restored when it ends, even by unwinding.
struct Frame {
    shared: Arc<Shared>,
    outer_stop: bool,
}

impl Frame {
    fn enter(shared: &Arc<Shared>, mode: Mode) -> Frame {
        let depth = with_state(|s| {
            s.stack.push(mode);
            s.known.insert(mode);
            s.stack.len()
        });
        shared.mode.store(mode.0, Ordering::Release);
        // Each run starts with no stop request; a request made while no run
        // was active is dropped.
        let outer_stop = shared.stop.swap(false, Ordering::AcqRel) && depth > 1;
        Frame { shared: shared.clone(), outer_stop }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let outer = with_state(|s| {
            s.stack.pop();
            s.current_mode()
        });
        self.shared.mode.store(outer.map_or(NOT_RUNNING, |m| m.0), Ordering::Release);
        self.shared.stop.store(self.outer_stop, Ordering::Release);
    }
}

/// Run this thread's loop in `mode` until `limit` (`None`: no limit; a
/// limit already past: one pass without sleeping). Returns `None` without
/// running if the mode has nothing in it.
pub(crate) fn run(mode: Mode, limit: Option<Instant>, return_after_source: bool) -> Option<RunResult> {
    let shared = current_shared();
    drain_inbox(&shared);
    if with_state(|s| s.is_empty(mode)) {
        return None;
    }
    let poll = limit.is_some_and(|l| l <= Instant::now());
    let frame = Frame::enter(&shared, mode);
    notify(Activity::ENTRY, mode);
    let result = if shared.stop.swap(false, Ordering::AcqRel) {
        RunResult::Stopped
    } else {
        loop {
            let pass = autoreleasepool(|_| pass(&shared, mode, limit, poll, return_after_source));
            if let Some(result) = pass {
                break result;
            }
        }
    };
    notify(Activity::EXIT, mode);
    drop(frame);
    Some(result)
}

/// One iteration; `Some` when the run is over.
fn pass(shared: &Arc<Shared>, mode: Mode, limit: Option<Instant>, poll: bool, return_after: bool) -> Option<RunResult> {
    drain_inbox(shared);
    notify(Activity::BEFORE_TIMERS, mode);
    notify(Activity::BEFORE_SOURCES, mode);
    do_blocks(mode);
    let mut handled = do_sources(mode, return_after);
    if handled {
        do_blocks(mode);
    }
    let main_queue = with_state(|s| s.main_queue_runs_in(mode));
    if !(handled || poll || (main_queue && main_queue_pending())) {
        notify(Activity::BEFORE_WAITING, mode);
        let next_timer = with_state(|s| s.next_due(mode).map(|(key, _)| key.0));
        let deadline = match (next_timer, limit) {
            (Some(t), Some(l)) => Some(t.min(l)),
            (t, l) => t.or(l),
        };
        shared.waiting.store(true, Ordering::Release);
        shared.wake.sleep(deadline);
        shared.waiting.store(false, Ordering::Release);
        notify(Activity::AFTER_WAITING, mode);
        drain_inbox(shared);
    }
    super::timers::fire_due(mode, Instant::now());
    if main_queue && with_state(|s| s.main_queue_runs_in(mode)) && drain_main_queue() {
        handled = true;
    }
    do_blocks(mode);

    if handled && return_after {
        Some(RunResult::HandledSource)
    } else if poll || limit.is_some_and(|l| Instant::now() >= l) {
        Some(RunResult::TimedOut)
    } else if shared.stop.swap(false, Ordering::AcqRel) {
        Some(RunResult::Stopped)
    } else if with_state(|s| s.is_empty(mode)) {
        Some(RunResult::Finished)
    } else {
        None
    }
}

/// Call the observers of `mode` interested in `activity`, lowest order
/// first.
pub(crate) fn notify(activity: Activity, mode: Mode) {
    let list = with_state(|s| {
        if s.observers.is_empty() {
            return None;
        }
        let mut list = std::mem::take(&mut s.scratch_observers);
        list.extend(
            s.observers
                .iter()
                .filter(|e| e.activities & activity.0 != 0 && e.reg.modes.contains(mode))
                .map(|e| e.target.clone()),
        );
        Some(list)
    });
    let Some(mut list) = list else { return };
    for target in &list {
        target.call(activity);
    }
    list.clear();
    with_state(|s| {
        if s.scratch_observers.capacity() == 0 {
            s.scratch_observers = list;
        }
    });
}

/// Run the queued blocks that may run in `mode`, in order; the others stay
/// queued. Blocks queued meanwhile wait for the next call.
fn do_blocks(mode: Mode) {
    let (mut pending, common) = with_state(|s| {
        if s.blocks.is_empty() {
            return (VecDeque::new(), ModeSet::default());
        }
        (std::mem::take(&mut s.blocks), s.common.clone())
    });
    if pending.is_empty() {
        return;
    }
    let mut kept = VecDeque::new();
    while let Some(block) = pending.pop_front() {
        if block.modes.matches(mode, &common) {
            (block.work)();
        } else {
            kept.push_back(block);
        }
    }
    with_state(|s| {
        kept.append(&mut s.blocks);
        s.blocks = kept;
    });
}

/// Perform the signalled sources of `mode`, lowest order first; only the
/// first when `stop_after_one`. Returns whether any was performed.
fn do_sources(mode: Mode, stop_after_one: bool) -> bool {
    let list = with_state(|s| {
        if s.sources.is_empty() {
            return None;
        }
        let mut gone = Vec::new();
        s.sources.retain(|source| {
            let keep = source.flag.valid.load(Ordering::Acquire);
            if !keep {
                gone.push(source.perform.clone());
            }
            keep
        });
        s.dead.extend(gone.into_iter().map(Dead::Source));
        let mut list = std::mem::take(&mut s.scratch_sources);
        list.extend(
            s.sources
                .iter()
                .filter(|src| src.reg.modes.contains(mode) && src.flag.signalled.load(Ordering::Acquire))
                .map(|src| (src.flag.clone(), src.perform.clone())),
        );
        Some(list)
    });
    let Some(mut list) = list else { return false };
    let mut handled = false;
    for (flag, perform) in &list {
        if flag.signalled.swap(false, Ordering::AcqRel) && flag.valid.load(Ordering::Acquire) {
            perform();
            handled = true;
            if stop_after_one {
                break;
            }
        }
    }
    list.clear();
    with_state(|s| {
        if s.scratch_sources.capacity() == 0 {
            s.scratch_sources = list;
        }
    });
    handled
}
