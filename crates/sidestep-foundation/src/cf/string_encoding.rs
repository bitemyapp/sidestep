//! `CFString`'s encodings: their names, their `NSStringEncoding`, IANA and
//! Windows code page equivalents, and the representations made with them
//! (external ones, with a byte-order mark; the file system's, UTF-8; and
//! Pascal strings, a length byte then the bytes). The values are macOS's,
//! measured there (`conformance/tests/cf_strings.rs`).
//!
//! Sidestep encodes and decodes the eleven encodings `super::string`
//! lists; `CFStringGetListOfAvailableEncodings` lists those, and names,
//! code pages and IANA names are known for those. The conversions between
//! CoreFoundation's and Foundation's encoding numbers cover the encodings
//! Foundation names too, since they are only numbers.

use std::ffi::{CStr, c_char, c_void};

use objc2::rc::Retained;
use objc2_foundation::{NSData, NSString};

use super::string::{CFRange, decode, encoding, text};
use super::types::{object, owned};

type CFIndex = isize;
type CFStringEncoding = u32;
type Boolean = u8;

/// `kCFStringEncodingInvalidId`.
const INVALID: CFStringEncoding = 0xffff_ffff;

/// An encoding Sidestep has: its number, `NSStringEncoding`, name, IANA
/// name, Windows code page (0 for none) and most compatible Mac encoding.
struct Known {
    cf: CFStringEncoding,
    ns: u64,
    name: &'static str,
    iana: &'static str,
    code_page: u32,
    mac: CFStringEncoding,
}

const UNICODE: CFStringEncoding = encoding::UTF16;

const KNOWN: [Known; 11] = [
    Known {
        cf: encoding::MAC_ROMAN,
        ns: 30,
        name: "Western (Mac OS Roman)",
        iana: "macintosh",
        code_page: 10000,
        mac: encoding::MAC_ROMAN,
    },
    Known {
        cf: encoding::WINDOWS_LATIN1,
        ns: 12,
        name: "Western (Windows Latin 1)",
        iana: "windows-1252",
        code_page: 1252,
        mac: encoding::MAC_ROMAN,
    },
    Known {
        cf: encoding::ISO_LATIN1,
        ns: 5,
        name: "Western (ISO Latin 1)",
        iana: "iso-8859-1",
        code_page: 28591,
        mac: encoding::MAC_ROMAN,
    },
    Known {
        cf: encoding::ASCII,
        ns: 1,
        name: "Western (ASCII)",
        iana: "us-ascii",
        code_page: 20127,
        mac: encoding::MAC_ROMAN,
    },
    Known { cf: encoding::UTF8, ns: 4, name: "Unicode (UTF-8)", iana: "utf-8", code_page: 65001, mac: UNICODE },
    Known { cf: encoding::UTF16, ns: 10, name: "Unicode (UTF-16)", iana: "utf-16", code_page: 1200, mac: UNICODE },
    Known {
        cf: encoding::UTF16_BE,
        ns: 0x9000_0100,
        name: "Unicode (UTF-16BE)",
        iana: "utf-16be",
        code_page: 1201,
        mac: UNICODE,
    },
    Known {
        cf: encoding::UTF16_LE,
        ns: 0x9400_0100,
        name: "Unicode (UTF-16LE)",
        iana: "utf-16le",
        code_page: 0,
        mac: UNICODE,
    },
    Known {
        cf: encoding::UTF32,
        ns: 0x8c00_0100,
        name: "Unicode (UTF-32)",
        iana: "utf-32",
        code_page: 65005,
        mac: UNICODE,
    },
    Known {
        cf: encoding::UTF32_BE,
        ns: 0x9800_0100,
        name: "Unicode (UTF-32BE)",
        iana: "utf-32be",
        code_page: 65006,
        mac: UNICODE,
    },
    Known {
        cf: encoding::UTF32_LE,
        ns: 0x9c00_0100,
        name: "Unicode (UTF-32LE)",
        iana: "utf-32le",
        code_page: 0,
        mac: UNICODE,
    },
];

/// Foundation's other named encodings and CoreFoundation's numbers for
/// them (NEXTSTEP, Japanese EUC, Symbol, non-lossy ASCII, Shift JIS, ISO
/// Latin 2, Windows 1251, 1253, 1254 and 1250, ISO 2022-JP).
const NS_ONLY: [(u64, CFStringEncoding); 11] = [
    (2, 0x0b01),
    (3, 0x0920),
    (6, 0x0021),
    (7, 0x0bff),
    (8, 0x0420),
    (9, 0x0202),
    (11, 0x0502),
    (13, 0x0503),
    (14, 0x0504),
    (15, 0x0501),
    (21, 0x0820),
];

/// Other IANA names for the known encodings, besides their own and the
/// same with `-` and `_` left out.
const ALIASES: [(&str, CFStringEncoding); 20] = [
    ("ascii", encoding::ASCII),
    ("ansix3.41968", encoding::ASCII),
    ("iso646us", encoding::ASCII),
    ("us", encoding::ASCII),
    ("csascii", encoding::ASCII),
    ("latin1", encoding::ISO_LATIN1),
    ("l1", encoding::ISO_LATIN1),
    ("iso88591", encoding::ISO_LATIN1),
    ("iso885911987", encoding::ISO_LATIN1),
    ("isoir100", encoding::ISO_LATIN1),
    ("cp819", encoding::ISO_LATIN1),
    ("csisolatin1", encoding::ISO_LATIN1),
    ("cp1252", encoding::WINDOWS_LATIN1),
    ("xmacroman", encoding::MAC_ROMAN),
    ("mac", encoding::MAC_ROMAN),
    ("csmacintosh", encoding::MAC_ROMAN),
    ("unicode11utf8", encoding::UTF8),
    ("ucs2", encoding::UTF16),
    ("iso10646ucs2", encoding::UTF16),
    ("ucs4", encoding::UTF32),
];

fn known(cf: CFStringEncoding) -> Option<&'static Known> {
    KNOWN.iter().find(|k| k.cf == cf)
}

/// A name's letters and digits, lowercase, for comparing IANA names.
fn squeeze(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, '-' | '_' | ':')).flat_map(char::to_lowercase).collect()
}

/// The known encodings, then `kCFStringEncodingInvalidId`.
static AVAILABLE: [CFStringEncoding; 12] = [
    encoding::MAC_ROMAN,
    encoding::WINDOWS_LATIN1,
    encoding::ISO_LATIN1,
    encoding::ASCII,
    encoding::UTF8,
    encoding::UTF16,
    encoding::UTF16_BE,
    encoding::UTF16_LE,
    encoding::UTF32,
    encoding::UTF32_BE,
    encoding::UTF32_LE,
    INVALID,
];

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetListOfAvailableEncodings() -> *const CFStringEncoding {
    AVAILABLE.as_ptr()
}

/// A name that is a constant for the process's life, as CoreFoundation's
/// are (the Get functions hand them out without a reference).
fn constant(text: &'static str) -> *mut c_void {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashMap<&'static str, usize>>> = OnceLock::new();
    let mut names = crate::thread::lock(NAMES.get_or_init(Default::default));
    *names.entry(text).or_insert_with(|| Retained::into_raw(NSString::from_str(text)) as usize) as *mut c_void
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetNameOfEncoding(cf: CFStringEncoding) -> *mut c_void {
    known(cf).map_or(std::ptr::null_mut(), |k| constant(k.name))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringConvertEncodingToIANACharSetName(cf: CFStringEncoding) -> *mut c_void {
    known(cf).map_or(std::ptr::null_mut(), |k| constant(k.iana))
}

/// # Safety
///
/// `name` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringConvertIANACharSetNameToEncoding(name: *const c_void) -> CFStringEncoding {
    if name.is_null() {
        return INVALID;
    }
    // SAFETY: per this function's contract.
    let name = squeeze(&text(unsafe { object(name) }));
    KNOWN
        .iter()
        .find(|k| squeeze(k.iana) == name)
        .map(|k| k.cf)
        .or_else(|| ALIASES.iter().find(|(alias, _)| *alias == name).map(|&(_, cf)| cf))
        .unwrap_or(INVALID)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringConvertEncodingToNSStringEncoding(cf: CFStringEncoding) -> u64 {
    if let Some(k) = known(cf) {
        return k.ns;
    }
    if let Some(&(ns, _)) = NS_ONLY.iter().find(|(_, c)| *c == cf) {
        return ns;
    }
    if cf == INVALID { 0 } else { 0x8000_0000 | u64::from(cf) }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringConvertNSStringEncodingToEncoding(ns: u64) -> CFStringEncoding {
    if let Some(k) = KNOWN.iter().find(|k| k.ns == ns) {
        return k.cf;
    }
    if let Some(&(_, cf)) = NS_ONLY.iter().find(|(n, _)| *n == ns) {
        return cf;
    }
    if ns & 0x8000_0000 != 0 && ns <= 0xffff_ffff { (ns & 0x7fff_ffff) as CFStringEncoding } else { INVALID }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringConvertEncodingToWindowsCodepage(cf: CFStringEncoding) -> u32 {
    known(cf).map(|k| k.code_page).filter(|&p| p != 0).unwrap_or(INVALID)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringConvertWindowsCodepageToEncoding(code_page: u32) -> CFStringEncoding {
    KNOWN.iter().find(|k| k.code_page == code_page && code_page != 0).map_or(INVALID, |k| k.cf)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetMostCompatibleMacStringEncoding(cf: CFStringEncoding) -> CFStringEncoding {
    known(cf).map_or(INVALID, |k| k.mac)
}

/// ASCII for ASCII text, Mac Roman for text it holds, else UTF-16.
///
/// # Safety
///
/// `cf` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetSmallestEncoding(cf: *const c_void) -> CFStringEncoding {
    // SAFETY: per this function's contract.
    let text = text(unsafe { object(cf) });
    if text.is_ascii() {
        return encoding::ASCII;
    }
    let mut sink = Vec::new();
    if text.chars().all(|c| super::string::encode_char(c, encoding::MAC_ROMAN, &mut sink)) {
        encoding::MAC_ROMAN
    } else {
        UNICODE
    }
}

/// The string's bytes in `encoding`, with a byte-order mark for UTF-16 and
/// UTF-32; characters the encoding lacks become `loss_byte`, or fail the
/// conversion if it is 0.
///
/// # Safety
///
/// `cf` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateExternalRepresentation(
    _alloc: *const c_void,
    cf: *const c_void,
    encoding: CFStringEncoding,
    loss_byte: u8,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let length = unsafe { super::string::CFStringGetLength(cf) };
    let range = CFRange { location: 0, length };
    let mut size: CFIndex = 0;
    // SAFETY: as above; a first pass measures.
    let converted = unsafe {
        super::string::CFStringGetBytes(cf, range, encoding, loss_byte, 1, std::ptr::null_mut(), 0, &mut size)
    };
    if converted < length {
        return std::ptr::null_mut();
    }
    let mut bytes = vec![0u8; size.max(0) as usize];
    // SAFETY: room for `size` bytes.
    unsafe { super::string::CFStringGetBytes(cf, range, encoding, loss_byte, 1, bytes.as_mut_ptr(), size, &mut size) };
    owned(NSData::with_bytes(&bytes))
}

/// # Safety
///
/// `data` is null or a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateFromExternalRepresentation(
    _alloc: *const c_void,
    data: *const c_void,
    encoding: CFStringEncoding,
) -> *mut c_void {
    if data.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract; nothing changes the data while
    // it is read.
    let bytes = unsafe { crate::data::bytes(&*data.cast::<NSData>()) };
    decode(bytes, encoding, true).map_or(std::ptr::null_mut(), |t| owned(NSString::from_str(&t)))
}

/// # Safety
///
/// `path` is a NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithFileSystemRepresentation(
    _alloc: *const c_void,
    path: *const c_char,
) -> *mut c_void {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let bytes = unsafe { CStr::from_ptr(path) }.to_bytes();
    std::str::from_utf8(bytes).map_or(std::ptr::null_mut(), |t| owned(NSString::from_str(t)))
}

/// The path's bytes, UTF-8 (Linux file systems take names as they are),
/// and a NUL; false if they don't fit.
///
/// # Safety
///
/// `cf` is a string; `buffer` has room for `max` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetFileSystemRepresentation(
    cf: *const c_void,
    buffer: *mut c_char,
    max: CFIndex,
) -> Boolean {
    // SAFETY: per this function's contract.
    let text = text(unsafe { object(cf) });
    if text.contains('\0') || text.len() + 1 > max.max(0) as usize {
        return 0;
    }
    // SAFETY: the buffer holds `max` bytes, more than these.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer.cast::<u8>(), text.len());
        buffer.add(text.len()).write(0);
    }
    1
}

/// # Safety
///
/// `cf` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetMaximumSizeOfFileSystemRepresentation(cf: *const c_void) -> CFIndex {
    // SAFETY: per this function's contract.
    let length = unsafe { super::string::CFStringGetLength(cf) };
    super::string::CFStringGetMaximumSizeForEncoding(length, encoding::UTF8) + 1
}

/// # Safety
///
/// `pascal` is a length byte and that many bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithPascalString(
    _alloc: *const c_void,
    pascal: *const u8,
    encoding: CFStringEncoding,
) -> *mut c_void {
    if pascal.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let bytes = unsafe { std::slice::from_raw_parts(pascal.add(1), usize::from(*pascal)) };
    decode(bytes, encoding, false).map_or(std::ptr::null_mut(), |t| owned(NSString::from_str(&t)))
}

/// # Safety
///
/// As [`CFStringCreateWithPascalString`]; the bytes were allocated with
/// `malloc` unless `deallocator` is `kCFAllocatorNull`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringCreateWithPascalStringNoCopy(
    alloc: *const c_void,
    pascal: *const u8,
    encoding: CFStringEncoding,
    deallocator: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract; the text is copied, so the
    // buffer can go at once.
    unsafe {
        let string = CFStringCreateWithPascalString(alloc, pascal, encoding);
        if !pascal.is_null() {
            super::string::free_contents(pascal.cast(), deallocator);
        }
        string
    }
}

/// # Safety
///
/// `cf` is a string; `buffer` has room for `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFStringGetPascalString(
    cf: *const c_void,
    buffer: *mut u8,
    size: CFIndex,
    encoding: CFStringEncoding,
) -> Boolean {
    // SAFETY: per this function's contract.
    let text = text(unsafe { object(cf) });
    let mut bytes = Vec::with_capacity(text.len());
    if !text.chars().all(|c| super::string::encode_char(c, encoding, &mut bytes)) {
        return 0;
    }
    if bytes.len() > 255 || bytes.len() + 1 > size.max(0) as usize {
        return 0;
    }
    // SAFETY: the buffer holds `size` bytes, more than these.
    unsafe {
        buffer.write(bytes.len() as u8);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.add(1), bytes.len());
    }
    1
}

/// NULL, which CoreFoundation allows: callers copy with
/// `CFStringGetPascalString` instead.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetPascalStringPtr(_cf: *const c_void, _encoding: CFStringEncoding) -> *const u8 {
    std::ptr::null()
}

/// Whether hyphenation is available for a locale: Sidestep has no
/// hyphenation dictionaries, so it isn't, as macOS answers for a locale it
/// has none for.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringIsHyphenationAvailableForLocale(_locale: *const c_void) -> Boolean {
    0
}

/// `kCFNotFound`: no hyphenation point, as for a locale without
/// hyphenation.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFStringGetHyphenationLocationBeforeIndex(
    _cf: *const c_void,
    _location: CFIndex,
    _limit: CFRange,
    _options: usize,
    _locale: *const c_void,
    _character: *mut u32,
) -> CFIndex {
    -1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_trip() {
        for k in &KNOWN {
            assert_eq!(
                CFStringConvertNSStringEncodingToEncoding(CFStringConvertEncodingToNSStringEncoding(k.cf)),
                k.cf
            );
            if k.code_page != 0 {
                assert_eq!(CFStringConvertWindowsCodepageToEncoding(k.code_page), k.cf);
            }
        }
        assert_eq!(CFStringConvertEncodingToNSStringEncoding(0x0a01), 0x8000_0a01);
        assert_eq!(CFStringConvertNSStringEncodingToEncoding(0x8000_0a01), 0x0a01);
        assert_eq!(CFStringConvertNSStringEncodingToEncoding(1000), INVALID);
        assert_eq!(squeeze("ISO_8859-1"), "iso88591");
    }
}
