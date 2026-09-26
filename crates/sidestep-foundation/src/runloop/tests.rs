//! The Rust API other parts of Sidestep drive run loops with. The
//! Objective-C and CoreFoundation behaviour is in
//! `conformance/tests/runloop.rs`; these check what only Sidestep has.
//! Each test runs on a thread of its own, so each has a fresh loop.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use super::{Activity, Mode, RunResult, current};

fn soon(ms: u64) -> Option<Instant> {
    Some(Instant::now() + Duration::from_millis(ms))
}

#[test]
fn perform_from_another_thread_wakes_the_loop() {
    let here = current();
    let source = here.add_source(&[Mode::DEFAULT], 0, || {});
    let (tx, rx) = mpsc::channel();
    let target = here.clone();
    let other = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        let sent = Instant::now();
        target.perform(&[Mode::DEFAULT], move || {
            tx.send(sent.elapsed()).unwrap();
            current().stop();
        });
    });
    // A source keeps the mode alive; only the block can end the run early.
    assert_eq!(here.run_mode(Mode::DEFAULT, soon(5000), false), RunResult::Stopped);
    other.join().unwrap();
    let latency = rx.recv().unwrap();
    assert!(latency < Duration::from_millis(100), "{latency:?}");
    source.invalidate();
}

#[test]
fn sources_end_a_run_that_asks() {
    let here = current();
    let performed = Rc::new(RefCell::new(Vec::new()));
    let p = performed.clone();
    let late = here.add_source(&[Mode::DEFAULT], 10, move || p.borrow_mut().push("late"));
    let p = performed.clone();
    let early = here.add_source(&[Mode::DEFAULT], -10, move || p.borrow_mut().push("early"));
    late.signal();
    early.signal();
    // One source per run when returning after a source, lowest order first.
    assert_eq!(here.run_mode(Mode::DEFAULT, soon(1000), true), RunResult::HandledSource);
    assert_eq!(*performed.borrow(), ["early"]);
    assert_eq!(here.run_mode(Mode::DEFAULT, soon(1000), true), RunResult::HandledSource);
    assert_eq!(*performed.borrow(), ["early", "late"]);

    // Signalled from another thread, with a wake-up.
    let signal = late.clone();
    let other = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        signal.signal_and_wake();
    });
    assert_eq!(here.run_mode(Mode::DEFAULT, soon(5000), true), RunResult::HandledSource);
    other.join().unwrap();
    assert_eq!(performed.borrow().len(), 3);

    // Not in other modes, and gone once invalidated.
    late.signal();
    assert_eq!(here.run_mode(Mode::named("SourcelessMode"), soon(10), true), RunResult::Finished);
    late.invalidate();
    early.invalidate();
    assert_eq!(here.run_mode(Mode::DEFAULT, soon(1000), true), RunResult::Finished);
    assert_eq!(performed.borrow().len(), 3);
}

#[test]
fn closure_observers() {
    let here = current();
    let source = here.add_source(&[Mode::DEFAULT], 0, || {});
    let log = Rc::new(RefCell::new(Vec::new()));
    let l = log.clone();
    let late = here.add_observer(&[Mode::COMMON], Activity::BEFORE_WAITING, 2_000_000, move |a| {
        l.borrow_mut().push(("display", a.0));
    });
    let l = log.clone();
    let early = here.add_observer(&[Mode::DEFAULT], Activity::BEFORE_WAITING | Activity::EXIT, 0, move |a| {
        l.borrow_mut().push(("early", a.0));
    });
    here.run_mode(Mode::DEFAULT, soon(10), false);
    assert_eq!(*log.borrow(), [("early", 32), ("display", 32), ("early", 128)]);
    here.remove_observer(early);
    log.borrow_mut().clear();
    here.run_mode(Mode::DEFAULT, soon(10), false);
    assert_eq!(*log.borrow(), [("display", 32)]);

    // An observer may run the loop again, and see itself called inside.
    log.borrow_mut().clear();
    here.remove_observer(late);
    let depth = Rc::new(Cell::new(0));
    let (l, d) = (log.clone(), depth.clone());
    let nested = here.add_observer(&[Mode::DEFAULT], Activity::ENTRY, 0, move |_| {
        l.borrow_mut().push(("entry", current().depth()));
        if d.replace(d.get() + 1) == 0 {
            current().run_mode(Mode::DEFAULT, soon(1), false);
        }
    });
    here.run_mode(Mode::DEFAULT, soon(10), false);
    assert_eq!(*log.borrow(), [("entry", 1), ("entry", 2)]);
    here.remove_observer(nested);
    source.invalidate();
}

#[test]
fn common_modes_from_another_thread() {
    let here = current();
    let tracking = Mode::named("RustApiTrackingMode");
    let ran = Rc::new(Cell::new(0));
    let r = ran.clone();
    let source = here.add_source(&[Mode::COMMON], 0, move || r.set(r.get() + 1));
    source.signal();
    assert_eq!(here.run_mode(tracking, soon(10), true), RunResult::Finished);
    let target = here.clone();
    std::thread::spawn(move || target.add_common_mode(tracking)).join().unwrap();
    assert_eq!(here.run_mode(tracking, soon(1000), true), RunResult::HandledSource);
    assert_eq!(ran.get(), 1);

    // Blocks for the common modes follow the set as it is when they run.
    let ran = Arc::new(AtomicU32::new(0));
    let r = ran.clone();
    here.perform(&[Mode::COMMON], move || {
        r.fetch_add(1, Ordering::Relaxed);
    });
    here.run_mode(tracking, soon(10), false);
    assert_eq!(ran.load(Ordering::Relaxed), 1);
    source.invalidate();
}

#[test]
fn stop_from_another_thread() {
    let here = current();
    let source = here.add_source(&[Mode::DEFAULT], 0, || {});
    let target = here.clone();
    let other = std::thread::spawn(move || {
        while target.current_mode() != Some(Mode::DEFAULT) || !target.is_waiting() {
            std::thread::sleep(Duration::from_millis(1));
        }
        target.stop();
    });
    assert_eq!(here.run_mode(Mode::DEFAULT, soon(5000), false), RunResult::Stopped);
    other.join().unwrap();
    source.invalidate();
}

#[test]
fn an_idle_loop_sleeps() {
    let here = current();
    let source = here.add_source(&[Mode::DEFAULT], 0, || {});
    let wakes = Rc::new(Cell::new(0));
    let w = wakes.clone();
    let observer = here.add_observer(&[Mode::DEFAULT], Activity::AFTER_WAITING, 0, move |_| w.set(w.get() + 1));
    let start = Instant::now();
    assert_eq!(here.run_mode(Mode::DEFAULT, soon(1000), false), RunResult::TimedOut);
    assert!(start.elapsed() >= Duration::from_millis(1000));
    // One wake-up: the deadline.
    assert_eq!(wakes.get(), 1);
    here.remove_observer(observer);
    source.invalidate();
}
