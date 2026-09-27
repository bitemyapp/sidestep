//! `CFData`, `CFDate` and `CFError` functions over their Foundation
//! classes.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, msg_send};
use objc2_foundation::{NSData, NSDate, NSError, NSMutableData};

use super::string::CFRange;
use super::types::{CFTypeID, id, is_null_allocator, object, owned};

type CFIndex = isize;

fn data<'a>(cf: *const c_void) -> &'a NSData {
    // SAFETY: the callers' contracts: `cf` is a data object.
    unsafe { &*cf.cast::<NSData>() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFDataGetTypeID() -> CFTypeID {
    id::DATA
}

/// # Safety
///
/// `bytes` points to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataCreate(_alloc: *const c_void, bytes: *const u8, length: CFIndex) -> *mut c_void {
    let bytes = if length <= 0 || bytes.is_null() {
        &[][..]
    } else {
        // SAFETY: per this function's contract.
        unsafe { std::slice::from_raw_parts(bytes, length as usize) }
    };
    owned(NSData::with_bytes(bytes))
}

/// # Safety
///
/// As [`CFDataCreate`]; the bytes were allocated with `malloc` unless
/// `deallocator` is `kCFAllocatorNull`, and stay valid until the data
/// frees them.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataCreateWithBytesNoCopy(
    _alloc: *const c_void,
    bytes: *const u8,
    length: CFIndex,
    deallocator: *const c_void,
) -> *mut c_void {
    let free = !is_null_allocator(deallocator);
    // SAFETY: the caller's buffer, which the data now owns (and frees with
    // free(3)) or borrows.
    let data: Retained<NSData> = unsafe {
        let this: objc2::rc::Allocated<NSData> = msg_send![NSData::class(), alloc];
        msg_send![this, initWithBytesNoCopy: bytes.cast_mut().cast::<c_void>(), length: length.max(0) as usize, freeWhenDone: free]
    };
    owned(data)
}

/// # Safety
///
/// `cf` is a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataCreateCopy(_alloc: *const c_void, cf: *const c_void) -> *mut c_void {
    // SAFETY: nothing mutates the data while it is copied.
    owned(NSData::with_bytes(unsafe { crate::data::bytes(data(cf)) }))
}

/// # Safety
///
/// `cf` is a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataGetLength(cf: *const c_void) -> CFIndex {
    // SAFETY: only the length is read.
    unsafe { crate::data::bytes(data(cf)) }.len() as CFIndex
}

/// # Safety
///
/// `cf` is a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataGetBytePtr(cf: *const c_void) -> *const u8 {
    // SAFETY: -bytes returns the data's buffer.
    let bytes: *const c_void = unsafe { msg_send![data(cf), bytes] };
    bytes.cast()
}

/// # Safety
///
/// `cf` is a data object; `buffer` has room for `range.length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataGetBytes(cf: *const c_void, range: CFRange, buffer: *mut u8) {
    // SAFETY: nothing mutates the data while it is copied out.
    let bytes = unsafe { crate::data::bytes(data(cf)) };
    let (Ok(start), Ok(length)) = (usize::try_from(range.location), usize::try_from(range.length)) else { return };
    if let Some(part) = bytes.get(start..start.saturating_add(length)) {
        // SAFETY: the caller's buffer has room for the range.
        unsafe { std::ptr::copy_nonoverlapping(part.as_ptr(), buffer, part.len()) };
    }
}

/// Where `find`'s bytes first (or, backwards, last) occur within the
/// range, or at its start (or end) only, anchored; `{kCFNotFound, 0}` if
/// they don't.
///
/// # Safety
///
/// `cf` and `find` are data objects.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataFind(
    cf: *const c_void,
    find: *const c_void,
    range: CFRange,
    flags: usize,
) -> CFRange {
    const BACKWARDS: usize = 1;
    const ANCHORED: usize = 2;
    let not_found = CFRange { location: -1, length: 0 };
    if find.is_null() {
        return not_found;
    }
    // SAFETY: nothing mutates either while they are read.
    let (haystack, needle) = unsafe { (crate::data::bytes(data(cf)), crate::data::bytes(data(find))) };
    let (Ok(start), Ok(length)) = (usize::try_from(range.location), usize::try_from(range.length)) else {
        return not_found;
    };
    let Some(within) = haystack.get(start..start.saturating_add(length).min(haystack.len())) else { return not_found };
    if needle.is_empty() || needle.len() > within.len() {
        return not_found;
    }
    let last = within.len() - needle.len();
    let at = match (flags & BACKWARDS != 0, flags & ANCHORED != 0) {
        (false, false) => within.windows(needle.len()).position(|w| w == needle),
        (true, false) => within.windows(needle.len()).rposition(|w| w == needle),
        (false, true) => within.starts_with(needle).then_some(0),
        (true, true) => within.ends_with(needle).then_some(last),
    };
    at.map_or(not_found, |at| CFRange { location: (start + at) as CFIndex, length: needle.len() as CFIndex })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFDataCreateMutable(_alloc: *const c_void, _capacity: CFIndex) -> *mut c_void {
    owned(NSMutableData::new())
}

/// # Safety
///
/// `cf` is a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataCreateMutableCopy(
    _alloc: *const c_void,
    _capacity: CFIndex,
    cf: *const c_void,
) -> *mut c_void {
    // SAFETY: nothing mutates the data while it is copied.
    owned(NSMutableData::with_bytes(unsafe { crate::data::bytes(data(cf)) }))
}

fn mutable<'a>(cf: *mut c_void) -> &'a NSMutableData {
    // SAFETY: the callers' contracts: `cf` is mutable data.
    unsafe { &*cf.cast::<NSMutableData>() }
}

/// # Safety
///
/// `cf` is mutable data.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataGetMutableBytePtr(cf: *mut c_void) -> *mut u8 {
    // SAFETY: -mutableBytes returns the buffer.
    let bytes: *mut c_void = unsafe { msg_send![mutable(cf), mutableBytes] };
    bytes.cast()
}

/// # Safety
///
/// `cf` is mutable data.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataSetLength(cf: *mut c_void, length: CFIndex) {
    // SAFETY: -setLength: takes NSUInteger.
    let () = unsafe { msg_send![mutable(cf), setLength: length.max(0) as usize] };
}

/// # Safety
///
/// `cf` is mutable data.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataIncreaseLength(cf: *mut c_void, extra: CFIndex) {
    // SAFETY: -increaseLengthBy: takes NSUInteger.
    let () = unsafe { msg_send![mutable(cf), increaseLengthBy: extra.max(0) as usize] };
}

/// # Safety
///
/// `cf` is mutable data; `bytes` points to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataAppendBytes(cf: *mut c_void, bytes: *const u8, length: CFIndex) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable(cf), appendBytes: bytes.cast::<c_void>(), length: length.max(0) as usize] };
}

/// # Safety
///
/// `cf` is mutable data; `bytes` points to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataReplaceBytes(cf: *mut c_void, range: CFRange, bytes: *const u8, length: CFIndex) {
    let range = objc2_foundation::NSRange::new(range.location.max(0) as usize, range.length.max(0) as usize);
    // SAFETY: per this function's contract.
    let () = unsafe {
        msg_send![mutable(cf), replaceBytesInRange: range, withBytes: bytes.cast::<c_void>(), length: length.max(0) as usize]
    };
}

/// # Safety
///
/// `cf` is mutable data.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDataDeleteBytes(cf: *mut c_void, range: CFRange) {
    // SAFETY: as CFDataReplaceBytes, with nothing to insert.
    unsafe { CFDataReplaceBytes(cf, range, std::ptr::null(), 0) }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFDateGetTypeID() -> CFTypeID {
    id::DATE
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFDateCreate(_alloc: *const c_void, at: f64) -> *mut c_void {
    owned(NSDate::dateWithTimeIntervalSinceReferenceDate(at))
}

/// # Safety
///
/// `date` is a date.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDateGetAbsoluteTime(date: *const c_void) -> f64 {
    // SAFETY: per this function's contract.
    crate::date::time_of(unsafe { &*date.cast::<NSDate>() })
}

/// # Safety
///
/// Both are dates.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDateGetTimeIntervalSinceDate(date: *const c_void, other: *const c_void) -> f64 {
    // SAFETY: per this function's contract.
    unsafe { CFDateGetAbsoluteTime(date) - CFDateGetAbsoluteTime(other) }
}

/// # Safety
///
/// Both are dates.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDateCompare(
    date: *const c_void,
    other: *const c_void,
    _context: *mut c_void,
) -> CFIndex {
    // SAFETY: per this function's contract.
    let (a, b) = unsafe { (CFDateGetAbsoluteTime(date), CFDateGetAbsoluteTime(other)) };
    a.partial_cmp(&b).map_or(0, |o| o as CFIndex)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFErrorGetTypeID() -> CFTypeID {
    id::ERROR
}

/// # Safety
///
/// `domain` is a string; `user_info` is null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorCreate(
    _alloc: *const c_void,
    domain: *const c_void,
    code: CFIndex,
    user_info: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let error: Retained<NSError> = unsafe {
        let this: objc2::rc::Allocated<NSError> = msg_send![NSError::class(), alloc];
        let info = user_info.cast::<AnyObject>().as_ref();
        msg_send![this, initWithDomain: object(domain), code: code, userInfo: info]
    };
    owned(error)
}

/// # Safety
///
/// `domain` is a string; `keys` and `values` point to `count` objects.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorCreateWithUserInfoKeysAndValues(
    alloc: *const c_void,
    domain: *const c_void,
    code: CFIndex,
    keys: *const *const c_void,
    values: *const *const c_void,
    count: CFIndex,
) -> *mut c_void {
    let empty: [*const c_void; 0] = [];
    let (keys, values, count) = if count <= 0 || keys.is_null() || values.is_null() {
        (empty.as_ptr(), empty.as_ptr(), 0)
    } else {
        (keys, values, count)
    };
    // SAFETY: per this function's contract.
    let info = unsafe {
        super::collections::CFDictionaryCreate(
            alloc,
            keys,
            values,
            count,
            &super::collections::kCFTypeDictionaryKeyCallBacks,
            &super::collections::kCFTypeDictionaryValueCallBacks,
        )
    };
    // SAFETY: as above; the error retains the dictionary.
    let error = unsafe { CFErrorCreate(alloc, domain, code, info) };
    // SAFETY: the +1 reference CFDictionaryCreate returned.
    unsafe { super::base::CFRelease(info) };
    error
}

fn error<'a>(cf: *const c_void) -> &'a NSError {
    // SAFETY: the callers' contracts: `cf` is an error.
    unsafe { &*cf.cast::<NSError>() }
}

/// # Safety
///
/// `cf` is an error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorGetDomain(cf: *const c_void) -> *const c_void {
    // The domain lives in the error; hand out the error's own reference.
    let domain = error(cf).domain();
    let ptr = Retained::as_ptr(&domain).cast::<c_void>();
    drop(domain);
    ptr
}

/// # Safety
///
/// `cf` is an error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorGetCode(cf: *const c_void) -> CFIndex {
    error(cf).code()
}

/// # Safety
///
/// `cf` is an error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorCopyUserInfo(cf: *const c_void) -> *mut c_void {
    owned(error(cf).userInfo())
}

/// # Safety
///
/// `cf` is an error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorCopyDescription(cf: *const c_void) -> *mut c_void {
    owned(error(cf).localizedDescription())
}

/// # Safety
///
/// `cf` is an error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorCopyFailureReason(cf: *const c_void) -> *mut c_void {
    error(cf).localizedFailureReason().map_or(std::ptr::null_mut(), owned)
}

/// # Safety
///
/// `cf` is an error.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFErrorCopyRecoverySuggestion(cf: *const c_void) -> *mut c_void {
    error(cf).localizedRecoverySuggestion().map_or(std::ptr::null_mut(), owned)
}

crate::runloop::modes::exported_strings! {
    kCFErrorDomainPOSIX, CF_POSIX = "NSPOSIXErrorDomain";
    kCFErrorDomainOSStatus, CF_OSSTATUS = "NSOSStatusErrorDomain";
    kCFErrorDomainMach, CF_MACH = "NSMachErrorDomain";
    kCFErrorDomainCocoa, CF_COCOA = "NSCocoaErrorDomain";
    kCFErrorLocalizedDescriptionKey, CF_DESCRIPTION = "NSLocalizedDescription";
    kCFErrorLocalizedFailureKey, CF_FAILURE = "NSLocalizedFailure";
    kCFErrorLocalizedFailureReasonKey, CF_REASON = "NSLocalizedFailureReason";
    kCFErrorLocalizedRecoverySuggestionKey, CF_SUGGESTION = "NSLocalizedRecoverySuggestion";
    kCFErrorDescriptionKey, CF_ERROR_DESCRIPTION = "NSDescription";
    kCFErrorUnderlyingErrorKey, CF_UNDERLYING = "NSUnderlyingError";
    kCFErrorURLKey, CF_URL = "NSURL";
    kCFErrorFilePathKey, CF_FILE_PATH = "NSFilePath";
}
