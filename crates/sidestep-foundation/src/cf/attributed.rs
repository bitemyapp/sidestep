//! `CFAttributedString` over `NSAttributedString` and
//! `NSMutableAttributedString`, and the bidirectional levels of its text
//! (`crate::bidi`).

use std::ffi::c_void;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2::{ClassType, msg_send};
use objc2_foundation::{NSAttributedString, NSDictionary, NSMutableAttributedString, NSRange, NSString};

use super::string::CFRange;
use super::types::{CFTypeID, id, object, owned, owned_by};

type CFIndex = isize;
type Boolean = u8;

fn attributed<'a>(cf: *const c_void) -> &'a NSAttributedString {
    // SAFETY: the callers' contracts: `cf` is an attributed string.
    unsafe { &*cf.cast::<NSAttributedString>() }
}

fn mutable<'a>(cf: *mut c_void) -> &'a NSMutableAttributedString {
    // SAFETY: the callers' contracts: `cf` is a mutable attributed string.
    unsafe { &*cf.cast::<NSMutableAttributedString>() }
}

fn ns_range(range: CFRange) -> NSRange {
    NSRange::new(range.location.max(0) as usize, range.length.max(0) as usize)
}

/// Store a found range, if the caller asked for it.
///
/// # Safety
///
/// `out` is null or writable.
unsafe fn write_range(out: *mut CFRange, range: NSRange) {
    if !out.is_null() {
        // SAFETY: per this function's contract.
        unsafe { out.write(CFRange { location: range.location as CFIndex, length: range.length as CFIndex }) };
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAttributedStringGetTypeID() -> CFTypeID {
    id::ATTRIBUTED_STRING
}

/// # Safety
///
/// `string` is null or a string; `attributes` null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreate(
    _alloc: *const c_void,
    string: *const c_void,
    attributes: *const c_void,
) -> *mut c_void {
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let made: Retained<AnyObject> = unsafe {
        let this: Allocated<AnyObject> = msg_send![NSAttributedString::class(), alloc];
        msg_send![this, initWithString: object(string), attributes: attributes.cast::<AnyObject>().as_ref()]
    };
    owned(made)
}

/// # Safety
///
/// `cf` is null or an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreateCopy(_alloc: *const c_void, cf: *const c_void) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -copy returns an immutable attributed string.
    let copy: Retained<AnyObject> = unsafe { msg_send![attributed(cf), copy] };
    owned(copy)
}

/// # Safety
///
/// `cf` is null or an attributed string the range lies in.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreateWithSubstring(
    _alloc: *const c_void,
    cf: *const c_void,
    range: CFRange,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    owned(attributed(cf).attributedSubstringFromRange(ns_range(range)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAttributedStringCreateMutable(_alloc: *const c_void, _max: CFIndex) -> *mut c_void {
    made_by_cf(owned(NSMutableAttributedString::new()))
}

/// What marks a mutable attributed string CoreFoundation made, whose
/// `CFAttributedStringGetMutableString` is NULL on macOS (an
/// `NSMutableAttributedString`'s is its `mutableString`).
static MADE_BY_CF: u8 = 0;

fn made_by_cf(cf: *mut c_void) -> *mut c_void {
    if !cf.is_null() {
        // SAFETY: `cf` is a live object; the key is a static address, and
        // so is the value, assigned (never retained or read as an object).
        unsafe {
            objc2::ffi::objc_setAssociatedObject(
                cf.cast(),
                (&raw const MADE_BY_CF).cast(),
                (&raw const MADE_BY_CF).cast_mut().cast(),
                objc2::ffi::OBJC_ASSOCIATION_ASSIGN,
            )
        };
    }
    cf
}

/// # Safety
///
/// `cf` is null or an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreateMutableCopy(
    _alloc: *const c_void,
    _max: CFIndex,
    cf: *const c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -mutableCopy returns a mutable attributed string.
    let copy: Retained<AnyObject> = unsafe { msg_send![attributed(cf), mutableCopy] };
    made_by_cf(owned(copy))
}

/// # Safety
///
/// `cf` is an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetLength(cf: *const c_void) -> CFIndex {
    attributed(cf).length() as CFIndex
}

/// The text, which the attributed string keeps.
///
/// # Safety
///
/// `cf` is an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetString(cf: *const c_void) -> *const c_void {
    static KEY: u8 = 0;
    let string: Retained<AnyObject> = attributed(cf).string().into();
    // SAFETY: per this function's contract.
    owned_by(unsafe { object(cf) }, &KEY, string)
}

/// The text as a mutable string whose edits change the attributed string;
/// NULL for one CoreFoundation made, as on macOS.
///
/// # Safety
///
/// `cf` is null or a mutable attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetMutableString(cf: *mut c_void) -> *const c_void {
    static KEY: u8 = 0;
    if cf.is_null() {
        return std::ptr::null();
    }
    // SAFETY: `cf` is a live object; the key is a static address.
    if !unsafe { objc2::ffi::objc_getAssociatedObject(cf.cast(), (&raw const MADE_BY_CF).cast()) }.is_null() {
        return std::ptr::null();
    }
    let string: Retained<AnyObject> = mutable(cf).mutableString().into();
    // SAFETY: per this function's contract.
    owned_by(unsafe { object(cf) }, &KEY, string)
}

/// The attributes at `location`, which the attributed string keeps.
///
/// # Safety
///
/// `cf` is an attributed string longer than `location`; `effective` null
/// or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttributes(
    cf: *const c_void,
    location: CFIndex,
    effective: *mut CFRange,
) -> *const c_void {
    let mut range = NSRange::new(0, 0);
    // SAFETY: per this function's contract; the run keeps the dictionary.
    let found: *const AnyObject =
        unsafe { msg_send![attributed(cf), attributesAtIndex: location as usize, effectiveRange: &mut range] };
    // SAFETY: as above.
    unsafe { write_range(effective, range) };
    found.cast()
}

/// # Safety
///
/// As [`CFAttributedStringGetAttributes`]; the range lies in the string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttributesAndLongestEffectiveRange(
    cf: *const c_void,
    location: CFIndex,
    within: CFRange,
    longest: *mut CFRange,
) -> *const c_void {
    let mut range = NSRange::new(0, 0);
    // SAFETY: per this function's contract; the string keeps what it
    // returns, looked up again below.
    let _: Retained<NSDictionary> = unsafe {
        msg_send![
            attributed(cf),
            attributesAtIndex: location as usize,
            longestEffectiveRange: &mut range,
            inRange: ns_range(within)
        ]
    };
    // SAFETY: as above.
    unsafe { write_range(longest, range) };
    // The dictionary the run keeps.
    // SAFETY: as above.
    unsafe { CFAttributedStringGetAttributes(cf, location, std::ptr::null_mut()) }
}

/// One attribute's value at `location`, which the attributed string keeps.
///
/// # Safety
///
/// As [`CFAttributedStringGetAttributes`]; `name` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttribute(
    cf: *const c_void,
    location: CFIndex,
    name: *const c_void,
    effective: *mut CFRange,
) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    let mut range = NSRange::new(0, 0);
    // SAFETY: per this function's contract; the attributes hold the value.
    let found: Option<Retained<AnyObject>> = unsafe {
        msg_send![attributed(cf), attribute: object(name), atIndex: location as usize, effectiveRange: &mut range]
    };
    // SAFETY: as above.
    unsafe { write_range(effective, range) };
    found.map_or(std::ptr::null(), |v| Retained::as_ptr(&v).cast())
}

/// # Safety
///
/// As [`CFAttributedStringGetAttribute`]; the range lies in the string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttributeAndLongestEffectiveRange(
    cf: *const c_void,
    location: CFIndex,
    name: *const c_void,
    within: CFRange,
    longest: *mut CFRange,
) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    let mut range = NSRange::new(0, 0);
    // SAFETY: per this function's contract; the attributes hold the value.
    let found: Option<Retained<AnyObject>> = unsafe {
        msg_send![
            attributed(cf),
            attribute: object(name),
            atIndex: location as usize,
            longestEffectiveRange: &mut range,
            inRange: ns_range(within)
        ]
    };
    // SAFETY: as above.
    unsafe { write_range(longest, range) };
    found.map_or(std::ptr::null(), |v| Retained::as_ptr(&v).cast())
}

/// # Safety
///
/// `cf` is null or a mutable attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringBeginEditing(cf: *mut c_void) {
    if !cf.is_null() {
        mutable(cf).beginEditing();
    }
}

/// # Safety
///
/// `cf` is null or a mutable attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringEndEditing(cf: *mut c_void) {
    if !cf.is_null() {
        mutable(cf).endEditing();
    }
}

/// # Safety
///
/// `cf` is a mutable attributed string the range lies in; `replacement`
/// null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringReplaceString(
    cf: *mut c_void,
    range: CFRange,
    replacement: *const c_void,
) {
    let replacement = if replacement.is_null() {
        crate::string::empty()
    } else {
        // SAFETY: per this function's contract.
        objc2::Message::retain(unsafe { &*replacement.cast::<NSString>() })
    };
    mutable(cf).replaceCharactersInRange_withString(ns_range(range), &replacement);
}

/// # Safety
///
/// `cf` is a mutable attributed string the range lies in; `replacement` an
/// attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringReplaceAttributedString(
    cf: *mut c_void,
    range: CFRange,
    replacement: *const c_void,
) {
    if !replacement.is_null() {
        mutable(cf).replaceCharactersInRange_withAttributedString(ns_range(range), attributed(replacement));
    }
}

/// Set one attribute over the range, keeping the others.
///
/// # Safety
///
/// `cf` is a mutable attributed string the range lies in; `name` a string;
/// `value` an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringSetAttribute(
    cf: *mut c_void,
    range: CFRange,
    name: *const c_void,
    value: *const c_void,
) {
    if name.is_null() || value.is_null() {
        return;
    }
    // SAFETY: per this function's contract.
    let () =
        unsafe { msg_send![mutable(cf), addAttribute: object(name), value: object(value), range: ns_range(range)] };
}

/// Set attributes over the range: in place of all others, or added to
/// them.
///
/// # Safety
///
/// `cf` is a mutable attributed string the range lies in; `attributes`
/// null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringSetAttributes(
    cf: *mut c_void,
    range: CFRange,
    attributes: *const c_void,
    clear_others: Boolean,
) {
    // SAFETY: per this function's contract.
    let attributes = unsafe { attributes.cast::<AnyObject>().as_ref() };
    if clear_others != 0 {
        // SAFETY: as above; nil sets none.
        let () = unsafe { msg_send![mutable(cf), setAttributes: attributes, range: ns_range(range)] };
    } else if let Some(attributes) = attributes {
        // SAFETY: as above.
        let () = unsafe { msg_send![mutable(cf), addAttributes: attributes, range: ns_range(range)] };
    }
}

/// # Safety
///
/// `cf` is a mutable attributed string the range lies in; `name` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringRemoveAttribute(
    cf: *mut c_void,
    range: CFRange,
    name: *const c_void,
) {
    if !name.is_null() {
        // SAFETY: per this function's contract.
        let () = unsafe { msg_send![mutable(cf), removeAttribute: object(name), range: ns_range(range)] };
    }
}

/// The bidirectional levels of the range's text, a byte per UTF-16 unit,
/// and each unit's paragraph direction (0 left to right, 1 right to
/// left), from `base` (-1 for each paragraph's first strong character, 0
/// left to right, 1 right to left); whether any text runs right to left.
///
/// # Safety
///
/// `cf` is an attributed string the range lies in; `levels` and
/// `directions` null or with room for the range's length.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetBidiLevelsAndResolvedDirections(
    cf: *const c_void,
    range: CFRange,
    base: i8,
    levels: *mut u8,
    directions: *mut u8,
) -> bool {
    let whole: Retained<AnyObject> = attributed(cf).string().into();
    let Some(part) = super::string::substring(&whole, range) else { return false };
    let chars: Vec<char> = part.chars().collect();
    let base = match base {
        0 => Some(0),
        1 => Some(1),
        _ => None,
    };
    let resolved = crate::bidi::resolve(&chars, base);
    let mut at = 0;
    for (k, c) in chars.iter().enumerate() {
        for _ in 0..c.len_utf16() {
            // SAFETY: the buffers hold the range's length in units.
            unsafe {
                if !levels.is_null() {
                    levels.add(at).write(resolved.levels[k]);
                }
                if !directions.is_null() {
                    directions.add(at).write(resolved.paragraphs[k]);
                }
            }
            at += 1;
        }
    }
    resolved.levels.iter().any(|l| l % 2 == 1)
}

/// The same as [`CFAttributedStringGetBidiLevelsAndResolvedDirections`]:
/// the paragraphs' directions come from their first strong characters, as
/// measured on macOS for the text it was tried with.
///
/// # Safety
///
/// As [`CFAttributedStringGetBidiLevelsAndResolvedDirections`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetStatisticalWritingDirections(
    cf: *const c_void,
    range: CFRange,
    base: i8,
    levels: *mut u8,
    directions: *mut u8,
) -> bool {
    // SAFETY: per this function's contract.
    unsafe { CFAttributedStringGetBidiLevelsAndResolvedDirections(cf, range, base, levels, directions) }
}
