//! `NSThread`, checked on macOS and on Linux alike. The first thread start
//! of the process posts a notification, so this file has its own `main`
//! and checks that before anything else starts a thread.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, MainThreadMarker, define_class, msg_send, sel};
use objc2_foundation::{NSDate, NSNotification, NSNotificationCenter, NSQualityOfService, NSString, NSThread};

use sidestep as _;

static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn events() -> Vec<String> {
    std::mem::take(&mut *EVENTS.lock().unwrap())
}

fn record(event: String) {
    EVENTS.lock().unwrap().push(event);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "ThreadsTestTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(run:))]
        fn run(&self, argument: Option<&AnyObject>) {
            let thread = NSThread::currentThread();
            let argument = argument.map(|a| {
                let text: Retained<NSString> = unsafe { msg_send![a, description] };
                text.to_string()
            });
            record(format!("run {argument:?} main={} executing={}", NSThread::isMainThread_class(), thread.isExecuting()));
        }

        #[unsafe(method(note:))]
        fn note(&self, note: &NSNotification) {
            let object = note.object().map(|o| {
                let current: Retained<AnyObject> = NSThread::currentThread().into();
                std::ptr::eq(&*o, &*current)
            });
            record(format!("{} main={} object_is_current={object:?}", note.name(), NSThread::isMainThread_class()));
        }
    }
);

fn target() -> Retained<Target> {
    unsafe { msg_send![Target::alloc(), init] }
}

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn retain_count(object: &AnyObject) -> usize {
    unsafe { msg_send![object, retainCount] }
}

fn wait_until(what: impl Fn() -> bool) {
    let start = Instant::now();
    while !what() {
        assert!(start.elapsed() < Duration::from_secs(5), "timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ---------------------------------------------------------------------------

fn first_start_posts_will_become_multi_threaded() {
    let observer = target();
    let center = NSNotificationCenter::defaultCenter();
    unsafe {
        center.addObserver_selector_name_object(
            &observer,
            sel!(note:),
            Some(&s("NSWillBecomeMultiThreadedNotification")),
            None,
        )
    };
    let thread = unsafe { NSThread::initWithTarget_selector_object(NSThread::alloc(), &observer, sel!(run:), None) };
    thread.start();
    wait_until(|| thread.isFinished());
    assert!(NSThread::isMultiThreaded());
    let second = unsafe { NSThread::initWithTarget_selector_object(NSThread::alloc(), &observer, sel!(run:), None) };
    second.start();
    wait_until(|| second.isFinished());
    assert_eq!(
        events(),
        [
            "NSWillBecomeMultiThreadedNotification main=true object_is_current=None",
            "run None main=false executing=true",
            "run None main=false executing=true",
        ]
    );
    unsafe { center.removeObserver(&observer) };
}

fn the_main_thread() {
    let main = NSThread::mainThread();
    assert!(main.isMainThread());
    assert!(NSThread::isMainThread_class());
    assert!(std::ptr::eq(&*NSThread::currentThread(), &*main));
    assert_eq!(main.name().unwrap().to_string(), "main");
    assert!(main.isExecuting());
    assert_eq!(main.stackSize(), 512 * 1024);
    assert_eq!(main.threadPriority(), 0.5);
    let main_address = &*main as *const NSThread as usize;
    std::thread::spawn(move || {
        assert!(!NSThread::isMainThread_class());
        let from_here = NSThread::mainThread();
        assert_eq!(&*from_here as *const NSThread as usize, main_address);
        assert!(from_here.isMainThread());
        assert!(!NSThread::currentThread().isMainThread());
    })
    .join()
    .unwrap();
}

fn thread_states() {
    let block = RcBlock::new(|| std::thread::sleep(Duration::from_millis(50)));
    let thread = unsafe { NSThread::initWithBlock(NSThread::alloc(), &block) };
    assert!(!thread.isExecuting() && !thread.isFinished() && !thread.isCancelled());
    assert!(thread.name().is_none());
    assert_eq!(thread.stackSize(), 512 * 1024);
    assert_eq!(thread.qualityOfService(), NSQualityOfService::Default);
    assert!(!thread.isMainThread());
    // Names aren't cut short, whatever the kernel keeps.
    let long = "a thread name much longer than fifteen bytes";
    thread.setName(Some(&s(long)));
    thread.start();
    wait_until(|| thread.isExecuting() || thread.isFinished());
    wait_until(|| thread.isFinished());
    assert!(!thread.isExecuting());
    assert_eq!(thread.name().unwrap().to_string(), long);
    thread.cancel();
    assert!(thread.isCancelled());

    // A thread started without a name reads as the empty string, inside.
    let seen = Arc::new(Mutex::new(None));
    let s2 = seen.clone();
    let block = RcBlock::new(move || {
        let current = NSThread::currentThread();
        let again = NSThread::currentThread();
        *s2.lock().unwrap() =
            Some((current.name().map(|n| n.to_string()), std::ptr::eq(&*current, &*again), current.isMainThread()));
    });
    unsafe { NSThread::detachNewThreadWithBlock(&block) };
    wait_until(|| seen.lock().unwrap().is_some());
    assert_eq!(*seen.lock().unwrap(), Some((Some(String::new()), true, false)));
}

fn target_threads_retain_and_post_on_exit() {
    let target = target();
    let argument = s("a thread argument long enough not to be tagged");
    let counts = (retain_count(&target), retain_count(&argument));
    let center = NSNotificationCenter::defaultCenter();
    unsafe {
        center.addObserver_selector_name_object(&target, sel!(note:), Some(&s("NSThreadWillExitNotification")), None)
    };
    let thread =
        unsafe { NSThread::initWithTarget_selector_object(NSThread::alloc(), &target, sel!(run:), Some(&argument)) };
    assert_eq!((retain_count(&target), retain_count(&argument)), (counts.0 + 1, counts.1 + 1));
    thread.start();
    wait_until(|| thread.isFinished());
    assert_eq!((retain_count(&target), retain_count(&argument)), counts);
    assert_eq!(
        events(),
        [
            "run Some(\"a thread argument long enough not to be tagged\") main=false executing=true",
            "NSThreadWillExitNotification main=false object_is_current=Some(true)",
        ]
    );
    unsafe { NSThread::detachNewThreadSelector_toTarget_withObject(sel!(run:), &target, None) };
    wait_until(|| EVENTS.lock().unwrap().len() == 2);
    assert_eq!(events()[1], "NSThreadWillExitNotification main=false object_is_current=Some(true)");
    unsafe { center.removeObserver(&target) };
}

fn threads_nobody_started() {
    std::thread::spawn(|| {
        let a = NSThread::currentThread();
        assert!(std::ptr::eq(&*a, &*NSThread::currentThread()));
        assert!(!a.isMainThread());
        assert!(a.isExecuting());
        assert_eq!(a.name().unwrap().to_string(), "");
    })
    .join()
    .unwrap();
    std::thread::Builder::new()
        .name("rusty".into())
        .spawn(|| assert_eq!(NSThread::currentThread().name().unwrap().to_string(), "rusty"))
        .unwrap()
        .join()
        .unwrap();
}

fn exit_ends_the_thread() {
    let after = Arc::new(Mutex::new(false));
    let a = after.clone();
    let block = RcBlock::new(move || {
        NSThread::exit();
        *a.lock().unwrap() = true;
    });
    let thread = unsafe { NSThread::initWithBlock(NSThread::alloc(), &block) };
    thread.start();
    wait_until(|| thread.isFinished());
    assert!(!*after.lock().unwrap());
}

fn sleeping() {
    let start = Instant::now();
    NSThread::sleepForTimeInterval(0.05);
    assert!(start.elapsed() >= Duration::from_millis(50));
    let start = Instant::now();
    NSThread::sleepUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
    assert!(start.elapsed() >= Duration::from_millis(45));
    let start = Instant::now();
    NSThread::sleepForTimeInterval(-1.0);
    // Returns at once; the bound leaves room for a loaded CI runner.
    assert!(start.elapsed() < Duration::from_millis(200));
}

type Test = (&'static str, fn());

fn main() {
    assert!(MainThreadMarker::new().is_some(), "runs on the main thread");
    let tests: &[Test] = &[
        ("first_start_posts_will_become_multi_threaded", first_start_posts_will_become_multi_threaded),
        ("the_main_thread", the_main_thread),
        ("thread_states", thread_states),
        ("target_threads_retain_and_post_on_exit", target_threads_retain_and_post_on_exit),
        ("threads_nobody_started", threads_nobody_started),
        ("exit_ends_the_thread", exit_ends_the_thread),
        ("sleeping", sleeping),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test());
        assert!(events().is_empty(), "{name} left events");
        println!("test {name} ... ok");
    }
}
