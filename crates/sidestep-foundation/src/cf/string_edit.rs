//! `CFMutableString` functions, and the `CFString` functions that search,
//! compare, split and join through `NSString`'s methods.
//!
//! Mutable strings are `NSMutableString`s, edited through its methods (so
//! a subclass sees each change through its primitive, as with Foundation's
//! own methods). CoreFoundation's comparison flags are `NSString`'s search
//! options but for one: CoreFoundation compares literally unless asked
//! for `kCFCompareNonliteral`, where `NSString` compares canonically
//! unless asked for `NSLiteralSearch`.
//!
//! A string made by `CFStringCreateMutableWithExternalCharactersNoCopy`
//! keeps its UTF-16 text in the caller's buffer, as CoreFoundation's does:
//! edits within the capacity write there, and growing past it reallocates
//! the buffer with `realloc` (the default allocator), or moves the text to
//! storage of the string's own for `kCFAllocatorNull`, which can't.

use std::cell::RefCell;
use std::ffi::{c_char, c_void};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSCharacterSet, NSMutableString, NSRange, NSString};

use super::string::{CFRange, text};
use super::types::{is_null_allocator, object, owned};

type CFIndex = isize;
type CFStringEncoding = u32;
type Boolean = u8;
type UniChar = u16;

/// `kCFCompareNonliteral` and `kCFCompareLocalized`.
const NONLITERAL: usize = 16;
const LOCALIZED: usize = 32;
/// `NSLiteralSearch`.
const LITERAL_SEARCH: usize = 2;

/// The `NSStringCompareOptions` for CoreFoundation's comparison flags.
fn ns_options(flags: usize) -> usize {
    let options = flags & !(NONLITERAL | LOCALIZED);
    if flags & NONLITERAL == 0 { options | LITERAL_SEARCH } else { options }
}

fn ns_range(range: CFRange) -> NSRange {
    NSRange::new(range.location.max(0) as usize, range.length.max(0) as usize)
}

fn cf_range(range: NSRange) -> CFRange {
    if range.location == usize::MAX || range.location as isize == isize::MAX {
        CFRange { location: -1, length: 0 }
    } else {
        CFRange { location: range.location as CFIndex, length: range.length as CFIndex }
    }
}

fn string<'a>(cf: *const c_void) -> &'a NSString {
    // SAFETY: the callers' contracts: `cf` is a string.
    unsafe { &*cf.cast::<NSString>() }
}

fn mutable<'a>(cf: *mut c_void) -> &'a NSMutableString {
    // SAFETY: the callers' contracts: `cf` is a mutable string.
    unsafe { &*cf.cast::<NSMutableString>() }
}

/// The string's length in UTF-16 units.
fn length(cf: *const c_void) -> usize {
    string(cf).length()
}

/// # Safety
///
/// `cf` is a mutable string; `appended` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringAppend(cf: *mut c_void, appended: *const c_void) {
    if appended.is_null() {
        return;
    }
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable(cf), appendString: string(appended)] };
}

/// # Safety
///
/// `cf` is a mutable string; `bytes` a NUL-terminated string in `encoding`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringAppendCString(
    cf: *mut c_void,
    bytes: *const c_char,
    encoding: CFStringEncoding,
) {
    // SAFETY: per this function's contract.
    let made = unsafe { super::string::CFStringCreateWithCString(std::ptr::null(), bytes, encoding) };
    append_owned(cf, made);
}

/// # Safety
///
/// `cf` is a mutable string; `chars` points to `count` UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringAppendCharacters(cf: *mut c_void, chars: *const UniChar, count: CFIndex) {
    // SAFETY: per this function's contract.
    let made = unsafe { super::string::CFStringCreateWithCharacters(std::ptr::null(), chars, count) };
    append_owned(cf, made);
}

/// # Safety
///
/// `cf` is a mutable string; `pascal` a length byte and that many bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringAppendPascalString(
    cf: *mut c_void,
    pascal: *const u8,
    encoding: CFStringEncoding,
) {
    // SAFETY: per this function's contract.
    let made = unsafe { super::string_encoding::CFStringCreateWithPascalString(std::ptr::null(), pascal, encoding) };
    append_owned(cf, made);
}

/// Append a +1 string (if any) and release it.
fn append_owned(cf: *mut c_void, made: *mut c_void) {
    if made.is_null() {
        return;
    }
    // SAFETY: a string this module made; its reference is released after.
    unsafe {
        CFStringAppend(cf, made);
        super::base::CFRelease(made);
    }
}

/// # Safety
///
/// `cf` is a mutable string of at least `index` units; `inserted` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringInsert(cf: *mut c_void, index: CFIndex, inserted: *const c_void) {
    if inserted.is_null() {
        return;
    }
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable(cf), insertString: string(inserted), atIndex: index.max(0) as usize] };
}

/// # Safety
///
/// `cf` is a mutable string the range lies in.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringDelete(cf: *mut c_void, range: CFRange) {
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable(cf), deleteCharactersInRange: ns_range(range)] };
}

/// # Safety
///
/// `cf` is a mutable string the range lies in; `replacement` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringReplace(cf: *mut c_void, range: CFRange, replacement: *const c_void) {
    let replacement =
        if replacement.is_null() { crate::string::empty() } else { objc2::Message::retain(string(replacement)) };
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable(cf), replaceCharactersInRange: ns_range(range), withString: &*replacement] };
}

/// # Safety
///
/// `cf` is a mutable string; `replacement` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringReplaceAll(cf: *mut c_void, replacement: *const c_void) {
    let replacement =
        if replacement.is_null() { crate::string::empty() } else { objc2::Message::retain(string(replacement)) };
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable(cf), setString: &*replacement] };
}

/// Replace each occurrence of `find` in the range; how many there were.
///
/// # Safety
///
/// `cf` is a mutable string the range lies in; `find` and `replacement`
/// are strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringFindAndReplace(
    cf: *mut c_void,
    find: *const c_void,
    replacement: *const c_void,
    range: CFRange,
    flags: usize,
) -> CFIndex {
    if find.is_null() || length(find) == 0 {
        return 0;
    }
    let replacement =
        if replacement.is_null() { crate::string::empty() } else { objc2::Message::retain(string(replacement)) };
    // SAFETY: per this function's contract.
    let count: usize = unsafe {
        msg_send![
            mutable(cf),
            replaceOccurrencesOfString: string(find),
            withString: &*replacement,
            options: ns_options(flags),
            range: ns_range(range)
        ]
    };
    count as CFIndex
}

/// Cut the string to `length` units, or lengthen it to that with `pad`'s
/// characters from `index` on, repeated.
///
/// # Safety
///
/// `cf` is a mutable string; `pad` null (when shortening) or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringPad(cf: *mut c_void, pad: *const c_void, new_length: CFIndex, index: CFIndex) {
    let current = length(cf);
    let new_length = new_length.max(0) as usize;
    if new_length <= current {
        if new_length < current {
            // SAFETY: a range within the string.
            let () = unsafe {
                msg_send![mutable(cf), deleteCharactersInRange: NSRange::new(new_length, current - new_length)]
            };
        }
        return;
    }
    if pad.is_null() || length(pad) == 0 {
        return;
    }
    let units: Vec<u16> = text(string(pad)).encode_utf16().collect();
    let start = (index.max(0) as usize) % units.len();
    let added: Vec<u16> = units.iter().copied().cycle().skip(start).take(new_length - current).collect();
    let added = NSString::from_str(&String::from_utf16_lossy(&added));
    // SAFETY: per this function's contract.
    let () = unsafe { msg_send![mutable(cf), appendString: &*added] };
}

/// Remove whole copies of `trim` from both ends.
///
/// # Safety
///
/// `cf` is a mutable string; `trim` a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringTrim(cf: *mut c_void, trim: *const c_void) {
    if trim.is_null() {
        return;
    }
    let (whole, trim) = (text(string(cf)).into_owned(), text(string(trim)).into_owned());
    if trim.is_empty() {
        return;
    }
    let mut rest = whole.as_str();
    while let Some(after) = rest.strip_prefix(trim.as_str()) {
        rest = after;
    }
    while let Some(before) = rest.strip_suffix(trim.as_str()) {
        rest = before;
    }
    if rest.len() != whole.len() {
        set(cf, rest);
    }
}

/// Remove whitespace and line ends from both ends.
///
/// # Safety
///
/// `cf` is a mutable string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringTrimWhitespace(cf: *mut c_void) {
    let set = NSCharacterSet::whitespaceAndNewlineCharacterSet();
    // SAFETY: -stringByTrimmingCharactersInSet: returns a string.
    let trimmed: Retained<NSString> = unsafe { msg_send![mutable(cf), stringByTrimmingCharactersInSet: &*set] };
    if trimmed.length() != length(cf) {
        // SAFETY: per this function's contract.
        let () = unsafe { msg_send![mutable(cf), setString: &*trimmed] };
    }
}

/// Replace the text with `new`.
fn set(cf: *mut c_void, new: &str) {
    let new = NSString::from_str(new);
    // SAFETY: the callers' contracts: `cf` is a mutable string.
    let () = unsafe { msg_send![mutable(cf), setString: &*new] };
}

/// Replace the text with what `f` makes of it, if that differs.
fn map(cf: *mut c_void, f: impl FnOnce(&NSString) -> Retained<NSString>) {
    let new = f(mutable(cf));
    // SAFETY: -isEqualToString: takes a string.
    let same: bool = unsafe { msg_send![mutable(cf), isEqualToString: &*new] };
    if !same {
        // SAFETY: the callers' contracts: `cf` is a mutable string.
        let () = unsafe { msg_send![mutable(cf), setString: &*new] };
    }
}

/// # Safety
///
/// `cf` is a mutable string; `locale` null or a locale.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringUppercase(cf: *mut c_void, locale: *const c_void) {
    // SAFETY: -uppercaseStringWithLocale: takes a locale or nil.
    map(cf, |s| unsafe { msg_send![s, uppercaseStringWithLocale: locale.cast::<AnyObject>().as_ref()] });
}

/// # Safety
///
/// As [`CFStringUppercase`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringLowercase(cf: *mut c_void, locale: *const c_void) {
    // SAFETY: as above.
    map(cf, |s| unsafe { msg_send![s, lowercaseStringWithLocale: locale.cast::<AnyObject>().as_ref()] });
}

/// # Safety
///
/// As [`CFStringUppercase`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCapitalize(cf: *mut c_void, locale: *const c_void) {
    // SAFETY: as above.
    map(cf, |s| unsafe { msg_send![s, capitalizedStringWithLocale: locale.cast::<AnyObject>().as_ref()] });
}

/// Fold for comparison: case, diacritics and width, as the flags say.
///
/// # Safety
///
/// As [`CFStringUppercase`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringFold(cf: *mut c_void, flags: usize, locale: *const c_void) {
    // `kCFCompareCaseInsensitive`, `…DiacriticInsensitive` and
    // `…WidthInsensitive` are NSString's options too.
    let options = flags & (1 | 128 | 256);
    // SAFETY: as above.
    map(cf, |s| unsafe {
        msg_send![s, stringByFoldingWithOptions: options, locale: locale.cast::<AnyObject>().as_ref()]
    });
}

/// # Safety
///
/// `cf` is a mutable string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringNormalize(cf: *mut c_void, form: CFIndex) {
    // kCFStringNormalizationFormD, KD, C and KC.
    // SAFETY: each returns a string.
    map(cf, |s| unsafe {
        match form {
            0 => msg_send![s, decomposedStringWithCanonicalMapping],
            1 => msg_send![s, decomposedStringWithCompatibilityMapping],
            2 => msg_send![s, precomposedStringWithCanonicalMapping],
            _ => msg_send![s, precomposedStringWithCompatibilityMapping],
        }
    });
}

/// Apply a transform to the string, or the part of it the range covers
/// (updated to the result's range); whether the transform is known. See
/// [`super::string_transform`] for the ones Sidestep has.
///
/// # Safety
///
/// `cf` is a mutable string; `range` null or a range within it; `name` a
/// string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringTransform(
    cf: *mut c_void,
    range: *mut CFRange,
    name: *const c_void,
    reverse: Boolean,
) -> Boolean {
    if name.is_null() {
        return 0;
    }
    let Some(transform) = super::string_transform::Transform::named(&text(string(name))) else { return 0 };
    let all = CFRange { location: 0, length: length(cf) as CFIndex };
    // SAFETY: per this function's contract.
    let within = unsafe { range.as_ref() }.copied().unwrap_or(all);
    let part = super::string::substring(string(cf), within);
    let Some(part) = part else { return 0 };
    let changed = transform.apply(&part, reverse != 0);
    let new_length = changed.encode_utf16().count() as CFIndex;
    if changed != part {
        let replacement = NSString::from_str(&changed);
        // SAFETY: a range within the string.
        let () =
            unsafe { msg_send![mutable(cf), replaceCharactersInRange: ns_range(within), withString: &*replacement] };
    }
    if !range.is_null() {
        // SAFETY: per this function's contract.
        unsafe { range.write(CFRange { location: within.location, length: new_length }) };
    }
    1
}

/// # Safety
///
/// `cf` is a string; `find` null or a string; `result` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringFindWithOptions(
    cf: *const c_void,
    find: *const c_void,
    range: CFRange,
    flags: usize,
    result: *mut CFRange,
) -> Boolean {
    // SAFETY: per this function's contract.
    unsafe { CFStringFindWithOptionsAndLocale(cf, find, range, flags, std::ptr::null(), result) }
}

/// # Safety
///
/// As [`CFStringFindWithOptions`]; `locale` null or a locale.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringFindWithOptionsAndLocale(
    cf: *const c_void,
    find: *const c_void,
    range: CFRange,
    flags: usize,
    locale: *const c_void,
    result: *mut CFRange,
) -> Boolean {
    if find.is_null() || length(find) == 0 {
        return 0;
    }
    // SAFETY: per this function's contract.
    let found: NSRange = unsafe {
        msg_send![
            string(cf),
            rangeOfString: string(find),
            options: ns_options(flags),
            range: ns_range(range),
            locale: locale.cast::<AnyObject>().as_ref()
        ]
    };
    write_found(found, result)
}

/// Where a character of `set` first (or, backwards, last) is in the range.
///
/// # Safety
///
/// `cf` is a string; `set` null or a character set; `result` null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringFindCharacterFromSet(
    cf: *const c_void,
    set: *const c_void,
    range: CFRange,
    flags: usize,
    result: *mut CFRange,
) -> Boolean {
    if set.is_null() {
        return 0;
    }
    // SAFETY: per this function's contract.
    let found: NSRange = unsafe {
        msg_send![string(cf), rangeOfCharacterFromSet: object(set), options: ns_options(flags), range: ns_range(range)]
    };
    write_found(found, result)
}

/// Store a found range and say whether there was one.
fn write_found(found: NSRange, result: *mut CFRange) -> Boolean {
    let found = cf_range(found);
    if found.location < 0 {
        return 0;
    }
    if !result.is_null() {
        // SAFETY: the callers' contracts: `result` is writable.
        unsafe { result.write(found) };
    }
    1
}

/// # Safety
///
/// `a` is a string the range lies in; `b` a string; `locale` null or a
/// locale.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCompareWithOptionsAndLocale(
    a: *const c_void,
    b: *const c_void,
    range: CFRange,
    flags: usize,
    locale: *const c_void,
) -> CFIndex {
    if b.is_null() {
        return 1;
    }
    // A localized comparison without a locale uses the current one.
    let current = (flags & LOCALIZED != 0 && locale.is_null()).then(objc2_foundation::NSLocale::currentLocale);
    let locale: Option<&AnyObject> = match &current {
        Some(current) => Some(current.as_ref()),
        // SAFETY: null or a locale.
        None => unsafe { locale.cast::<AnyObject>().as_ref() },
    };
    // SAFETY: per this function's contract.
    let order: isize = unsafe {
        msg_send![string(a), compare: string(b), options: ns_options(flags), range: ns_range(range), locale: locale]
    };
    order
}

/// # Safety
///
/// `cf` is a string with more than `index` units.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetRangeOfComposedCharactersAtIndex(
    cf: *const c_void,
    index: CFIndex,
) -> CFRange {
    // SAFETY: per this function's contract.
    let found: NSRange =
        unsafe { msg_send![string(cf), rangeOfComposedCharacterSequenceAtIndex: index.max(0) as usize] };
    cf_range(found)
}

/// # Safety
///
/// `cf` is a string the range lies in; the others null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetLineBounds(
    cf: *const c_void,
    range: CFRange,
    begin: *mut CFIndex,
    end: *mut CFIndex,
    contents_end: *mut CFIndex,
) {
    // SAFETY: per this function's contract; CFIndex and NSUInteger have the
    // same size, and the indices fit either.
    let () = unsafe {
        msg_send![
            string(cf),
            getLineStart: begin.cast::<usize>(),
            end: end.cast::<usize>(),
            contentsEnd: contents_end.cast::<usize>(),
            forRange: ns_range(range)
        ]
    };
}

/// # Safety
///
/// As [`CFStringGetLineBounds`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetParagraphBounds(
    cf: *const c_void,
    range: CFRange,
    begin: *mut CFIndex,
    end: *mut CFIndex,
    contents_end: *mut CFIndex,
) {
    // SAFETY: as above.
    let () = unsafe {
        msg_send![
            string(cf),
            getParagraphStart: begin.cast::<usize>(),
            end: end.cast::<usize>(),
            contentsEnd: contents_end.cast::<usize>(),
            forRange: ns_range(range)
        ]
    };
}

/// # Safety
///
/// `cf` is null or a string; `separator` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateArrayBySeparatingStrings(
    _alloc: *const c_void,
    cf: *const c_void,
    separator: *const c_void,
) -> *mut c_void {
    if cf.is_null() || separator.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let parts: Retained<NSArray> = unsafe { msg_send![string(cf), componentsSeparatedByString: string(separator)] };
    owned(parts)
}

/// # Safety
///
/// `array` is null or an array of strings; `separator` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateByCombiningStrings(
    _alloc: *const c_void,
    array: *const c_void,
    separator: *const c_void,
) -> *mut c_void {
    if array.is_null() {
        return std::ptr::null_mut();
    }
    let separator =
        if separator.is_null() { crate::string::empty() } else { objc2::Message::retain(string(separator)) };
    // SAFETY: per this function's contract.
    let joined: Retained<NSString> = unsafe { msg_send![object(array), componentsJoinedByString: &*separator] };
    owned(joined)
}

/// The ranges where `find` occurs in the range, as an array whose values
/// point to `CFRange`s; NULL if there are none.
///
/// # Safety
///
/// `cf` is null or a string the range lies in; `find` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateArrayWithFindResults(
    _alloc: *const c_void,
    cf: *const c_void,
    find: *const c_void,
    range: CFRange,
    flags: usize,
) -> *mut c_void {
    if cf.is_null() || find.is_null() || length(find) == 0 {
        return std::ptr::null_mut();
    }
    let backwards = flags & 4 != 0;
    let mut found = Vec::new();
    let (mut start, mut end) = (range.location.max(0), range.location.max(0) + range.length.max(0));
    while start < end {
        let mut hit = CFRange { location: -1, length: 0 };
        // SAFETY: per this function's contract.
        let ok = unsafe {
            CFStringFindWithOptions(cf, find, CFRange { location: start, length: end - start }, flags, &mut hit)
        };
        if ok == 0 {
            break;
        }
        found.push(hit);
        if backwards {
            end = hit.location;
        } else {
            start = hit.location + hit.length.max(1);
        }
    }
    if found.is_empty() {
        return std::ptr::null_mut();
    }
    owned(super::value_array::of_ranges(found))
}

/// The external buffer of a string made with
/// `CFStringCreateMutableWithExternalCharactersNoCopy`.
struct External {
    chars: *mut UniChar,
    len: usize,
    capacity: usize,
    /// The buffer is the caller's and can't grow (`kCFAllocatorNull`);
    /// otherwise it came from `malloc` and grows with `realloc`.
    fixed: bool,
    /// The text, once it outgrew a fixed buffer.
    own: Option<Vec<UniChar>>,
}

impl External {
    fn units(&self) -> &[UniChar] {
        match &self.own {
            Some(own) => own,
            None if self.chars.is_null() || self.len == 0 => &[],
            // SAFETY: the caller's buffer holds `len` units.
            None => unsafe { std::slice::from_raw_parts(self.chars, self.len) },
        }
    }

    fn replace(&mut self, range: std::ops::Range<usize>, with: &[UniChar]) {
        let mut units = self.units().to_vec();
        units.splice(range, with.iter().copied());
        if self.own.is_none() && units.len() > self.capacity {
            if self.fixed {
                self.own = Some(units);
                return;
            }
            let bytes = units.len().max(1) * size_of::<UniChar>();
            // SAFETY: a malloc'd buffer (or null) grown to hold the text.
            let grown = unsafe { libc::realloc(self.chars.cast(), bytes) }.cast::<UniChar>();
            if grown.is_null() {
                self.own = Some(units);
                return;
            }
            (self.chars, self.capacity) = (grown, units.len());
        }
        match &mut self.own {
            Some(own) => *own = units,
            None => {
                // SAFETY: the buffer holds `capacity` units, at least these.
                unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), self.chars, units.len()) };
                self.len = units.len();
            }
        }
    }
}

define_class!(
    /// A mutable string whose text is the caller's UTF-16 buffer.
    #[unsafe(super(NSMutableString, NSString, NSObject))]
    #[name = "_SidestepCFExternalString"]
    #[ivars = RefCell<External>]
    struct ExternalString;

    impl ExternalString {
        #[unsafe(method(length))]
        fn length(&self) -> usize {
            self.ivars().borrow().units().len()
        }

        #[unsafe(method(characterAtIndex:))]
        fn character_at(&self, index: usize) -> UniChar {
            let external = self.ivars().borrow();
            match external.units().get(index) {
                Some(&unit) => unit,
                None => crate::string::index_panic("characterAtIndex:", index, external.units().len()),
            }
        }

        #[unsafe(method(getCharacters:range:))]
        fn get_characters(&self, buffer: *mut UniChar, range: NSRange) {
            let external = self.ivars().borrow();
            let units = external.units();
            let Some(part) = units.get(range.location..range.location.saturating_add(range.length)) else {
                crate::string::index_panic("getCharacters:range:", range.location.saturating_add(range.length), units.len());
            };
            // SAFETY: the caller's buffer holds the range.
            unsafe { std::ptr::copy_nonoverlapping(part.as_ptr(), buffer, part.len()) };
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace(&self, range: NSRange, with: &NSString) {
            let with: Vec<UniChar> = text(with).encode_utf16().collect();
            let mut external = self.ivars().borrow_mut();
            let len = external.units().len();
            let end = range.location.saturating_add(range.length);
            if end > len {
                drop(external);
                crate::string::index_panic("replaceCharactersInRange:withString:", end, len);
            }
            external.replace(range.location..end, &with);
        }
    }
);

impl Drop for External {
    fn drop(&mut self) {
        if !self.fixed && !self.chars.is_null() {
            // SAFETY: a buffer the string owns, from malloc.
            unsafe { libc::free(self.chars.cast()) };
        }
    }
}

/// # Safety
///
/// `chars` points to a buffer of `capacity` units holding `count`; unless
/// `allocator` is `kCFAllocatorNull`, from `malloc`, and the string's from
/// now on.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateMutableWithExternalCharactersNoCopy(
    _alloc: *const c_void,
    chars: *mut UniChar,
    count: CFIndex,
    capacity: CFIndex,
    allocator: *const c_void,
) -> *mut c_void {
    let len = count.max(0) as usize;
    let external = External {
        chars,
        len,
        capacity: (capacity.max(0) as usize).max(len),
        fixed: is_null_allocator(allocator),
        own: None,
    };
    let this = ExternalString::alloc().set_ivars(RefCell::new(external));
    // SAFETY: NSObject's -init; NSMutableString's own storage stays empty.
    let this: Retained<ExternalString> = unsafe { msg_send![super(this), init] };
    owned(this)
}

/// Give a string from `CFStringCreateMutableWithExternalCharactersNoCopy`
/// another buffer; other strings are left alone.
///
/// # Safety
///
/// `cf` is a mutable string; `chars` as for
/// [`CFStringCreateMutableWithExternalCharactersNoCopy`], the caller's.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringSetExternalCharactersNoCopy(
    cf: *mut c_void,
    chars: *mut UniChar,
    count: CFIndex,
    capacity: CFIndex,
) {
    // SAFETY: per this function's contract.
    let Some(this) = (unsafe { object(cf) }).downcast_ref::<ExternalString>() else { return };
    let len = count.max(0) as usize;
    let mut external = this.ivars().borrow_mut();
    let fixed = external.fixed;
    // The old buffer goes back to the caller unfreed; the new one grows as
    // the string's allocator allows.
    external.fixed = true;
    *external = External { chars, len, capacity: (capacity.max(0) as usize).max(len), fixed, own: None };
}

/// # Safety
///
/// `cf` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateMutableCopy(
    _alloc: *const c_void,
    _max: CFIndex,
    cf: *const c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: -mutableCopy returns a mutable string.
    let copy: Retained<NSMutableString> = unsafe { msg_send![string(cf), mutableCopy] };
    owned(copy)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringCreateMutable(_alloc: *const c_void, _max: CFIndex) -> *mut c_void {
    // SAFETY: NSMutableString's -init.
    let made: Retained<NSMutableString> = unsafe {
        let this: Allocated<NSMutableString> = msg_send![NSMutableString::class(), alloc];
        msg_send![this, init]
    };
    owned(made)
}
