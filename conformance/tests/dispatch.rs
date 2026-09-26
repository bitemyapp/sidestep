//! libdispatch through dispatch2, and `NSOperationQueue`'s main queue,
//! checked on macOS and on Linux alike. The main queue needs the main
//! thread's run loop, so this file has its own `main`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{
    DispatchGroup, DispatchObject, DispatchOnce, DispatchQueue, DispatchQueueAttr, DispatchRetained, DispatchSemaphore,
    DispatchSource, DispatchTime, GlobalQueueIdentifier, MainThreadBound, dispatch_get_specific,
};
use objc2::MainThreadMarker;
use objc2_core_foundation::CFRunLoop;
use objc2_foundation::{
    NSBlockOperation, NSDate, NSDefaultRunLoopMode, NSOperation, NSOperationQueue, NSRunLoop, NSString,
};

use sidestep as _;

fn run_main(seconds: f64) {
    NSRunLoop::currentRunLoop()
        .runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &NSDate::dateWithTimeIntervalSinceNow(seconds));
}

fn run_main_until(done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        run_main(0.01);
    }
}

fn wait_until(done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn after(ms: i64) -> DispatchTime {
    DispatchTime::NOW.time(ms * 1_000_000)
}

fn global() -> DispatchRetained<DispatchQueue> {
    DispatchQueue::global_queue(GlobalQueueIdentifier::Priority(dispatch2::DispatchQueueGlobalPriority::Default))
}

// ---------------------------------------------------------------------------

fn main_queue_order_and_modes() {
    let log = Arc::new(Mutex::new(Vec::new()));
    for i in 0..5 {
        let l = log.clone();
        DispatchQueue::main().exec_async(move || {
            assert!(MainThreadMarker::new().is_some());
            l.lock().unwrap().push(i);
        });
    }
    run_main_until(|| log.lock().unwrap().len() == 5);
    assert_eq!(*log.lock().unwrap(), [0, 1, 2, 3, 4]);

    // It drains in the common modes only.
    let ran = Arc::new(AtomicUsize::new(0));
    let r = ran.clone();
    DispatchQueue::main().exec_async(move || {
        r.fetch_add(1, Ordering::SeqCst);
    });
    let custom = NSString::from_str("DispatchCustomMode");
    let keep =
        unsafe { objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(100.0, false, &RcBlock::new(|_| {})) };
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&keep, &custom) };
    NSRunLoop::currentRunLoop().runMode_beforeDate(&custom, &NSDate::dateWithTimeIntervalSinceNow(0.05));
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    let common = NSString::from_str("DispatchCommonMode");
    CFRunLoop::main().unwrap().add_common_mode(Some(unsafe { &*(&*common as *const NSString).cast() }));
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&keep, &common) };
    NSRunLoop::currentRunLoop().runMode_beforeDate(&common, &NSDate::dateWithTimeIntervalSinceNow(0.05));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    keep.invalidate();

    // Queued from another thread.
    let r = ran.clone();
    std::thread::spawn(move || {
        DispatchQueue::main().exec_async(move || {
            r.fetch_add(1, Ordering::SeqCst);
        })
    })
    .join()
    .unwrap();
    run_main_until(|| ran.load(Ordering::SeqCst) == 2);
}

fn main_queue_after_and_sync() {
    let start = Instant::now();
    let ran_at = Arc::new(Mutex::new(None));
    let r = ran_at.clone();
    DispatchQueue::main().after(after(50), move || *r.lock().unwrap() = Some(start.elapsed())).unwrap();
    run_main_until(|| ran_at.lock().unwrap().is_some());
    assert!(ran_at.lock().unwrap().unwrap() >= Duration::from_millis(50));

    // exec_sync from another thread returns after the work ran on the main
    // thread.
    let order = Arc::new(Mutex::new(Vec::new()));
    let o = order.clone();
    let other = std::thread::spawn(move || {
        let inner = o.clone();
        DispatchQueue::main()
            .exec_sync(move || inner.lock().unwrap().push(format!("work main={}", MainThreadMarker::new().is_some())));
        o.lock().unwrap().push("returned".to_string());
    });
    run_main_until(|| other.is_finished());
    other.join().unwrap();
    assert_eq!(*order.lock().unwrap(), ["work main=true", "returned"]);

    // MainThreadBound lets other threads reach main-thread values.
    let mtm = MainThreadMarker::new().unwrap();
    let bound = Arc::new(MainThreadBound::new(41, mtm));
    let b = bound.clone();
    let other = std::thread::spawn(move || b.get_on_main(|value| *value + 1));
    run_main_until(|| other.is_finished());
    assert_eq!(other.join().unwrap(), 42);
    assert_eq!(*bound.get(mtm), 41);
}

fn serial_queues() {
    let queue = DispatchQueue::new("org.sidestep.conformance.serial", DispatchQueueAttr::SERIAL);
    let log = Arc::new(Mutex::new(Vec::new()));
    let busy = Arc::new(AtomicUsize::new(0));
    for i in 0..100 {
        let (l, b) = (log.clone(), busy.clone());
        queue.exec_async(move || {
            assert_eq!(b.fetch_add(1, Ordering::SeqCst), 0, "one at a time");
            l.lock().unwrap().push(i);
            b.fetch_sub(1, Ordering::SeqCst);
        });
    }
    let l = log.clone();
    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    queue.exec_sync(move || s.store(l.lock().unwrap().len(), Ordering::SeqCst));
    // Sync work runs after everything queued before it.
    assert_eq!(seen.load(Ordering::SeqCst), 100);
    assert_eq!(*log.lock().unwrap(), (0..100).collect::<Vec<_>>());

    // From another thread, too.
    let q = queue.clone();
    let l = log.clone();
    std::thread::spawn(move || q.exec_sync(move || l.lock().unwrap().push(100))).join().unwrap();
    assert_eq!(log.lock().unwrap().len(), 101);
    let label = DispatchQueue::label(Some(&queue));
    assert_eq!(
        unsafe { std::ffi::CStr::from_ptr(label.as_ptr()) }.to_str().unwrap(),
        "org.sidestep.conformance.serial"
    );
}

fn global_queues_run_concurrently() {
    let rendezvous = Arc::new(Barrier::new(3));
    for _ in 0..2 {
        let r = rendezvous.clone();
        global().exec_async(move || {
            r.wait();
        });
    }
    // Both items block until the other runs: only concurrency gets here.
    rendezvous.wait();
    let same = std::ptr::eq(&*global(), &*global());
    assert!(same, "a global queue is one object");
}

fn concurrent_queue_barriers() {
    let queue = DispatchQueue::new("org.sidestep.conformance.concurrent", DispatchQueueAttr::concurrent());
    let log = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new(Barrier::new(3));
    for i in 0..2 {
        let (l, g) = (log.clone(), gate.clone());
        queue.exec_async(move || {
            g.wait();
            l.lock().unwrap().push(format!("before {i}"));
        });
    }
    let l = log.clone();
    queue.barrier_async(move || l.lock().unwrap().push("barrier".to_string()));
    let l = log.clone();
    queue.exec_async(move || l.lock().unwrap().push("after".to_string()));
    // The two items run together; the barrier waits for them.
    gate.wait();
    let l = log.clone();
    queue.barrier_sync(move || l.lock().unwrap().push("sync barrier".to_string()));
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 5);
    let mut before: Vec<_> = log[..2].to_vec();
    before.sort();
    assert_eq!(before, ["before 0", "before 1"]);
    assert_eq!(log[2..], ["barrier", "after", "sync barrier"]);
}

fn dispatch_once_runs_once() {
    static ONCE: DispatchOnce = DispatchOnce::new();
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let start = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let s = start.clone();
            std::thread::spawn(move || {
                s.wait();
                ONCE.call_once(|| {
                    std::thread::sleep(Duration::from_millis(20));
                    RUNS.fetch_add(1, Ordering::SeqCst);
                });
                // Everyone returns after the one run finished.
                assert_eq!(RUNS.load(Ordering::SeqCst), 1);
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(RUNS.load(Ordering::SeqCst), 1);
}

fn dispatch_times() {
    let a = DispatchTime::NOW.time(0);
    std::thread::sleep(Duration::from_millis(2));
    let b = DispatchTime::NOW.time(0);
    assert!(b.0 > a.0 && a.0 != 0);
    assert!(after(1000).0 > b.0);
    assert_eq!(DispatchTime::FOREVER.time(5).0, u64::MAX);
}

fn groups_and_semaphores() {
    let group = DispatchGroup::new();
    let count = Arc::new(AtomicUsize::new(0));
    for _ in 0..10 {
        let c = count.clone();
        group.exec_async(&global(), move || {
            std::thread::sleep(Duration::from_millis(5));
            c.fetch_add(1, Ordering::SeqCst);
        });
    }
    group.wait(DispatchTime::FOREVER).unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 10);

    let guard = group.enter();
    assert!(group.wait(after(20)).is_err(), "a wait times out while the group is entered");
    let notified = Arc::new(AtomicUsize::new(0));
    let n = notified.clone();
    group.notify(&global(), move || {
        n.fetch_add(1, Ordering::SeqCst);
    });
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(notified.load(Ordering::SeqCst), 0);
    guard.leave();
    wait_until(|| notified.load(Ordering::SeqCst) == 1);

    let semaphore = DispatchSemaphore::new(0);
    assert!(semaphore.try_acquire(after(20)).is_err());
    let s = semaphore.clone();
    global().exec_async(move || {
        std::thread::sleep(Duration::from_millis(10));
        s.signal();
    });
    let start = Instant::now();
    let guard = semaphore.try_acquire(DispatchTime::FOREVER).unwrap();
    assert!(start.elapsed() >= Duration::from_millis(5));
    // Releasing the guard gives the unit back.
    guard.release();
    semaphore.try_acquire(DispatchTime::NOW).unwrap().release();
}

fn data_and_timer_sources() {
    let queue = DispatchQueue::new("org.sidestep.conformance.sources", DispatchQueueAttr::SERIAL);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let source = unsafe {
        DispatchSource::new(&raw const dispatch2::_dispatch_source_type_data_add as *mut _, 0, 0, Some(&queue))
    };
    let (s, src) = (seen.clone(), source.clone());
    let handler = RcBlock::new(move || s.lock().unwrap().push(src.data()));
    unsafe { source.set_event_handler_with_block(RcBlock::as_ptr(&handler)) };
    // Data merged before activation is delivered together, once.
    source.merge_data(2);
    source.merge_data(3);
    std::thread::sleep(Duration::from_millis(20));
    assert!(seen.lock().unwrap().is_empty(), "sources start inactive");
    source.activate();
    wait_until(|| !seen.lock().unwrap().is_empty());
    queue.exec_sync(|| {});
    assert_eq!(*seen.lock().unwrap(), [5]);
    source.cancel();
    assert_ne!(source.testcancel(), 0);

    let fires = Arc::new(AtomicUsize::new(0));
    let timer =
        unsafe { DispatchSource::new(&raw const dispatch2::_dispatch_source_type_timer as *mut _, 0, 0, Some(&queue)) };
    let f = fires.clone();
    let handler = RcBlock::new(move || {
        f.fetch_add(1, Ordering::SeqCst);
    });
    unsafe { timer.set_event_handler_with_block(RcBlock::as_ptr(&handler)) };
    timer.set_timer(after(10), 20_000_000, 1_000_000);
    let start = Instant::now();
    timer.activate();
    wait_until(|| fires.load(Ordering::SeqCst) >= 3);
    assert!(start.elapsed() >= Duration::from_millis(45));
    timer.cancel();
    queue.exec_sync(|| {});
    let after_cancel = fires.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(fires.load(Ordering::SeqCst), after_cancel);
}

fn vnode_sources() {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    let dir = std::env::temp_dir().join(format!("sidestep-vnode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("watched");
    std::fs::write(&path, b"one").unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let queue = DispatchQueue::new("org.sidestep.conformance.vnode", DispatchQueueAttr::SERIAL);
    // DISPATCH_VNODE_WRITE | DISPATCH_VNODE_EXTEND | DISPATCH_VNODE_DELETE
    let mask = 0x2 | 0x4 | 0x1;
    let source = unsafe {
        DispatchSource::new(
            &raw const dispatch2::_dispatch_source_type_vnode as *mut _,
            file.as_raw_fd() as usize,
            mask,
            Some(&queue),
        )
    };
    let seen = Arc::new(AtomicUsize::new(0));
    let (s, src) = (seen.clone(), source.clone());
    let handler = RcBlock::new(move || {
        s.fetch_or(src.data(), Ordering::SeqCst);
    });
    unsafe { source.set_event_handler_with_block(RcBlock::as_ptr(&handler)) };
    source.activate();
    std::thread::sleep(Duration::from_millis(50));
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b" two").unwrap();
    wait_until(|| seen.load(Ordering::SeqCst) & 0x2 != 0);
    std::fs::remove_file(&path).unwrap();
    wait_until(|| seen.load(Ordering::SeqCst) & 0x1 != 0);
    source.cancel();
    drop(file);
    std::fs::remove_dir_all(&dir).unwrap();
}

fn operation_main_queue() {
    let main = NSOperationQueue::mainQueue();
    assert!(std::ptr::eq(&*main, &*NSOperationQueue::mainQueue()));
    assert!(std::ptr::eq(&*NSOperationQueue::currentQueue().unwrap(), &*main));
    assert_eq!(main.maxConcurrentOperationCount(), 1);
    let seen = Arc::new(Mutex::new(None));
    let s = seen.clone();
    let block = RcBlock::new(move || {
        let current = NSOperationQueue::currentQueue().map(|q| std::ptr::eq(&*q, &*NSOperationQueue::mainQueue()));
        *s.lock().unwrap() = Some((MainThreadMarker::new().is_some(), current));
    });
    unsafe { main.addOperationWithBlock(&block) };
    assert!(seen.lock().unwrap().is_none());
    run_main_until(|| seen.lock().unwrap().is_some());
    assert_eq!(*seen.lock().unwrap(), Some((true, Some(true))));
    std::thread::spawn(|| assert!(NSOperationQueue::currentQueue().is_none())).join().unwrap();
}

objc2::define_class!(
    #[unsafe(super(NSOperation, objc2::runtime::NSObject))]
    #[name = "DispatchTestOperation"]
    #[ivars = Arc<Mutex<Vec<String>>>]
    struct CustomOperation;

    impl CustomOperation {
        #[unsafe(method(main))]
        fn main(&self) {
            use objc2::DefinedClass;
            self.ivars().lock().unwrap().push(format!("custom main={}", MainThreadMarker::new().is_some()));
        }
    }
);

#[allow(deprecated)] // operationCount: still the way to ask on both platforms.
fn operations() {
    use objc2::AnyThread;
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    let op = unsafe {
        NSBlockOperation::blockOperationWithBlock(&RcBlock::new(move || {
            l.lock().unwrap().push(format!("block main={}", MainThreadMarker::new().is_some()))
        }))
    };
    assert!(op.isReady() && !op.isExecuting() && !op.isFinished() && !op.isCancelled());
    assert!(op.name().is_none());
    let l = log.clone();
    unsafe { op.addExecutionBlock(&RcBlock::new(move || l.lock().unwrap().push("second".to_string()))) };
    let completed = Arc::new(Mutex::new(None));
    let c = completed.clone();
    unsafe {
        op.setCompletionBlock(Some(&RcBlock::new(move || *c.lock().unwrap() = Some(MainThreadMarker::new().is_some()))))
    };
    op.start();
    assert!(op.isFinished() && !op.isExecuting());
    // The extra block runs meanwhile, on another thread: no order between
    // the two.
    let mut ran = log.lock().unwrap().clone();
    ran.sort();
    assert_eq!(ran, ["block main=true", "second"]);
    wait_until(|| completed.lock().unwrap().is_some());
    assert_eq!(*completed.lock().unwrap(), Some(false), "completion blocks run on another thread");
    log.lock().unwrap().clear();

    // Cancelled before starting: finishes without running.
    let l = log.clone();
    let op = unsafe {
        NSBlockOperation::blockOperationWithBlock(&RcBlock::new(move || l.lock().unwrap().push("ran".into())))
    };
    op.cancel();
    op.start();
    assert!(op.isFinished() && op.isCancelled());
    assert!(log.lock().unwrap().is_empty());

    let queue = NSOperationQueue::new();
    assert!(queue.name().unwrap().to_string().starts_with("NSOperationQueue 0x"));
    assert_eq!(queue.maxConcurrentOperationCount(), -1);
    queue.setMaxConcurrentOperationCount(1);
    for i in 0..5 {
        let (l, q) = (log.clone(), queue.clone());
        unsafe {
            queue.addOperationWithBlock(&RcBlock::new(move || {
                std::thread::sleep(Duration::from_millis(2));
                let current = NSOperationQueue::currentQueue().map(|c| std::ptr::eq(&*c, &*q));
                l.lock().unwrap().push(format!("{i} current={current:?} main={}", MainThreadMarker::new().is_some()));
            }))
        };
    }
    queue.waitUntilAllOperationsAreFinished();
    assert_eq!(queue.operationCount(), 0);
    let expected: Vec<String> = (0..5).map(|i| format!("{i} current=Some(true) main=false")).collect();
    assert_eq!(*log.lock().unwrap(), expected);
    log.lock().unwrap().clear();

    // A suspended queue holds its operations; cancelled ones never run.
    queue.setSuspended(true);
    let l = log.clone();
    unsafe { queue.addOperationWithBlock(&RcBlock::new(move || l.lock().unwrap().push("ran".into()))) };
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(queue.operationCount(), 1);
    assert!(log.lock().unwrap().is_empty());
    queue.cancelAllOperations();
    queue.setSuspended(false);
    queue.waitUntilAllOperationsAreFinished();
    assert!(log.lock().unwrap().is_empty());

    // Subclasses override -main.
    let custom = CustomOperation::alloc().set_ivars(log.clone());
    let custom: objc2::rc::Retained<CustomOperation> = unsafe { objc2::msg_send![super(custom), init] };
    let custom: objc2::rc::Retained<NSOperation> = objc2::rc::Retained::into_super(custom);
    queue.addOperation(&custom);
    custom.waitUntilFinished();
    assert!(custom.isFinished());
    assert_eq!(*log.lock().unwrap(), ["custom main=false"]);

    // Dependencies: an operation waits for those it depends on, wherever
    // they run.
    let order = Arc::new(Mutex::new(Vec::new()));
    let step = |name: &'static str| {
        let order = order.clone();
        unsafe { NSBlockOperation::blockOperationWithBlock(&RcBlock::new(move || order.lock().unwrap().push(name))) }
    };
    let (first, second, third) = (step("first"), step("second"), step("third"));
    second.addDependency(&first);
    third.addDependency(&second);
    assert!(first.isReady());
    assert!(!second.isReady() && !third.isReady());
    let queue = NSOperationQueue::new();
    queue.addOperation(&third);
    queue.addOperation(&second);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(order.lock().unwrap().is_empty(), "nothing is ready yet");
    let other = NSOperationQueue::new();
    other.addOperation(&first);
    queue.waitUntilAllOperationsAreFinished();
    assert_eq!(*order.lock().unwrap(), ["first", "second", "third"]);
    assert!(third.isFinished());

    let (lonely, blocker) = (step("lonely"), step("blocker"));
    lonely.addDependency(&blocker);
    assert!(!lonely.isReady());
    lonely.removeDependency(&blocker);
    assert!(lonely.isReady());
}

/// Cancelling makes an operation ready whatever it depends on, so a queue
/// holding it drains; this is how chains of dependent operations are torn
/// down.
#[allow(deprecated)] // operationCount: still the way to ask on both platforms.
fn cancelled_operations_are_ready() {
    let ran = Arc::new(AtomicUsize::new(0));
    let op = |ran: &Arc<AtomicUsize>| {
        let r = ran.clone();
        unsafe {
            NSBlockOperation::blockOperationWithBlock(&RcBlock::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
            }))
        }
    };
    let (waiting, never_queued) = (op(&ran), op(&ran));
    waiting.addDependency(&never_queued);
    let queue = NSOperationQueue::new();
    queue.addOperation(&waiting);
    std::thread::sleep(Duration::from_millis(20));
    assert!(!waiting.isReady() && !waiting.isFinished());
    waiting.cancel();
    assert!(waiting.isReady());
    queue.waitUntilAllOperationsAreFinished();
    assert!(waiting.isFinished() && waiting.isCancelled());
    assert_eq!(queue.operationCount(), 0);

    let (waiting, never_queued) = (op(&ran), op(&ran));
    waiting.addDependency(&never_queued);
    queue.addOperation(&waiting);
    std::thread::sleep(Duration::from_millis(20));
    queue.cancelAllOperations();
    queue.waitUntilAllOperationsAreFinished();
    assert!(waiting.isFinished() && waiting.isCancelled());
    assert_eq!(queue.operationCount(), 0);
    assert_eq!(ran.load(Ordering::SeqCst), 0, "cancelled operations don't run");
}

/// An operation on one queue waiting for one on another starts as soon as
/// that one finishes, however close together the two are.
fn dependencies_across_queues() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (first_queue, second_queue) = (NSOperationQueue::new(), NSOperationQueue::new());
        for _ in 0..2000 {
            objc2::rc::autoreleasepool(|_| {
                let first = unsafe { NSBlockOperation::blockOperationWithBlock(&RcBlock::new(|| {})) };
                let second = unsafe { NSBlockOperation::blockOperationWithBlock(&RcBlock::new(|| {})) };
                second.addDependency(&first);
                first_queue.addOperation(&first);
                second_queue.addOperation(&second);
                second_queue.waitUntilAllOperationsAreFinished();
                assert!(second.isFinished());
            });
        }
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(60)).expect("an operation waited for a dependency that had finished");
}

/// The completion block read back from an operation stays alive until the
/// reader's autorelease pool drains, even when it is replaced meanwhile.
fn completion_blocks_read_back() {
    struct SetOnDrop(Arc<AtomicUsize>);
    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(1, Ordering::SeqCst);
        }
    }
    let freed = Arc::new(AtomicUsize::new(0));
    let called = Arc::new(AtomicUsize::new(0));
    let op = unsafe { NSBlockOperation::blockOperationWithBlock(&RcBlock::new(|| {})) };
    let (guard, c) = (SetOnDrop(freed.clone()), called.clone());
    unsafe {
        op.setCompletionBlock(Some(&RcBlock::new(move || {
            let _keep = &guard;
            c.fetch_add(1, Ordering::SeqCst);
        })))
    };
    objc2::rc::autoreleasepool(|_| {
        let block = unsafe { op.completionBlock() };
        assert!(!block.is_null());
        unsafe { op.setCompletionBlock(None) };
        assert_eq!(freed.load(Ordering::SeqCst), 0, "freed while the reader holds it");
        unsafe { &*block }.call(());
        assert_eq!(called.load(Ordering::SeqCst), 1);
    });
    assert_eq!(freed.load(Ordering::SeqCst), 1);
    assert!(unsafe { op.completionBlock() }.is_null());
}

/// A queue attribute made from `DISPATCH_QUEUE_SERIAL` or the concurrent
/// one keeps that kind whatever quality of service or autorelease
/// frequency it adds; an initially inactive queue runs nothing until
/// `activate`, whatever else its attribute says.
fn queue_attributes() {
    use dispatch2::{DispatchAutoReleaseFrequency, DispatchQoS};
    fn serial_check(queue: &DispatchQueue) {
        let busy = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        for i in 0..20 {
            let (b, o) = (busy.clone(), order.clone());
            queue.exec_async(move || {
                assert_eq!(b.fetch_add(1, Ordering::SeqCst), 0, "one at a time");
                std::thread::sleep(Duration::from_micros(200));
                o.lock().unwrap().push(i);
                b.fetch_sub(1, Ordering::SeqCst);
            });
        }
        queue.exec_sync(|| {});
        assert_eq!(*order.lock().unwrap(), (0..20).collect::<Vec<_>>());
    }
    fn concurrent_check(queue: &DispatchQueue) {
        let rendezvous = Arc::new(Barrier::new(3));
        for _ in 0..2 {
            let r = rendezvous.clone();
            queue.exec_async(move || {
                r.wait();
            });
        }
        rendezvous.wait();
        queue.barrier_sync(|| {});
    }
    let serial = [
        DispatchQueueAttr::with_qos_class(DispatchQueueAttr::SERIAL, DispatchQoS::UserInitiated, 0),
        DispatchQueueAttr::with_qos_class(DispatchQueueAttr::SERIAL, DispatchQoS::Utility, -4),
        DispatchQueueAttr::with_autorelease_frequency(
            DispatchQueueAttr::SERIAL,
            DispatchAutoReleaseFrequency::WORK_ITEM,
        ),
    ];
    for attr in &serial {
        serial_check(&DispatchQueue::new("org.sidestep.conformance.attr-serial", Some(attr)));
    }
    let concurrent = [
        DispatchQueueAttr::with_qos_class(DispatchQueueAttr::concurrent(), DispatchQoS::UserInitiated, 0),
        DispatchQueueAttr::with_autorelease_frequency(
            DispatchQueueAttr::concurrent(),
            DispatchAutoReleaseFrequency::NEVER,
        ),
    ];
    for attr in &concurrent {
        concurrent_check(&DispatchQueue::new("org.sidestep.conformance.attr-concurrent", Some(attr)));
    }

    let serial_inactive = DispatchQueueAttr::new_initially_inactive(DispatchQueueAttr::SERIAL);
    let concurrent_inactive = DispatchQueueAttr::new_initially_inactive(DispatchQueueAttr::concurrent());
    let inactive_with_qos = DispatchQueueAttr::with_qos_class(Some(&serial_inactive), DispatchQoS::Utility, 0);
    let attrs = [(&serial_inactive, true), (&concurrent_inactive, false), (&inactive_with_qos, true)];
    for (attr, is_serial) in attrs {
        let queue = DispatchQueue::new("org.sidestep.conformance.inactive", Some(attr));
        let ran = Arc::new(AtomicUsize::new(0));
        let r = ran.clone();
        queue.exec_async(move || {
            r.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(ran.load(Ordering::SeqCst), 0, "an inactive queue runs nothing");
        queue.activate();
        wait_until(|| ran.load(Ordering::SeqCst) == 1);
        // Activating again changes nothing.
        queue.activate();
        if is_serial {
            serial_check(&queue);
        } else {
            concurrent_check(&queue);
        }
    }
}

/// Suspending a queue holds what is queued on it, from outside or from
/// inside one of its own items, until it is resumed as often.
fn suspend_and_resume() {
    let queue = DispatchQueue::new("org.sidestep.conformance.suspend", DispatchQueueAttr::SERIAL);
    let ran = Arc::new(AtomicUsize::new(0));
    let bump = |ran: &Arc<AtomicUsize>| {
        let r = ran.clone();
        move || {
            r.fetch_add(1, Ordering::SeqCst);
        }
    };
    queue.suspend();
    queue.suspend();
    queue.exec_async(bump(&ran));
    std::thread::sleep(Duration::from_millis(20));
    queue.resume();
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(ran.load(Ordering::SeqCst), 0, "suspended twice, resumed once");
    queue.resume();
    wait_until(|| ran.load(Ordering::SeqCst) == 1);

    // From inside an item: the items after it wait.
    let q = queue.clone();
    queue.exec_async(move || q.suspend());
    queue.exec_async(bump(&ran));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    queue.resume();
    wait_until(|| ran.load(Ordering::SeqCst) == 2);

    // A concurrent queue, too.
    let queue = DispatchQueue::new("org.sidestep.conformance.suspend-concurrent", DispatchQueueAttr::concurrent());
    queue.suspend();
    queue.exec_async(bump(&ran));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(ran.load(Ordering::SeqCst), 2);
    queue.resume();
    wait_until(|| ran.load(Ordering::SeqCst) == 3);
}

/// A queue's work runs on its target: a concurrent queue that targets a
/// serial one runs one item at a time, as that queue's work, and values set
/// with `set_specific` on the target are found through the chain.
fn targets_and_specifics() {
    static KEY: u8 = 0;
    fn key() -> std::ptr::NonNull<()> {
        std::ptr::NonNull::from(&KEY).cast()
    }
    fn specific() -> usize {
        unsafe { dispatch_get_specific(key().cast()) as usize }
    }
    let serial = DispatchQueue::new("org.sidestep.conformance.target", DispatchQueueAttr::SERIAL);
    let destroyed = Arc::new(AtomicUsize::new(0));
    let d = destroyed.clone();
    serial.set_specific(key(), move || {
        d.fetch_add(1, Ordering::SeqCst);
    });
    let value = unsafe { serial.specific(key().cast()) } as usize;
    assert_ne!(value, 0);
    assert!(specific() == 0, "not on the main thread outside the queue");

    let concurrent = DispatchQueue::new_with_target(
        "org.sidestep.conformance.targeting",
        DispatchQueueAttr::concurrent(),
        Some(&serial),
    );
    assert!(unsafe { concurrent.specific(key().cast()) }.is_null(), "a queue's own values only");
    let busy = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(Mutex::new(Vec::new()));
    for i in 0..20 {
        let (b, d, s) = (busy.clone(), done.clone(), serial.clone());
        concurrent.exec_async(move || {
            assert_eq!(b.fetch_add(1, Ordering::SeqCst), 0, "the serial target runs one at a time");
            std::thread::sleep(Duration::from_micros(200));
            s.assert();
            d.lock().unwrap().push((i, specific()));
            b.fetch_sub(1, Ordering::SeqCst);
        });
    }
    wait_until(|| done.lock().unwrap().len() == 20);
    assert!(done.lock().unwrap().iter().all(|&(_, found)| found == value));

    // Sync on the targeting queue runs on the caller, as its work.
    let found = Arc::new(AtomicUsize::new(0));
    let f = found.clone();
    concurrent.exec_sync(move || f.store(specific(), Ordering::SeqCst));
    assert_eq!(found.load(Ordering::SeqCst), value);
    global().exec_sync(|| assert_eq!(specific(), 0));

    // Setting a value again runs the old one's destructor (macOS does it
    // on another thread, later); freeing the queue, the current one's.
    let d = destroyed.clone();
    serial.set_specific(key(), move || {
        d.fetch_add(10, Ordering::SeqCst);
    });
    wait_until(|| destroyed.load(Ordering::SeqCst) == 1);
    drop(concurrent);
    drop(serial);
    wait_until(|| destroyed.load(Ordering::SeqCst) == 11);
}

/// Contexts, finalizers, `apply`, `async_and_wait`, `after` on a queue of
/// one's own, and the global queues' labels.
fn queue_functions() {
    // A finalizer runs with the context when the queue goes.
    let queue = DispatchQueue::new("org.sidestep.conformance.context", DispatchQueueAttr::SERIAL);
    let mut value = 5u32;
    unsafe { queue.set_context((&raw mut value).cast()) };
    assert_eq!(queue.context(), (&raw mut value).cast());
    let finalized = Arc::new(AtomicUsize::new(0));
    let f = finalized.clone();
    queue.set_finalizer(move || {
        f.fetch_add(1, Ordering::SeqCst);
    });
    queue.exec_async(|| std::thread::sleep(Duration::from_millis(10)));
    drop(queue);
    wait_until(|| finalized.load(Ordering::SeqCst) == 1);

    // apply covers every index once; on a serial queue, in order.
    let counts: Arc<Vec<AtomicUsize>> = Arc::new((0..500).map(|_| AtomicUsize::new(0)).collect());
    let c = counts.clone();
    DispatchQueue::apply_with_block(
        500,
        DispatchQueue::APPLY_AUTO,
        &RcBlock::new(move |i: usize| {
            c[i].fetch_add(1, Ordering::SeqCst);
        }),
    );
    assert!(counts.iter().all(|c| c.load(Ordering::SeqCst) == 1));
    let serial = DispatchQueue::new("org.sidestep.conformance.apply", DispatchQueueAttr::SERIAL);
    let order = Arc::new(Mutex::new(Vec::new()));
    let o = order.clone();
    DispatchQueue::apply_with_block(50, Some(&serial), &RcBlock::new(move |i: usize| o.lock().unwrap().push(i)));
    assert_eq!(*order.lock().unwrap(), (0..50).collect::<Vec<_>>());
    let concurrent = DispatchQueue::new("org.sidestep.conformance.apply-concurrent", DispatchQueueAttr::concurrent());
    let sum = Arc::new(AtomicUsize::new(0));
    let s = sum.clone();
    DispatchQueue::apply_with_block(
        100,
        Some(&concurrent),
        &RcBlock::new(move |i: usize| {
            s.fetch_add(i, Ordering::SeqCst);
        }),
    );
    assert_eq!(sum.load(Ordering::SeqCst), 4950);

    // async_and_wait runs after what was queued before, and has run when
    // it returns.
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    serial.exec_async(move || {
        std::thread::sleep(Duration::from_millis(10));
        l.lock().unwrap().push("async");
    });
    extern "C" fn waited(ctx: *mut std::ffi::c_void) {
        unsafe { &*ctx.cast::<Mutex<Vec<&'static str>>>() }.lock().unwrap().push("waited");
    }
    unsafe { serial.exec_sync_and_wait_f(Arc::as_ptr(&log).cast_mut().cast(), waited) };
    assert_eq!(*log.lock().unwrap(), ["async", "waited"]);

    // after on a serial queue runs as that queue's work, no earlier.
    let start = Instant::now();
    let ran = Arc::new(Mutex::new(None));
    let (r, q) = (ran.clone(), serial.clone());
    serial
        .after(after(30), move || {
            q.assert();
            *r.lock().unwrap() = Some(start.elapsed());
        })
        .unwrap();
    wait_until(|| ran.lock().unwrap().is_some());
    assert!(ran.lock().unwrap().unwrap() >= Duration::from_millis(30));

    let label = |identifier| {
        let queue = DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(identifier));
        let label = DispatchQueue::label(Some(&queue));
        unsafe { std::ffi::CStr::from_ptr(label.as_ptr()) }.to_str().unwrap().to_string()
    };
    use dispatch2::DispatchQoS;
    assert_eq!(label(DispatchQoS::UserInitiated), "com.apple.root.user-initiated-qos");
    assert_eq!(label(DispatchQoS::Default), "com.apple.root.default-qos");
    assert_eq!(label(DispatchQoS::Utility), "com.apple.root.utility-qos");
    assert_eq!(label(DispatchQoS::Background), "com.apple.root.background-qos");
    // The current queue's label.
    let seen = Arc::new(Mutex::new(String::new()));
    let s = seen.clone();
    serial.exec_sync(move || {
        let label = DispatchQueue::label(DispatchQueue::CURRENT_QUEUE_LABEL);
        *s.lock().unwrap() = unsafe { std::ffi::CStr::from_ptr(label.as_ptr()) }.to_str().unwrap().to_string();
    });
    assert_eq!(*seen.lock().unwrap(), "org.sidestep.conformance.apply");
    // The assertions pass where they should (failing ones crash).
    serial.exec_sync(|| {});
    DispatchQueue::main().assert();
    serial.assert_not();
}

/// Cancel handlers run once, after the last event handler; a timer with
/// no interval fires once.
fn source_cancel_and_one_shot_timers() {
    let queue = DispatchQueue::new("org.sidestep.conformance.cancel", DispatchQueueAttr::SERIAL);
    let events = Arc::new(AtomicUsize::new(0));
    let cancels = Arc::new(AtomicUsize::new(0));
    let source = unsafe {
        DispatchSource::new(&raw const dispatch2::_dispatch_source_type_data_or as *mut _, 0, 0, Some(&queue))
    };
    let e = events.clone();
    let handler = RcBlock::new(move || {
        e.fetch_add(1, Ordering::SeqCst);
    });
    unsafe { source.set_event_handler_with_block(RcBlock::as_ptr(&handler)) };
    let (c, e) = (cancels.clone(), events.clone());
    let cancel = RcBlock::new(move || {
        assert_eq!(e.load(Ordering::SeqCst), 1, "after the event handler");
        c.fetch_add(1, Ordering::SeqCst);
    });
    unsafe { source.set_cancel_handler_with_block(RcBlock::as_ptr(&cancel)) };
    source.activate();
    source.merge_data(1);
    wait_until(|| events.load(Ordering::SeqCst) == 1);
    source.cancel();
    source.cancel();
    wait_until(|| cancels.load(Ordering::SeqCst) == 1);
    source.merge_data(1);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!((events.load(Ordering::SeqCst), cancels.load(Ordering::SeqCst)), (1, 1));

    let fires = Arc::new(AtomicUsize::new(0));
    let timer =
        unsafe { DispatchSource::new(&raw const dispatch2::_dispatch_source_type_timer as *mut _, 0, 0, Some(&queue)) };
    let f = fires.clone();
    let handler = RcBlock::new(move || {
        f.fetch_add(1, Ordering::SeqCst);
    });
    unsafe { timer.set_event_handler_with_block(RcBlock::as_ptr(&handler)) };
    timer.set_timer(after(10), DispatchTime::FOREVER.0, 0);
    timer.activate();
    wait_until(|| fires.load(Ordering::SeqCst) == 1);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(fires.load(Ordering::SeqCst), 1, "a timer with no interval fires once");
    timer.cancel();
}

/// Sync on an idle queue from a thread of the pool's still runs when every
/// worker is busy: the caller runs the work itself.
fn sync_while_the_pool_is_busy() {
    let queue = DispatchQueue::new("org.sidestep.conformance.sync-busy", DispatchQueueAttr::SERIAL);
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()) * 2;
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let started = Arc::new(AtomicUsize::new(0));
    for _ in 0..workers {
        let (r, s) = (release.clone(), started.clone());
        global().exec_async(move || {
            s.fetch_add(1, Ordering::SeqCst);
            let mut go = r.0.lock().unwrap();
            while !*go {
                go = r.1.wait(go).unwrap();
            }
        });
    }
    wait_until(|| started.load(Ordering::SeqCst) > 0);
    let start = Instant::now();
    let ran = Arc::new(AtomicUsize::new(0));
    let r = ran.clone();
    queue.exec_sync(move || {
        r.fetch_add(1, Ordering::SeqCst);
    });
    let took = start.elapsed();
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(took < Duration::from_millis(40), "sync waited {took:?} for a busy pool");
}

/// A block that unwinds out of a queue ends the process (libdispatch
/// can't carry on after a callout that throws) instead of leaving the
/// queue wedged and the next sync on it hanging. Checked in a child
/// process.
fn unwinding_blocks_end_the_process() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .env("SIDESTEP_DISPATCH_CHILD", "unwinding-block")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output().unwrap()).unwrap());
    let output = rx.recv_timeout(Duration::from_secs(20)).expect("the queue wedged");
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("returned"));
}

/// The child process of `unwinding_blocks_end_the_process`.
fn unwinding_block() {
    let queue = DispatchQueue::new("org.sidestep.conformance.unwinding", DispatchQueueAttr::SERIAL);
    let block = RcBlock::new(|| panic!("a block that unwinds"));
    unsafe { queue.exec_async_with_block(RcBlock::as_ptr(&block)) };
    std::thread::sleep(Duration::from_millis(100));
    queue.exec_sync(|| {});
    println!("returned");
}

type Test = (&'static str, fn());

fn main() {
    assert!(MainThreadMarker::new().is_some(), "runs on the main thread");
    let tests: &[Test] = &[
        ("main_queue_order_and_modes", main_queue_order_and_modes),
        ("main_queue_after_and_sync", main_queue_after_and_sync),
        ("serial_queues", serial_queues),
        ("global_queues_run_concurrently", global_queues_run_concurrently),
        ("concurrent_queue_barriers", concurrent_queue_barriers),
        ("dispatch_once_runs_once", dispatch_once_runs_once),
        ("dispatch_times", dispatch_times),
        ("groups_and_semaphores", groups_and_semaphores),
        ("data_and_timer_sources", data_and_timer_sources),
        ("vnode_sources", vnode_sources),
        ("operation_main_queue", operation_main_queue),
        ("operations", operations),
        ("cancelled_operations_are_ready", cancelled_operations_are_ready),
        ("dependencies_across_queues", dependencies_across_queues),
        ("completion_blocks_read_back", completion_blocks_read_back),
        ("queue_attributes", queue_attributes),
        ("suspend_and_resume", suspend_and_resume),
        ("targets_and_specifics", targets_and_specifics),
        ("queue_functions", queue_functions),
        ("source_cancel_and_one_shot_timers", source_cancel_and_one_shot_timers),
        ("sync_while_the_pool_is_busy", sync_while_the_pool_is_busy),
        ("unwinding_blocks_end_the_process", unwinding_blocks_end_the_process),
    ];
    if std::env::var("SIDESTEP_DISPATCH_CHILD").is_ok_and(|c| c == "unwinding-block") {
        unwinding_block();
        return;
    }
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test());
        println!("test {name} ... ok");
    }
}
