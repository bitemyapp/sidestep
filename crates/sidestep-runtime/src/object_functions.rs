//! Foundation's C functions over objects that need the runtime's insides:
//! allocating, copying and freeing instances by hand, and the extra
//! reference count that classes managing their own `-retain` keep with
//! `NSIncrementExtraRefCount`. Zones are gone, as on macOS: every zone
//! argument is ignored.
//!
//! As measured on macOS, a new object's extra count is 0 (its
//! `retainCount` less one), incrementing it counts one more reference, and
//! `NSDecrementExtraRefCountWasZero` answers YES, changing nothing, when
//! the count is already 0, so the caller deallocates.

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use crate::class::{Class, class_getInstanceSize};
use crate::object::{DEALLOCATING, IMMORTAL, Object, RC_ONE, class_createInstance, header, isa, object_dispose};

/// `BOOL`.
type Bool = u8;

/// # Safety
///
/// `class` is a class or null.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSAllocateObject(class: *const Class, extra: usize, _zone: *mut c_void) -> *mut Object {
    // SAFETY: forwarded contract.
    unsafe { class_createInstance(class, extra) }
}

/// # Safety
///
/// `object` is a live object the caller owns, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSDeallocateObject(object: *mut Object) {
    // SAFETY: forwarded contract.
    unsafe { object_dispose(object) };
}

/// A new instance of `object`'s class holding a copy of its bytes (a
/// shallow copy: objects it points to aren't retained), with `extra` bytes
/// more, zeroed.
///
/// # Safety
///
/// `object` is a live object, or null.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSCopyObject(object: *mut Object, extra: usize, _zone: *mut c_void) -> *mut Object {
    if object.is_null() {
        return object;
    }
    // SAFETY: a live object's class, and its instance size.
    let (class, size) = unsafe {
        let class = isa(object);
        (class, class_getInstanceSize(class))
    };
    // SAFETY: a class.
    let copy = unsafe { class_createInstance(class, extra) };
    if !copy.is_null() {
        // SAFETY: both hold at least `size` bytes of instance (the isa
        // included), and don't overlap.
        unsafe { std::ptr::copy_nonoverlapping(object.cast::<u8>(), copy.cast::<u8>(), size) };
    }
    copy
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSShouldRetainWithZone(_object: *mut Object, _zone: *mut c_void) -> Bool {
    1
}

/// # Safety
///
/// `object` is a live object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSIncrementExtraRefCount(object: *mut Object) {
    // SAFETY: forwarded contract.
    let h = unsafe { header(object) };
    if h.rc.load(Ordering::Relaxed) & IMMORTAL == 0 {
        h.rc.fetch_add(RC_ONE, Ordering::Relaxed);
    }
}

/// # Safety
///
/// `object` is a live object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSDecrementExtraRefCountWasZero(object: *mut Object) -> Bool {
    // SAFETY: forwarded contract.
    let h = unsafe { header(object) };
    let mut cur = h.rc.load(Ordering::Relaxed);
    loop {
        if cur & (IMMORTAL | DEALLOCATING) != 0 {
            return 0;
        }
        if cur < RC_ONE {
            // The caller deallocates next: what other threads did before
            // their releases happens before that, as for a last release.
            std::sync::atomic::fence(Ordering::Acquire);
            return 1;
        }
        match h.rc.compare_exchange_weak(cur, cur - RC_ONE, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return 0,
            Err(actual) => cur = actual,
        }
    }
}

/// # Safety
///
/// `object` is a live object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSExtraRefCount(object: *mut Object) -> usize {
    // SAFETY: forwarded contract.
    let bits = unsafe { header(object) }.rc.load(Ordering::Relaxed);
    if bits & IMMORTAL != 0 { usize::MAX } else { bits / RC_ONE }
}
