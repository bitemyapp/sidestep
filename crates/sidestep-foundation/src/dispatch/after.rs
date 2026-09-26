//! `dispatch_after` and timer sources: one thread keeps every pending
//! deadline in a heap and, when one comes due, queues its work on its
//! queue. With nothing pending it sleeps without a timeout.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::c_void;
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::Instant;

use block2::DynBlock;

use super::queue::{Obj, block_work, enqueue, function};
use crate::runloop::core::Work;
use crate::thread::lock;

/// What happens at a deadline.
pub(crate) enum Due {
    /// Queue this work on the queue.
    Work(Obj, Work),
    /// Fire a timer source.
    Source(Obj, u64),
}

struct Entry {
    deadline: Instant,
    seq: u64,
    due: Due,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        (self.deadline, self.seq) == (other.deadline, other.seq)
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.deadline, self.seq).cmp(&(other.deadline, other.seq))
    }
}

struct Timers {
    heap: Mutex<(BinaryHeap<Reverse<Entry>>, u64, bool)>,
    cv: Condvar,
}

static TIMERS: LazyLock<Timers> =
    LazyLock::new(|| Timers { heap: Mutex::new((BinaryHeap::new(), 0, false)), cv: Condvar::new() });

/// Arrange for `due` to happen at `deadline`.
pub(crate) fn at(deadline: Instant, due: Due) {
    let timers = &*TIMERS;
    let mut heap = lock(&timers.heap);
    heap.1 += 1;
    let seq = heap.1;
    let earliest = heap.0.peek().is_none_or(|Reverse(e)| deadline < e.deadline);
    heap.0.push(Reverse(Entry { deadline, seq, due }));
    if !heap.2 {
        heap.2 = true;
        std::thread::Builder::new()
            .name("dispatch-timers".into())
            .spawn(run)
            .expect("sidestep: couldn't start a thread");
    } else if earliest {
        timers.cv.notify_one();
    }
}

fn run() {
    let timers = &*TIMERS;
    let mut heap = lock(&timers.heap);
    loop {
        let now = Instant::now();
        let mut ready = Vec::new();
        while heap.0.peek().is_some_and(|Reverse(e)| e.deadline <= now) {
            ready.push(heap.0.pop().expect("peeked").0);
        }
        if !ready.is_empty() {
            drop(heap);
            for entry in ready {
                match entry.due {
                    Due::Work(queue, work) => enqueue(queue.ptr(), work, false),
                    Due::Source(source, generation) => super::source::timer_fired(source, generation),
                }
            }
            heap = lock(&timers.heap);
            continue;
        }
        heap = match heap.0.peek().map(|Reverse(e)| e.deadline) {
            None => timers.cv.wait(heap).unwrap_or_else(|e| e.into_inner()),
            Some(deadline) => timers.cv.wait_timeout(heap, deadline - now).unwrap_or_else(|e| e.into_inner()).0,
        };
    }
}

fn after(when: u64, queue: *mut c_void, work: Work) {
    match super::time::deadline(when) {
        // Never: the work stays unrun, as with libdispatch.
        None => {}
        Some(deadline) if deadline <= Instant::now() => enqueue(queue, work, false),
        Some(deadline) => at(deadline, Due::Work(Obj::retain(queue), work)),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_after_f(
    when: u64,
    queue: *mut c_void,
    ctx: *mut c_void,
    f: unsafe extern "C" fn(*mut c_void),
) {
    after(when, queue, Work { f: function(f), ctx });
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn dispatch_after(when: u64, queue: *mut c_void, block: *const DynBlock<dyn Fn()>) {
    // SAFETY: the caller passes a block.
    after(when, queue, block_work(unsafe { &*block }));
}
