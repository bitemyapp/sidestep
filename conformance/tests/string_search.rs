//! NSString comparison, searching, case mapping, replacement, lines and
//! paragraphs, enumeration, splitting and trimming, and number parsing.
//! Expected values are what macOS returns.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{
    NSArray, NSCharacterSet, NSComparisonResult, NSMutableString, NSRange, NSString, NSStringCompareOptions,
    NSStringEnumerationOptions,
};

use sidestep as _;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

const CI: usize = 1;
const LITERAL: usize = 2;
const BACKWARDS: usize = 4;
const ANCHORED: usize = 8;
const NUMERIC: usize = 64;
const DIACRITIC: usize = 128;
const WIDTH: usize = 256;
const FORCED: usize = 512;
const REGEX: usize = 1024;

fn cmp(a: &str, b: &str, options: usize) -> isize {
    s(a).compare_options(&s(b), NSStringCompareOptions(options)) as isize
}

/// A found range, or None for {NSNotFound, 0}.
fn found(r: NSRange) -> Option<(usize, usize)> {
    (r.location != isize::MAX as usize).then_some((r.location, r.length))
}

fn find(h: &str, n: &str, options: usize) -> Option<(usize, usize)> {
    let r = s(h).rangeOfString_options(&s(n), NSStringCompareOptions(options));
    if r.location == isize::MAX as usize {
        assert_eq!(r.length, 0, "not found is {{NSNotFound, 0}}");
    }
    found(r)
}

fn strings(a: &NSArray<NSString>) -> Vec<String> {
    (0..a.count()).map(|i| a.objectAtIndex(i).to_string()).collect()
}

#[test]
fn compare_orders() {
    // Default comparison is by code point of the decomposed text; literal
    // comparison is by UTF-16 unit.
    assert_eq!(cmp("\u{FFFD}", "\u{1F600}", 0), -1);
    assert_eq!(cmp("\u{FFFD}", "\u{1F600}", LITERAL), 1);
    assert_eq!(cmp("\u{E000}", "\u{1F600}", 0), -1);
    assert_eq!(cmp("à", "b", 0), -1);
    assert_eq!(cmp("z", "É", 0), 1);
    assert_eq!(cmp("a", "B", 0), 1);
    assert_eq!(cmp("ab", "abc", 0), -1);
    assert_eq!(cmp("", "a", 0), -1);
    assert_eq!(cmp("", "", 0), 0);
    assert_eq!(cmp("ß", "ss", 0), 1);
    // Canonical equivalence.
    assert_eq!(cmp("é", "e\u{301}", 0), 0);
    assert_eq!(cmp("é", "e\u{301}", LITERAL), 1);
}

#[test]
fn compare_options() {
    assert_eq!(cmp("a", "B", CI), -1);
    assert_eq!(cmp("A", "a", CI), 0);
    assert_eq!(cmp("A", "a", CI | FORCED), -1);
    assert_eq!(cmp("a", "A", CI | FORCED), 1);
    assert_eq!(cmp("file2", "file10", 0), 1);
    assert_eq!(cmp("file2", "file10", NUMERIC), -1);
    assert_eq!(cmp("a01", "a1", NUMERIC), 0);
    assert_eq!(cmp("a1", "a01", NUMERIC), 0);
    assert_eq!(cmp("a01", "a1", NUMERIC | FORCED), 1);
    assert_eq!(cmp("x9y", "x10y", NUMERIC), -1);
    assert_eq!(cmp("1.5", "1.10", NUMERIC), -1);
    assert_eq!(cmp("resume", "résumé", 0), -1);
    assert_eq!(cmp("resume", "résumé", DIACRITIC), 0);
    assert_eq!(cmp("A", "\u{FF21}", 0), -1);
    assert_eq!(cmp("A", "\u{FF21}", WIDTH), 0);
    // Full case folding.
    assert_eq!(cmp("straße", "STRASSE", CI), 0);
    assert_eq!(cmp("strasse", "straße", CI), 0);
    assert_eq!(cmp("ß", "ss", CI), 0);
    // No Turkish dotted and dotless I without a locale.
    assert_eq!(cmp("İ", "i", CI), 1);
    assert_eq!(cmp("ı", "I", CI), 1);
    assert_eq!(cmp("ı", "i", CI), 1);
    // A range of the receiver against the whole argument.
    let r = s("xxabcxx").compare_options_range(&s("abc"), NSStringCompareOptions(0), NSRange::new(2, 3));
    assert_eq!(r, NSComparisonResult::Same);
    let r = s("xxABCxx").compare_options_range(&s("abc"), NSStringCompareOptions(CI), NSRange::new(2, 3));
    assert_eq!(r, NSComparisonResult::Same);
    assert_eq!(s("Hello").caseInsensitiveCompare(&s("hello")), NSComparisonResult::Same);
    assert_eq!(s("a").compare(&s("b")), NSComparisonResult::Ascending);
}

#[test]
fn localized_sort_orders() {
    let list = [
        "apple",
        "Apple",
        "APPLE",
        "banana",
        "Banana",
        "file10",
        "file2",
        "file1",
        "résumé",
        "resume",
        "Resume",
        "zebra",
        "Zebra",
        "a b",
        "ab",
        "a-b",
        "a_b",
        "10",
        "9",
        "é",
        "e",
        "f",
        "Ångström",
        "angstrom",
    ];
    let sorted = |f: fn(&NSString, &NSString) -> NSComparisonResult| {
        let mut v: Vec<Retained<NSString>> = list.iter().map(|t| s(t)).collect();
        v.sort_by(|a, b| (f(a, b) as isize).cmp(&0));
        v.iter().map(|x| x.to_string()).collect::<Vec<_>>()
    };
    assert_eq!(
        sorted(|a, b| a.compare(b)),
        [
            "10",
            "9",
            "APPLE",
            "Apple",
            "Ångström",
            "Banana",
            "Resume",
            "Zebra",
            "a b",
            "a-b",
            "a_b",
            "ab",
            "angstrom",
            "apple",
            "banana",
            "e",
            "é",
            "f",
            "file1",
            "file10",
            "file2",
            "resume",
            "résumé",
            "zebra"
        ]
    );
    assert_eq!(
        sorted(|a, b| a.caseInsensitiveCompare(b)),
        [
            "10",
            "9",
            "a b",
            "a-b",
            "a_b",
            "ab",
            "angstrom",
            "apple",
            "Apple",
            "APPLE",
            "Ångström",
            "banana",
            "Banana",
            "e",
            "é",
            "f",
            "file1",
            "file10",
            "file2",
            "resume",
            "Resume",
            "résumé",
            "zebra",
            "Zebra"
        ]
    );
    let localized = [
        "10",
        "9",
        "a b",
        "a_b",
        "a-b",
        "ab",
        "angstrom",
        "Ångström",
        "apple",
        "Apple",
        "APPLE",
        "banana",
        "Banana",
        "e",
        "é",
        "f",
        "file1",
        "file10",
        "file2",
        "resume",
        "Resume",
        "résumé",
        "zebra",
        "Zebra",
    ];
    assert_eq!(sorted(|a, b| a.localizedCompare(b)), localized);
    assert_eq!(sorted(|a, b| a.localizedCaseInsensitiveCompare(b)), localized);
    assert_eq!(
        sorted(|a, b| a.localizedStandardCompare(b)),
        [
            "9",
            "10",
            "a b",
            "a_b",
            "a-b",
            "ab",
            "angstrom",
            "Ångström",
            "apple",
            "Apple",
            "APPLE",
            "banana",
            "Banana",
            "e",
            "é",
            "f",
            "file1",
            "file2",
            "file10",
            "resume",
            "Resume",
            "résumé",
            "zebra",
            "Zebra"
        ]
    );
}

#[test]
fn localized_ties() {
    let std = |a: &str, b: &str| s(a).localizedStandardCompare(&s(b)) as isize;
    let ci = |a: &str, b: &str| s(a).localizedCaseInsensitiveCompare(&s(b)) as isize;
    let loc = |a: &str, b: &str| s(a).localizedCompare(&s(b)) as isize;
    // The standard (Finder) order sees case, and then leading zeros and
    // anything else, so it never calls different strings the same.
    assert_eq!([std("a", "A"), std("A", "a"), std("Ab", "aB"), std("aB", "Ab")], [-1, 1, 1, -1]);
    assert_eq!([std("file1", "file01"), std("file01", "file1"), std("file2", "file10")], [-1, 1, -1]);
    assert_eq!([std("a1", "a01"), std("a01", "a001"), std("007", "7"), std("0", "00")], [-1, -1, 1, -1]);
    assert_eq!([std("ß", "ss"), std("\u{FB01}", "fi"), std("\u{FF21}", "A"), std("1", "\u{661}")], [1, 1, 1, -1]);
    assert_eq!([std("x9", "x09y"), std("1.5", "1.10"), std("é", "e"), std("A", "b")], [-1, -1, 1, -1]);
    assert_eq!([ci("a", "A"), ci("ß", "ss"), ci("file1", "file01"), ci("é", "e")], [0, 0, 1, 1]);
    assert_eq!([loc("a", "A"), loc("A", "a"), loc("ß", "ss"), loc("file2", "file10")], [-1, 1, 1, 1]);
    // So the order of a sort doesn't depend on the order it starts in.
    let list = ["APPLE", "file01", "Apple", "file1", "apple", "B", "b", "a", "A", "file001", "File1", "FILE01"];
    let expected = ["a", "A", "apple", "Apple", "APPLE", "b", "B", "file1", "file01", "file001", "File1", "FILE01"];
    let mut rotated = list.to_vec();
    rotated.rotate_left(5);
    for order in [list.to_vec(), list.iter().rev().copied().collect(), rotated] {
        let mut v: Vec<Retained<NSString>> = order.iter().map(|t| s(t)).collect();
        v.sort_by(|a, b| (a.localizedStandardCompare(b) as isize).cmp(&0));
        assert_eq!(v.iter().map(|x| x.to_string()).collect::<Vec<_>>(), expected);
    }
}

#[test]
fn prefixes_and_empty_arguments() {
    // An empty needle is never found or contained.
    assert_eq!(find("abc", "", 0), None);
    assert_eq!(find("", "a", 0), None);
    assert_eq!(find("", "", 0), None);
    assert!(!s("abc").hasPrefix(&s("")));
    assert!(!s("abc").hasSuffix(&s("")));
    assert!(!s("abc").containsString(&s("")));
    // Prefixes and suffixes are literal.
    assert!(!s("éa").hasPrefix(&s("e\u{301}")));
    assert!(s("e\u{301}a").hasPrefix(&s("e")));
    assert!(!s("e\u{301}x").hasPrefix(&s("é")));
    assert!(!s("aé").hasSuffix(&s("e\u{301}")));
    assert!(s("hello").hasPrefix(&s("he")) && s("hello").hasSuffix(&s("llo")));
    assert!(!s("he").hasPrefix(&s("hello")));
}

#[test]
fn range_of_string() {
    assert_eq!(find("abc", "x", 0), None);
    assert_eq!(find("abcabc", "bc", 0), Some((1, 2)));
    assert_eq!(find("abcabc", "bc", BACKWARDS), Some((4, 2)));
    assert_eq!(find("abcabc", "bc", ANCHORED), None);
    assert_eq!(find("abcabc", "ab", ANCHORED), Some((0, 2)));
    assert_eq!(find("abcabc", "bc", ANCHORED | BACKWARDS), Some((4, 2)));
    assert_eq!(find("abcabc", "ab", ANCHORED | BACKWARDS), None);
    let r = |loc, len| {
        found(s("abcabc").rangeOfString_options_range(&s("bc"), NSStringCompareOptions(0), NSRange::new(loc, len)))
    };
    assert_eq!(r(2, 4), Some((4, 2)));
    assert_eq!(r(2, 2), None);
    assert_eq!(find("Hello World", "WORLD", CI), Some((6, 5)));
    assert_eq!(find("a🎉b🎉c", "b🎉", 0), Some((3, 3)));
    // Non-literal matches whole composed character sequences.
    assert_eq!(find("xe\u{301}y", "e", 0), None);
    assert_eq!(find("xe\u{301}y", "e", LITERAL), Some((1, 1)));
    assert_eq!(find("xéy", "e\u{301}", 0), Some((1, 1)));
    assert_eq!(find("xe\u{301}y", "é", 0), Some((1, 2)));
    // Folding changes lengths.
    assert_eq!(find("die straße", "STRASSE", CI), Some((4, 6)));
    assert_eq!(find("DIE STRASSE", "straße", CI), Some((4, 7)));
    assert_eq!(find("straße", "ss", CI), Some((4, 1)));
    assert_eq!(find("strasse", "ß", CI), Some((4, 2)));
    assert_eq!(find("my résumé", "resume", DIACRITIC), Some((3, 6)));
    assert_eq!(find("xe\u{301}y", "e", DIACRITIC), Some((1, 2)));
    assert_eq!(find("x\u{FF21}\u{FF22}y", "AB", WIDTH), Some((1, 2)));
    // The localized conveniences.
    assert_eq!(found(s("Résumé.PDF").localizedStandardRangeOfString(&s("resume"))), Some((0, 6)));
    assert!(s("Straße").localizedCaseInsensitiveContainsString(&s("STRASSE")));
    assert!(s("Résumé").localizedStandardContainsString(&s("resume")));
    assert!(s("hello").containsString(&s("ell")));
}

#[test]
fn ascii_needles_in_other_text() {
    // An ASCII needle in text that is not all ASCII: still whole composed
    // character sequences, still canonical equivalence and case folding.
    assert_eq!(find("漢字 the 🎉", "the", 0), Some((3, 3)));
    assert_eq!(find("漢字 THE 🎉", "the", CI), Some((3, 3)));
    assert_eq!(find("the 漢字 the", "the", BACKWARDS), Some((7, 3)));
    assert_eq!(find("é the", "the", ANCHORED), None);
    assert_eq!(find("é the", "the", ANCHORED | BACKWARDS), Some((2, 3)));
    assert_eq!(find("ae\u{301} ae", "ae", 0), Some((4, 2)));
    assert_eq!(find("ae ae\u{301}", "ae", BACKWARDS), Some((0, 2)));
    assert_eq!(find("aaa\u{301}", "aa", BACKWARDS), Some((0, 2)));
    assert_eq!(find("x\u{212A}y", "K", 0), Some((1, 1)));
    assert_eq!(find("x\u{212A}y", "k", CI), Some((1, 1)));
    assert_eq!(find("die straße", "strasse", CI), Some((4, 6)));
    let all = |h: &str, n: &str| s(h).stringByReplacingOccurrencesOfString_withString(&s(n), &s("_")).to_string();
    assert_eq!(all("e\u{301}e é e", "e"), "e\u{301}_ é _");
    assert_eq!(all("漢aa字aaa", "aa"), "漢_字_a");
}

#[test]
fn regular_expression_search() {
    assert_eq!(find("abc  \t", "[ \t]+$", REGEX), Some((3, 3)));
    assert_eq!(find("abc  \t\n", "[ \t]+$", REGEX), Some((3, 3)));
    assert_eq!(find("a  \nb", "[ \t]+$", REGEX), None);
    assert_eq!(find("abc", "^b", REGEX), None);
    assert_eq!(find("a\nbc", "^b", REGEX), None);
    assert_eq!(find("hello world", "wor|hel", REGEX), Some((0, 3)));
    assert_eq!(find("abc123", "[0-9]+", REGEX), Some((3, 3)));
    assert_eq!(find("ABC", "b+", REGEX | CI), Some((1, 1)));
    // Backwards makes no difference to a regular expression search.
    assert_eq!(find("abab", "ab", REGEX | BACKWARDS), Some((0, 2)));
    assert_eq!(find("abab", "ab", REGEX | ANCHORED), Some((0, 2)));
    assert_eq!(find("xab", "ab", REGEX | ANCHORED), None);
    assert_eq!(find("abc", "(", REGEX), None, "an invalid pattern finds nothing");
    assert_eq!(find("abc", "x*", REGEX), Some((0, 0)), "an empty match is a match");
    assert_eq!(find("a\nb", "a.b", REGEX), None);
    assert_eq!(find("foo bar", "\\bbar", REGEX), Some((4, 3)));
    assert_eq!(find("xéy", "é", REGEX), Some((1, 1)));
    assert_eq!(find("a🎉b", ".b", REGEX), Some((1, 3)));
    let replace = |h: &str, p: &str, t: &str| {
        let hs = s(h);
        hs.stringByReplacingOccurrencesOfString_withString_options_range(
            &s(p),
            &s(t),
            NSStringCompareOptions(REGEX),
            NSRange::new(0, hs.length()),
        )
        .to_string()
    };
    assert_eq!(replace("John Smith", "(\\w+) (\\w+)", "$2, $1"), "Smith, John");
    assert_eq!(replace("abc", "x*", "-"), "-a-b-c-");
    assert_eq!(replace("a1", "(\\d)", "\\$$1"), "a$1");
    assert_eq!(replace("ab", "b", "[$0]"), "a[b]");
    assert_eq!(replace("trailing   \nspace  ", "[ \t]+$", ""), "trailing   \nspace");
}

#[test]
fn composed_character_sequences() {
    let at = |t: &str, i: usize| {
        let r = s(t).rangeOfComposedCharacterSequenceAtIndex(i);
        (r.location, r.length)
    };
    let family = "a👨\u{200d}👩\u{200d}👧b";
    assert_eq!(at(family, 1), (1, 8));
    assert_eq!(at(family, 3), (1, 8));
    assert_eq!(at(family, 9), (9, 1));
    let flags = "🇫🇷🇩🇪🇺🇸";
    assert_eq!([0, 3, 4, 7, 8, 11].map(|i| at(flags, i)), [(0, 4), (0, 4), (4, 4), (4, 4), (8, 4), (8, 4)]);
    assert_eq!(at("🇫🇷🇩🇪🇺", 9), (8, 2));
    assert_eq!(at("👍🏽x", 1), (0, 4));
    assert_eq!(at("a\r\nb", 1), (1, 1));
    assert_eq!(at("a\r\nb", 2), (2, 1));
    assert_eq!(at("\u{1100}\u{1161}\u{11A8}x", 1), (0, 3));
    assert_eq!(at("e\u{301}\u{302}x", 2), (0, 3));
    assert_eq!(at("क्षि", 2), (0, 4));
    assert_eq!(at("\u{0600}x", 0), (0, 1));
    let lone = s("🎉").substringToIndex(1).stringByAppendingString(&s("x"));
    let r = lone.rangeOfComposedCharacterSequenceAtIndex(0);
    assert_eq!((r.location, r.length), (0, 1));
    let r = s(family).rangeOfComposedCharacterSequencesForRange(NSRange::new(2, 2));
    assert_eq!((r.location, r.length), (1, 8));
    let r = s(family).rangeOfComposedCharacterSequencesForRange(NSRange::new(3, 0));
    assert_eq!((r.location, r.length), (1, 8));
    // An empty range at the end (a caret after the last character) stays.
    let for_range = |t: &str, loc: usize, len: usize| {
        let r = s(t).rangeOfComposedCharacterSequencesForRange(NSRange::new(loc, len));
        (r.location, r.length)
    };
    assert_eq!(for_range("ab", 2, 0), (2, 0));
    assert_eq!(for_range("a🎉", 3, 0), (3, 0));
    assert_eq!(for_range("e\u{301}x", 3, 0), (3, 0));
    assert_eq!(for_range("", 0, 0), (0, 0));
    assert_eq!(for_range("e\u{301}x", 1, 0), (0, 2));
    assert_eq!(for_range("a🎉", 1, 2), (1, 2));
}

#[test]
fn character_set_searches() {
    let t = s("ab🎉c 1");
    let digits = NSCharacterSet::decimalDigitCharacterSet();
    let letters = NSCharacterSet::letterCharacterSet();
    let party = NSCharacterSet::characterSetWithCharactersInString(&s("🎉"));
    let opts = NSStringCompareOptions;
    assert_eq!(found(t.rangeOfCharacterFromSet(&digits)), Some((6, 1)));
    assert_eq!(found(t.rangeOfCharacterFromSet(&party)), Some((2, 2)));
    assert_eq!(found(t.rangeOfCharacterFromSet(&NSCharacterSet::symbolCharacterSet())), Some((2, 2)));
    assert_eq!(found(t.rangeOfCharacterFromSet_options(&letters, opts(BACKWARDS))), Some((4, 1)));
    assert_eq!(found(t.rangeOfCharacterFromSet_options(&letters, opts(ANCHORED))), Some((0, 1)));
    assert_eq!(found(t.rangeOfCharacterFromSet_options(&digits, opts(ANCHORED))), None);
    assert_eq!(found(t.rangeOfCharacterFromSet_options_range(&letters, opts(0), NSRange::new(2, 3))), Some((4, 1)));
    let lone = s("🎉").substringToIndex(1);
    assert_eq!(found(lone.rangeOfCharacterFromSet(&party)), None);
}

#[test]
fn replacing_and_padding() {
    let rep =
        |h: &str, t: &str, w: &str| s(h).stringByReplacingOccurrencesOfString_withString(&s(t), &s(w)).to_string();
    assert_eq!(rep("abc", "", "x"), "abc");
    assert_eq!(rep("aaa", "aa", "b"), "ba");
    assert_eq!(rep("aaaa", "aa", "b"), "bb");
    assert_eq!(rep("a.b.c", ".", "--"), "a--b--c");
    assert_eq!(rep("héllo wörld", "ö", "o"), "héllo world");
    let m = NSMutableString::from_str("aaaa");
    assert_eq!(
        m.replaceOccurrencesOfString_withString_options_range(
            &s("aa"),
            &s("b"),
            NSStringCompareOptions(0),
            NSRange::new(0, 4)
        ),
        2
    );
    let m = NSMutableString::from_str("Aa aA");
    assert_eq!(
        m.replaceOccurrencesOfString_withString_options_range(
            &s("aa"),
            &s("x"),
            NSStringCompareOptions(CI),
            NSRange::new(0, 5)
        ),
        2
    );
    assert_eq!(m.to_string(), "x x");
    let m = NSMutableString::from_str("abcabc");
    let n = m.replaceOccurrencesOfString_withString_options_range(
        &s("bc"),
        &s("X"),
        NSStringCompareOptions(BACKWARDS | ANCHORED),
        NSRange::new(0, 6),
    );
    assert_eq!((n, m.to_string()), (1, "abcaX".into()));
    assert_eq!(
        s("hello").stringByReplacingCharactersInRange_withString(NSRange::new(1, 3), &s("EY")).to_string(),
        "hEYo"
    );
    let r = s("aAaA").stringByReplacingOccurrencesOfString_withString_options_range(
        &s("a"),
        &s("x"),
        NSStringCompareOptions(CI),
        NSRange::new(1, 2),
    );
    assert_eq!(r.to_string(), "axxA");
    let pad = |h: &str, len: usize, p: &str, i: usize| {
        s(h).stringByPaddingToLength_withString_startingAtIndex(len, &s(p), i).to_string()
    };
    assert_eq!(pad("abc", 8, "xyz", 1), "abcyzxyz");
    assert_eq!(pad("abcdef", 3, ".", 0), "abc");
    assert_eq!(pad("ab", 5, "12", 0), "ab121");
    assert_eq!(pad("", 3, "ab", 1), "bab");
}

#[test]
fn mutable_replacing_expands_templates() {
    let replace = |text: &str, pattern: &str, template: &str| {
        let m = NSMutableString::from_str(text);
        let n = m.replaceOccurrencesOfString_withString_options_range(
            &s(pattern),
            &s(template),
            NSStringCompareOptions(REGEX),
            NSRange::new(0, m.length()),
        );
        (n, m.to_string())
    };
    assert_eq!(replace("a1b22", "(\\d+)", "<$1>"), (2, "a<1>b<22>".into()));
    assert_eq!(replace("abab", "b", "\\$"), (2, "a$a$".into()));
    assert_eq!(replace("2024-05-06", "(\\d+)-(\\d+)-(\\d+)", "$3/$2/$1"), (1, "06/05/2024".into()));
    assert_eq!(replace("abc", "x", "y"), (0, "abc".into()));
    // Through an attributed string's mutable string, a subclass that sees
    // each edit.
    let a = objc2_foundation::NSMutableAttributedString::from_nsstring(&s("a1b22"));
    let n = a.mutableString().replaceOccurrencesOfString_withString_options_range(
        &s("(\\d+)"),
        &s("<$1>"),
        NSStringCompareOptions(REGEX),
        NSRange::new(0, 5),
    );
    assert_eq!((n, a.string().to_string()), (2, "a<1>b<22>".into()));
    // Many replacements in long mixed text agree with the immutable method.
    let text = "the café, THE naïve 漢字 the end 🎉 ".repeat(200);
    for options in [0, CI, LITERAL, BACKWARDS, DIACRITIC | CI] {
        let m = NSMutableString::from_str(&text);
        let whole = NSRange::new(0, m.length());
        let expected = s(&text).stringByReplacingOccurrencesOfString_withString_options_range(
            &s("the"),
            &s("a"),
            NSStringCompareOptions(options),
            whole,
        );
        let n = m.replaceOccurrencesOfString_withString_options_range(
            &s("the"),
            &s("a"),
            NSStringCompareOptions(options),
            whole,
        );
        assert_eq!(m.to_string(), expected.to_string(), "options {options}");
        assert_eq!(n, if options & CI != 0 { 600 } else { 400 }, "options {options}");
    }
}

#[test]
fn common_prefixes() {
    let prefix = |a: &str, b: &str, options: usize| {
        s(a).commonPrefixWithString_options(&s(b), NSStringCompareOptions(options)).to_string()
    };
    assert_eq!(prefix("abc", "abd", 0), "ab");
    assert_eq!(prefix("abc", "ABD", 0), "");
    assert_eq!(prefix("abc", "ABD", CI), "ab");
    assert_eq!(prefix("abc", "", 0), "");
    // A prefix may end where a sequence ends in both strings, even inside
    // another's fold (ß is SS). (Some cases where the other string ends
    // just after such a fold give a shorter prefix on macOS, depending on
    // where the fold falls; those aren't pinned here.)
    assert_eq!(prefix("STRASSE", "straße", CI), "STRASSE");
    assert_eq!(prefix("straße", "STRASSE", CI), "straße");
    assert_eq!(prefix("STRAS", "straße", CI), "STRA");
    assert_eq!(prefix("xSSy", "xßz", CI), "xSS");
    assert_eq!(prefix("xßy", "xSSz", CI), "xß");
    assert_eq!(prefix("aSSb", "aßc", CI), "aSS");
    assert_eq!(prefix("\u{FB01}le", "FILE", CI), "\u{FB01}le");
    assert_eq!(prefix("FILE", "\u{FB01}le", CI), "FILE");
    assert_eq!(prefix("FILEX", "\u{FB01}le", CI), "FILE");
    assert_eq!(prefix("résumé", "resume", DIACRITIC), "résumé");
    assert_eq!(prefix("résuméX", "resume", DIACRITIC), "résumé");
    assert_eq!(prefix("résumé", "resume", 0), "r");
    assert_eq!(prefix("e\u{301}x", "éy", 0), "e\u{301}");
    assert_eq!(prefix("éa", "e\u{301}b", 0), "é");
    assert_eq!(prefix("e\u{301}x", "éy", LITERAL), "");
    assert_eq!(prefix("ae\u{301}", "ae", 0), "a");
    assert_eq!(prefix("ae\u{301}", "ae", LITERAL), "ae");
    assert_eq!(prefix("x\u{FF21}B", "xAb", WIDTH | CI), "x\u{FF21}B");
    // Long shared starts, composed one way and decomposed the other.
    let long = "é straße ".repeat(400);
    let other = "e\u{301} STRASSE ".repeat(400);
    assert_eq!(prefix(&format!("{long}x"), &format!("{other}y"), CI), long);
    assert_eq!(prefix(&format!("{long}x"), &format!("{long}y"), 0), long);
    assert_eq!(prefix("a🎉b", "a🎉c", 0), "a🎉");
}

#[test]
fn lone_surrogates_in_searches() {
    let high = s("🎉").substringToIndex(1);
    let low = s("🎉").substringFromIndex(1);
    let r =
        |h: &str, n: &NSString, options: usize| found(s(h).rangeOfString_options(n, NSStringCompareOptions(options)));
    assert!(s("a🎉").containsString(&high));
    assert_eq!(r("a🎉", &high, 0), Some((1, 1)));
    assert_eq!(r("a🎉", &high, LITERAL), Some((1, 1)));
    assert_eq!(r("a🎉b", &low, 0), Some((2, 1)));
    assert_eq!(r("🎉🎉", &high, BACKWARDS), Some((2, 1)));
    assert_eq!(r("abc", &high, 0), None);
    let replaced = s("a🎉b").stringByReplacingOccurrencesOfString_withString(&high, &s("X"));
    let units: Vec<u16> = (0..replaced.length()).map(|i| replaced.characterAtIndex(i)).collect();
    assert_eq!(units, [0x61, 0x58, 0xDF89, 0x62]);
}

#[test]
fn forms_taking_a_locale() {
    // With no locale these are the plain forms.
    let r = unsafe {
        s("xxABCxx").compare_options_range_locale(&s("abc"), NSStringCompareOptions(CI), NSRange::new(2, 3), None)
    };
    assert_eq!(r, NSComparisonResult::Same);
    let r =
        unsafe { s("b").compare_options_range_locale(&s("a"), NSStringCompareOptions(0), NSRange::new(0, 1), None) };
    assert_eq!(r, NSComparisonResult::Descending);
    let r = s("Hello World").rangeOfString_options_range_locale(
        &s("WORLD"),
        NSStringCompareOptions(CI),
        NSRange::new(0, 11),
        None,
    );
    assert_eq!(found(r), Some((6, 5)));
    assert_eq!(s("straße").uppercaseStringWithLocale(None).to_string(), "STRASSE");
    assert_eq!(s("ÉCOLE").lowercaseStringWithLocale(None).to_string(), "école");
    assert_eq!(s("hello wORLD").capitalizedStringWithLocale(None).to_string(), "Hello World");
    assert_eq!(s("ÉCOLE").localizedLowercaseString().to_string(), "école");
    assert_eq!(s("école").localizedUppercaseString().to_string(), "ÉCOLE");
}

#[test]
fn case_mapping() {
    let case = |t: &str| {
        let x = s(t);
        (x.uppercaseString().to_string(), x.lowercaseString().to_string(), x.capitalizedString().to_string())
    };
    assert_eq!(case("straße"), ("STRASSE".into(), "straße".into(), "Straße".into()));
    assert_eq!(case("İstanbul"), ("İSTANBUL".into(), "i\u{307}stanbul".into(), "İstanbul".into()));
    assert_eq!(case("ǆemal"), ("ǄEMAL".into(), "ǆemal".into(), "ǅemal".into()));
    assert_eq!(case("ﬁle"), ("FILE".into(), "ﬁle".into(), "File".into()));
    assert_eq!(case("hello wORLD"), ("HELLO WORLD".into(), "hello world".into(), "Hello World".into()));
    assert_eq!(s("ΣΑΣ").lowercaseString().to_string(), "σας");
    assert_eq!(s("Σ").lowercaseString().to_string(), "σ");
    assert_eq!(s("ǆ").capitalizedString().to_string(), "ǅ");
    assert_eq!(
        s("hello world\tfoo\nbar-baz qux's quux_corge 3rd x3y a.b a,b a/b (a) \"a\" ¿a")
            .capitalizedString()
            .to_string(),
        "Hello World\tFoo\nBar-Baz Qux's Quux_Corge 3Rd X3Y A.B A,B A/B (A) \"A\" ¿A"
    );
    assert_eq!(s("istanbul").localizedUppercaseString().to_string(), "ISTANBUL");
    assert_eq!(s("hello world").localizedCapitalizedString().to_string(), "Hello World");
    assert_eq!(s("MiXeD").lowercaseStringWithLocale(None).to_string(), "mixed");
    assert_eq!(s("é").decomposedStringWithCanonicalMapping().length(), 2);
    assert_eq!(s("e\u{301}").precomposedStringWithCanonicalMapping().length(), 1);
    assert_eq!(s("ﬁ").decomposedStringWithCompatibilityMapping().to_string(), "fi");
    assert_eq!(s("①").precomposedStringWithCompatibilityMapping().to_string(), "1");
    let fold = |t: &str, o: usize| s(t).stringByFoldingWithOptions_locale(NSStringCompareOptions(o), None).to_string();
    assert_eq!(fold("Straße Résumé", CI), "strasse résumé");
    assert_eq!(fold("Résumé", DIACRITIC), "Resume");
    assert_eq!(fold("\u{FF21}\u{FF22}", WIDTH), "AB");
    assert_eq!(fold("Å\u{FF21}ß", CI | DIACRITIC | WIDTH), "aass");
}

const TERMINATED: &str = "ab\ncd\r\nef\rgh\u{2028}ij\u{2029}kl\u{85}mn";

#[test]
fn lines_and_paragraphs() {
    let t = s(TERMINATED);
    let lines: Vec<(usize, usize)> =
        (0..t.length()).map(|i| t.lineRangeForRange(NSRange::new(i, 0))).map(|r| (r.location, r.length)).collect();
    let expect = |spans: &[(usize, usize)]| -> Vec<(usize, usize)> {
        spans.iter().flat_map(|&(l, n)| std::iter::repeat_n((l, n), n)).collect()
    };
    assert_eq!(lines, expect(&[(0, 3), (3, 4), (7, 3), (10, 3), (13, 3), (16, 3), (19, 2)]));
    let paragraphs: Vec<(usize, usize)> =
        (0..t.length()).map(|i| t.paragraphRangeForRange(NSRange::new(i, 0))).map(|r| (r.location, r.length)).collect();
    // U+2028 and U+0085 end lines, not paragraphs.
    assert_eq!(paragraphs, expect(&[(0, 3), (3, 4), (7, 3), (10, 6), (16, 5)]));
    let line = |text: &str, loc, len| {
        let r = s(text).lineRangeForRange(NSRange::new(loc, len));
        (r.location, r.length)
    };
    assert_eq!(line(TERMINATED, 1, 5), (0, 7));
    assert_eq!(line(TERMINATED, 21, 0), (19, 2));
    assert_eq!(line("", 0, 0), (0, 0));
    assert_eq!(line("ab\n", 3, 0), (3, 0));
    assert_eq!(line("ab\r\ncd", 2, 1), (0, 4));
    assert_eq!(line("ab\r\ncd", 0, 3), (0, 4));
    let (mut start, mut end, mut contents) = (0, 0, 0);
    unsafe { t.getLineStart_end_contentsEnd_forRange(&mut start, &mut end, &mut contents, NSRange::new(3, 1)) };
    assert_eq!((start, end, contents), (3, 7, 5));
    unsafe { t.getLineStart_end_contentsEnd_forRange(&mut start, &mut end, &mut contents, NSRange::new(6, 0)) };
    assert_eq!((start, end, contents), (3, 7, 5));
    unsafe { t.getParagraphStart_end_contentsEnd_forRange(&mut start, &mut end, &mut contents, NSRange::new(12, 0)) };
    assert_eq!((start, end, contents), (10, 16, 15));
    unsafe {
        t.getLineStart_end_contentsEnd_forRange(
            std::ptr::null_mut(),
            &mut end,
            std::ptr::null_mut(),
            NSRange::new(0, 0),
        )
    };
    assert_eq!(end, 3);
}

/// A substring (if passed), its range and its enclosing range.
type Seen = (Option<String>, (usize, usize), (usize, usize));

fn enumerate(text: &str, range: NSRange, options: usize) -> Vec<Seen> {
    let out = Rc::new(RefCell::new(Vec::new()));
    let seen = out.clone();
    let block = RcBlock::new(move |sub: *mut NSString, r: NSRange, e: NSRange, _stop: NonNull<Bool>| {
        let sub = (!sub.is_null()).then(|| unsafe { &*sub }.to_string());
        seen.borrow_mut().push((sub, (r.location, r.length), (e.location, e.length)));
    });
    s(text).enumerateSubstringsInRange_options_usingBlock(range, NSStringEnumerationOptions(options), &block);
    out.take()
}

/// Enumerate a mutable string by `options`, letting `edit` change it for
/// each piece: what the block saw and the text left.
fn enumerate_editing(
    text: &str,
    options: usize,
    edit: impl Fn(&NSMutableString, NSRange, NSRange) + 'static,
) -> (Vec<Seen>, String) {
    let m = NSMutableString::from_str(text);
    let out = Rc::new(RefCell::new(Vec::new()));
    let (seen, target) = (out.clone(), m.clone());
    let block = RcBlock::new(move |sub: *mut NSString, r: NSRange, e: NSRange, _stop: NonNull<Bool>| {
        let sub = (!sub.is_null()).then(|| unsafe { &*sub }.to_string());
        seen.borrow_mut().push((sub, (r.location, r.length), (e.location, e.length)));
        edit(&target, r, e);
    });
    let all = NSRange::new(0, m.length());
    m.enumerateSubstringsInRange_options_usingBlock(all, NSStringEnumerationOptions(options), &block);
    drop(block);
    (out.take(), m.to_string())
}

#[test]
fn enumeration_follows_edits_to_a_mutable_string() {
    const BY_WORDS: usize = 3;
    const REVERSE: usize = 1 << 8;
    let seen = |t: &str, r: (usize, usize), e: (usize, usize)| (Some(t.to_string()), r, e);
    let delete = |m: &NSMutableString, _: NSRange, e: NSRange| m.deleteCharactersInRange(e);
    assert_eq!(
        enumerate_editing("one two three", BY_WORDS, delete),
        (vec![seen("one", (0, 3), (0, 4)), seen("two", (0, 3), (0, 4)), seen("three", (0, 5), (0, 5))], "".into())
    );
    let upper = |m: &NSMutableString, r: NSRange, _: NSRange| {
        m.replaceCharactersInRange_withString(r, &s("WORD"));
    };
    assert_eq!(
        enumerate_editing("a bb ccc", BY_WORDS, upper),
        (
            vec![seen("a", (0, 1), (0, 2)), seen("bb", (5, 2), (5, 3)), seen("ccc", (10, 3), (10, 3))],
            "WORD WORD WORD".into()
        )
    );
    assert_eq!(
        enumerate_editing("one two three", BY_WORDS | REVERSE, delete),
        (vec![seen("three", (8, 5), (8, 5)), seen("two", (4, 3), (4, 4)), seen("one", (0, 3), (0, 4))], "".into())
    );
    let lines = |m: &NSMutableString, r: NSRange, _: NSRange| m.replaceCharactersInRange_withString(r, &s("<>"));
    assert_eq!(
        enumerate_editing("a\nbbb\ncc", 0, lines),
        (vec![seen("a", (0, 1), (0, 2)), seen("bbb", (3, 3), (3, 4)), seen("cc", (6, 2), (6, 2))], "<>\n<>\n<>".into())
    );
}

#[test]
fn enumerating_substrings() {
    let text = "Hello, world! It's 3.14 now.\nMr. Smith 漢字 e\u{301}🇫🇷\r\nLast";
    let all = NSRange::new(0, s(text).length());
    let pieces =
        |options| enumerate(text, all, options).into_iter().map(|(s, r, e)| (s.unwrap(), r, e)).collect::<Vec<_>>();
    let lines = pieces(0);
    assert_eq!(
        lines,
        [
            ("Hello, world! It's 3.14 now.".into(), (0, 28), (0, 29)),
            ("Mr. Smith 漢字 e\u{301}🇫🇷".into(), (29, 19), (29, 21)),
            ("Last".into(), (50, 4), (50, 4)),
        ]
    );
    assert_eq!(pieces(1), lines);
    let composed = pieces(2);
    assert_eq!(composed.len(), 50);
    assert_eq!(composed[42], ("e\u{301}".into(), (42, 2), (42, 2)));
    assert_eq!(composed[43], ("🇫🇷".into(), (44, 4), (44, 4)));
    assert_eq!(composed[44], ("\r".into(), (48, 1), (48, 1)));
    assert_eq!(composed[45], ("\n".into(), (49, 1), (49, 1)));
    let words = pieces(3);
    assert_eq!(
        words[..6],
        [
            ("Hello".into(), (0, 5), (0, 7)),
            ("world".into(), (7, 5), (7, 7)),
            ("It's".into(), (14, 4), (14, 5)),
            ("3.14".into(), (19, 4), (19, 5)),
            ("now".into(), (24, 3), (24, 5)),
            ("Mr".into(), (29, 2), (29, 4)),
        ]
    );
    assert_eq!(words.last().unwrap(), &("Last".into(), (50, 4), (50, 4)));
    let sentences = pieces(4);
    assert_eq!(sentences[0], ("Hello, world! ".into(), (0, 14), (0, 14)));
    assert_eq!(sentences[1], ("It's 3.14 now.\n".into(), (14, 15), (14, 15)));
    assert_eq!(sentences.last().unwrap(), &("Last".into(), (50, 4), (50, 4)));
    // Reverse order, no substrings, clipped to the range.
    let got = enumerate("ab cd ef", NSRange::new(1, 6), 3 | (1 << 8) | (1 << 9));
    assert_eq!(got, [(None, (6, 1), (6, 1)), (None, (3, 2), (3, 3)), (None, (1, 1), (1, 2))]);
    // Stop.
    let out = Rc::new(RefCell::new(0));
    let count = out.clone();
    let block = RcBlock::new(move |_: *mut NSString, _: NSRange, _: NSRange, stop: NonNull<Bool>| {
        *count.borrow_mut() += 1;
        unsafe { *stop.as_ptr() = Bool::YES };
    });
    s("a b c").enumerateSubstringsInRange_options_usingBlock(NSRange::new(0, 5), NSStringEnumerationOptions(3), &block);
    assert_eq!(*out.borrow(), 1);
    // enumerateLinesUsingBlock: leaves terminators out and adds no line
    // after a final one.
    let lines = Rc::new(RefCell::new(Vec::new()));
    let seen = lines.clone();
    let block = RcBlock::new(move |l: NonNull<NSString>, _: NonNull<Bool>| {
        seen.borrow_mut().push(unsafe { l.as_ref() }.to_string())
    });
    s("a\nb\r\nc\n\nd\n").enumerateLinesUsingBlock(&block);
    assert_eq!(*lines.borrow(), ["a", "b", "c", "", "d"]);
}

#[test]
#[cfg_attr(not(target_vendor = "apple"), ignore = "needs NSArray from the collections workstream")]
fn components_and_trimming() {
    let comps = |t: &str, sep: &str| strings(&s(t).componentsSeparatedByString(&s(sep)));
    assert_eq!(comps("", ","), [""]);
    assert_eq!(comps(",a,,b,", ","), ["", "a", "", "b", ""]);
    assert_eq!(comps("abc", ""), ["abc"]);
    assert_eq!(comps("a--b", "--"), ["a", "b"]);
    assert_eq!(comps("aaa", "aa"), ["", "a"]);
    let ws = NSCharacterSet::whitespaceAndNewlineCharacterSet();
    assert_eq!(strings(&s(" a b\n\tc ").componentsSeparatedByCharactersInSet(&ws)), ["", "a", "b", "", "c", ""]);
    let party = NSCharacterSet::characterSetWithCharactersInString(&s("🎉"));
    assert_eq!(strings(&s("x🎉y").componentsSeparatedByCharactersInSet(&party)), ["x", "y"]);
}

#[test]
fn trimming() {
    let ws = NSCharacterSet::whitespaceAndNewlineCharacterSet();
    assert_eq!(s("  a b  ").stringByTrimmingCharactersInSet(&ws).to_string(), "a b");
    assert_eq!(s(" \n ").stringByTrimmingCharactersInSet(&ws).to_string(), "");
    assert_eq!(s("").stringByTrimmingCharactersInSet(&ws).to_string(), "");
    // Trimming tests single UTF-16 units, so it never trims a character
    // outside the Basic Multilingual Plane.
    let party = NSCharacterSet::characterSetWithCharactersInString(&s("🎉"));
    assert_eq!(s("🎉a🎉").stringByTrimmingCharactersInSet(&party).to_string(), "🎉a🎉");
    assert_eq!(s("🎉a🎉").stringByTrimmingCharactersInSet(&NSCharacterSet::symbolCharacterSet()).to_string(), "🎉a🎉");
    assert_eq!(
        s("xxhixx")
            .stringByTrimmingCharactersInSet(&NSCharacterSet::characterSetWithCharactersInString(&s("x")))
            .to_string(),
        "hi"
    );
    // A set holding both halves of a pair trims the character; holding
    // only the outer half, it splits it.
    let others = NSCharacterSet::alphanumericCharacterSet().invertedSet();
    let trim = |t: &str, set: &NSCharacterSet| {
        let r = s(t).stringByTrimmingCharactersInSet(set);
        (0..r.length()).map(|i| r.characterAtIndex(i)).collect::<Vec<u16>>()
    };
    assert_eq!(trim("🎉hello🎉", &others), "hello".encode_utf16().collect::<Vec<_>>());
    assert_eq!(trim("hi!👋", &others), [0x68, 0x69]);
    assert_eq!(trim("👋", &others), []);
    let high = NSCharacterSet::characterSetWithRange(NSRange::new(0xD83C, 1));
    assert_eq!(trim("🎉", &high), [0xDF89]);
    assert_eq!(trim("a🎉", &high), [0x61, 0xD83C, 0xDF89]);
    let low = NSCharacterSet::characterSetWithRange(NSRange::new(0xDF89, 1));
    assert_eq!(trim("🎉", &low), [0xD83C]);
    assert_eq!(trim("🎉a", &low), [0xD83C, 0xDF89, 0x61]);
}

#[test]
fn percent_encoding() {
    let path = NSCharacterSet::URLPathAllowedCharacterSet();
    let enc = |t: &str| s(t).stringByAddingPercentEncodingWithAllowedCharacters(&path).map(|x| x.to_string());
    assert_eq!(enc("a b/c?d"), Some("a%20b/c%3Fd".into()));
    assert_eq!(enc("é🎉"), Some("%C3%A9%F0%9F%8E%89".into()));
    // Only ASCII characters are ever kept, whatever the set holds.
    let letters = NSCharacterSet::alphanumericCharacterSet();
    let kept = s("é b~1").stringByAddingPercentEncodingWithAllowedCharacters(&letters).unwrap();
    assert_eq!((kept.to_string(), kept.length()), ("%C3%A9%20b%7E1".into(), 14));
    let query = NSCharacterSet::URLQueryAllowedCharacterSet();
    let q = s("q=a b&x=é").stringByAddingPercentEncodingWithAllowedCharacters(&query).unwrap();
    assert_eq!(q.to_string(), "q=a%20b&x=%C3%A9");
    let dec = |t: &str| s(t).stringByRemovingPercentEncoding().map(|x| x.to_string());
    assert_eq!(dec("a%20b%2Fc"), Some("a b/c".into()));
    assert_eq!(dec("%C3%A9"), Some("é".into()));
    assert_eq!(dec("100%"), None);
    assert_eq!(dec("%zz"), None);
    assert_eq!(dec("%FF"), None);
    assert_eq!(dec("%+1"), None);
    assert_eq!(dec("%e9"), None);
    assert_eq!(dec("%c3%a9!"), Some("é!".into()));
    assert_eq!(dec("plain"), Some("plain".into()));
}

#[test]
fn numbers() {
    struct N {
        text: &'static str,
        double: f64,
        int: i32,
        integer: isize,
        bool: bool,
    }
    let n = |text, double, int, integer, bool| N { text, double, int, integer, bool };
    let cases = [
        n("", 0.0, 0, 0, false),
        n("42", 42.0, 42, 42, true),
        n("  42", 42.0, 42, 42, true),
        n("\t\n42", 0.0, 0, 0, false),
        n("+7", 7.0, 7, 7, true),
        n("-7", -7.0, -7, -7, true),
        n("- 7", 0.0, -7, -7, false),
        n("42abc", 42.0, 42, 42, true),
        n("abc42", 0.0, 0, 0, false),
        n("1e3", 1000.0, 1, 1, true),
        n("1.5e", 1.5, 1, 1, true),
        n(".5", 0.5, 0, 0, false),
        n("5.", 5.0, 5, 5, true),
        n("-.5", -0.5, 0, 0, false),
        n("inf", 0.0, 0, 0, false),
        n("nan", 0.0, 0, 0, false),
        n("0x1A", 0.0, 0, 0, false),
        n("007", 7.0, 7, 7, true),
        n("1,5", 1.0, 1, 1, true),
        n("\u{663}", 3.0, 3, 3, false),
        n("3\u{663}", 33.0, 33, 33, true),
        n("\u{FF11}\u{FF12}", 12.0, 12, 12, false),
        n("99999999999999999999", 1e20, i32::MAX, isize::MAX, true),
        n("-99999999999999999999", -1e20, i32::MIN, isize::MIN, true),
        n("2147483648", 2147483648.0, i32::MAX, 2147483648, true),
        n("YES", 0.0, 0, 0, true),
        n("yes", 0.0, 0, 0, true),
        n("t", 0.0, 0, 0, true),
        n("T", 0.0, 0, 0, true),
        n("no", 0.0, 0, 0, false),
        n("-1", -1.0, -1, -1, true),
        n("01", 1.0, 1, 1, true),
        n("00", 0.0, 0, 0, false),
        n("0.1", 0.1, 0, 0, false),
        n("  yes", 0.0, 0, 0, true),
        n(" +5", 5.0, 5, 5, true),
        n("+-5", 0.0, 0, 0, false),
    ];
    for c in cases {
        let x = s(c.text);
        assert_eq!(x.doubleValue(), c.double, "doubleValue {:?}", c.text);
        assert_eq!(x.floatValue(), c.double as f32, "floatValue {:?}", c.text);
        assert_eq!(x.intValue(), c.int, "intValue {:?}", c.text);
        assert_eq!(x.integerValue(), c.integer, "integerValue {:?}", c.text);
        assert_eq!(x.longLongValue(), c.integer as i64, "longLongValue {:?}", c.text);
        assert_eq!(x.boolValue(), c.bool, "boolValue {:?}", c.text);
    }
    assert_eq!(s("1e400").doubleValue(), f64::INFINITY);
    assert_eq!(s("-1e400").doubleValue(), f64::NEG_INFINITY);
}
