//! `CFRetain` and the other functions every CoreFoundation type shares.

use std::ffi::c_void;

use objc2::msg_send;
use objc2::runtime::AnyObject;

/// `CFAbsoluteTime`: seconds since 2001-01-01 00:00:00 UTC.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAbsoluteTimeGetCurrent() -> f64 {
    crate::date::now()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRetain(cf: *mut c_void) -> *mut c_void {
    // SAFETY: the caller passes a live object (CoreFoundation objects are
    // Objective-C objects here).
    unsafe { objc2::ffi::objc_retain(cf.cast()).cast() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFRelease(cf: *mut c_void) {
    // SAFETY: the caller owns a reference to a live object.
    unsafe { objc2::ffi::objc_release(cf.cast()) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAutorelease(cf: *mut c_void) -> *mut c_void {
    // SAFETY: as for CFRelease.
    unsafe { objc2::ffi::objc_autorelease(cf.cast()).cast() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFGetRetainCount(cf: *const c_void) -> isize {
    // SAFETY: the caller passes a live object; -retainCount takes nothing.
    let count: usize = unsafe { msg_send![object(cf), retainCount] };
    count as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFEqual(a: *const c_void, b: *const c_void) -> u8 {
    if a == b {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    // Arrays of values that needn't be objects compare by their callbacks
    // either way round.
    // SAFETY: the caller passes live objects.
    if let Some(equal) = unsafe { super::value_array::cf_equal(a, b) } {
        return equal as u8;
    }
    // SAFETY: the caller passes live objects; -isEqual: returns BOOL.
    let equal: bool = unsafe { msg_send![object(a), isEqual: object(b)] };
    equal as u8
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFHash(cf: *const c_void) -> usize {
    // SAFETY: the caller passes a live object; -hash returns NSUInteger.
    unsafe { msg_send![object(cf), hash] }
}

/// # Safety
/// `cf` must be a live object.
unsafe fn object<'a>(cf: *const c_void) -> &'a AnyObject {
    // SAFETY: guaranteed by the caller.
    unsafe { &*cf.cast::<AnyObject>() }
}
