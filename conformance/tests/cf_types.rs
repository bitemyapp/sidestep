//! CoreFoundation's other types: attributed strings (bidi levels too),
//! character sets (bitmap representations too), locales, calendars,
//! bundles, streams, message ports and file security objects. Runs on
//! macOS against Apple's CoreFoundation and on Linux against Sidestep's.
#![allow(deprecated, unused_unsafe)]

use std::ffi::c_void;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2_core_foundation::*;
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

fn s(text: &str) -> CFRetained<CFString> {
    CFString::from_str(text)
}

fn range(location: CFIndex, length: CFIndex) -> (CFIndex, CFIndex) {
    (location, length)
}

fn pair(r: CFRange) -> (CFIndex, CFIndex) {
    (r.location, r.length)
}

fn cf_range(location: CFIndex, length: CFIndex) -> CFRange {
    CFRange { location, length }
}

/// A CF object's description as text, for values of any type.
fn describe(value: Option<&CFType>) -> Option<String> {
    value.map(|v| {
        if let Some(string) = v.downcast_ref::<CFString>() {
            string.to_string()
        } else if let Some(number) = v.downcast_ref::<CFNumber>() {
            let mut out = 0i64;
            unsafe { CFNumberGetValue(number, CFNumberType::SInt64Type, (&raw mut out).cast()) };
            out.to_string()
        } else if let Some(boolean) = v.downcast_ref::<CFBoolean>() {
            boolean.as_bool().to_string()
        } else {
            CFCopyDescription(Some(v)).map(|d| d.to_string()).unwrap_or_default()
        }
    })
}

fn texts(array: Option<&CFArray>) -> Vec<String> {
    let Some(array) = array else { return Vec::new() };
    (0..CFArrayGetCount(array))
        .map(|i| {
            let value = unsafe { CFArrayGetValueAtIndex(array, i) };
            unsafe { &*value.cast::<CFString>() }.to_string()
        })
        .collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sidestep-cf-types-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the current run loop until `done` holds, for at most ten seconds.
fn run_until(mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() && start.elapsed() < Duration::from_secs(10) {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, 0.01, true);
    }
}

#[test]
fn attributed_strings() {
    unsafe {
        let mut c = Checks::default();
        let key_a = s("a");
        let key_b = s("b");
        let one = CFNumber::new_i32(1);
        let two = CFNumber::new_i32(2);
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&[&*key_a], &[&*one]);
        let a =
            unsafe { CFAttributedStringCreate(None, Some(&s("hello world")), Some(attributes.as_opaque())) }.unwrap();
        c.eq("type", CFGetTypeID(Some(&a)), CFAttributedString::type_id());
        c.eq("length", CFAttributedStringGetLength(&a), 11);
        c.eq("string", CFAttributedStringGetString(&a).map(|t| t.to_string()), Some("hello world".into()));
        let mut effective = cf_range(-1, -1);
        let value = unsafe { CFAttributedStringGetAttribute(&a, 3, Some(&key_a), &mut effective) };
        c.eq("attribute a", describe(value.as_deref()), Some("1".into()));
        c.eq("attribute a range", pair(effective), range(0, 11));
        let missing = unsafe { CFAttributedStringGetAttribute(&a, 3, Some(&key_b), std::ptr::null_mut()) };
        c.eq("attribute b", missing.is_none(), true);

        let m = CFAttributedStringCreateMutableCopy(None, 0, Some(&a)).unwrap();
        CFAttributedStringBeginEditing(Some(&m));
        unsafe { CFAttributedStringSetAttribute(Some(&m), cf_range(0, 5), Some(&key_b), Some(&two)) };
        CFAttributedStringEndEditing(Some(&m));
        let mut effective = cf_range(-1, -1);
        let at0 = unsafe { CFAttributedStringGetAttributes(&m, 0, &mut effective) }.unwrap();
        c.eq("attributes at 0", (CFDictionaryGetCount(&at0), pair(effective)), (2, range(0, 5)));
        let at6 = unsafe { CFAttributedStringGetAttributes(&m, 6, &mut effective) }.unwrap();
        c.eq("attributes at 6", (CFDictionaryGetCount(&at6), pair(effective)), (1, range(5, 6)));
        let mut longest = cf_range(-1, -1);
        let value = unsafe {
            CFAttributedStringGetAttributeAndLongestEffectiveRange(&m, 7, Some(&key_a), cf_range(0, 11), &mut longest)
        };
        c.eq("longest a", (describe(value.as_deref()), pair(longest)), (Some("1".into()), range(0, 11)));
        let mut longest = cf_range(-1, -1);
        let _ = unsafe { CFAttributedStringGetAttributesAndLongestEffectiveRange(&m, 8, cf_range(2, 9), &mut longest) };
        c.eq("longest attributes", pair(longest), range(5, 6));

        CFAttributedStringReplaceString(Some(&m), cf_range(0, 5), Some(&s("HELLO THERE")));
        c.eq("replaced", CFAttributedStringGetString(&m).map(|t| t.to_string()), Some("HELLO THERE world".into()));
        let mut effective = cf_range(-1, -1);
        let value = unsafe { CFAttributedStringGetAttribute(&m, 0, Some(&key_b), &mut effective) };
        c.eq("replaced text keeps b", (describe(value.as_deref()), pair(effective)), (Some("2".into()), range(0, 11)));
        CFAttributedStringRemoveAttribute(Some(&m), cf_range(0, 17), Some(&key_b));
        let at0 = unsafe { CFAttributedStringGetAttributes(&m, 0, std::ptr::null_mut()) }.unwrap();
        c.eq("b removed", CFDictionaryGetCount(&at0), 1);

        let only_b = CFDictionary::<CFString, CFType>::from_slices(&[&*key_b], &[&*two]);
        unsafe { CFAttributedStringSetAttributes(Some(&m), cf_range(0, 3), Some(only_b.as_opaque()), true) };
        let mut effective = cf_range(-1, -1);
        let at0 = unsafe { CFAttributedStringGetAttributes(&m, 0, &mut effective) }.unwrap();
        c.eq("cleared to b", (CFDictionaryGetCount(&at0), pair(effective)), (1, range(0, 3)));
        let value = unsafe { CFAttributedStringGetAttribute(&m, 0, Some(&key_a), std::ptr::null_mut()) };
        c.eq("a cleared", value.is_none(), true);
        unsafe { CFAttributedStringSetAttributes(Some(&m), cf_range(3, 2), Some(only_b.as_opaque()), false) };
        let at3 = unsafe { CFAttributedStringGetAttributes(&m, 3, std::ptr::null_mut()) }.unwrap();
        c.eq("merged with b", CFDictionaryGetCount(&at3), 2);

        let sub = CFAttributedStringCreateWithSubstring(None, Some(&m), cf_range(12, 5)).unwrap();
        c.eq("substring", CFAttributedStringGetString(&sub).map(|t| t.to_string()), Some("world".into()));
        let mut effective = cf_range(-1, -1);
        let value = unsafe { CFAttributedStringGetAttribute(&sub, 0, Some(&key_a), &mut effective) };
        c.eq("substring attribute", (describe(value.as_deref()), pair(effective)), (Some("1".into()), range(0, 5)));

        CFAttributedStringReplaceAttributedString(Some(&m), cf_range(0, 6), Some(&sub));
        c.eq(
            "replaced with attributed",
            CFAttributedStringGetString(&m).map(|t| t.to_string()),
            Some("worldTHERE world".into()),
        );
        // (An effective range needn't be the longest: macOS's CoreFoundation
        // merges equal runs here, its Foundation not.)
        let at0 =
            unsafe { CFAttributedStringGetAttributesAndLongestEffectiveRange(&m, 0, cf_range(0, 16), &mut effective) }
                .unwrap();
        c.eq("its attributes", (CFDictionaryGetCount(&at0), pair(effective)), (1, range(0, 16)));

        // CoreFoundation's own mutable attributed strings have no mutable
        // string to edit them through.
        c.eq("no mutable string", CFAttributedStringGetMutableString(Some(&m)).is_none(), true);
        CFAttributedStringReplaceString(Some(&m), cf_range(16, 0), Some(&s("!")));
        c.eq("appended", CFAttributedStringGetLength(&m), 17);
        let copy = CFAttributedStringCreateCopy(None, Some(&m)).unwrap();
        c.eq("copy equal", CFEqual(Some(&copy), Some(&m)), true);

        let empty = CFAttributedStringCreateMutable(None, 0).unwrap();
        c.eq("empty", CFAttributedStringGetLength(&empty), 0);
        c.done();
    }
}

fn bidi(text: &str, base: i8, statistical: bool) -> (bool, Vec<u8>, Vec<u8>) {
    let a = unsafe { CFAttributedStringCreate(None, Some(&s(text)), None) }.unwrap();
    let n = CFAttributedStringGetLength(&a) as usize;
    let mut levels = vec![0xffu8; n];
    let mut directions = vec![0xffu8; n];
    let found = unsafe {
        if statistical {
            CFAttributedStringGetStatisticalWritingDirections(
                &a,
                cf_range(0, n as CFIndex),
                base,
                levels.as_mut_ptr(),
                directions.as_mut_ptr(),
            )
        } else {
            CFAttributedStringGetBidiLevelsAndResolvedDirections(
                &a,
                cf_range(0, n as CFIndex),
                base,
                levels.as_mut_ptr(),
                directions.as_mut_ptr(),
            )
        }
    };
    (found, levels, directions)
}

#[test]
fn bidi_levels() {
    unsafe {
        let mut c = Checks::default();
        c.eq("latin", bidi("abc", 0, false), (false, vec![0, 0, 0], vec![0, 0, 0]));
        c.eq(
            "hebrew in latin",
            bidi("ab \u{5d0}\u{5d1} 12", 0, false),
            (true, vec![0, 0, 0, 1, 1, 1, 2, 2], vec![0; 8]),
        );
        c.eq("latin in hebrew, right to left", bidi("\u{5d0} ab", 1, false), (true, vec![1, 1, 2, 2], vec![1; 4]));
        c.eq(
            "natural base from the first strong letter",
            bidi("\u{5d0} ab", -1, false),
            (true, vec![1, 1, 2, 2], vec![1; 4]),
        );
        c.eq("natural latin", bidi("ab \u{5d0}", -1, false), (true, vec![0, 0, 0, 1], vec![0; 4]));
        c.eq("statistical", bidi("ab \u{5d0}", -1, true), (true, vec![0, 0, 0, 1], vec![0; 4]));
        c.eq("two paragraphs", bidi("\u{5d0}\nab", -1, false), (true, vec![1, 1, 0, 0], vec![1, 1, 0, 0]));
        c.eq("arabic digits", bidi("\u{627} 1", 0, false), (true, vec![1, 1, 2], vec![0; 3]));
        c.done();
    }
}

#[test]
fn character_sets() {
    unsafe {
        let mut c = Checks::default();
        let letters = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::Letter).unwrap();
        c.eq("type", CFGetTypeID(Some(&letters)), CFCharacterSet::type_id());
        let member = |set: &CFCharacterSet, ch: char| CFCharacterSetIsLongCharacterMember(set, ch as u32);
        c.eq("letters", (member(&letters, 'a'), member(&letters, 'é'), member(&letters, '1')), (true, true, false));
        let digits = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::DecimalDigit).unwrap();
        c.eq("digits", (member(&digits, '7'), member(&digits, '\u{663}'), member(&digits, 'x')), (true, true, false));
        let space = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::WhitespaceAndNewline).unwrap();
        c.eq("space", (member(&space, ' '), member(&space, '\n'), member(&space, '_')), (true, true, false));
        let space_only = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::Whitespace).unwrap();
        c.eq("whitespace", (member(&space_only, '\t'), member(&space_only, '\n')), (true, false));
        let punctuation = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::Punctuation).unwrap();
        c.eq("punctuation", (member(&punctuation, '!'), member(&punctuation, '+')), (true, false));
        let symbols = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::Symbol).unwrap();
        c.eq("symbols", (member(&symbols, '+'), member(&symbols, 'a')), (true, false));
        let upper = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::UppercaseLetter).unwrap();
        c.eq("upper", (member(&upper, 'A'), member(&upper, 'a')), (true, false));
        let newline = CFCharacterSetGetPredefined(CFCharacterSetPredefinedSet::Newline).unwrap();
        c.eq("newline", (member(&newline, '\u{2028}'), member(&newline, ' ')), (true, false));
        c.eq("utf16 member", CFCharacterSetIsCharacterMember(&letters, 'b' as u16), true);

        let range_set = CFCharacterSetCreateWithCharactersInRange(None, cf_range('a' as CFIndex, 3)).unwrap();
        c.eq("range", (member(&range_set, 'a'), member(&range_set, 'c'), member(&range_set, 'd')), (true, true, false));
        let string_set = CFCharacterSetCreateWithCharactersInString(None, Some(&s("xz"))).unwrap();
        c.eq("string", (member(&string_set, 'x'), member(&string_set, 'y')), (true, false));
        c.eq("plane 0", CFCharacterSetHasMemberInPlane(&string_set, 0), true);
        c.eq("plane 1 of latin", CFCharacterSetHasMemberInPlane(&string_set, 1), false);
        let emoji_set = CFCharacterSetCreateWithCharactersInString(None, Some(&s("😀😁"))).unwrap();
        c.eq(
            "emoji",
            (member(&emoji_set, '😀'), member(&emoji_set, '😂'), CFCharacterSetIsCharacterMember(&emoji_set, 0xd83d)),
            (true, false, false),
        );
        c.eq("plane 1", CFCharacterSetHasMemberInPlane(&emoji_set, 1), true);
        c.eq("plane 2", CFCharacterSetHasMemberInPlane(&emoji_set, 2), false);
        c.eq("plane 1 of the range", CFCharacterSetHasMemberInPlane(&range_set, 1), false);
        let inverted = CFCharacterSetCreateInvertedSet(None, Some(&range_set)).unwrap();
        c.eq("inverted", (member(&inverted, 'a'), member(&inverted, 'z')), (false, true));
        c.eq("superset", CFCharacterSetIsSupersetOfSet(&letters, Some(&range_set)), true);
        c.eq("not superset", CFCharacterSetIsSupersetOfSet(&range_set, Some(&letters)), false);

        let abc = CFCharacterSetCreateWithCharactersInString(None, Some(&s("abc"))).unwrap();
        let bitmap = CFCharacterSetCreateBitmapRepresentation(None, Some(&abc)).unwrap();
        let bytes = bitmap.to_vec();
        c.eq("bitmap length", bytes.len(), 8192);
        c.eq("bitmap bits", (bytes[12], bytes.iter().filter(|b| **b != 0).count()), (0b1110, 1));
        let back = CFCharacterSetCreateWithBitmapRepresentation(None, Some(&bitmap)).unwrap();
        c.eq("from bitmap", (member(&back, 'b'), member(&back, 'd')), (true, false));
        let wide = CFCharacterSetCreateBitmapRepresentation(None, Some(&emoji_set)).unwrap().to_vec();
        c.eq("plane 1 bitmap", (wide.len(), wide.get(8192).copied()), (8192 + 1 + 8192, Some(1)));
        let emoji = 0x1f600 - 0x10000;
        c.eq("plane 1 bits", wide.get(8193 + emoji / 8).copied(), Some(0b11));
        c.eq("plane 0 of the emoji", wide[..8192].iter().all(|b| *b == 0), true);
        let wide_back = CFCharacterSetCreateWithBitmapRepresentation(None, Some(&CFData::from_bytes(&wide))).unwrap();
        c.eq(
            "plane 1 from bitmap",
            (member(&wide_back, '😀'), member(&wide_back, '😁'), member(&wide_back, '😂')),
            (true, true, false),
        );

        let m = CFCharacterSetCreateMutable(None).unwrap();
        CFCharacterSetAddCharactersInRange(Some(&m), cf_range('0' as CFIndex, 10));
        CFCharacterSetAddCharactersInString(Some(&m), Some(&s("xyz")));
        CFCharacterSetRemoveCharactersInString(Some(&m), Some(&s("5y")));
        CFCharacterSetRemoveCharactersInRange(Some(&m), cf_range('8' as CFIndex, 2));
        let has = |set: &CFCharacterSet| "0123456789xyz".chars().filter(|ch| member(set, *ch)).collect::<String>();
        c.eq("edited", has(&m), "0123467xz".to_string());
        CFCharacterSetUnion(Some(&m), Some(&range_set));
        c.eq("union", (has(&m), member(&m, 'a')), ("0123467xz".to_string(), true));
        CFCharacterSetIntersect(Some(&m), Some(&digits));
        c.eq("intersect", (has(&m), member(&m, 'a')), ("0123467".to_string(), false));
        CFCharacterSetInvert(Some(&m));
        c.eq("invert", (has(&m), member(&m, 'q')), ("589xyz".to_string(), true));
        let copy = CFCharacterSetCreateCopy(None, Some(&abc)).unwrap();
        c.eq("copy", (member(&copy, 'a'), CFEqual(Some(&copy), Some(&abc))), (true, true));
        let mutable_copy = CFCharacterSetCreateMutableCopy(None, Some(&abc)).unwrap();
        CFCharacterSetAddCharactersInString(Some(&mutable_copy), Some(&s("d")));
        c.eq("mutable copy", (member(&mutable_copy, 'd'), member(&abc, 'd')), (true, false));
        c.done();
    }
}

fn locale(identifier: &str) -> CFRetained<CFLocale> {
    CFLocaleCreate(None, Some(&s(identifier))).unwrap()
}

fn value(locale: &CFLocale, key: Option<&CFString>) -> Option<String> {
    let v = CFLocaleGetValue(locale, key);
    describe(v.as_deref())
}

#[test]
fn locales() {
    unsafe {
        let mut c = Checks::default();
        let us = locale("en_US");
        c.eq("type", CFGetTypeID(Some(&us)), CFLocale::type_id());
        c.eq("identifier", CFLocaleGetIdentifier(&us).map(|t| t.to_string()), Some("en_US".into()));
        unsafe {
            type Key<'a> = (&'a str, Option<&'a CFString>, Option<&'a str>, Option<&'a str>);
            let keys: [Key<'_>; 16] = [
                ("identifier", kCFLocaleIdentifier, Some("en_US"), Some("fr_FR")),
                ("language", kCFLocaleLanguageCode, Some("en"), Some("fr")),
                ("country", kCFLocaleCountryCode, Some("US"), Some("FR")),
                ("script", kCFLocaleScriptCode, None, None),
                ("variant", kCFLocaleVariantCode, None, None),
                ("decimal", kCFLocaleDecimalSeparator, Some("."), Some(",")),
                ("grouping", kCFLocaleGroupingSeparator, Some(","), Some("\u{202f}")),
                ("metric", kCFLocaleUsesMetricSystem, Some("false"), Some("true")),
                ("measurement", kCFLocaleMeasurementSystem, Some("U.S."), Some("Metric")),
                ("currency", kCFLocaleCurrencyCode, Some("USD"), Some("EUR")),
                ("symbol", kCFLocaleCurrencySymbol, Some("$"), Some("€")),
                ("calendar identifier", kCFLocaleCalendarIdentifier, Some("gregorian"), Some("gregorian")),
                ("quote begin", kCFLocaleQuotationBeginDelimiterKey, Some("\u{201c}"), Some("«")),
                ("quote end", kCFLocaleQuotationEndDelimiterKey, Some("\u{201d}"), Some("»")),
                ("alternate begin", kCFLocaleAlternateQuotationBeginDelimiterKey, Some("\u{2018}"), Some("«")),
                ("alternate end", kCFLocaleAlternateQuotationEndDelimiterKey, Some("\u{2019}"), Some("»")),
            ];
            let fr = locale("fr_FR");
            let separators = |id: &str| {
                (value(&locale(id), kCFLocaleDecimalSeparator), value(&locale(id), kCFLocaleGroupingSeparator))
            };
            let pairs = |d: &str, g: &str| (Some(d.to_string()), Some(g.to_string()));
            c.eq("fr_CA separators", separators("fr_CA"), pairs(",", "\u{a0}"));
            c.eq("de_CH separators", separators("de_CH"), pairs(".", "'"));
            c.eq("de_DE separators", separators("de_DE"), pairs(",", "."));
            c.eq("ar_EG separators", separators("ar_EG"), pairs("\u{66b}", "\u{66c}"));
            c.eq("en_FR separators", separators("en_FR"), pairs(",", "\u{202f}"));
            for (what, key, en_want, fr_want) in keys {
                c.eq(&format!("en_US {what}"), value(&us, key), en_want.map(String::from));
                c.eq(&format!("fr_FR {what}"), value(&fr, key), fr_want.map(String::from));
            }
            let calendar = CFLocaleGetValue(&us, kCFLocaleCalendar);
            c.eq(
                "calendar",
                calendar
                    .as_deref()
                    .and_then(|v| v.downcast_ref::<CFCalendar>())
                    .and_then(|c| CFCalendarGetIdentifier(c))
                    .map(|i| i.to_string()),
                Some("gregorian".into()),
            );
            let exemplar = CFLocaleGetValue(&fr, kCFLocaleExemplarCharacterSet);
            let exemplar = exemplar.as_deref().and_then(|v| v.downcast_ref::<CFCharacterSet>());
            c.eq(
                "exemplar",
                exemplar.map(|e| {
                    (
                        CFCharacterSetIsLongCharacterMember(e, 'é' as u32),
                        CFCharacterSetIsLongCharacterMember(e, 'ñ' as u32),
                    )
                }),
                Some((true, false)),
            );
            // A script that goes without saying leaves the identifier.
            let hant = locale("zh_Hant_TW");
            c.eq(
                "script dropped",
                (CFLocaleGetIdentifier(&hant).map(|t| t.to_string()), value(&hant, kCFLocaleScriptCode)),
                (Some("zh_TW".into()), None),
            );
            let latin = locale("sr_Latn_RS");
            c.eq(
                "script kept",
                (CFLocaleGetIdentifier(&latin).map(|t| t.to_string()), value(&latin, kCFLocaleScriptCode)),
                (Some("sr-Latn_RS".into()), Some("Latn".into())),
            );
            // Exemplar characters, by language, script and region.
            let letters = |id: &str, chars: &str| {
                let v = CFLocaleGetValue(&locale(id), kCFLocaleExemplarCharacterSet);
                let set = v.as_deref().and_then(|v| v.downcast_ref::<CFCharacterSet>());
                chars
                    .chars()
                    .map(|ch| set.is_some_and(|s| CFCharacterSetIsLongCharacterMember(s, ch as u32)))
                    .collect::<Vec<_>>()
            };
            c.eq("english letters", letters("en_US", "aZé"), vec![true, true, false]);
            c.eq("german letters", letters("de_DE", "ßä"), vec![true, true]);
            c.eq("swiss german letters", letters("de_CH", "ßä"), vec![false, true]);
            c.eq("serbian letters", letters("sr_RS", "жa"), vec![true, false]);
            c.eq("latin serbian letters", letters("sr_Latn_RS", "жaž"), vec![false, true, true]);
            c.eq("montenegrin serbian letters", letters("sr_ME", "жa"), vec![false, true]);
            c.eq("chinese letters", letters("zh_CN", "国國"), vec![true, false]);
            c.eq("taiwanese letters", letters("zh_TW", "国國"), vec![false, true]);
            c.eq("japanese letters", letters("ja_JP", "あア国"), vec![true, true, true]);
            c.eq("unknown language letters", letters("xx", "aZ"), vec![true, true]);

            // Display names, in English.
            let names = [
                (kCFLocaleIdentifier, "fr_FR", Some("French (France)")),
                (kCFLocaleIdentifier, "zh_Hant_TW", Some("Chinese, Traditional (Taiwan)")),
                (kCFLocaleLanguageCode, "de", Some("German")),
                (kCFLocaleCountryCode, "JP", Some("Japan")),
                (kCFLocaleScriptCode, "Cyrl", Some("Cyrillic")),
                (kCFLocaleCurrencyCode, "EUR", Some("Euro")),
                (kCFLocaleCurrencySymbol, "EUR", Some("€")),
                (kCFLocaleCurrencySymbol, "CAD", Some("CA$")),
                (kCFLocaleCurrencySymbol, "CHF", Some("CHF")),
                (kCFLocaleIdentifier, "sr_Latn_RS", Some("Serbian (Latin, Serbia)")),
                (kCFLocaleIdentifier, "zh_Hans", Some("Chinese, Simplified")),
                (kCFLocaleIdentifier, "en_Hant", Some("English (Traditional)")),
                (kCFLocaleIdentifier, "en_US_POSIX", Some("English (United States, Computer)")),
                (kCFLocaleCalendarIdentifier, "gregorian", Some("Gregorian Calendar")),
                (kCFLocaleLanguageCode, "qqq", None),
            ];
            for (key, v, want) in names {
                c.eq(
                    &format!("display name of {v}"),
                    CFLocaleCopyDisplayNameForPropertyValue(&us, key, Some(&s(v))).map(|t| t.to_string()),
                    want.map(String::from),
                );
            }
        }

        let copy = CFLocaleCreateCopy(None, Some(&us)).unwrap();
        c.eq("copy", CFLocaleGetIdentifier(&copy).map(|t| t.to_string()), Some("en_US".into()));
        c.eq(
            "system",
            CFLocaleGetSystem().and_then(|l| CFLocaleGetIdentifier(&l)).map(|t| t.to_string()),
            Some(String::new()),
        );
        c.eq("current", CFLocaleCopyCurrent().is_some(), true);

        let languages = texts(CFLocaleCopyISOLanguageCodes().as_deref());
        c.eq("languages", ["en", "fr", "zh", "haw"].iter().all(|l| languages.iter().any(|x| x == l)), true);
        let countries = texts(CFLocaleCopyISOCountryCodes().as_deref());
        c.eq("countries", ["US", "FR", "JP"].iter().all(|l| countries.iter().any(|x| x == l)), true);
        let currencies = texts(CFLocaleCopyISOCurrencyCodes().as_deref());
        c.eq("currencies", ["USD", "EUR", "JPY"].iter().all(|l| currencies.iter().any(|x| x == l)), true);
        let common = texts(CFLocaleCopyCommonISOCurrencyCodes().as_deref());
        c.eq("common currencies", (common.iter().any(|x| x == "USD"), common.len() < currencies.len()), (true, true));
        let available = texts(CFLocaleCopyAvailableLocaleIdentifiers().as_deref());
        c.eq("available", ["en_US", "fr_FR", "ja_JP"].iter().all(|l| available.iter().any(|x| x == l)), true);
        c.eq("preferred", CFLocaleCopyPreferredLanguages().map(|a| CFArrayGetCount(&a) > 0), Some(true));

        let canonical = [
            ("en-us", "en-US", "en-US"),
            ("EN_us", "en-US", "en_US"),
            ("zh-hant-tw", "zh-Hant-TW", "zh-Hant-TW"),
            ("iw", "he", "he"),
            ("en_US@calendar=japanese", "en-US@calendar=japanese", "en_US@calendar=japanese"),
            ("es-419", "es-419", "es-419"),
            ("zh_Hant_TW", "zh-Hant-TW", "zh_TW"),
            ("zh_Hant-TW", "zh-Hant-TW", "zh-Hant-TW"),
            ("zh_Hans", "zh-Hans", "zh-Hans"),
            ("sr_Cyrl_RS", "sr-RS", "sr_RS"),
            ("sr_Latn_RS", "sr-Latn-RS", "sr-Latn_RS"),
            ("en_Hans_CN", "en-Hans-CN", "en_CN"),
            ("ja_Jpan_JP", "ja-JP", "ja_JP"),
            ("yue_Hant_HK", "yue-Hant-HK", "yue_HK"),
        ];
        for (input, language, locale_id) in canonical {
            c.eq(
                &format!("canonical language {input}"),
                CFLocaleCreateCanonicalLanguageIdentifierFromString(None, Some(&s(input))).map(|t| t.to_string()),
                Some(language.to_string()),
            );
            c.eq(
                &format!("canonical locale {input}"),
                CFLocaleCreateCanonicalLocaleIdentifierFromString(None, Some(&s(input))).map(|t| t.to_string()),
                Some(locale_id.to_string()),
            );
        }
        c.eq(
            "script manager",
            [(0, 0), (1, 1), (2, 3)].map(|(l, r)| {
                CFLocaleCreateCanonicalLocaleIdentifierFromScriptManagerCodes(None, l, r).map(|t| t.to_string())
            }),
            [Some("en_US".into()), Some("fr_FR".into()), Some("de_DE".into())],
        );

        let components =
            CFLocaleCreateComponentsFromLocaleIdentifier(None, Some(&s("zh_Hant_TW@calendar=japanese"))).unwrap();
        let component = |key: &str| {
            let key = s(key);
            let v = unsafe { CFDictionaryGetValue(&components, (&raw const *key).cast()) };
            (!v.is_null()).then(|| unsafe { &*v.cast::<CFString>() }.to_string())
        };
        c.eq(
            "components",
            (
                CFDictionaryGetCount(&components),
                component("kCFLocaleLanguageCodeKey"),
                component("kCFLocaleScriptCodeKey"),
                component("kCFLocaleCountryCodeKey"),
                component("calendar"),
            ),
            (4, Some("zh".into()), Some("Hant".into()), Some("TW".into()), Some("japanese".into())),
        );
        let back = CFLocaleCreateLocaleIdentifierFromComponents(None, Some(&components)).map(|t| t.to_string());
        c.eq("from components", back, Some("zh_Hant_TW@calendar=japanese".into()));
        // Components come out in canonical case; keywords' names lowercase.
        let all = |id: &str| {
            let d = CFLocaleCreateComponentsFromLocaleIdentifier(None, Some(&s(id))).unwrap();
            let n = CFDictionaryGetCount(&d) as usize;
            let (mut keys, mut values) = (vec![std::ptr::null(); n], vec![std::ptr::null(); n]);
            unsafe { CFDictionaryGetKeysAndValues(&d, keys.as_mut_ptr(), values.as_mut_ptr()) };
            let text = |p: *const c_void| unsafe { &*p.cast::<CFString>() }.to_string();
            let mut pairs: Vec<(String, String)> =
                keys.iter().zip(&values).map(|(&k, &v)| (text(k), text(v))).collect();
            pairs.sort();
            pairs
        };
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        c.eq(
            "components' case",
            all("zh-hant-tw"),
            vec![
                pair("kCFLocaleCountryCodeKey", "TW"),
                pair("kCFLocaleLanguageCodeKey", "zh"),
                pair("kCFLocaleScriptCodeKey", "Hant"),
            ],
        );
        c.eq(
            "components' case 2",
            all("EN_us"),
            vec![pair("kCFLocaleCountryCodeKey", "US"), pair("kCFLocaleLanguageCodeKey", "en")],
        );
        c.eq(
            "components' variant and keyword",
            all("en-US-posix@Currency=usd"),
            vec![
                pair("currency", "usd"),
                pair("kCFLocaleCountryCodeKey", "US"),
                pair("kCFLocaleLanguageCodeKey", "en"),
                pair("kCFLocaleVariantCodeKey", "POSIX"),
            ],
        );
        // A currency keyword's currency wins over the region's.
        for (id, code, symbol) in
            [("en_US@currency=EUR", "EUR", "€"), ("de_DE@currency=USD", "USD", "$"), ("ja_JP@currency=gbp", "GBP", "£")]
        {
            let l = CFLocale::new(None, Some(&s(id))).unwrap();
            let get = |k: Option<&CFString>| describe(l.value(k).as_deref());
            unsafe {
                c.eq(
                    &format!("{id} currency"),
                    (get(kCFLocaleCurrencyCode), get(kCFLocaleCurrencySymbol)),
                    (Some(code.into()), Some(symbol.into())),
                );
            }
        }
        // A locale's calendar is there, whichever it is.
        let buddhist = CFLocale::new(None, Some(&s("en_US@calendar=buddhist"))).unwrap();
        c.eq(
            "a Buddhist locale's calendar",
            unsafe { buddhist.value(kCFLocaleCalendar) }.map(|v| CFGetTypeID(Some(&v))),
            Some(CFCalendar::type_id()),
        );

        c.eq("windows code", CFLocaleGetWindowsLocaleCodeFromLocaleIdentifier(Some(&s("en_US"))), 0x409);
        c.eq("windows code fr", CFLocaleGetWindowsLocaleCodeFromLocaleIdentifier(Some(&s("fr_FR"))), 0x40c);
        c.eq(
            "from windows code",
            CFLocaleCreateLocaleIdentifierFromWindowsLocaleCode(None, 0x407).map(|t| t.to_string()),
            Some("de_DE".into()),
        );
        let direction = |l: &str| {
            (CFLocaleGetLanguageCharacterDirection(Some(&s(l))).0, CFLocaleGetLanguageLineDirection(Some(&s(l))).0)
        };
        c.eq("directions", [direction("en"), direction("ar"), direction("he")], [(1, 3), (2, 3), (2, 3)]);
        c.done();
    }
}

/// 2024-03-15 13:45:30 GMT.
const AT: CFAbsoluteTime = 8474.0 * 86400.0 + 13.0 * 3600.0 + 45.0 * 60.0 + 30.0;

fn gmt_calendar(identifier: Option<&CFString>) -> CFRetained<CFCalendar> {
    let calendar = CFCalendarCreateWithIdentifier(None, identifier).unwrap();
    let gmt = objc2_foundation::NSTimeZone::timeZoneWithName(&objc2_foundation::NSString::from_str("GMT")).unwrap();
    let gmt: &CFTimeZone = unsafe { &*(&raw const *gmt).cast() };
    CFCalendarSetTimeZone(&calendar, Some(gmt));
    unsafe { CFCalendarSetLocale(&calendar, Some(&locale("en_US"))) };
    calendar
}

#[test]
fn calendars() {
    unsafe {
        let mut c = Checks::default();
        let calendar = gmt_calendar(unsafe { kCFGregorianCalendar });
        c.eq("type", CFGetTypeID(Some(&calendar)), CFCalendar::type_id());
        c.eq("identifier", CFCalendarGetIdentifier(&calendar).map(|i| i.to_string()), Some("gregorian".into()));
        c.eq(
            "locale",
            CFCalendarCopyLocale(&calendar).and_then(|l| CFLocaleGetIdentifier(&l)).map(|i| i.to_string()),
            Some("en_US".into()),
        );
        c.eq("time zone", CFCalendarCopyTimeZone(&calendar).is_some(), true);
        c.eq(
            "week settings",
            (CFCalendarGetFirstWeekday(&calendar), CFCalendarGetMinimumDaysInFirstWeek(&calendar)),
            (1, 1),
        );
        let unit = CFCalendarUnit;
        let (era, year, month, day, hour, minute, second) =
            (unit(2), unit(4), unit(8), unit(16), unit(32), unit(64), unit(128));
        let (weekday, weekday_ordinal, quarter, week_of_month, week_of_year, year_for_week, day_of_year) =
            (unit(512), unit(1024), unit(2048), unit(4096), unit(8192), unit(16384), unit(65536));
        let ordinal = |small, big| CFCalendarGetOrdinalityOfUnit(&calendar, small, big, AT);
        c.eq(
            "ordinalities",
            [
                ordinal(year, era),
                ordinal(month, year),
                ordinal(month, quarter),
                ordinal(day, era),
                ordinal(day, year),
                ordinal(day, month),
                ordinal(hour, day),
                ordinal(hour, era),
                ordinal(minute, hour),
                ordinal(second, minute),
                ordinal(weekday, month),
                ordinal(week_of_year, year),
                ordinal(week_of_month, month),
                ordinal(quarter, year),
                ordinal(day, year_for_week),
            ],
            [2024, 3, 3, 738_960, 75, 15, 14, 17_735_030, 46, 31, 3, 11, 3, 1, 76],
        );
        c.eq("undefined ordinality", ordinal(week_of_year, day), -1);
        let in_ = |small, big| pair(CFCalendarGetRangeOfUnit(&calendar, small, big, AT));
        c.eq(
            "ranges",
            [
                in_(day, month),
                in_(day, year),
                in_(month, year),
                in_(hour, day),
                in_(weekday, week_of_year),
                in_(week_of_month, month),
            ],
            [(1, 31), (1, 366), (1, 12), (0, 24), (1, 7), (1, 6)],
        );
        c.eq(
            "minimum ranges",
            [day, month, hour, weekday, week_of_year].map(|u| pair(CFCalendarGetMinimumRangeOfUnit(&calendar, u))),
            [(1, 28), (1, 12), (0, 24), (1, 7), (1, 52)],
        );
        c.eq(
            "maximum ranges",
            [day, month, hour, weekday, week_of_year, weekday_ordinal]
                .map(|u| pair(CFCalendarGetMaximumRangeOfUnit(&calendar, u))),
            [(1, 31), (1, 12), (0, 24), (1, 7), (1, 53), (1, 5)],
        );
        let time_range = |u| {
            let (mut start, mut length) = (0.0, 0.0);
            let ok = unsafe { CFCalendarGetTimeRangeOfUnit(&calendar, u, AT, &mut start, &mut length) };
            (ok, start, length)
        };
        let midnight = 8474.0 * 86400.0;
        c.eq("day", time_range(day), (true, midnight, 86400.0));
        c.eq("hour", time_range(hour), (true, midnight + 13.0 * 3600.0, 3600.0));
        c.eq("month", time_range(month), (true, midnight - 14.0 * 86400.0, 31.0 * 86400.0));
        c.eq("week", time_range(week_of_year), (true, midnight - 5.0 * 86400.0, 7.0 * 86400.0));
        c.eq("day of year", time_range(day_of_year).0, true);

        CFCalendarSetFirstWeekday(&calendar, 2);
        CFCalendarSetMinimumDaysInFirstWeek(&calendar, 4);
        c.eq(
            "week settings set",
            (CFCalendarGetFirstWeekday(&calendar), CFCalendarGetMinimumDaysInFirstWeek(&calendar)),
            (2, 4),
        );
        c.eq("week with monday first", (ordinal(week_of_year, year), ordinal(weekday, week_of_year)), (11, 5));
        c.eq("week start with monday first", time_range(week_of_year).1, midnight - 4.0 * 86400.0);

        let iso = gmt_calendar(unsafe { kCFISO8601Calendar });
        c.eq("iso8601", CFCalendarGetIdentifier(&iso).map(|i| i.to_string()), Some("iso8601".into()));
        c.eq(
            "iso8601 week settings",
            (CFCalendarGetFirstWeekday(&iso), CFCalendarGetMinimumDaysInFirstWeek(&iso)),
            (2, 4),
        );
        c.eq("current", CFCalendarCopyCurrent().is_some(), true);
        c.done();
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn url_path(url: Option<CFRetained<CFURL>>) -> Option<String> {
    let url = url?;
    let absolute = CFURLCopyAbsoluteURL(&url)?;
    let path = CFURLCopyFileSystemPath(&absolute, CFURLPathStyle::CFURLPOSIXPathStyle)?;
    Some(path.to_string())
}

fn file_name(url: &CFURL) -> String {
    CFURLCopyLastPathComponent(url).map(|n| n.to_string()).unwrap_or_default()
}

fn names(urls: Option<CFRetained<CFArray>>) -> Vec<String> {
    let Some(urls) = urls else { return Vec::new() };
    let mut out: Vec<String> = (0..CFArrayGetCount(&urls))
        .map(|i| file_name(unsafe { &*CFArrayGetValueAtIndex(&urls, i).cast::<CFURL>() }))
        .collect();
    out.sort();
    out
}

#[test]
fn bundles() {
    unsafe {
        let mut c = Checks::default();
        let dir = scratch("bundle");
        let root = dir.join("Sample.bundle");
        let contents = root.join("Contents");
        write(
            &contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>org.sidestep.conformance.cf-types</string>
    <key>CFBundleVersion</key><string>1.2.3</string>
    <key>CFBundleShortVersionString</key><string>1.2</string>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleSignature</key><string>SDST</string>
    <key>CFBundleName</key><string>Sample</string>
    <key>Custom</key><string>value</string>
</dict>
</plist>
"#,
        );
        let resources = contents.join("Resources");
        write(&resources.join("a.txt"), "a");
        write(&resources.join("b.txt"), "b");
        write(&resources.join("c.dat"), "c");
        write(&resources.join("sub/d.txt"), "d");
        write(&resources.join("en.lproj/Localizable.strings"), "\"greeting\" = \"Hello\";\n\"only\" = \"English\";\n");
        write(&resources.join("fr.lproj/Localizable.strings"), "/* French */\n\"greeting\" = \"Bonjour\";\n");
        write(&resources.join("fr.lproj/l.txt"), "fr");
        write(&resources.join("Other.strings"), "\"k\" = \"from other\";\n");

        let url = CFURL::from_directory_path(&root).unwrap();
        let bundle = CFBundleCreate(None, Some(&url)).unwrap();
        c.eq("type", CFGetTypeID(Some(&bundle)), CFBundle::type_id());
        c.eq(
            "identifier",
            CFBundleGetIdentifier(&bundle).map(|i| i.to_string()),
            Some("org.sidestep.conformance.cf-types".into()),
        );
        c.eq("development region", CFBundleGetDevelopmentRegion(&bundle).map(|i| i.to_string()), Some("en".into()));
        let info_value = |key: &str| describe(CFBundleGetValueForInfoDictionaryKey(&bundle, Some(&s(key))).as_deref());
        c.eq("custom value", info_value("Custom"), Some("value".into()));
        c.eq("missing value", info_value("Nope"), None);
        unsafe {
            c.eq(
                "version key",
                describe(CFBundleGetValueForInfoDictionaryKey(&bundle, kCFBundleVersionKey).as_deref()),
                Some("1.2.3".into()),
            );
            c.eq(
                "name key",
                describe(CFBundleGetValueForInfoDictionaryKey(&bundle, kCFBundleNameKey).as_deref()),
                Some("Sample".into()),
            );
        }
        c.eq("info dictionary", CFBundleGetInfoDictionary(&bundle).map(|d| CFDictionaryGetCount(&d) >= 8), Some(true));
        c.eq("version number", CFBundleGetVersionNumber(&bundle), 0x0123_8000);
        let (mut package, mut creator) = (0u32, 0u32);
        unsafe { CFBundleGetPackageInfo(&bundle, &mut package, &mut creator) };
        c.eq("package info", (package.to_be_bytes(), creator.to_be_bytes()), (*b"BNDL", *b"SDST"));
        let (mut package, mut creator) = (0u32, 0u32);
        let found = unsafe { CFBundleGetPackageInfoInDirectory(Some(&url), &mut package, &mut creator) };
        c.eq("package info in directory", (found, package.to_be_bytes()), (true, *b"BNDL"));
        let root_path = std::fs::canonicalize(&root).unwrap().to_string_lossy().into_owned();
        let canonical =
            |p: Option<String>| p.and_then(|p| std::fs::canonicalize(p).ok()).map(|p| p.to_string_lossy().into_owned());
        c.eq("bundle url", canonical(url_path(CFBundleCopyBundleURL(&bundle))), Some(root_path.clone()));
        c.eq(
            "support files",
            canonical(url_path(CFBundleCopySupportFilesDirectoryURL(&bundle))),
            Some(format!("{root_path}/Contents")),
        );
        c.eq(
            "resources",
            canonical(url_path(CFBundleCopyResourcesDirectoryURL(&bundle))),
            Some(format!("{root_path}/Contents/Resources")),
        );
        c.eq("no executable", CFBundleCopyExecutableURL(&bundle).is_none(), true);
        c.eq("not loaded", CFBundleIsExecutableLoaded(&bundle), false);
        c.eq(
            "no architectures",
            CFBundleCopyExecutableArchitectures(&bundle).map(|a| CFArrayGetCount(&a)).unwrap_or(0),
            0,
        );

        let resource = |name: &str, kind: Option<&str>, sub: Option<&str>| {
            canonical(url_path(CFBundleCopyResourceURL(
                &bundle,
                Some(&s(name)),
                kind.map(s).as_deref(),
                sub.map(s).as_deref(),
            )))
        };
        c.eq("resource a", resource("a", Some("txt"), None), Some(format!("{root_path}/Contents/Resources/a.txt")));
        c.eq(
            "resource with extension",
            resource("a.txt", None, None),
            Some(format!("{root_path}/Contents/Resources/a.txt")),
        );
        c.eq(
            "resource in sub",
            resource("d", Some("txt"), Some("sub")),
            Some(format!("{root_path}/Contents/Resources/sub/d.txt")),
        );
        c.eq("missing resource", resource("zz", Some("txt"), None), None);
        c.eq(
            "of type",
            names(CFBundleCopyResourceURLsOfType(&bundle, Some(&s("txt")), None)),
            vec!["a.txt".to_string(), "b.txt".into()],
        );
        c.eq(
            "of type in sub",
            names(CFBundleCopyResourceURLsOfType(&bundle, Some(&s("txt")), Some(&s("sub")))),
            vec!["d.txt".to_string()],
        );
        c.eq(
            "for localization",
            canonical(url_path(CFBundleCopyResourceURLForLocalization(
                &bundle,
                Some(&s("Localizable")),
                Some(&s("strings")),
                None,
                Some(&s("fr")),
            ))),
            Some(format!("{root_path}/Contents/Resources/fr.lproj/Localizable.strings")),
        );
        c.eq(
            "of type for localization",
            names(CFBundleCopyResourceURLsOfTypeForLocalization(&bundle, Some(&s("txt")), None, Some(&s("fr")))),
            vec!["a.txt".to_string(), "b.txt".into(), "l.txt".into()],
        );
        c.eq(
            "in directory",
            canonical(url_path(CFBundleCopyResourceURLInDirectory(Some(&url), Some(&s("c")), Some(&s("dat")), None))),
            Some(format!("{root_path}/Contents/Resources/c.dat")),
        );
        c.eq(
            "of type in directory",
            names(CFBundleCopyResourceURLsOfTypeInDirectory(Some(&url), Some(&s("dat")), None)),
            vec!["c.dat".to_string()],
        );

        let mut localizations = texts(CFBundleCopyBundleLocalizations(&bundle).as_deref());
        localizations.sort();
        c.eq("localizations", localizations, vec!["en".to_string(), "fr".into()]);
        let mut for_url = texts(CFBundleCopyLocalizationsForURL(Some(&url)).as_deref());
        for_url.sort();
        c.eq("localizations for url", for_url, vec!["en".to_string(), "fr".into()]);
        let strings = |list: &[&str]| {
            let items: Vec<CFRetained<CFString>> = list.iter().map(|t| s(t)).collect();
            let refs: Vec<&CFString> = items.iter().map(|t| &**t).collect();
            CFArray::from_objects(&refs)
        };
        c.eq(
            "for preferences",
            texts(
                CFBundleCopyLocalizationsForPreferences(
                    Some(strings(&["en", "fr"]).as_opaque()),
                    Some(strings(&["fr", "en"]).as_opaque()),
                )
                .as_deref(),
            ),
            vec!["fr".to_string()],
        );
        c.eq(
            "for preferences, a region",
            texts(
                CFBundleCopyLocalizationsForPreferences(
                    Some(strings(&["en", "fr"]).as_opaque()),
                    Some(strings(&["fr-CA"]).as_opaque()),
                )
                .as_deref(),
            ),
            vec!["fr".to_string()],
        );
        let localized = |key: &str, table: Option<&str>, localizations: &[&str]| {
            CFBundleCopyLocalizedStringForLocalizations(
                &bundle,
                Some(&s(key)),
                Some(&s("fallback")),
                table.map(s).as_deref(),
                Some(strings(localizations).as_opaque()),
            )
            .map(|t| t.to_string())
        };
        c.eq("localized fr", localized("greeting", None, &["fr"]), Some("Bonjour".into()));
        c.eq("localized en", localized("greeting", None, &["en"]), Some("Hello".into()));
        c.eq("localized missing", localized("absent", None, &["fr"]), Some("fallback".into()));
        c.eq("other table", localized("k", Some("Other"), &["fr"]), Some("from other".into()));

        let info = CFBundleCopyInfoDictionaryForURL(Some(&url));
        c.eq("info for url", info.map(|d| CFDictionaryGetCount(&d)), Some(8));
        let info = CFBundleCopyInfoDictionaryInDirectory(Some(&url));
        c.eq("info in directory", info.is_some(), true);
        c.eq(
            "by identifier",
            CFBundleGetBundleWithIdentifier(Some(&s("org.sidestep.conformance.cf-types")))
                .map(|b| std::ptr::eq(&*b, &*bundle)),
            Some(true),
        );
        c.eq("main bundle", CFBundleGetMainBundle().is_some(), true);
        c.eq(
            "bundles in directory",
            CFBundleCreateBundlesFromDirectory(
                None,
                Some(&CFURL::from_directory_path(&dir).unwrap()),
                Some(&s("bundle")),
            )
            .map(|a| CFArrayGetCount(&a)),
            Some(1),
        );
        c.eq("no function", unsafe { CFBundleGetFunctionPointerForName(&bundle, Some(&s("nothing"))) }.is_null(), true);
        let _ = std::fs::remove_dir_all(&dir);
        c.done();
    }
}

/// Where a bundle looks for a resource: a localization named, only in its
/// own `.lproj` (after the resource directory); none, in `Base.lproj`, the
/// preferred localization's and the development region's, English's under
/// both its names.
#[test]
fn bundle_resource_search() {
    let mut c = Checks::default();
    let dir = scratch("bundle-search");
    let root = dir.join("Search.bundle");
    let contents = root.join("Contents");
    write(
        &contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>org.sidestep.conformance.bundle-search</string>
<key>CFBundleDevelopmentRegion</key><string>de</string>
</dict></plist>"#,
    );
    let r = contents.join("Resources");
    write(&r.join("top.txt"), "top");
    write(&r.join("Base.lproj/only_base.txt"), "Base");
    write(&r.join("English.lproj/only_english.txt"), "English");
    for l in ["en", "de", "fr", "fr_CA"] {
        write(&r.join(format!("{l}.lproj/only_{l}.txt")), l);
    }
    for l in ["en", "de", "fr"] {
        write(&r.join(format!("{l}.lproj/both.txt")), l);
    }
    let bundle = CFBundleCreate(None, Some(&CFURL::from_directory_path(&root).unwrap())).unwrap();
    let read = |u: Option<CFRetained<CFURL>>| {
        u.and_then(|u| CFURLCopyFileSystemPath(&u, CFURLPathStyle::CFURLPOSIXPathStyle))
            .and_then(|p| std::fs::read_to_string(p.to_string()).ok())
    };
    let default = |name: &str| read(CFBundleCopyResourceURL(&bundle, Some(&s(name)), Some(&s("txt")), None));
    let for_localization = |name: &str, l: &str| {
        read(unsafe {
            CFBundleCopyResourceURLForLocalization(&bundle, Some(&s(name)), Some(&s("txt")), None, Some(&s(l)))
        })
    };
    // The preferred localization is English on the machines the tests run
    // on.
    c.eq("top", default("top"), Some("top".into()));
    c.eq("Base", default("only_base"), Some("Base".into()));
    c.eq("preferred", default("only_en"), Some("en".into()));
    c.eq("preferred's other name", default("only_english"), Some("English".into()));
    c.eq("development region", default("only_de"), Some("de".into()));
    c.eq("neither", default("only_fr"), None);
    c.eq("preferred first", default("both"), Some("en".into()));
    c.eq("named: top", for_localization("top", "fr"), Some("top".into()));
    c.eq("named: its own", for_localization("only_fr", "fr"), Some("fr".into()));
    c.eq("named: not Base", for_localization("only_base", "fr"), None);
    c.eq("named: not the development region", for_localization("only_de", "fr"), None);
    c.eq("named: region, not language", for_localization("only_fr", "fr_CA"), None);
    c.eq("named: region", for_localization("only_fr_CA", "fr_CA"), Some("fr_CA".into()));
    c.eq("named: exact name", for_localization("only_english", "en"), None);
    c.eq("named: both", for_localization("both", "fr"), Some("fr".into()));
    let _ = std::fs::remove_dir_all(&dir);
    c.done();
}

/// `.strings` files as macOS reads them: escapes, single quotes, bare
/// words and the `"key";` shorthand.
#[test]
fn strings_files() {
    let mut c = Checks::default();
    let dir = scratch("strings-files");
    let root = dir.join("Strings.bundle");
    let contents = root.join("Contents");
    write(
        &contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>org.sidestep.conformance.strings-files</string>
<key>CFBundleDevelopmentRegion</key><string>en</string>
</dict></plist>"#,
    );
    write(
        &contents.join("Resources/en.lproj/Localizable.strings"),
        concat!(
            "\"octal\" = \"\\101\\102\";\n",
            "\"short octal\" = \"\\7x\\12y\";\n",
            "\"NEXTSTEP\" = \"\\351\\200\\241\\376\";\n",
            "\"surrogates\" = \"\\UD83D\\UDE00!\";\n",
            "\"short U\" = \"\\U41x\";\n",
            "\"lower u\" = \"\\u00e9\";\n",
            "\"controls\" = \"\\a\\b\\f\\v\\n\\r\\t\";\n",
            "\"quote\" = \"\\\"\\\\\";\n",
            "\"unknown\" = \"\\q\";\n",
            "\"escaped newline\" = \"a\\\nb\";\n",
            "\"single\" = 'x \"y\"';\n",
            "bare = word.1;\n",
            "\"shorthand\";\n",
            "/* a comment */ \"after\" = \"ok\"; // and another\n",
        ),
    );
    let bundle = CFBundleCreate(None, Some(&CFURL::from_directory_path(&root).unwrap())).unwrap();
    let look = |key: &str| {
        unsafe { CFBundleCopyLocalizedString(&bundle, Some(&s(key)), Some(&s("MISSING")), None) }
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    c.eq("octal", look("octal"), "AB".into());
    c.eq("short octal", look("short octal"), "\u{7}x\ny".into());
    c.eq("NEXTSTEP", look("NEXTSTEP"), "\u{d8}\u{a0}\u{a1}\0".into());
    c.eq("surrogates", look("surrogates"), "\u{1f600}!".into());
    c.eq("short U", look("short U"), "Ax".into());
    c.eq("lower u", look("lower u"), "u00e9".into());
    c.eq("controls", look("controls"), "\u{7}\u{8}\u{c}\u{b}\n\r\t".into());
    c.eq("quote", look("quote"), "\"\\".into());
    c.eq("unknown", look("unknown"), "q".into());
    c.eq("escaped newline", look("escaped newline"), "a\nb".into());
    c.eq("single", look("single"), "x \"y\"".into());
    c.eq("bare", look("bare"), "word.1".into());
    c.eq("shorthand", look("shorthand"), "shorthand".into());
    c.eq("after", look("after"), "ok".into());
    let _ = std::fs::remove_dir_all(&dir);
    c.done();
}

unsafe extern "C-unwind" fn read_events(_s: *mut CFReadStream, event: CFStreamEventType, info: *mut c_void) {
    unsafe { (*info.cast::<Vec<usize>>()).push(event.0) };
}

unsafe extern "C-unwind" fn write_events(_s: *mut CFWriteStream, event: CFStreamEventType, info: *mut c_void) {
    unsafe { (*info.cast::<Vec<usize>>()).push(event.0) };
}

fn read(stream: &CFReadStream, n: usize) -> (CFIndex, String) {
    let mut buffer = vec![0u8; n];
    let got = unsafe { CFReadStreamRead(stream, buffer.as_mut_ptr(), n as CFIndex) };
    (got, String::from_utf8_lossy(&buffer[..got.max(0) as usize]).into_owned())
}

fn error(e: CFStreamError) -> (CFIndex, i32) {
    (e.domain, e.error)
}

fn number(value: Option<CFRetained<CFType>>) -> Option<i64> {
    describe(value.as_deref()).and_then(|t| t.parse().ok())
}

#[test]
fn file_and_memory_streams() {
    unsafe {
        let mut c = Checks::default();
        let dir = scratch("streams");
        let path = dir.join("f.txt");
        std::fs::write(&path, "hello world").unwrap();
        let url = CFURL::from_file_path(&path).unwrap();
        unsafe {
            let r = CFReadStreamCreateWithFile(None, Some(&url)).unwrap();
            c.eq(
                "types",
                (CFGetTypeID(Some(&r)), CFReadStream::type_id() != CFWriteStream::type_id()),
                (CFReadStream::type_id(), true),
            );
            c.eq("not open", (CFReadStreamGetStatus(&r).0, CFReadStreamHasBytesAvailable(&r)), (0, false));
            c.eq("open", (CFReadStreamOpen(&r), CFReadStreamGetStatus(&r).0), (true, 2));
            c.eq("open again", CFReadStreamOpen(&r), false);
            c.eq("read", read(&r, 4), (4, "hell".into()));
            c.eq("offset", number(CFReadStreamCopyProperty(&r, kCFStreamPropertyFileCurrentOffset)), Some(4));
            c.eq(
                "seek",
                CFReadStreamSetProperty(&r, kCFStreamPropertyFileCurrentOffset, Some(&CFNumber::new_i64(6))),
                true,
            );
            c.eq("read the rest", read(&r, 64), (5, "world".into()));
            c.eq("still open", (CFReadStreamGetStatus(&r).0, CFReadStreamHasBytesAvailable(&r)), (2, true));
            c.eq("at the end", read(&r, 64), (0, String::new()));
            c.eq("end", (CFReadStreamGetStatus(&r).0, CFReadStreamHasBytesAvailable(&r)), (5, false));
            let mut n = -1;
            c.eq("no buffer", (CFReadStreamGetBuffer(&r, 10, &mut n).is_null(), n), (true, 0));
            CFReadStreamClose(&r);
            c.eq("closed", (CFReadStreamGetStatus(&r).0, error(CFReadStreamGetError(&r))), (6, (0, 0)));
            c.eq("read when closed", read(&r, 4).0, -1);

            let r = CFReadStreamCreateWithFile(None, Some(&url)).unwrap();
            c.eq(
                "seek before open",
                CFReadStreamSetProperty(&r, kCFStreamPropertyFileCurrentOffset, Some(&CFNumber::new_i64(3))),
                true,
            );
            c.eq(
                "offset before open",
                number(CFReadStreamCopyProperty(&r, kCFStreamPropertyFileCurrentOffset)),
                Some(3),
            );
            CFReadStreamOpen(&r);
            c.eq("read after the seek", read(&r, 4), (4, "lo w".into()));

            let missing =
                CFReadStreamCreateWithFile(None, Some(&CFURL::from_file_path(dir.join("missing")).unwrap())).unwrap();
            c.eq("missing", (CFReadStreamOpen(&missing), CFReadStreamGetStatus(&missing).0), (false, 7));
            c.eq("missing error", error(CFReadStreamGetError(&missing)), (1, 2));
            let copied = CFReadStreamCopyError(&missing);
            c.eq(
                "copied error",
                copied.map(|e| (CFErrorGetDomain(&e).map(|d| d.to_string()), CFErrorGetCode(&e))),
                Some((Some("NSPOSIXErrorDomain".into()), 2)),
            );
            c.eq("read missing", read(&missing, 4).0, -1);

            let bytes = b"abcdef";
            let m = CFReadStreamCreateWithBytesNoCopy(None, bytes.as_ptr(), 6, kCFAllocatorNull).unwrap();
            c.eq("read before open", (read(&m, 4).0, CFReadStreamGetStatus(&m).0), (-1, 0));
            CFReadStreamClose(&m);
            c.eq("closed before open", CFReadStreamGetStatus(&m).0, 0);
            c.eq("memory open", (CFReadStreamOpen(&m), CFReadStreamHasBytesAvailable(&m)), (true, true));
            let mut n = 0;
            let buffer = CFReadStreamGetBuffer(&m, 4, &mut n);
            c.eq(
                "memory buffer",
                (n, (!buffer.is_null()).then(|| std::slice::from_raw_parts(buffer, n as usize).to_vec())),
                (4, Some(b"abcd".to_vec())),
            );
            c.eq("memory read", read(&m, 64), (2, "ef".into()));
            c.eq("memory at end", CFReadStreamGetStatus(&m).0, 5);
            let empty = CFReadStreamCreateWithBytesNoCopy(None, bytes.as_ptr(), 0, kCFAllocatorNull).unwrap();
            c.eq(
                "empty",
                (CFReadStreamOpen(&empty), CFReadStreamHasBytesAvailable(&empty), read(&empty, 4).0),
                (true, false, 0),
            );
            c.eq("empty at end", CFReadStreamGetStatus(&empty).0, 5);

            let mut out = [0u8; 8];
            let w = CFWriteStreamCreateWithBuffer(None, out.as_mut_ptr(), 8).unwrap();
            c.eq("write type", CFGetTypeID(Some(&w)), CFWriteStream::type_id());
            c.eq("write before open", CFWriteStreamWrite(&w, b"1".as_ptr(), 1), -1);
            c.eq("buffer", (CFWriteStreamOpen(&w), CFWriteStreamCanAcceptBytes(&w)), (true, true));
            c.eq(
                "writes",
                (CFWriteStreamWrite(&w, b"12345".as_ptr(), 5), CFWriteStreamWrite(&w, b"6789".as_ptr(), 4)),
                (5, -1),
            );
            c.eq(
                "overflow",
                (CFWriteStreamGetStatus(&w).0, error(CFWriteStreamGetError(&w)), CFWriteStreamCanAcceptBytes(&w)),
                (7, (1, 12), false),
            );
            c.eq("written", &out[..6], b"12345\0");

            let a = CFWriteStreamCreateWithAllocatedBuffers(None, None).unwrap();
            CFWriteStreamOpen(&a);
            CFWriteStreamWrite(&a, b"hello ".as_ptr(), 6);
            CFWriteStreamWrite(&a, b"there".as_ptr(), 5);
            let data = CFWriteStreamCopyProperty(&a, kCFStreamPropertyDataWritten);
            let data = data.as_deref().and_then(|d| d.downcast_ref::<CFData>()).map(|d| d.to_vec());
            c.eq("allocated", data, Some(b"hello there".to_vec()));

            let f = CFWriteStreamCreateWithFile(None, Some(&url)).unwrap();
            c.eq(
                "append",
                CFWriteStreamSetProperty(&f, kCFStreamPropertyAppendToFile, kCFBooleanTrue.map(|b| &**b)),
                true,
            );
            c.eq(
                "unknown property",
                CFWriteStreamSetProperty(&f, Some(&s("whatever")), Some(&CFNumber::new_i64(3))),
                false,
            );
            CFWriteStreamOpen(&f);
            c.eq("append write", CFWriteStreamWrite(&f, b"!!".as_ptr(), 2), 2);
            CFWriteStreamClose(&f);
            c.eq("appended", std::fs::read_to_string(&path).unwrap(), "hello world!!".to_string());
            let f = CFWriteStreamCreateWithFile(None, Some(&url)).unwrap();
            CFWriteStreamOpen(&f);
            CFWriteStreamWrite(&f, b"new".as_ptr(), 3);
            c.eq("write offset", number(CFWriteStreamCopyProperty(&f, kCFStreamPropertyFileCurrentOffset)), Some(3));
            CFWriteStreamClose(&f);
            c.eq("replaced", std::fs::read_to_string(&path).unwrap(), "new".to_string());
            let d = CFWriteStreamCreateWithFile(None, Some(&CFURL::from_directory_path(&dir).unwrap())).unwrap();
            c.eq("directory", (CFWriteStreamOpen(&d), CFWriteStreamGetStatus(&d).0), (false, 7));
        }
        let _ = std::fs::remove_dir_all(&dir);
        c.done();
    }
}

fn bound_pair(size: CFIndex) -> (CFRetained<CFReadStream>, CFRetained<CFWriteStream>) {
    let mut r: *mut CFReadStream = std::ptr::null_mut();
    let mut w: *mut CFWriteStream = std::ptr::null_mut();
    unsafe {
        CFStreamCreateBoundPair(None, &mut r, &mut w, size);
        (CFRetained::from_raw(NonNull::new(r).unwrap()), CFRetained::from_raw(NonNull::new(w).unwrap()))
    }
}

#[test]
fn bound_pairs() {
    unsafe {
        let mut c = Checks::default();
        unsafe {
            let (r, w) = bound_pair(4);
            c.eq("open", (CFReadStreamOpen(&r), CFWriteStreamOpen(&w)), (true, true));
            c.eq("status", (CFReadStreamGetStatus(&r).0, CFWriteStreamGetStatus(&w).0), (2, 2));
            c.eq("empty", (CFReadStreamHasBytesAvailable(&r), CFWriteStreamCanAcceptBytes(&w)), (false, true));
            c.eq("write", (CFWriteStreamWrite(&w, b"abcdef".as_ptr(), 6), CFWriteStreamCanAcceptBytes(&w)), (4, false));
            c.eq("has bytes", CFReadStreamHasBytesAvailable(&r), true);
            c.eq("read", read(&r, 64), (4, "abcd".into()));
            c.eq("room again", CFWriteStreamCanAcceptBytes(&w), true);
            CFWriteStreamClose(&w);
            c.eq(
                "after the writer closed",
                (CFReadStreamHasBytesAvailable(&r), read(&r, 64).0, CFReadStreamGetStatus(&r).0),
                (true, 0, 5),
            );

            // A reader waits for the writer, on another thread.
            let (r, w) = bound_pair(16);
            CFReadStreamOpen(&r);
            CFWriteStreamOpen(&w);
            let writer = w.clone();
            struct Send(CFRetained<CFWriteStream>);
            unsafe impl std::marker::Send for Send {}
            let writer = Send(writer);
            let thread = std::thread::spawn(move || {
                let writer = writer;
                std::thread::sleep(Duration::from_millis(20));
                CFWriteStreamWrite(&writer.0, b"late".as_ptr(), 4);
                CFWriteStreamClose(&writer.0);
            });
            c.eq("waited for", read(&r, 64), (4, "late".into()));
            thread.join().unwrap();
            c.eq("then the end", read(&r, 64).0, 0);

            let (r, w) = bound_pair(8);
            CFReadStreamOpen(&r);
            CFWriteStreamOpen(&w);
            CFReadStreamClose(&r);
            c.eq(
                "reader closed",
                (
                    CFWriteStreamWrite(&w, b"ab".as_ptr(), 2),
                    CFWriteStreamGetStatus(&w).0,
                    error(CFWriteStreamGetError(&w)),
                ),
                (-1, 2, (0, 0)),
            );
        }
        c.done();
    }
}

#[test]
fn stream_clients() {
    unsafe {
        let mut c = Checks::default();
        unsafe {
            let bytes = b"abcdef";
            let r = CFReadStreamCreateWithBytesNoCopy(None, bytes.as_ptr(), 6, kCFAllocatorNull).unwrap();
            let mut events: Vec<usize> = Vec::new();
            let mut context = CFStreamClientContext {
                version: 0,
                info: (&raw mut events).cast(),
                retain: None,
                release: None,
                copyDescription: None,
            };
            c.eq("client", CFReadStreamSetClient(&r, 1 | 2 | 8 | 16, Some(read_events), &mut context), true);
            let run_loop = CFRunLoop::current().unwrap();
            CFReadStreamScheduleWithRunLoop(&r, Some(&run_loop), kCFRunLoopDefaultMode);
            CFReadStreamOpen(&r);
            let events_ptr: *const Vec<usize> = &raw const events;
            run_until(|| (*events_ptr).len() >= 2);
            c.eq("opened", events.clone(), vec![1, 2]);
            c.eq("read", read(&r, 3).0, 3);
            run_until(|| (*events_ptr).len() >= 3);
            c.eq("more bytes", events.clone(), vec![1, 2, 2]);
            c.eq("read the rest", read(&r, 3).0, 3);
            run_until(|| (*events_ptr).len() >= 4);
            c.eq("end", events.clone(), vec![1, 2, 2, 16]);
            CFReadStreamUnscheduleFromRunLoop(&r, Some(&run_loop), kCFRunLoopDefaultMode);
            CFReadStreamSetClient(&r, 0, None, std::ptr::null_mut());

            let w = CFWriteStreamCreateWithAllocatedBuffers(None, None).unwrap();
            let mut write_log: Vec<usize> = Vec::new();
            let mut context = CFStreamClientContext {
                version: 0,
                info: (&raw mut write_log).cast(),
                retain: None,
                release: None,
                copyDescription: None,
            };
            CFWriteStreamSetClient(&w, 1 | 4 | 8 | 16, Some(write_events), &mut context);
            CFWriteStreamScheduleWithRunLoop(&w, Some(&run_loop), kCFRunLoopDefaultMode);
            CFWriteStreamOpen(&w);
            let log_ptr: *const Vec<usize> = &raw const write_log;
            run_until(|| (*log_ptr).len() >= 2);
            c.eq("write opened", write_log.clone(), vec![1, 4]);
            CFWriteStreamWrite(&w, b"x".as_ptr(), 1);
            run_until(|| (*log_ptr).len() >= 3);
            c.eq("written", write_log.clone(), vec![1, 4, 4]);
            CFWriteStreamUnscheduleFromRunLoop(&w, Some(&run_loop), kCFRunLoopDefaultMode);
            CFWriteStreamSetClient(&w, 0, None, std::ptr::null_mut());
        }
        c.done();
    }
}

#[test]
fn socket_streams() {
    unsafe {
        let mut c = Checks::default();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut socket, _) = listener.accept().unwrap();
            let mut got = [0u8; 4];
            socket.read_exact(&mut got).unwrap();
            socket.write_all(b"pong").unwrap();
            got
        });
        unsafe {
            let mut r: *mut CFReadStream = std::ptr::null_mut();
            let mut w: *mut CFWriteStream = std::ptr::null_mut();
            CFStreamCreatePairWithSocketToHost(None, Some(&s("127.0.0.1")), u32::from(port), &mut r, &mut w);
            let (r, w) =
                (CFRetained::from_raw(NonNull::new(r).unwrap()), CFRetained::from_raw(NonNull::new(w).unwrap()));
            c.eq("not open", (CFReadStreamGetStatus(&r).0, CFWriteStreamGetStatus(&w).0), (0, 0));
            c.eq(
                "host",
                describe(CFReadStreamCopyProperty(&r, kCFStreamPropertySocketRemoteHostName).as_deref()),
                Some("127.0.0.1".into()),
            );
            c.eq("open", (CFReadStreamOpen(&r), CFWriteStreamOpen(&w)), (true, true));
            run_until(|| CFWriteStreamGetStatus(&w).0 == 2 && CFReadStreamGetStatus(&r).0 == 2);
            c.eq("connected", (CFReadStreamGetStatus(&r).0, CFWriteStreamGetStatus(&w).0), (2, 2));
            c.eq(
                "port",
                number(CFReadStreamCopyProperty(&r, kCFStreamPropertySocketRemotePortNumber)),
                Some(i64::from(port)),
            );
            c.eq("send", CFWriteStreamWrite(&w, b"ping".as_ptr(), 4), 4);
            let mut got = String::new();
            let start = Instant::now();
            while got.len() < 4 && start.elapsed() < Duration::from_secs(10) {
                let (n, text) = read(&r, 4 - got.len());
                if n <= 0 {
                    break;
                }
                got.push_str(&text);
            }
            c.eq("received", got, "pong".to_string());
            CFReadStreamClose(&r);
            CFWriteStreamClose(&w);
        }
        c.eq("server got", server.join().unwrap(), *b"ping");
        c.done();
    }
}

#[test]
fn ports_and_file_security() {
    unsafe {
        let mut c = Checks::default();
        let remote = CFMessagePortCreateRemote(None, Some(&s("org.sidestep.conformance.no-such-port")));
        c.eq("no remote port", remote.is_none(), true);
        let security = CFFileSecurity::new(None).unwrap();
        c.eq("type", CFGetTypeID(Some(&security)), CFFileSecurity::type_id());
        c.eq("clear", CFFileSecurityClearProperties(&security, CFFileSecurityClearOptions(0x7f)), true);
        c.eq("clear nothing", CFFileSecurityClearProperties(&security, CFFileSecurityClearOptions(0)), true);
        let copy = CFFileSecurityCreateCopy(None, Some(&security)).unwrap();
        c.eq("copy", (std::ptr::eq(&*copy, &*security), CFEqual(Some(&copy), Some(&security))), (false, true));
        let fresh = CFFileSecurityCreate(None).unwrap();
        c.eq("fresh equal", CFEqual(Some(&fresh), Some(&security)), true);
        c.eq("port types differ", CFMachPort::type_id() != CFMessagePort::type_id(), true);
        c.done();
    }
}
