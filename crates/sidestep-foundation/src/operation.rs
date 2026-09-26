//! `NSOperation`, `NSBlockOperation` and `NSOperationQueue`.
//!
//! The main queue runs its operations on the main thread through the main
//! dispatch queue, so they run whenever the main loop runs in a common mode.
//! Like macOS, it hands the dispatch queue one operation at a time: the
//! next one is queued when the previous one has finished, so blocks
//! dispatched meanwhile run in between. Other queues run their operations
//! on the dispatch pool, up to `maxConcurrentOperationCount` at a time
//! (the pool's width when it is -1), in the order they were added.
//! `+currentQueue` is the main queue anywhere on the main thread, the
//! running queue inside an operation, and nil elsewhere.
//!
//! An operation is ready once the operations it depends on have finished,
//! or once it is cancelled; queues start only ready operations, in the
//! order they were added, and a queue whose waiting operations aren't
//! ready looks again whenever an operation finishes, is cancelled or loses
//! a dependency. An operation that was cancelled before it started
//! finishes without running. A completion block runs on a pool thread once
//! the operation has finished, as on macOS. A block operation runs its
//! first block on the thread that starts it and the others on the pool at
//! the same time, and finishes when all have run.

use std::cell::Cell;
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSCopying, NSInteger, NSNotification, NSOperation, NSOperationQueue, NSOperationQueuePriority, NSQualityOfService,
    NSString, NSUInteger,
};

use crate::runloop::core::{Work, main_queue_push};
use crate::thread::{is_main_thread, lock};

sidestep_runtime::static_class!(pub(crate) NSOPERATION, NSOPERATION_META = "NSOperation", || {
    let _ = NSOperationImpl::class();
    crate::perform::install();
});

sidestep_runtime::static_class!(pub(crate) NSBLOCKOPERATION, NSBLOCKOPERATION_META = "NSBlockOperation", || {
    let _ = NSBlockOperationImpl::class();
    crate::perform::install();
});

sidestep_runtime::static_class!(pub(crate) NSOPERATIONQUEUE, NSOPERATIONQUEUE_META = "NSOperationQueue", || {
    let _ = NSOperationQueueImpl::class();
    crate::perform::install();
});

// ---------------------------------------------------------------------------
// NSOperation

const READY: u8 = 0;
const EXECUTING: u8 = 1;
const FINISHED: u8 = 2;

pub(crate) struct OperationIvars {
    state: AtomicU8,
    cancelled: AtomicBool,
    name: Mutex<Option<Retained<NSString>>>,
    completion: Mutex<Option<RcBlock<dyn Fn()>>>,
    qos: AtomicIsize,
    priority: AtomicIsize,
    finished: (Mutex<bool>, Condvar),
    /// Operations that must finish first, held strongly as on macOS.
    dependencies: Mutex<Vec<Retained<AnyObject>>>,
}

// SAFETY: everything mutable is behind locks and atomics; the completion
// block runs on a pool thread, as on macOS.
unsafe impl Send for OperationIvars {}
unsafe impl Sync for OperationIvars {}

impl OperationIvars {
    fn new() -> Self {
        OperationIvars {
            state: AtomicU8::new(READY),
            cancelled: AtomicBool::new(false),
            name: Mutex::new(None),
            completion: Mutex::new(None),
            qos: AtomicIsize::new(NSQualityOfService::Default.0),
            priority: AtomicIsize::new(NSOperationQueuePriority::Normal.0),
            finished: (Mutex::new(false), Condvar::new()),
            dependencies: Mutex::new(Vec::new()),
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSOperation"]
    #[ivars = OperationIvars]
    pub(crate) struct NSOperationImpl;

    impl NSOperationImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(OperationIvars::new());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        /// Run `-main` here and now, unless cancelled, then finish.
        #[unsafe(method(start))]
        fn start(&self) {
            let ivars = self.ivars();
            assert!(
                ivars.state.compare_exchange(READY, EXECUTING, Ordering::AcqRel, Ordering::Acquire).is_ok(),
                "sidestep: -[NSOperation start]: the operation already started"
            );
            struct Finish<'a>(&'a NSOperationImpl);
            impl Drop for Finish<'_> {
                fn drop(&mut self) {
                    self.0.finish();
                }
            }
            let _finish = Finish(self);
            if !ivars.cancelled.load(Ordering::Acquire) {
                // SAFETY: -main takes nothing; subclasses override it.
                let _: () = unsafe { msg_send![self, main] };
            }
        }

        #[unsafe(method(main))]
        fn main(&self) {}

        /// A cancelled operation is ready whatever it depends on, so a
        /// queue waiting on its dependencies looks again.
        #[unsafe(method(cancel))]
        fn cancel(&self) {
            if !self.ivars().cancelled.swap(true, Ordering::AcqRel) {
                dependency_finished();
            }
        }

        #[unsafe(method(isCancelled))]
        fn is_cancelled(&self) -> bool {
            self.ivars().cancelled.load(Ordering::Acquire)
        }

        #[unsafe(method(isExecuting))]
        fn is_executing(&self) -> bool {
            self.ivars().state.load(Ordering::Acquire) == EXECUTING
        }

        #[unsafe(method(isFinished))]
        fn is_finished(&self) -> bool {
            self.ivars().state.load(Ordering::Acquire) == FINISHED
        }

        #[unsafe(method(isReady))]
        fn is_ready(&self) -> bool {
            self.ready()
        }

        #[unsafe(method(addDependency:))]
        fn add_dependency(&self, operation: &NSOperation) {
            let operation: &AnyObject = operation.as_ref();
            let mut dependencies = lock(&self.ivars().dependencies);
            if !dependencies.iter().any(|d| std::ptr::eq(&**d, operation)) {
                dependencies.push(operation.retain());
            }
        }

        #[unsafe(method(removeDependency:))]
        fn remove_dependency(&self, operation: &NSOperation) {
            let operation: &AnyObject = operation.as_ref();
            lock(&self.ivars().dependencies).retain(|d| !std::ptr::eq(&**d, operation));
            // Something waiting on this one may be ready now.
            dependency_finished();
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(dependencies))]
        fn dependencies(&self) -> Retained<AnyObject> {
            let dependencies = lock(&self.ivars().dependencies).clone();
            objc2_foundation::NSArray::from_retained_slice(&dependencies).into()
        }

        #[unsafe(method(isConcurrent))]
        fn is_concurrent(&self) -> bool {
            false
        }

        #[unsafe(method(isAsynchronous))]
        fn is_asynchronous(&self) -> bool {
            false
        }

        #[unsafe(method(waitUntilFinished))]
        fn wait_until_finished(&self) {
            let (lock_, cv) = &self.ivars().finished;
            let mut finished = lock(lock_);
            while !*finished {
                finished = cv.wait(finished).unwrap_or_else(|e| e.into_inner());
            }
        }

        /// Retained and autoreleased, as an atomic property's getter
        /// returns it: the block stays alive until the caller's pool
        /// drains, even if it is replaced or the operation finishes.
        #[unsafe(method(completionBlock))]
        fn completion_block(&self) -> *mut DynBlock<dyn Fn()> {
            let Some(block) = lock(&self.ivars().completion).clone() else { return std::ptr::null_mut() };
            // SAFETY: a block is an object; the autorelease pool takes over
            // the reference `into_raw` gives up.
            unsafe { objc2::ffi::objc_autoreleaseReturnValue(RcBlock::into_raw(block).cast()).cast() }
        }

        #[unsafe(method(setCompletionBlock:))]
        fn set_completion_block(&self, block: Option<&DynBlock<dyn Fn()>>) {
            let old = std::mem::replace(&mut *lock(&self.ivars().completion), block.map(|b| b.copy()));
            drop(old);
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            lock(&self.ivars().name).clone()
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            let old = std::mem::replace(&mut *lock(&self.ivars().name), name.map(|n| n.copy()));
            drop(old);
        }

        #[unsafe(method(qualityOfService))]
        fn quality_of_service(&self) -> NSQualityOfService {
            NSQualityOfService(self.ivars().qos.load(Ordering::Relaxed))
        }

        #[unsafe(method(setQualityOfService:))]
        fn set_quality_of_service(&self, qos: NSQualityOfService) {
            self.ivars().qos.store(qos.0, Ordering::Relaxed);
        }

        #[unsafe(method(queuePriority))]
        fn queue_priority(&self) -> NSOperationQueuePriority {
            NSOperationQueuePriority(self.ivars().priority.load(Ordering::Relaxed))
        }

        #[unsafe(method(setQueuePriority:))]
        fn set_queue_priority(&self, priority: NSOperationQueuePriority) {
            self.ivars().priority.store(priority.0, Ordering::Relaxed);
        }
    }

    unsafe impl NSObjectProtocol for NSOperationImpl {}
);

impl NSOperationImpl {
    /// Cancelled, or every dependency finished.
    fn ready(&self) -> bool {
        if self.ivars().cancelled.load(Ordering::Acquire) {
            return true;
        }
        let dependencies = lock(&self.ivars().dependencies).clone();
        dependencies.iter().all(|op| {
            // SAFETY: operations answer -isFinished.
            unsafe { msg_send![&**op, isFinished] }
        })
    }

    /// Mark finished, wake waiters and hand the completion block to the
    /// pool.
    fn finish(&self) {
        let ivars = self.ivars();
        ivars.state.store(FINISHED, Ordering::Release);
        let (lock_, cv) = &ivars.finished;
        *lock(lock_) = true;
        cv.notify_all();
        dependency_finished();
        if let Some(block) = lock(&ivars.completion).take() {
            struct SendBlock(RcBlock<dyn Fn()>);
            // SAFETY: completion blocks run on a pool thread, as on macOS.
            unsafe impl Send for SendBlock {}
            let block = SendBlock(block);
            crate::dispatch::global_async(Work::boxed(move || {
                let block = block;
                block.0.call(());
            }));
        }
    }
}

pub(crate) struct BlockOperationIvars {
    blocks: Mutex<Vec<RcBlock<dyn Fn()>>>,
}

// SAFETY: the blocks are behind a lock and run where the operation runs.
unsafe impl Send for BlockOperationIvars {}
unsafe impl Sync for BlockOperationIvars {}

define_class!(
    #[unsafe(super(NSOperation, NSObject))]
    #[name = "NSBlockOperation"]
    #[ivars = BlockOperationIvars]
    pub(crate) struct NSBlockOperationImpl;

    impl NSBlockOperationImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(BlockOperationIvars { blocks: Mutex::new(Vec::new()) });
            // SAFETY: NSOperation's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(blockOperationWithBlock:))]
        fn block_operation(block: &DynBlock<dyn Fn()>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(BlockOperationIvars { blocks: Mutex::new(vec![block.copy()]) });
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(addExecutionBlock:))]
        fn add_execution_block(&self, block: &DynBlock<dyn Fn()>) {
            lock(&self.ivars().blocks).push(block.copy());
        }

        /// Runs the first block here and the others on the pool meanwhile,
        /// as macOS does, and returns when all have run.
        #[unsafe(method(main))]
        fn main(&self) {
            let blocks = lock(&self.ivars().blocks).clone();
            let Some((first, rest)) = blocks.split_first() else { return };
            if rest.is_empty() {
                first.call(());
                return;
            }
            let left = Arc::new((Mutex::new(rest.len()), Condvar::new()));
            for block in rest {
                struct Job(RcBlock<dyn Fn()>, Arc<(Mutex<usize>, Condvar)>);
                // SAFETY: execution blocks run on pool threads, as on macOS.
                unsafe impl Send for Job {}
                let job = Job(block.clone(), left.clone());
                crate::dispatch::global_async(Work::boxed(move || {
                    let job = job;
                    // Counted even if the block unwinds, so -main returns.
                    struct Done(Arc<(Mutex<usize>, Condvar)>);
                    impl Drop for Done {
                        fn drop(&mut self) {
                            *lock(&self.0.0) -= 1;
                            self.0.1.notify_all();
                        }
                    }
                    let _done = Done(job.1);
                    job.0.call(());
                }));
            }
            first.call(());
            let mut count = lock(&left.0);
            while *count > 0 {
                count = left.1.wait(count).unwrap_or_else(|e| e.into_inner());
            }
        }
    }

    unsafe impl NSObjectProtocol for NSBlockOperationImpl {}
);

// ---------------------------------------------------------------------------
// NSOperationQueue

/// One queued operation: a block, or an operation object.
enum Operation {
    Block(RcBlock<dyn Fn()>),
    Object(Retained<AnyObject>),
}

// SAFETY: an operation is handed to the thread that runs it; blocks and
// objects are reference counted atomically.
unsafe impl Send for Operation {}

impl Operation {
    fn is_ready(&self) -> bool {
        match self {
            Operation::Block(_) => true,
            // SAFETY: operation objects answer -isReady.
            Operation::Object(op) => unsafe { msg_send![&**op, isReady] },
        }
    }

    fn run(self) {
        match self {
            Operation::Block(block) => block.call(()),
            // SAFETY: operation objects answer -start.
            Operation::Object(op) => unsafe {
                let _: () = msg_send![&*op, start];
            },
        }
    }
}

#[derive(Default)]
struct Pending {
    queue: VecDeque<Operation>,
    /// Operations handed to the dispatch queues and not finished yet.
    running: usize,
}

pub(crate) struct QueueIvars {
    main: bool,
    name: Mutex<Option<Retained<NSString>>>,
    max_concurrent: AtomicIsize,
    suspended: AtomicBool,
    pending: Mutex<Pending>,
    /// Signalled whenever the queue empties.
    idle: Condvar,
}

// SAFETY: the name is an immutable string, only replaced under the lock.
unsafe impl Send for QueueIvars {}
unsafe impl Sync for QueueIvars {}

impl QueueIvars {
    fn new(main: bool, name: Option<Retained<NSString>>) -> Self {
        QueueIvars {
            main,
            name: Mutex::new(name),
            max_concurrent: AtomicIsize::new(if main { 1 } else { -1 }),
            suspended: AtomicBool::new(false),
            pending: Mutex::new(Pending::default()),
            idle: Condvar::new(),
        }
    }
}

thread_local! {
    /// The queue whose operation this thread is running.
    static CURRENT: Cell<*const NSOperationQueueImpl> = const { Cell::new(std::ptr::null()) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSOperationQueue"]
    #[ivars = QueueIvars]
    pub(crate) struct NSOperationQueueImpl;

    impl NSOperationQueueImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(QueueIvars::new(false, None));
            // SAFETY: NSObject's designated initializer.
            let queue: Retained<Self> = unsafe { msg_send![super(this), init] };
            let name = NSString::from_str(&format!("NSOperationQueue {:p}", &*queue));
            *lock(&queue.ivars().name) = Some(name);
            queue
        }

        #[unsafe(method_id(mainQueue))]
        fn main_queue() -> Retained<Self> {
            main_queue_impl()
        }

        #[unsafe(method_id(currentQueue))]
        fn current_queue() -> Option<Retained<Self>> {
            current_impl()
        }

        #[unsafe(method(addOperationWithBlock:))]
        fn add_operation_with_block(&self, block: &DynBlock<dyn Fn()>) {
            self.enqueue(Operation::Block(block.copy()));
        }

        #[unsafe(method(addOperation:))]
        fn add_operation(&self, operation: &NSOperation) {
            self.enqueue(Operation::Object(operation.retain().into_super().into()));
        }

        #[unsafe(method(addOperations:waitUntilFinished:))]
        fn add_operations(&self, operations: &AnyObject, wait: bool) {
            // SAFETY: the argument is an array of operations.
            let count: usize = unsafe { msg_send![operations, count] };
            for i in 0..count {
                // SAFETY: as above.
                let op: Retained<AnyObject> = unsafe { msg_send![operations, objectAtIndex: i] };
                self.enqueue(Operation::Object(op));
            }
            if wait {
                self.wait_until_idle();
            }
        }

        #[unsafe(method(operationCount))]
        fn operation_count(&self) -> NSUInteger {
            let pending = lock(&self.ivars().pending);
            pending.queue.len() + pending.running
        }

        #[unsafe(method(cancelAllOperations))]
        fn cancel_all_operations(&self) {
            // Cancelled outside the lock: cancelling pumps waiting queues,
            // this one among them.
            let (objects, blocks): (Vec<Retained<AnyObject>>, Vec<Operation>) = {
                let mut pending = lock(&self.ivars().pending);
                let (kept, gone): (Vec<_>, Vec<_>) =
                    pending.queue.drain(..).partition(|op| matches!(op, Operation::Object(_)));
                pending.queue = kept.into();
                let objects = pending
                    .queue
                    .iter()
                    .filter_map(|op| match op {
                        Operation::Object(op) => Some(op.clone()),
                        Operation::Block(_) => None,
                    })
                    .collect();
                (objects, gone)
            };
            drop(blocks);
            for op in &objects {
                // SAFETY: operation objects answer -cancel.
                let _: () = unsafe { msg_send![&**op, cancel] };
            }
            self.pump();
            self.signal_if_idle();
        }

        #[unsafe(method(waitUntilAllOperationsAreFinished))]
        fn wait_until_all(&self) {
            self.wait_until_idle();
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            lock(&self.ivars().name).clone()
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            let old = std::mem::replace(&mut *lock(&self.ivars().name), name.map(|n| n.copy()));
            drop(old);
        }

        #[unsafe(method(maxConcurrentOperationCount))]
        fn max_concurrent(&self) -> NSInteger {
            self.ivars().max_concurrent.load(Ordering::Relaxed)
        }

        #[unsafe(method(setMaxConcurrentOperationCount:))]
        fn set_max_concurrent(&self, count: NSInteger) {
            if !self.ivars().main {
                self.ivars().max_concurrent.store(count, Ordering::Relaxed);
                self.pump();
            }
        }

        #[unsafe(method(isSuspended))]
        fn is_suspended(&self) -> bool {
            self.ivars().suspended.load(Ordering::Acquire)
        }

        #[unsafe(method(setSuspended:))]
        fn set_suspended(&self, suspended: bool) {
            self.ivars().suspended.store(suspended, Ordering::Release);
            if !suspended {
                self.pump();
            }
        }
    }

    unsafe impl NSObjectProtocol for NSOperationQueueImpl {}
);

impl NSOperationQueueImpl {
    fn enqueue(&self, op: Operation) {
        lock(&self.ivars().pending).queue.push_back(op);
        self.pump();
    }

    /// How many operations may run at once.
    fn width(&self) -> usize {
        match self.ivars().max_concurrent.load(Ordering::Relaxed) {
            n if n > 0 => n as usize,
            0 => 0,
            _ => std::thread::available_parallelism().map_or(4, |n| n.get()),
        }
    }

    /// Start operations while there is room and the queue isn't
    /// suspended: on the main dispatch queue for the main queue, on the pool
    /// for others.
    ///
    /// When none of the waiting operations is ready, the queue registers
    /// to be pumped again and then looks once more: an operation finishes
    /// by marking itself finished before pumping the registered queues, so
    /// either that second look sees it or its finish finds the queue.
    fn pump(&self) {
        struct Queue(Retained<NSOperationQueueImpl>);
        // SAFETY: the queue's state is behind locks and atomics.
        unsafe impl Send for Queue {}
        let mut registered = false;
        loop {
            if self.ivars().suspended.load(Ordering::Acquire) {
                return;
            }
            let op = {
                let mut pending = lock(&self.ivars().pending);
                if pending.running >= self.width() {
                    return;
                }
                let Some(at) = pending.queue.iter().position(Operation::is_ready) else {
                    if pending.queue.is_empty() || registered {
                        return;
                    }
                    drop(pending);
                    wait_for_dependencies(self);
                    registered = true;
                    continue;
                };
                let op = pending.queue.remove(at).expect("an operation");
                pending.running += 1;
                op
            };
            let queue = Queue(self.retain());
            let job = Work::boxed(move || {
                let queue = queue;
                queue.0.run(op);
            });
            if self.ivars().main {
                main_queue_push(job);
            } else {
                crate::dispatch::global_async(job);
            }
        }
    }

    /// Run one operation on this thread as the queue's, then start more.
    fn run(&self, op: Operation) {
        {
            let outer = CURRENT.with(|c| c.replace(self));
            struct Restore(*const NSOperationQueueImpl);
            impl Drop for Restore {
                fn drop(&mut self) {
                    CURRENT.with(|c| c.set(self.0));
                }
            }
            let _restore = Restore(outer);
            op.run();
        }
        lock(&self.ivars().pending).running -= 1;
        self.pump();
        self.signal_if_idle();
    }

    fn signal_if_idle(&self) {
        let pending = lock(&self.ivars().pending);
        if pending.running == 0 && pending.queue.is_empty() {
            self.ivars().idle.notify_all();
        }
    }

    fn wait_until_idle(&self) {
        let mut pending = lock(&self.ivars().pending);
        while pending.running > 0 || !pending.queue.is_empty() {
            pending = self.ivars().idle.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn is_current(&self) -> bool {
        if self.ivars().main && is_main_thread() {
            return true;
        }
        CURRENT.with(|c| std::ptr::eq(c.get(), self))
    }
}

/// A queue with operations waiting on others.
struct Waiting(Retained<NSOperationQueueImpl>);

// SAFETY: queues' state is behind locks and atomics.
unsafe impl Send for Waiting {}

static WAITING: Mutex<Vec<Waiting>> = Mutex::new(Vec::new());

/// Remember a queue whose next operations wait on dependencies.
fn wait_for_dependencies(queue: &NSOperationQueueImpl) {
    let mut waiting = lock(&WAITING);
    if !waiting.iter().any(|w| std::ptr::eq(&*w.0, queue)) {
        waiting.push(Waiting(queue.retain()));
    }
}

/// An operation finished (or lost a dependency): let waiting queues look
/// again.
fn dependency_finished() {
    let waiting = std::mem::take(&mut *lock(&WAITING));
    for queue in waiting {
        queue.0.pump();
    }
}

struct MainQueue(*const NSOperationQueueImpl);

// SAFETY: the main queue is never released; its state is behind locks and
// atomics.
unsafe impl Send for MainQueue {}
unsafe impl Sync for MainQueue {}

static MAIN: OnceLock<MainQueue> = OnceLock::new();

fn main_queue_impl() -> Retained<NSOperationQueueImpl> {
    let main = MAIN.get_or_init(|| {
        // SAFETY: +class loads the class the shell names.
        let _: *const objc2::runtime::AnyClass = unsafe { msg_send![NSOperationQueue::class(), class] };
        let name = NSString::from_str("NSOperationQueue Main Queue");
        let this = NSOperationQueueImpl::alloc().set_ivars(QueueIvars::new(true, Some(name)));
        // SAFETY: NSObject's designated initializer.
        let queue: Retained<NSOperationQueueImpl> = unsafe { msg_send![super(this), init] };
        MainQueue(Retained::into_raw(queue))
    });
    // SAFETY: the main queue is immortal.
    unsafe { Retained::retain(main.0.cast_mut()) }.expect("the main queue exists")
}

fn current_impl() -> Option<Retained<NSOperationQueueImpl>> {
    let running = CURRENT.with(|c| c.get());
    // SAFETY: a queue running an operation is alive for its duration.
    if let Some(queue) = unsafe { running.as_ref() } {
        return Some(queue.retain());
    }
    is_main_thread().then(main_queue_impl)
}

/// Call a notification observer's `block` with `note` on `queue`, and wait
/// until it has run: posting is synchronous whatever queue an observer
/// asked for. On the queue's own thread the block runs at once.
pub(crate) fn run_and_wait(queue: &AnyObject, block: &RcBlock<dyn Fn(NonNull<NSNotification>)>, note: &NSNotification) {
    let Some(queue) = queue.downcast_ref::<NSOperationQueue>() else {
        block.call((NonNull::from(note),));
        return;
    };
    // SAFETY: every NSOperationQueue is an instance of this class.
    let queue = unsafe { &*(queue as *const NSOperationQueue).cast::<NSOperationQueueImpl>() };
    if queue.is_current() {
        block.call((NonNull::from(note),));
        return;
    }
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let signal = done.clone();
    let (block, note) = (block.clone(), note.retain());
    let op = RcBlock::new(move || {
        block.call((NonNull::from(&*note),));
        *lock(&signal.0) = true;
        signal.1.notify_all();
    });
    queue.enqueue(Operation::Block(op.copy()));
    let mut finished = lock(&done.0);
    while !*finished {
        finished = done.1.wait(finished).unwrap_or_else(|e| e.into_inner());
    }
}
