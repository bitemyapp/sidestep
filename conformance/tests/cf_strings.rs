//! CoreFoundation's string functions beyond the basics (services.rs has
//! those): mutable strings edited in place, arrays of pieces, external
//! and file system representations, Pascal strings, line bounds, searches,
//! encodings and their names, case, folding, normalization and
//! transforms. Runs on macOS against Apple's CoreFoundation and on Linux
//! against Sidestep's.
#![allow(deprecated)]

use std::ffi::{CStr, c_char, c_void};
use std::fmt::Debug;

use objc2_core_foundation::{CFIndex, CFMutableString, CFRange, CFRetained, CFString, CFStringBuiltInEncodings};
use sidestep as _;

/// Mismatches, collected so that one run shows them all.
#[derive(Default)]
struct Checks(Vec<String>);

impl Checks {
    fn eq<T: PartialEq + Debug>(&mut self, what: &str, got: T, want: T) {
        if got != want {
            self.0.push(format!("{what}: got {got:?}, want {want:?}"));
        }
    }

    fn done(self) {
        assert!(self.0.is_empty(), "{:#?}", self.0);
    }
}

fn range(location: CFIndex, length: CFIndex) -> CFRange {
    CFRange { location, length }
}

fn s(text: &str) -> CFRetained<CFString> {
    CFString::from_str(text)
}

fn mutable(text: &str) -> CFRetained<CFMutableString> {
    let m = objc2_core_foundation::CFStringCreateMutable(None, 0).unwrap();
    objc2_core_foundation::CFStringAppend(Some(&m), Some(&s(text)));
    m
}

const UTF8: u32 = CFStringBuiltInEncodings::EncodingUTF8.0;
const ASCII: u32 = CFStringBuiltInEncodings::EncodingASCII.0;
const MAC_ROMAN: u32 = CFStringBuiltInEncodings::EncodingMacRoman.0;
const LATIN1: u32 = CFStringBuiltInEncodings::EncodingISOLatin1.0;
const WINDOWS_LATIN1: u32 = CFStringBuiltInEncodings::EncodingWindowsLatin1.0;
const UTF16: u32 = CFStringBuiltInEncodings::EncodingUnicode.0;
const UTF16BE: u32 = CFStringBuiltInEncodings::EncodingUTF16BE.0;
const UTF16LE: u32 = CFStringBuiltInEncodings::EncodingUTF16LE.0;
const UTF32: u32 = CFStringBuiltInEncodings::EncodingUTF32.0;
const UTF32BE: u32 = CFStringBuiltInEncodings::EncodingUTF32BE.0;
const UTF32LE: u32 = CFStringBuiltInEncodings::EncodingUTF32LE.0;

#[test]
fn mutable_strings_change_in_place() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let m = CFStringCreateMutable(None, 0).unwrap();
    c.eq("new is empty", m.to_string(), String::new());
    CFStringAppend(Some(&m), Some(&s("hé")));
    unsafe { CFStringAppendCString(Some(&m), c"llo".as_ptr(), UTF8) };
    let units: Vec<u16> = " w".encode_utf16().collect();
    unsafe { CFStringAppendCharacters(Some(&m), units.as_ptr(), units.len() as CFIndex) };
    unsafe { CFStringAppendPascalString(Some(&m), b"\x03orl".as_ptr().cast(), ASCII) };
    c.eq("appended", m.to_string(), "héllo worl".to_string());
    unsafe { CFStringInsert(Some(&m), 0, Some(&s(">"))) };
    c.eq("inserted", m.to_string(), ">héllo worl".to_string());
    unsafe { CFStringDelete(Some(&m), range(0, 1)) };
    c.eq("deleted", m.to_string(), "héllo worl".to_string());
    unsafe { CFStringReplace(Some(&m), range(6, 4), Some(&s("there"))) };
    c.eq("replaced", m.to_string(), "héllo there".to_string());
    CFStringReplaceAll(Some(&m), Some(&s("a-b-a-b")));
    let replaced = unsafe {
        CFStringFindAndReplace(Some(&m), Some(&s("a")), Some(&s("xy")), range(0, 7), CFStringCompareFlags::empty())
    };
    c.eq("find and replace count", replaced, 2);
    c.eq("found and replaced", m.to_string(), "xy-b-xy-b".to_string());
    let replaced = unsafe {
        CFStringFindAndReplace(
            Some(&m),
            Some(&s("XY")),
            Some(&s("z")),
            range(1, 8),
            CFStringCompareFlags::CompareCaseInsensitive,
        )
    };
    c.eq("case-insensitive count in 1..9", replaced, 1);
    c.eq("replaced in a range", m.to_string(), "xy-b-z-b".to_string());

    // Padding truncates or repeats the pad from an index into it.
    let p = mutable("abc");
    unsafe { CFStringPad(Some(&p), Some(&s("xyz")), 8, 1) };
    c.eq("padded", p.to_string(), "abcyzxyz".to_string());
    unsafe { CFStringPad(Some(&p), None, 2, 0) };
    c.eq("truncated", p.to_string(), "ab".to_string());

    // Trimming a string trims whole copies of it from both ends.
    let t = mutable("xyxyabcxyx");
    CFStringTrim(Some(&t), Some(&s("xy")));
    c.eq("trimmed xy", t.to_string(), "abcxyx".to_string());
    let t = mutable("xyxyabcxy");
    CFStringTrim(Some(&t), Some(&s("xy")));
    c.eq("trimmed xy at both ends", t.to_string(), "abc".to_string());
    let w = mutable(" \t\n abc \u{a0}\r\n");
    CFStringTrimWhitespace(Some(&w));
    c.eq("trimmed whitespace", w.to_string(), "abc".to_string());

    let u = mutable("straße ǆ");
    CFStringUppercase(Some(&u), None);
    c.eq("uppercased", u.to_string(), "STRASSE Ǆ".to_string());
    CFStringLowercase(Some(&u), None);
    c.eq("lowercased", u.to_string(), "strasse ǆ".to_string());
    let t = mutable("hello wORLD o'neil");
    CFStringCapitalize(Some(&t), None);
    c.eq("capitalized", t.to_string(), "Hello World O'neil".to_string());

    let f = mutable("Café");
    CFStringFold(
        Some(&f),
        CFStringCompareFlags::CompareCaseInsensitive | CFStringCompareFlags::CompareDiacriticInsensitive,
        None,
    );
    c.eq("folded", f.to_string(), "cafe".to_string());
    let n = mutable("e\u{301}");
    unsafe { CFStringNormalize(Some(&n), CFStringNormalizationForm::C) };
    c.eq("NFC", n.to_string(), "\u{e9}".to_string());
    unsafe { CFStringNormalize(Some(&n), CFStringNormalizationForm::D) };
    c.eq("NFD", n.to_string(), "e\u{301}".to_string());
    let k = mutable("ﬁ²");
    unsafe { CFStringNormalize(Some(&k), CFStringNormalizationForm::KC) };
    c.eq("NFKC", k.to_string(), "fi2".to_string());

    let copy = CFStringCreateMutableCopy(None, 0, Some(&s("copy"))).unwrap();
    CFStringAppend(Some(&copy), Some(&s("!")));
    c.eq("mutable copy", copy.to_string(), "copy!".to_string());
    c.done();
}

#[test]
fn transforms() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let transform = |text: &str, t: Option<&CFString>, reverse: bool| {
        let m = mutable(text);
        let ok = unsafe { CFStringTransform(Some(&m), std::ptr::null_mut(), t, reverse) };
        (ok, m.to_string())
    };
    c.eq(
        "strip marks",
        transform("Café ǅ", unsafe { kCFStringTransformStripCombiningMarks }, false),
        (true, "Cafe ǅ".into()),
    );
    c.eq(
        "strip diacritics",
        transform("Ñandú", unsafe { kCFStringTransformStripDiacritics }, false),
        (true, "Nandu".into()),
    );
    c.eq(
        "to XML hex",
        transform("a é 😀", unsafe { kCFStringTransformToXMLHex }, false),
        (true, "a &#xE9; &#x1F600;".into()),
    );
    c.eq(
        "from XML hex",
        transform("a &#xE9; &#x1F600;", unsafe { kCFStringTransformToXMLHex }, true),
        (true, "a é 😀".into()),
    );
    c.eq(
        "fullwidth",
        transform("ＡＢＣ１２３！", unsafe { kCFStringTransformFullwidthHalfwidth }, false),
        (true, "ABC123!".into()),
    );
    c.eq(
        "to fullwidth",
        transform("AB1", unsafe { kCFStringTransformFullwidthHalfwidth }, true),
        (true, "ＡＢ１".into()),
    );
    c.eq(
        "hiragana",
        transform("ひらがな", unsafe { kCFStringTransformHiraganaKatakana }, false),
        (true, "ヒラガナ".into()),
    );
    c.eq(
        "katakana",
        transform("カタカナ", unsafe { kCFStringTransformHiraganaKatakana }, true),
        (true, "かたかな".into()),
    );
    c.eq(
        "XML hex of controls",
        transform("a\n\t<&>\u{7f}\u{80}é", unsafe { kCFStringTransformToXMLHex }, false),
        (true, "a&#xA;&#x9;<&>&#x7F;&#x80;&#xE9;".into()),
    );
    c.eq(
        "only lowercase hex escapes",
        transform("&#xe9;&#233;&#x41;&#X42;&#xZZ;&amp;&#x1f600;x", unsafe { kCFStringTransformToXMLHex }, true),
        (true, "é&#233;A&#X42;&#xZZ;&amp;😀x".into()),
    );
    c.eq(
        "katakana to halfwidth",
        transform("ガパア　ＡＢ￥ー、。「」한", unsafe { kCFStringTransformFullwidthHalfwidth }, false),
        (true, "ｶ\u{ff9e}ﾊ\u{ff9f}ｱ AB¥ｰ､｡｢｣한".into()),
    );
    c.eq(
        "halfwidth katakana to fullwidth",
        transform("ｶﾞﾊﾟｱ Ab¥ｰ､｡｢｣~\\", unsafe { kCFStringTransformFullwidthHalfwidth }, true),
        (true, "ガパア\u{3000}Ａｂ￥ー、。「」～＼".into()),
    );
    c.eq(
        "hiragana's marks",
        transform("ゝゞゔゕゖぁ ア", unsafe { kCFStringTransformHiraganaKatakana }, false),
        (true, "ヽヾヴゕゖァ ア".into()),
    );
    c.eq(
        "katakana without hiragana",
        transform("ヽヾヴヵヶァヷーカ ", unsafe { kCFStringTransformHiraganaKatakana }, true),
        (true, "ゝゞゔかけぁわ\u{3099}ーか ".into()),
    );
    c.eq(
        "strip more",
        transform("Ñandú ø Å ǅ ﬁ ẞ ḱ", unsafe { kCFStringTransformStripDiacritics }, false),
        (true, "Nandu ø A ǅ ﬁ ẞ k".into()),
    );
    c.eq(
        "strip reversed",
        transform("Ñandú", unsafe { kCFStringTransformStripCombiningMarks }, true),
        (true, "Ñandú".into()),
    );
    c.eq("an ICU name", transform("é", Some(&s("Any-Hex/XML")), false), (true, "&#xE9;".into()));
    c.eq("an unknown name", transform("é", Some(&s("Bogus-Thing")), false), (false, "é".into()));
    // Part of a string, the range updated to the result's.
    let m = mutable("ab é cd é");
    let mut r = range(3, 3);
    let ok = unsafe { CFStringTransform(Some(&m), &mut r, kCFStringTransformToXMLHex, false) };
    c.eq("ranged", (ok, m.to_string(), r.location, r.length), (true, "ab &#xE9; cd é".into(), 3, 8));
    c.done();
}

#[test]
fn transform_names() {
    let names = unsafe {
        [
            (objc2_core_foundation::kCFStringTransformStripCombiningMarks, ")kCFStringTransformStripCombiningMarks"),
            (objc2_core_foundation::kCFStringTransformToLatin, ")kCFStringTransformToLatin"),
            (objc2_core_foundation::kCFStringTransformFullwidthHalfwidth, ")kCFStringTransformFullwidthHalfwidth"),
            (objc2_core_foundation::kCFStringTransformLatinKatakana, ")kCFStringTransformLatinKatakana"),
            (objc2_core_foundation::kCFStringTransformLatinHiragana, ")kCFStringTransformLatinHiragana"),
            (objc2_core_foundation::kCFStringTransformHiraganaKatakana, ")kCFStringTransformHiraganaKatakana"),
            (objc2_core_foundation::kCFStringTransformMandarinLatin, ")kCFStringTransformMandarinLatin"),
            (objc2_core_foundation::kCFStringTransformLatinHangul, ")kCFStringTransformLatinHangul"),
            (objc2_core_foundation::kCFStringTransformLatinArabic, ")kCFStringTransformLatinArabic"),
            (objc2_core_foundation::kCFStringTransformLatinHebrew, ")kCFStringTransformLatinHebrew"),
            (objc2_core_foundation::kCFStringTransformLatinThai, ")kCFStringTransformLatinThai"),
            (objc2_core_foundation::kCFStringTransformLatinCyrillic, ")kCFStringTransformLatinCyrillic"),
            (objc2_core_foundation::kCFStringTransformLatinGreek, ")kCFStringTransformLatinGreek"),
            (objc2_core_foundation::kCFStringTransformToXMLHex, ")kCFStringTransformToXMLHex"),
            (objc2_core_foundation::kCFStringTransformToUnicodeName, ")kCFStringTransformToUnicodeName"),
            (objc2_core_foundation::kCFStringTransformStripDiacritics, ")kCFStringTransformStripDiacritics"),
        ]
    };
    for (name, want) in names {
        assert_eq!(name.map(|n| n.to_string()).as_deref(), Some(want));
    }
}

#[test]
fn external_characters_are_the_backing_store() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    // The string reads the caller's buffer, and edits within its capacity
    // write there.
    let mut buffer: Vec<u16> = "abc".encode_utf16().chain([0; 5]).collect();
    let m =
        unsafe { CFStringCreateMutableWithExternalCharactersNoCopy(None, buffer.as_mut_ptr(), 3, 8, kCFAllocatorNull) }
            .unwrap();
    c.eq("reads the buffer", m.to_string(), "abc".to_string());
    CFStringAppend(Some(&m), Some(&s("de")));
    c.eq("appended", m.to_string(), "abcde".to_string());
    c.eq("buffer written", String::from_utf16_lossy(&buffer[..5]), "abcde".to_string());
    // A new buffer.
    let mut other: Vec<u16> = "xy".encode_utf16().chain([0; 2]).collect();
    unsafe { CFStringSetExternalCharactersNoCopy(Some(&m), other.as_mut_ptr(), 2, 4) };
    c.eq("new buffer", m.to_string(), "xy".to_string());
    CFStringAppend(Some(&m), Some(&s("z")));
    c.eq("new buffer written", String::from_utf16_lossy(&other[..3]), "xyz".to_string());
    c.eq("length", CFStringGetLength(&m), 3);
    drop(m);
    c.done();
}

#[test]
fn pieces_and_find_results() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let strings = |array: &CFArray| {
        let n = CFArrayGetCount(array);
        (0..n).map(|i| unsafe { &*CFArrayGetValueAtIndex(array, i).cast::<CFString>() }.to_string()).collect::<Vec<_>>()
    };
    let parts = CFStringCreateArrayBySeparatingStrings(None, Some(&s("a,b,,c")), Some(&s(","))).unwrap();
    c.eq("separated", strings(&parts), vec!["a".into(), "b".into(), "".into(), "c".into()]);
    let whole = CFStringCreateArrayBySeparatingStrings(None, Some(&s("abc")), Some(&s(","))).unwrap();
    c.eq("no separator", strings(&whole), vec!["abc".to_string()]);
    let empty = CFStringCreateArrayBySeparatingStrings(None, Some(&s("")), Some(&s(","))).unwrap();
    c.eq("empty", strings(&empty), vec![String::new()]);
    let joined = unsafe { CFStringCreateByCombiningStrings(None, Some(&parts), Some(&s("+"))) }.unwrap();
    c.eq("combined", joined.to_string(), "a+b++c".to_string());

    // The results are pointers to ranges.
    let found = unsafe {
        CFStringCreateArrayWithFindResults(
            None,
            Some(&s("abcabcab")),
            Some(&s("ab")),
            range(0, 8),
            CFStringCompareFlags::empty(),
        )
    }
    .unwrap();
    let ranges: Vec<(CFIndex, CFIndex)> = (0..CFArrayGetCount(&found))
        .map(|i| {
            let r = unsafe { &*CFArrayGetValueAtIndex(&found, i).cast::<CFRange>() };
            (r.location, r.length)
        })
        .collect();
    c.eq("find results", ranges, vec![(0, 2), (3, 2), (6, 2)]);
    let none = unsafe {
        CFStringCreateArrayWithFindResults(
            None,
            Some(&s("abc")),
            Some(&s("x")),
            range(0, 3),
            CFStringCompareFlags::empty(),
        )
    };
    c.eq("no find results", none.is_none(), true);
    c.done();
}

/// A find-results array holds pointers to ranges, not objects: copying,
/// describing, comparing and changing it go by the ranges.
#[test]
fn find_results_as_an_array() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let results = |text: &str| {
        let n = s(text).length();
        unsafe {
            CFStringCreateArrayWithFindResults(
                None,
                Some(&s(text)),
                Some(&s("ab")),
                range(0, n),
                CFStringCompareFlags::empty(),
            )
        }
        .unwrap()
    };
    let ranges = |a: &CFArray| -> Vec<(CFIndex, CFIndex)> {
        (0..CFArrayGetCount(a))
            .map(|i| {
                let r = unsafe { &*CFArrayGetValueAtIndex(a, i).cast::<CFRange>() };
                (r.location, r.length)
            })
            .collect()
    };
    let pointers = |a: &CFArray| {
        (0..CFArrayGetCount(a)).map(|i| unsafe { CFArrayGetValueAtIndex(a, i) } as usize).collect::<Vec<_>>()
    };
    let found = results("abcabcab");
    let copy = unsafe { CFArrayCreateCopy(None, Some(&found)) }.unwrap();
    c.eq("copy's ranges", ranges(&copy), vec![(0, 2), (3, 2), (6, 2)]);
    c.eq("copy shares the pointers", pointers(&copy), pointers(&found));
    let m = unsafe { CFArrayCreateMutableCopy(None, 0, Some(&found)) }.unwrap();
    c.eq("mutable copy shares the pointers", pointers(&m), pointers(&found));
    let description = CFCopyDescription(Some(&found)).unwrap().to_string();
    c.eq(
        "description's values",
        description.split_once("{type = ").map(|(_, rest)| rest.to_string()),
        Some("mutable-small, count = 3, values = (\n\t0 : {0, 2}\n\t1 : {3, 2}\n\t2 : {6, 2}\n)}".into()),
    );
    c.eq("description's start", description.starts_with("<CFArray 0x"), true);
    c.eq("equal to another search's", CFEqual(Some(&found), Some(&results("abcabcab"))), true);
    c.eq("equal to its copies", (CFEqual(Some(&found), Some(&copy)), CFEqual(Some(&m), Some(&found))), (true, true));
    c.eq("not equal to other ranges", CFEqual(Some(&found), Some(&results("xabcabcab"))), false);
    c.eq("hash", CFHash(Some(&found)), 3);
    // Values compare by their ranges.
    let probe = range(3, 2);
    let probe = (&raw const probe).cast::<c_void>();
    unsafe {
        c.eq("count of a range", CFArrayGetCountOfValue(&found, range(0, 3), probe), 1);
        c.eq("first index of a range", CFArrayGetFirstIndexOfValue(&found, range(0, 3), probe), 1);
        c.eq("last index of a range", CFArrayGetLastIndexOfValue(&found, range(0, 3), probe), 1);
    }
    unsafe extern "C-unwind" fn later_first(a: *const c_void, b: *const c_void, _: *mut c_void) -> CFComparisonResult {
        let (a, b) = unsafe { (&*a.cast::<CFRange>(), &*b.cast::<CFRange>()) };
        CFComparisonResult(b.location.cmp(&a.location) as CFIndex)
    }
    unsafe { CFArraySortValues(Some(&m), range(0, 3), Some(later_first), std::ptr::null_mut()) };
    c.eq("sorted", ranges(&m), vec![(6, 2), (3, 2), (0, 2)]);
    unsafe { CFArrayExchangeValuesAtIndices(Some(&m), 0, 2) };
    unsafe { CFArrayRemoveValueAtIndex(Some(&m), 1) };
    unsafe { CFArrayAppendValue(Some(&m), CFArrayGetValueAtIndex(&found, 0)) };
    c.eq("changed", ranges(&m), vec![(0, 2), (6, 2), (0, 2)]);
    drop((found, copy));
    c.eq("the ranges outlive the arrays they came from", ranges(&m), vec![(0, 2), (6, 2), (0, 2)]);
    CFArrayRemoveAllValues(Some(&m));
    c.eq("emptied", CFArrayGetCount(&m), 0);
    c.done();
}

#[test]
fn representations() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let bytes = |d: &CFData| d.to_vec();
    let text = s("hé");
    let external =
        |encoding: u32| CFStringCreateExternalRepresentation(None, Some(&text), encoding, 0).map(|d| bytes(&d));
    c.eq("utf8", external(UTF8), Some(b"h\xc3\xa9".to_vec()));
    c.eq("utf16", external(UTF16), Some(vec![0xff, 0xfe, b'h', 0, 0xe9, 0]));
    c.eq("utf16be", external(UTF16BE), Some(vec![0, b'h', 0, 0xe9]));
    c.eq("utf32", external(UTF32), Some(vec![0xff, 0xfe, 0, 0, b'h', 0, 0, 0, 0xe9, 0, 0, 0]));
    c.eq("latin1", external(LATIN1), Some(vec![b'h', 0xe9]));
    c.eq("ascii without a loss byte", external(ASCII), None);
    let lossy = CFStringCreateExternalRepresentation(None, Some(&text), ASCII, b'?').map(|d| bytes(&d));
    c.eq("ascii with a loss byte", lossy, Some(b"h?".to_vec()));
    let from = |b: &[u8], encoding: u32| {
        CFStringCreateFromExternalRepresentation(None, Some(&CFData::from_bytes(b)), encoding).map(|s| s.to_string())
    };
    c.eq("from utf16 with a mark", from(&[0xfe, 0xff, 0, b'h', 0, 0xe9], UTF16), Some("hé".into()));
    c.eq("from utf16 without one", from(&[0, b'h', 0, 0xe9], UTF16), Some("hé".into()));
    c.eq("from utf8 with a mark", from(b"\xef\xbb\xbfh\xc3\xa9", UTF8), Some("hé".into()));
    c.eq("from bad utf8", from(b"\xff", UTF8), None);

    // File system representations: UTF-8, NUL-terminated.
    let fs = unsafe { CFStringCreateWithFileSystemRepresentation(None, c"/tmp/a b".as_ptr()) }.unwrap();
    c.eq("from fs", fs.to_string(), "/tmp/a b".to_string());
    let mut buffer = [0 as c_char; 32];
    c.eq("to fs", unsafe { CFStringGetFileSystemRepresentation(&fs, buffer.as_mut_ptr(), 32) }, true);
    c.eq("to fs bytes", unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes(), b"/tmp/a b".as_slice());
    c.eq("to fs too small", unsafe { CFStringGetFileSystemRepresentation(&fs, buffer.as_mut_ptr(), 8) }, false);
    c.eq("max fs size", CFStringGetMaximumSizeOfFileSystemRepresentation(&s("abc")), 10);
    c.eq("max fs size empty", CFStringGetMaximumSizeOfFileSystemRepresentation(&s("")), 1);

    // Pascal strings: a length byte, then the bytes.
    let p = unsafe { CFStringCreateWithPascalString(None, b"\x03caf\x8e".as_ptr().cast(), MAC_ROMAN) }.unwrap();
    c.eq("from pascal", p.to_string(), "caf".to_string());
    let p = unsafe { CFStringCreateWithPascalString(None, b"\x04caf\x8e".as_ptr().cast(), MAC_ROMAN) }.unwrap();
    c.eq("from pascal mac roman", p.to_string(), "café".to_string());
    let pascal = b"\x02hi".to_vec();
    let q =
        unsafe { CFStringCreateWithPascalStringNoCopy(None, pascal.as_ptr().cast(), ASCII, kCFAllocatorNull) }.unwrap();
    c.eq("from pascal, no copy", q.to_string(), "hi".to_string());
    let mut out = [0u8; 8];
    c.eq("to pascal", unsafe { CFStringGetPascalString(&p, out.as_mut_ptr().cast(), 8, MAC_ROMAN) }, true);
    c.eq("to pascal bytes", &out[..5], b"\x04caf\x8e".as_slice());
    c.eq("to pascal too small", unsafe { CFStringGetPascalString(&p, out.as_mut_ptr().cast(), 4, MAC_ROMAN) }, false);
    let long = s(&"x".repeat(300));
    let mut big = [0u8; 400];
    c.eq("to pascal too long", unsafe { CFStringGetPascalString(&long, big.as_mut_ptr().cast(), 400, ASCII) }, false);
    c.done();
}

#[test]
fn bounds_and_searches() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let text = s("one\ntwo\u{2028}three\r\nfour");
    let (mut begin, mut end, mut contents) = (0, 0, 0);
    unsafe { CFStringGetLineBounds(&text, range(5, 0), &mut begin, &mut end, &mut contents) };
    c.eq("line", (begin, end, contents), (4, 8, 7));
    unsafe { CFStringGetParagraphBounds(&text, range(5, 0), &mut begin, &mut end, &mut contents) };
    c.eq("paragraph", (begin, end, contents), (4, 15, 13));
    unsafe { CFStringGetLineBounds(&text, range(10, 0), &mut begin, std::ptr::null_mut(), std::ptr::null_mut()) };
    c.eq("line start only", begin, 8);
    let composed = s("ae\u{301}😀");
    let r = unsafe { CFStringGetRangeOfComposedCharactersAtIndex(&composed, 2) };
    c.eq("composed é", (r.location, r.length), (1, 2));
    let r = unsafe { CFStringGetRangeOfComposedCharactersAtIndex(&composed, 4) };
    c.eq("composed emoji", (r.location, r.length), (3, 2));

    let hay = s("Hello, hello, HELLO");
    let find = |needle: &str, r: CFRange, flags: CFStringCompareFlags| {
        let mut out = range(-9, -9);
        let found = unsafe { CFStringFindWithOptions(&hay, Some(&s(needle)), r, flags, &mut out) };
        (found, out.location, out.length)
    };
    c.eq("find", find("hello", range(0, 19), CFStringCompareFlags::empty()), (true, 7, 5));
    c.eq("find ci", find("hello", range(0, 19), CFStringCompareFlags::CompareCaseInsensitive), (true, 0, 5));
    c.eq(
        "find ci backwards",
        find(
            "hello",
            range(0, 19),
            CFStringCompareFlags::CompareCaseInsensitive | CFStringCompareFlags::CompareBackwards,
        ),
        (true, 14, 5),
    );
    c.eq("find anchored", find("ello", range(0, 19), CFStringCompareFlags::CompareAnchored), (false, -9, -9));
    c.eq("find in range", find("hello", range(8, 11), CFStringCompareFlags::CompareCaseInsensitive), (true, 14, 5));
    let find_in = |hay: &str, needle: &str, flags: CFStringCompareFlags| {
        let mut out = range(-9, -9);
        let hay = s(hay);
        let length = CFStringGetLength(&hay);
        let found = unsafe { CFStringFindWithOptions(&hay, Some(&s(needle)), range(0, length), flags, &mut out) };
        (found, out.location, out.length)
    };
    c.eq(
        "find without diacritics",
        find_in("résumé", "e", CFStringCompareFlags::CompareDiacriticInsensitive),
        (true, 1, 1),
    );
    c.eq("find literally", find_in("e\u{301}x", "\u{e9}", CFStringCompareFlags::empty()), (false, -9, -9));
    c.eq("find nonliterally", find_in("e\u{301}x", "\u{e9}", CFStringCompareFlags::CompareNonliteral), (true, 0, 2));
    let mut out = range(0, 0);
    let found = unsafe {
        CFStringFindWithOptionsAndLocale(
            &hay,
            Some(&s("HELLO")),
            range(0, 19),
            CFStringCompareFlags::empty(),
            None,
            &mut out,
        )
    };
    c.eq("find with a locale", (found, out.location), (true, 14));
    let digits = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::DecimalDigit).unwrap();
    let with_digits = s("ab12cd3");
    let mut out = range(0, 0);
    let found = unsafe {
        CFStringFindCharacterFromSet(&with_digits, Some(&digits), range(0, 7), CFStringCompareFlags::empty(), &mut out)
    };
    c.eq("first digit", (found, out.location, out.length), (true, 2, 1));
    let found = unsafe {
        CFStringFindCharacterFromSet(
            &with_digits,
            Some(&digits),
            range(0, 7),
            CFStringCompareFlags::CompareBackwards,
            &mut out,
        )
    };
    c.eq("last digit", (found, out.location, out.length), (true, 6, 1));
    let letters: CFRetained<CFCharacterSet> = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::Letter).unwrap();
    let found = unsafe {
        CFStringFindCharacterFromSet(&s("123"), Some(&letters), range(0, 3), CFStringCompareFlags::empty(), &mut out)
    };
    c.eq("no letter", found, false);

    let compare = |a: &str, b: &str, flags: CFStringCompareFlags, locale: Option<&CFLocale>| {
        unsafe {
            CFStringCompareWithOptionsAndLocale(
                &s(a),
                Some(&s(b)),
                range(0, a.encode_utf16().count() as CFIndex),
                flags,
                locale,
            )
        }
        .0
    };
    c.eq("compare literal", compare("a", "B", CFStringCompareFlags::empty(), None), 1);
    c.eq("compare ci", compare("a", "B", CFStringCompareFlags::CompareCaseInsensitive, None), -1);
    c.eq("compare numeric", compare("file10", "file9", CFStringCompareFlags::CompareNumerically, None), 1);
    // Literal comparison tells composed and decomposed text apart; a
    // non-literal one doesn't.
    c.eq("compare literal é", compare("\u{e9}", "e\u{301}", CFStringCompareFlags::empty(), None) == 0, false);
    c.eq("compare nonliteral é", compare("\u{e9}", "e\u{301}", CFStringCompareFlags::CompareNonliteral, None), 0);
    c.eq("compare diacritics", compare("résumé", "resume", CFStringCompareFlags::CompareDiacriticInsensitive, None), 0);
    c.done();
}

#[test]
fn encodings() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    // The list ends with kCFStringEncodingInvalidId and holds the
    // encodings the string functions take.
    let list = CFStringGetListOfAvailableEncodings();
    let mut available = Vec::new();
    for i in 0.. {
        let e = unsafe { *list.add(i) };
        if e == 0xffff_ffff {
            break;
        }
        available.push(e);
    }
    for e in [UTF8, ASCII, MAC_ROMAN, LATIN1, WINDOWS_LATIN1, UTF16, UTF16BE, UTF16LE, UTF32, UTF32BE, UTF32LE] {
        c.eq(&format!("{e:#x} available"), available.contains(&e), true);
    }
    let name = |e: u32| CFStringGetNameOfEncoding(e).map(|n| n.to_string());
    c.eq("utf8 name", name(UTF8), Some("Unicode (UTF-8)".into()));
    c.eq("ascii name", name(ASCII), Some("Western (ASCII)".into()));
    c.eq("mac roman name", name(MAC_ROMAN), Some("Western (Mac OS Roman)".into()));
    c.eq("latin1 name", name(LATIN1), Some("Western (ISO Latin 1)".into()));
    c.eq("windows latin1 name", name(WINDOWS_LATIN1), Some("Western (Windows Latin 1)".into()));
    c.eq("utf16 name", name(UTF16), Some("Unicode (UTF-16)".into()));
    c.eq("utf16be name", name(UTF16BE), Some("Unicode (UTF-16BE)".into()));
    c.eq("utf16le name", name(UTF16LE), Some("Unicode (UTF-16LE)".into()));
    c.eq("utf32 name", name(UTF32), Some("Unicode (UTF-32)".into()));
    c.eq("utf32be name", name(UTF32BE), Some("Unicode (UTF-32BE)".into()));
    c.eq("utf32le name", name(UTF32LE), Some("Unicode (UTF-32LE)".into()));
    c.eq("unknown name", name(0x7777), None);
    let iana = |e: u32| CFStringConvertEncodingToIANACharSetName(e).map(|n| n.to_string());
    c.eq("utf8 iana", iana(UTF8), Some("utf-8".into()));
    c.eq("ascii iana", iana(ASCII), Some("us-ascii".into()));
    c.eq("mac roman iana", iana(MAC_ROMAN), Some("macintosh".into()));
    c.eq("latin1 iana", iana(LATIN1), Some("iso-8859-1".into()));
    c.eq("windows latin1 iana", iana(WINDOWS_LATIN1), Some("windows-1252".into()));
    c.eq("utf16 iana", iana(UTF16), Some("utf-16".into()));
    c.eq("utf16be iana", iana(UTF16BE), Some("utf-16be".into()));
    c.eq("utf16le iana", iana(UTF16LE), Some("utf-16le".into()));
    c.eq("utf32 iana", iana(UTF32), Some("utf-32".into()));
    c.eq("utf32be iana", iana(UTF32BE), Some("utf-32be".into()));
    c.eq("utf32le iana", iana(UTF32LE), Some("utf-32le".into()));
    let from_iana = |n: &str| CFStringConvertIANACharSetNameToEncoding(&s(n));
    c.eq("UTF-8 from iana", from_iana("UTF-8"), UTF8);
    c.eq("utf8 from iana", from_iana("utf8"), UTF8);
    c.eq("csUTF8 from iana", from_iana("csUTF8"), 0xffff_ffff);
    c.eq("latin1 from iana", from_iana("ISO-8859-1"), LATIN1);
    c.eq("latin1 alias from iana", from_iana("latin1"), LATIN1);
    c.eq("ascii from iana", from_iana("US-ASCII"), ASCII);
    c.eq("macintosh from iana", from_iana("macintosh"), MAC_ROMAN);
    c.eq("unknown from iana", from_iana("klingon"), 0xffff_ffff);
    let to_ns = |e: u32| CFStringConvertEncodingToNSStringEncoding(e) as u64;
    c.eq("utf8 ns", to_ns(UTF8), 4);
    c.eq("ascii ns", to_ns(ASCII), 1);
    c.eq("mac roman ns", to_ns(MAC_ROMAN), 30);
    c.eq("latin1 ns", to_ns(LATIN1), 5);
    c.eq("windows latin1 ns", to_ns(WINDOWS_LATIN1), 12);
    c.eq("utf16 ns", to_ns(UTF16), 10);
    c.eq("utf16be ns", to_ns(UTF16BE), 0x9000_0100);
    c.eq("utf16le ns", to_ns(UTF16LE), 0x9400_0100);
    c.eq("utf32 ns", to_ns(UTF32), 0x8c00_0100);
    c.eq("utf32be ns", to_ns(UTF32BE), 0x9800_0100);
    c.eq("utf32le ns", to_ns(UTF32LE), 0x9c00_0100);
    c.eq("other ns", to_ns(0x0a01), 0x8000_0a01);
    let from_ns = |e: u64| CFStringConvertNSStringEncodingToEncoding(e as _);
    for (cf, ns) in
        [(UTF8, 4), (ASCII, 1), (MAC_ROMAN, 30), (LATIN1, 5), (WINDOWS_LATIN1, 12), (UTF16, 10), (UTF16BE, 0x9000_0100)]
    {
        c.eq(&format!("from ns {ns:#x}"), from_ns(ns), cf);
    }
    c.eq("from ns other", from_ns(0x8000_0a01), 0x0a01);
    c.eq("from ns unknown", from_ns(1000), 0xffff_ffff);
    let cp = |e: u32| CFStringConvertEncodingToWindowsCodepage(e);
    c.eq("utf8 codepage", cp(UTF8), 65001);
    c.eq("latin1 codepage", cp(LATIN1), 28591);
    c.eq("windows latin1 codepage", cp(WINDOWS_LATIN1), 1252);
    c.eq("mac roman codepage", cp(MAC_ROMAN), 10000);
    c.eq("ascii codepage", cp(ASCII), 20127);
    c.eq("utf16le codepage", cp(UTF16LE), 0xffff_ffff);
    c.eq("utf16 codepage", cp(UTF16), 1200);
    c.eq("utf32 codepage", cp(UTF32), 65005);
    c.eq("utf32be codepage", cp(UTF32BE), 65006);
    c.eq("utf16be codepage", cp(UTF16BE), 1201);
    let from_cp = |p: u32| CFStringConvertWindowsCodepageToEncoding(p);
    c.eq("65001", from_cp(65001), UTF8);
    c.eq("1252", from_cp(1252), WINDOWS_LATIN1);
    c.eq("28591", from_cp(28591), LATIN1);
    c.eq("unknown codepage", from_cp(3), 0xffff_ffff);
    c.eq("most compatible mac of utf8", CFStringGetMostCompatibleMacStringEncoding(UTF8), UTF16);
    c.eq("most compatible mac of ascii", CFStringGetMostCompatibleMacStringEncoding(ASCII), MAC_ROMAN);
    c.eq("most compatible mac of latin1", CFStringGetMostCompatibleMacStringEncoding(LATIN1), MAC_ROMAN);
    c.eq("smallest of ascii", CFStringGetSmallestEncoding(&s("abc")), ASCII);
    c.eq("smallest of latin", CFStringGetSmallestEncoding(&s("café")), MAC_ROMAN);
    c.eq("smallest of emoji", CFStringGetSmallestEncoding(&s("😀")), UTF16);
    c.eq("smallest of a euro", CFStringGetSmallestEncoding(&s("€")), MAC_ROMAN);
    c.eq("smallest of nothing", CFStringGetSmallestEncoding(&s("")), ASCII);
    c.done();
}

#[test]
fn hyphenation_without_a_dictionary() {
    use objc2_core_foundation::*;
    // With no locale, no hyphenation is available.
    assert!(!CFStringIsHyphenationAvailableForLocale(None));
    let mut character = 0u32;
    let at = unsafe {
        CFStringGetHyphenationLocationBeforeIndex(&s("hyphenation"), 8, range(0, 11), 0, None, &mut character)
    };
    assert_eq!(at, -1);
}
