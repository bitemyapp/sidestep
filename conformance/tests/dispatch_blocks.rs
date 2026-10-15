//! Cancellable dispatch work through the published dispatch2 ABI.
use block2::{DynBlock, RcBlock};
use dispatch2::{
    DispatchQoS, DispatchQueue, DispatchTime, dispatch_block_cancel, dispatch_block_create_with_qos_class,
    dispatch_block_notify, dispatch_block_testcancel, dispatch_block_wait,
};
use sidestep as _;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn item(f: impl Fn() + 'static) -> RcBlock<dyn Fn()> {
    let body = RcBlock::new(f);
    unsafe {
        RcBlock::from_raw(dispatch_block_create_with_qos_class(
            std::mem::zeroed(),
            DispatchQoS::Unspecified,
            0,
            RcBlock::as_ptr(&body),
        ))
        .unwrap()
    }
}
#[test]
fn cancellation_is_shared_by_copies_and_completes_when_executed() {
    let called = Arc::new(AtomicUsize::new(0));
    let c = called.clone();
    let block = item(move || {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let copy = block.copy();
    unsafe {
        assert_ne!(dispatch_block_wait(RcBlock::as_ptr(&block), DispatchTime::NOW), 0);
        dispatch_block_cancel(RcBlock::as_ptr(&copy));
        assert_ne!(dispatch_block_testcancel(RcBlock::as_ptr(&block)), 0);
    }
    drop(copy);
    block.call(());
    assert_eq!(called.load(Ordering::SeqCst), 0);
    unsafe {
        assert_eq!(dispatch_block_wait(RcBlock::as_ptr(&block), DispatchTime::NOW), 0);
    }
}
#[test]
fn queue_owns_block_until_completion_and_runs_notifications() {
    let queue = DispatchQueue::new("sidestep.dispatch-blocks", None);
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let c = tx.clone();
    let block = item(move || {
        c.lock().unwrap().send(1).unwrap();
    });
    let c = tx.clone();
    let notification = RcBlock::new(move || {
        c.lock().unwrap().send(2).unwrap();
    });
    unsafe {
        dispatch_block_notify(RcBlock::as_ptr(&block), &queue, RcBlock::as_ptr(&notification));
        queue.exec_async_with_block(RcBlock::as_ptr(&block));
    }
    drop(block);
    drop(notification);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
}
#[test]
fn cancellation_does_not_interrupt_running_body() {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let resume_rx = std::sync::Mutex::new(resume_rx);
    let finished = Arc::new(AtomicUsize::new(0));
    let f = finished.clone();
    let block = item(move || {
        started_tx.send(()).unwrap();
        resume_rx.lock().unwrap().recv().unwrap();
        f.store(1, Ordering::SeqCst);
    });
    let queue = DispatchQueue::new("sidestep.running-block", None);
    unsafe {
        queue.exec_async_with_block(RcBlock::as_ptr(&block));
    }
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    unsafe {
        dispatch_block_cancel(RcBlock::as_ptr(&block));
    }
    resume_tx.send(()).unwrap();
    unsafe {
        assert_eq!(dispatch_block_wait(RcBlock::as_ptr(&block), DispatchTime::NOW.time(5_000_000_000)), 0);
    }
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}
#[test]
fn releasing_an_unsubmitted_item_releases_its_capture() {
    let capture = Arc::new(());
    let c = capture.clone();
    let block = item(move || {
        let _ = &c;
    });
    assert_eq!(Arc::strong_count(&capture), 2);
    let copy = block.copy();
    drop(block);
    assert_eq!(Arc::strong_count(&capture), 2);
    drop(copy);
    assert_eq!(Arc::strong_count(&capture), 1);
}
#[test]
fn invalid_qos_is_rejected() {
    let body: RcBlock<dyn Fn()> = RcBlock::new(|| {});
    let raw: *mut DynBlock<dyn Fn()> = unsafe {
        dispatch_block_create_with_qos_class(std::mem::zeroed(), DispatchQoS::Utility, 1, RcBlock::as_ptr(&body))
    };
    assert!(raw.is_null());
}

#[test]
fn barrier_flag_orders_concurrent_queue_work() {
    use dispatch2::{DispatchQueueAttr, dispatch_block_create};
    let queue = DispatchQueue::new("sidestep.block-barrier", DispatchQueueAttr::concurrent());
    let before = Arc::new(AtomicUsize::new(0));
    for _ in 0..16 {
        let before = before.clone();
        queue.exec_async(move || {
            before.fetch_add(1, Ordering::SeqCst);
        });
    }
    let b = before.clone();
    let body = RcBlock::new(move || {
        assert_eq!(b.load(Ordering::SeqCst), 16);
        b.store(17, Ordering::SeqCst);
    });
    // dispatch2 does not re-export its flags type; it is an unsigned long.
    #[allow(clippy::missing_transmute_annotations)] // The destination flags type is private in dispatch2.
    let block = unsafe {
        RcBlock::from_raw(dispatch_block_create(std::mem::transmute::<std::ffi::c_ulong, _>(1), RcBlock::as_ptr(&body)))
            .unwrap()
    };
    unsafe {
        queue.exec_async_with_block(RcBlock::as_ptr(&block));
    }
    for _ in 0..16 {
        let b = before.clone();
        queue.exec_async(move || {
            assert_eq!(b.load(Ordering::SeqCst), 17);
        });
    }
    queue.barrier_sync(|| {});
    assert_eq!(before.load(Ordering::SeqCst), 17);
}
