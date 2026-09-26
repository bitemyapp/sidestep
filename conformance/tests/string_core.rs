//! NSString and NSMutableString storage: the UTF-16 model, lone surrogates,
//! creation in every supported encoding, byte lengths and conversions,
//! equality and hashing, substrings, mutation, and the NSRange functions.
//! Expected values are what macOS returns.

use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSCopying, NSIntersectionRange, NSMutableCopying, NSMutableString, NSRange, NSRangeFromString, NSString,
    NSStringEncodingConversionOptions, NSUnionRange, ns_string,
};

use sidestep as _;

const ASCII: usize = 1;
const UTF8: usize = 4;
const LATIN1: usize = 5;
const WINDOWS_1252: usize = 12;
const UTF16: usize = 10;
const MAC_ROMAN: usize = 30;
const UTF16_BE: usize = 0x9000_0100;
const UTF16_LE: usize = 0x9400_0100;
const UTF32: usize = 0x8c00_0100;
const UTF32_BE: usize = 0x9800_0100;
const UTF32_LE: usize = 0x9c00_0100;

/// `-initWithBytes:length:encoding:`. objc2's own helpers send the encoding
/// as `i32` on GNUstep, and Sidestep registers the method that way (see
/// docs/abi.md), so the test does too.
fn from_bytes(bytes: &[u8], encoding: usize) -> Option<Retained<NSString>> {
    let ptr: *const c_void = bytes.as_ptr().cast();
    unsafe { msg_send![NSString::alloc(), initWithBytes: ptr, length: bytes.len(), encoding: arg(encoding)] }
}

#[cfg(target_vendor = "apple")]
fn arg(encoding: usize) -> usize {
    encoding
}

#[cfg(not(target_vendor = "apple"))]
fn arg(encoding: usize) -> i32 {
    encoding as i32
}

/// `-lengthOfBytesUsingEncoding:`, with the encoding width as above.
fn byte_len(s: &NSString, encoding: usize) -> usize {
    unsafe { msg_send![s, lengthOfBytesUsingEncoding: arg(encoding)] }
}

fn units(s: &NSString) -> Vec<u16> {
    (0..s.length()).map(|i| s.characterAtIndex(i)).collect()
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn from_units(u: &[u16]) -> Retained<NSString> {
    unsafe {
        NSString::initWithCharacters_length(NSString::alloc(), NonNull::new(u.as_ptr().cast_mut()).unwrap(), u.len())
    }
}

/// The emoji's high surrogate alone, as `substringToIndex:1` leaves it.
fn lone_high() -> Retained<NSString> {
    NSString::from_str("🎉").substringToIndex(1)
}

#[test]
fn utf16_model() {
    for text in ["", "hello", "héllo", "漢字かな", "a🎉b😀", "e\u{301}\u{302}", "\u{1F1EB}\u{1F1F7}"] {
        let s = NSString::from_str(text);
        let expected = utf16(text);
        assert_eq!(s.length(), expected.len(), "{text:?}");
        assert_eq!(units(&s), expected, "{text:?}");
        // getCharacters:range: from every start, including inside a pair.
        for loc in 0..expected.len() {
            let mut buf = vec![0u16; expected.len() - loc];
            if !buf.is_empty() {
                unsafe { s.getCharacters_range(NonNull::new(buf.as_mut_ptr()).unwrap(), NSRange::new(loc, buf.len())) };
            }
            assert_eq!(buf, expected[loc..], "{text:?} from {loc}");
        }
    }
}

#[test]
fn walking_long_mixed_text() {
    let text = "Grüße, 世界! 🎉 ".repeat(200);
    let s = NSString::from_str(&text);
    let expected = utf16(&text);
    // Forward, backward and strided, as a text view reads.
    for (i, &unit) in expected.iter().enumerate() {
        assert_eq!(s.characterAtIndex(i), unit);
    }
    for i in (0..expected.len()).rev() {
        assert_eq!(s.characterAtIndex(i), expected[i]);
    }
    for k in 0..expected.len() {
        let i = (k * 7919) % expected.len();
        assert_eq!(s.characterAtIndex(i), expected[i]);
    }
}

#[test]
fn lone_surrogates() {
    let s = NSString::from_str("🎉");
    let (high, low) = (s.substringToIndex(1), s.substringFromIndex(1));
    assert_eq!((high.length(), units(&high)), (1, vec![0xD83C]));
    assert_eq!((low.length(), units(&low)), (1, vec![0xDF89]));
    // The same unit built directly is equal, and hashes equally.
    let built = from_units(&[0xD83C]);
    assert!(high.isEqualToString(&built));
    assert_eq!(high.hash(), built.hash());
    // It has no UTF-8 form.
    assert!(high.UTF8String().is_null());
    assert_eq!(byte_len(&high, UTF8), 0);
    assert_eq!(high.maximumLengthOfBytesUsingEncoding(UTF8), 3);
    assert!(high.cStringUsingEncoding(UTF8).is_null());
    assert_eq!(byte_len(&high, UTF16), 2);
    assert!(!high.cStringUsingEncoding(UTF16).is_null());
    assert!(high.canBeConvertedToEncoding(UTF8));
    assert!(high.canBeConvertedToEncoding(UTF16));
    assert!(!high.canBeConvertedToEncoding(UTF32));
    assert!(!high.canBeConvertedToEncoding(ASCII));
    // The halves join back into the character.
    let joined = high.stringByAppendingString(&low);
    assert_eq!(joined.length(), 2);
    assert!(joined.isEqualToString(&s));
    assert_eq!(joined.hash(), s.hash());
    assert_eq!(joined.to_string(), "🎉");
    // Unpaired surrogates in the middle of text are kept.
    let mixed = from_units(&[0x61, 0xD800, 0x62]);
    assert_eq!(units(&mixed), [0x61, 0xD800, 0x62]);
    assert!(mixed.UTF8String().is_null());
    assert_eq!(byte_len(&mixed, UTF32), 0);
}

#[test]
fn creation_from_utf8() {
    assert!(from_bytes(&[0x61, 0xFF], UTF8).is_none(), "invalid UTF-8");
    assert!(from_bytes(&[0x61, 0xC3], UTF8).is_none(), "truncated sequence");
    assert_eq!(units(&from_bytes(b"a\0b", UTF8).unwrap()), [0x61, 0, 0x62]);
    assert_eq!(from_bytes(b"", UTF8).unwrap().length(), 0);
    // A leading byte order mark is dropped, including from from_str; one
    // anywhere else is kept.
    assert_eq!(units(&from_bytes(&[0xEF, 0xBB, 0xBF, 0x61], UTF8).unwrap()), [0x61]);
    assert_eq!(units(&from_bytes(&[0xEF, 0xBB, 0xBF, 0xEF, 0xBB, 0xBF], UTF8).unwrap()), [0xFEFF]);
    assert_eq!(NSString::from_str("\u{FEFF}a").length(), 1);
    assert_eq!(NSString::from_str("x\u{FEFF}a").length(), 3);
    assert_eq!(NSMutableString::from_str("\u{FEFF}a").length(), 1);
    // Unsupported encodings fail.
    assert!(from_bytes(b"abc", 999).is_none());
}

#[test]
fn creation_from_single_byte_encodings() {
    // ASCII accepts every byte, reading the upper half as Latin 1.
    assert_eq!(units(&from_bytes(&[0x61, 0x80, 0xFF], ASCII).unwrap()), [0x61, 0x80, 0xFF]);
    assert_eq!(units(&from_bytes(&[0x61, 0xE9, 0x80, 0xFF], LATIN1).unwrap()), [0x61, 0xE9, 0x80, 0xFF]);
    let roman = from_bytes(&(0x80..=0xFF).collect::<Vec<u8>>(), MAC_ROMAN).unwrap();
    assert_eq!(
        units(&roman),
        [
            196, 197, 199, 201, 209, 214, 220, 225, 224, 226, 228, 227, 229, 231, 233, 232, 234, 235, 237, 236, 238,
            239, 241, 243, 242, 244, 246, 245, 250, 249, 251, 252, 8224, 176, 162, 163, 167, 8226, 182, 223, 174, 169,
            8482, 180, 168, 8800, 198, 216, 8734, 177, 8804, 8805, 165, 181, 8706, 8721, 8719, 960, 8747, 170, 186,
            937, 230, 248, 191, 161, 172, 8730, 402, 8776, 8710, 171, 187, 8230, 160, 192, 195, 213, 338, 339, 8211,
            8212, 8220, 8221, 8216, 8217, 247, 9674, 255, 376, 8260, 8364, 8249, 8250, 64257, 64258, 8225, 183, 8218,
            8222, 8240, 194, 202, 193, 203, 200, 205, 206, 207, 204, 211, 212, 63743, 210, 218, 219, 217, 305, 710,
            732, 175, 728, 729, 730, 184, 733, 731, 711
        ]
    );
    let defined = [
        0x80, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8e, 0x91, 0x92, 0x93, 0x94, 0x95,
        0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b, 0x9c, 0x9e, 0x9f,
    ];
    assert_eq!(
        units(&from_bytes(&defined, WINDOWS_1252).unwrap()),
        [
            8364, 8218, 402, 8222, 8230, 8224, 8225, 710, 8240, 352, 8249, 338, 381, 8216, 8217, 8220, 8221, 8226,
            8211, 8212, 732, 8482, 353, 8250, 339, 382, 376
        ]
    );
    assert_eq!(units(&from_bytes(&[0xE9, 0xFF], WINDOWS_1252).unwrap()), [0xE9, 0xFF]);
    for undefined in [0x81u8, 0x8D, 0x8F, 0x90, 0x9D] {
        assert!(from_bytes(&[0x61, undefined], WINDOWS_1252).is_none(), "{undefined:#x}");
    }
}

#[test]
fn creation_from_utf16_and_utf32() {
    let u16s = |b: &[u8], e| from_bytes(b, e).map(|s| units(&s));
    // Unmarked UTF-16 is big-endian; a byte order mark decides and is
    // dropped; a trailing odd byte is ignored.
    assert_eq!(u16s(&[0x00, 0x61, 0x00, 0x62], UTF16), Some(vec![0x61, 0x62]));
    assert_eq!(u16s(&[0x61, 0x00, 0x62, 0x00], UTF16), Some(vec![0x6100, 0x6200]));
    assert_eq!(u16s(&[0xFE, 0xFF, 0x00, 0x61], UTF16), Some(vec![0x61]));
    assert_eq!(u16s(&[0xFF, 0xFE, 0x61, 0x00], UTF16), Some(vec![0x61]));
    assert_eq!(u16s(&[0xFE, 0xFF], UTF16), Some(vec![]));
    assert_eq!(u16s(&[0xFF, 0xFE, 0xFF, 0xFE], UTF16), Some(vec![0xFEFF]));
    assert_eq!(u16s(&[0x61, 0x00, 0x62], UTF16), Some(vec![0x6100]));
    // The explicit byte orders keep a leading U+FEFF as a character.
    assert_eq!(u16s(&[0xFE, 0xFF, 0x00, 0x61], UTF16_BE), Some(vec![0xFEFF, 0x61]));
    assert_eq!(u16s(&[0xFF, 0xFE, 0x61, 0x00], UTF16_LE), Some(vec![0xFEFF, 0x61]));
    // Unpaired surrogates are accepted.
    assert_eq!(u16s(&[0x3C, 0xD8, 0x61, 0x00], UTF16_LE), Some(vec![0xD83C, 0x61]));
    assert_eq!(u16s(&[0xFF, 0xFE, 0x3C, 0xD8], UTF16), Some(vec![0xD83C]));
    // UTF-32: the same rules, except invalid scalars fail.
    assert_eq!(u16s(&[0, 0, 0, 0x61], UTF32), Some(vec![0x61]));
    assert_eq!(u16s(&[0x61, 0, 0, 0], UTF32), None);
    assert_eq!(u16s(&[0, 0, 0xFE, 0xFF, 0, 0, 0, 0x61], UTF32), Some(vec![0x61]));
    assert_eq!(u16s(&[0xFF, 0xFE, 0, 0, 0x61, 0, 0, 0], UTF32), Some(vec![0x61]));
    assert_eq!(u16s(&[0, 0, 0xFE, 0xFF], UTF32), Some(vec![]));
    assert_eq!(u16s(&[0, 0, 0, 0x61, 0], UTF32), Some(vec![0x61]));
    assert_eq!(u16s(&[0, 0, 0xFE, 0xFF, 0, 0, 0, 0x61], UTF32_BE), Some(vec![0xFEFF, 0x61]));
    assert_eq!(u16s(&[0x89, 0xF3, 0x01, 0], UTF32_LE), Some(vec![0xD83C, 0xDF89]));
    assert_eq!(u16s(&[0, 0, 0x11, 0], UTF32_LE), None);
    assert_eq!(u16s(&[0x3C, 0xD8, 0, 0], UTF32_LE), None);
}

#[test]
fn other_initializers() {
    let chars = utf16("a🎉é");
    let s = from_units(&chars);
    assert_eq!(s.to_string(), "a🎉é");
    let s = unsafe {
        NSString::initWithCharactersNoCopy_length_freeWhenDone(
            NSString::alloc(),
            NonNull::new(chars.as_ptr().cast_mut()).unwrap(),
            chars.len(),
            false,
        )
    };
    assert_eq!(s.to_string(), "a🎉é");
    let utf8 = c"h\xc3\xa9llo";
    let s = unsafe { NSString::initWithUTF8String(NSString::alloc(), NonNull::new(utf8.as_ptr().cast_mut()).unwrap()) };
    assert_eq!(s.unwrap().to_string(), "héllo");
    let bad = c"\xff";
    assert!(
        unsafe { NSString::initWithUTF8String(NSString::alloc(), NonNull::new(bad.as_ptr().cast_mut()).unwrap()) }
            .is_none()
    );
    let latin = c"caf\xe9";
    let s = unsafe {
        NSString::initWithCString_encoding(NSString::alloc(), NonNull::new(latin.as_ptr().cast_mut()).unwrap(), LATIN1)
    };
    assert_eq!(s.unwrap().to_string(), "café");
    let bytes = *b"no copy";
    let s = unsafe {
        NSString::initWithBytesNoCopy_length_encoding_freeWhenDone(
            NSString::alloc(),
            NonNull::new(bytes.as_ptr().cast_mut().cast()).unwrap(),
            bytes.len(),
            ASCII,
            false,
        )
    };
    assert_eq!(s.unwrap().to_string(), "no copy");
    assert_eq!(NSString::new().length(), 0);
    assert_eq!(NSString::string().length(), 0);
    assert_eq!(NSString::stringWithString(ns_string!("same")).to_string(), "same");
    let s = unsafe { NSString::stringWithUTF8String(NonNull::new(utf8.as_ptr().cast_mut()).unwrap()) };
    assert_eq!(s.unwrap().to_string(), "héllo");
    let s =
        unsafe { NSString::stringWithCharacters_length(NonNull::new(chars.as_ptr().cast_mut()).unwrap(), chars.len()) };
    assert_eq!(s.to_string(), "a🎉é");
    let s = unsafe { NSString::stringWithCString_encoding(NonNull::new(latin.as_ptr().cast_mut()).unwrap(), LATIN1) };
    assert_eq!(s.unwrap().to_string(), "café");
    let s = NSString::initWithString(NSString::alloc(), &NSMutableString::from_str("from mutable"));
    assert_eq!(s.to_string(), "from mutable");
}

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

/// A malloc'ed copy of `bytes`, as the NoCopy initializers take.
fn malloced(bytes: &[u8]) -> NonNull<c_void> {
    let p = NonNull::new(unsafe { malloc(bytes.len()) }).expect("malloc");
    unsafe { p.as_ptr().cast::<u8>().copy_from_nonoverlapping(bytes.as_ptr(), bytes.len()) };
    p
}

#[test]
fn no_copy_initializers_free_only_what_they_keep() {
    // A failed initializer leaves the buffer to its caller even with
    // freeWhenDone:YES, so the caller frees it (a double free otherwise).
    let bad = malloced(b"ok \xff");
    let s =
        unsafe { NSString::initWithBytesNoCopy_length_encoding_freeWhenDone(NSString::alloc(), bad, 4, UTF8, true) };
    assert!(s.is_none());
    unsafe { free(bad.as_ptr()) };
    let bad = malloced(b"ok \xff");
    let s = unsafe {
        NSMutableString::initWithBytesNoCopy_length_encoding_freeWhenDone(NSMutableString::alloc(), bad, 4, UTF8, true)
    };
    assert!(s.is_none());
    unsafe { free(bad.as_ptr()) };
    // A successful one takes the buffer over.
    let good = malloced(b"kept");
    let s =
        unsafe { NSString::initWithBytesNoCopy_length_encoding_freeWhenDone(NSString::alloc(), good, 4, UTF8, true) };
    assert_eq!(s.unwrap().to_string(), "kept");
    let good = malloced(b"kept");
    let s = unsafe {
        NSMutableString::initWithBytesNoCopy_length_encoding_freeWhenDone(NSMutableString::alloc(), good, 4, UTF8, true)
    };
    assert_eq!(s.unwrap().to_string(), "kept");
}

#[test]
fn class_methods_build_the_receiving_class() {
    let m = NSMutableString::stringWithString(ns_string!("x"));
    m.appendString(ns_string!("y"));
    assert_eq!(m.to_string(), "xy");
    let m = NSMutableString::string();
    m.appendString(ns_string!("q"));
    assert_eq!(m.to_string(), "q");
    let m = NSMutableString::stringWithCapacity(10);
    assert_eq!(m.length(), 0);
    m.appendString(ns_string!("grows"));
    assert_eq!(m.to_string(), "grows");
    let m = NSMutableString::initWithCapacity(NSMutableString::alloc(), 3);
    m.appendString(ns_string!("0123456789"));
    assert_eq!(m.length(), 10);
}

#[test]
fn byte_lengths_and_encodings() {
    struct Case {
        text: &'static str,
        len: [usize; 6],
        max: [usize; 5],
        can: [bool; 3],
        fastest: usize,
        smallest: usize,
    }
    // len: ASCII, UTF-8, UTF-16, Latin 1, UTF-32, Mac Roman.
    // max: ASCII, UTF-8, UTF-16, Latin 1, UTF-32.
    // can: ASCII, Latin 1, Mac Roman.
    let cases = [
        Case { text: "abc", len: [3, 3, 6, 3, 12, 3], max: [3, 9, 6, 3, 12], can: [true; 3], fastest: 1, smallest: 1 },
        Case {
            text: "é",
            len: [0, 2, 2, 1, 4, 1],
            max: [1, 3, 2, 1, 4],
            can: [false, true, true],
            fastest: 10,
            smallest: 30,
        },
        Case {
            text: "漢字",
            len: [0, 6, 4, 0, 8, 0],
            max: [2, 6, 4, 2, 8],
            can: [false; 3],
            fastest: 10,
            smallest: 10,
        },
        Case {
            text: "a🎉",
            len: [0, 5, 6, 0, 8, 0],
            max: [3, 9, 6, 3, 12],
            can: [false; 3],
            fastest: 10,
            smallest: 10,
        },
        Case { text: "", len: [0; 6], max: [0; 5], can: [true; 3], fastest: 1, smallest: 1 },
    ];
    for c in cases {
        let s = NSString::from_str(c.text);
        let len = [ASCII, UTF8, UTF16, LATIN1, UTF32, MAC_ROMAN].map(|e| byte_len(&s, e));
        assert_eq!(len, c.len, "{:?} lengthOfBytesUsingEncoding:", c.text);
        let max = [ASCII, UTF8, UTF16, LATIN1, UTF32].map(|e| s.maximumLengthOfBytesUsingEncoding(e));
        assert_eq!(max, c.max, "{:?} maximumLengthOfBytesUsingEncoding:", c.text);
        let can = [ASCII, LATIN1, MAC_ROMAN].map(|e| s.canBeConvertedToEncoding(e));
        assert_eq!(can, c.can, "{:?} canBeConvertedToEncoding:", c.text);
        assert_eq!(s.fastestEncoding(), c.fastest, "{:?}", c.text);
        assert_eq!(s.smallestEncoding(), c.smallest, "{:?}", c.text);
    }
    // Characters in Windows-1252 or Latin 1 but not Mac Roman.
    assert_eq!(NSString::from_str("€").smallestEncoding(), MAC_ROMAN);
    assert_eq!(NSString::from_str("Š").smallestEncoding(), UTF16);
    assert_eq!(byte_len(&NSString::from_str("™€ﬁ"), MAC_ROMAN), 3);
    assert_eq!(byte_len(&NSString::from_str("¤"), MAC_ROMAN), 0);
    assert_eq!(byte_len(&NSString::from_str("¤"), WINDOWS_1252), 1);
    assert_eq!(NSMutableString::from_str("abc").fastestEncoding(), ASCII);
}

#[test]
fn c_strings() {
    let s = NSString::from_str("héllo");
    let mut buf = [0x7f as c_char; 8];
    let ok = unsafe { s.getCString_maxLength_encoding(NonNull::new(buf.as_mut_ptr()).unwrap(), 7, UTF8) };
    assert!(ok);
    assert_eq!(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_bytes(), "héllo".as_bytes());
    // Too small: NO, and an empty C string.
    let mut buf = [0x7f as c_char; 8];
    let ok = unsafe { s.getCString_maxLength_encoding(NonNull::new(buf.as_mut_ptr()).unwrap(), 6, UTF8) };
    assert!(!ok);
    assert_eq!(buf[0], 0);
    let mut buf = [0x7f as c_char; 8];
    assert!(!unsafe { s.getCString_maxLength_encoding(NonNull::new(buf.as_mut_ptr()).unwrap(), 8, ASCII) });
    assert_eq!(buf[0], 0);
    let mut buf = [0x7f as c_char; 8];
    assert!(unsafe { s.getCString_maxLength_encoding(NonNull::new(buf.as_mut_ptr()).unwrap(), 8, LATIN1) });
    // SAFETY: c_char and u8 have the same size and alignment on every target.
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), 6) };
    assert_eq!(bytes, [0x68, 0xE9, 0x6C, 0x6C, 0x6F, 0]);
    // cStringUsingEncoding:
    let c = NSString::from_str("é").cStringUsingEncoding(ASCII);
    assert!(c.is_null());
    autoreleasepool(|_| {
        let c = NSString::from_str("café").cStringUsingEncoding(LATIN1);
        assert_eq!(unsafe { CStr::from_ptr(c) }.to_bytes(), b"caf\xe9");
        let c = NSString::from_str("a€b").cStringUsingEncoding(MAC_ROMAN);
        assert_eq!(unsafe { CStr::from_ptr(c) }.to_bytes(), b"a\xdbb");
        let c = NSMutableString::from_str("mutable").cStringUsingEncoding(UTF8);
        assert_eq!(unsafe { CStr::from_ptr(c) }.to_bytes(), b"mutable");
        let s = NSString::from_str("a\0b");
        let c = s.UTF8String();
        assert_eq!(unsafe { CStr::from_ptr(c) }.to_bytes(), b"a");
        assert_eq!(byte_len(&s, UTF8), 3);
        assert_eq!(unsafe { CStr::from_ptr(NSString::new().UTF8String()) }.to_bytes(), b"");
    });
}

/// `-getBytes:maxLength:usedLength:encoding:options:range:remainingRange:`.
fn get_bytes(s: &NSString, max: usize, encoding: usize, options: usize, range: NSRange) -> (bool, Vec<u8>, NSRange) {
    let mut buf = [0u8; 32];
    let mut used = 999;
    let mut rest = NSRange::new(99, 99);
    let ok = unsafe {
        s.getBytes_maxLength_usedLength_encoding_options_range_remainingRange(
            buf.as_mut_ptr().cast(),
            max,
            &mut used,
            encoding,
            NSStringEncodingConversionOptions(options),
            range,
            &mut rest,
        )
    };
    (ok, buf[..used].to_vec(), rest)
}

#[test]
fn get_bytes_converts_until_it_must_stop() {
    let s = NSString::from_str("aé🎉b");
    let all = NSRange::new(0, 5);
    // Stops before a character that doesn't fit.
    assert_eq!(get_bytes(&s, 4, UTF8, 0, all), (true, vec![0x61, 0xC3, 0xA9], NSRange::new(2, 3)));
    // Stops at a character the encoding lacks, unless lossy.
    assert_eq!(get_bytes(&s, 16, ASCII, 0, all), (true, vec![0x61], NSRange::new(1, 4)));
    let (ok, lossy, rest) = get_bytes(&s, 16, ASCII, 1, all);
    assert_eq!((ok, lossy.len(), lossy[0], lossy[3], rest), (true, 4, 0x61, 0x62, NSRange::new(5, 0)));
    assert_eq!(lossy[1], b'e', "é loses its accent");
    // UTF-16 can split a pair; UTF-8 can't encode half of one.
    assert_eq!(
        get_bytes(&s, 16, UTF16_LE, 0, NSRange::new(1, 2)),
        (true, vec![0xE9, 0, 0x3C, 0xD8], NSRange::new(3, 0))
    );
    assert_eq!(get_bytes(&s, 16, UTF8, 0, NSRange::new(2, 1)), (false, vec![], NSRange::new(2, 1)));
    let pair = NSString::from_str("a🎉");
    assert_eq!(get_bytes(&pair, 16, UTF32_LE, 0, NSRange::new(0, 2)), (true, vec![0x61, 0, 0, 0], NSRange::new(1, 1)));
    assert_eq!(
        get_bytes(&pair, 16, UTF32_LE, 0, NSRange::new(0, 3)),
        (true, vec![0x61, 0, 0, 0, 0x89, 0xF3, 0x01, 0], NSRange::new(3, 0))
    );
    // Nothing converted is NO, even for an empty range.
    let e = NSString::from_str("é");
    assert_eq!(get_bytes(&e, 16, ASCII, 0, NSRange::new(0, 1)), (false, vec![], NSRange::new(0, 1)));
    assert_eq!(get_bytes(&e, 16, UTF8, 0, NSRange::new(0, 0)), (false, vec![], NSRange::new(0, 0)));
    // Unmarked UTF-16 and UTF-32 are in host order, with a byte order mark
    // only for the external representation.
    let ab = NSString::from_str("ab");
    let host = |v: u16| if cfg!(target_endian = "little") { v.to_le_bytes() } else { v.to_be_bytes() };
    let mut expected = host(0x61).to_vec();
    expected.extend(host(0x62));
    assert_eq!(get_bytes(&ab, 16, UTF16, 0, NSRange::new(0, 2)).1, expected);
    let mut marked = host(0xFEFF).to_vec();
    marked.extend(&expected);
    assert_eq!(get_bytes(&ab, 16, UTF16, 2, NSRange::new(0, 2)).1, marked);
    assert_eq!(get_bytes(&ab, 16, UTF32, 2, NSRange::new(0, 2)).1.len(), 12);
    // Encodings with characters beyond Latin 1.
    let euro = NSString::from_str("a€b");
    assert_eq!(get_bytes(&euro, 16, MAC_ROMAN, 0, NSRange::new(0, 3)).1, [0x61, 0xDB, 0x62]);
    assert_eq!(get_bytes(&euro, 16, WINDOWS_1252, 0, NSRange::new(0, 3)).1, [0x61, 0x80, 0x62]);
    assert_eq!(get_bytes(&euro, 16, LATIN1, 1, NSRange::new(0, 3)).1, [0x61, b'?', 0x62]);
    // With no buffer, only the length is measured.
    let mut used = 0;
    let mut rest = NSRange::new(0, 0);
    let ok = unsafe {
        s.getBytes_maxLength_usedLength_encoding_options_range_remainingRange(
            std::ptr::null_mut(),
            3,
            &mut used,
            UTF8,
            NSStringEncodingConversionOptions(0),
            all,
            &mut rest,
        )
    };
    assert_eq!((ok, used, rest), (true, 8, NSRange::new(5, 0)));
}

#[test]
fn equality_is_literal_and_hashes_agree() {
    let precomposed = NSString::from_str("\u{e9}");
    let decomposed = NSString::from_str("e\u{301}");
    assert!(!precomposed.isEqualToString(&decomposed));
    assert!(!precomposed.isEqual(Some(&decomposed)));
    for text in ["", "same text", "héllo wörld", "🎉 party"] {
        let immutable = NSString::from_str(text);
        let mutable = NSMutableString::from_str(text);
        let built = from_units(&utf16(text));
        assert!(immutable.isEqualToString(&mutable));
        assert!(mutable.isEqualToString(&built));
        assert!(mutable.isEqual(Some(&immutable)));
        assert_eq!(immutable.hash(), mutable.hash());
        assert_eq!(immutable.hash(), built.hash());
    }
    let literal = ns_string!("a literal");
    assert_eq!(literal.hash(), NSString::from_str("a literal").hash());
    assert!(literal.isEqual(Some(&NSMutableString::from_str("a literal"))));
    assert!(!literal.isEqual(Some(&NSObject::new())));
    assert!(!literal.isEqual(None));
}

#[test]
fn substrings() {
    let s = NSString::from_str("héllo wörld");
    assert_eq!(s.substringFromIndex(6).to_string(), "wörld");
    assert_eq!(s.substringToIndex(5).to_string(), "héllo");
    assert_eq!(s.substringWithRange(NSRange::new(1, 3)).to_string(), "éll");
    assert_eq!(s.substringFromIndex(11).length(), 0);
    assert_eq!(s.substringToIndex(0).length(), 0);
    assert_eq!(s.substringWithRange(NSRange::new(0, 11)).to_string(), "héllo wörld");
    let e = NSString::from_str("a🎉b😀c");
    assert_eq!(units(&e.substringWithRange(NSRange::new(2, 3))), [0xDF89, 0x62, 0xD83D]);
    assert_eq!(e.substringFromIndex(4).to_string(), "😀c");
    assert_eq!(s.stringByAppendingString(ns_string!("!")).to_string(), "héllo wörld!");
    assert_eq!(s.stringByAppendingString(&NSString::new()).to_string(), "héllo wörld");
    assert_eq!(NSString::new().stringByAppendingString(&s).to_string(), "héllo wörld");
}

#[test]
fn mutable_strings() {
    let m = NSMutableString::from_str("0123456789");
    let n = m.replaceOccurrencesOfString_withString_options_range(
        ns_string!("1"),
        ns_string!("xx"),
        objc2_foundation::NSStringCompareOptions(0),
        NSRange::new(0, 10),
    );
    assert_eq!((n, m.to_string()), (1, "0xx23456789".into()));
    m.insertString_atIndex(ns_string!("S"), 0);
    m.insertString_atIndex(ns_string!("E"), m.length());
    m.deleteCharactersInRange(NSRange::new(1, 2));
    assert_eq!(m.to_string(), "Sx23456789E");
    m.replaceCharactersInRange_withString(NSRange::new(1, 1), ns_string!("é🎉"));
    assert_eq!(m.to_string(), "Sé🎉23456789E");
    assert_eq!(m.length(), 13);
    m.setString(ns_string!("reset"));
    assert_eq!(m.to_string(), "reset");
    // Appending the string to itself.
    let m = NSMutableString::from_str("ab");
    m.appendString(&m);
    assert_eq!(m.to_string(), "abab");
    m.insertString_atIndex(&m.copy(), 2);
    assert_eq!(m.to_string(), "abababab");
    // Deleting half a pair leaves the other half.
    let m = NSMutableString::from_str("🎉");
    m.deleteCharactersInRange(NSRange::new(0, 1));
    assert_eq!(units(&m), [0xDF89]);
    m.insertString_atIndex(&lone_high(), 0);
    assert_eq!(units(&m), [0xD83C, 0xDF89]);
    // Many small edits keep indexing right.
    let m = NSMutableString::new();
    let mut expected = String::new();
    for i in 0..300 {
        let piece = if i % 3 == 0 {
            "ü"
        } else if i % 3 == 1 {
            "a"
        } else {
            "🎉"
        };
        m.appendString(&NSString::from_str(piece));
        expected.push_str(piece);
        if i % 10 == 0 {
            let at = expected.encode_utf16().count() / 2;
            assert_eq!(m.characterAtIndex(at), utf16(&expected)[at]);
        }
    }
    assert_eq!(m.to_string(), expected);
    assert_eq!(units(&m), utf16(&expected));
}

#[test]
fn mutable_copies_and_pointers() {
    autoreleasepool(|pool| {
        let m = NSMutableString::from_str("hello");
        let p = unsafe { m.to_str(pool) };
        m.appendString(ns_string!(" world"));
        m.setString(ns_string!("something else entirely, and longer"));
        assert_eq!(p, "hello");
    });
    let m = NSMutableString::from_str("x");
    let copy = m.copy();
    m.appendString(ns_string!("y"));
    assert_eq!((copy.to_string(), m.to_string()), ("x".into(), "xy".into()));
    let constant = ns_string!("const").mutableCopy();
    constant.appendString(ns_string!("!"));
    assert_eq!(constant.to_string(), "const!");
    let immutable = NSString::from_str("im");
    let mutable = immutable.mutableCopy();
    mutable.appendString(ns_string!("mutable"));
    assert_eq!((immutable.to_string(), mutable.to_string()), ("im".into(), "immutable".into()));
    let desc: Retained<NSString> = unsafe { msg_send![&*mutable, description] };
    assert_eq!(desc.to_string(), "immutable");
}

struct Units(Vec<u16>);

define_class!(
    // An app's own string class that implements only the primitives.
    #[unsafe(super(NSString))]
    #[ivars = Units]
    struct PrimitiveString;

    impl PrimitiveString {
        #[unsafe(method(length))]
        fn length(&self) -> usize {
            self.ivars().0.len()
        }

        #[unsafe(method(characterAtIndex:))]
        fn character_at_index(&self, index: usize) -> u16 {
            self.ivars().0[index]
        }
    }
);

fn primitive(text: &str) -> Retained<PrimitiveString> {
    let this = PrimitiveString::alloc().set_ivars(Units(utf16(text)));
    unsafe { msg_send![super(this), init] }
}

#[test]
fn subclasses_with_only_primitives() {
    let p = primitive("héllo 🎉");
    let s: &NSString = &p;
    assert_eq!(s.length(), 8);
    assert!(s.isEqualToString(ns_string!("héllo 🎉")));
    assert!(ns_string!("héllo 🎉").isEqualToString(s));
    assert_eq!(s.hash(), NSString::from_str("héllo 🎉").hash());
    assert_eq!(s.to_string(), "héllo 🎉");
    assert_eq!(s.substringWithRange(NSRange::new(1, 4)).to_string(), "éllo");
    assert_eq!(s.stringByAppendingString(ns_string!("!")).to_string(), "héllo 🎉!");
    let mut buf = [0u16; 3];
    unsafe { s.getCharacters_range(NonNull::new(buf.as_mut_ptr()).unwrap(), NSRange::new(5, 3)) };
    assert_eq!(buf, [0x20, 0xD83C, 0xDF89]);
    let m = s.mutableCopy();
    m.appendString(ns_string!("?"));
    assert_eq!(m.to_string(), "héllo 🎉?");
    assert_eq!(s.copy().to_string(), "héllo 🎉");
}

#[test]
fn range_functions() {
    assert_eq!(NSUnionRange(NSRange::new(2, 3), NSRange::new(10, 2)), NSRange::new(2, 10));
    assert_eq!(NSUnionRange(NSRange::new(10, 2), NSRange::new(2, 3)), NSRange::new(2, 10));
    assert_eq!(NSIntersectionRange(NSRange::new(2, 5), NSRange::new(4, 10)), NSRange::new(4, 3));
    // Touching and disjoint ranges intersect in {0, 0}; an empty range
    // inside another keeps its location.
    assert_eq!(NSIntersectionRange(NSRange::new(2, 3), NSRange::new(5, 10)), NSRange::new(0, 0));
    assert_eq!(NSIntersectionRange(NSRange::new(2, 3), NSRange::new(8, 10)), NSRange::new(0, 0));
    assert_eq!(NSIntersectionRange(NSRange::new(8, 10), NSRange::new(2, 3)), NSRange::new(0, 0));
    assert_eq!(NSIntersectionRange(NSRange::new(4, 0), NSRange::new(2, 10)), NSRange::new(4, 0));
    assert_eq!(NSString::from_range(NSRange::new(3, 4)).to_string(), "{3, 4}");
    assert_eq!(NSString::from_range(NSRange::new(0, 0)).to_string(), "{0, 0}");
    let parse = |s: &str| NSRangeFromString(&NSString::from_str(s));
    assert_eq!(parse("{3, 4}"), NSRange::new(3, 4));
    assert_eq!(parse("{3,4}"), NSRange::new(3, 4));
    assert_eq!(parse("3 4"), NSRange::new(3, 4));
    assert_eq!(parse(" 12 , 5 "), NSRange::new(12, 5));
    assert_eq!(parse("7"), NSRange::new(7, 0));
    assert_eq!(parse(""), NSRange::new(0, 0));
    assert_eq!(parse("garbage"), NSRange::new(0, 0));
    assert_eq!(parse("x5y6"), NSRange::new(5, 6));
    assert_eq!(parse("-3 4"), NSRange::new(3, 4));
    assert_eq!(parse("3.7 4.2"), NSRange::new(3, 7));
    assert_eq!(parse("a 1 b 2 c 3"), NSRange::new(1, 2));
    assert_eq!(parse("{99999999999999999999, 1}"), NSRange::new(usize::MAX, 1));
}
