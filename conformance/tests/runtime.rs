//! Runtime behavior an objc2 program relies on. Every test here must pass on
//! macOS against Apple's runtime and on Linux against Sidestep's.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use block2::{RcBlock, StackBlock};
use objc2::rc::{Allocated, Retained, Weak, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, ProtocolType, define_class, msg_send, sel};

use sidestep as _;

/// Counts drops of a counter's ivars, so tests can see deallocation.
#[derive(Default)]
struct CounterIvars {
    value: Cell<i32>,
    drops: RefCell<Option<Arc<AtomicUsize>>>,
}

impl Drop for CounterIvars {
    fn drop(&mut self) {
        if let Some(drops) = self.drops.borrow().as_ref() {
            drops.fetch_add(1, Ordering::SeqCst);
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepTestCounter"]
    #[ivars = CounterIvars]
    struct Counter;

    impl Counter {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(CounterIvars::default());
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(value))]
        fn value(&self) -> i32 {
            self.ivars().value.get()
        }

        #[unsafe(method(increment))]
        fn increment(&self) {
            self.ivars().value.set(self.ivars().value.get() + 1);
        }

        #[unsafe(method(addValue:))]
        fn add_value(&self, n: i32) -> i32 {
            let v = self.ivars().value.get() + n;
            self.ivars().value.set(v);
            v
        }

        #[unsafe(method(classAnswer))]
        fn class_answer() -> i32 {
            42
        }
    }

    unsafe impl NSObjectProtocol for Counter {}
);

define_class!(
    #[unsafe(super(Counter, NSObject))]
    #[name = "SidestepTestDoubler"]
    struct Doubler;

    impl Doubler {
        #[unsafe(method(value))]
        fn value(&self) -> i32 {
            let base: i32 = unsafe { msg_send![super(self), value] };
            base * 2
        }
    }
);

fn counter() -> (Retained<Counter>, Arc<AtomicUsize>) {
    let drops = Arc::new(AtomicUsize::new(0));
    let counter: Retained<Counter> = unsafe { msg_send![Counter::alloc(), init] };
    *counter.ivars().drops.borrow_mut() = Some(drops.clone());
    (counter, drops)
}

/// objc2 conservatively marks most objects `!Send`. The thread tests below
/// exercise the runtime's own thread safety (reference counts, weak
/// references) on NSObject, which really is safe to share.
struct SendAnyway<T>(T);
unsafe impl<T> Send for SendAnyway<T> {}
unsafe impl<T> Sync for SendAnyway<T> {}

fn retain_count(obj: &AnyObject) -> usize {
    unsafe { msg_send![obj, retainCount] }
}

#[test]
fn nsobject_basics() {
    let obj = NSObject::new();
    let other = NSObject::new();
    assert!(obj.isEqual(Some(&obj)));
    assert!(!obj.isEqual(Some(&other)));
    assert_eq!(obj.hash(), obj.hash());
    assert_eq!(obj.class().name(), c"NSObject");
    assert!(obj.respondsToSelector(sel!(init)));
    assert!(!obj.respondsToSelector(sel!(sidestepNoSuchMethod)));
    assert!(NSObject::class().superclass().is_none());
}

#[test]
fn methods_and_ivars() {
    let (c, _) = counter();
    let v: i32 = unsafe { msg_send![&*c, value] };
    assert_eq!(v, 0);
    unsafe { msg_send![&*c, increment] }
    let v: i32 = unsafe { msg_send![&*c, addValue: 5i32] };
    assert_eq!(v, 6);
    let answer: i32 = unsafe { msg_send![Counter::class(), classAnswer] };
    assert_eq!(answer, 42);
}

#[test]
fn dealloc_drops_ivars_after_last_release() {
    let (c, drops) = counter();
    let c2 = c.clone();
    assert_eq!(retain_count(&c), 2);
    drop(c);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    assert_eq!(retain_count(&c2), 1);
    drop(c2);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn subclass_and_super() {
    let d: Retained<Doubler> = unsafe { msg_send![Doubler::alloc(), init] };
    unsafe { msg_send![&*d, increment] }
    unsafe { msg_send![&*d, increment] }
    let v: i32 = unsafe { msg_send![&*d, value] };
    assert_eq!(v, 4);
    assert!(d.isKindOfClass(Counter::class()));
    assert!(d.isKindOfClass(NSObject::class()));
    assert!(!d.isMemberOfClass(Counter::class()));
    assert_eq!(Doubler::class().superclass(), Some(Counter::class()));
    assert_eq!(Counter::class().superclass(), Some(NSObject::class()));
    let (c, _) = counter();
    assert!(!c.isKindOfClass(Doubler::class()));
}

#[test]
fn autorelease_pools_release_on_pop() {
    let (c, drops) = counter();
    autoreleasepool(|pool| {
        let c = unsafe { Retained::autorelease(c, pool) };
        let v: i32 = unsafe { msg_send![c, value] };
        assert_eq!(v, 0);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
    });
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn nested_pools() {
    let (a, a_drops) = counter();
    let (b, b_drops) = counter();
    autoreleasepool(|outer| {
        let _ = unsafe { Retained::autorelease(a, outer) };
        autoreleasepool(|inner| {
            let _ = unsafe { Retained::autorelease(b, inner) };
        });
        assert_eq!(b_drops.load(Ordering::SeqCst), 1);
        assert_eq!(a_drops.load(Ordering::SeqCst), 0);
    });
    assert_eq!(a_drops.load(Ordering::SeqCst), 1);
}

#[test]
fn weak_references_clear_on_dealloc() {
    let (c, drops) = counter();
    let weak = Weak::from_retained(&c);
    assert!(weak.load().is_some_and(|l| std::ptr::eq(&*l, &*c)));
    drop(c);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(weak.load().is_none());
}

#[test]
fn weak_references_across_threads() {
    for _ in 0..200 {
        let obj = NSObject::new();
        let weak = Arc::new(SendAnyway(Weak::from_retained(&obj)));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let weak = weak.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        if let Some(obj) = weak.0.load() {
                            assert!(obj.isEqual(Some(&obj)));
                        }
                    }
                })
            })
            .collect();
        drop(obj);
        for r in readers {
            r.join().unwrap();
        }
        assert!(weak.0.load().is_none());
    }
}

#[test]
fn retain_release_across_threads() {
    let obj = NSObject::new();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let obj = SendAnyway(obj.clone());
            std::thread::spawn(move || {
                let obj = obj;
                for _ in 0..10_000 {
                    drop(obj.0.clone());
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(retain_count(&obj), 1);
}

#[test]
fn protocols() {
    let proto = <dyn NSObjectProtocol>::protocol().expect("the NSObject protocol exists");
    assert_eq!(AnyProtocol::get(c"NSObject"), Some(proto));
    assert!(NSObject::class().conforms_to(proto));
    let (c, _) = counter();
    let as_proto: &ProtocolObject<dyn NSObjectProtocol> = ProtocolObject::from_ref(&*c);
    assert_eq!(as_proto.hash(), c.hash());
    assert!(c.conformsToProtocol(proto));
}

#[test]
fn class_introspection() {
    let cls = Counter::class();
    assert_eq!(AnyClass::get(c"SidestepTestCounter"), Some(cls));
    assert_eq!(cls.name(), c"SidestepTestCounter");
    assert!(cls.instance_size() > NSObject::class().instance_size());
    assert!(cls.instance_method(sel!(value)).is_some());
    assert!(cls.instance_method(sel!(init)).is_some());
    assert!(cls.class_method(sel!(classAnswer)).is_some());
    assert!(cls.instance_method(sel!(classAnswer)).is_none());
    assert!(cls.metaclass().is_metaclass());
    assert!(!cls.is_metaclass());
    let method = cls.instance_method(sel!(addValue:)).unwrap();
    assert_eq!(method.name(), sel!(addValue:));
    assert_eq!(method.arguments_count(), 3);
}

#[test]
fn blocks() {
    let captured = Arc::new(());
    let c2 = captured.clone();
    let add = RcBlock::new(move |a: i32, b: i32| {
        let _keep = &c2;
        a + b
    });
    assert_eq!(add.call((2, 3)), 5);
    let copy = add.clone();
    assert_eq!(copy.call((10, 20)), 30);
    assert_eq!(Arc::strong_count(&captured), 2);
    drop(add);
    drop(copy);
    assert_eq!(Arc::strong_count(&captured), 1);

    let base = 7;
    let stack = StackBlock::new(move |x: i32| x * base);
    let heap = stack.copy();
    assert_eq!(heap.call((6,)), 42);
}

#[test]
fn main_thread_marker_off_main_thread() {
    let on_other_thread = std::thread::spawn(|| MainThreadMarker::new().is_some()).join().unwrap();
    assert!(!on_other_thread);
}

#[test]
#[cfg(debug_assertions)]
fn unknown_selector_fails_loudly() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let obj = NSObject::new();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _: () = unsafe { msg_send![&*obj, sidestepNoSuchMethod] };
    }));
    assert!(result.is_err());
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepTestAnswerBase"]
    struct AnswerBase;

    impl AnswerBase {
        #[unsafe(method(answer))]
        fn answer(&self) -> i32 {
            1
        }
    }
);

define_class!(
    #[unsafe(super(AnswerBase, NSObject))]
    #[name = "SidestepTestAnswerDerived"]
    struct AnswerDerived;
);

extern "C-unwind" fn answer_two(_: &AnyObject, _: objc2::runtime::Sel) -> i32 {
    2
}

extern "C-unwind" fn answer_three(_: &AnyObject, _: objc2::runtime::Sel) -> i32 {
    3
}

#[test]
fn method_changes_reach_cached_messages() {
    let base: Retained<AnswerBase> = unsafe { msg_send![AnswerBase::alloc(), init] };
    let derived: Retained<AnswerDerived> = unsafe { msg_send![AnswerDerived::alloc(), init] };
    let answer = |o: &AnyObject| -> i32 { unsafe { msg_send![o, answer] } };
    // Send enough messages that both classes have the method cached.
    for _ in 0..3 {
        assert_eq!(answer(&base), 1);
        assert_eq!(answer(&derived), 1);
    }

    // A subclass gains an override of a method it had cached.
    let imp: objc2::runtime::Imp = unsafe { std::mem::transmute(answer_two as extern "C-unwind" fn(_, _) -> _) };
    let cls = (AnswerDerived::class() as *const AnyClass).cast_mut();
    let added = unsafe { objc2::ffi::class_addMethod(cls, sel!(answer), imp, c"i@:".as_ptr()) };
    assert!(added.as_bool());
    assert_eq!(answer(&derived), 2);
    assert_eq!(answer(&base), 1);

    // A method's implementation is replaced.
    let method = AnswerBase::class().instance_method(sel!(answer)).expect("answer");
    let imp: objc2::runtime::Imp = unsafe { std::mem::transmute(answer_three as extern "C-unwind" fn(_, _) -> _) };
    unsafe { method.set_implementation(imp) };
    assert_eq!(answer(&base), 3);
    assert_eq!(answer(&derived), 2);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepTestRaceTarget"]
    struct RaceTarget;

    impl RaceTarget {
        #[unsafe(method(stable))]
        fn stable(&self) -> i32 {
            7
        }

        #[unsafe(method(changing))]
        fn changing(&self) -> i32 {
            0
        }
    }
);

extern "C-unwind" fn changing_one(_: &AnyObject, _: objc2::runtime::Sel) -> i32 {
    1
}

extern "C-unwind" fn changing_two(_: &AnyObject, _: objc2::runtime::Sel) -> i32 {
    2
}

#[test]
fn messages_race_method_changes() {
    use std::sync::atomic::AtomicBool;

    struct Shared(Retained<RaceTarget>);
    // SAFETY: RaceTarget has no state; messages to it are thread-safe.
    unsafe impl Send for Shared {}
    unsafe impl Sync for Shared {}

    let target = Arc::new(Shared(unsafe { msg_send![RaceTarget::alloc(), init] }));
    let stop = Arc::new(AtomicBool::new(false));
    let sent = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(std::sync::Barrier::new(5));
    let senders: Vec<_> = (0..4)
        .map(|_| {
            let (target, stop, sent, start) = (target.clone(), stop.clone(), sent.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                while !stop.load(Ordering::Relaxed) {
                    let stable: i32 = unsafe { msg_send![&*target.0, stable] };
                    assert_eq!(stable, 7);
                    let changing: i32 = unsafe { msg_send![&*target.0, changing] };
                    assert!((0..=2).contains(&changing), "{changing}");
                    let hash: usize = unsafe { msg_send![&*target.0, hash] };
                    assert_ne!(hash, 0);
                    sent.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    // Keep swapping one method's implementation while the others run, which
    // empties every class's cache each time.
    let method = RaceTarget::class().instance_method(sel!(changing)).expect("changing");
    type Changing = extern "C-unwind" fn(&AnyObject, objc2::runtime::Sel) -> i32;
    let imps: [objc2::runtime::Imp; 2] = unsafe {
        [
            std::mem::transmute::<Changing, objc2::runtime::Imp>(changing_one),
            std::mem::transmute::<Changing, objc2::runtime::Imp>(changing_two),
        ]
    };
    start.wait();
    // Swap at least 2000 times, and until the senders have really raced the
    // swaps, however the threads get scheduled.
    let mut i = 0;
    while i < 2000 || sent.load(Ordering::Relaxed) < 10_000 {
        unsafe { method.set_implementation(imps[i % 2]) };
        let now: i32 = unsafe { msg_send![&*target.0, changing] };
        assert_eq!(now, (i % 2) as i32 + 1, "a thread sees its own change at once");
        std::thread::yield_now();
        i += 1;
    }
    stop.store(true, Ordering::Relaxed);
    for sender in senders {
        sender.join().unwrap();
    }
}
