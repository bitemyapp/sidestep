//! `CFAttributedString` and `CFMutableAttributedString` over
//! `NSAttributedString` and `NSMutableAttributedString`: each function
//! sends the Foundation method behind it, so attributed strings of any
//! class (a text storage, say) work as they answer. Ranges and indices
//! count UTF-16 units. What a Get function returns belongs to the string
//! (its text, its attribute dictionaries), as CoreFoundation's does.
//!
//! Bidi levels: characters of right-to-left scripts get level 1 and the
//! rest the base direction's level; numbers and neutrals aren't resolved
//! further (the text engine does that when it lays text out).

use std::ffi::c_void;

use icu_properties::CodePointMapData;
use icu_properties::props::BidiClass;
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2_foundation::{NSAttributedString, NSMutableAttributedString, NSRange};

use super::string::CFRange;
use super::types::{CFTypeID, id, object, owned, owned_by};

type CFIndex = isize;
type Boolean = u8;

fn ns_range(r: CFRange) -> NSRange {
    NSRange::new(r.location.max(0) as usize, r.length.max(0) as usize)
}

fn cf_range(r: NSRange) -> CFRange {
    CFRange { location: r.location as CFIndex, length: r.length as CFIndex }
}

/// Write `r` through `out` if it isn't null.
///
/// # Safety
///
/// `out` is null or writable.
unsafe fn store(out: *mut CFRange, r: NSRange) {
    if !out.is_null() {
        // SAFETY: as the caller promises.
        unsafe { out.write(cf_range(r)) };
    }
}

fn class_of(mutable: bool) -> &'static objc2::runtime::AnyClass {
    if mutable {
        <NSMutableAttributedString as objc2::ClassType>::class()
    } else {
        <NSAttributedString as objc2::ClassType>::class()
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAttributedStringGetTypeID() -> CFTypeID {
    id::ATTRIBUTED_STRING
}

/// # Safety
///
/// `string` is a string and `attributes` null or a dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreate(
    _alloc: *const c_void,
    string: *const c_void,
    attributes: *const c_void,
) -> *mut c_void {
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -initWithString:attributes: takes a string and a dictionary
    // or nil.
    let made: Retained<AnyObject> = unsafe {
        let this: Allocated<AnyObject> = msg_send![class_of(false), alloc];
        let attributes: Option<&AnyObject> = attributes.cast::<AnyObject>().as_ref();
        msg_send![this, initWithString: object(string), attributes: attributes]
    };
    owned(made)
}

/// # Safety
///
/// `string` is an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreateWithSubstring(
    _alloc: *const c_void,
    string: *const c_void,
    range: CFRange,
) -> *mut c_void {
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -attributedSubstringFromRange: takes a range; -copy makes it
    // immutable.
    let made: Retained<AnyObject> = unsafe {
        let sub: Retained<AnyObject> = msg_send![object(string), attributedSubstringFromRange: ns_range(range)];
        msg_send![&*sub, copy]
    };
    owned(made)
}

/// # Safety
///
/// `string` is an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreateCopy(
    _alloc: *const c_void,
    string: *const c_void,
) -> *mut c_void {
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -copy takes nothing.
    let made: Retained<AnyObject> = unsafe { msg_send![object(string), copy] };
    owned(made)
}

static STRING_KEY: u8 = 0;
static MUTABLE_STRING_KEY: u8 = 0;

/// # Safety
///
/// `string` is an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetString(string: *const c_void) -> *const c_void {
    if string.is_null() {
        return std::ptr::null();
    }
    // SAFETY: -string returns a string.
    let owner = unsafe { object(string) };
    let text: Retained<AnyObject> = unsafe { msg_send![owner, string] };
    owned_by(owner, &STRING_KEY, text)
}

/// # Safety
///
/// `string` is an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetLength(string: *const c_void) -> CFIndex {
    if string.is_null() {
        return 0;
    }
    // SAFETY: -length takes nothing.
    let length: usize = unsafe { msg_send![object(string), length] };
    length as CFIndex
}

/// # Safety
///
/// `string` is an attributed string; `effective_range` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttributes(
    string: *const c_void,
    loc: CFIndex,
    effective_range: *mut CFRange,
) -> *const c_void {
    let mut range = NSRange::new(0, 0);
    // SAFETY: -attributesAtIndex:effectiveRange: returns the dictionary of
    // the run at the index, which the string keeps.
    let dict: *const AnyObject =
        unsafe { msg_send![object(string), attributesAtIndex: loc.max(0) as usize, effectiveRange: &mut range] };
    // SAFETY: as the caller promises.
    unsafe { store(effective_range, range) };
    dict.cast()
}

/// # Safety
///
/// `string` is an attributed string, `name` a string; `effective_range`
/// null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttribute(
    string: *const c_void,
    loc: CFIndex,
    name: *const c_void,
    effective_range: *mut CFRange,
) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    let mut range = NSRange::new(0, 0);
    // SAFETY: -attribute:atIndex:effectiveRange: returns a value of the
    // run's dictionary, which the string keeps.
    let value: *const AnyObject = unsafe {
        msg_send![object(string), attribute: object(name), atIndex: loc.max(0) as usize, effectiveRange: &mut range]
    };
    // SAFETY: as the caller promises.
    unsafe { store(effective_range, range) };
    value.cast()
}

/// # Safety
///
/// As `CFAttributedStringGetAttributes`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttributesAndLongestEffectiveRange(
    string: *const c_void,
    loc: CFIndex,
    in_range: CFRange,
    longest: *mut CFRange,
) -> *const c_void {
    let mut range = NSRange::new(0, 0);
    // SAFETY: as in CFAttributedStringGetAttributes.
    let dict: *const AnyObject = unsafe {
        msg_send![object(string), attributesAtIndex: loc.max(0) as usize, longestEffectiveRange: &mut range, inRange: ns_range(in_range)]
    };
    // SAFETY: as the caller promises.
    unsafe { store(longest, range) };
    dict.cast()
}

/// # Safety
///
/// As `CFAttributedStringGetAttribute`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetAttributeAndLongestEffectiveRange(
    string: *const c_void,
    loc: CFIndex,
    name: *const c_void,
    in_range: CFRange,
    longest: *mut CFRange,
) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    let mut range = NSRange::new(0, 0);
    // SAFETY: as in CFAttributedStringGetAttribute.
    let value: *const AnyObject = unsafe {
        msg_send![object(string), attribute: object(name), atIndex: loc.max(0) as usize, longestEffectiveRange: &mut range, inRange: ns_range(in_range)]
    };
    // SAFETY: as the caller promises.
    unsafe { store(longest, range) };
    value.cast()
}

/// The maximum length is a hint, and ignored.
///
/// # Safety
///
/// `string` is an attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringCreateMutableCopy(
    _alloc: *const c_void,
    _max_length: CFIndex,
    string: *const c_void,
) -> *mut c_void {
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -mutableCopy takes nothing.
    let made: Retained<AnyObject> = unsafe { msg_send![object(string), mutableCopy] };
    owned(made)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFAttributedStringCreateMutable(_alloc: *const c_void, _max_length: CFIndex) -> *mut c_void {
    // SAFETY: -init makes an empty attributed string.
    let made: Retained<AnyObject> = unsafe {
        let this: Allocated<AnyObject> = msg_send![class_of(true), alloc];
        msg_send![this, init]
    };
    owned(made)
}

/// # Safety
///
/// `string` is a mutable attributed string, `replacement` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringReplaceString(
    string: *mut c_void,
    range: CFRange,
    replacement: *const c_void,
) {
    if string.is_null() || replacement.is_null() {
        return;
    }
    // SAFETY: as the caller promises.
    unsafe {
        let _: () =
            msg_send![object(string), replaceCharactersInRange: ns_range(range), withString: object(replacement)];
    }
}

/// # Safety
///
/// `string` is a mutable attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetMutableString(string: *mut c_void) -> *mut c_void {
    if string.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -mutableString returns the string's text, editable.
    let owner = unsafe { object(string) };
    let text: Retained<AnyObject> = unsafe { msg_send![owner, mutableString] };
    owned_by(owner, &MUTABLE_STRING_KEY, text).cast_mut()
}

/// # Safety
///
/// `string` is a mutable attributed string, `replacement` null or a
/// dictionary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringSetAttributes(
    string: *mut c_void,
    range: CFRange,
    replacement: *const c_void,
    clear_other_attributes: Boolean,
) {
    if string.is_null() {
        return;
    }
    // SAFETY: as the caller promises; nil attributes clear the range.
    unsafe {
        let attributes: Option<&AnyObject> = replacement.cast::<AnyObject>().as_ref();
        if clear_other_attributes != 0 || attributes.is_none() {
            let _: () = msg_send![object(string), setAttributes: attributes, range: ns_range(range)];
        } else {
            let _: () = msg_send![object(string), addAttributes: attributes, range: ns_range(range)];
        }
    }
}

/// # Safety
///
/// `string` is a mutable attributed string, `name` a string, `value` an
/// object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringSetAttribute(
    string: *mut c_void,
    range: CFRange,
    name: *const c_void,
    value: *const c_void,
) {
    if string.is_null() || name.is_null() || value.is_null() {
        return;
    }
    // SAFETY: as the caller promises.
    unsafe {
        let _: () = msg_send![object(string), addAttribute: object(name), value: object(value), range: ns_range(range)];
    }
}

/// # Safety
///
/// `string` is a mutable attributed string, `name` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringRemoveAttribute(
    string: *mut c_void,
    range: CFRange,
    name: *const c_void,
) {
    if string.is_null() || name.is_null() {
        return;
    }
    // SAFETY: as the caller promises.
    unsafe {
        let _: () = msg_send![object(string), removeAttribute: object(name), range: ns_range(range)];
    }
}

/// # Safety
///
/// `string` is a mutable attributed string, `replacement` an attributed
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringReplaceAttributedString(
    string: *mut c_void,
    range: CFRange,
    replacement: *const c_void,
) {
    if string.is_null() || replacement.is_null() {
        return;
    }
    // SAFETY: as the caller promises.
    unsafe {
        let _: () = msg_send![object(string), replaceCharactersInRange: ns_range(range), withAttributedString: object(replacement)];
    }
}

/// # Safety
///
/// `string` is a mutable attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringBeginEditing(string: *mut c_void) {
    if !string.is_null() {
        // SAFETY: as the caller promises.
        unsafe {
            let _: () = msg_send![object(string), beginEditing];
        }
    }
}

/// # Safety
///
/// `string` is a mutable attributed string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringEndEditing(string: *mut c_void) {
    if !string.is_null() {
        // SAFETY: as the caller promises.
        unsafe {
            let _: () = msg_send![object(string), endEditing];
        }
    }
}

/// Bidi levels as the module says: whether any aren't level 0.
///
/// # Safety
///
/// `string` is an attributed string; `levels` and `directions` null or
/// with room for the range's units.
unsafe fn levels(string: *const c_void, range: CFRange, base: i8, levels: *mut u8, directions: *mut u8) -> bool {
    if string.is_null() {
        return false;
    }
    // SAFETY: -string returns a string.
    let text: Retained<objc2_foundation::NSString> = unsafe { msg_send![object(string), string] };
    let units: Vec<u16> = text.to_string().encode_utf16().collect();
    let start = (range.location.max(0) as usize).min(units.len());
    let end = start.saturating_add(range.length.max(0) as usize).min(units.len());
    let classes = CodePointMapData::<BidiClass>::new();
    let rtl = |u: u16| {
        let c = char::from_u32(u32::from(u)).unwrap_or('\u{fffd}');
        matches!(classes.get(c), BidiClass::RightToLeft | BidiClass::ArabicLetter)
    };
    let strong_ltr = |u: u16| {
        let c = char::from_u32(u32::from(u)).unwrap_or('\u{fffd}');
        classes.get(c) == BidiClass::LeftToRight
    };
    let any_rtl = units[start..end].iter().any(|&u| rtl(u));
    // Natural: the first strong character's direction.
    let base_level = match base {
        1 => 1,
        0 => 0,
        _ => u8::from(units[start..end].iter().find(|&&u| rtl(u) || strong_ltr(u)).is_some_and(|&u| rtl(u))),
    };
    for (k, &u) in units[start..end].iter().enumerate() {
        let level = match (rtl(u), base_level) {
            (true, _) => 1,
            (false, 1) if strong_ltr(u) => 2,
            (false, level) => level,
        };
        if !levels.is_null() {
            // SAFETY: as the caller promises.
            unsafe { levels.add(k).write(level) };
        }
        if !directions.is_null() {
            // SAFETY: as the caller promises.
            unsafe { directions.add(k).write(base_level) };
        }
    }
    any_rtl || base_level == 1
}

/// # Safety
///
/// As `levels`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetBidiLevelsAndResolvedDirections(
    string: *const c_void,
    range: CFRange,
    base_direction: i8,
    bidi_levels: *mut u8,
    base_directions: *mut u8,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { levels(string, range, base_direction, bidi_levels, base_directions) }
}

/// # Safety
///
/// As `levels`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFAttributedStringGetStatisticalWritingDirections(
    string: *const c_void,
    range: CFRange,
    base_direction: i8,
    bidi_levels: *mut u8,
    base_directions: *mut u8,
) -> bool {
    // SAFETY: as the caller promises.
    unsafe { levels(string, range, base_direction, bidi_levels, base_directions) }
}
