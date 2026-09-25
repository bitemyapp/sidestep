//! Foundation behavior, checked on macOS and on Linux alike.

use objc2::ClassType;
use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, NSObjectProtocol};
use objc2_foundation::{NSString, ns_string};

use sidestep as _;

#[test]
fn strings_round_trip() {
    for text in ["", "hello", "héllo wörld", "emoji 🎉 and CJK 漢字", "nul\0inside"] {
        let s = NSString::from_str(text);
        assert_eq!(s.to_string(), text);
        assert_eq!(s.len(), text.len());
        assert_eq!(s.len_utf16(), text.encode_utf16().count());
        autoreleasepool(|pool| assert_eq!(unsafe { s.to_str(pool) }, text));
    }
}

#[test]
fn string_equality_and_hash() {
    let a = NSString::from_str("same");
    let b = NSString::from_str("same");
    let c = NSString::from_str("different");
    // Whether equal strings share an object is unspecified: Apple's short
    // strings are tagged pointers, so they do.
    assert!(a.isEqual(Some(&b)));
    assert_eq!(a.hash(), b.hash());
    assert!(!a.isEqual(Some(&c)));
    assert!(a.isEqualToString(&b));
    assert_eq!(a, b);
}

#[test]
fn string_literals() {
    let s = ns_string!("from a literal");
    assert_eq!(s.to_string(), "from a literal");
}

#[test]
fn utf16_access() {
    let s = NSString::from_str("a🎉b");
    assert_eq!(s.length(), 4);
    let units: Vec<u16> = (0..s.length()).map(|i| s.characterAtIndex(i)).collect();
    assert_eq!(units, "a🎉b".encode_utf16().collect::<Vec<_>>());
}

#[test]
fn string_class_is_linked() {
    let s = NSString::from_str("x");
    assert!(s.isKindOfClass(NSString::class()));
    assert!(AnyClass::get(c"NSString").is_some());
}
