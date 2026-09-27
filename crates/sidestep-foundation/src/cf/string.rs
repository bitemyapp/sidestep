//! `CFString` functions over `NSString`.
//!
//! Strings are Foundation's strings; these functions read their text and
//! make new ones. Indices and lengths count UTF-16 code units, as
//! CoreFoundation's do. `CFStringGetCStringPtr` and
//! `CFStringGetCharactersPtr` return NULL, which CoreFoundation allows,
//! so callers fall back on the copying functions, a unit or a chunk at a
//! time: those take the length the string keeps (`-length`) rather than
//! walking its text, index ASCII text (as many units as bytes) directly,
//! and encode other text only up to the end of the range asked for, with
//! nothing collected. Allocators are ignored.
//!
//! Encodings: UTF-8, ASCII, ISO Latin 1, Windows Latin 1 (as Latin 1),
//! Mac Roman, and UTF-16 and UTF-32 in either byte order (external
//! representations with a byte-order mark).

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::sync::{Mutex, OnceLock};

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSString, NSUInteger};

use super::types::{CFTypeID, id, is_null_allocator, object, owned};

type CFIndex = isize;
type CFStringEncoding = u32;
type Boolean = u8;
type UniChar = u16;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CFRange {
    pub location: CFIndex,
    pub length: CFIndex,
}

/// `kCFNotFound`.
const NOT_FOUND: CFIndex = -1;

pub(crate) mod encoding {
    pub(crate) const MAC_ROMAN: u32 = 0;
    pub(crate) const WINDOWS_LATIN1: u32 = 0x0500;
    pub(crate) const ISO_LATIN1: u32 = 0x0201;
    pub(crate) const ASCII: u32 = 0x0600;
    pub(crate) const UTF8: u32 = 0x0800_0100;
    pub(crate) const UTF16: u32 = 0x0100;
    pub(crate) const UTF16_BE: u32 = 0x1000_0100;
    pub(crate) const UTF16_LE: u32 = 0x1400_0100;
    pub(crate) const UTF32: u32 = 0x0c00_0100;
    pub(crate) const UTF32_BE: u32 = 0x1800_0100;
    pub(crate) const UTF32_LE: u32 = 0x1c00_0100;
}

/// Mac Roman's upper half.
const MAC_ROMAN_HIGH: [char; 128] = [
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è', 'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ',
    'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü', '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠',
    'Æ', 'Ø', '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø', '¿', '¡', '¬', '√', 'ƒ',
    '≈', '∆', '«', '»', '…', '\u{a0}', 'À', 'Ã', 'Õ', 'Œ', 'œ', '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄',
    '€', '‹', '›', 'ﬁ', 'ﬂ', '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô',
    '\u{f8ff}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ',
];

/// Text from bytes in an encoding; `None` if they aren't valid in it.
pub(crate) fn decode(bytes: &[u8], encoding: CFStringEncoding, external: bool) -> Option<String> {
    let units16 = |bytes: &[u8], big: bool| -> Vec<u16> {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| if big { u16::from_be_bytes(c) } else { u16::from_le_bytes(c) })
            .collect()
    };
    let units32 = |bytes: &[u8], big: bool| -> Option<String> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| char::from_u32(if big { u32::from_be_bytes(c) } else { u32::from_le_bytes(c) }))
            .collect()
    };
    match encoding {
        encoding::UTF8 => {
            let bytes = if external { bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes) } else { bytes };
            std::str::from_utf8(bytes).ok().map(str::to_string)
        }
        // Bytes past ASCII read as Latin 1 rather than failing.
        encoding::ASCII | encoding::ISO_LATIN1 | encoding::WINDOWS_LATIN1 => {
            Some(bytes.iter().map(|&b| b as char).collect())
        }
        encoding::MAC_ROMAN => {
            Some(bytes.iter().map(|&b| if b < 0x80 { b as char } else { MAC_ROMAN_HIGH[b as usize - 0x80] }).collect())
        }
        encoding::UTF16 | encoding::UTF16_BE | encoding::UTF16_LE => {
            if !bytes.len().is_multiple_of(2) {
                return None;
            }
            let (big, bytes) = match (encoding, bytes) {
                (encoding::UTF16_BE, _) => (true, bytes),
                (encoding::UTF16_LE, _) => (false, bytes),
                (_, [0xfe, 0xff, rest @ ..]) if external => (true, rest),
                (_, [0xff, 0xfe, rest @ ..]) if external => (false, rest),
                // External representations without a mark are big-endian;
                // internal ones are in the machine's order.
                _ => (external || cfg!(target_endian = "big"), bytes),
            };
            String::from_utf16(&units16(bytes, big)).ok()
        }
        encoding::UTF32 | encoding::UTF32_BE | encoding::UTF32_LE => {
            if !bytes.len().is_multiple_of(4) {
                return None;
            }
            let (big, bytes) = match (encoding, bytes) {
                (encoding::UTF32_BE, _) => (true, bytes),
                (encoding::UTF32_LE, _) => (false, bytes),
                (_, [0, 0, 0xfe, 0xff, rest @ ..]) if external => (true, rest),
                (_, [0xff, 0xfe, 0, 0, rest @ ..]) if external => (false, rest),
                _ => (external || cfg!(target_endian = "big"), bytes),
            };
            units32(bytes, big)
        }
        _ => None,
    }
}

/// A character's bytes in an encoding; `None` if it can't be encoded.
pub(crate) fn encode_char(c: char, encoding: CFStringEncoding, out: &mut Vec<u8>) -> bool {
    match encoding {
        encoding::UTF8 => {
            let mut buf = [0; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        encoding::ASCII if c.is_ascii() => out.push(c as u8),
        encoding::ISO_LATIN1 | encoding::WINDOWS_LATIN1 if (c as u32) < 0x100 => out.push(c as u8),
        encoding::MAC_ROMAN if c.is_ascii() => out.push(c as u8),
        encoding::MAC_ROMAN => match MAC_ROMAN_HIGH.iter().position(|&m| m == c) {
            Some(i) => out.push(0x80 + i as u8),
            None => return false,
        },
        encoding::UTF16 | encoding::UTF16_BE | encoding::UTF16_LE => {
            let mut buf = [0; 2];
            for unit in c.encode_utf16(&mut buf) {
                let bytes = match encoding {
                    encoding::UTF16_BE => unit.to_be_bytes(),
                    encoding::UTF16_LE => unit.to_le_bytes(),
                    _ => unit.to_ne_bytes(),
                };
                out.extend_from_slice(&bytes);
            }
        }
        encoding::UTF32 | encoding::UTF32_BE | encoding::UTF32_LE => {
            let bytes = match encoding {
                encoding::UTF32_BE => (c as u32).to_be_bytes(),
                encoding::UTF32_LE => (c as u32).to_le_bytes(),
                _ => (c as u32).to_ne_bytes(),
            };
            out.extend_from_slice(&bytes);
        }
        _ => return false,
    }
    true
}

/// The text of a string object.
pub(crate) fn text(object: &AnyObject) -> Cow<'_, str> {
    if let Some((text, _)) = crate::string::fast_parts(object) {
        return Cow::Borrowed(text);
    }
    match object.downcast_ref::<NSString>() {
        Some(string) => Cow::Owned(string.to_string()),
        None => Cow::Borrowed(""),
    }
}

/// A string's length in UTF-16 units, which Sidestep's strings keep.
fn length_of(object: &AnyObject) -> usize {
    // SAFETY: strings answer -length.
    let length: NSUInteger = unsafe { msg_send![object, length] };
    length
}

/// The UTF-16 units of a range of a string's text, if the range lies
/// within its `length` units.
fn range_units(text: &str, length: usize, range: CFRange) -> Option<Units<'_>> {
    let start = usize::try_from(range.location).ok()?;
    let count = usize::try_from(range.length).ok()?;
    let end = start.checked_add(count).filter(|&end| end <= length)?;
    Some(if length == text.len() {
        // As many units as bytes: the text is ASCII.
        Units::Ascii(text.as_bytes()[start..end].iter())
    } else {
        let (at, skip) = unit_offset(text, start);
        Units::Encoded(text[at..].encode_utf16().skip(skip).take(count))
    })
}

/// Where UTF-16 unit `unit` of `text` starts: the byte offset of the
/// character it belongs to, and 1 if it is the second unit of that
/// character's surrogate pair (else 0). Units are counted from the bytes,
/// 64 at a time where the unit lies further on: a byte starts a unit
/// unless it continues a character, and a four-byte character's first byte
/// starts two.
fn unit_offset(text: &str, unit: usize) -> (usize, usize) {
    fn units_of(b: u8) -> u8 {
        u8::from(b & 0xc0 != 0x80) + u8::from(b >= 0xf0)
    }
    let bytes = text.as_bytes();
    let (mut units, mut at) = (0, 0);
    for chunk in bytes.as_chunks::<64>().0 {
        let n = usize::from(chunk.iter().fold(0u8, |sum, &b| sum + units_of(b)));
        if units + n > unit {
            break;
        }
        units += n;
        at += 64;
    }
    while let Some(&b) = bytes.get(at) {
        let n = usize::from(units_of(b));
        if units + n > unit {
            return (at, unit - units);
        }
        units += n;
        at += 1;
    }
    (at, 0)
}

/// The units [`range_units`] yields, without collecting them.
enum Units<'a> {
    Ascii(std::slice::Iter<'a, u8>),
    Encoded(std::iter::Take<std::iter::Skip<std::str::EncodeUtf16<'a>>>),
}

impl Units<'_> {
    /// The text the units spell, a lone surrogate replaced.
    fn into_string(self) -> String {
        char::decode_utf16(self).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
    }
}

impl Iterator for Units<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        match self {
            Units::Ascii(bytes) => bytes.next().map(|&b| u16::from(b)),
            Units::Encoded(units) => units.next(),
        }
    }
}

/// The text of a range of a string's UTF-16 units, if the range lies
/// within it.
pub(crate) fn substring(string: &AnyObject, range: CFRange) -> Option<String> {
    let text = text(string);
    range_units(&text, length_of(string), range).map(Units::into_string)
}

fn make(text: &str) -> *mut c_void {
    owned(NSString::from_str(text))
}

/// Free a no-copy buffer the way its deallocator asks.
///
/// # Safety
///
/// `bytes` came from `malloc` unless the deallocator is `kCFAllocatorNull`.
pub(crate) unsafe fn free_contents(bytes: *const c_void, deallocator: *const c_void) {
    if !is_null_allocator(deallocator) {
        // SAFETY: per this function's contract.
        unsafe { libc::free(bytes.cast_mut()) };
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetTypeID() -> CFTypeID {
    id::STRING
}

/// # Safety
///
/// `bytes` points to `length` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithBytes(
    _alloc: *const c_void,
    bytes: *const u8,
    length: CFIndex,
    encoding: CFStringEncoding,
    external: Boolean,
) -> *mut c_void {
    let bytes = if length <= 0 || bytes.is_null() {
        &[][..]
    } else {
        // SAFETY: per this function's contract.
        unsafe { std::slice::from_raw_parts(bytes, length as usize) }
    };
    decode(bytes, encoding, external != 0).map_or(std::ptr::null_mut(), |t| make(&t))
}

/// # Safety
///
/// As [`CFStringCreateWithBytes`]; the bytes were allocated with `malloc`
/// unless `deallocator` is `kCFAllocatorNull`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithBytesNoCopy(
    alloc: *const c_void,
    bytes: *const u8,
    length: CFIndex,
    encoding: CFStringEncoding,
    external: Boolean,
    deallocator: *const c_void,
) -> *mut c_void {
    // The text is copied, so the buffer can go at once.
    // SAFETY: per this function's contract.
    let string = unsafe { CFStringCreateWithBytes(alloc, bytes, length, encoding, external) };
    // SAFETY: as above.
    unsafe { free_contents(bytes.cast(), deallocator) };
    string
}

/// # Safety
///
/// `text` is a NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithCString(
    _alloc: *const c_void,
    text: *const c_char,
    encoding: CFStringEncoding,
) -> *mut c_void {
    if text.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
    decode(bytes, encoding, false).map_or(std::ptr::null_mut(), |t| make(&t))
}

/// # Safety
///
/// As [`CFStringCreateWithCString`] and [`CFStringCreateWithBytesNoCopy`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithCStringNoCopy(
    alloc: *const c_void,
    text: *const c_char,
    encoding: CFStringEncoding,
    deallocator: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let string = unsafe { CFStringCreateWithCString(alloc, text, encoding) };
    // SAFETY: as above.
    unsafe { free_contents(text.cast(), deallocator) };
    string
}

/// # Safety
///
/// `chars` points to `length` UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithCharacters(
    _alloc: *const c_void,
    chars: *const UniChar,
    length: CFIndex,
) -> *mut c_void {
    let units = if length <= 0 || chars.is_null() {
        &[][..]
    } else {
        // SAFETY: per this function's contract.
        unsafe { std::slice::from_raw_parts(chars, length as usize) }
    };
    make(&String::from_utf16_lossy(units))
}

/// # Safety
///
/// As [`CFStringCreateWithCharacters`] and [`CFStringCreateWithBytesNoCopy`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithCharactersNoCopy(
    alloc: *const c_void,
    chars: *const UniChar,
    length: CFIndex,
    deallocator: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let string = unsafe { CFStringCreateWithCharacters(alloc, chars, length) };
    // SAFETY: as above.
    unsafe { free_contents(chars.cast(), deallocator) };
    string
}

/// # Safety
///
/// `string` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateCopy(_alloc: *const c_void, string: *const c_void) -> *mut c_void {
    // SAFETY: per this function's contract.
    make(&text(unsafe { object(string) }))
}

/// # Safety
///
/// `string` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithSubstring(
    _alloc: *const c_void,
    string: *const c_void,
    range: CFRange,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let string = unsafe { object(string) };
    let text = text(string);
    let Some(units) = range_units(&text, length_of(string), range) else { return std::ptr::null_mut() };
    make(&units.into_string())
}

/// # Safety
///
/// `string` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetLength(string: *const c_void) -> CFIndex {
    // SAFETY: per this function's contract.
    length_of(unsafe { object(string) }) as CFIndex
}

/// # Safety
///
/// `string` is a string with more than `index` UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetCharacterAtIndex(string: *const c_void, index: CFIndex) -> UniChar {
    // SAFETY: per this function's contract.
    let string = unsafe { object(string) };
    let text = text(string);
    range_units(&text, length_of(string), CFRange { location: index, length: 1 })
        .and_then(|mut u| u.next())
        .unwrap_or(0)
}

/// # Safety
///
/// `string` is a string; `buffer` has room for `range.length` units.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetCharacters(string: *const c_void, range: CFRange, buffer: *mut UniChar) {
    // SAFETY: per this function's contract.
    let string = unsafe { object(string) };
    let text = text(string);
    if let Some(units) = range_units(&text, length_of(string), range) {
        for (i, unit) in units.enumerate() {
            // SAFETY: the caller's buffer has room for the range.
            unsafe { buffer.add(i).write(unit) };
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetCStringPtr(_string: *const c_void, _encoding: CFStringEncoding) -> *const c_char {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetCharactersPtr(_string: *const c_void) -> *const UniChar {
    std::ptr::null()
}

/// # Safety
///
/// `string` is a string; `buffer` has room for `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetCString(
    string: *const c_void,
    buffer: *mut c_char,
    size: CFIndex,
    encoding: CFStringEncoding,
) -> Boolean {
    // SAFETY: per this function's contract.
    let text = text(unsafe { object(string) });
    let mut bytes = Vec::with_capacity(text.len() + 1);
    if !text.chars().all(|c| encode_char(c, encoding, &mut bytes)) || bytes.contains(&0) && encoding == encoding::UTF8 {
        return 0;
    }
    if bytes.len() + 1 > size.max(0) as usize {
        return 0;
    }
    bytes.push(0);
    // SAFETY: the caller's buffer holds `size` bytes, more than these.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), bytes.len()) };
    1
}

/// # Safety
///
/// `string` is a string; `buffer` is null or has room for `max` bytes;
/// `used` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetBytes(
    string: *const c_void,
    range: CFRange,
    encoding: CFStringEncoding,
    loss_byte: u8,
    external: Boolean,
    buffer: *mut u8,
    max: CFIndex,
    used: *mut CFIndex,
) -> CFIndex {
    // SAFETY: per this function's contract.
    let string = unsafe { object(string) };
    let text = text(string);
    let Some(units) = range_units(&text, length_of(string), range) else { return 0 };
    let mut out = Vec::new();
    if external != 0 && matches!(encoding, encoding::UTF16 | encoding::UTF32) {
        encode_char('\u{feff}', encoding, &mut out);
    }
    let limit = if buffer.is_null() { usize::MAX } else { max.max(0) as usize };
    let mut converted = 0;
    for c in char::decode_utf16(units) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        let before = out.len();
        if !encode_char(c, encoding, &mut out) {
            if loss_byte == 0 {
                break;
            }
            out.push(loss_byte);
        }
        if out.len() > limit {
            out.truncate(before);
            break;
        }
        converted += c.len_utf16();
    }
    if !buffer.is_null() {
        // SAFETY: at most `max` bytes, which the caller's buffer holds.
        unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), buffer, out.len()) };
    }
    if !used.is_null() {
        // SAFETY: per this function's contract.
        unsafe { used.write(out.len() as CFIndex) };
    }
    converted as CFIndex
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: CFStringEncoding) -> CFIndex {
    let per_unit = match encoding {
        encoding::UTF8 => 3,
        encoding::UTF16 | encoding::UTF16_BE | encoding::UTF16_LE => 2,
        encoding::UTF32 | encoding::UTF32_BE | encoding::UTF32_LE => 4,
        _ => 1,
    };
    length.saturating_mul(per_unit)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetSystemEncoding() -> CFStringEncoding {
    encoding::UTF8
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringIsEncodingAvailable(encoding: CFStringEncoding) -> Boolean {
    let mut sink = Vec::new();
    u8::from(encode_char('a', encoding, &mut sink))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetFastestEncoding(_string: *const c_void) -> CFStringEncoding {
    encoding::UTF8
}

/// `kCFCompareCaseInsensitive` and `kCFCompareNumerically`.
const CASE_INSENSITIVE: usize = 1;
const NUMERICALLY: usize = 64;

/// Compare, optionally ignoring case and comparing digit runs by value.
pub(crate) fn compare(a: &str, b: &str, flags: usize) -> std::cmp::Ordering {
    let fold = |s: &str| -> Vec<char> {
        if flags & CASE_INSENSITIVE != 0 {
            s.chars().flat_map(char::to_lowercase).collect()
        } else {
            s.chars().collect()
        }
    };
    let (a, b) = (fold(a), fold(b));
    if flags & NUMERICALLY == 0 {
        let (a, b): (Vec<u16>, Vec<u16>) = (
            a.iter().collect::<String>().encode_utf16().collect(),
            b.iter().collect::<String>().encode_utf16().collect(),
        );
        return a.cmp(&b);
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let x: String = a[si..i].iter().collect::<String>().trim_start_matches('0').to_string();
            let y: String = b[sj..j].iter().collect::<String>().trim_start_matches('0').to_string();
            let order = x.len().cmp(&y.len()).then_with(|| x.cmp(&y));
            if order.is_ne() {
                return order;
            }
        } else {
            let order = a[i].cmp(&b[j]);
            if order.is_ne() {
                return order;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn comparison(order: std::cmp::Ordering) -> CFIndex {
    order as CFIndex
}

/// # Safety
///
/// `a` is a string; `b` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCompare(a: *const c_void, b: *const c_void, flags: usize) -> CFIndex {
    if b.is_null() {
        return 1;
    }
    // SAFETY: per this function's contract.
    let (a, b) = unsafe { (text(object(a)), text(object(b))) };
    comparison(compare(&a, &b, flags))
}

/// # Safety
///
/// As [`CFStringCompare`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCompareWithOptions(
    a: *const c_void,
    b: *const c_void,
    range: CFRange,
    flags: usize,
) -> CFIndex {
    // SAFETY: per this function's contract.
    let (a_string, b) = unsafe { (object(a), text(object(b))) };
    let a = range_units(&text(a_string), length_of(a_string), range).map(Units::into_string).unwrap_or_default();
    comparison(compare(&a, &b, flags))
}

/// # Safety
///
/// Both are strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringHasPrefix(string: *const c_void, prefix: *const c_void) -> Boolean {
    // SAFETY: per this function's contract.
    let (s, p) = unsafe { (text(object(string)), text(object(prefix))) };
    u8::from(s.starts_with(&*p))
}

/// # Safety
///
/// Both are strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringHasSuffix(string: *const c_void, suffix: *const c_void) -> Boolean {
    // SAFETY: per this function's contract.
    let (s, p) = unsafe { (text(object(string)), text(object(suffix))) };
    u8::from(s.ends_with(&*p))
}

/// # Safety
///
/// Both are strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringFind(string: *const c_void, find: *const c_void, flags: usize) -> CFRange {
    // SAFETY: per this function's contract.
    let (s, f) = unsafe { (text(object(string)), text(object(find))) };
    let (s, f): (Vec<u16>, Vec<u16>) = if flags & CASE_INSENSITIVE != 0 {
        (s.to_lowercase().encode_utf16().collect(), f.to_lowercase().encode_utf16().collect())
    } else {
        (s.encode_utf16().collect(), f.encode_utf16().collect())
    };
    let backwards = flags & 4 != 0;
    let found = if f.is_empty() || f.len() > s.len() {
        None
    } else if backwards {
        s.windows(f.len()).rposition(|w| w == f.as_slice())
    } else {
        s.windows(f.len()).position(|w| w == f.as_slice())
    };
    match found {
        Some(at) => CFRange { location: at as CFIndex, length: f.len() as CFIndex },
        None => CFRange { location: NOT_FOUND, length: 0 },
    }
}

/// # Safety
///
/// `string` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetIntValue(string: *const c_void) -> i32 {
    // SAFETY: per this function's contract.
    let text = text(unsafe { object(string) });
    let t = text.trim_start();
    let end = t
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))))
        .map_or(t.len(), |(i, _)| i);
    t[..end].parse::<i64>().map_or(0, |v| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
}

/// # Safety
///
/// `string` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetDoubleValue(string: *const c_void) -> f64 {
    // SAFETY: per this function's contract.
    let text = text(unsafe { object(string) });
    let t = text.trim();
    (1..=t.len()).rev().filter(|&n| t.is_char_boundary(n)).find_map(|n| t[..n].parse::<f64>().ok()).unwrap_or(0.0)
}

/// `CFSTR`: an immortal string for a C string literal, the same object for
/// the same text.
///
/// # Safety
///
/// `text` is a NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn __CFStringMakeConstantString(text: *const c_char) -> *const c_void {
    static CONSTANTS: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    // SAFETY: per this function's contract.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned();
    let mut constants = crate::thread::lock(CONSTANTS.get_or_init(Default::default));
    let ptr = *constants.entry(text).or_insert_with_key(|text| Retained::into_raw(NSString::from_str(text)) as usize);
    ptr as *const c_void
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        assert_eq!(decode(b"caf\xe9", encoding::ISO_LATIN1, false).as_deref(), Some("café"));
        assert_eq!(decode(b"caf\x8e", encoding::MAC_ROMAN, false).as_deref(), Some("café"));
        assert_eq!(decode(b"\xff\xfea\x00", encoding::UTF16, true).as_deref(), Some("a"));
        assert_eq!(decode(b"\x00a", encoding::UTF16_BE, false).as_deref(), Some("a"));
        assert!(decode(b"\xff", encoding::UTF8, false).is_none());
        let mut out = Vec::new();
        assert!(encode_char('é', encoding::MAC_ROMAN, &mut out));
        assert_eq!(out, [0x8e]);
        assert!(!encode_char('é', encoding::ASCII, &mut out));
    }

    #[test]
    fn ranges_of_units() {
        let range = |location, length| CFRange { location, length };
        let units = |text: &str, r| range_units(text, text.encode_utf16().count(), r).map(Iterator::collect::<Vec<_>>);
        assert_eq!(units("hello", range(1, 3)), Some(vec![b'e' as u16, b'l' as u16, b'l' as u16]));
        assert_eq!(units("hello", range(5, 0)), Some(vec![]));
        assert_eq!(units("hello", range(4, 2)), None);
        assert_eq!(units("hello", range(-1, 1)), None);
        let text = "h\u{e9}\u{1f600}!";
        assert_eq!(units(text, range(1, 3)), Some(vec![0xe9, 0xd83d, 0xde00]));
        assert_eq!(units(text, range(3, 2)), Some(vec![0xde00, b'!' as u16]));
        assert_eq!(units(text, range(4, 2)), None);
        assert_eq!(range_units(text, 5, range(2, 2)).unwrap().into_string(), "\u{1f600}");
        assert_eq!(range_units(text, 5, range(3, 1)).unwrap().into_string(), "\u{fffd}");

        // Offsets past whole 64-byte chunks.
        let long = "a\u{e9}\u{1f600}\u{4e2d}".repeat(50);
        let expected: Vec<u16> = long.encode_utf16().collect();
        for start in 0..expected.len() {
            let got: Vec<u16> =
                range_units(&long, expected.len(), range(start as isize, 3.min(expected.len() - start) as isize))
                    .unwrap()
                    .collect();
            assert_eq!(got, expected[start..(start + 3).min(expected.len())], "from {start}");
        }
        assert_eq!(unit_offset(&long, expected.len()), (long.len(), 0));
    }

    #[test]
    fn comparisons() {
        use std::cmp::Ordering::*;
        assert_eq!(compare("a", "B", 0), Greater);
        assert_eq!(compare("a", "B", CASE_INSENSITIVE), Less);
        assert_eq!(compare("file10", "file9", 0), Less);
        assert_eq!(compare("file10", "file9", NUMERICALLY), Greater);
        assert_eq!(compare("abc", "abc", 0), Equal);
    }
}
