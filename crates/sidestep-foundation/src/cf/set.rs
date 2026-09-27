//! `CFSet` over `NSSet` and `NSMutableSet`.
//!
//! As with dictionaries, only sets of CoreFoundation objects are
//! supported: creating one with callbacks other than `kCFTypeSetCallBacks`
//! or `kCFCopyStringSetCallBacks` returns NULL, since Foundation's sets
//! retain what they hold. A set made with `kCFCopyStringSetCallBacks` (and
//! its mutable copies) holds copies of what goes in, as macOS's does.

use std::ffi::c_void;

use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2::{ClassType, Message};
use objc2_foundation::{NSMutableSet, NSSet};

use super::collections::KeyCallBacks;
use super::types::{CFTypeID, id, object, owned};

type CFIndex = isize;
type Boolean = u8;
type Applier = unsafe extern "C-unwind" fn(*const c_void, *mut c_void);

#[unsafe(no_mangle)]
pub static kCFTypeSetCallBacks: KeyCallBacks = KeyCallBacks::OBJECTS;

#[unsafe(no_mangle)]
pub static kCFCopyStringSetCallBacks: KeyCallBacks = KeyCallBacks::OBJECTS;

fn object_values(callbacks: *const KeyCallBacks) -> bool {
    std::ptr::eq(callbacks, &kCFTypeSetCallBacks) || copies(callbacks)
}

fn copies(callbacks: *const KeyCallBacks) -> bool {
    std::ptr::eq(callbacks, &kCFCopyStringSetCallBacks)
}

/// What marks a set made with `kCFCopyStringSetCallBacks`.
static COPYING: u8 = 0;

/// Mark `cf`, a set, as one holding copies.
fn copying(cf: *mut c_void) -> *mut c_void {
    if !cf.is_null() {
        // SAFETY: `cf` is a live object; the key is a static address, and
        // so is the value, assigned (never retained or read as an object).
        unsafe {
            objc2::ffi::objc_setAssociatedObject(
                cf.cast(),
                (&raw const COPYING).cast(),
                (&raw const COPYING).cast_mut().cast(),
                objc2::ffi::OBJC_ASSOCIATION_ASSIGN,
            )
        };
    }
    cf
}

/// Whether `cf`, a set, holds copies.
fn holds_copies(cf: *const c_void) -> bool {
    // SAFETY: `cf` is a live object; the key is a static address.
    !unsafe { objc2::ffi::objc_getAssociatedObject(cf.cast(), (&raw const COPYING).cast()) }.is_null()
}

/// What goes into `cf` for `value`: a copy if the set holds copies.
///
/// # Safety
///
/// `cf` is a set; `value` an object (one that copies, if the set holds
/// copies).
unsafe fn entering(cf: *const c_void, value: *const c_void) -> Retained<AnyObject> {
    // SAFETY: per this function's contract.
    let value = unsafe { object(value) };
    if holds_copies(cf) {
        // SAFETY: -copy returns a +1 object.
        unsafe { msg_send![value, copy] }
    } else {
        value.retain()
    }
}

fn set<'a>(cf: *const c_void) -> &'a NSSet {
    // SAFETY: the callers' contracts: `cf` is a set.
    unsafe { &*cf.cast::<NSSet>() }
}

fn mutable_set<'a>(cf: *mut c_void) -> &'a NSMutableSet {
    // SAFETY: the callers' contracts: `cf` is a mutable set.
    unsafe { &*cf.cast::<NSMutableSet>() }
}

/// The member equal to `value`, not retained (the set holds it).
///
/// # Safety
///
/// `cf` is a set; `value` an object.
unsafe fn member(cf: *const c_void, value: *const c_void) -> *const c_void {
    if value.is_null() {
        return std::ptr::null();
    }
    // SAFETY: per this function's contract; -member: returns a member or nil.
    let found: *const AnyObject = unsafe { msg_send![set(cf), member: object(value)] };
    found.cast()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFSetGetTypeID() -> CFTypeID {
    id::SET
}

/// # Safety
///
/// `values` points to `count` objects.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetCreate(
    _alloc: *const c_void,
    values: *const *const c_void,
    count: CFIndex,
    callbacks: *const KeyCallBacks,
) -> *mut c_void {
    if !object_values(callbacks) {
        return std::ptr::null_mut();
    }
    let copied: Vec<Retained<AnyObject>>;
    let values = if copies(callbacks) && count > 0 && !values.is_null() {
        // SAFETY: per this function's contract: `count` objects, which copy.
        copied = (0..count as usize).map(|i| unsafe { msg_send![object(*values.add(i)), copy] }).collect();
        copied.as_ptr().cast::<*const c_void>()
    } else {
        values
    };
    // SAFETY: -initWithObjects:count: copies `count` objects.
    let made: Retained<AnyObject> = unsafe {
        let this: Allocated<AnyObject> = msg_send![NSSet::<AnyObject>::class(), alloc];
        msg_send![this, initWithObjects: values.cast::<*const AnyObject>(), count: count.max(0) as usize]
    };
    let made = owned(made);
    if copies(callbacks) { copying(made) } else { made }
}

/// # Safety
///
/// `cf` is a set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetCreateCopy(_alloc: *const c_void, cf: *const c_void) -> *mut c_void {
    // SAFETY: -copy returns an immutable set.
    let copy: Retained<AnyObject> = unsafe { msg_send![set(cf), copy] };
    let copy = owned(copy);
    if holds_copies(cf) && copy.cast_const() != cf { copying(copy) } else { copy }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFSetCreateMutable(
    _alloc: *const c_void,
    _capacity: CFIndex,
    callbacks: *const KeyCallBacks,
) -> *mut c_void {
    if !object_values(callbacks) {
        return std::ptr::null_mut();
    }
    let made = owned(NSMutableSet::<AnyObject>::new());
    if copies(callbacks) { copying(made) } else { made }
}

/// # Safety
///
/// `cf` is a set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetCreateMutableCopy(
    _alloc: *const c_void,
    _capacity: CFIndex,
    cf: *const c_void,
) -> *mut c_void {
    // SAFETY: -mutableCopy returns a mutable set.
    let copy: Retained<AnyObject> = unsafe { msg_send![set(cf), mutableCopy] };
    let copy = owned(copy);
    if holds_copies(cf) { copying(copy) } else { copy }
}

/// # Safety
///
/// `cf` is a set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetGetCount(cf: *const c_void) -> CFIndex {
    set(cf).count() as CFIndex
}

/// # Safety
///
/// `cf` is a set; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetGetCountOfValue(cf: *const c_void, value: *const c_void) -> CFIndex {
    // SAFETY: per this function's contract.
    CFIndex::from(!unsafe { member(cf, value) }.is_null())
}

/// # Safety
///
/// `cf` is a set; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetContainsValue(cf: *const c_void, value: *const c_void) -> Boolean {
    // SAFETY: per this function's contract.
    u8::from(!unsafe { member(cf, value) }.is_null())
}

/// # Safety
///
/// `cf` is a set; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetGetValue(cf: *const c_void, value: *const c_void) -> *const c_void {
    // SAFETY: per this function's contract.
    unsafe { member(cf, value) }
}

/// # Safety
///
/// `cf` is a set; `candidate` an object; `value` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetGetValueIfPresent(
    cf: *const c_void,
    candidate: *const c_void,
    value: *mut *const c_void,
) -> Boolean {
    // SAFETY: per this function's contract.
    let found = unsafe { member(cf, candidate) };
    if !found.is_null() && !value.is_null() {
        // SAFETY: as above.
        unsafe { value.write(found) };
    }
    u8::from(!found.is_null())
}

/// # Safety
///
/// `cf` is a set; `values` has room for its count.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetGetValues(cf: *const c_void, values: *mut *const c_void) {
    if values.is_null() {
        return;
    }
    for (i, value) in set(cf).allObjects().iter().enumerate() {
        // SAFETY: the caller's buffer holds the count; the set holds the
        // objects.
        unsafe { values.add(i).write(Retained::as_ptr(&value).cast()) };
    }
}

/// # Safety
///
/// `cf` is a set; `applier` a function taking its values and `context`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetApplyFunction(cf: *const c_void, applier: Option<Applier>, context: *mut c_void) {
    let Some(applier) = applier else { return };
    for value in set(cf).allObjects().iter() {
        // SAFETY: per this function's contract; the snapshot holds it.
        unsafe { applier(Retained::as_ptr(&value).cast(), context) };
    }
}

/// Add `value` unless an equal member is there.
///
/// # Safety
///
/// `cf` is a mutable set; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetAddValue(cf: *mut c_void, value: *const c_void) {
    if value.is_null() {
        return;
    }
    // SAFETY: per this function's contract.
    if holds_copies(cf) && !unsafe { member(cf, value) }.is_null() {
        return;
    }
    // SAFETY: per this function's contract; -addObject: keeps a member.
    let () = unsafe { msg_send![mutable_set(cf), addObject: &*entering(cf, value)] };
}

/// Put `value` in place of an equal member, if there is one.
///
/// # Safety
///
/// As [`CFSetAddValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetReplaceValue(cf: *mut c_void, value: *const c_void) {
    // SAFETY: per this function's contract.
    if unsafe { member(cf, value) }.is_null() {
        return;
    }
    // SAFETY: as above.
    unsafe { replace(cf, value) };
}

/// Put `value` in, in place of an equal member if there is one.
///
/// # Safety
///
/// As [`CFSetAddValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetSetValue(cf: *mut c_void, value: *const c_void) {
    // SAFETY: per this function's contract.
    unsafe { replace(cf, value) };
}

/// # Safety
///
/// As [`CFSetAddValue`].
unsafe fn replace(cf: *mut c_void, value: *const c_void) {
    if value.is_null() {
        return;
    }
    // Held while the member it may equal goes.
    // SAFETY: per the callers' contracts.
    let value = unsafe { entering(cf, value) };
    // SAFETY: a mutable set and a live object.
    unsafe {
        let () = msg_send![mutable_set(cf), removeObject: &*value];
        let () = msg_send![mutable_set(cf), addObject: &*value];
    }
}

/// # Safety
///
/// `cf` is a mutable set; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetRemoveValue(cf: *mut c_void, value: *const c_void) {
    if value.is_null() {
        return;
    }
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_set(cf), removeObject: object(value)] };
}

/// # Safety
///
/// `cf` is a mutable set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFSetRemoveAllValues(cf: *mut c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_set(cf), removeAllObjects] };
}
