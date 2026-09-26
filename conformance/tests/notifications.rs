//! `NSNotificationCenter` and `NSNotification`, checked on macOS and on
//! Linux alike. Observers on the main operation queue need the main
//! thread's run loop, so this file has its own `main`.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, sel};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSDictionary, NSNotification, NSNotificationCenter, NSOperationQueue, NSRunLoop,
    NSString, NSTimer,
};

use sidestep as _;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// What an observer does besides logging, for mutations during a post.
enum Also {
    Nothing,
    Remove(Retained<Observer>),
    Add(Retained<Observer>),
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NotificationsTestObserver"]
    #[ivars = (String, RefCell<Also>)]
    struct Observer;

    impl Observer {
        #[unsafe(method(note:))]
        fn note(&self, note: &NSNotification) {
            log(format!("{}:{}", self.ivars().0, note.name()));
            let center = NSNotificationCenter::defaultCenter();
            match &*self.ivars().1.borrow() {
                Also::Nothing => {}
                Also::Remove(other) => unsafe { center.removeObserver(other) },
                Also::Add(other) => unsafe {
                    center.addObserver_selector_name_object(other, sel!(note:), Some(&note.name()), None)
                },
            }
        }

        #[unsafe(method(other:))]
        fn other(&self, note: &NSNotification) {
            log(format!("{}-other:{}", self.ivars().0, note.name()));
        }

        #[unsafe(method(thread:))]
        fn thread(&self, _note: &NSNotification) {
            log(format!("main={}", MainThreadMarker::new().is_some()));
        }
    }
);

fn observer(tag: &str) -> Retained<Observer> {
    observer_doing(tag, Also::Nothing)
}

fn observer_doing(tag: &str, also: Also) -> Retained<Observer> {
    let this = Observer::alloc().set_ivars((tag.to_string(), RefCell::new(also)));
    unsafe { msg_send![super(this), init] }
}

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn retain_count(object: &AnyObject) -> usize {
    unsafe { msg_send![object, retainCount] }
}

fn center() -> Retained<NSNotificationCenter> {
    NSNotificationCenter::defaultCenter()
}

fn add(
    center: &NSNotificationCenter,
    observer: &Observer,
    selector: objc2::runtime::Sel,
    name: Option<&str>,
    object: Option<&AnyObject>,
) {
    let name = name.map(s);
    unsafe { center.addObserver_selector_name_object(observer, selector, name.as_deref(), object) };
}

fn post(name: &str, object: Option<&AnyObject>) {
    unsafe { center().postNotificationName_object(&s(name), object) };
}

type Token = Retained<ProtocolObject<dyn NSObjectProtocol>>;

fn block_observer(
    center: &NSNotificationCenter,
    name: Option<&str>,
    queue: Option<&NSOperationQueue>,
    tag: &'static str,
) -> Token {
    let block =
        RcBlock::new(move |note: NonNull<NSNotification>| log(format!("{tag}:{}", unsafe { note.as_ref() }.name())));
    let name = name.map(s);
    unsafe { center.addObserverForName_object_queue_usingBlock(name.as_deref(), None, queue, &block) }
}

fn remove(observer: &AnyObject) {
    unsafe { center().removeObserver(observer) };
}

fn run_main_loop(seconds: f64) {
    NSRunLoop::currentRunLoop()
        .runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &NSDate::dateWithTimeIntervalSinceNow(seconds));
}

// ---------------------------------------------------------------------------

fn default_center_is_one_object() {
    assert!(std::ptr::eq(&*center(), &*center()));
    let other = NSNotificationCenter::new();
    assert!(!std::ptr::eq(&*other, &*center()));
}

fn observers_are_not_retained() {
    let a = observer("a");
    let before = retain_count(&a);
    add(&center(), &a, sel!(note:), Some("Unretained"), None);
    assert_eq!(retain_count(&a), before);
    post("Unretained", None);
    assert_eq!(take_log(), ["a:Unretained"]);
    remove(&a);

    // An observer that goes away without unregistering is no longer called.
    {
        let gone = observer("gone");
        add(&center(), &gone, sel!(note:), Some("Gone"), None);
    }
    post("Gone", None);
    assert!(take_log().is_empty());
}

fn delivery_in_registration_order() {
    let nc = center();
    let (o1, o2, o3) = (observer("o1"), observer("o2"), observer("o3"));
    let sender = NSObject::new();
    add(&nc, &o2, sel!(note:), Some("Order"), None);
    let t1 = block_observer(&nc, Some("Order"), None, "block1");
    add(&nc, &o1, sel!(note:), Some("Order"), None);
    add(&nc, &o3, sel!(note:), None, None);
    let t2 = block_observer(&nc, Some("Order"), None, "block2");
    add(&nc, &o1, sel!(other:), Some("Order"), Some(&sender));
    add(&nc, &o2, sel!(other:), None, Some(&sender));
    post("Order", Some(&sender));
    assert_eq!(
        take_log(),
        ["o2:Order", "block1:Order", "o1:Order", "o3:Order", "block2:Order", "o1-other:Order", "o2-other:Order"]
    );
    post("Order", None);
    assert_eq!(take_log(), ["o2:Order", "block1:Order", "o1:Order", "o3:Order", "block2:Order"]);
    for o in [&o1, &o2, &o3] {
        remove(o);
    }
    remove(t1.as_ref());
    remove(t2.as_ref());
    post("Order", Some(&sender));
    assert!(take_log().is_empty());
}

fn duplicates_and_filters() {
    let nc = center();
    let d = observer("d");
    add(&nc, &d, sel!(note:), Some("Twice"), None);
    add(&nc, &d, sel!(note:), Some("Twice"), None);
    post("Twice", None);
    assert_eq!(take_log(), ["d:Twice", "d:Twice"]);
    unsafe { nc.removeObserver_name_object(&d, Some(&s("Twice")), None) };
    post("Twice", None);
    assert!(take_log().is_empty());

    // Names match by string equality, objects by identity, and objects
    // aren't retained.
    let e = observer("e");
    let text = "a sender long enough not to be a tagged pointer";
    let (sender, equal_sender) = (s(text), s(text));
    let sender_count = retain_count(&sender);
    let name = "a name long enough not to be a tagged pointer";
    add(&nc, &e, sel!(note:), Some(name), Some(&sender));
    assert_eq!(retain_count(&sender), sender_count);
    post(name, Some(&sender));
    post(name, Some(&equal_sender));
    post(name, None);
    assert_eq!(take_log(), [format!("e:{name}")]);
    remove(&e);
}

fn mutation_during_a_post() {
    let nc = center();
    // Removed by an earlier observer of the same post: not called.
    let victim = observer("victim");
    let remover = observer_doing("remover", Also::Remove(victim.clone()));
    add(&nc, &remover, sel!(note:), Some("Mutate"), None);
    add(&nc, &victim, sel!(note:), Some("Mutate"), None);
    post("Mutate", None);
    assert_eq!(take_log(), ["remover:Mutate"]);
    remove(&remover);

    // Added during a post: called from the next one on.
    let added = observer("added");
    let adder = observer_doing("adder", Also::Add(added.clone()));
    add(&nc, &adder, sel!(note:), Some("Grow"), None);
    post("Grow", None);
    assert_eq!(take_log(), ["adder:Grow"]);
    remove(&adder);
    post("Grow", None);
    assert_eq!(take_log(), ["added:Grow"]);
    remove(&added);
}

fn removing_with_filters() {
    let nc = center();
    let w = observer("w");
    let (a, b) = (NSObject::new(), NSObject::new());
    add(&nc, &w, sel!(note:), Some("W1"), Some(&a));
    add(&nc, &w, sel!(note:), Some("W1"), Some(&b));
    add(&nc, &w, sel!(note:), Some("W2"), Some(&a));
    add(&nc, &w, sel!(note:), None, Some(&a));
    add(&nc, &w, sel!(note:), Some("W1"), None);
    // Everything registered for object a goes, whatever its name.
    unsafe { nc.removeObserver_name_object(&w, None, Some(&a)) };
    post("W1", Some(&a));
    assert_eq!(take_log(), ["w:W1"]);
    post("W1", Some(&b));
    assert_eq!(take_log(), ["w:W1", "w:W1"]);
    post("W2", Some(&a));
    assert!(take_log().is_empty());
    // Everything named W1 goes, whatever its object.
    unsafe { nc.removeObserver_name_object(&w, Some(&s("W1")), None) };
    post("W1", Some(&b));
    assert!(take_log().is_empty());
    // A registration for any name isn't one for a particular name.
    add(&nc, &w, sel!(note:), None, None);
    unsafe { nc.removeObserver_name_object(&w, Some(&s("W1")), None) };
    post("W1", None);
    assert_eq!(take_log(), ["w:W1"]);
    remove(&w);
}

fn delivery_on_the_posting_thread() {
    let th = observer("th");
    add(&center(), &th, sel!(thread:), Some("Thread"), None);
    let token = {
        let block =
            RcBlock::new(|_: NonNull<NSNotification>| log(format!("block main={}", MainThreadMarker::new().is_some())));
        unsafe { center().addObserverForName_object_queue_usingBlock(Some(&s("Thread")), None, None, &block) }
    };
    let seen = std::thread::spawn(|| {
        post("Thread", None);
        take_log()
    })
    .join()
    .unwrap();
    assert_eq!(seen, ["main=false", "block main=false"]);
    remove(&th);
    remove(token.as_ref());
}

fn block_observers_on_the_main_queue() {
    let main_queue = NSOperationQueue::mainQueue();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let r = ran.clone();
    let block = RcBlock::new(move |_: NonNull<NSNotification>| {
        r.lock().unwrap().push(MainThreadMarker::new().is_some());
    });
    let token = unsafe {
        center().addObserverForName_object_queue_usingBlock(Some(&s("MainQ")), None, Some(&main_queue), &block)
    };

    // Posted on the main thread: runs before the post returns.
    post("MainQ", None);
    assert_eq!(*ran.lock().unwrap(), [true]);

    // Posted elsewhere: the post waits until the main thread has run it.
    let (r, returned) = (ran.clone(), Arc::new(Mutex::new(None)));
    let ret = returned.clone();
    let poster = std::thread::spawn(move || {
        post("MainQ", None);
        *ret.lock().unwrap() = Some(r.lock().unwrap().len());
    });
    let start = Instant::now();
    while returned.lock().unwrap().is_none() && start.elapsed() < Duration::from_secs(5) {
        run_main_loop(0.05);
    }
    poster.join().unwrap();
    assert_eq!(*returned.lock().unwrap(), Some(2), "the post returned after the block ran");
    assert_eq!(*ran.lock().unwrap(), [true, true]);

    // The main queue runs in the common modes only.
    let r = ran.clone();
    let poster = std::thread::spawn(move || {
        post("MainQ", None);
        r.lock().unwrap().len()
    });
    let custom = s("NotificationsCustomMode");
    let keep = unsafe { NSTimer::timerWithTimeInterval_repeats_block(100.0, false, &RcBlock::new(|_| {})) };
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&keep, &custom) };
    NSRunLoop::currentRunLoop().runMode_beforeDate(&custom, &NSDate::dateWithTimeIntervalSinceNow(0.2));
    assert_eq!(ran.lock().unwrap().len(), 2);
    let start = Instant::now();
    while !poster.is_finished() && start.elapsed() < Duration::from_secs(5) {
        run_main_loop(0.05);
    }
    assert_eq!(poster.join().unwrap(), 3);
    keep.invalidate();
    remove(token.as_ref());
}

fn tokens_keep_their_blocks_until_removed() {
    let captured = NSObject::new();
    let before = retain_count(&captured);
    // The token may pass through an autorelease pool on its way back.
    let weak_token = objc2::rc::autoreleasepool(|_| {
        let c = captured.clone();
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            let _ = &c;
            log("token".into());
        });
        let token = unsafe { center().addObserverForName_object_queue_usingBlock(Some(&s("Tok")), None, None, &block) };
        Weak::from_retained(&token)
    });
    // The center keeps the token, and the token the block.
    assert!(weak_token.load().is_some());
    assert_eq!(retain_count(&captured), before + 1);
    post("Tok", None);
    assert_eq!(take_log(), ["token"]);
    objc2::rc::autoreleasepool(|_| {
        let token = weak_token.load().unwrap();
        // A token goes whatever the name and object given.
        unsafe { center().removeObserver_name_object(token.as_ref(), Some(&s("SomethingElse")), None) };
    });
    assert!(weak_token.load().is_none());
    assert_eq!(retain_count(&captured), before);
    post("Tok", None);
    assert!(take_log().is_empty());

    // A block observer for any name.
    let token = block_observer(&center(), None, None, "any");
    post("Anything", None);
    assert_eq!(take_log(), ["any:Anything"]);
    remove(token.as_ref());
}

fn notifications() {
    let name = s("Note");
    let note = unsafe { NSNotification::notificationWithName_object(&name, None) };
    assert!(note.userInfo().is_none());
    assert!(note.object().is_none());
    assert_eq!(note.name().to_string(), "Note");
    let copy: Retained<NSNotification> = unsafe { msg_send![&*note, copy] };
    assert!(std::ptr::eq(&*copy, &*note));

    let (x, y) = (NSObject::new(), NSObject::new());
    let count = retain_count(&x);
    let with_x = unsafe { NSNotification::notificationWithName_object(&name, Some(&x)) };
    assert_eq!(retain_count(&x), count + 1, "a notification retains its object");
    assert!(std::ptr::eq(&*with_x.object().unwrap(), (*x).as_ref() as &AnyObject));
    let same = unsafe { NSNotification::notificationWithName_object(&s("Note"), Some(&x)) };
    let other_object = unsafe { NSNotification::notificationWithName_object(&name, Some(&y)) };
    let other_name = unsafe { NSNotification::notificationWithName_object(&s("Other"), Some(&x)) };
    assert!(with_x.isEqual(Some(&same)));
    assert_eq!(with_x.hash(), same.hash());
    assert_eq!(with_x.hash(), name.hash());
    assert!(!with_x.isEqual(Some(&other_object)));
    assert!(!with_x.isEqual(Some(&other_name)));

    let info = NSDictionary::<NSString, NSString>::from_slices(&[&*s("k")], &[&*s("v")]);
    let info: &NSDictionary = unsafe { &*(&*info as *const NSDictionary<NSString, NSString>).cast() };
    let with_info = unsafe { NSNotification::notificationWithName_object_userInfo(&name, Some(&x), Some(info)) };
    assert!(std::ptr::eq(&*with_info.userInfo().unwrap(), info));
    assert!(!with_x.isEqual(Some(&with_info)));
    let initialized =
        unsafe { NSNotification::initWithName_object_userInfo(NSNotification::alloc(), &name, None, Some(info)) };
    assert!(std::ptr::eq(&*initialized.userInfo().unwrap(), info));

    // postNotification: delivers the very object.
    let seen = Arc::new(Mutex::new(0usize));
    let s2 = seen.clone();
    let block = RcBlock::new(move |n: NonNull<NSNotification>| *s2.lock().unwrap() = n.as_ptr() as usize);
    let token = unsafe { center().addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block) };
    center().postNotification(&with_info);
    assert_eq!(*seen.lock().unwrap(), &*with_info as *const NSNotification as usize);
    remove(token.as_ref());
}

fn private_centers_are_separate() {
    let private = NSNotificationCenter::new();
    let p = observer("p");
    add(&private, &p, sel!(note:), Some("Private"), None);
    post("Private", None);
    assert!(take_log().is_empty());
    unsafe { private.postNotificationName_object(&s("Private"), None) };
    assert_eq!(take_log(), ["p:Private"]);
    unsafe { private.removeObserver(&p) };
}

type Test = (&'static str, fn());

fn main() {
    assert!(MainThreadMarker::new().is_some(), "runs on the main thread");
    let tests: &[Test] = &[
        ("default_center_is_one_object", default_center_is_one_object),
        ("observers_are_not_retained", observers_are_not_retained),
        ("delivery_in_registration_order", delivery_in_registration_order),
        ("duplicates_and_filters", duplicates_and_filters),
        ("mutation_during_a_post", mutation_during_a_post),
        ("removing_with_filters", removing_with_filters),
        ("delivery_on_the_posting_thread", delivery_on_the_posting_thread),
        ("block_observers_on_the_main_queue", block_observers_on_the_main_queue),
        ("tokens_keep_their_blocks_until_removed", tokens_keep_their_blocks_until_removed),
        ("notifications", notifications),
        ("private_centers_are_separate", private_centers_are_separate),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test());
        assert!(take_log().is_empty(), "{name} left log entries");
        println!("test {name} ... ok");
    }
}
