//! NSAttributedString and NSMutableAttributedString: runs and how they
//! split and merge, effective and longest ranges, enumeration, how edits
//! inherit attributes, the live backing string, equality and copies, and a
//! subclass that implements only the primitives. Expected values are what
//! macOS returns. Attribute values are strings, long enough that Apple
//! doesn't store them as tagged pointers.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{
    NSAttributedString, NSAttributedStringEnumerationOptions, NSCopying, NSDictionary, NSMutableAttributedString,
    NSMutableCopying, NSMutableString, NSRange, NSString,
};

use sidestep as _;

type Dict = NSDictionary<NSString, AnyObject>;

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn dict(pairs: &[(&str, &NSString)]) -> Retained<Dict> {
    let keys: Vec<Retained<NSString>> = pairs.iter().map(|p| s(p.0)).collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&AnyObject> = pairs.iter().map(|p| p.1.as_ref()).collect();
    NSDictionary::from_slices(&key_refs, &values)
}

/// A dictionary's entries as sorted text.
fn entries(d: &Dict) -> Vec<(String, String)> {
    let (keys, values) = d.to_vecs();
    let mut out: Vec<(String, String)> = keys
        .iter()
        .zip(&values)
        .map(|(k, v)| (k.to_string(), v.downcast_ref::<NSString>().map(|v| v.to_string()).unwrap_or_default()))
        .collect();
    out.sort();
    out
}

type Runs = Vec<(usize, usize, Vec<(String, String)>)>;

/// The runs `attributesAtIndex:effectiveRange:` reports.
fn runs(a: &NSAttributedString) -> Runs {
    let mut out = Vec::new();
    let mut i = 0;
    while i < a.length() {
        let mut r = NSRange::new(0, 0);
        let d = unsafe { a.attributesAtIndex_effectiveRange(i, &mut r) };
        out.push((r.location, r.length, entries(&d)));
        i = r.location + r.length;
    }
    out
}

fn run(loc: usize, len: usize, attrs: &[(&str, &str)]) -> (usize, usize, Vec<(String, String)>) {
    (loc, len, attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
}

const RED: &str = "red-attribute-value";
const BLUE: &str = "blue-attribute-value";

fn add(m: &NSMutableAttributedString, key: &str, value: &NSString, loc: usize, len: usize) {
    unsafe { m.addAttribute_value_range(&s(key), value, NSRange::new(loc, len)) };
}

/// "aaabbb" with x=red on the first half and y=blue on the second.
fn halves() -> Retained<NSMutableAttributedString> {
    let m = NSMutableAttributedString::from_nsstring(&s("aaabbb"));
    add(&m, "x", &s(RED), 0, 3);
    add(&m, "y", &s(BLUE), 3, 3);
    m
}

#[test]
fn basics() {
    let a = unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &s("hello"), None) };
    assert_eq!(runs(&a), [run(0, 5, &[])]);
    assert_eq!(a.length(), 5);
    assert_eq!(a.string().to_string(), "hello");
    let d = unsafe { a.attributesAtIndex_effectiveRange(2, std::ptr::null_mut()) };
    assert_eq!(d.count(), 0);
    let red = s(RED);
    let a = unsafe { NSAttributedString::new_with_attributes(&s("hi"), &dict(&[("k", &red)])) };
    assert_eq!(runs(&a), [run(0, 2, &[("k", RED)])]);
    assert_eq!(NSAttributedString::new().length(), 0);
    assert_eq!(NSMutableAttributedString::new().length(), 0);
    assert_eq!(NSAttributedString::from_nsstring(&s("")).length(), 0);
}

#[test]
fn runs_split_but_only_shared_dictionaries_merge() {
    let red = s(RED);
    let other_red = s(RED);
    let m = NSMutableAttributedString::from_nsstring(&s("0123456789"));
    add(&m, "color", &red, 2, 3);
    assert_eq!(runs(&m), [run(0, 2, &[]), run(2, 3, &[("color", RED)]), run(5, 5, &[])]);
    // Adding the same value next door makes a new dictionary: no merge,
    // whether or not the value is the same object.
    add(&m, "color", &red, 5, 2);
    add(&m, "color", &other_red, 7, 1);
    assert_eq!(
        runs(&m),
        [
            run(0, 2, &[]),
            run(2, 3, &[("color", RED)]),
            run(5, 2, &[("color", RED)]),
            run(7, 1, &[("color", RED)]),
            run(8, 2, &[])
        ]
    );
    // Separately built equal dictionaries stay separate runs too.
    let m = NSMutableAttributedString::from_nsstring(&s("abcdef"));
    unsafe {
        m.setAttributes_range(Some(&dict(&[("k", &red)])), NSRange::new(0, 3));
        m.setAttributes_range(Some(&dict(&[("k", &other_red)])), NSRange::new(3, 3));
    }
    assert_eq!(runs(&m), [run(0, 3, &[("k", RED)]), run(3, 3, &[("k", RED)])]);
    // Splitting a run leaves both sides with the same dictionary.
    unsafe { m.setAttributes_range(Some(&dict(&[("k", &s(BLUE))])), NSRange::new(1, 1)) };
    assert_eq!(
        runs(&m),
        [run(0, 1, &[("k", RED)]), run(1, 1, &[("k", BLUE)]), run(2, 1, &[("k", RED)]), run(3, 3, &[("k", RED)])]
    );
    unsafe { m.setAttributes_range(None, NSRange::new(0, 6)) };
    assert_eq!(runs(&m), [run(0, 6, &[])]);
}

#[test]
fn text_without_attributes_is_one_run() {
    let m = NSMutableAttributedString::from_nsstring(&s("ab"));
    m.appendAttributedString(&NSAttributedString::from_nsstring(&s("cd")));
    assert_eq!(runs(&m), [run(0, 4, &[])]);
    let m = NSMutableAttributedString::from_nsstring(&s("abcd"));
    add(&m, "k", &s(RED), 0, 2);
    m.removeAttribute_range(&s("k"), NSRange::new(0, 2));
    assert_eq!(runs(&m), [run(0, 4, &[])]);
    let m = NSMutableAttributedString::from_nsstring(&s("abcd"));
    unsafe { m.setAttributes_range(Some(&dict(&[])), NSRange::new(0, 2)) };
    assert_eq!(runs(&m), [run(0, 4, &[])]);
    // Text inserted into an empty string has no attributes, whatever the
    // string was created or last held.
    let m = unsafe {
        NSMutableAttributedString::initWithString_attributes(
            NSMutableAttributedString::alloc(),
            &s(""),
            Some(&dict(&[("k", &s(RED))])),
        )
    };
    m.replaceCharactersInRange_withString(NSRange::new(0, 0), &s("x"));
    assert_eq!(runs(&m), [run(0, 1, &[])]);
    let m = NSMutableAttributedString::from_nsstring(&s("abcd"));
    add(&m, "k", &s(RED), 0, 4);
    m.deleteCharactersInRange(NSRange::new(0, 4));
    m.replaceCharactersInRange_withString(NSRange::new(0, 0), &s("x"));
    assert_eq!(runs(&m), [run(0, 1, &[])]);
}

#[test]
fn adding_and_removing_attributes() {
    let (red, blue) = (s(RED), s(BLUE));
    let m = NSMutableAttributedString::from_nsstring(&s("abcdef"));
    unsafe {
        m.addAttributes_range(&dict(&[("a", &red), ("b", &red)]), NSRange::new(0, 4));
        m.addAttributes_range(&dict(&[("b", &blue), ("c", &blue)]), NSRange::new(2, 4));
    }
    let expected = vec![
        run(0, 2, &[("a", RED), ("b", RED)]),
        run(2, 2, &[("a", RED), ("b", BLUE), ("c", BLUE)]),
        run(4, 2, &[("b", BLUE), ("c", BLUE)]),
    ];
    assert_eq!(runs(&m), expected);
    m.removeAttribute_range(&s("absent"), NSRange::new(0, 6));
    assert_eq!(runs(&m), expected, "removing an absent attribute changes nothing");
    m.removeAttribute_range(&s("b"), NSRange::new(1, 2));
    assert_eq!(
        runs(&m),
        [
            run(0, 1, &[("a", RED), ("b", RED)]),
            run(1, 1, &[("a", RED)]),
            run(2, 1, &[("a", RED), ("c", BLUE)]),
            run(3, 1, &[("a", RED), ("b", BLUE), ("c", BLUE)]),
            run(4, 2, &[("b", BLUE), ("c", BLUE)]),
        ]
    );
    let before = runs(&m);
    add(&m, "z", &red, 3, 0);
    assert_eq!(runs(&m), before, "a zero-length range is a no-op");
}

#[test]
fn effective_and_longest_ranges() {
    let (red, blue) = (s(RED), s(BLUE));
    let m = NSMutableAttributedString::from_nsstring(&s("0123456789"));
    add(&m, "color", &red, 2, 3);
    add(&m, "color", &s(RED), 5, 3);
    add(&m, "font", &blue, 0, 10);
    let mut r = NSRange::new(0, 0);
    // The effective range is the run's, even when the value goes on.
    let v = unsafe { m.attribute_atIndex_effectiveRange(&s("color"), 3, &mut r) };
    assert_eq!((v.is_some(), r), (true, NSRange::new(2, 3)));
    let v = unsafe { m.attribute_atIndex_effectiveRange(&s("font"), 3, &mut r) };
    assert_eq!((v.is_some(), r), (true, NSRange::new(2, 3)));
    let v = unsafe { m.attribute_atIndex_effectiveRange(&s("color"), 0, &mut r) };
    assert_eq!((v.is_none(), r), (true, NSRange::new(0, 2)));
    // Longest ranges look past run boundaries, comparing by value, within
    // the limit.
    unsafe { m.attribute_atIndex_longestEffectiveRange_inRange(&s("font"), 3, &mut r, NSRange::new(1, 6)) };
    assert_eq!(r, NSRange::new(1, 6));
    unsafe { m.attribute_atIndex_longestEffectiveRange_inRange(&s("color"), 3, &mut r, NSRange::new(0, 10)) };
    assert_eq!(r, NSRange::new(2, 6));
    let v = unsafe { m.attribute_atIndex_longestEffectiveRange_inRange(&s("color"), 0, &mut r, NSRange::new(0, 10)) };
    assert_eq!((v.is_none(), r), (true, NSRange::new(0, 2)));
    let d = unsafe { m.attributesAtIndex_longestEffectiveRange_inRange(3, &mut r, NSRange::new(0, 10)) };
    assert_eq!((d.count(), r), (2, NSRange::new(2, 6)));
    unsafe { m.attributesAtIndex_longestEffectiveRange_inRange(3, &mut r, NSRange::new(3, 4)) };
    assert_eq!(r, NSRange::new(3, 4));
    // Runs returned for the same run are the same dictionary.
    let d1 = unsafe { m.attributesAtIndex_effectiveRange(2, std::ptr::null_mut()) };
    let d2 = unsafe { m.attributesAtIndex_effectiveRange(4, std::ptr::null_mut()) };
    assert!(std::ptr::eq(&*d1, &*d2));
}

/// "aabbccdd": k=red on 0..2, an equal but separate red on 2..4, j=blue on
/// 1..3, k=blue on 6..8.
fn enumerable() -> Retained<NSMutableAttributedString> {
    let m = NSMutableAttributedString::from_nsstring(&s("aabbccdd"));
    add(&m, "k", &s(RED), 0, 2);
    add(&m, "k", &s(RED), 2, 2);
    add(&m, "j", &s(BLUE), 1, 2);
    add(&m, "k", &s(BLUE), 6, 2);
    m
}

fn enumerate_attribute(m: &NSAttributedString, range: NSRange, opts: usize) -> Vec<(usize, usize, bool)> {
    let out = RefCell::new(Vec::new());
    let block = RcBlock::new(|v: *mut AnyObject, r: NSRange, _stop: NonNull<Bool>| {
        out.borrow_mut().push((r.location, r.length, !v.is_null()));
    });
    m.enumerateAttribute_inRange_options_usingBlock(&s("k"), range, NSAttributedStringEnumerationOptions(opts), &block);
    drop(block);
    out.into_inner()
}

fn enumerate_attributes(m: &NSAttributedString, range: NSRange, opts: usize) -> Vec<(usize, usize, usize)> {
    let out = RefCell::new(Vec::new());
    let block = RcBlock::new(|d: NonNull<Dict>, r: NSRange, _stop: NonNull<Bool>| {
        out.borrow_mut().push((r.location, r.length, unsafe { d.as_ref() }.count()));
    });
    m.enumerateAttributesInRange_options_usingBlock(range, NSAttributedStringEnumerationOptions(opts), &block);
    drop(block);
    out.into_inner()
}

const REVERSE: usize = 1 << 1;
const NOT_LONGEST: usize = 1 << 20;

#[test]
fn enumeration() {
    let m = enumerable();
    let all = NSRange::new(0, 8);
    assert_eq!(enumerate_attribute(&m, all, 0), [(0, 4, true), (4, 2, false), (6, 2, true)]);
    assert_eq!(
        enumerate_attribute(&m, all, NOT_LONGEST),
        [(0, 1, true), (1, 1, true), (2, 1, true), (3, 1, true), (4, 2, false), (6, 2, true)]
    );
    assert_eq!(enumerate_attribute(&m, all, REVERSE), [(6, 2, true), (4, 2, false), (0, 4, true)]);
    assert_eq!(
        enumerate_attribute(&m, all, REVERSE | NOT_LONGEST),
        [(6, 2, true), (4, 2, false), (3, 1, true), (2, 1, true), (1, 1, true), (0, 1, true)]
    );
    let inner = NSRange::new(1, 6);
    assert_eq!(enumerate_attributes(&m, inner, 0), [(1, 2, 2), (3, 1, 1), (4, 2, 0), (6, 1, 1)]);
    assert_eq!(enumerate_attributes(&m, inner, NOT_LONGEST), [(1, 1, 2), (2, 1, 2), (3, 1, 1), (4, 2, 0), (6, 1, 1)]);
    assert_eq!(enumerate_attributes(&m, inner, REVERSE), [(6, 1, 1), (4, 2, 0), (3, 1, 1), (1, 2, 2)]);
    assert_eq!(enumerate_attribute(&m, NSRange::new(3, 0), 0), []);
    // Stop.
    let seen = RefCell::new(Vec::new());
    let block = RcBlock::new(|_: *mut AnyObject, r: NSRange, stop: NonNull<Bool>| {
        seen.borrow_mut().push((r.location, r.length));
        unsafe { *stop.as_ptr() = Bool::YES };
    });
    m.enumerateAttribute_inRange_options_usingBlock(&s("k"), all, NSAttributedStringEnumerationOptions(0), &block);
    drop(block);
    assert_eq!(seen.into_inner(), [(0, 4)]);
}

#[test]
fn enumeration_allows_edits_to_the_passed_range() {
    let m = NSMutableAttributedString::from_nsstring(&s("aabbccdd"));
    add(&m, "k", &s(RED), 0, 2);
    add(&m, "k", &s(BLUE), 4, 2);
    let seen = RefCell::new(Vec::new());
    let filled = s("filled-in-by-the-block");
    let block = RcBlock::new(|v: *mut AnyObject, r: NSRange, _stop: NonNull<Bool>| {
        seen.borrow_mut().push((r.location, r.length, !v.is_null()));
        if v.is_null() {
            unsafe { m.addAttribute_value_range(&s("k"), &filled, r) };
        } else {
            m.removeAttribute_range(&s("k"), r);
        }
    });
    m.enumerateAttribute_inRange_options_usingBlock(
        &s("k"),
        NSRange::new(0, 8),
        NSAttributedStringEnumerationOptions(0),
        &block,
    );
    drop(block);
    assert_eq!(seen.into_inner(), [(0, 2, true), (0, 4, false), (4, 2, true), (4, 4, false)]);
    let filled = "filled-in-by-the-block";
    assert_eq!(runs(&m), [run(0, 4, &[("k", filled)]), run(4, 4, &[("k", filled)])]);
}

/// Enumerate `k` over `range` of `text` with k=red on 2..4, letting `edit`
/// change the text for each range found: the ranges reported and the text
/// left.
fn enumerate_editing(
    text: &str,
    range: NSRange,
    opts: usize,
    each: bool,
    edit: impl Fn(&NSMutableAttributedString, NSRange),
) -> (Vec<(usize, usize)>, String) {
    let m = NSMutableAttributedString::from_nsstring(&s(text));
    add(&m, "k", &s(RED), 2, 2);
    let seen = RefCell::new(Vec::new());
    let opts = NSAttributedStringEnumerationOptions(opts);
    if each {
        let block = RcBlock::new(|_: NonNull<Dict>, r: NSRange, _stop: NonNull<Bool>| {
            seen.borrow_mut().push((r.location, r.length));
            edit(&m, r);
        });
        m.enumerateAttributesInRange_options_usingBlock(range, opts, &block);
    } else {
        let block = RcBlock::new(|_: *mut AnyObject, r: NSRange, _stop: NonNull<Bool>| {
            seen.borrow_mut().push((r.location, r.length));
            edit(&m, r);
        });
        m.enumerateAttribute_inRange_options_usingBlock(&s("k"), range, opts, &block);
    }
    (seen.into_inner(), m.string().to_string())
}

#[test]
fn enumeration_follows_edits_that_change_the_length() {
    let all = NSRange::new(0, 6);
    let delete_first = |m: &NSMutableAttributedString, r: NSRange| {
        if r.location == 0 {
            m.deleteCharactersInRange(NSRange::new(0, 1));
        }
    };
    let replace = |m: &NSMutableAttributedString, r: NSRange| m.replaceCharactersInRange_withString(r, &s("X"));
    let insert = |m: &NSMutableAttributedString, r: NSRange| {
        m.replaceCharactersInRange_withString(NSRange::new(r.location, 0), &s("__"))
    };
    let delete = |m: &NSMutableAttributedString, r: NSRange| m.deleteCharactersInRange(r);
    // Going forwards, the next range starts after the edited one.
    assert_eq!(
        enumerate_editing("aabbcc", all, 0, false, delete_first),
        (vec![(0, 2), (1, 2), (3, 2)], "abbcc".into())
    );
    assert_eq!(enumerate_editing("aabbcc", all, 0, true, delete_first), (vec![(0, 2), (1, 2), (3, 2)], "abbcc".into()));
    assert_eq!(enumerate_editing("aabbcc", all, 0, true, replace), (vec![(0, 2), (1, 2), (2, 2)], "XXX".into()));
    assert_eq!(enumerate_editing("aabbcc", all, 0, false, replace), (vec![(0, 2), (1, 2), (2, 2)], "XXX".into()));
    assert_eq!(
        enumerate_editing("aabbcc", all, 0, false, insert),
        (vec![(0, 2), (4, 2), (8, 2)], "__aa__bb__cc".into())
    );
    assert_eq!(enumerate_editing("aabbcc", all, 0, false, delete), (vec![(0, 2), (0, 2), (0, 2)], "".into()));
    assert_eq!(enumerate_editing("aabbcc", all, NOT_LONGEST, true, delete), (vec![(0, 2), (0, 2), (0, 2)], "".into()));
    // A range inside the text.
    assert_eq!(
        enumerate_editing("xaabbccx", NSRange::new(1, 6), 0, false, replace),
        (vec![(1, 1), (2, 2), (3, 3)], "xXXXx".into())
    );
    // Going backwards, edits don't move what is left to visit.
    assert_eq!(enumerate_editing("aabbcc", all, REVERSE, false, delete), (vec![(4, 2), (2, 2), (0, 2)], "".into()));
    assert_eq!(enumerate_editing("aabbcc", all, REVERSE, true, replace), (vec![(4, 2), (2, 2), (0, 2)], "XXX".into()));
    assert_eq!(
        enumerate_editing("aabbcc", all, REVERSE, false, insert),
        (vec![(4, 2), (2, 4), (0, 4)], "__aa__bb__cc".into())
    );
}

#[test]
fn edits_between_begin_and_end_editing() {
    let x = [("x", RED)];
    let y = [("y", BLUE)];
    let m = halves();
    m.beginEditing();
    m.beginEditing();
    m.replaceCharactersInRange_withString(NSRange::new(2, 2), &s("ZZZ"));
    add(&m, "y", &s(BLUE), 0, 1);
    m.endEditing();
    m.deleteCharactersInRange(NSRange::new(6, 1));
    m.endEditing();
    assert_eq!(m.string().to_string(), "aaZZZb");
    assert_eq!(runs(&m), [run(0, 1, &[("x", RED), ("y", BLUE)]), run(1, 4, &x), run(5, 1, &y)]);
    // An extra endEditing does nothing.
    m.endEditing();
    assert_eq!(m.length(), 6);
}

#[test]
fn edits_inherit_attributes() {
    let x = [("x", RED)];
    let y = [("y", BLUE)];
    let edit = |loc: usize, len: usize, text: &str| {
        let m = halves();
        m.replaceCharactersInRange_withString(NSRange::new(loc, len), &s(text));
        runs(&m)
    };
    // New text takes the attributes of the first replaced unit, or for an
    // insertion those of the unit before it.
    assert_eq!(edit(2, 2, "ZZ"), [run(0, 4, &x), run(4, 2, &y)]);
    assert_eq!(edit(3, 0, "II"), [run(0, 5, &x), run(5, 3, &y)]);
    assert_eq!(edit(1, 0, "II"), [run(0, 5, &x), run(5, 3, &y)]);
    assert_eq!(edit(0, 0, "II"), [run(0, 5, &x), run(5, 3, &y)]);
    assert_eq!(edit(6, 0, "II"), [run(0, 3, &x), run(3, 5, &y)]);
    assert_eq!(edit(0, 6, "new"), [run(0, 3, &x)]);
    assert_eq!(edit(0, 3, ""), [run(0, 3, &y)]);
    assert_eq!(edit(2, 4, ""), [run(0, 2, &x)]);
    let m = NSMutableAttributedString::new();
    m.replaceCharactersInRange_withString(NSRange::new(0, 0), &s("xy"));
    assert_eq!(runs(&m), [run(0, 2, &[])]);
    let m = halves();
    m.deleteCharactersInRange(NSRange::new(1, 4));
    assert_eq!(runs(&m), [run(0, 1, &x), run(1, 1, &y)]);
    // Deleting what split a run joins its two halves again.
    let m = NSMutableAttributedString::from_nsstring(&s("aaXaa"));
    add(&m, "x", &s(RED), 0, 5);
    add(&m, "y", &s(BLUE), 2, 1);
    assert_eq!(runs(&m).len(), 3);
    m.deleteCharactersInRange(NSRange::new(2, 1));
    assert_eq!(runs(&m), [run(0, 4, &x)]);
}

#[test]
fn attributed_edits_keep_and_clip_runs() {
    let x = [("x", RED)];
    let y = [("y", BLUE)];
    let other = unsafe { NSAttributedString::new_with_attributes(&s("CC"), &dict(&[("y", &s(BLUE))])) };
    let m = halves();
    m.appendAttributedString(&other);
    assert_eq!(runs(&m), [run(0, 3, &x), run(3, 3, &y), run(6, 2, &y)]);
    let m = halves();
    m.insertAttributedString_atIndex(&other, 1);
    assert_eq!(runs(&m), [run(0, 1, &x), run(1, 2, &y), run(3, 2, &x), run(5, 3, &y)]);
    let m = halves();
    m.replaceCharactersInRange_withAttributedString(NSRange::new(2, 2), &other);
    assert_eq!(runs(&m), [run(0, 2, &x), run(2, 2, &y), run(4, 2, &y)]);
    assert_eq!(m.string().to_string(), "aaCCbb");
    let m = halves();
    let sub = m.attributedSubstringFromRange(NSRange::new(2, 3));
    assert_eq!((runs(&sub), sub.string().to_string()), (vec![run(0, 1, &x), run(1, 2, &y)], "abb".into()));
    let m = halves();
    m.appendAttributedString(&m.copy());
    assert_eq!(runs(&m), [run(0, 3, &x), run(3, 3, &y), run(6, 3, &x), run(9, 3, &y)]);
    let m = halves();
    m.appendAttributedString(&m);
    assert_eq!(runs(&m), [run(0, 3, &x), run(3, 3, &y), run(6, 3, &x), run(9, 3, &y)]);
    assert_eq!(m.string().to_string(), "aaabbbaaabbb");
    let m = halves();
    m.setAttributedString(&other);
    assert_eq!(runs(&m), [run(0, 2, &y)]);
}

#[test]
fn the_mutable_string_is_live() {
    let x = [("x", RED)];
    let y = [("y", BLUE)];
    let m = halves();
    let string = m.string();
    m.replaceCharactersInRange_withString(NSRange::new(0, 1), &s("Q"));
    assert_eq!(string.to_string(), "Qaabbb");
    // Edits through mutableString move the runs as the primitive does.
    let m = halves();
    let ms = m.mutableString();
    ms.insertString_atIndex(&s("MM"), 4);
    assert_eq!((runs(&m), m.string().to_string()), (vec![run(0, 3, &x), run(3, 5, &y)], "aaabMMbb".into()));
    ms.appendString(&s("E"));
    assert_eq!(runs(&m), [run(0, 3, &x), run(3, 6, &y)]);
    ms.deleteCharactersInRange(NSRange::new(0, 2));
    assert_eq!(runs(&m), [run(0, 1, &x), run(1, 6, &y)]);
    ms.setString(&s("fresh"));
    assert_eq!(runs(&m), [run(0, 5, &x)]);
    assert_eq!(ms.to_string(), "fresh");
    // An immutable attributed string keeps its own copy.
    let source = NSMutableString::from_str("src");
    let a = NSAttributedString::from_nsstring(&source);
    source.appendString(&s("!"));
    assert_eq!(a.string().to_string(), "src");
}

#[test]
fn equality_hash_and_copies() {
    let (red, other_red) = (s(RED), s(RED));
    let a = unsafe { NSAttributedString::new_with_attributes(&s("eq"), &dict(&[("k", &red)])) };
    let b = unsafe { NSAttributedString::new_with_attributes(&s("eq"), &dict(&[("k", &other_red)])) };
    assert!(a.isEqualToAttributedString(&b));
    assert!(a.isEqual(Some(&b)));
    assert_eq!(a.hash(), b.hash());
    let m = NSMutableAttributedString::from_attributed_nsstring(&a);
    assert!(a.isEqual(Some(&m)) && m.isEqual(Some(&a)));
    // Equality ignores how the runs are split.
    let split = NSMutableAttributedString::from_nsstring(&s("eq"));
    unsafe {
        split.setAttributes_range(Some(&dict(&[("k", &red)])), NSRange::new(0, 1));
        split.setAttributes_range(Some(&dict(&[("k", &other_red)])), NSRange::new(1, 1));
    }
    assert!(a.isEqualToAttributedString(&split));
    assert!(!a.isEqualToAttributedString(&NSAttributedString::from_nsstring(&s("eq"))));
    assert!(!a.isEqual(Some(&s("eq"))));
    // Copies are independent of the original and of each other.
    let m = halves();
    let copy = m.copy();
    let mutable = m.mutableCopy();
    m.replaceCharactersInRange_withString(NSRange::new(0, 1), &s("Z"));
    mutable.appendAttributedString(&NSAttributedString::from_nsstring(&s("CC")));
    assert_eq!(
        (copy.string().to_string(), mutable.string().to_string(), m.string().to_string()),
        ("aaabbb".into(), "aaabbbCC".into(), "Zaabbb".into())
    );
    assert_eq!(runs(&copy), [run(0, 3, &[("x", RED)]), run(3, 3, &[("y", BLUE)])]);
}

#[test]
fn attribute_dictionaries_are_copied() {
    use objc2::runtime::ProtocolObject;
    use objc2_foundation::NSMutableDictionary;
    let md = NSMutableDictionary::<NSString, AnyObject>::new();
    unsafe { md.setObject_forKey(&*s(RED), ProtocolObject::from_ref(&*s("k"))) };
    let m = NSMutableAttributedString::from_nsstring(&s("abc"));
    unsafe { m.setAttributes_range(Some(&md), NSRange::new(0, 3)) };
    unsafe { md.setObject_forKey(&*s(BLUE), ProtocolObject::from_ref(&*s("k2"))) };
    assert_eq!(runs(&m), [run(0, 3, &[("k", RED)])]);
}

#[test]
fn ranges_may_split_surrogate_pairs() {
    let m = NSMutableAttributedString::from_nsstring(&s("a🎉b"));
    add(&m, "x", &s(RED), 0, 2);
    assert_eq!(runs(&m), [run(0, 2, &[("x", RED)]), run(2, 2, &[])]);
    assert_eq!(m.attributedSubstringFromRange(NSRange::new(1, 1)).length(), 1);
    m.replaceCharactersInRange_withString(NSRange::new(2, 1), &s("-"));
    assert_eq!(m.length(), 4);
    assert_eq!(runs(&m), [run(0, 2, &[("x", RED)]), run(2, 2, &[])]);
}

/// Storage for an attributed string class of an app's own: its text, and a
/// dictionary per unit.
struct Own {
    text: Retained<NSMutableString>,
    attrs: RefCell<Vec<Retained<Dict>>>,
    primitive_calls: Cell<usize>,
}

define_class!(
    // A subclass that implements only the four primitives over its own
    // storage, as an NSTextStorage does.
    #[unsafe(super(NSMutableAttributedString, NSAttributedString))]
    #[ivars = Own]
    struct OwnStorage;

    impl OwnStorage {
        #[unsafe(method_id(string))]
        fn string(&self) -> Retained<NSString> {
            Retained::into_super(self.ivars().text.clone())
        }

        #[unsafe(method_id(attributesAtIndex:effectiveRange:))]
        fn attributes_at(&self, index: usize, range: *mut NSRange) -> Retained<Dict> {
            if !range.is_null() {
                unsafe { *range = NSRange::new(index, 1) };
            }
            self.ivars().attrs.borrow()[index].clone()
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace(&self, range: NSRange, string: &NSString) {
            let iv = self.ivars();
            iv.primitive_calls.set(iv.primitive_calls.get() + 1);
            let inherited = {
                let attrs = iv.attrs.borrow();
                let at = if range.length > 0 { Some(range.location) } else { range.location.checked_sub(1) };
                at.and_then(|i| attrs.get(i).cloned()).unwrap_or_else(NSDictionary::new)
            };
            let len = string.length();
            iv.text.replaceCharactersInRange_withString(range, string);
            iv.attrs.borrow_mut().splice(range.location..range.location + range.length, (0..len).map(|_| inherited.clone()));
        }

        #[unsafe(method(setAttributes:range:))]
        fn set_attributes(&self, attrs: Option<&Dict>, range: NSRange) {
            let iv = self.ivars();
            iv.primitive_calls.set(iv.primitive_calls.get() + 1);
            let d: Retained<Dict> = attrs.map(|d| d.copy()).unwrap_or_else(NSDictionary::new);
            for slot in &mut iv.attrs.borrow_mut()[range.location..range.location + range.length] {
                *slot = d.clone();
            }
        }
    }
);

fn own(text: &str) -> Retained<OwnStorage> {
    let units = text.encode_utf16().count();
    let this = OwnStorage::alloc().set_ivars(Own {
        text: NSMutableString::from_str(text),
        attrs: RefCell::new((0..units).map(|_| NSDictionary::new()).collect()),
        primitive_calls: Cell::new(0),
    });
    unsafe { msg_send![super(this), init] }
}

#[test]
fn subclasses_implementing_only_the_primitives() {
    let o = own("aaabbb");
    let m: &NSMutableAttributedString = &o;
    add(m, "x", &s(RED), 0, 3);
    add(m, "y", &s(BLUE), 3, 3);
    assert!(o.ivars().primitive_calls.get() > 0, "addAttribute: goes through setAttributes:range:");
    assert_eq!(m.length(), 6);
    // Runs are the subclass's (one unit each); longest ranges merge them.
    assert_eq!(runs(m).len(), 6);
    let mut r = NSRange::new(0, 0);
    unsafe { m.attributesAtIndex_longestEffectiveRange_inRange(1, &mut r, NSRange::new(0, 6)) };
    assert_eq!(r, NSRange::new(0, 3));
    assert_eq!(enumerate_attribute(m, NSRange::new(0, 6), 0), [(0, 6, false)]);
    let sub = m.attributedSubstringFromRange(NSRange::new(2, 2));
    assert_eq!(sub.string().to_string(), "ab");
    assert!(sub.isEqualToAttributedString(&halves().attributedSubstringFromRange(NSRange::new(2, 2))));
    assert!(m.isEqualToAttributedString(&halves()));
    let calls = o.ivars().primitive_calls.get();
    let other = unsafe { NSAttributedString::new_with_attributes(&s("CC"), &dict(&[("y", &s(BLUE))])) };
    m.appendAttributedString(&other);
    assert!(o.ivars().primitive_calls.get() > calls, "appendAttributedString: goes through the primitives");
    assert_eq!(m.string().to_string(), "aaabbbCC");
    assert!(m.attributedSubstringFromRange(NSRange::new(6, 2)).isEqualToAttributedString(&other));
    // mutableString is live and edits through it reach the primitive.
    let calls = o.ivars().primitive_calls.get();
    let ms = m.mutableString();
    ms.appendString(&s("!"));
    assert!(o.ivars().primitive_calls.get() > calls);
    assert_eq!(m.string().to_string(), "aaabbbCC!");
    assert_eq!(ms.to_string(), "aaabbbCC!");
    m.deleteCharactersInRange(NSRange::new(0, 3));
    assert_eq!(ms.to_string(), "bbbCC!");
    let copy = m.copy();
    assert_eq!(copy.string().to_string(), "bbbCC!");
    assert!(copy.isEqualToAttributedString(m));
}
