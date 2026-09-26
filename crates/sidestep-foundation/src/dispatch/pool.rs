//! The worker threads behind the global queues.
//!
//! One pool serves all four priorities, highest first. It starts as many
//! workers as the machine has cores, lazily, as work arrives. Work that
//! blocks (a `dispatch_sync`, a semaphore wait, a group wait) could
//! otherwise starve the pool, so a monitor thread adds a worker whenever
//! queued work has made no progress for 50 ms, up to a cap, and extra
//! workers leave after 5 s without work. Idle, the pool and its monitor
//! sleep without timeouts: no wake-ups.

use std::collections::VecDeque;
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::runloop::core::Work;
use crate::thread::lock;

/// Priorities, highest first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Priority {
    High = 0,
    Default = 1,
    Low = 2,
    Background = 3,
}

const STALL: Duration = Duration::from_millis(50);
const RETIRE: Duration = Duration::from_secs(5);

struct State {
    queues: [VecDeque<Work>; 4],
    workers: usize,
    idle: usize,
    /// Jobs taken by workers so far, for the monitor to see progress.
    started: u64,
    /// Whether the monitor thread exists, and whether it sleeps until
    /// there is work.
    monitor: bool,
    monitor_asleep: bool,
}

struct Pool {
    state: Mutex<State>,
    work: Condvar,
    monitor: Condvar,
    /// Workers started on demand.
    base: usize,
    /// Most workers the monitor may add up to.
    cap: usize,
}

static POOL: LazyLock<Pool> = LazyLock::new(|| {
    let base = std::thread::available_parallelism().map_or(4, |n| n.get());
    Pool {
        state: Mutex::new(State {
            queues: Default::default(),
            workers: 0,
            idle: 0,
            started: 0,
            monitor: false,
            monitor_asleep: false,
        }),
        work: Condvar::new(),
        monitor: Condvar::new(),
        base,
        cap: 64 + 4 * base,
    }
});

/// Queue work on the pool.
pub(crate) fn submit(priority: Priority, work: Work) {
    let pool = &*POOL;
    let mut state = lock(&pool.state);
    state.queues[priority as usize].push_back(work);
    if state.idle > 0 {
        pool.work.notify_one();
    } else if state.workers < pool.base {
        state.workers += 1;
        spawn_worker(false);
    }
    if !state.monitor {
        state.monitor = true;
        std::thread::Builder::new()
            .name("dispatch-monitor".into())
            .spawn(monitor)
            .expect("sidestep: couldn't start a thread");
    } else if state.monitor_asleep {
        pool.monitor.notify_one();
    }
}

fn spawn_worker(extra: bool) {
    std::thread::Builder::new()
        .name("dispatch-worker".into())
        .spawn(move || worker(extra))
        .expect("sidestep: couldn't start a thread");
}

fn next(state: &mut State) -> Option<Work> {
    state.queues.iter_mut().find_map(|q| q.pop_front())
}

fn worker(extra: bool) {
    let pool = &*POOL;
    // A worker that dies panicking gives its place back.
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            lock(&POOL.state).workers -= 1;
        }
    }
    let _leave = Leave;
    let mut state = lock(&pool.state);
    loop {
        if let Some(work) = next(&mut state) {
            state.started += 1;
            drop(state);
            // SAFETY: queued work pairs a function with its own context.
            objc2::rc::autoreleasepool(|_| unsafe { work.run() });
            state = lock(&pool.state);
            continue;
        }
        state.idle += 1;
        let (guard, timeout) = if extra {
            let (guard, result) = pool.work.wait_timeout(state, RETIRE).unwrap_or_else(|e| e.into_inner());
            (guard, result.timed_out())
        } else {
            (pool.work.wait(state).unwrap_or_else(|e| e.into_inner()), false)
        };
        state = guard;
        state.idle -= 1;
        if timeout && state.queues.iter().all(|q| q.is_empty()) {
            return;
        }
    }
}

/// Add a worker whenever queued work sits unstarted for a while.
fn monitor() {
    let pool = &*POOL;
    let mut state = lock(&pool.state);
    loop {
        while state.queues.iter().all(|q| q.is_empty()) {
            state.monitor_asleep = true;
            state = pool.monitor.wait(state).unwrap_or_else(|e| e.into_inner());
            state.monitor_asleep = false;
        }
        let (started, since) = (state.started, Instant::now());
        while since.elapsed() < STALL {
            let left = STALL.saturating_sub(since.elapsed());
            state = pool.monitor.wait_timeout(state, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        let waiting = state.queues.iter().any(|q| !q.is_empty());
        if waiting && state.started == started && state.idle == 0 && state.workers < pool.cap {
            state.workers += 1;
            spawn_worker(true);
        }
    }
}
