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

mod attributed;
mod charset;
mod const_string;
mod dictionary;
mod geometry;
mod notification;
mod regex;
mod scanner;
mod string;
mod thread;
mod timer;

// Collections and values.
mod array;
mod deque;
mod describe;
mod enumerator;
mod guarded;
mod index_set;
mod null;
mod number;
mod set;
mod table;
mod util;
mod value;

pub use const_string::{ConstStr, ConstantString};
pub use notification::notification;
pub use timer::{fire_due_timers, next_timer_deadline};

#[doc(hidden)]
pub use attributed::{RunRef, with_runs};
#[doc(hidden)]
pub use string::with_str;

/// Make sure a static class shell has been defined, for code that uses a
/// private class's implementation type directly (its `class()` must not run
/// before the runtime has asked the shell's loader to).
pub(crate) fn load_shell(shell: &'static sidestep_runtime::Class) {
    // SAFETY: any runtime function that takes a class loads it first.
    let _ = unsafe { objc2::ffi::class_getInstanceSize((shell as *const sidestep_runtime::Class).cast()) };
}

#[doc(hidden)]
pub mod __private {
    pub use sidestep_runtime::ObjectRef;
}

sidestep_runtime::static_class!(pub NSSTRING, NSSTRING_META = "NSString", string::load);

sidestep_runtime::static_class!(pub NSTHREAD, NSTHREAD_META = "NSThread", || {
    let _ = thread::NSThreadImpl::class();
});

sidestep_runtime::static_class!(pub NSDICTIONARY, NSDICTIONARY_META = "NSDictionary", || {
    let _ = dictionary::NSDictionaryImpl::class();
});

sidestep_runtime::static_class!(pub NSTIMER, NSTIMER_META = "NSTimer", || {
    let _ = timer::NSTimerImpl::class();
});

sidestep_runtime::static_class!(pub NSRUNLOOP, NSRUNLOOP_META = "NSRunLoop", || {
    let _ = timer::NSRunLoopImpl::class();
});

sidestep_runtime::static_class!(pub NSNOTIFICATION, NSNOTIFICATION_META = "NSNotification", || {
    let _ = notification::NSNotificationImpl::class();
});

sidestep_runtime::static_class!(
    pub CONSTANT_STRING_CLASS,
    CONSTANT_STRING_META = "_SidestepConstantString",
    const_string::load
);

// Collections and values.

sidestep_runtime::static_class!(pub NSARRAY, NSARRAY_META = "NSArray", || {
    let _ = array::NSArrayImpl::class();
});

sidestep_runtime::static_class!(pub NSMUTABLEARRAY, NSMUTABLEARRAY_META = "NSMutableArray", || {
    let _ = array::NSMutableArrayImpl::class();
});

sidestep_runtime::static_class!(pub NSMUTABLEDICTIONARY, NSMUTABLEDICTIONARY_META = "NSMutableDictionary", || {
    let _ = dictionary::NSMutableDictionaryImpl::class();
});

sidestep_runtime::static_class!(pub NSSET, NSSET_META = "NSSet", || {
    let _ = set::NSSetImpl::class();
});

sidestep_runtime::static_class!(pub NSMUTABLESET, NSMUTABLESET_META = "NSMutableSet", || {
    let _ = set::NSMutableSetImpl::class();
});

sidestep_runtime::static_class!(pub NSENUMERATOR, NSENUMERATOR_META = "NSEnumerator", || {
    let _ = enumerator::NSEnumeratorImpl::class();
});

sidestep_runtime::static_class!(pub NSVALUE, NSVALUE_META = "NSValue", || {
    let _ = value::NSValueImpl::class();
});

sidestep_runtime::static_class!(pub NSNUMBER, NSNUMBER_META = "NSNumber", || {
    let _ = number::NSNumberImpl::class();
});

sidestep_runtime::static_class!(pub NSNULL, NSNULL_META = "NSNull", || {
    let _ = null::NSNullImpl::class();
});

sidestep_runtime::static_class!(pub NSINDEXSET, NSINDEXSET_META = "NSIndexSet", || {
    let _ = index_set::NSIndexSetImpl::class();
});

sidestep_runtime::static_class!(pub NSMUTABLEINDEXSET, NSMUTABLEINDEXSET_META = "NSMutableIndexSet", || {
    let _ = index_set::NSMutableIndexSetImpl::class();
});
