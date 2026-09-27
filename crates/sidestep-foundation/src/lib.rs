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
mod base64;
mod bidi;
mod bundle;
mod cf;
mod charset;
mod const_string;
mod constants;
pub mod data;
mod date;
mod date_format;
mod date_formatter;
mod dictionary;
mod dispatch;
mod error;
mod file_manager;
mod file_wrapper;
mod functions;
mod geometry;
mod invocation;
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
mod regex;
mod resource_values;
pub mod runloop;
mod scanner;
mod string;
mod thread;
mod time_zone;
mod timer;
mod undo;
mod url;
mod user_defaults;
mod uuid;
mod xdg;

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

// Collections follow-on: each module declares its classes' shells.
mod cache;
mod counted_set;
mod hash_index;
mod ordered_set;
mod pointer_array;
mod pointer_table;
mod sort_descriptor;

// Geometry.
mod affine;

pub use const_string::{ConstStr, ConstantString};
pub use notification::notification;

#[doc(hidden)]
pub use attributed::{RunRef, clip_to_limit, with_runs};
#[doc(hidden)]
pub use string::with_str;

/// Make sure a static class shell has been defined, for code that uses a
/// private class's implementation type directly (its `class()` must not run
/// before the runtime has asked the shell's loader to).
pub(crate) fn load_shell(shell: &'static sidestep_runtime::Class) {
    // SAFETY: any runtime function that takes a class loads it first.
    let _ = unsafe { objc2::ffi::class_getInstanceSize((shell as *const sidestep_runtime::Class).cast()) };
}

/// Mac Roman bytes as text (for CoreGraphics' `CGContextShowText`).
#[doc(hidden)]
pub fn decode_mac_roman(bytes: &[u8]) -> String {
    cf::string::decode(bytes, cf::string::encoding::MAC_ROMAN, false).unwrap_or_default()
}

#[doc(hidden)]
pub mod __private {
    pub use sidestep_runtime::ObjectRef;
}

/// The CoreFoundation type IDs of CoreGraphics', ImageIO's and CoreText's
/// types, for Sidestep's AppKit, which defines them (`CFGetTypeID` works
/// them out from the object's class, by name).
#[doc(hidden)]
pub mod cf_type_ids {
    pub use crate::cf::types::id::{
        CG_COLOR, CG_COLOR_SPACE, CG_CONTEXT, CG_DATA_CONSUMER, CG_DATA_PROVIDER, CG_FONT, CG_FUNCTION, CG_GRADIENT,
        CG_IMAGE, CG_IMAGE_DESTINATION, CG_IMAGE_SOURCE, CG_PATH, CG_PATTERN, CG_SHADING, CT_FONT, CT_FONT_COLLECTION,
        CT_FONT_DESCRIPTOR, CT_FRAME, CT_FRAMESETTER, CT_GLYPH_INFO, CT_LINE, CT_PARAGRAPH_STYLE, CT_RUBY_ANNOTATION,
        CT_RUN, CT_RUN_DELEGATE, CT_TEXT_TAB, CT_TYPESETTER,
    };
}

sidestep_runtime::static_class!(pub NSSTRING, NSSTRING_META = "NSString", string::load);

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

sidestep_runtime::static_class!(pub NSAFFINETRANSFORM, NSAFFINETRANSFORM_META = "NSAffineTransform", || {
    let _ = affine::NSAffineTransformImpl::class();
});
