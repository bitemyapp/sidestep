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
//!   the name.

use std::ptr::NonNull;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, kCFRunLoopDefaultMode};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObject, NSString};

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

fn main() {
    wake_to_run();
    timer_jitter();
    for observers in [0, 1, 100] {
        post_cost(observers);
    }
}
