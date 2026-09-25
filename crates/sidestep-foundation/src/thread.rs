//! `NSThread`. For now only what `MainThreadMarker` needs.

use objc2::define_class;
use objc2::runtime::{NSObject, NSObjectProtocol};

unsafe extern "C" {
    fn getpid() -> i32;
    fn gettid() -> i32;
}

/// The main thread is the process's initial thread, whose thread id equals
/// the process id.
pub(crate) fn is_main_thread() -> bool {
    // SAFETY: plain libc calls.
    unsafe { gettid() == getpid() }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSThread"]
    pub(crate) struct NSThreadImpl;

    impl NSThreadImpl {
        #[unsafe(method(isMainThread))]
        fn class_is_main_thread() -> bool {
            is_main_thread()
        }

        #[unsafe(method(isMainThread))]
        fn is_main_thread(&self) -> bool {
            is_main_thread()
        }
    }

    unsafe impl NSObjectProtocol for NSThreadImpl {}
);
