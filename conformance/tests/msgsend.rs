//! Sending messages through `objc_msgSend` directly, cast to each method's
//! signature, as code written against the C API does.

use objc2::ffi;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send, sel};

use sidestep as _;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wide([i64; 5]);

unsafe impl objc2::Encode for Wide {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("SidestepSendWide", &[<[i64; 5]>::ENCODING]);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SidestepSendTarget"]
    #[ivars = i64]
    struct Target;

    impl Target {
        #[unsafe(method(add:to:))]
        fn add(&self, a: i64, b: i64) -> i64 {
            *self.ivars() + a + b
        }

        #[unsafe(method(half:))]
        fn half(&self, x: f64) -> f64 {
            x / 2.0
        }

        #[unsafe(method(wide:))]
        fn wide(&self, x: i64) -> Wide {
            Wide([x, x + 1, x + 2, x + 3, *self.ivars()])
        }

        #[unsafe(method(classAnswer))]
        fn class_answer() -> i32 {
            42
        }
    }
);

fn target() -> Retained<Target> {
    let this = Target::alloc().set_ivars(100);
    unsafe { msg_send![super(this), init] }
}

fn msg_send_fn<F: Copy>() -> F {
    assert_eq!(size_of::<F>(), size_of::<unsafe extern "C-unwind" fn()>());
    let f: unsafe extern "C-unwind" fn() = ffi::objc_msgSend;
    unsafe { std::mem::transmute_copy(&f) }
}

#[test]
fn objc_msg_send_directly() {
    let obj = target();
    let receiver = Retained::as_ptr(&obj).cast_mut().cast::<AnyObject>();
    let add: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64, i64) -> i64 = msg_send_fn();
    assert_eq!(unsafe { add(receiver, sel!(add:to:), 1, 2) }, 103);
    let half: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, f64) -> f64 = msg_send_fn();
    assert_eq!(unsafe { half(receiver, sel!(half:), 5.0) }, 2.5);
    let class_answer: unsafe extern "C-unwind" fn(*const objc2::runtime::AnyClass, Sel) -> i32 = msg_send_fn();
    assert_eq!(unsafe { class_answer(Target::class(), sel!(classAnswer)) }, 42);
    // Inherited, and the first message a class gets.
    let hash: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> usize = msg_send_fn();
    assert_eq!(unsafe { hash(receiver, sel!(hash)) }, obj.hash());
}

#[test]
fn objc_msg_send_to_nil() {
    let add: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64, i64) -> i64 = msg_send_fn();
    assert_eq!(unsafe { add(std::ptr::null_mut(), sel!(add:to:), 1, 2) }, 0);
    let half: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, f64) -> f64 = msg_send_fn();
    assert_eq!(unsafe { half(std::ptr::null_mut(), sel!(half:), 5.0) }, 0.0);
}

/// On aarch64 a large struct comes back through the address in x8, which
/// `objc_msgSend` passes on; x86_64 has `objc_msgSend_stret` for it.
#[test]
fn objc_msg_send_returning_a_large_struct() {
    let obj = target();
    let receiver = Retained::as_ptr(&obj).cast_mut().cast::<AnyObject>();
    #[cfg(target_arch = "x86_64")]
    let wide: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64) -> Wide = {
        let f: unsafe extern "C-unwind" fn() = ffi::objc_msgSend_stret;
        unsafe { std::mem::transmute(f) }
    };
    #[cfg(not(target_arch = "x86_64"))]
    let wide: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64) -> Wide = msg_send_fn();
    assert_eq!(unsafe { wide(receiver, sel!(wide:), 7) }, Wide([7, 8, 9, 10, 100]));
}

/// x86_64 returns `long double` on the x87 stack, and `objc_msgSend_fpret`
/// sends messages returning one (to nil it pushes a zero there, so it is
/// only sent to nil with that signature); a live receiver's method is
/// called as it is, so a `double` comes back as usual.
/// `class_getMethodImplementation_stret` is the implementation for a
/// message returning a struct in memory.
#[test]
#[cfg(target_arch = "x86_64")]
fn x86_64_variants() {
    let obj = target();
    let receiver = Retained::as_ptr(&obj).cast_mut().cast::<AnyObject>();
    let half: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, f64) -> f64 = {
        let f: unsafe extern "C-unwind" fn() = ffi::objc_msgSend_fpret;
        unsafe { std::mem::transmute(f) }
    };
    assert_eq!(unsafe { half(receiver, sel!(half:), 5.0) }, 2.5);

    let lookup: unsafe extern "C-unwind" fn(*const objc2::runtime::AnyClass, Sel) -> Option<objc2::runtime::Imp> = {
        let f: unsafe extern "C-unwind" fn() = ffi::class_getMethodImplementation_stret;
        unsafe { std::mem::transmute(f) }
    };
    let imp = unsafe { lookup(Target::class(), sel!(wide:)) }.expect("an implementation");
    let wide: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64) -> Wide = unsafe { std::mem::transmute(imp) };
    assert_eq!(unsafe { wide(receiver, sel!(wide:), 7) }, Wide([7, 8, 9, 10, 100]));
}

/// Answers `sidestepMethodN` with N, for any N: many selectors on one
/// class, so the method cache fills up and selectors share slots.
extern "C-unwind" fn numbered(_: *mut AnyObject, sel: Sel) -> usize {
    sel.name().to_str().unwrap().trim_start_matches("sidestepMethod").parse().unwrap()
}

/// Repeated sends find the method cached; with many selectors on one class,
/// some probe past a slot another selector took.
#[test]
fn objc_msg_send_hits_the_cache() {
    let mut builder = objc2::runtime::ClassBuilder::new(c"SidestepSendNumbered", NSObject::class()).unwrap();
    let sels: Vec<Sel> =
        (0..200).map(|n| Sel::register(&std::ffi::CString::new(format!("sidestepMethod{n}")).unwrap())).collect();
    for &sel in &sels {
        unsafe { builder.add_method(sel, numbered as extern "C-unwind" fn(_, _) -> _) };
    }
    let cls = builder.register();
    let obj: Retained<NSObject> = unsafe { msg_send![cls, new] };
    let receiver = Retained::as_ptr(&obj).cast_mut().cast::<AnyObject>();
    let send: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> usize = msg_send_fn();
    for _ in 0..20 {
        for (n, &sel) in sels.iter().enumerate() {
            assert_eq!(unsafe { send(receiver, sel) }, n);
        }
    }

    let target = target();
    let receiver = Retained::as_ptr(&target).cast_mut().cast::<AnyObject>();
    let add: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64, i64) -> i64 = msg_send_fn();
    let half: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, f64) -> f64 = msg_send_fn();
    let class_answer: unsafe extern "C-unwind" fn(*const objc2::runtime::AnyClass, Sel) -> i32 = msg_send_fn();
    for i in 0..100 {
        assert_eq!(unsafe { add(receiver, sel!(add:to:), i, 2) }, 102 + i);
        assert_eq!(unsafe { half(receiver, sel!(half:), i as f64) }, i as f64 / 2.0);
        assert_eq!(unsafe { class_answer(Target::class(), sel!(classAnswer)) }, 42);
    }

    // The entry points for structs returned in memory and, on x86_64, for
    // floating-point returns, find cached methods too.
    #[cfg(target_arch = "x86_64")]
    let (wide, half): (
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64) -> Wide,
        unsafe extern "C-unwind" fn(*mut AnyObject, Sel, f64) -> f64,
    ) = unsafe {
        let stret: unsafe extern "C-unwind" fn() = ffi::objc_msgSend_stret;
        let fpret: unsafe extern "C-unwind" fn() = ffi::objc_msgSend_fpret;
        (
            std::mem::transmute::<
                unsafe extern "C-unwind" fn(),
                unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64) -> Wide,
            >(stret),
            std::mem::transmute::<
                unsafe extern "C-unwind" fn(),
                unsafe extern "C-unwind" fn(*mut AnyObject, Sel, f64) -> f64,
            >(fpret),
        )
    };
    #[cfg(not(target_arch = "x86_64"))]
    let wide: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, i64) -> Wide = msg_send_fn();
    for i in 0..100 {
        assert_eq!(unsafe { wide(receiver, sel!(wide:), i) }, Wide([i, i + 1, i + 2, i + 3, 100]));
        assert_eq!(unsafe { half(receiver, sel!(half:), i as f64) }, i as f64 / 2.0);
    }
}
