//! Message forwarding with `-forwardingTargetForSelector:`: a message a
//! class doesn't implement goes to the object it names, arguments and
//! return value intact, and the target is asked for again each time.
//!
//! objc2 checks in debug builds that a message's method exists before
//! sending it, so these tests send forwarded messages through the
//! implementation `-methodForSelector:` hands out, which forwards however
//! it is called. Release builds also send them with `msg_send!`.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send, sel};

use sidestep as _;

/// Returned in registers on aarch64 (four doubles), in memory on x86_64.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Quad {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
}

unsafe impl objc2::Encode for Quad {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("SidestepQuad", &[f64::ENCODING; 4]);
}

/// Returned through memory on every architecture.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wide {
    v: [i64; 5],
}

unsafe impl objc2::Encode for Wide {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("SidestepWide", &[<[i64; 5]>::ENCODING]);
}

struct TargetIvars {
    base: i64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepForwardTarget"]
    #[ivars = TargetIvars]
    struct Target;

    impl Target {
        #[unsafe(method(add:to:))]
        fn add(&self, a: i64, b: i64) -> i64 {
            self.ivars().base + a + b
        }

        #[unsafe(method(scale:by:))]
        fn scale(&self, x: f64, y: f64) -> f64 {
            self.ivars().base as f64 * x * y
        }

        /// More integer arguments than there are registers for them.
        #[unsafe(method(ints:b:c:d:e:f:g:h:i:j:k:l:))]
        #[allow(clippy::too_many_arguments)]
        fn ints(
            &self,
            a: i64,
            b: i64,
            c: i64,
            d: i64,
            e: i64,
            f: i64,
            g: i64,
            h: i64,
            i: i64,
            j: i64,
            k: i64,
            l: i64,
        ) -> i64 {
            // Weighted, so arguments arriving in the wrong places show.
            [a, b, c, d, e, f, g, h, i, j, k, l].iter().enumerate().map(|(n, v)| (n as i64 + 1) * v).sum::<i64>()
                + self.ivars().base
        }

        /// More floating-point arguments than there are registers for them.
        #[unsafe(method(floats:b:c:d:e:f:g:h:i:j:k:))]
        #[allow(clippy::too_many_arguments)]
        fn floats(
            &self,
            a: f64,
            b: f64,
            c: f64,
            d: f64,
            e: f64,
            f: f64,
            g: f64,
            h: f64,
            i: f64,
            j: f64,
            k: f64,
        ) -> f64 {
            [a, b, c, d, e, f, g, h, i, j, k].iter().enumerate().map(|(n, v)| (n as f64 + 1.0) * v).sum::<f64>()
                + self.ivars().base as f64
        }

        #[unsafe(method(quad:))]
        fn quad(&self, x: f64) -> Quad {
            Quad { a: x, b: x * 2.0, c: x * 3.0, d: self.ivars().base as f64 }
        }

        #[unsafe(method(wide:))]
        fn wide(&self, x: i64) -> Wide {
            Wide { v: [x, x + 1, x + 2, x + 3, self.ivars().base] }
        }

        #[unsafe(method(base))]
        fn base(&self) -> i64 {
            self.ivars().base
        }
    }
);

impl Target {
    fn new(base: i64) -> Retained<Self> {
        let this = Self::alloc().set_ivars(TargetIvars { base });
        unsafe { msg_send![super(this), init] }
    }
}

struct ProxyIvars {
    targets: [Retained<Target>; 2],
    which: Cell<usize>,
    asked: Cell<usize>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepForwardProxy"]
    #[ivars = ProxyIvars]
    struct Proxy;

    impl Proxy {
        #[unsafe(method(forwardingTargetForSelector:))]
        fn forwarding_target(&self, sel: Sel) -> *mut AnyObject {
            self.ivars().asked.set(self.ivars().asked.get() + 1);
            if sel == sel!(unknownToEveryone) {
                return std::ptr::null_mut();
            }
            let target = &self.ivars().targets[self.ivars().which.get()];
            Retained::as_ptr(target).cast_mut().cast()
        }

        #[unsafe(method(ownValue))]
        fn own_value(&self) -> i64 {
            -1
        }
    }
);

impl Proxy {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ProxyIvars {
            targets: [Target::new(100), Target::new(200)],
            which: Cell::new(0),
            asked: Cell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepForwardClassTarget"]
    struct ClassTarget;

    impl ClassTarget {
        #[unsafe(method(classAnswer:))]
        fn class_answer(x: i64) -> i64 {
            x * 3
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepForwardClassProxy"]
    struct ClassProxy;

    impl ClassProxy {
        #[unsafe(method(forwardingTargetForSelector:))]
        fn class_forwarding_target(_sel: Sel) -> *mut AnyObject {
            (ClassTarget::class() as *const AnyClass).cast_mut().cast()
        }
    }
);

/// The implementation a message to `obj` with `sel` resolves to.
fn imp_for(obj: &AnyObject, sel: Sel) -> Imp {
    let imp: Option<Imp> = unsafe { msg_send![obj, methodForSelector: sel] };
    imp.expect("an implementation")
}

#[test]
fn forwards_integer_and_float_arguments() {
    let proxy = Proxy::new();
    let add: unsafe extern "C-unwind" fn(&Proxy, Sel, i64, i64) -> i64 =
        unsafe { std::mem::transmute(imp_for(&proxy, sel!(add:to:))) };
    assert_eq!(unsafe { add(&proxy, sel!(add:to:), 2, 3) }, 105);
    let scale: unsafe extern "C-unwind" fn(&Proxy, Sel, f64, f64) -> f64 =
        unsafe { std::mem::transmute(imp_for(&proxy, sel!(scale:by:))) };
    assert_eq!(unsafe { scale(&proxy, sel!(scale:by:), 0.5, 3.0) }, 150.0);
}

type Ints = unsafe extern "C-unwind" fn(&Proxy, Sel, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) -> i64;
type Floats = unsafe extern "C-unwind" fn(&Proxy, Sel, f64, f64, f64, f64, f64, f64, f64, f64, f64, f64, f64) -> f64;

#[test]
fn forwards_stack_arguments() {
    let proxy = Proxy::new();
    let sel = sel!(ints:b:c:d:e:f:g:h:i:j:k:l:);
    let ints: Ints = unsafe { std::mem::transmute(imp_for(&proxy, sel)) };
    let got = unsafe { ints(&proxy, sel, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12) };
    assert_eq!(got, (1..=12).map(|n| n * n).sum::<i64>() + 100);
    let sel = sel!(floats:b:c:d:e:f:g:h:i:j:k:);
    let floats: Floats = unsafe { std::mem::transmute(imp_for(&proxy, sel)) };
    let got = unsafe { floats(&proxy, sel, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0, 5.5) };
    assert_eq!(got, (1..=11).map(|n| (n * n) as f64 / 2.0).sum::<f64>() + 100.0);
}

#[test]
fn forwards_struct_returns() {
    let proxy = Proxy::new();
    let quad: unsafe extern "C-unwind" fn(&Proxy, Sel, f64) -> Quad =
        unsafe { std::mem::transmute(imp_for(&proxy, sel!(quad:))) };
    assert_eq!(unsafe { quad(&proxy, sel!(quad:), 1.5) }, Quad { a: 1.5, b: 3.0, c: 4.5, d: 100.0 });
    let wide: unsafe extern "C-unwind" fn(&Proxy, Sel, i64) -> Wide =
        unsafe { std::mem::transmute(imp_for(&proxy, sel!(wide:))) };
    assert_eq!(unsafe { wide(&proxy, sel!(wide:), 7) }, Wide { v: [7, 8, 9, 10, 100] });
}

#[test]
fn asks_for_the_target_every_time() {
    let proxy = Proxy::new();
    let base: unsafe extern "C-unwind" fn(&Proxy, Sel) -> i64 =
        unsafe { std::mem::transmute(imp_for(&proxy, sel!(base))) };
    let before = proxy.ivars().asked.get();
    assert_eq!(unsafe { base(&proxy, sel!(base)) }, 100);
    proxy.ivars().which.set(1);
    assert_eq!(unsafe { base(&proxy, sel!(base)) }, 200);
    proxy.ivars().which.set(0);
    assert_eq!(unsafe { base(&proxy, sel!(base)) }, 100);
    assert_eq!(proxy.ivars().asked.get() - before, 3);
}

#[test]
fn own_methods_are_not_forwarded() {
    let proxy = Proxy::new();
    let own: i64 = unsafe { msg_send![&*proxy, ownValue] };
    assert_eq!(own, -1);
    assert_eq!(proxy.ivars().asked.get(), 0);
    // Forwarding doesn't make a class respond to what it forwards.
    assert!(!proxy.respondsToSelector(sel!(add:to:)));
    assert!(!Proxy::class().responds_to(sel!(base)));
    assert!(Proxy::class().instance_method(sel!(base)).is_none());
}

#[test]
fn forwards_class_messages() {
    let cls = ClassProxy::class();
    let imp: Option<Imp> = unsafe { msg_send![cls, methodForSelector: sel!(classAnswer:)] };
    let answer: unsafe extern "C-unwind" fn(&AnyClass, Sel, i64) -> i64 =
        unsafe { std::mem::transmute(imp.expect("an implementation")) };
    assert_eq!(unsafe { answer(cls, sel!(classAnswer:), 14) }, 42);
}

#[test]
#[cfg(not(debug_assertions))]
fn msg_send_forwards() {
    let proxy = Proxy::new();
    for _ in 0..3 {
        let sum: i64 = unsafe { msg_send![&*proxy, add: 1i64, to: 2i64] };
        assert_eq!(sum, 103);
        let quad: Quad = unsafe { msg_send![&*proxy, quad: 2.0f64] };
        assert_eq!(quad, Quad { a: 2.0, b: 4.0, c: 6.0, d: 100.0 });
        let wide: Wide = unsafe { msg_send![&*proxy, wide: 1i64] };
        assert_eq!(wide.v, [1, 2, 3, 4, 100]);
    }
    let answer: i64 = unsafe { msg_send![ClassProxy::class(), classAnswer: 5i64] };
    assert_eq!(answer, 15);
}

/// Without a target, the message is unrecognized. On Apple's runtime that
/// raises an Objective-C exception, which Rust can't catch; Sidestep
/// panics, unwinding through the forwarding machinery.
#[test]
#[cfg(not(target_vendor = "apple"))]
fn no_target_is_unrecognized() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let proxy = Proxy::new();
    let imp = imp_for(&proxy, sel!(unknownToEveryone));
    let unknown: unsafe extern "C-unwind" fn(&Proxy, Sel) -> i64 = unsafe { std::mem::transmute(imp) };
    let result = catch_unwind(AssertUnwindSafe(|| unsafe { unknown(&proxy, sel!(unknownToEveryone)) }));
    let message = result.expect_err("an unrecognized selector panics");
    let message = message.downcast_ref::<String>().expect("a message");
    assert!(message.contains("unrecognized selector"), "{message}");
    // The proxy is intact and keeps forwarding.
    let base: unsafe extern "C-unwind" fn(&Proxy, Sel) -> i64 =
        unsafe { std::mem::transmute(imp_for(&proxy, sel!(base))) };
    assert_eq!(unsafe { base(&proxy, sel!(base)) }, 100);
}

#[test]
fn plain_objects_do_not_forward() {
    let obj = NSObject::new();
    assert!(!obj.respondsToSelector(sel!(add:to:)));
}
