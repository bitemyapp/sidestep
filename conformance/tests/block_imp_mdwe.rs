//! Methods implemented by blocks in a process the kernel denies memory
//! that is writable and then executable (`PR_SET_MDWE`, which systemd's
//! `MemoryDenyWriteExecute=` also asks for). Linux only; the setting lasts
//! for the rest of the process, hence a file of its own.

#[cfg(target_os = "linux")]
mod linux {
    use block2::RcBlock;
    use objc2::ffi;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ClassBuilder, NSObject};
    use objc2::{ClassType, msg_send, sel};

    use sidestep as _;

    const PR_SET_MDWE: i32 = 65;
    const PR_MDWE_REFUSE_EXEC_GAIN: u64 = 1;

    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }

    #[test]
    fn block_methods_without_writable_executable_memory() {
        // Kernels before 6.3 don't have the setting; nothing to test there.
        if unsafe { prctl(PR_SET_MDWE, PR_MDWE_REFUSE_EXEC_GAIN, 0u64, 0u64, 0u64) } != 0 {
            eprintln!("PR_SET_MDWE unsupported: {}", std::io::Error::last_os_error());
            return;
        }
        let class = ClassBuilder::new(c"SidestepBlockImpMdwe", NSObject::class()).unwrap().register();
        let block = RcBlock::new(|_this: *mut AnyObject, a: i64| -> i64 { a * 3 });
        let imp = unsafe { ffi::imp_implementationWithBlock(RcBlock::as_ptr(&block).cast::<AnyObject>()) };
        let class_ptr = (class as *const objc2::runtime::AnyClass).cast_mut();
        assert!(unsafe { ffi::class_addMethod(class_ptr, sel!(triple:), imp, c"q@:q".as_ptr()) }.as_bool());
        let obj: Retained<AnyObject> = unsafe { msg_send![class, new] };
        let got: i64 = unsafe { msg_send![&*obj, triple: 14i64] };
        assert_eq!(got, 42);
    }
}
