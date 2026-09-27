//! `CFDictionary`, `CFArray`, `CFNumber` and `CFBoolean` over their
//! Foundation classes.
//!
//! Only collections of CoreFoundation objects are supported: creating one
//! with callbacks other than the `kCFType…CallBacks` (or
//! `kCFCopyStringDictionaryKeyCallBacks`) returns NULL, since Foundation's
//! collections retain what they hold.

use std::ffi::c_void;

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSMutableArray, NSMutableDictionary, NSNumber};

use super::types::{CFTypeID, id, object, owned};

type CFIndex = isize;
type Boolean = u8;

type Retain = unsafe extern "C-unwind" fn(*const c_void, *const c_void) -> *const c_void;
type Release = unsafe extern "C-unwind" fn(*const c_void, *const c_void);
type CopyDescription = unsafe extern "C-unwind" fn(*const c_void) -> *mut c_void;
type Equal = unsafe extern "C-unwind" fn(*const c_void, *const c_void) -> Boolean;
type Hash = unsafe extern "C-unwind" fn(*const c_void) -> usize;

/// `CFDictionaryKeyCallBacks`, also `CFSetCallBacks`.
#[repr(C)]
pub struct KeyCallBacks {
    version: CFIndex,
    retain: Option<Retain>,
    release: Option<Release>,
    copy_description: Option<CopyDescription>,
    equal: Option<Equal>,
    hash: Option<Hash>,
}

/// `CFDictionaryValueCallBacks`, also `CFArrayCallBacks`.
#[repr(C)]
pub struct ValueCallBacks {
    version: CFIndex,
    retain: Option<Retain>,
    release: Option<Release>,
    copy_description: Option<CopyDescription>,
    equal: Option<Equal>,
}

unsafe extern "C-unwind" fn retain(_alloc: *const c_void, value: *const c_void) -> *const c_void {
    // SAFETY: the collections' values are objects.
    unsafe { super::base::CFRetain(value.cast_mut()) }
}

unsafe extern "C-unwind" fn release(_alloc: *const c_void, value: *const c_void) {
    // SAFETY: as above.
    unsafe { super::base::CFRelease(value.cast_mut()) }
}

unsafe extern "C-unwind" fn copy_description(value: *const c_void) -> *mut c_void {
    // SAFETY: as above.
    unsafe { super::types::CFCopyDescription(value) }
}

unsafe extern "C-unwind" fn equal(a: *const c_void, b: *const c_void) -> Boolean {
    // SAFETY: as above.
    unsafe { super::base::CFEqual(a, b) }
}

unsafe extern "C-unwind" fn hash(value: *const c_void) -> usize {
    // SAFETY: as above.
    unsafe { super::base::CFHash(value) }
}

#[unsafe(no_mangle)]
pub static kCFTypeDictionaryKeyCallBacks: KeyCallBacks = KeyCallBacks {
    version: 0,
    retain: Some(retain),
    release: Some(release),
    copy_description: Some(copy_description),
    equal: Some(equal),
    hash: Some(hash),
};

#[unsafe(no_mangle)]
pub static kCFCopyStringDictionaryKeyCallBacks: KeyCallBacks = KeyCallBacks {
    version: 0,
    retain: Some(retain),
    release: Some(release),
    copy_description: Some(copy_description),
    equal: Some(equal),
    hash: Some(hash),
};

#[unsafe(no_mangle)]
pub static kCFTypeDictionaryValueCallBacks: ValueCallBacks = ValueCallBacks {
    version: 0,
    retain: Some(retain),
    release: Some(release),
    copy_description: Some(copy_description),
    equal: Some(equal),
};

#[unsafe(no_mangle)]
pub static kCFTypeArrayCallBacks: ValueCallBacks = ValueCallBacks {
    version: 0,
    retain: Some(retain),
    release: Some(release),
    copy_description: Some(copy_description),
    equal: Some(equal),
};

fn object_keys(callbacks: *const KeyCallBacks) -> bool {
    std::ptr::eq(callbacks, &kCFTypeDictionaryKeyCallBacks)
        || std::ptr::eq(callbacks, &kCFCopyStringDictionaryKeyCallBacks)
}

fn object_values(callbacks: *const ValueCallBacks) -> bool {
    std::ptr::eq(callbacks, &kCFTypeDictionaryValueCallBacks) || std::ptr::eq(callbacks, &kCFTypeArrayCallBacks)
}

fn dictionary<'a>(cf: *const c_void) -> &'a NSDictionary {
    // SAFETY: the callers' contracts: `cf` is a dictionary.
    unsafe { &*cf.cast::<NSDictionary>() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFDictionaryGetTypeID() -> CFTypeID {
    id::DICTIONARY
}

/// # Safety
///
/// `keys` and `values` point to `count` objects each.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryCreate(
    _alloc: *const c_void,
    keys: *const *const c_void,
    values: *const *const c_void,
    count: CFIndex,
    key_callbacks: *const KeyCallBacks,
    value_callbacks: *const ValueCallBacks,
) -> *mut c_void {
    if !object_keys(key_callbacks) || !object_values(value_callbacks) {
        return std::ptr::null_mut();
    }
    let count = count.max(0) as usize;
    // SAFETY: -initWithObjects:forKeys:count: copies `count` of each.
    let made: Retained<AnyObject> = unsafe {
        let this: objc2::rc::Allocated<AnyObject> = msg_send![<NSDictionary as objc2::ClassType>::class(), alloc];
        msg_send![this, initWithObjects: values.cast::<*const AnyObject>(), forKeys: keys.cast::<*const AnyObject>(), count: count]
    };
    owned(made)
}

/// # Safety
///
/// `cf` is a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryCreateCopy(_alloc: *const c_void, cf: *const c_void) -> *mut c_void {
    // SAFETY: -copy returns an immutable dictionary.
    let copy: Retained<AnyObject> = unsafe { msg_send![dictionary(cf), copy] };
    owned(copy)
}

/// # Safety
///
/// `cf` is a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryGetCount(cf: *const c_void) -> CFIndex {
    // SAFETY: -count takes nothing.
    let count: usize = unsafe { msg_send![dictionary(cf), count] };
    count as CFIndex
}

/// # Safety
///
/// `cf` is a dictionary; `key` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryGetValue(cf: *const c_void, key: *const c_void) -> *const c_void {
    if key.is_null() {
        return std::ptr::null();
    }
    // SAFETY: -objectForKey: returns a value the dictionary holds, or nil.
    let value: *const AnyObject = unsafe { msg_send![dictionary(cf), objectForKey: object(key)] };
    value.cast()
}

/// # Safety
///
/// As [`CFDictionaryGetValue`]; `value` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryGetValueIfPresent(
    cf: *const c_void,
    key: *const c_void,
    value: *mut *const c_void,
) -> Boolean {
    // SAFETY: per this function's contract.
    let found = unsafe { CFDictionaryGetValue(cf, key) };
    if !found.is_null() && !value.is_null() {
        // SAFETY: per this function's contract.
        unsafe { value.write(found) };
    }
    u8::from(!found.is_null())
}

/// # Safety
///
/// As [`CFDictionaryGetValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryContainsKey(cf: *const c_void, key: *const c_void) -> Boolean {
    // SAFETY: per this function's contract.
    u8::from(!unsafe { CFDictionaryGetValue(cf, key) }.is_null())
}

/// How many times `key` is a key: once or not at all.
///
/// # Safety
///
/// `cf` is a dictionary; `key` an object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryGetCountOfKey(cf: *const c_void, key: *const c_void) -> CFIndex {
    // SAFETY: per this function's contract.
    CFIndex::from(unsafe { CFDictionaryContainsKey(cf, key) })
}

/// How many keys have a value equal to `value`.
///
/// # Safety
///
/// `cf` is a dictionary; `value` an object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryGetCountOfValue(cf: *const c_void, value: *const c_void) -> CFIndex {
    if value.is_null() {
        return 0;
    }
    let entries = crate::plist::dictionary_entries(dictionary(cf));
    // SAFETY: both are objects.
    let equal = |v: &Retained<AnyObject>| unsafe { super::base::CFEqual(Retained::as_ptr(v).cast(), value) } != 0;
    entries.iter().filter(|(_, v)| equal(v)).count() as CFIndex
}

/// # Safety
///
/// As [`CFDictionaryGetCountOfValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryContainsValue(cf: *const c_void, value: *const c_void) -> Boolean {
    // SAFETY: per this function's contract.
    u8::from(unsafe { CFDictionaryGetCountOfValue(cf, value) } > 0)
}

/// The function a dictionary's pairs are handed to.
type Applier = Option<unsafe extern "C-unwind" fn(*const c_void, *const c_void, *mut c_void)>;

/// Call `applier` with each key and value, and `context`. The pairs are
/// taken first, so the function may change the dictionary.
///
/// # Safety
///
/// `cf` is a dictionary; `applier` is null or a function taking a key, a
/// value and `context`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryApplyFunction(cf: *const c_void, applier: Applier, context: *mut c_void) {
    let Some(applier) = applier else { return };
    for (key, value) in crate::plist::dictionary_entries(dictionary(cf)) {
        // SAFETY: per this function's contract; the pairs are held while
        // the function runs.
        unsafe { applier(Retained::as_ptr(&key).cast(), Retained::as_ptr(&value).cast(), context) };
    }
}

/// # Safety
///
/// `cf` is a dictionary; `keys` and `values` are null or have room for its
/// count.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryGetKeysAndValues(
    cf: *const c_void,
    keys: *mut *const c_void,
    values: *mut *const c_void,
) {
    for (i, (key, value)) in crate::plist::dictionary_entries(dictionary(cf)).into_iter().enumerate() {
        // SAFETY: the caller's buffers hold the dictionary's count; the
        // pointers stay valid because the dictionary holds the objects.
        unsafe {
            if !keys.is_null() {
                keys.add(i).write(Retained::as_ptr(&key).cast());
            }
            if !values.is_null() {
                values.add(i).write(Retained::as_ptr(&value).cast());
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFArrayGetTypeID() -> CFTypeID {
    id::ARRAY
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFNumberGetTypeID() -> CFTypeID {
    id::NUMBER
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFBooleanGetTypeID() -> CFTypeID {
    id::BOOLEAN
}

/// # Safety
///
/// `boolean` is a number.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFBooleanGetValue(boolean: *const c_void) -> Boolean {
    // SAFETY: per this function's contract; -boolValue returns BOOL.
    let value: bool = unsafe { msg_send![object(boolean), boolValue] };
    u8::from(value)
}

fn array<'a>(cf: *const c_void) -> &'a NSArray {
    // SAFETY: the callers' contracts: `cf` is an array.
    unsafe { &*cf.cast::<NSArray>() }
}

fn mutable_array<'a>(cf: *mut c_void) -> &'a NSMutableArray {
    // SAFETY: the callers' contracts: `cf` is a mutable array.
    unsafe { &*cf.cast::<NSMutableArray>() }
}

/// # Safety
///
/// `values` points to `count` objects.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayCreate(
    _alloc: *const c_void,
    values: *const *const c_void,
    count: CFIndex,
    callbacks: *const ValueCallBacks,
) -> *mut c_void {
    if !object_values(callbacks) {
        return std::ptr::null_mut();
    }
    // SAFETY: -initWithObjects:count: copies `count` objects.
    let made: Retained<AnyObject> = unsafe {
        let this: objc2::rc::Allocated<AnyObject> = msg_send![<NSArray as objc2::ClassType>::class(), alloc];
        msg_send![this, initWithObjects: values.cast::<*const AnyObject>(), count: count.max(0) as usize]
    };
    owned(made)
}

/// # Safety
///
/// `cf` is an array.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayCreateCopy(_alloc: *const c_void, cf: *const c_void) -> *mut c_void {
    // SAFETY: -copy returns an immutable array.
    let copy: Retained<AnyObject> = unsafe { msg_send![array(cf), copy] };
    owned(copy)
}

/// # Safety
///
/// `cf` is an array.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayGetCount(cf: *const c_void) -> CFIndex {
    array(cf).count() as CFIndex
}

/// # Safety
///
/// `cf` is an array with more than `index` values.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayGetValueAtIndex(cf: *const c_void, index: CFIndex) -> *const c_void {
    // SAFETY: the array holds the object it returns.
    let value: *const AnyObject = unsafe { msg_send![array(cf), objectAtIndex: index as usize] };
    value.cast()
}

/// # Safety
///
/// `cf` is an array; `values` has room for `range.length` pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayGetValues(
    cf: *const c_void,
    range: super::string::CFRange,
    values: *mut *const c_void,
) {
    for i in 0..range.length.max(0) {
        // SAFETY: per this function's contract.
        unsafe { values.add(i as usize).write(CFArrayGetValueAtIndex(cf, range.location + i)) };
    }
}

/// # Safety
///
/// `cf` is an array; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayContainsValue(
    cf: *const c_void,
    range: super::string::CFRange,
    value: *const c_void,
) -> Boolean {
    // SAFETY: per this function's contract.
    u8::from(unsafe { CFArrayGetFirstIndexOfValue(cf, range, value) } >= 0)
}

/// # Safety
///
/// As [`CFArrayContainsValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayGetFirstIndexOfValue(
    cf: *const c_void,
    range: super::string::CFRange,
    value: *const c_void,
) -> CFIndex {
    for i in range.location.max(0)..range.location.max(0) + range.length.max(0) {
        // SAFETY: per this function's contract.
        if unsafe { super::base::CFEqual(CFArrayGetValueAtIndex(cf, i), value) } != 0 {
            return i;
        }
    }
    -1
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFArrayCreateMutable(
    _alloc: *const c_void,
    _capacity: CFIndex,
    callbacks: *const ValueCallBacks,
) -> *mut c_void {
    if !object_values(callbacks) {
        return std::ptr::null_mut();
    }
    owned(NSMutableArray::<AnyObject>::new())
}

/// # Safety
///
/// `cf` is an array.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayCreateMutableCopy(
    _alloc: *const c_void,
    _capacity: CFIndex,
    cf: *const c_void,
) -> *mut c_void {
    // SAFETY: -mutableCopy returns a mutable array.
    let copy: Retained<AnyObject> = unsafe { msg_send![array(cf), mutableCopy] };
    owned(copy)
}

/// # Safety
///
/// `cf` is a mutable array; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayAppendValue(cf: *mut c_void, value: *const c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_array(cf), addObject: object(value)] };
}

/// # Safety
///
/// `cf` is a mutable array with at least `index` values; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayInsertValueAtIndex(cf: *mut c_void, index: CFIndex, value: *const c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_array(cf), insertObject: object(value), atIndex: index as usize] };
}

/// # Safety
///
/// `cf` is a mutable array with more than `index` values; `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArraySetValueAtIndex(cf: *mut c_void, index: CFIndex, value: *const c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_array(cf), replaceObjectAtIndex: index as usize, withObject: object(value)] };
}

/// # Safety
///
/// `cf` is a mutable array with more than `index` values.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayRemoveValueAtIndex(cf: *mut c_void, index: CFIndex) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_array(cf), removeObjectAtIndex: index as usize] };
}

/// # Safety
///
/// `cf` is a mutable array.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFArrayRemoveAllValues(cf: *mut c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_array(cf), removeAllObjects] };
}

fn mutable_dictionary<'a>(cf: *mut c_void) -> &'a NSMutableDictionary {
    // SAFETY: the callers' contracts: `cf` is a mutable dictionary.
    unsafe { &*cf.cast::<NSMutableDictionary>() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFDictionaryCreateMutable(
    _alloc: *const c_void,
    _capacity: CFIndex,
    key_callbacks: *const KeyCallBacks,
    value_callbacks: *const ValueCallBacks,
) -> *mut c_void {
    if !object_keys(key_callbacks) || !object_values(value_callbacks) {
        return std::ptr::null_mut();
    }
    owned(NSMutableDictionary::<AnyObject, AnyObject>::new())
}

/// # Safety
///
/// `cf` is a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryCreateMutableCopy(
    _alloc: *const c_void,
    _capacity: CFIndex,
    cf: *const c_void,
) -> *mut c_void {
    // SAFETY: -mutableCopy returns a mutable dictionary.
    let copy: Retained<AnyObject> = unsafe { msg_send![dictionary(cf), mutableCopy] };
    owned(copy)
}

/// # Safety
///
/// `cf` is a mutable dictionary; `key` and `value` objects.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionarySetValue(cf: *mut c_void, key: *const c_void, value: *const c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_dictionary(cf), setObject: object(value), forKey: object(key)] };
}

/// # Safety
///
/// As [`CFDictionarySetValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryAddValue(cf: *mut c_void, key: *const c_void, value: *const c_void) {
    // SAFETY: per this function's contract.
    if unsafe { CFDictionaryGetValue(cf, key) }.is_null() {
        // SAFETY: as above.
        unsafe { CFDictionarySetValue(cf, key, value) };
    }
}

/// # Safety
///
/// As [`CFDictionarySetValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryReplaceValue(cf: *mut c_void, key: *const c_void, value: *const c_void) {
    // SAFETY: per this function's contract.
    if !unsafe { CFDictionaryGetValue(cf, key) }.is_null() {
        // SAFETY: as above.
        unsafe { CFDictionarySetValue(cf, key, value) };
    }
}

/// # Safety
///
/// `cf` is a mutable dictionary; `key` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryRemoveValue(cf: *mut c_void, key: *const c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_dictionary(cf), removeObjectForKey: object(key)] };
}

/// # Safety
///
/// `cf` is a mutable dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFDictionaryRemoveAllValues(cf: *mut c_void) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable_dictionary(cf), removeAllObjects] };
}

/// `CFNumberType`s.
mod number_type {
    pub(super) const SINT8: isize = 1;
    pub(super) const SINT16: isize = 2;
    pub(super) const SINT32: isize = 3;
    pub(super) const SINT64: isize = 4;
    pub(super) const FLOAT32: isize = 5;
    pub(super) const FLOAT64: isize = 6;
    pub(super) const CHAR: isize = 7;
    pub(super) const SHORT: isize = 8;
    pub(super) const INT: isize = 9;
    pub(super) const LONG: isize = 10;
    pub(super) const LONG_LONG: isize = 11;
    pub(super) const FLOAT: isize = 12;
    pub(super) const DOUBLE: isize = 13;
    pub(super) const CF_INDEX: isize = 14;
    pub(super) const NS_INTEGER: isize = 15;
    pub(super) const CG_FLOAT: isize = 16;
}

/// # Safety
///
/// `value` points to a number of the type.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFNumberCreate(
    _alloc: *const c_void,
    kind: isize,
    value: *const c_void,
) -> *mut c_void {
    use number_type::*;
    // SAFETY: per this function's contract.
    let number = unsafe {
        match kind {
            SINT8 | CHAR => NSNumber::new_i8(*value.cast::<i8>()),
            SINT16 | SHORT => NSNumber::new_i16(*value.cast::<i16>()),
            SINT32 | INT => NSNumber::new_i32(*value.cast::<i32>()),
            SINT64 | LONG | LONG_LONG | CF_INDEX | NS_INTEGER => NSNumber::new_i64(*value.cast::<i64>()),
            FLOAT32 | FLOAT => NSNumber::new_f32(*value.cast::<f32>()),
            FLOAT64 | DOUBLE | CG_FLOAT => NSNumber::new_f64(*value.cast::<f64>()),
            _ => return std::ptr::null_mut(),
        }
    };
    owned(number)
}

fn number<'a>(cf: *const c_void) -> &'a NSNumber {
    // SAFETY: the callers' contracts: `cf` is a number.
    unsafe { &*cf.cast::<NSNumber>() }
}

/// # Safety
///
/// `cf` is a number.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFNumberGetType(cf: *const c_void) -> isize {
    use number_type::*;
    // SAFETY: -objCType returns a C string.
    let kind = unsafe { std::ffi::CStr::from_ptr(number(cf).objCType().as_ptr()) }.to_bytes().first().copied();
    match (crate::plist::number_value(number(cf)), kind) {
        // A float stays one (as `+numberWithFloat:` makes on macOS).
        (_, Some(b'f')) => FLOAT32,
        (plist::Value::Real(_), _) => FLOAT64,
        (_, Some(b'c')) => SINT8,
        (_, Some(b's')) => SINT16,
        (_, Some(b'i')) => SINT32,
        _ => SINT64,
    }
}

/// # Safety
///
/// `cf` is a number.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFNumberIsFloatType(cf: *const c_void) -> Boolean {
    // SAFETY: per this function's contract.
    u8::from(matches!(unsafe { CFNumberGetType(cf) }, number_type::FLOAT32 | number_type::FLOAT64))
}

/// # Safety
///
/// `cf` is a number.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFNumberGetByteSize(cf: *const c_void) -> CFIndex {
    // SAFETY: per this function's contract.
    match unsafe { CFNumberGetType(cf) } {
        number_type::SINT8 => 1,
        number_type::SINT16 => 2,
        number_type::SINT32 | number_type::FLOAT32 => 4,
        _ => 8,
    }
}

/// Store a number as a type; whether it fitted without loss.
///
/// # Safety
///
/// `cf` is a number; `out` has room for the type.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFNumberGetValue(cf: *const c_void, kind: isize, out: *mut c_void) -> Boolean {
    use number_type::*;
    let n = number(cf);
    let (int, float) = (n.longLongValue(), n.doubleValue());
    // SAFETY: per this function's contract.
    let exact = unsafe {
        match kind {
            SINT8 | CHAR => {
                out.cast::<i8>().write(int as i8);
                i64::from(int as i8) == int && float.fract() == 0.0
            }
            SINT16 | SHORT => {
                out.cast::<i16>().write(int as i16);
                i64::from(int as i16) == int && float.fract() == 0.0
            }
            SINT32 | INT => {
                out.cast::<i32>().write(int as i32);
                i64::from(int as i32) == int && float.fract() == 0.0
            }
            SINT64 | LONG | LONG_LONG | CF_INDEX | NS_INTEGER => {
                out.cast::<i64>().write(int);
                float.fract() == 0.0
            }
            FLOAT32 | FLOAT => {
                out.cast::<f32>().write(float as f32);
                f64::from(float as f32) == float
            }
            FLOAT64 | DOUBLE | CG_FLOAT => {
                out.cast::<f64>().write(float);
                true
            }
            _ => return 0,
        }
    };
    u8::from(exact)
}

/// # Safety
///
/// Both are numbers.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFNumberCompare(a: *const c_void, b: *const c_void, _context: *mut c_void) -> CFIndex {
    number(a).compare(number(b)) as CFIndex
}
