//! Cost of the runtime's hot paths, in nanoseconds per operation. Run in
//! release mode on macOS (Apple's runtime) and on Linux (Sidestep's) to
//! compare: `cargo run --release -p msgbench`. An argument runs only the
//! cases whose names contain it: `cargo run --release -p msgbench -- alloc`.
//! `MSGBENCH_ITERS=1000` makes every case short, for a profiler.

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc};
use std::cell::Cell;
use std::ffi::c_void;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, ffi, msg_send, sel};
use objc2_foundation::{NSInvocation, NSMethodSignature, NSString};

use sidestep as _;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MsgBenchTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(value))]
        fn value(&self) -> i32 {
            7
        }

        #[unsafe(method(classValue))]
        fn class_value() -> i32 {
            9
        }

        #[unsafe(method(echo:))]
        fn echo(&self, obj: *mut AnyObject) -> *mut AnyObject {
            obj
        }
    }
);

/// Forwards what it doesn't implement to a `Target` of its own.
struct ProxyIvars {
    target: Retained<Target>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MsgBenchProxy"]
    #[ivars = ProxyIvars]
    struct Proxy;

    impl Proxy {
        #[unsafe(method(forwardingTargetForSelector:))]
        fn forwarding_target(&self, _sel: Sel) -> *mut AnyObject {
            Retained::as_ptr(&self.ivars().target).cast_mut().cast()
        }
    }
);

impl Proxy {
    fn new() -> Retained<Self> {
        let target: Retained<Target> = unsafe { msg_send![Target::alloc(), init] };
        let this = Self::alloc().set_ivars(ProxyIvars { target });
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    /// Forwards what it doesn't implement to a `Target` of its own
    /// through `-forwardInvocation:`.
    #[unsafe(super(NSObject))]
    #[name = "MsgBenchInvocationProxy"]
    #[ivars = ProxyIvars]
    struct InvocationProxy;

    impl InvocationProxy {
        #[unsafe(method_id(methodSignatureForSelector:))]
        fn method_signature(&self, sel: Sel) -> Option<Retained<NSMethodSignature>> {
            unsafe { msg_send![&*self.ivars().target, methodSignatureForSelector: sel] }
        }

        #[unsafe(method(forwardInvocation:))]
        fn forward_invocation(&self, invocation: &NSInvocation) {
            unsafe { invocation.invokeWithTarget(&self.ivars().target) };
        }
    }
);

impl InvocationProxy {
    fn new() -> Retained<Self> {
        let target: Retained<Target> = unsafe { msg_send![Target::alloc(), init] };
        let this = Self::alloc().set_ivars(ProxyIvars { target });
        unsafe { msg_send![super(this), init] }
    }
}

/// What a typical app class holds: a few plain fields and an object.
struct FieldsIvars {
    _count: Cell<i64>,
    _scale: Cell<f64>,
    child: Retained<NSObject>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MsgBenchFields"]
    #[ivars = FieldsIvars]
    struct Fields;

    impl Fields {
        /// Returns an object the caller doesn't own: autoreleased on the
        /// way out, and retained again by objc2 on the way in.
        #[unsafe(method_id(child))]
        fn child(&self) -> Retained<NSObject> {
            self.ivars().child.clone()
        }
    }
);

impl Fields {
    fn new(child: &NSObject) -> Retained<Self> {
        let this = Self::alloc().set_ivars(FieldsIvars {
            _count: Cell::new(1),
            _scale: Cell::new(1.0),
            child: child.retain(),
        });
        unsafe { msg_send![super(this), init] }
    }
}

#[inline(never)]
fn plain_rust(x: i32) -> i32 {
    x.wrapping_add(7)
}

/// Runs only the cases whose names contain the first argument, if any.
fn wanted(name: &str) -> bool {
    std::env::args().nth(1).is_none_or(|filter| name.contains(&filter))
}

/// `MSGBENCH_ITERS` overrides every case's iteration count, for running
/// under a profiler.
fn iterations(default: u64) -> u64 {
    std::env::var("MSGBENCH_ITERS").ok().and_then(|n| n.parse().ok()).unwrap_or(default)
}

/// Median of seven timed runs, after a warm-up.
fn bench(name: &str, iters: u64, mut f: impl FnMut()) {
    if !wanted(name) {
        return;
    }
    let iters = iterations(iters);
    for _ in 0..iters / 10 {
        f();
    }
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iters {
                f();
            }
            start.elapsed().as_nanos() as f64 / iters as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    println!("{name:<48} {:>8.2} ns  (min {:.2})", runs[3], runs[0]);
}

/// Four threads each running `f` `iters` times at once: the wall time per
/// iteration, median of seven runs.
fn bench_threads<S: 'static>(name: &str, iters: u64, state: S, f: fn(&S)) {
    if !wanted(name) {
        return;
    }
    let iters = iterations(iters);
    let state = Arc::new(SendAnyway(state));
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Arc::new(std::sync::Barrier::new(5));
            let threads: Vec<_> = (0..4)
                .map(|_| {
                    let (state, start) = (state.clone(), start.clone());
                    std::thread::spawn(move || {
                        start.wait();
                        for _ in 0..iters {
                            f(&state.0);
                        }
                    })
                })
                .collect();
            start.wait();
            let began = Instant::now();
            for t in threads {
                t.join().unwrap();
            }
            began.elapsed().as_nanos() as f64 / iters as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    println!("{name:<48} {:>8.2} ns  (min {:.2})", runs[3], runs[0]);
}

/// objc2 marks most objects `!Send`; these benchmarks share only objects
/// whose use from several threads the runtime supports (reference counts,
/// weak references, associations).
struct SendAnyway<T>(T);
unsafe impl<T> Send for SendAnyway<T> {}
unsafe impl<T> Sync for SendAnyway<T> {}

static KEY: u8 = 0;

fn key() -> *const c_void {
    (&raw const KEY).cast()
}

fn set_associated(obj: &AnyObject, value: &AnyObject) {
    let obj = (obj as *const AnyObject).cast_mut();
    let value = (value as *const AnyObject).cast_mut();
    // SAFETY: live objects and a static key.
    unsafe { ffi::objc_setAssociatedObject(obj, key(), value, ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC) };
}

fn get_associated(obj: &AnyObject) -> *const AnyObject {
    // SAFETY: a live object and a static key.
    unsafe { ffi::objc_getAssociatedObject(obj, key()) }
}

fn main() {
    let target: Retained<Target> = unsafe { msg_send![Target::alloc(), init] };
    let obj = NSObject::new();
    let f: fn(i32) -> i32 = black_box(plain_rust);

    bench("baseline: indirect call to a Rust fn", 50_000_000, || {
        black_box(f(black_box(1)));
    });
    bench("msg_send instance method (define_class!)", 50_000_000, || {
        let v: i32 = unsafe { msg_send![black_box(&*target), value] };
        black_box(v);
    });
    bench("msg_send class method", 50_000_000, || {
        let v: i32 = unsafe { msg_send![black_box(Target::class()), classValue] };
        black_box(v);
    });
    bench("generated binding: -[NSObject hash]", 50_000_000, || {
        black_box(black_box(&*obj).hash());
    });
    bench("retain + release (Retained clone/drop)", 50_000_000, || {
        drop(black_box(obj.clone()));
    });
    bench("retain + msg_send + release", 50_000_000, || {
        let t = black_box(&target).clone();
        let v: i32 = unsafe { msg_send![&*t, value] };
        black_box(v);
        drop(t);
    });
    // Code written against the C API, calling objc_msgSend itself.
    let send: unsafe extern "C-unwind" fn(*const Target, Sel) -> i32 =
        unsafe { std::mem::transmute(ffi::objc_msgSend as unsafe extern "C-unwind" fn()) };
    bench("objc_msgSend called directly", 50_000_000, || {
        black_box(unsafe { send(black_box(&*target), sel!(value)) });
    });

    // Messages a class doesn't implement, sent on through
    // -forwardingTargetForSelector:.
    let proxy = Proxy::new();
    bench("forwarded message (forwardingTargetForSelector:)", 10_000_000, || {
        let v: i32 = unsafe { msg_send![black_box(&*proxy), value] };
        black_box(v);
    });
    bench("performSelector:withObject: forwarded", 10_000_000, || {
        let r: *mut AnyObject =
            unsafe { msg_send![black_box(&*proxy), performSelector: sel!(echo:), withObject: &*obj] };
        black_box(r);
    });

    // Messages forwarded through -forwardInvocation:, and invocations.
    let invocation_proxy = InvocationProxy::new();
    autoreleasepool(|_| {
        bench("forwarded message (forwardInvocation:)", 1_000_000, || {
            let v: i32 = unsafe { msg_send![black_box(&*invocation_proxy), value] };
            black_box(v);
        });
    });
    let signature: Option<Retained<NSMethodSignature>> =
        unsafe { msg_send![&*target, methodSignatureForSelector: sel!(echo:)] };
    let invocation = unsafe { NSInvocation::invocationWithMethodSignature(&signature.unwrap()) };
    unsafe {
        invocation.setTarget(Some(&target));
        invocation.setSelector(sel!(echo:));
        let arg: *const NSObject = &*obj;
        invocation.setArgument_atIndex(std::ptr::NonNull::from(&arg).cast(), 2);
    }
    bench("NSInvocation invoke, one object argument", 10_000_000, || {
        unsafe { black_box(&*invocation).invoke() };
    });

    // What an object allocation costs the allocator alone: an NSObject
    // takes 16 bytes on Apple's runtime and 24 on Sidestep's.
    let layout = Layout::from_size_align(24, 16).unwrap();
    bench("allocator: malloc + free, 24 bytes", 10_000_000, || unsafe {
        dealloc(black_box(alloc(layout)), layout);
    });
    bench("allocator: calloc + free, 24 bytes", 10_000_000, || unsafe {
        dealloc(black_box(alloc_zeroed(layout)), layout);
    });
    bench("alloc + init + release NSObject", 10_000_000, || {
        drop(black_box(NSObject::new()));
    });
    bench("alloc + init + release, class with ivars", 10_000_000, || {
        drop(black_box(Fields::new(&obj)));
    });

    bench("autorelease pool push/pop, one object", 10_000_000, || {
        autoreleasepool(|pool| {
            let o = unsafe { Retained::autorelease(obj.clone(), pool) };
            black_box(o);
        });
    });
    let fields = Fields::new(&obj);
    autoreleasepool(|_| {
        bench("autoreleased return, retained by the caller", 10_000_000, || {
            let child: Retained<NSObject> = unsafe { msg_send![black_box(&*fields), child] };
            drop(black_box(child));
        });
    });
    bench("autoreleased return not kept, own pool", 10_000_000, || {
        autoreleasepool(|_| {
            let child: *mut NSObject = unsafe { msg_send![black_box(&*fields), child] };
            black_box(child);
        });
    });
    bench("NSString::from_str(\"hello\") + release", 5_000_000, || {
        drop(black_box(NSString::from_str(black_box("hello"))));
    });

    let weak = Weak::from_retained(&obj);
    bench("weak load + release", 10_000_000, || {
        drop(black_box(weak.load()));
    });
    bench("weak reference create + destroy", 10_000_000, || {
        drop(black_box(Weak::from_retained(&obj)));
    });
    set_associated(&obj, &target);
    bench("associated object get", 10_000_000, || {
        black_box(get_associated(black_box(&obj)));
    });
    bench("associated object set + get + remove", 5_000_000, || {
        let o = black_box(&*fields);
        set_associated(o, &target);
        black_box(get_associated(o));
        // SAFETY: a live object.
        unsafe {
            ffi::objc_setAssociatedObject((o as *const Fields).cast_mut().cast(), key(), std::ptr::null_mut(), 1)
        };
    });

    // Four threads on one object's reference count, or on the runtime's
    // tables for objects of their own.
    bench_threads("4 threads: retain + release, one object", 2_000_000, obj.clone(), |o| {
        drop(black_box(o.clone()));
    });
    bench_threads("4 threads: weak load + release, one object", 2_000_000, Weak::from_retained(&obj), |w| {
        drop(black_box(w.load()));
    });
    let own: Vec<_> = (0..4).map(|_| NSObject::new()).collect();
    bench_threads("4 threads: weak create + destroy, own objects", 2_000_000, own.clone(), |own| {
        let o = &own[thread_slot(own.len())];
        drop(black_box(Weak::from_retained(o)));
    });
    // The same without objc2's `Weak`, which allocates its location on the
    // heap: what the runtime's weak table costs, apart from the allocator.
    bench_threads("4 threads: weak init + destroy, stack location", 2_000_000, own.clone(), |own| {
        let o = &own[thread_slot(own.len())];
        let mut location: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: a live object, and a location that lives until destroyed.
        unsafe {
            ffi::objc_initWeak(&mut location, Retained::as_ptr(o).cast_mut().cast());
            ffi::objc_destroyWeak(black_box(&mut location));
        }
    });
    for o in &own {
        set_associated(o, &target);
    }
    bench_threads("4 threads: associated object get, own objects", 2_000_000, own, |own| {
        black_box(get_associated(&own[thread_slot(own.len())]));
    });
    bench_threads("4 threads: alloc + init + release", 2_000_000, (), |_| {
        drop(black_box(NSObject::new()));
    });
    let proxies: Vec<_> = (0..4).map(|_| Proxy::new()).collect();
    bench_threads("4 threads: performSelector:withObject: forwarded", 2_000_000, (proxies, obj.clone()), |state| {
        let proxy = &state.0[thread_slot(state.0.len())];
        let r: *mut AnyObject =
            unsafe { msg_send![black_box(&**proxy), performSelector: sel!(echo:), withObject: &*state.1] };
        black_box(r);
    });
}

/// A small number that differs between the benchmark's threads, so each
/// thread can use an object of its own.
fn thread_slot(n: usize) -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    thread_local!(static SLOT: usize = NEXT.fetch_add(1, Ordering::Relaxed));
    SLOT.with(|s| *s % n)
}
