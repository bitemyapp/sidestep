//! Worker threads that lay text out in parallel.
//!
//! A display pass that draws many lines nobody has measured before (a new
//! page, a tile scrolled into view) defers laying them out until its end,
//! then lays them all out at once: the calling thread takes jobs alongside
//! the workers, and waits for the ones they took. Shaping is the costly
//! part of drawing new text, and lines are independent, so a page lays out
//! about as many times faster as there are cores.
//!
//! Each worker keeps its own [`Ctx`](super::Ctx), as every thread that lays
//! text out does. Jobs carry only Rust data (text, attributes, options), no
//! Objective-C objects.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use super::layout::{Job, TextLayout, compute};

struct Batch {
    /// A job, until a thread takes it; then the job with its layout.
    slots: Vec<Mutex<Slot>>,
    /// The next job nobody has taken.
    next: AtomicUsize,
    done: Mutex<usize>,
    all_done: Condvar,
}

enum Slot {
    Waiting(Job),
    Taken,
    Done(Job, TextLayout),
}

fn workers() -> &'static [Sender<Arc<Batch>>] {
    static WORKERS: OnceLock<Vec<Sender<Arc<Batch>>>> = OnceLock::new();
    WORKERS.get_or_init(|| {
        let count = std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(1).clamp(1, 7);
        (0..count)
            .filter_map(|i| {
                let (tx, rx) = mpsc::channel::<Arc<Batch>>();
                let spawned = std::thread::Builder::new()
                    .name(format!("sidestep-text-{i}"))
                    .spawn(move || rx.iter().for_each(|batch| work(&batch)));
                spawned.ok().map(|_| tx)
            })
            .collect()
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Lay `jobs` out, in parallel if there are other cores to help, and hand
/// them back with their layouts, in order.
pub fn run(jobs: Vec<Job>) -> Vec<(Job, TextLayout)> {
    let count = jobs.len();
    let batch = Arc::new(Batch {
        slots: jobs.into_iter().map(|job| Mutex::new(Slot::Waiting(job))).collect(),
        next: AtomicUsize::new(0),
        done: Mutex::new(0),
        all_done: Condvar::new(),
    });
    for worker in workers().iter().take(count.saturating_sub(1)) {
        // A worker that has gone leaves its share to the others.
        let _ = worker.send(batch.clone());
    }
    work(&batch);
    let mut done = lock(&batch.done);
    while *done < count {
        done = batch.all_done.wait(done).unwrap_or_else(|e| e.into_inner());
    }
    drop(done);
    batch
        .slots
        .iter()
        .filter_map(|slot| match std::mem::replace(&mut *lock(slot), Slot::Taken) {
            Slot::Done(job, layout) => Some((job, layout)),
            _ => None,
        })
        .collect()
}

/// Take jobs from `batch` until none are left.
fn work(batch: &Batch) {
    loop {
        let index = batch.next.fetch_add(1, Ordering::Relaxed);
        let Some(slot) = batch.slots.get(index) else { return };
        let Slot::Waiting(job) = std::mem::replace(&mut *lock(slot), Slot::Taken) else { continue };
        // A panic here must not leave the caller waiting: the job is then
        // drawn empty.
        let layout = catch_unwind(AssertUnwindSafe(|| {
            super::with_ctx(|ctx| compute(ctx, &job.text, &job.attrs, &job.runs, &job.opts))
        }))
        .unwrap_or_default();
        *lock(slot) = Slot::Done(job, layout);
        let mut done = lock(&batch.done);
        *done += 1;
        if *done == batch.slots.len() {
            batch.all_done.notify_all();
        }
    }
}
