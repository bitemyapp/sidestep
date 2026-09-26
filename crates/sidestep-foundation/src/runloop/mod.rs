//! Run loops: `NSRunLoop`, `CFRunLoop`, and the Rust API the rest of
//! Sidestep drives them with.
//!
//! Every thread has one loop, made on first use. A loop runs in one mode at
//! a time and, while it runs, fires the timers, performs the signalled
//! sources and runs the queued blocks registered in that mode, and calls
//! its observers at each step; when nothing is due it sleeps, making no
//! wake-ups at all while idle. Runs nest: any callout may run the loop
//! again, in the same or another mode. The main thread's loop also drains
//! the main dispatch queue in its common modes.
//!
//! Other threads hand work to a loop with [`RunLoop::perform`] (and the
//! Objective-C and C equivalents built on it), which wakes it. That is the
//! one way into the main thread for Sidestep's own background threads.
//!
//! AppKit's event loop is built on this: an input source signalled by the
//! render thread, the display pass as a before-waiting observer, and
//! `-[NSApplication run]` as [`RunLoop::run_mode`] in the default mode.
//!
//! The module is split by concern: `modes` interns mode names, `wake`
//! sleeps and wakes, `core` holds the loop and one run, `timers` and
//! `observers` their registries, and `nsrunloop` the Objective-C classes.

pub(crate) mod core;
pub(crate) mod modes;
pub(crate) mod nsrunloop;
pub(crate) mod observers;
#[cfg(test)]
mod tests;
pub(crate) mod timers;
mod wake;

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub use modes::Mode;
pub use modes::{NSDefaultRunLoopMode, NSRunLoopCommonModes, kCFRunLoopCommonModes, kCFRunLoopDefaultMode};

use self::core::{BlockModes, Shared, SourceEntry, SourceFlag, with_state};
use self::modes::Registration;

/// How a run ended, with `CFRunLoopRunResult`'s values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum RunResult {
    /// The mode has no timers, sources or blocks.
    Finished = 1,
    /// [`RunLoop::stop`] was called.
    Stopped = 2,
    /// The limit passed.
    TimedOut = 3,
    /// A source was performed and the caller asked to return then.
    HandledSource = 4,
}

/// Points of a run observers can watch, with `CFRunLoopActivity`'s values.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Activity(pub usize);

impl Activity {
    pub const ENTRY: Activity = Activity(1 << 0);
    pub const BEFORE_TIMERS: Activity = Activity(1 << 1);
    pub const BEFORE_SOURCES: Activity = Activity(1 << 2);
    pub const BEFORE_WAITING: Activity = Activity(1 << 5);
    pub const AFTER_WAITING: Activity = Activity(1 << 6);
    pub const EXIT: Activity = Activity(1 << 7);
    pub const ALL: Activity = Activity(0x0FFF_FFFF);
}

impl std::ops::BitOr for Activity {
    type Output = Activity;

    fn bitor(self, other: Activity) -> Activity {
        Activity(self.0 | other.0)
    }
}

/// A thread's run loop. Cheap to clone, and usable from any thread for
/// [`perform`](Self::perform), [`wake`](Self::wake), [`stop`](Self::stop)
/// and the questions; running it and registering observers and sources
/// belong to its own thread.
#[derive(Clone)]
pub struct RunLoop(pub(crate) Arc<Shared>);

/// The main thread's loop, from any thread.
pub fn main() -> RunLoop {
    RunLoop(self::core::main_shared().clone())
}

/// This thread's loop.
pub fn current() -> RunLoop {
    RunLoop(self::core::current_shared())
}

/// An observer registered with [`RunLoop::add_observer`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObserverId(u64);

impl RunLoop {
    /// Whether this is the calling thread's loop.
    pub fn is_current(&self) -> bool {
        self.0.is_current()
    }

    /// Whether this is the main thread's loop.
    pub fn is_main(&self) -> bool {
        self.0.is_main()
    }

    fn assert_current(&self, what: &str) {
        assert!(self.is_current(), "sidestep: {what} belongs to the run loop's own thread");
    }

    /// Run `work` on the loop's thread, the next time it runs in one of
    /// `modes`, and wake it. Blocks run in the order they were queued.
    pub fn perform(&self, modes: &[Mode], work: impl FnOnce() + Send + 'static) {
        let mut set = BlockModes::new();
        for &mode in modes {
            set.add(mode);
        }
        self::core::enqueue(&self.0, set, Box::new(work), true);
    }

    /// Make the loop look for work now if it is sleeping.
    pub fn wake(&self) {
        self.0.wake.wake();
    }

    /// End the innermost run; no effect when the loop isn't running.
    pub fn stop(&self) {
        self.0.stop();
    }

    /// Whether the loop is asleep.
    pub fn is_waiting(&self) -> bool {
        self.0.waiting.load(Ordering::Acquire)
    }

    /// The innermost mode the loop is running in.
    pub fn current_mode(&self) -> Option<Mode> {
        self.0.running_mode()
    }

    /// Add `mode` to the loop's common modes: everything registered in the
    /// common modes joins it.
    pub fn add_common_mode(&self, mode: Mode) {
        if mode == Mode::COMMON || !lock_insert(&self.0, mode) {
            return;
        }
        self::core::on_owner(&self.0, move || with_state(|s| s.add_common_mode(mode)));
    }

    /// Whether `mode` is one of the loop's common modes.
    pub fn is_common_mode(&self, mode: Mode) -> bool {
        mode == Mode::COMMON || crate::thread::lock(&self.0.common).contains(mode)
    }

    /// Run the loop in `mode` until `limit` (`None`: until it finishes or
    /// is stopped; a limit already past: one pass without sleeping). With
    /// `return_after_source`, return after performing a source. A mode
    /// with nothing in it returns [`RunResult::Finished`] at once.
    pub fn run_mode(&self, mode: Mode, limit: Option<Instant>, return_after_source: bool) -> RunResult {
        self.assert_current("running a run loop");
        self::core::run(mode, limit, return_after_source).unwrap_or(RunResult::Finished)
    }

    /// How many runs are active on the loop, innermost included.
    pub fn depth(&self) -> usize {
        self.assert_current("run-loop depth");
        with_state(|s| s.depth())
    }

    /// Call `f` at the given points of every run in `modes`. Observers are
    /// called in ascending `order`, and in registration order among equal
    /// ones; AppKit's display pass has order 2 000 000.
    pub fn add_observer(
        &self,
        modes: &[Mode],
        activities: Activity,
        order: isize,
        f: impl Fn(Activity) + 'static,
    ) -> ObserverId {
        self.assert_current("adding an observer");
        ObserverId(with_state(|s| s.add_closure_observer(modes, activities, order, Rc::new(f))))
    }

    pub fn remove_observer(&self, id: ObserverId) {
        self.assert_current("removing an observer");
        with_state(|s| s.remove_closure_observer(id.0));
    }

    /// Add a source to `modes`: `perform` runs on this thread after the
    /// returned signal is raised, once per raise, lower `order` first.
    /// Performing a source ends a run that asked to return after one.
    pub fn add_source(&self, modes: &[Mode], order: isize, perform: impl Fn() + 'static) -> SourceSignal {
        self.assert_current("adding a source");
        let flag = Arc::new(SourceFlag { signalled: AtomicBool::new(false), valid: AtomicBool::new(true) });
        with_state(|s| {
            let mut reg = Registration::default();
            for &mode in modes {
                reg.add(mode, &s.common);
            }
            s.known.extend(&reg.modes);
            let seq = s.next_seq();
            let entry = SourceEntry { order, seq, reg, flag: flag.clone(), perform: Rc::new(perform) };
            let at = s.sources.partition_point(|e| (e.order, e.seq) <= (order, seq));
            s.sources.insert(at, entry);
        });
        SourceSignal { flag, shared: self.0.clone() }
    }

    /// Add a source made with [`add_source`](Self::add_source) to `mode`
    /// as well, as `CFRunLoopAddSource` with another mode does: one signal
    /// then serves every mode it is in.
    pub fn add_source_mode(&self, signal: &SourceSignal, mode: Mode) {
        self.assert_current("adding a source");
        with_state(|s| {
            let (sources, common) = (&mut s.sources, &s.common);
            if let Some(entry) = sources.iter_mut().find(|e| Arc::ptr_eq(&e.flag, &signal.flag)) {
                entry.reg.add(mode, common);
                let modes = entry.reg.modes.clone();
                s.known.extend(&modes);
            }
        });
    }
}

fn lock_insert(shared: &Shared, mode: Mode) -> bool {
    let mut common = crate::thread::lock(&shared.common);
    if common.contains(mode) {
        return false;
    }
    common.insert(mode);
    true
}

/// Raises a source registered with [`RunLoop::add_source`], from any
/// thread.
#[derive(Clone)]
pub struct SourceSignal {
    flag: Arc<SourceFlag>,
    shared: Arc<Shared>,
}

impl SourceSignal {
    /// Mark the source for performing. A sleeping loop only notices when
    /// something wakes it; see [`signal_and_wake`](Self::signal_and_wake).
    pub fn signal(&self) {
        self.flag.signalled.store(true, Ordering::Release);
    }

    pub fn signal_and_wake(&self) {
        self.signal();
        self.shared.wake.wake();
    }

    /// Remove the source from its loop for good.
    pub fn invalidate(&self) {
        self.flag.valid.store(false, Ordering::Release);
        self.shared.wake.wake();
    }
}
