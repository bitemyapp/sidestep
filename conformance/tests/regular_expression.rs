//! NSRegularExpression and NSTextCheckingResult. Expected values are what
//! macOS returns.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol};
use objc2_foundation::{
    NSMatchingFlags, NSMatchingOptions, NSMutableString, NSRange, NSRegularExpression, NSRegularExpressionOptions,
    NSString, NSTextCheckingResult,
};

use sidestep as _;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

fn re(pattern: &str, options: usize) -> Retained<NSRegularExpression> {
    NSRegularExpression::regularExpressionWithPattern_options_error(&s(pattern), NSRegularExpressionOptions(options))
        .expect("a valid pattern")
}

fn found(r: NSRange) -> Option<(usize, usize)> {
    (r.location != isize::MAX as usize).then_some((r.location, r.length))
}

fn first(r: &NSRegularExpression, text: &str, options: usize) -> Option<(usize, usize)> {
    let t = s(text);
    found(r.rangeOfFirstMatchInString_options_range(&t, NSMatchingOptions(options), NSRange::new(0, t.length())))
}

fn groups(m: &NSTextCheckingResult) -> Vec<Option<(usize, usize)>> {
    (0..m.numberOfRanges()).map(|k| found(m.rangeAtIndex(k))).collect()
}

#[test]
fn escaping() {
    assert_eq!(
        NSRegularExpression::escapedPatternForString(&s(r"a.b*c?d+e[f]g(h)i{j}k^l$m|n\o/p-q#r s")).to_string(),
        r"a\.b\*c\?d\+e\[f]g\(h\)i\{j\}k\^l\$m\|n\\o\/p-q#r s"
    );
    assert_eq!(NSRegularExpression::escapedTemplateForString(&s(r"$1 \x a$")).to_string(), r"\$1 \\x a\$");
}

#[test]
fn matching() {
    let r = re("(a)(b)?(c)", 0);
    assert_eq!((r.numberOfCaptureGroups(), r.pattern().to_string(), r.options().0), (3, "(a)(b)?(c)".into(), 0));
    let text = s("xacyabcz");
    let all = NSRange::new(0, text.length());
    assert_eq!(r.numberOfMatchesInString_options_range(&text, NSMatchingOptions(0), all), 2);
    let m = r.firstMatchInString_options_range(&text, NSMatchingOptions(0), all).unwrap();
    assert_eq!(groups(&m), [Some((1, 2)), Some((1, 1)), None, Some((2, 1))]);
    assert_eq!(m.resultType().0, 1 << 10);
    assert_eq!(
        found(r.rangeOfFirstMatchInString_options_range(&text, NSMatchingOptions(0), NSRange::new(3, 5))),
        Some((4, 3))
    );
    // Anchored matches must start where the range does.
    assert_eq!(found(r.rangeOfFirstMatchInString_options_range(&text, NSMatchingOptions(4), all)), None);
    assert_eq!(
        found(r.rangeOfFirstMatchInString_options_range(&text, NSMatchingOptions(4), NSRange::new(1, 7))),
        Some((1, 2))
    );
    // The range is the input for anchors, unless asked otherwise.
    let caret = re("^b", 0);
    assert_eq!(
        found(caret.rangeOfFirstMatchInString_options_range(&s("ab"), NSMatchingOptions(0), NSRange::new(1, 1))),
        Some((1, 1))
    );
    assert_eq!(
        found(caret.rangeOfFirstMatchInString_options_range(&s("ab"), NSMatchingOptions(16), NSRange::new(1, 1))),
        None
    );
    assert_eq!(first(&re("abc", 1), "xABC", 0), Some((1, 3)));
    let meta = re("a.c", 4);
    assert_eq!((first(&meta, "abc a.c", 0), meta.numberOfCaptureGroups()), (Some((4, 3)), 0));
    assert_eq!(first(&re("^b$", 16), "a\nb\nc", 0), Some((2, 1)));
    assert_eq!(first(&re("a.b", 8), "a\nb", 0), Some((0, 3)));
    assert_eq!(first(&re("a.b", 0), "a\nb", 0), None);
    assert_eq!(
        re("x*", 0).numberOfMatchesInString_options_range(&s("abc"), NSMatchingOptions(0), NSRange::new(0, 3)),
        4
    );
    let m =
        re("b(.)c", 0).firstMatchInString_options_range(&s("ab🎉c"), NSMatchingOptions(0), NSRange::new(0, 5)).unwrap();
    assert_eq!(groups(&m), [Some((1, 4)), Some((2, 2))]);
    let named = re(r"(?<year>\d{4})-(?<m>\d\d)", 0);
    let m =
        named.firstMatchInString_options_range(&s("on 2024-05"), NSMatchingOptions(0), NSRange::new(0, 10)).unwrap();
    assert_eq!((found(m.rangeWithName(&s("year"))), found(m.rangeWithName(&s("m")))), (Some((3, 4)), Some((8, 2))));
    assert_eq!(named.numberOfCaptureGroups(), 2);
    let moved = m.resultByAdjustingRangesWithOffset(2);
    assert_eq!((found(moved.range()), found(moved.rangeAtIndex(1))), (Some((5, 7)), Some((5, 4))));
}

#[test]
fn results_from_ranges() {
    let r = re("(a)(b)", 0);
    let mut ranges = [NSRange::new(2, 2), NSRange::new(2, 1), NSRange::new(isize::MAX as usize, 0)];
    let m = unsafe {
        NSTextCheckingResult::regularExpressionCheckingResultWithRanges_count_regularExpression(
            ranges.as_mut_ptr(),
            3,
            &r,
        )
    };
    assert_eq!(groups(&m), [Some((2, 2)), Some((2, 1)), None]);
    assert_eq!((found(m.range()), m.numberOfRanges(), m.resultType().0), (Some((2, 2)), 3, 1 << 10));
    assert!(m.regularExpression().is_some_and(|x| std::ptr::eq(&*x, &*r)));
    let t = s("xxab");
    // A group that took no part expands to nothing.
    assert_eq!(r.replacementStringForResult_inString_offset_template(&m, &t, 0, &s("$2$1")).to_string(), "a");
}

#[test]
fn replacing() {
    let r = re(r"(\w+)@(\w+)", 0);
    let t = s("me@home you@work");
    let all = NSRange::new(0, t.length());
    assert_eq!(
        r.stringByReplacingMatchesInString_options_range_withTemplate(&t, NSMatchingOptions(0), all, &s("$2:$1"))
            .to_string(),
        "home:me work:you"
    );
    let m = NSMutableString::from_str("me@home you@work");
    assert_eq!(r.replaceMatchesInString_options_range_withTemplate(&m, NSMatchingOptions(0), all, &s("<$0>")), 2);
    assert_eq!(m.to_string(), "<me@home> <you@work>");
    let first = r.firstMatchInString_options_range(&t, NSMatchingOptions(0), all).unwrap();
    assert_eq!(r.replacementStringForResult_inString_offset_template(&first, &t, 0, &s("[$1]")).to_string(), "[me]");
    assert_eq!(
        r.replacementStringForResult_inString_offset_template(&first, &s("XYZme@home"), 3, &s("[$1]")).to_string(),
        "[me]"
    );
}

#[test]
fn enumerating() {
    let r = re(r"(\w+)@(\w+)", 0);
    let t = s("me@home you@work");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    let block = RcBlock::new(move |res: *mut NSTextCheckingResult, flags: NSMatchingFlags, _stop: NonNull<Bool>| {
        let range = (!res.is_null()).then(|| found(unsafe { &*res }.range())).flatten();
        log.borrow_mut().push((range, flags.0));
    });
    // With NSMatchingReportCompletion, a last call with no result.
    r.enumerateMatchesInString_options_range_usingBlock(&t, NSMatchingOptions(2), NSRange::new(0, t.length()), &block);
    assert_eq!(*seen.borrow(), [(Some((0, 7)), 0), (Some((8, 8)), 4), (None, 6)]);
}

/// Every match's range and the flags reported with it, through
/// enumeration (which needs no NSArray), with `options`.
fn walk(
    pattern: &str,
    re_options: usize,
    text: &str,
    options: usize,
    range: Option<(usize, usize)>,
) -> Vec<(Option<(usize, usize)>, usize)> {
    let r = re(pattern, re_options);
    let t = s(text);
    let range = range.map_or(NSRange::new(0, t.length()), |(l, n)| NSRange::new(l, n));
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    let block = RcBlock::new(move |res: *mut NSTextCheckingResult, flags: NSMatchingFlags, _stop: NonNull<Bool>| {
        let range = (!res.is_null()).then(|| found(unsafe { &*res }.range())).flatten();
        log.borrow_mut().push((range, flags.0));
    });
    r.enumerateMatchesInString_options_range_usingBlock(&t, NSMatchingOptions(options), range, &block);
    drop(block);
    seen.take()
}

/// The ranges of every match.
fn all(pattern: &str, re_options: usize, text: &str) -> Vec<(usize, usize)> {
    walk(pattern, re_options, text, 0, None).into_iter().filter_map(|(r, _)| r).collect()
}

const LINES: usize = 1 << 4;
const UNIX: usize = 1 << 5;

#[test]
fn lines_and_inline_flags() {
    // Inline flags, alone or for a group.
    assert_eq!(all("(?m)^b$", 0, "a\nb\nc"), [(2, 1)]);
    assert_eq!(all("(?m)^\\s*$", 0, "a\n\nb"), [(2, 0)]);
    assert_eq!(all("(?m)$", 0, "a\nb\n"), [(1, 0), (3, 0), (4, 0)]);
    assert_eq!(all("(?is)A.B", 0, "a\nb"), [(0, 3)]);
    assert_eq!(all("(?m:^b)|c$", 0, "a\nb\nc"), [(2, 1), (4, 1)]);
    assert_eq!(all("(?m)(?-m:^a)|^b", 0, "a\nb"), [(0, 1), (2, 1)]);
    assert_eq!(all("(?-s:a.c)", 8, "a\nc"), []);
    assert_eq!(all("(?i)b(?-i)c", 0, "BC Bc bC"), [(3, 2)]);
    assert_eq!(all("(?x) a b # comment [ \n c", 0, "abc"), [(0, 3)]);
    // Every ICU line terminator, CR LF counting as one.
    assert_eq!(all("^\\w+$", LINES, "one\r\ntwo\r\n"), [(0, 3), (5, 3)]);
    assert_eq!(all("^#.*$", LINES, "#a\r\n#b"), [(0, 2), (4, 2)]);
    assert_eq!(all("^b$", LINES, "a\rb\rc"), [(2, 1)]);
    assert_eq!(all("a.b", 0, "a\u{b}b"), []);
    assert_eq!(all("b$", 0, "ab\u{b}"), [(1, 1)]);
    assert_eq!(all("^", LINES, "a\n"), [(0, 0)]);
    assert_eq!(all("^", LINES, "a\r\nb"), [(0, 0), (3, 0)]);
    assert_eq!(all("$", LINES, "a\r\nb"), [(1, 0), (4, 0)]);
    assert_eq!(all("$", 0, "a\r\n"), [(1, 0), (3, 0)]);
    assert_eq!(all("$", LINES, "\r\u{b}"), [(0, 0), (1, 0), (2, 0)]);
    assert_eq!(all("$", 0, "a\r\u{b}"), [(2, 0), (3, 0)]);
    assert_eq!(all("^$", LINES, "a\n\nb\n"), [(2, 0)]);
    assert_eq!(all("\\Z", 0, "a\n\n"), [(2, 0), (3, 0)]);
    // Only LF, with UseUnixLineSeparators.
    assert_eq!(all("$", UNIX | LINES, "a\rb\nc"), [(3, 0), (5, 0)]);
    assert_eq!(all("^", UNIX | LINES, "a\rb\nc"), [(0, 0), (4, 0)]);
    assert_eq!(all(".", UNIX, "\r"), [(0, 1)]);
    assert_eq!(all("$", UNIX, "a\r"), [(2, 0)]);
}

#[test]
fn icu_syntax() {
    assert_eq!(all("[[:alpha:]]+", 0, "héllo"), [(0, 5)]);
    assert_eq!(all("[[:space:]]+", 0, "a\u{a0}\u{3000}b"), [(1, 2)]);
    assert_eq!(all("[[:punct:]]", 0, "a«b"), [(1, 1)]);
    assert_eq!(all("[[:upper:]]", 0, "éÉ"), [(1, 1)]);
    assert_eq!(all("[[:digit:]]+", 0, "x١٢y"), [(1, 2)]);
    assert_eq!(all("[[:xdigit:]]+", 0, "xAf9ｆg"), [(1, 4)]);
    assert_eq!(all("[[:alnum:]]+", 0, "_é1_"), [(1, 2)]);
    assert_eq!(all("[[:blank:]]+", 0, "a \t\u{a0}\nb"), [(1, 3)]);
    assert_eq!(all("[[:cntrl:]]", 0, "a\u{1}b"), [(1, 1)]);
    assert_eq!(all("[[:word:]]+", 0, "a_b-c"), [(0, 3), (4, 1)]);
    assert_eq!(all("[^[:alpha:]]+", 0, "ab12cd"), [(2, 2)]);
    assert_eq!(all("[[:^alpha:]]+", 0, "ab12cd"), [(2, 2)]);
    assert_eq!(all("\\h+", 0, "a \t\u{a0}\nb"), [(1, 3)]);
    assert_eq!(all("\\H+", 0, " ab "), [(1, 2)]);
    assert_eq!(all("\\R", 0, "a\r\nb\rc\u{b}"), [(1, 2), (4, 1), (6, 1)]);
    assert_eq!(all("\\v+", 0, "a\n\u{b}\u{c}b"), [(1, 3)]);
    assert_eq!(all("\\V+", 0, "a\nbc"), [(0, 1), (2, 2)]);
    assert_eq!(all("\\X", 0, "e\u{301}🇫🇷👨\u{200d}👩x\r\n"), [(0, 2), (2, 4), (6, 5), (11, 1), (12, 2)]);
    assert_eq!(all("\\s", 0, "a\u{b}b\u{85}c\u{a0}d"), [(1, 1), (3, 1), (5, 1)]);
    assert_eq!(all("\\0141", 0, "bab"), [(1, 1)]);
    assert_eq!(all("\\ca", 0, "\u{1}"), [(0, 1)]);
    assert_eq!(all("\\e", 0, "\u{1b}"), [(0, 1)]);
    assert_eq!(all("(?#comment)a", 0, "ba"), [(1, 1)]);
    assert_eq!(all("[a-c&&b-d]+", 0, "abcd"), [(1, 2)]);
    assert_eq!(all("[\\w--\\d]+", 0, "ab12"), [(0, 2)]);
    assert_eq!(all("(?w)\\bcan't\\b", 0, "I can't go"), [(2, 5)]);
    assert_eq!(all("\\bcan't\\b", 64, "I can't go"), [(2, 5)]);
}

#[test]
fn ranges_are_regions() {
    let first = |p: &str, t: &str, options: usize, loc: usize, len: usize| {
        found(re(p, 0).rangeOfFirstMatchInString_options_range(
            &s(t),
            NSMatchingOptions(options),
            NSRange::new(loc, len),
        ))
    };
    const TRANSPARENT: usize = 1 << 3;
    const NO_ANCHORING: usize = 1 << 4;
    // Look-behind sees what comes before the range.
    assert_eq!(first("(?<=a)b", "ab", 0, 1, 1), Some((1, 1)));
    assert_eq!(first("(?<!a)b", "ab", 0, 1, 1), None);
    // Word boundaries and look-ahead see the range's ends as the text's,
    // unless the bounds are transparent.
    assert_eq!(first("\\bb", "ab", 0, 1, 1), Some((1, 1)));
    assert_eq!(first("\\bb", "ab", TRANSPARENT, 1, 1), None);
    assert_eq!(first("\\Bb", "ab", 0, 1, 1), None);
    assert_eq!(first("b(?=c)", "abc", 0, 1, 1), None);
    assert_eq!(first("b(?=c)", "abc", TRANSPARENT, 1, 1), Some((1, 1)));
    assert_eq!(first("b\\b", "abc", 0, 1, 1), Some((1, 1)));
    assert_eq!(first("b\\b", "abc", TRANSPARENT, 1, 1), None);
    // Matches stop at the range's end; anchors match there only with
    // anchoring bounds.
    assert_eq!(first("a+", "aaa", 0, 0, 2), Some((0, 2)));
    assert_eq!(first("a+", "aaa", NO_ANCHORING, 0, 2), Some((0, 2)));
    assert_eq!(first("a.", "ab\ncd", 0, 0, 1), None);
    assert_eq!(first("b$", "abc", 0, 1, 1), Some((1, 1)));
    assert_eq!(first("b$", "abc", NO_ANCHORING, 1, 1), None);
    assert_eq!(first("b$", "ab", NO_ANCHORING, 1, 1), Some((1, 1)));
    assert_eq!(first("^a", "ab", NO_ANCHORING, 0, 1), Some((0, 1)));
    assert_eq!(first("x|$", "ab", 0, 0, 1), Some((1, 0)));
    assert_eq!(first("x|$", "ab", NO_ANCHORING, 0, 1), None);
    // The string search option works the same way.
    let r = s("ab").rangeOfString_options_range(
        &s("(?<=a)b"),
        objc2_foundation::NSStringCompareOptions(1024),
        NSRange::new(1, 1),
    );
    assert_eq!(found(r), Some((1, 1)));
}

#[test]
fn flags_progress_and_completion() {
    const PROGRESS: usize = 1;
    const COMPLETION: usize = 2;
    const ANCHORED: usize = 4;
    let w = |p: &str, t: &str, o: usize| walk(p, 0, t, o, None);
    // Hit end when the match could have read on; required end when it
    // needed the end.
    assert_eq!(w("b", "ab", 0), [(Some((1, 1)), 0)]);
    assert_eq!(w("abc", "xabc", 0), [(Some((1, 3)), 0)]);
    assert_eq!(w("b+", "ab", 0), [(Some((1, 1)), 4)]);
    assert_eq!(w("b$", "ab", 0), [(Some((1, 1)), 12)]);
    assert_eq!(w("a\\z", "a", 0), [(Some((0, 1)), 12)]);
    assert_eq!(w("a\\b", "a", 0), [(Some((0, 1)), 4)]);
    assert_eq!(w("a|ab", "a", 0), [(Some((0, 1)), 0)]);
    assert_eq!(w("a+b", "aab", 0), [(Some((0, 3)), 0)]);
    assert_eq!(w("ba?", "b", 0), [(Some((0, 1)), 4)]);
    assert_eq!(w("a+?", "aa", 0), [(Some((0, 1)), 0), (Some((1, 1)), 0)]);
    assert_eq!(w("\\w+", "ab cd", COMPLETION), [(Some((0, 2)), 0), (Some((3, 2)), 4), (None, 6)]);
    // The completion call has hit the end unless an anchored search
    // stopped short of it.
    assert_eq!(w("^a", "ba", COMPLETION), [(None, 2)]);
    assert_eq!(w("a", "ba", COMPLETION), [(Some((1, 1)), 0), (None, 6)]);
    assert_eq!(w("^ab", "ab", COMPLETION), [(Some((0, 2)), 0), (None, 6)]);
    assert_eq!(w("x", "", COMPLETION), [(None, 6)]);
    assert_eq!(w("a*", "b", COMPLETION), [(Some((0, 0)), 0), (Some((1, 0)), 4), (None, 6)]);
    assert_eq!(w("b", "ab", ANCHORED | COMPLETION), [(None, 2)]);
    assert_eq!(w("a", "aba", ANCHORED | COMPLETION), [(Some((0, 1)), 0), (None, 2)]);
    // Progress: a call for each position the search moves on from.
    assert_eq!(w("a", "aa", PROGRESS), [(Some((0, 1)), 0), (Some((1, 1)), 0)]);
    assert_eq!(w("a", "ba", PROGRESS | COMPLETION), [(None, 1), (Some((1, 1)), 0), (None, 6)]);
    assert_eq!(w("x", &"a".repeat(10), PROGRESS), vec![(None, 1); 9]);
    assert_eq!(w("b", "abc", PROGRESS | COMPLETION), [(None, 1), (Some((1, 1)), 0), (None, 6)]);
}

#[test]
fn equality_and_lone_surrogates() {
    let (a, b, c) = (re("a+", 1), re("a+", 1), re("a+", 0));
    assert!(a.isEqual(Some(&b)) && a.hash() == b.hash());
    assert!(!a.isEqual(Some(&c)));
    // Text holding a lone surrogate keeps it.
    let high = s("🎉").substringToIndex(1);
    assert_eq!(NSRegularExpression::escapedPatternForString(&high).length(), 1);
    assert_eq!(NSRegularExpression::escapedTemplateForString(&high).length(), 1);
    let text = s("a").stringByAppendingString(&high).stringByAppendingString(&s("b"));
    let out = re("b", 0).stringByReplacingMatchesInString_options_range_withTemplate(
        &text,
        NSMatchingOptions(0),
        NSRange::new(0, 3),
        &s("c"),
    );
    assert_eq!((0..out.length()).map(|i| out.characterAtIndex(i)).collect::<Vec<_>>(), [0x61, 0xD83C, 0x63]);
    let out = re("a", 0).stringByReplacingMatchesInString_options_range_withTemplate(
        &s("xa"),
        NSMatchingOptions(0),
        NSRange::new(0, 2),
        &high,
    );
    assert_eq!((0..out.length()).map(|i| out.characterAtIndex(i)).collect::<Vec<_>>(), [0x78, 0xD83C]);
}

#[test]
fn invalid_patterns_are_errors() {
    let e = NSRegularExpression::regularExpressionWithPattern_options_error(&s("("), NSRegularExpressionOptions(0))
        .expect_err("an invalid pattern");
    assert_eq!((e.domain().to_string(), e.code()), ("NSCocoaErrorDomain".into(), 2048));
}

#[test]
fn matches_as_an_array() {
    let r = re("(a)(b)?(c)", 0);
    let text = s("xacyabcz");
    let m = r.matchesInString_options_range(&text, NSMatchingOptions(0), NSRange::new(0, text.length()));
    assert_eq!(m.count(), 2);
    assert_eq!(groups(&m.objectAtIndex(1)), [Some((4, 3)), Some((4, 1)), Some((5, 1)), Some((6, 1))]);
}
