//! Latency and cost of the run loop and the notification center, as the
//! median of seven runs. Run in release mode on macOS (Apple's
//! Foundation) and on Linux (Sidestep's) to compare:
//! `cargo run --release -p servicebench`.
//!
//! - wake-to-run: another thread hands a block to a sleeping run loop
//!   (`CFRunLoopPerformBlock` and `CFRunLoopWakeUp`); the time until the
//!   block runs.
//! - timer jitter: how late a 10 ms repeating timer fires, per fire.
//! - post: `-postNotificationName:object:` with 0, 1 and 100 observers of
//!   the name, and with 1000 observers of the name each for its own
//!   object (one of them the poster).
//! - sync: `dispatch_sync` to an idle serial queue, alone and while every
//!   pool thread is busy.
//! - locks: an uncontended lock and unlock of `NSLock` and
//!   `NSRecursiveLock`.
//! - perform burst: 4000 `performSelectorOnMainThread:…waitUntilDone:NO`
//!   requests, then `-runMode:beforeDate:` until all have run.
//! - CFString reads: every unit of a 100k-unit string through
//!   `CFStringGetCharacterAtIndex`, and in 64-unit `CFStringGetCharacters`
//!   chunks, ASCII and not.

use std::ptr::NonNull;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchQueueGlobalPriority, GlobalQueueIdentifier};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, define_class, msg_send, sel};
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRange, CFRetained, CFRunLoop, CFRunLoopTimer, CFString, kCFRunLoopDefaultMode,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSLock, NSLocking, NSNotification, NSNotificationCenter, NSObject,
    NSObjectNSThreadPerformAdditions, NSRecursiveLock, NSRunLoop, NSString,
};

use sidestep as _;

/// A run loop on its own thread, kept alive by a distant timer.
struct LoopThread {
    run_loop: CFRetained<CFRunLoop>,
}

// SAFETY: CFRunLoop's perform and wake-up functions may be called from any
// thread.
unsafe impl Send for LoopThread {}

fn spawn_loop() -> LoopThread {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let current = CFRunLoop::current().unwrap();
        let keep_alive =
            unsafe { CFRunLoopTimer::with_handler(None, CFAbsoluteTimeGetCurrent() + 1e9, 0.0, 0, 0, None) }.unwrap();
        current.add_timer(Some(&keep_alive), unsafe { kCFRunLoopDefaultMode });
        tx.send(LoopThread { run_loop: current }).unwrap();
        loop {
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 1e9, false);
        }
    });
    rx.recv().unwrap()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn median_of_seven(mut runs: Vec<f64>) -> f64 {
    runs.sort_by(f64::total_cmp);
    runs[runs.len() / 2]
}

fn wake_to_run() {
    let target = spawn_loop();
    let mut p50s = Vec::new();
    let mut p99s = Vec::new();
    for _ in 0..7 {
        let mut samples = Vec::new();
        for _ in 0..300 {
            // Let the loop fall asleep first.
            std::thread::sleep(Duration::from_micros(300));
            let (tx, rx) = mpsc::channel::<Duration>();
            let sent = Instant::now();
            let block = RcBlock::new(move || {
                let _ = tx.send(sent.elapsed());
            });
            unsafe { target.run_loop.perform_block(kCFRunLoopDefaultMode.map(|m| m.as_ref()), Some(&block)) };
            target.run_loop.wake_up();
            samples.push(rx.recv().unwrap().as_secs_f64() * 1e6);
        }
        samples.sort_by(f64::total_cmp);
        p50s.push(percentile(&samples, 0.5));
        p99s.push(percentile(&samples, 0.99));
    }
    println!("{:<44} {:>8.1} µs  (p99 {:.1} µs)", "wake-to-run, p50", median_of_seven(p50s), median_of_seven(p99s));
}

fn timer_jitter() {
    let mut p50s = Vec::new();
    let mut p99s = Vec::new();
    for _ in 0..7 {
        let lateness = Arc::new(Mutex::new(Vec::new()));
        let recorded = lateness.clone();
        std::thread::spawn(move || {
            let interval = 0.010;
            let start = CFAbsoluteTimeGetCurrent() + interval;
            let fires = Arc::new(Mutex::new(0u32));
            let count = fires.clone();
            let block = RcBlock::new(move |timer: *mut CFRunLoopTimer| {
                let mut n = count.lock().unwrap();
                let due = start + f64::from(*n) * interval;
                recorded.lock().unwrap().push((CFAbsoluteTimeGetCurrent() - due) * 1e6);
                *n += 1;
                if *n == 50 {
                    unsafe { (*timer).invalidate() };
                    CFRunLoop::current().unwrap().stop();
                }
            });
            let timer = unsafe { CFRunLoopTimer::with_handler(None, start, interval, 0, 0, Some(&block)) }.unwrap();
            CFRunLoop::current().unwrap().add_timer(Some(&timer), unsafe { kCFRunLoopDefaultMode });
            CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 5.0, false);
            drop(fires);
        })
        .join()
        .unwrap();
        let mut samples = lateness.lock().unwrap().clone();
        samples.sort_by(f64::total_cmp);
        p50s.push(percentile(&samples, 0.5));
        p99s.push(percentile(&samples, 0.99));
    }
    println!(
        "{:<44} {:>8.1} µs  (p99 {:.1} µs)",
        "timer lateness, 10 ms repeating, p50",
        median_of_seven(p50s),
        median_of_seven(p99s)
    );
}

fn post_cost(observers: usize) {
    let center = NSNotificationCenter::defaultCenter();
    let name = NSString::from_str(&format!("ServiceBench{observers}"));
    let sender = NSObject::new();
    let hits = Arc::new(Mutex::new(0usize));
    let tokens: Vec<Retained<AnyObject>> = (0..observers)
        .map(|_| {
            let hits = hits.clone();
            let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
                *hits.lock().unwrap() += 1;
            });
            let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block) };
            unsafe { Retained::cast_unchecked(token) }
        })
        .collect();
    let iters = if observers >= 100 { 20_000 } else { 200_000 };
    let runs = (0..7)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iters {
                unsafe { center.postNotificationName_object(&name, Some(&sender)) };
            }
            start.elapsed().as_nanos() as f64 / iters as f64
        })
        .collect();
    println!("{:<44} {:>8.1} ns", format!("post, {observers} observers"), median_of_seven(runs));
    for token in tokens {
        unsafe { center.removeObserver(&token) };
    }
}

/// Posts of a name 1000 observers registered for, each for its own object.
fn post_per_object() {
    let center = NSNotificationCenter::defaultCenter();
    let name = NSString::from_str("ServiceBenchPerObject");
    let senders: Vec<_> = (0..1000).map(|_| NSObject::new()).collect();
    let block = RcBlock::new(|_note: NonNull<NSNotification>| {});
    let tokens: Vec<Retained<AnyObject>> = senders
        .iter()
        .map(|sender| {
            let token =
                unsafe { center.addObserverForName_object_queue_usingBlock(Some(&name), Some(sender), None, &block) };
            unsafe { Retained::cast_unchecked(token) }
        })
        .collect();
    let stranger = NSObject::new();
    for (label, sender) in [("one match", &*senders[500]), ("no match", &*stranger)] {
        let iters = 200_000;
        let runs = (0..7)
            .map(|_| {
                let start = Instant::now();
                for _ in 0..iters {
                    unsafe { center.postNotificationName_object(&name, Some(sender)) };
                }
                start.elapsed().as_nanos() as f64 / iters as f64
            })
            .collect();
        println!("{:<44} {:>8.1} ns", format!("post, 1000 per-object observers, {label}"), median_of_seven(runs));
    }
    for token in tokens {
        unsafe { center.removeObserver(&token) };
    }
}

fn dispatch_sync() {
    let queue = DispatchQueue::new("servicebench.sync", DispatchQueueAttr::SERIAL);
    let iters = 200_000;
    let runs = (0..7)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iters {
                queue.exec_sync(|| {});
            }
            start.elapsed().as_nanos() as f64 / iters as f64
        })
        .collect();
    println!("{:<44} {:>8.1} ns", "dispatch_sync, idle serial queue", median_of_seven(runs));

    // Every pool thread busy for 400 ms, then one sync to an idle queue.
    let global = DispatchQueue::global_queue(GlobalQueueIdentifier::Priority(DispatchQueueGlobalPriority::Default));
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()) * 2;
    let runs = (0..7)
        .map(|_| {
            for _ in 0..workers {
                global.exec_async(|| std::thread::sleep(Duration::from_millis(400)));
            }
            std::thread::sleep(Duration::from_millis(20));
            let start = Instant::now();
            queue.exec_sync(|| {});
            let took = start.elapsed().as_secs_f64() * 1e3;
            std::thread::sleep(Duration::from_millis(500));
            took
        })
        .collect();
    println!("{:<44} {:>8.2} ms", "dispatch_sync, idle queue, busy pool", median_of_seven(runs));
}

fn locks() {
    fn cost(label: &str, lock: &dyn Fn(), unlock: &dyn Fn()) {
        let iters = 2_000_000;
        let runs = (0..7)
            .map(|_| {
                let start = Instant::now();
                for _ in 0..iters {
                    lock();
                    unlock();
                }
                start.elapsed().as_nanos() as f64 / iters as f64
            })
            .collect();
        println!("{:<44} {:>8.1} ns", format!("{label} lock+unlock, uncontended"), median_of_seven(runs));
    }
    let lock = NSLock::new();
    cost("NSLock", &|| unsafe { lock.lock() }, &|| unsafe { lock.unlock() });
    let recursive = unsafe { NSRecursiveLock::new() };
    cost("NSRecursiveLock", &|| unsafe { recursive.lock() }, &|| unsafe { recursive.unlock() });
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "ServiceBenchPerformer"]
    struct Performer;

    impl Performer {
        #[unsafe(method(hit:))]
        fn hit(&self, _argument: Option<&AnyObject>) {
            HITS.with(|h| h.set(h.get() + 1));
        }
    }
);

thread_local!(static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

/// Runs of `-runMode:beforeDate:` a burst of main-thread performs takes,
/// and the time.
fn perform_burst() {
    let target: Retained<Performer> = unsafe { msg_send![Performer::class(), new] };
    let count = 4000;
    let mut times = Vec::new();
    let mut passes = 0;
    for _ in 0..7 {
        HITS.with(|h| h.set(0));
        for _ in 0..count {
            unsafe { target.performSelectorOnMainThread_withObject_waitUntilDone(sel!(hit:), None, false) };
        }
        let start = Instant::now();
        passes = 0;
        while HITS.with(|h| h.get()) < count {
            NSRunLoop::currentRunLoop()
                .runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &NSDate::dateWithTimeIntervalSinceNow(1.0));
            passes += 1;
        }
        times.push(start.elapsed().as_secs_f64() * 1e3);
    }
    println!(
        "{:<44} {:>8.2} ms  ({passes} runMode:beforeDate:)",
        format!("perform burst, {count} requests"),
        median_of_seven(times)
    );
}

fn cfstring_reads() {
    for (label, text) in [("ASCII", "0123456789".repeat(10_000)), ("non-ASCII", "h\u{e9}llo w\u{f6}rld".repeat(1_000))]
    {
        let string = CFString::from_str(&text);
        let length = string.length();
        let runs = (0..7)
            .map(|_| {
                let start = Instant::now();
                let mut sum = 0u64;
                for i in 0..length {
                    sum += u64::from(unsafe { string.character_at_index(i) });
                }
                std::hint::black_box(sum);
                start.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        println!(
            "{:<44} {:>8.2} ms",
            format!("CFString {label}, {length} units, one at a time"),
            median_of_seven(runs)
        );
        let runs = (0..7)
            .map(|_| {
                let start = Instant::now();
                let mut buffer = [0u16; 64];
                let mut at = 0;
                while at < length {
                    let n = 64.min(length - at);
                    unsafe { string.characters(CFRange { location: at, length: n }, buffer.as_mut_ptr()) };
                    at += n;
                }
                std::hint::black_box(buffer);
                start.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        println!(
            "{:<44} {:>8.2} ms",
            format!("CFString {label}, {length} units, 64-unit chunks"),
            median_of_seven(runs)
        );
    }
}

fn main() {
    let only = std::env::args().nth(1);
    let run = |name: &str| only.as_deref().is_none_or(|o| o == name);
    if run("wake") {
        wake_to_run();
    }
    if run("timer") {
        timer_jitter();
    }
    if run("post") {
        for observers in [0, 1, 100] {
            post_cost(observers);
        }
        post_per_object();
    }
    if run("sync") {
        dispatch_sync();
    }
    if run("locks") {
        locks();
    }
    if run("perform") {
        perform_burst();
    }
    if run("cfstring") {
        cfstring_reads();
    }
}
