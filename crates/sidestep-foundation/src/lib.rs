//! Foundation for Sidestep: `NSString`, `NSThread` and friends, written in
//! Rust with objc2's `define_class!` and linked in as the classes objc2 and
//! objc2-foundation refer to.
//!
//! Each class here is a static shell (see `sidestep_runtime::static_class!`)
//! whose loader forces the matching `define_class!` type. Code in this crate
//! must reach other classes through their objc2-foundation types (for
//! example `objc2_foundation::NSString::alloc()`), never by calling another
//! implementation type's `class()` directly: that would define the class
//! before the runtime asks for it, and the shell would stay empty.
#![cfg(not(target_vendor = "apple"))]

use objc2::ClassType;

mod string;
mod thread;

sidestep_runtime::static_class!(pub NSSTRING, NSSTRING_META = "NSString", || {
    let _ = string::NSStringImpl::class();
});

sidestep_runtime::static_class!(pub NSTHREAD, NSTHREAD_META = "NSThread", || {
    let _ = thread::NSThreadImpl::class();
});
