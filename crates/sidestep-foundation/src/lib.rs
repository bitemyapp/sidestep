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

mod base64;
mod bundle;
mod cf;
mod const_string;
pub mod data;
mod date;
mod date_format;
mod date_formatter;
mod dictionary;
mod dispatch;
mod error;
mod file_manager;
mod json;
mod locale;
mod lock;
mod notification;
pub mod notification_center;
mod objc_runtime;
mod operation;
mod path;
mod perform;
mod plist;
mod process_info;
pub mod runloop;
mod string;
mod thread;
mod time_zone;
mod timer;
pub mod url;
mod user_defaults;
mod uuid;
mod xdg;

pub use const_string::{ConstStr, ConstantString};
pub use notification::notification;
pub use runloop::{fire_due_timers, next_timer_deadline};

#[doc(hidden)]
pub mod __private {
    pub use sidestep_runtime::ObjectRef;
}

sidestep_runtime::static_class!(pub NSSTRING, NSSTRING_META = "NSString", || {
    let _ = string::NSStringImpl::class();
});

sidestep_runtime::static_class!(pub NSTHREAD, NSTHREAD_META = "NSThread", || {
    let _ = thread::NSThreadImpl::class();
    perform::install();
});

sidestep_runtime::static_class!(pub NSDICTIONARY, NSDICTIONARY_META = "NSDictionary", || {
    let _ = dictionary::NSDictionaryImpl::class();
});

sidestep_runtime::static_class!(pub NSTIMER, NSTIMER_META = "NSTimer", || {
    let _ = timer::NSTimerImpl::class();
    perform::install();
});

sidestep_runtime::static_class!(pub NSRUNLOOP, NSRUNLOOP_META = "NSRunLoop", || {
    let _ = runloop::nsrunloop::NSRunLoopImpl::class();
    perform::install();
});

sidestep_runtime::static_class!(pub NSNOTIFICATION, NSNOTIFICATION_META = "NSNotification", || {
    let _ = notification::NSNotificationImpl::class();
    perform::install();
});

sidestep_runtime::static_class!(
    pub CONSTANT_STRING_CLASS,
    CONSTANT_STRING_META = "_SidestepConstantString",
    const_string::load
);
