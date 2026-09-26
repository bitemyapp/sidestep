//! What Apple's runtime raises as Objective-C exceptions: an explicit
//! `objc_exception_throw`, and a collection mutated while it is enumerated
//! with no handler set. Sidestep panics with the exception's description,
//! which Rust can catch; on Apple's runtime an uncaught Objective-C
//! exception aborts the process, so these run on Linux only. The mutation
//! check's default needs a process where no test installs a handler, hence
//! a file of its own.

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use objc2::ffi;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};

    use sidestep as _;

    // objc2 declares this only for Apple's runtime.
    unsafe extern "C-unwind" {
        fn objc_enumerationMutation(obj: *mut AnyObject);
    }

    fn panic_message(f: impl FnOnce()) -> String {
        let payload = catch_unwind(AssertUnwindSafe(f)).expect_err("a panic");
        payload.downcast_ref::<String>().cloned().expect("a formatted message")
    }

    #[test]
    fn exception_throw_names_the_class() {
        let exception = NSObject::new();
        let ptr = Retained::as_ptr(&exception).cast_mut().cast::<AnyObject>();
        let message = panic_message(|| unsafe { ffi::objc_exception_throw(ptr) });
        assert!(message.contains("<NSObject: 0x"), "{message}");
    }

    #[test]
    fn enumeration_mutation_panics_by_default() {
        let collection = NSObject::new();
        let ptr = Retained::as_ptr(&collection).cast_mut().cast::<AnyObject>();
        let message = panic_message(|| unsafe { objc_enumerationMutation(ptr) });
        assert!(message.contains("*** Collection <NSObject: 0x"), "{message}");
        assert!(message.ends_with("was mutated while being enumerated."), "{message}");
    }
}
