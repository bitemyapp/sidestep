//! Selector observations, string object filters, suspension, and process delivery.
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::{
    NSDate, NSDistributedNotificationCenter, NSNotification, NSNotificationSuspensionBehavior, NSRunLoop, NSString,
};
use sidestep as _;
use std::cell::Cell;
use std::time::{Duration, Instant};
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepDistributedObserverTest"]
    #[ivars = Cell<usize>]
    struct Observer;
    impl Observer {
        #[unsafe(method(received:))]
        fn received(&self, _note: &NSNotification) { self.ivars().set(self.ivars().get()+1); }
    }
    unsafe impl NSObjectProtocol for Observer {}
);
fn pump(duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.01));
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--post") {
        let center = NSDistributedNotificationCenter::defaultCenter();
        for _ in 0..3 {
            unsafe {
                center.postNotificationName_object_userInfo_deliverImmediately(
                    &NSString::from_str(&args[2]),
                    Some(&NSString::from_str("same value")),
                    None,
                    true,
                );
                pump(Duration::from_millis(50));
            }
        }
        return;
    }
    objc2::rc::autoreleasepool(|_| {
        let center = NSDistributedNotificationCenter::defaultCenter();
        let observer = Observer::alloc().set_ivars(Cell::new(0));
        let observer: Retained<Observer> = unsafe { msg_send![super(observer), init] };
        let name = NSString::from_str(&format!("sidestep.conformance.{}", std::process::id()));
        unsafe {
            center.addObserver_selector_name_object_suspensionBehavior(
                &observer,
                sel!(received:),
                Some(&name),
                Some(&NSString::from_str("same value")),
                NSNotificationSuspensionBehavior::Coalesce,
            );
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--post", &name.to_string()])
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while observer.ivars().get() == 0 && Instant::now() < end {
            pump(Duration::from_millis(20));
        }
        assert!(child.wait().unwrap().success());
        pump(Duration::from_millis(100));
        assert!(observer.ivars().get() > 0, "cross-process notification was not delivered");
        observer.ivars().set(0);
        center.setSuspended(true);
        for _ in 0..3 {
            unsafe {
                center.postNotificationName_object_userInfo_deliverImmediately(
                    &name,
                    Some(&NSString::from_str("same value")),
                    None,
                    false,
                );
            }
        }
        pump(Duration::from_millis(100));
        assert_eq!(observer.ivars().get(), 0);
        center.setSuspended(false);
        pump(Duration::from_millis(100));
        assert_eq!(observer.ivars().get(), 1);
        unsafe {
            center.removeObserver(&observer);
        }
        observer.ivars().set(0);
        unsafe {
            center.postNotificationName_object_userInfo_deliverImmediately(
                &name,
                Some(&NSString::from_str("same value")),
                None,
                true,
            );
        }
        pump(Duration::from_millis(100));
        assert_eq!(observer.ivars().get(), 0);
        println!("test distributed notification processes, coalescing and removal ... ok");
    });
}
