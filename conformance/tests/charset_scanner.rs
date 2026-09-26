//! NSCharacterSet, NSMutableCharacterSet and NSScanner. Expected values are
//! what macOS returns.

use objc2::rc::Retained;
use objc2_foundation::{
    NSCharacterSet, NSCopying, NSMutableCharacterSet, NSMutableCopying, NSRange, NSScanner, NSString,
};

use sidestep as _;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

/// Characters to ask every predefined set about.
const PROBES: [u32; 34] = [
    0x20, 0x09, 0x0A, 0x0D, 0x0B, 0x0C, 0xA0, 0x200B, 0x2028, 0x2029, 0x85, 0xFEFF, 0x30, 0x39, 0x663, 0xE9, 0xDF,
    0x6F22, 0x301, 0x1F389, 0x2D, 0x5F, 0x24, 0x2B, 0xA9, 0xD83D, 0x41, 0x61, 0x1C5, 0x0, 0x7F, 0xAD, 0xFFFE, 0x378,
];

fn members(set: &NSCharacterSet) -> String {
    PROBES.iter().map(|&c| if set.longCharacterIsMember(c) { '1' } else { '0' }).collect()
}

#[test]
fn predefined_sets() {
    let sets: [(Retained<NSCharacterSet>, &str); 15] = [
        (NSCharacterSet::controlCharacterSet(), "0111110100110000000000000000011100"),
        (NSCharacterSet::whitespaceCharacterSet(), "1100001100000000000000000000000000"),
        (NSCharacterSet::whitespaceAndNewlineCharacterSet(), "1111111111100000000000000000000000"),
        (NSCharacterSet::decimalDigitCharacterSet(), "0000000000001110000000000000000000"),
        (NSCharacterSet::letterCharacterSet(), "0000000000000001111000000011100000"),
        (NSCharacterSet::lowercaseLetterCharacterSet(), "0000000000000001100000000001000000"),
        (NSCharacterSet::uppercaseLetterCharacterSet(), "0000000000000000000000000010100000"),
        (NSCharacterSet::nonBaseCharacterSet(), "0000000000000000001000000000000000"),
        (NSCharacterSet::alphanumericCharacterSet(), "0000000000001111111000000011100000"),
        (NSCharacterSet::decomposableCharacterSet(), "0000000000000001000000000000000000"),
        (NSCharacterSet::illegalCharacterSet(), "0000000000000000000000000000000011"),
        (NSCharacterSet::punctuationCharacterSet(), "0000000000000000000011000000000000"),
        (NSCharacterSet::capitalizedLetterCharacterSet(), "0000000000000000000000000000100000"),
        (NSCharacterSet::symbolCharacterSet(), "0000000000000000000100111000000000"),
        (NSCharacterSet::newlineCharacterSet(), "0011110011100000000000000000000000"),
    ];
    for (k, (set, expected)) in sets.iter().enumerate() {
        assert_eq!(members(set), *expected, "predefined set {k}");
    }
    // characterIsMember: asks about one UTF-16 unit.
    let letters = NSCharacterSet::letterCharacterSet();
    assert!(letters.characterIsMember(0xE9));
    assert!(!letters.characterIsMember(0xD83D));
}

#[test]
fn url_component_sets() {
    let ascii = |set: &NSCharacterSet| {
        (0x20u32..0x7F)
            .filter(|&c| set.longCharacterIsMember(c))
            .map(|c| char::from_u32(c).unwrap())
            .collect::<String>()
    };
    let sub = "!$&'()*+,-.";
    let alpha = "ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~";
    assert_eq!(ascii(&NSCharacterSet::URLFragmentAllowedCharacterSet()), format!("{sub}/0123456789:;=?@{alpha}"));
    assert_eq!(ascii(&NSCharacterSet::URLQueryAllowedCharacterSet()), format!("{sub}/0123456789:;=?@{alpha}"));
    assert_eq!(ascii(&NSCharacterSet::URLPathAllowedCharacterSet()), format!("{sub}/0123456789:;=@{alpha}"));
    assert_eq!(
        ascii(&NSCharacterSet::URLHostAllowedCharacterSet()),
        format!("{sub}0123456789:;=ABCDEFGHIJKLMNOPQRSTUVWXYZ[]_abcdefghijklmnopqrstuvwxyz~")
    );
    assert_eq!(ascii(&NSCharacterSet::URLUserAllowedCharacterSet()), format!("{sub}0123456789;={alpha}"));
    assert_eq!(ascii(&NSCharacterSet::URLPasswordAllowedCharacterSet()), format!("{sub}0123456789;={alpha}"));
    assert!(!NSCharacterSet::URLPathAllowedCharacterSet().longCharacterIsMember(0xE9));
}

#[test]
fn built_sets() {
    let party = NSCharacterSet::characterSetWithCharactersInString(&s("🎉"));
    assert!(party.longCharacterIsMember(0x1F389));
    assert!(!party.characterIsMember(0xD83C) && !party.characterIsMember(0xDF89));
    assert!(party.hasMemberInPlane(1));
    let a = NSCharacterSet::characterSetWithCharactersInString(&s("a"));
    assert!(a.hasMemberInPlane(0) && !a.hasMemberInPlane(1) && !a.hasMemberInPlane(2));
    let r = NSCharacterSet::characterSetWithRange(NSRange::new(0x61, 3));
    assert!(r.longCharacterIsMember(0x61) && r.longCharacterIsMember(0x63) && !r.longCharacterIsMember(0x64));
    let inv = r.invertedSet();
    assert!(!inv.longCharacterIsMember(0x61) && inv.longCharacterIsMember(0x64) && inv.longCharacterIsMember(0x1F389));
    assert!(!party.invertedSet().longCharacterIsMember(0x1F389));
    assert!(NSCharacterSet::letterCharacterSet().isSupersetOfSet(&NSCharacterSet::lowercaseLetterCharacterSet()));
    assert!(!NSCharacterSet::lowercaseLetterCharacterSet().isSupersetOfSet(&NSCharacterSet::letterCharacterSet()));
    assert!(r.isSupersetOfSet(&NSCharacterSet::characterSetWithCharactersInString(&s(""))));
    assert!(r.isSupersetOfSet(&NSCharacterSet::characterSetWithCharactersInString(&s("ba"))));
}

#[test]
fn mutable_sets() {
    let m = NSMutableCharacterSet::new();
    m.addCharactersInRange(NSRange::new(0x61, 26));
    m.removeCharactersInString(&s("aeiou"));
    m.addCharactersInString(&s("🎉"));
    assert!(m.longCharacterIsMember(0x62) && !m.longCharacterIsMember(0x61) && m.longCharacterIsMember(0x1F389));
    m.formIntersectionWithCharacterSet(&NSCharacterSet::characterSetWithRange(NSRange::new(0x61, 5)));
    assert!(m.longCharacterIsMember(0x62) && m.longCharacterIsMember(0x63));
    assert!(!m.longCharacterIsMember(0x66) && !m.longCharacterIsMember(0x1F389));
    m.formUnionWithCharacterSet(&NSCharacterSet::decimalDigitCharacterSet());
    m.invert();
    assert!(!m.longCharacterIsMember(0x62) && !m.longCharacterIsMember(0x35));
    assert!(m.longCharacterIsMember(0x7A) && m.longCharacterIsMember(0x1F389));
    // Copies are independent; a mutable copy of a predefined set changes.
    let frozen = m.copy();
    m.removeCharactersInRange(NSRange::new(0x7A, 1));
    assert!(frozen.longCharacterIsMember(0x7A) && !m.longCharacterIsMember(0x7A));
    let digits = NSCharacterSet::decimalDigitCharacterSet().mutableCopy();
    digits.addCharactersInString(&s("-"));
    assert!(digits.longCharacterIsMember(0x2D) && digits.longCharacterIsMember(0x663));
    assert!(!NSCharacterSet::decimalDigitCharacterSet().longCharacterIsMember(0x2D));
    // Class methods sent to the mutable class build mutable sets.
    let ws = NSMutableCharacterSet::whitespaceCharacterSet();
    ws.addCharactersInString(&s("x"));
    assert!(ws.longCharacterIsMember(0x78) && ws.longCharacterIsMember(0x20));
}

#[test]
fn scanning() {
    let sc = NSScanner::scannerWithString(&s("  42 abc 0x1F 3.5e2 ffffffffffffffffff 99999999999"));
    assert!(sc.charactersToBeSkipped().is_some_and(|c| c.longCharacterIsMember(0x0A)));
    assert!(!sc.caseSensitive());
    let mut i: i32 = 0;
    assert!(unsafe { sc.scanInt(&mut i) });
    assert_eq!((i, sc.scanLocation()), (42, 4));
    let mut out: Option<Retained<NSString>> = None;
    assert!(sc.scanString_intoString(&s("ABC"), Some(&mut out)));
    assert_eq!((out.map(|x| x.to_string()), sc.scanLocation()), (Some("abc".into()), 8));
    let mut h: u64 = 0;
    assert!(unsafe { sc.scanHexLongLong(&mut h) });
    assert_eq!((h, sc.scanLocation()), (0x1F, 13));
    let mut d: f64 = 0.0;
    assert!(unsafe { sc.scanDouble(&mut d) });
    assert_eq!((d, sc.scanLocation()), (350.0, 19));
    // Overflow saturates and still consumes every digit.
    assert!(unsafe { sc.scanHexLongLong(&mut h) });
    assert_eq!((h, sc.scanLocation()), (u64::MAX, 38));
    assert!(unsafe { sc.scanInt(&mut i) });
    assert_eq!((i, sc.scanLocation(), sc.isAtEnd()), (i32::MAX, 50, true));

    let sc = NSScanner::scannerWithString(&s("key=value; next"));
    let mut out: Option<Retained<NSString>> = None;
    assert!(sc.scanUpToString_intoString(&s("="), Some(&mut out)));
    assert_eq!((out.map(|x| x.to_string()), sc.scanLocation()), (Some("key".into()), 3));
    let mut out: Option<Retained<NSString>> = None;
    assert!(!sc.scanUpToString_intoString(&s("="), Some(&mut out)));
    assert_eq!((out, sc.scanLocation()), (None, 3));
    sc.setScanLocation(4);
    let mut out: Option<Retained<NSString>> = None;
    assert!(sc.scanUpToString_intoString(&s("#"), Some(&mut out)));
    assert_eq!((out.map(|x| x.to_string()), sc.scanLocation(), sc.isAtEnd()), (Some("value; next".into()), 15, true));

    let sc = NSScanner::scannerWithString(&s("abc   \n"));
    sc.setScanLocation(3);
    assert!(sc.isAtEnd());
    let sc = NSScanner::scannerWithString(&s("ABC def"));
    sc.setCaseSensitive(true);
    assert!(!sc.scanString_intoString(&s("abc"), None));
    assert!(sc.scanString_intoString(&s("ABC"), None));

    let sc = NSScanner::scannerWithString(&s("FF zz"));
    let mut h: u32 = 0;
    assert!(unsafe { sc.scanHexInt(&mut h) });
    assert_eq!(h, 255);
    assert!(!unsafe { sc.scanHexInt(&mut h) });
    let sc = NSScanner::scannerWithString(&s("x"));
    let mut n: isize = 5;
    assert!(!unsafe { sc.scanInteger(&mut n) });
    assert_eq!((n, sc.scanLocation()), (5, 0));
    let sc = NSScanner::scannerWithString(&s("  -12.5e-1xyz"));
    let mut f: f32 = 0.0;
    assert!(unsafe { sc.scanFloat(&mut f) });
    assert_eq!((f, sc.scanLocation()), (-1.25, 10));
    let sc = NSScanner::scannerWithString(&s("aaabbb"));
    let mut out: Option<Retained<NSString>> = None;
    let a = NSCharacterSet::characterSetWithCharactersInString(&s("a"));
    assert!(sc.scanCharactersFromSet_intoString(&a, Some(&mut out)));
    assert_eq!((out.map(|x| x.to_string()), sc.scanLocation()), (Some("aaa".into()), 3));
    let mut out: Option<Retained<NSString>> = None;
    let z = NSCharacterSet::characterSetWithCharactersInString(&s("z"));
    assert!(sc.scanUpToCharactersFromSet_intoString(&z, Some(&mut out)));
    assert_eq!(out.map(|x| x.to_string()), Some("bbb".into()));
    let sc = NSScanner::scannerWithString(&s("0x7fffffffffffffff 0x"));
    let mut h: u64 = 1;
    assert!(unsafe { sc.scanHexLongLong(&mut h) });
    assert_eq!(h, 0x7fff_ffff_ffff_ffff);
    assert!(unsafe { sc.scanHexLongLong(&mut h) });
    assert_eq!((h, sc.scanLocation()), (0, 20));
    let sc = NSScanner::scannerWithString(&s("9223372036854775808 -9223372036854775809 18446744073709551616"));
    let mut ll: i64 = 0;
    assert!(unsafe { sc.scanLongLong(&mut ll) });
    assert_eq!(ll, i64::MAX);
    assert!(unsafe { sc.scanLongLong(&mut ll) });
    assert_eq!(ll, i64::MIN);
    let mut ull: u64 = 0;
    assert!(unsafe { sc.scanUnsignedLongLong(&mut ull) });
    assert_eq!(ull, u64::MAX);
    let sc = NSScanner::scannerWithString(&s("0x1.8p1"));
    let mut hd: f64 = 0.0;
    assert!(unsafe { sc.scanHexDouble(&mut hd) });
    assert_eq!((hd, sc.scanLocation()), (3.0, 7));
    assert_eq!(NSScanner::scannerWithString(&s("text")).string().to_string(), "text");
    let sc = NSScanner::scannerWithString(&s("🎉 12"));
    assert!(!unsafe { sc.scanInt(&mut i) });
    sc.setScanLocation(2);
    assert!(unsafe { sc.scanInt(&mut i) });
    assert_eq!((i, sc.scanLocation()), (12, 5));
}

#[test]
fn scanning_signs_digits_and_hex_floats() {
    let sc = |t: &str| NSScanner::scannerWithString(&s(t));
    // Unsigned scans refuse a minus sign; a plus is fine.
    for (text, ok, value, loc) in [("-1", false, 0, 0), ("+1", true, 1, 2), ("- 5", false, 0, 0)] {
        let x = sc(text);
        let mut v = 0u64;
        assert_eq!((unsafe { x.scanUnsignedLongLong(&mut v) }, v, x.scanLocation()), (ok, value, loc), "{text:?}");
    }
    // Signed integers skip the skip set after the sign, and read digits of
    // any script.
    for (text, ok, value, loc) in [
        ("- 5", true, -5, 3),
        ("+ 5", true, 5, 3),
        ("-\n5", true, -5, 3),
        ("١٢", true, 12, 2),
        ("٣x", true, 3, 1),
        ("-١", true, -1, 2),
        ("１２", true, 12, 2),
        ("12٣", true, 123, 3),
        ("- x", false, 0, 0),
        ("-", false, 0, 0),
        ("-99999999999", true, i32::MIN, 12),
    ] {
        let x = sc(text);
        let mut v = 0i32;
        assert_eq!((unsafe { x.scanInt(&mut v) }, v, x.scanLocation()), (ok, value, loc), "{text:?}");
        let x = sc(text);
        let mut v = 0i64;
        let _ = unsafe { x.scanLongLong(&mut v) };
        assert_eq!(x.scanLocation(), loc, "{text:?}");
    }
    // Hex floats need 0x and a hex digit.
    for (text, ok, value, loc) in [
        ("1.8p1", false, 0.0, 0),
        ("12", false, 0.0, 0),
        ("0x.8", false, 0.0, 0),
        ("0x", false, 0.0, 0),
        ("0xg", false, 0.0, 0),
        ("- 0x2", false, 0.0, 0),
        ("0x1.8p1", true, 3.0, 7),
        ("-0x1p-1", true, -0.5, 7),
        ("0X1P2", true, 4.0, 5),
        ("0x1.8", true, 1.5, 5),
        ("+0x10", true, 16.0, 5),
        ("0x1p", true, 1.0, 3),
        ("0x1p+", true, 1.0, 3),
        (" 0x2", true, 2.0, 4),
    ] {
        let x = sc(text);
        let mut v = 0f64;
        assert_eq!((unsafe { x.scanHexDouble(&mut v) }, v, x.scanLocation()), (ok, value, loc), "{text:?}");
        let x = sc(text);
        let mut f = 0f32;
        assert_eq!((unsafe { x.scanHexFloat(&mut f) }, f), (ok, value as f32), "{text:?}");
    }
    // Decimal floats.
    for (text, ok, value, loc) in [
        (".5", true, 0.5, 2),
        ("1e3", true, 1000.0, 3),
        ("1e", true, 1.0, 1),
        ("inf", false, 0.0, 0),
        ("nan", false, 0.0, 0),
        ("0x1A", true, 0.0, 1),
        ("- 1.5", false, 0.0, 0),
        ("١.٥", true, 1.5, 3),
        ("1.5e+2x", true, 150.0, 6),
        ("+.e1", false, 0.0, 0),
        ("1,5", true, 1.0, 1),
        ("  -3.", true, -3.0, 5),
        ("1e400", true, f64::INFINITY, 5),
        ("1e-400", true, 0.0, 6),
    ] {
        let x = sc(text);
        let mut v = 0f64;
        assert_eq!((unsafe { x.scanDouble(&mut v) }, v, x.scanLocation()), (ok, value, loc), "{text:?}");
    }
    for (text, ok, value, loc) in [("0x", true, 0, 1), ("0xZ", true, 0, 1), ("- ff", false, 0, 0), ("١", false, 0, 0)]
    {
        let x = sc(text);
        let mut v = 0u32;
        assert_eq!((unsafe { x.scanHexInt(&mut v) }, v, x.scanLocation()), (ok, value, loc), "{text:?}");
    }
}

#[test]
fn scanner_copies_and_settings() {
    let x = NSScanner::scannerWithString(&s("  key=value;rest"));
    x.setScanLocation(2);
    x.setCaseSensitive(true);
    x.setCharactersToBeSkipped(Some(&NSCharacterSet::characterSetWithCharactersInString(&s("="))));
    let y: Retained<NSScanner> = unsafe { objc2::msg_send![&*x, copy] };
    assert!(!std::ptr::eq(&*x, &*y));
    assert_eq!((y.scanLocation(), y.caseSensitive(), y.string().to_string()), (2, true, "  key=value;rest".into()));
    assert!(y.charactersToBeSkipped().is_some_and(|c| c.characterIsMember('=' as u16)));
    x.setScanLocation(5);
    assert_eq!(y.scanLocation(), 2);
    // Scanning skips the new set: `=` before a scan, not spaces.
    let mut out: Option<Retained<NSString>> = None;
    assert!(y.scanUpToString_intoString(&s(";"), Some(&mut out)));
    assert_eq!(out.map(|o| o.to_string()), Some("key=value".into()));
    // A plain scanner has no locale, and keeps none it is given.
    assert!(y.locale().is_none());
    unsafe { y.setLocale(None) };
    assert!(y.locale().is_none());
    let x = NSScanner::scannerWithString(&s("  ab"));
    x.setCharactersToBeSkipped(None);
    assert!(x.charactersToBeSkipped().is_none());
    assert!(!x.scanString_intoString(&s("ab"), None));
    x.setScanLocation(2);
    assert!(x.scanString_intoString(&s("ab"), None));
    let mut f = 0f32;
    let x = NSScanner::scannerWithString(&s("2.5"));
    assert!(unsafe { x.scanFloat(&mut f) });
    assert_eq!(f, 2.5);
}
