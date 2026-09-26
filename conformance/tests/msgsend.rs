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
