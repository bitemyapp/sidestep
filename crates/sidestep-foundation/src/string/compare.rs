//! Comparing strings: `compare:` and its options, and the localized forms.
//!
//! Non-literal comparison folds both strings (see `fold`) and compares code
//! points, so canonically equivalent strings are equal and, with no options,
//! the order is that of the decomposed text: `à` sorts before `b`. Literal
//! comparison is by UTF-16 unit, as Foundation's is. `NSNumericSearch`
//! compares runs of ASCII digits by value; `NSForcedOrderingSearch` breaks
//! ties by the digit runs' lengths, then literally.
//!
//! The localized forms use ICU4X's collator for the locale named by
//! `LC_ALL`, `LC_COLLATE` or `LANG` (the root order when none is set):
//! `localizedCaseInsensitiveCompare:` at secondary strength, and
//! `localizedStandardCompare:` at tertiary strength with numeric ordering
//! and tie-breaks (see `standard`).

use std::cmp::Ordering;
use std::sync::OnceLock;

use icu_casemap::CaseMapperBorrowed;
use icu_collator::options::{CollatorOptions, Strength};
use icu_collator::preferences::CollationNumericOrdering;
use icu_collator::{CollatorBorrowed, CollatorPreferences};
use icu_locale_core::Locale;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class};
use objc2_foundation::{NSComparisonResult, NSRange, NSString, NSStringCompareOptions};

use super::fold::{
    self, CASE_INSENSITIVE, DIACRITIC_INSENSITIVE, FORCED_ORDERING, LITERAL, NUMERIC, WIDTH_INSENSITIVE,
};
use super::index::Text;
use super::view::view;
use super::wtf8;

fn result(o: Ordering) -> NSComparisonResult {
    match o {
        Ordering::Less => NSComparisonResult::Ascending,
        Ordering::Equal => NSComparisonResult::Same,
        Ordering::Greater => NSComparisonResult::Descending,
    }
}

/// Text folded lazily, one ASCII character or one stretch of other
/// characters at a time, for comparisons that stop at the first
/// difference.
struct Folding<'a> {
    bytes: &'a [u8],
    at: usize,
    lower: bool,
    memo: fold::Memo,
    buf: Vec<u32>,
    next: usize,
}

impl<'a> Folding<'a> {
    fn new(bytes: &'a [u8], options: usize) -> Self {
        let lower = options & CASE_INSENSITIVE != 0;
        Folding { bytes, at: 0, lower, memo: fold::Memo::new(options, bytes.len()), buf: Vec::new(), next: 0 }
    }
}

impl Iterator for Folding<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        if let Some(&c) = self.buf.get(self.next) {
            self.next += 1;
            return Some(c);
        }
        let &b = self.bytes.get(self.at)?;
        if b.is_ascii() {
            self.at += 1;
            return Some(u32::from(if self.lower { b.to_ascii_lowercase() } else { b }));
        }
        // Folding is context-free across ASCII characters, so a stretch of
        // others folds on its own.
        let end = self.bytes[self.at..].iter().position(u8::is_ascii).map_or(self.bytes.len(), |k| self.at + k);
        self.buf.clear();
        self.next = 0;
        self.memo.fold(&self.bytes[self.at..end], &mut self.buf);
        self.at = end;
        self.next()
    }
}

/// Where two texts start to differ, backed up to where folding may start
/// over: an ASCII character in both (and with `numeric`, the start of its
/// run of digits). Before it they fold alike.
fn common_start(a: &[u8], b: &[u8], numeric: bool) -> usize {
    let mut p = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let ascii_at = |s: &[u8], p: usize| s.get(p).is_some_and(u8::is_ascii);
    while p > 0 && !(ascii_at(a, p) && ascii_at(b, p)) {
        p -= 1;
    }
    while numeric && p > 0 && a[p - 1].is_ascii_digit() {
        p -= 1;
    }
    p
}

/// Skip the zeros `it` starts with, and count them.
fn zeros(it: &mut std::iter::Peekable<impl Iterator<Item = u32>>) -> usize {
    let mut n = 0;
    while it.next_if_eq(&0x30).is_some() {
        n += 1;
    }
    n
}

/// Compare two sequences of code points, digit runs by value when
/// `numeric`. Also reports the first difference in digit-run lengths, which
/// `NSForcedOrderingSearch` uses to break ties. Stops at the first
/// difference.
fn compare_points(a: impl Iterator<Item = u32>, b: impl Iterator<Item = u32>, numeric: bool) -> (Ordering, Ordering) {
    if !numeric {
        return (a.cmp(b), Ordering::Equal);
    }
    let digit = |c: &u32| (0x30..=0x39).contains(c);
    let (mut a, mut b) = (a.peekable(), b.peekable());
    let mut tie = Ordering::Equal;
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return (Ordering::Equal, tie),
            (None, Some(_)) => return (Ordering::Less, tie),
            (Some(_), None) => return (Ordering::Greater, tie),
            (Some(x), Some(y)) if digit(&x) && digit(&y) => {
                // Leading zeros, then the significant digits side by side.
                let (za, zb) = (zeros(&mut a), zeros(&mut b));
                let (mut la, mut lb) = (0, 0);
                let mut first = Ordering::Equal;
                loop {
                    match (a.next_if(digit), b.next_if(digit)) {
                        (Some(x), Some(y)) => {
                            first = first.then(x.cmp(&y));
                            la += 1;
                            lb += 1;
                        }
                        (Some(_), None) => la += 1,
                        (None, Some(_)) => lb += 1,
                        (None, None) => break,
                    }
                }
                let by_value = la.cmp(&lb).then(first);
                if by_value != Ordering::Equal {
                    return (by_value, tie);
                }
                if tie == Ordering::Equal {
                    tie = (za + la).cmp(&(zb + lb));
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return (x.cmp(&y), tie);
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Compare by UTF-16 unit, from the first difference.
pub(crate) fn compare_literal(a: &[u8], b: &[u8]) -> Ordering {
    let mut p = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    // Back to the start of the character holding the difference.
    while p > 0 && a.get(p).is_some_and(|&c| c & 0xC0 == 0x80) {
        p -= 1;
    }
    let (a, b) = (&a[p..], &b[p..]);
    match (a.first(), b.first()) {
        (Some(x), Some(y)) if x.is_ascii() && y.is_ascii() => x.cmp(y),
        _ => wtf8::units(a).cmp(wtf8::units(b)),
    }
}

/// What a literal comparison still folds or compares by value, as on
/// macOS: case, diacritics, width and digit runs, when asked.
const LITERAL_FOLDS: usize = CASE_INSENSITIVE | NUMERIC | DIACRITIC_INSENSITIVE | WIDTH_INSENSITIVE;

/// `compare:options:` on two texts.
pub(crate) fn compare(a: &Text, b: &Text, options: usize) -> Ordering {
    let numeric = options & NUMERIC != 0;
    let (order, tie) = if options & LITERAL != 0 && options & LITERAL_FOLDS == 0 {
        (compare_literal(a.bytes, b.bytes), Ordering::Equal)
    } else {
        let options = if options & LITERAL != 0 { options & LITERAL_FOLDS } else { options };
        let p = common_start(a.bytes, b.bytes, numeric);
        compare_points(Folding::new(&a.bytes[p..], options), Folding::new(&b.bytes[p..], options), numeric)
    };
    if order != Ordering::Equal || options & FORCED_ORDERING == 0 {
        return order;
    }
    // Forced ordering: equal strings that differ break the tie.
    tie.then_with(|| {
        let plain = compare(a, b, options & NUMERIC);
        if plain != Ordering::Equal { plain } else { compare_literal(a.bytes, b.bytes) }
    })
}

/// The locale collation follows, from the environment.
fn environment_locale() -> Option<Locale> {
    let name = ["LC_ALL", "LC_COLLATE", "LANG"].iter().filter_map(|v| std::env::var(v).ok()).find(|v| !v.is_empty())?;
    // POSIX names look like en_US.UTF-8@euro.
    let base = name.split(['.', '@']).next()?.replace('_', "-");
    if base == "C" || base == "POSIX" {
        return None;
    }
    Locale::try_from_str(&base).ok()
}

/// A collator, made once per kind of comparison.
fn collator(strength: Strength, numeric: bool) -> &'static CollatorBorrowed<'static> {
    static COLLATORS: [OnceLock<CollatorBorrowed<'static>>; 6] = [const { OnceLock::new() }; 6];
    let k = strength as usize * 2 + usize::from(numeric);
    COLLATORS[k.min(5)].get_or_init(|| {
        let mut prefs: CollatorPreferences = environment_locale().map(|l| (&l).into()).unwrap_or_default();
        prefs.numeric_ordering =
            Some(if numeric { CollationNumericOrdering::True } else { CollationNumericOrdering::False });
        let mut options = CollatorOptions::default();
        options.strength = Some(strength);
        CollatorBorrowed::try_new(prefs, options)
            .or_else(|_| CollatorBorrowed::try_new(CollatorPreferences::default(), options))
            .expect("sidestep: the root collation is compiled in")
    })
}

/// Compare with the collator for `options`.
pub(crate) fn collate(a: &Text, b: &Text, options: usize) -> Ordering {
    let strength = if options & fold::DIACRITIC_INSENSITIVE != 0 {
        Strength::Primary
    } else if options & CASE_INSENSITIVE != 0 {
        Strength::Secondary
    } else {
        Strength::Tertiary
    };
    let c = collator(strength, options & NUMERIC != 0);
    if options & CASE_INSENSITIVE != 0 && !(a.is_ascii() && b.is_ascii()) {
        // Case is folded before collating, fully, as on macOS: `ß` is the
        // same as `ss` and `ﬁ` as `fi`, which the collator alone tells
        // apart at this strength.
        let fold = |t: &Text| CaseMapperBorrowed::new().fold_string(&wtf8::to_str_lossy(t.bytes, t.flags)).into_owned();
        return c.compare(&fold(a), &fold(b));
    }
    match (a.as_str(), b.as_str()) {
        (Some(x), Some(y)) => c.compare(x, y),
        _ => c.compare(&wtf8::to_str_lossy(a.bytes, a.flags), &wtf8::to_str_lossy(b.bytes, b.flags)),
    }
}

/// `localizedStandardCompare:`, the order Finder sorts names in: the
/// collator's at tertiary strength (so case counts) with digit runs by
/// value. Strings that still tie are ordered by their digit runs' lengths
/// (fewer leading zeros first), then by UTF-16 unit, so different strings
/// never compare the same and a sort doesn't depend on where it started.
pub(crate) fn standard(a: &Text, b: &Text) -> Ordering {
    collate(a, b, NUMERIC).then_with(|| digit_runs(a.bytes, b.bytes)).then_with(|| compare_literal(a.bytes, b.bytes))
}

/// The first difference in the lengths of two texts' runs of decimal
/// digits, taken in order: the shorter run first.
fn digit_runs(a: &[u8], b: &[u8]) -> Ordering {
    let runs = |bytes: &[u8]| {
        let mut runs = Vec::new();
        let mut run = 0;
        for (_, c) in wtf8::code_points(bytes) {
            if is_digit(c) {
                run += 1;
            } else if run > 0 {
                runs.push(run);
                run = 0;
            }
        }
        if run > 0 {
            runs.push(run);
        }
        runs
    };
    let (ra, rb) = (runs(a), runs(b));
    ra.iter().zip(&rb).map(|(x, y)| x.cmp(y)).find(|o| o.is_ne()).unwrap_or(Ordering::Equal)
}

fn is_digit(c: u32) -> bool {
    (0x30..=0x39).contains(&c)
        || (c >= 0x80 && fold::general_category(c) == icu_properties::props::GeneralCategory::DecimalNumber)
}

fn this(obj: &Helper) -> &AnyObject {
    obj
}

/// The receiver's text in `range`, compared with all of `other`.
fn compare_range(obj: &AnyObject, other: &NSString, options: usize, range: NSRange, locale: bool) -> Ordering {
    let (va, vb) = (view(obj), view(other));
    let (ta, tb) = (va.text(), vb.text());
    super::check_range("compare:options:range:", range, ta.utf16_len);
    let (start, end) = ta.range(range.location, range.length);
    let bytes = wtf8::slice(ta.bytes, start, end);
    let flags = wtf8::flags_of(&bytes, true);
    let sub = Text::plain(&bytes, range.length, flags);
    if locale { collate(&sub, &tb, options) } else { compare(&sub, &tb, options) }
}

define_class!(
    // NSString's comparison methods, copied onto NSString when it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringCompare"]
    pub(crate) struct Helper;

    impl Helper {
        #[unsafe(method(compare:))]
        fn compare(&self, other: &NSString) -> NSComparisonResult {
            result(compare(&view(this(self)).text(), &view(other).text(), 0))
        }

        #[unsafe(method(compare:options:))]
        fn compare_options(&self, other: &NSString, options: NSStringCompareOptions) -> NSComparisonResult {
            result(compare(&view(this(self)).text(), &view(other).text(), options.0))
        }

        #[unsafe(method(compare:options:range:))]
        fn compare_options_range(&self, other: &NSString, options: NSStringCompareOptions, range: NSRange) -> NSComparisonResult {
            result(compare_range(this(self), other, options.0, range, false))
        }

        #[unsafe(method(compare:options:range:locale:))]
        fn compare_options_range_locale(
            &self,
            other: &NSString,
            options: NSStringCompareOptions,
            range: NSRange,
            locale: Option<&AnyObject>,
        ) -> NSComparisonResult {
            result(compare_range(this(self), other, options.0, range, locale.is_some()))
        }

        #[unsafe(method(caseInsensitiveCompare:))]
        fn case_insensitive_compare(&self, other: &NSString) -> NSComparisonResult {
            result(compare(&view(this(self)).text(), &view(other).text(), CASE_INSENSITIVE))
        }

        #[unsafe(method(localizedCompare:))]
        fn localized_compare(&self, other: &NSString) -> NSComparisonResult {
            result(collate(&view(this(self)).text(), &view(other).text(), 0))
        }

        #[unsafe(method(localizedCaseInsensitiveCompare:))]
        fn localized_case_insensitive_compare(&self, other: &NSString) -> NSComparisonResult {
            result(collate(&view(this(self)).text(), &view(other).text(), CASE_INSENSITIVE))
        }

        #[unsafe(method(localizedStandardCompare:))]
        fn localized_standard_compare(&self, other: &NSString) -> NSComparisonResult {
            result(standard(&view(this(self)).text(), &view(other).text()))
        }

        #[unsafe(method_id(commonPrefixWithString:options:))]
        fn common_prefix(&self, other: &NSString, options: NSStringCompareOptions) -> Retained<NSString> {
            super::search::common_prefix(this(self), other, options.0)
        }
    }
);

/// Copy the comparison methods onto NSString.
pub(crate) fn install(target: &AnyClass) {
    super::install::copy_methods(Helper::class(), target, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The comparison as first written: fold both texts, then compare.
    fn reference(a: &[u8], b: &[u8], options: usize) -> Ordering {
        fn points(a: &[u32], b: &[u32], numeric: bool) -> (Ordering, Ordering) {
            let digit = |c: u32| (0x30..=0x39).contains(&c);
            let (mut i, mut j) = (0, 0);
            let mut tie = Ordering::Equal;
            while i < a.len() && j < b.len() {
                if numeric && digit(a[i]) && digit(b[j]) {
                    let (si, sj) = (i, j);
                    while i < a.len() && digit(a[i]) {
                        i += 1;
                    }
                    while j < b.len() && digit(b[j]) {
                        j += 1;
                    }
                    let (ra, rb) = (&a[si..i], &b[sj..j]);
                    let strip = |r: &[u32]| r.len() - r.iter().take_while(|&&c| c == 0x30).count();
                    let (za, zb) = (&ra[ra.len() - strip(ra)..], &rb[rb.len() - strip(rb)..]);
                    let by_value = za.len().cmp(&zb.len()).then_with(|| za.cmp(zb));
                    if by_value != Ordering::Equal {
                        return (by_value, tie);
                    }
                    if tie == Ordering::Equal {
                        tie = ra.len().cmp(&rb.len());
                    }
                    continue;
                }
                match a[i].cmp(&b[j]) {
                    Ordering::Equal => {
                        i += 1;
                        j += 1;
                    }
                    other => return (other, tie),
                }
            }
            ((a.len() - i).cmp(&(b.len() - j)), tie)
        }
        if options & LITERAL != 0 && options & LITERAL_FOLDS == 0 {
            return wtf8::units(a).cmp(wtf8::units(b));
        }
        let fold_options = if options & LITERAL != 0 {
            options & (CASE_INSENSITIVE | DIACRITIC_INSENSITIVE | WIDTH_INSENSITIVE)
        } else {
            options
        };
        let (order, tie) =
            points(&fold::fold_text(a, fold_options), &fold::fold_text(b, fold_options), options & NUMERIC != 0);
        if order != Ordering::Equal || options & FORCED_ORDERING == 0 {
            return order;
        }
        tie.then_with(|| {
            let plain = reference(a, b, options & NUMERIC);
            if plain != Ordering::Equal { plain } else { wtf8::units(a).cmp(wtf8::units(b)) }
        })
    }

    #[test]
    fn comparisons_match_folding_everything_first() {
        #[rustfmt::skip]
        const PIECES: [&str; 24] = [
            "a", "A", "b", "0", "1", "9", "00", "01", " ", "é", "e\u{301}", "\u{301}", "\u{316}", "ß", "SS",
            "ﬁ", "\u{FF21}", "\u{FF11}", "漢", "\u{1F389}", "\u{212A}", "k", "\u{10000}", "\u{FFFD}",
        ];
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let options = [0, CASE_INSENSITIVE, NUMERIC, LITERAL, LITERAL | CASE_INSENSITIVE, FORCED_ORDERING | NUMERIC];
        for _ in 0..30_000 {
            let mut a = String::new();
            for _ in 0..next(6) {
                a.push_str(PIECES[next(PIECES.len() as u64) as usize]);
            }
            // Often a shared start, so the comparison skips it.
            let mut b = if next(2) == 0 { a.clone() } else { String::new() };
            for _ in 0..next(4) {
                b.push_str(PIECES[next(PIECES.len() as u64) as usize]);
            }
            let flags = |s: &str| wtf8::flags_of(s.as_bytes(), false);
            let ta = Text::plain(a.as_bytes(), a.encode_utf16().count(), flags(&a));
            let tb = Text::plain(b.as_bytes(), b.encode_utf16().count(), flags(&b));
            for o in options {
                assert_eq!(compare(&ta, &tb, o), reference(a.as_bytes(), b.as_bytes(), o), "{a:?} {b:?} {o}");
            }
        }
    }
}
