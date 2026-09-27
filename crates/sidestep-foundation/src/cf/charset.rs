//! `CFCharacterSet` over `NSCharacterSet` and `NSMutableCharacterSet`.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, msg_send};
use objc2_foundation::{NSCharacterSet, NSData, NSMutableCharacterSet, NSRange, NSString};

use super::string::CFRange;
use super::types::{CFTypeID, id, object, owned};

type CFIndex = isize;
type Boolean = u8;

fn set<'a>(cf: *const c_void) -> &'a NSCharacterSet {
    // SAFETY: the callers' contracts: `cf` is a character set.
    unsafe { &*cf.cast::<NSCharacterSet>() }
}

fn mutable<'a>(cf: *mut c_void) -> &'a NSMutableCharacterSet {
    // SAFETY: the callers' contracts: `cf` is a mutable character set.
    unsafe { &*cf.cast::<NSMutableCharacterSet>() }
}

fn ns_range(range: CFRange) -> NSRange {
    NSRange::new(range.location.max(0) as usize, range.length.max(0) as usize)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFCharacterSetGetTypeID() -> CFTypeID {
    id::CHARACTER_SET
}

/// One of the predefined sets (`kCFCharacterSetControl` = 1 to
/// `kCFCharacterSetNewline` = 15), shared, as `NSCharacterSet`'s are.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFCharacterSetGetPredefined(which: CFIndex) -> *const c_void {
    let set = match which {
        1 => NSCharacterSet::controlCharacterSet(),
        2 => NSCharacterSet::whitespaceCharacterSet(),
        3 => NSCharacterSet::whitespaceAndNewlineCharacterSet(),
        4 => NSCharacterSet::decimalDigitCharacterSet(),
        5 => NSCharacterSet::letterCharacterSet(),
        6 => NSCharacterSet::lowercaseLetterCharacterSet(),
        7 => NSCharacterSet::uppercaseLetterCharacterSet(),
        8 => NSCharacterSet::nonBaseCharacterSet(),
        9 => NSCharacterSet::decomposableCharacterSet(),
        10 => NSCharacterSet::alphanumericCharacterSet(),
        11 => NSCharacterSet::punctuationCharacterSet(),
        12 => NSCharacterSet::illegalCharacterSet(),
        13 => NSCharacterSet::capitalizedLetterCharacterSet(),
        14 => NSCharacterSet::symbolCharacterSet(),
        15 => NSCharacterSet::newlineCharacterSet(),
        _ => return std::ptr::null(),
    };
    // The predefined sets live for the rest of the process.
    Retained::as_ptr(&set).cast()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFCharacterSetCreateWithCharactersInRange(
    _alloc: *const c_void,
    range: CFRange,
) -> *mut c_void {
    owned(NSCharacterSet::characterSetWithRange(ns_range(range)))
}

/// # Safety
///
/// `string` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetCreateWithCharactersInString(
    _alloc: *const c_void,
    string: *const c_void,
) -> *mut c_void {
    let string = if string.is_null() {
        crate::string::empty()
    } else {
        // SAFETY: per this function's contract.
        objc2::Message::retain(unsafe { &*string.cast::<NSString>() })
    };
    owned(NSCharacterSet::characterSetWithCharactersInString(&string))
}

/// # Safety
///
/// `data` is null or a data object holding a bitmap representation.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetCreateWithBitmapRepresentation(
    _alloc: *const c_void,
    data: *const c_void,
) -> *mut c_void {
    let data = if data.is_null() {
        NSData::new()
    } else {
        // SAFETY: per this function's contract.
        objc2::Message::retain(unsafe { &*data.cast::<NSData>() })
    };
    owned(crate::charset::with_bitmap(NSCharacterSet::class(), &data))
}

/// # Safety
///
/// `cf` is null or a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetCreateInvertedSet(
    _alloc: *const c_void,
    cf: *const c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    owned(set(cf).invertedSet())
}

/// # Safety
///
/// `cf` is null or a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetCreateCopy(_alloc: *const c_void, cf: *const c_void) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -copy returns an immutable set.
    let copy: Retained<AnyObject> = unsafe { msg_send![set(cf), copy] };
    owned(copy)
}

/// # Safety
///
/// `cf` is null or a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetCreateMutableCopy(
    _alloc: *const c_void,
    cf: *const c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -mutableCopy returns a mutable set.
    let copy: Retained<AnyObject> = unsafe { msg_send![set(cf), mutableCopy] };
    owned(copy)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFCharacterSetCreateMutable(_alloc: *const c_void) -> *mut c_void {
    owned(NSMutableCharacterSet::new())
}

/// # Safety
///
/// `cf` is null or a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetCreateBitmapRepresentation(
    _alloc: *const c_void,
    cf: *const c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    owned(NSData::with_bytes(&crate::charset::bitmap(unsafe { object(cf) })))
}

/// # Safety
///
/// `cf` is a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetIsCharacterMember(cf: *const c_void, c: u16) -> Boolean {
    u8::from(set(cf).characterIsMember(c))
}

/// # Safety
///
/// `cf` is a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetIsLongCharacterMember(cf: *const c_void, c: u32) -> Boolean {
    u8::from(set(cf).longCharacterIsMember(c))
}

/// # Safety
///
/// `cf` is a character set; `other` null or one.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetIsSupersetOfSet(cf: *const c_void, other: *const c_void) -> Boolean {
    if other.is_null() {
        return 0;
    }
    u8::from(set(cf).isSupersetOfSet(set(other)))
}

/// # Safety
///
/// `cf` is a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetHasMemberInPlane(cf: *const c_void, plane: CFIndex) -> Boolean {
    if !(0..=16).contains(&plane) {
        return 0;
    }
    u8::from(set(cf).hasMemberInPlane(plane as u8))
}

/// # Safety
///
/// `cf` is a mutable character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetAddCharactersInRange(cf: *mut c_void, range: CFRange) {
    mutable(cf).addCharactersInRange(ns_range(range));
}

/// # Safety
///
/// `cf` is a mutable character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetRemoveCharactersInRange(cf: *mut c_void, range: CFRange) {
    mutable(cf).removeCharactersInRange(ns_range(range));
}

/// # Safety
///
/// `cf` is a mutable character set; `string` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetAddCharactersInString(cf: *mut c_void, string: *const c_void) {
    if !string.is_null() {
        // SAFETY: per this function's contract.
        mutable(cf).addCharactersInString(unsafe { &*string.cast::<NSString>() });
    }
}

/// # Safety
///
/// `cf` is a mutable character set; `string` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetRemoveCharactersInString(cf: *mut c_void, string: *const c_void) {
    if !string.is_null() {
        // SAFETY: per this function's contract.
        mutable(cf).removeCharactersInString(unsafe { &*string.cast::<NSString>() });
    }
}

/// # Safety
///
/// `cf` is a mutable character set; `other` null or a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetUnion(cf: *mut c_void, other: *const c_void) {
    if !other.is_null() {
        mutable(cf).formUnionWithCharacterSet(set(other));
    }
}

/// # Safety
///
/// `cf` is a mutable character set; `other` null or a character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetIntersect(cf: *mut c_void, other: *const c_void) {
    if !other.is_null() {
        mutable(cf).formIntersectionWithCharacterSet(set(other));
    }
}

/// # Safety
///
/// `cf` is a mutable character set.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFCharacterSetInvert(cf: *mut c_void) {
    mutable(cf).invert();
}
