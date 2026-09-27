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

/// A fixed pseudo-random sequence (xorshift), so every run is the same.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// What a weak location holds: its object's address, or null.
fn peek_weak(location: &mut *mut AnyObject) -> *mut AnyObject {
    // SAFETY: a weak location; the object is released right away, and the
    // result only compared.
    unsafe {
        let obj = objc2::ffi::objc_loadWeakRetained(location);
        objc2::ffi::objc_release(obj);
        obj
    }
}

/// Many weak references to one object (a delegate, an observed object),
/// destroyed in creation order, in reverse and shuffled: those left keep
/// loading the object, and are cleared when it goes.
#[test]
fn many_weak_references_to_one_object() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for order in ["creation", "reverse", "random"] {
        let (c, drops) = counter();
        let mut weaks: Vec<Weak<Counter>> = (0..3000).map(|_| Weak::from_retained(&c)).collect();
        match order {
            "reverse" => weaks.reverse(),
            "random" => {
                for i in (1..weaks.len()).rev() {
                    weaks.swap(i, rng.below(i + 1));
                }
            }
            _ => {}
        }
        let kept = weaks.split_off(2900);
        for (i, weak) in weaks.into_iter().enumerate() {
            if i % 250 == 0 {
                assert!(weak.load().is_some_and(|l| std::ptr::eq(&*l, &*c)), "{order} order, reference {i}");
                assert!(kept.iter().all(|k| k.load().is_some_and(|l| std::ptr::eq(&*l, &*c))));
            }
            drop(weak);
        }
        assert!(kept.iter().all(|k| k.load().is_some_and(|l| std::ptr::eq(&*l, &*c))), "{order} order");
        drop(c);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(kept.iter().all(|k| k.load().is_none()), "{order} order: cleared");
    }
}

/// An object's weak references rising into the thousands and falling back
/// to a handful, again and again: each still loads the object while it
/// lives, and none after.
#[test]
fn weak_reference_counts_rising_and_falling() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (c, drops) = counter();
    let mut weaks: Vec<Weak<Counter>> = Vec::new();
    for (grow_to, shrink_to) in [(20, 3), (5, 1), (40, 2), (17, 16), (1000, 4), (18, 0), (300, 10)] {
        while weaks.len() < grow_to {
            weaks.push(Weak::from_retained(&c));
        }
        while weaks.len() > shrink_to {
            drop(weaks.swap_remove(rng.below(weaks.len())));
        }
        assert!(weaks.iter().all(|w| w.load().is_some_and(|l| std::ptr::eq(&*l, &*c))));
    }
    drop(c);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(weaks.iter().all(|w| w.load().is_none()));
}

/// `objc_storeWeak` moving locations between two objects and nil, so that
/// each object's weak references go from a handful to hundreds and back.
#[test]
fn weak_locations_moving_between_objects() {
    use objc2::ffi::{objc_destroyWeak, objc_initWeak, objc_storeWeak};
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let (a, a_drops) = counter();
    let (b, b_drops) = counter();
    let (pa, pb) = (Retained::as_ptr(&a).cast_mut().cast::<AnyObject>(), Retained::as_ptr(&b).cast_mut().cast());
    // What each location holds, as the test expects it: 0 nil, 1 a, 2 b.
    let mut locations: Box<[*mut AnyObject]> = vec![std::ptr::null_mut(); 200].into_boxed_slice();
    let mut expected = vec![1u8; locations.len()];
    unsafe {
        for location in locations.iter_mut() {
            objc_initWeak(location, pa);
        }
        // Each phase moves nearly every location to one of the three, so
        // each object's count sweeps between a few and nearly all.
        for favored in [2u8, 0, 1, 2, 0, 1] {
            for _ in 0..1500 {
                let i = rng.below(locations.len());
                let to = if rng.below(32) == 0 { rng.below(3) as u8 } else { favored };
                objc_storeWeak(&mut locations[i], [std::ptr::null_mut(), pa, pb][to as usize]);
                expected[i] = to;
                let j = rng.below(locations.len());
                assert_eq!(peek_weak(&mut locations[j]), [std::ptr::null_mut(), pa, pb][expected[j] as usize]);
            }
        }
        drop(a);
        assert_eq!(a_drops.load(Ordering::SeqCst), 1);
        for (location, &e) in locations.iter_mut().zip(&expected) {
            assert_eq!(peek_weak(location), if e == 2 { pb } else { std::ptr::null_mut() });
        }
        drop(b);
        assert_eq!(b_drops.load(Ordering::SeqCst), 1);
        for location in locations.iter_mut() {
            assert!(peek_weak(location).is_null());
            objc_destroyWeak(location);
        }
    }
}

/// Four threads storing, moving and loading many weak locations to two
/// shared objects while one of them is freed: the locations holding it are
/// cleared, those holding the other still load it.
#[test]
fn many_weak_locations_under_contention() {
    use objc2::ffi::{objc_destroyWeak, objc_initWeak, objc_storeWeak};
    /// A worker's arrival at the barrier, on its way or when it unwinds:
    /// one whose check fails still lets the others through, so the test
    /// fails rather than hangs.
    struct Arrival(Option<Arc<std::sync::Barrier>>);
    impl Arrival {
        fn wait(&mut self) {
            if let Some(barrier) = self.0.take() {
                barrier.wait();
            }
        }
    }
    impl Drop for Arrival {
        fn drop(&mut self) {
            self.wait();
        }
    }
    for round in 0..20u64 {
        let (x, x_drops) = counter();
        let y = NSObject::new();
        let freed = Arc::new(std::sync::Barrier::new(5));
        let threads: Vec<_> = (0..4u64)
            .map(|t| {
                let (x, y, freed) = (SendAnyway(x.clone()), SendAnyway(y.clone()), freed.clone());
                std::thread::spawn(move || {
                    // Declared first, so an unwinding worker lets go of x
                    // before it arrives.
                    let mut freed = Arrival(Some(freed));
                    let (x, y) = (x, y);
                    let px = Retained::as_ptr(&x.0).cast_mut().cast::<AnyObject>();
                    let py = Retained::as_ptr(&y.0).cast_mut().cast::<AnyObject>();
                    let mut rng = Rng(0x5851_f42d_4c95_7f2d ^ (round << 8 | t));
                    let mut locations: Box<[*mut AnyObject]> = vec![std::ptr::null_mut(); 40].into_boxed_slice();
                    unsafe {
                        for location in locations.iter_mut() {
                            objc_initWeak(location, px);
                        }
                    }
                    for _ in 0..300 {
                        let i = rng.below(locations.len());
                        unsafe { objc_storeWeak(&mut locations[i], if rng.below(3) == 0 { py } else { px }) };
                        let obj = peek_weak(&mut locations[rng.below(40)]);
                        assert!(obj == px || obj == py);
                    }
                    // This thread's hold on x goes; the last one frees it,
                    // while the others go on here.
                    drop(x);
                    for _ in 0..300 {
                        let i = rng.below(locations.len());
                        if rng.below(2) == 0 {
                            unsafe { objc_storeWeak(&mut locations[i], py) };
                        }
                        let obj = peek_weak(&mut locations[rng.below(40)]);
                        assert!(obj == px || obj == py || obj.is_null());
                    }
                    freed.wait();
                    // x is gone: its locations are nil.
                    let mut holding_y = 0;
                    for location in locations.iter_mut() {
                        let obj = peek_weak(location);
                        assert!(obj == py || obj.is_null());
                        holding_y += usize::from(obj == py);
                        unsafe { objc_destroyWeak(location) };
                    }
                    holding_y
                })
            })
            .collect();
        drop(x);
        freed.wait();
        assert_eq!(x_drops.load(Ordering::SeqCst), 1, "round {round}");
        let holding_y: usize = threads.into_iter().map(|t| t.join().unwrap()).sum();
        assert!(holding_y > 0);
        assert_eq!(retain_count(&y), 1);
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

static LOADS: AtomicUsize = AtomicUsize::new(0);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepTestLoad"]
    struct LoadCounter;

    impl LoadCounter {
        #[unsafe(method(load))]
        fn load() {
            LOADS.fetch_add(1, Ordering::SeqCst);
        }
    }
);

/// `+load` goes to the classes of images as they load; a class defined at
/// run time, as `define_class!` does, gets none, and neither registering
/// it nor messaging it sends one.
#[test]
fn load_is_not_sent_to_classes_made_at_run_time() {
    let cls = LoadCounter::class();
    let _: *const AnyClass = unsafe { msg_send![cls, class] };
    assert_eq!(LOADS.load(Ordering::SeqCst), 0);
}

define_class!(
    /// Describes its class in its own words.
    #[unsafe(super(NSObject))]
    #[name = "SidestepTestDescribed"]
    struct Described;

    impl Described {
        #[unsafe(method_id(description))]
        fn class_description() -> Retained<objc2_foundation::NSString> {
            objc2_foundation::NSString::from_str("described in its own words")
        }
    }
);

/// A class describes itself by its name; a debug description, a class's
/// or an object's, is its description.
#[test]
fn descriptions() {
    use objc2_foundation::NSString;
    autoreleasepool(|_| {
        let name: Retained<NSString> = unsafe { msg_send![Described::class(), debugDescription] };
        assert_eq!(name.to_string(), "described in its own words");
        let name: Retained<NSString> = unsafe { msg_send![NSObject::class(), description] };
        assert_eq!(name.to_string(), "NSObject");
        let name: Retained<NSString> = unsafe { msg_send![Counter::class(), description] };
        assert_eq!(name.to_string(), "SidestepTestCounter");
        let name: Retained<NSString> = unsafe { msg_send![Counter::class(), debugDescription] };
        assert_eq!(name.to_string(), "SidestepTestCounter");
        let (obj, _) = counter();
        let description: Retained<NSString> = unsafe { msg_send![&*obj, description] };
        let debug: Retained<NSString> = unsafe { msg_send![&*obj, debugDescription] };
        assert!(description.to_string().starts_with("<SidestepTestCounter: 0x"), "{description}");
        assert_eq!(debug.to_string(), description.to_string());
    });
}
