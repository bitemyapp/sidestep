//! Searching: `rangeOfString:` and its relatives, prefixes and suffixes,
//! character sets, composed character sequences, and the replacements
//! built on them.
//!
//! Literal searches, and any search where both sides are ASCII (where the
//! non-literal rules change nothing but case), run on bytes with memchr, as
//! do ASCII needles in other text when only case is folded (see
//! `each_find`). Other searches fold the text as they walk it
//! (`fold::Stream`) and look for the folded needle at the receiver's
//! composed character sequence boundaries, so they stop as soon as they
//! find it. Finding every match is one pass, not a search per match, and so
//! is replacing them. The `NSRegularExpressionSearch` option hands the
//! pattern to `crate::regex`. Ranges are UTF-16 throughout; "not found" is
//! `{NSNotFound, 0}`.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class};
use objc2_foundation::{NSCharacterSet, NSRange, NSString, NSStringCompareOptions, NSUInteger};

use super::fold::{
    self, ANCHORED, BACKWARDS, CASE_INSENSITIVE, DIACRITIC_INSENSITIVE, LITERAL, REGULAR_EXPRESSION, WIDTH_INSENSITIVE,
};
use super::index::Text;
use super::view::{StrView, view};
use super::wtf8::Pos;
use super::{inline, wtf8};
use crate::charset;

/// `{NSNotFound, 0}`.
pub(crate) const NOT_FOUND: NSRange = NSRange { location: isize::MAX as usize, length: 0 };

fn found(r: Option<(usize, usize)>) -> NSRange {
    r.map_or(NOT_FOUND, |(loc, len)| NSRange::new(loc, len))
}

/// The bytes of a UTF-16 range, trimmed to whole characters.
pub(crate) fn window(t: &Text, loc: usize, len: usize) -> (usize, usize) {
    let (start, end) = t.range(loc, len);
    let from = if start.low { start.byte + 4 } else { start.byte };
    (from, end.byte.max(from))
}

/// A byte range of `t` as a UTF-16 (location, length).
pub(crate) fn to_utf16(t: &Text, from: usize, to: usize) -> (usize, usize) {
    let loc = t.utf16_at(from);
    (loc, wtf8::utf16_len(&t.bytes[from..to]))
}

/// What a visitor makes of a hit.
#[derive(Clone, Copy)]
enum Hit {
    /// Not a match: later hits may overlap it.
    Skip,
    /// A match: later hits don't overlap it.
    Take,
    Stop,
}

/// Visit each place `n` occurs in `hay`, ignoring ASCII case if asked,
/// forwards or backwards. Non-ASCII bytes only match themselves.
fn scan_bytes(hay: &[u8], n: &[u8], ci: bool, backwards: bool, mut visit: impl FnMut(usize) -> Hit) {
    let m = n.len();
    if m == 0 || m > hay.len() {
        return;
    }
    if !ci {
        if backwards {
            let finder = memchr::memmem::FinderRev::new(n);
            let mut to = hay.len();
            while let Some(at) = finder.rfind(&hay[..to]) {
                to = match visit(at) {
                    Hit::Skip => at + m - 1,
                    Hit::Take => at,
                    Hit::Stop => return,
                };
            }
        } else {
            let finder = memchr::memmem::Finder::new(n);
            let mut from = 0;
            while let Some(k) = hay.get(from..).and_then(|rest| finder.find(rest)) {
                let at = from + k;
                from = match visit(at) {
                    Hit::Skip => at + 1,
                    Hit::Take => at + m,
                    Hit::Stop => return,
                };
            }
        }
        return;
    }
    let (lo, up) = (n[0].to_ascii_lowercase(), n[0].to_ascii_uppercase());
    let same = |at: usize| hay[at..at + m].eq_ignore_ascii_case(n);
    // Candidates: where the first byte matches, up to the last start.
    let starts = &hay[..=hay.len() - m];
    if backwards {
        let mut to = starts.len();
        while let Some(at) = memchr::memrchr2(lo, up, &starts[..to]) {
            to = if !same(at) {
                at
            } else {
                match visit(at) {
                    Hit::Skip => at,
                    Hit::Take => (at + 1).saturating_sub(m),
                    Hit::Stop => return,
                }
            };
        }
    } else {
        let mut from = 0;
        while let Some(k) = starts.get(from..).and_then(|rest| memchr::memchr2(lo, up, rest)) {
            let at = from + k;
            from = if !same(at) {
                at + 1
            } else {
                match visit(at) {
                    Hit::Skip => at + 1,
                    Hit::Take => at + m,
                    Hit::Stop => return,
                }
            };
        }
    }
}

/// The characters outside ASCII that fold to ASCII alone, with what they
/// fold to: with no folding options (canonical singletons), and when
/// folding case. A unit test checks both against every code point.
const CANONICAL_ASCII: [(&str, &str); 3] = [("\u{37E}", ";"), ("\u{1FEF}", "`"), ("\u{212A}", "K")];
const CASELESS_ASCII: [(&str, &str); 13] = [
    ("\u{DF}", "ss"),
    ("\u{17F}", "s"),
    ("\u{37E}", ";"),
    ("\u{1E9E}", "ss"),
    ("\u{1FEF}", "`"),
    ("\u{212A}", "k"),
    ("\u{FB00}", "ff"),
    ("\u{FB01}", "fi"),
    ("\u{FB02}", "fl"),
    ("\u{FB03}", "ffi"),
    ("\u{FB04}", "ffl"),
    ("\u{FB05}", "st"),
    ("\u{FB06}", "st"),
];

/// The characters above that could fold into part of the ASCII needle `n`,
/// as UTF-8.
fn folding_into(n: &[u8], ci: bool) -> ([&'static [u8]; 13], usize) {
    let table: &[(&str, &str)] = if ci { &CASELESS_ASCII } else { &CANONICAL_ASCII };
    let in_needle = |t: u8| n.iter().any(|&b| if ci { b.eq_ignore_ascii_case(&t) } else { b == t });
    let mut out: [&[u8]; 13] = [&[]; 13];
    let mut count = 0;
    for &(c, to) in table {
        if to.bytes().all(in_needle) {
            out[count] = c.as_bytes();
            count += 1;
        }
    }
    (out, count)
}

/// Whether a composed character sequence ends at `end`, just after an
/// ASCII character in `w`: only the next character can join onto it.
fn ascii_ends_sequence(w: &[u8], end: usize) -> bool {
    if end == w.len() || w[end].is_ascii() {
        return true;
    }
    let next = (end + wtf8::width(w[end])).min(w.len());
    fold::clusters(&w[end - 1..next]).contains(&1)
}

fn regex_flags(options: usize) -> usize {
    if options & CASE_INSENSITIVE != 0 { crate::regex::CASE_INSENSITIVE } else { 0 }
}

/// Search `hay`'s UTF-16 range for `needle` under `options`; the match as a
/// UTF-16 (location, length).
pub(crate) fn find(hay: &Text, loc: usize, len: usize, needle: &Text, options: usize) -> Option<(usize, usize)> {
    if options & REGULAR_EXPRESSION != 0 {
        return find_regex(hay, loc, len, needle.bytes, options);
    }
    let mut hit = None;
    each_find(hay, loc, len, needle, options, |s, e| {
        hit = Some((s, e));
        false
    });
    hit.map(|(s, e)| pos_to_utf16(hay, s, e))
}

/// The UTF-16 (location, length) of the text between two positions.
pub(crate) fn pos_to_utf16(t: &Text, s: Pos, e: Pos) -> (usize, usize) {
    let unit = |p: Pos| t.utf16_at(p.byte) + usize::from(p.low);
    let start = unit(s);
    (start, unit(e) - start)
}

/// Each match of `n` in the UTF-16 range `loc..loc + len` of `h` under
/// `options` (not regular expressions): in order, or from the end with
/// `BACKWARDS`, not overlapping, until `f` returns false.
///
/// Literal searches, and searches where both sides are ASCII, compare
/// bytes. So does a search for an ASCII needle folding nothing but case,
/// where the text holds none of the few characters that fold to ASCII (it
/// checks a stretch at a time, ahead of where it looks) and a hit ends a
/// composed character sequence (nothing joins onto the start of an ASCII
/// character). Other searches fold the text as they go (`fold::Stream`)
/// and match whole sequences, or for literal ones whole characters. A
/// needle holding a lone surrogate may match half of a pair, so it is
/// looked for unit by unit.
fn each_find(h: &Text, loc: usize, len: usize, n: &Text, options: usize, mut f: impl FnMut(Pos, Pos) -> bool) {
    if n.bytes.is_empty() {
        return;
    }
    if n.flags & wtf8::HAS_SURROGATE != 0 {
        each_unit_match(h, loc, len, n, options, f);
        return;
    }
    let (from, to) = window(h, loc, len);
    let w = &h.bytes[from..to];
    let mut g = |s: usize, e: usize| f(Pos::at(from + s), Pos::at(from + e));
    let folding = CASE_INSENSITIVE | DIACRITIC_INSENSITIVE | WIDTH_INSENSITIVE;
    let ascii_needle = fold::ascii(n);
    if (options & LITERAL != 0 && options & folding == 0) || (h.is_ascii() && ascii_needle) {
        each_ascii(w, n.bytes, options, true, &mut g);
    } else if options & (LITERAL | DIACRITIC_INSENSITIVE | WIDTH_INSENSITIVE) == 0 && ascii_needle {
        each_ascii_needle(w, n.bytes, options, &mut g);
    } else {
        let per_char = options & LITERAL != 0;
        let fold_options = if per_char { options & CASE_INSENSITIVE } else { options };
        let needle = fold::fold_text(n.bytes, fold_options);
        if needle.is_empty() {
            return;
        }
        let anchored = options & ANCHORED != 0;
        let folds = Folds { needle: &needle, options: fold_options, per_char, anchored };
        if options & BACKWARDS != 0 {
            folds.backwards(w, w.len(), w.len(), &mut g);
        } else {
            folds.forwards(w, 0, 0, &mut g);
        }
    }
}

/// `each_find` on bytes; `bytewise` when any hit will do, otherwise hits
/// must end a composed character sequence.
fn each_ascii(w: &[u8], n: &[u8], options: usize, bytewise: bool, mut f: impl FnMut(usize, usize) -> bool) {
    let (m, ci) = (n.len(), options & CASE_INSENSITIVE != 0);
    let ends = |at: usize| bytewise || ascii_ends_sequence(w, at + m);
    if options & ANCHORED != 0 {
        let at = if options & BACKWARDS != 0 { w.len().checked_sub(m) } else { Some(0) };
        let same = |s: &[u8]| if ci { s.eq_ignore_ascii_case(n) } else { s == n };
        if let Some(at) = at.filter(|&at| w.get(at..at + m).is_some_and(same) && ends(at)) {
            f(at, at + m);
        }
        return;
    }
    scan_bytes(w, n, ci, options & BACKWARDS != 0, |at| {
        if !ends(at) {
            Hit::Skip
        } else if f(at, at + m) {
            Hit::Take
        } else {
            Hit::Stop
        }
    });
}

/// `each_find` for an ASCII needle when nothing but case is folded: on
/// bytes, as `each_ascii`, through stretches of text holding none of the
/// characters that fold into the needle (checked a stretch at a time just
/// ahead of the search, in stretches that double in size), and by folding
/// from the first stretch that holds one.
fn each_ascii_needle(w: &[u8], n: &[u8], options: usize, f: &mut impl FnMut(usize, usize) -> bool) {
    let (m, ci) = (n.len(), options & CASE_INSENSITIVE != 0);
    let (specials, count) = folding_into(n, ci);
    let specials = &specials[..count];
    if specials.is_empty() {
        each_ascii(w, n, options, false, f);
        return;
    }
    // A match spans at most `m` characters of up to three bytes each; the
    // character after it decides whether it ends a sequence.
    let reach = 3 * m + 4;
    let dirty = |a: usize, b: usize| specials.iter().any(|c| memchr::memmem::find(&w[a..b], c).is_some());
    let needle = fold::fold_text(n, options & CASE_INSENSITIVE);
    let anchored = options & ANCHORED != 0;
    let folds = Folds { needle: &needle, options, per_char: false, anchored };
    if options & BACKWARDS != 0 {
        if anchored {
            let a = fold::start_before(w, w.len().saturating_sub(reach), false);
            if dirty(a, w.len()) {
                folds.backwards(w, w.len(), w.len(), f);
            } else {
                each_ascii(w, n, options, false, f);
            }
            return;
        }
        let (mut b, mut limit, mut size) = (w.len(), w.len(), 4096);
        while b > 0 {
            let a = b.saturating_sub(size);
            if dirty(a, (b + reach).min(w.len())) {
                folds.backwards(w, limit, b, f);
                return;
            }
            // Hits starting before `b` and ending by `limit`.
            let end = (b + m - 1).min(limit);
            let mut stop = false;
            if end > a {
                scan_bytes(&w[a..end], n, ci, true, |k| {
                    let at = a + k;
                    if !ascii_ends_sequence(w, at + m) {
                        Hit::Skip
                    } else if f(at, at + m) {
                        limit = at;
                        Hit::Take
                    } else {
                        stop = true;
                        Hit::Stop
                    }
                });
            }
            if stop {
                return;
            }
            b = a;
            size = (size * 2).min(1 << 20);
        }
        return;
    }
    if anchored {
        if dirty(0, reach.min(w.len())) {
            folds.forwards(w, 0, 0, f);
        } else {
            each_ascii(w, n, options, false, f);
        }
        return;
    }
    let (mut a, mut next, mut size) = (0, 0, 4096);
    while a < w.len() {
        let b = (a + size).min(w.len());
        if dirty(a, (b + reach).min(w.len())) {
            folds.forwards(w, fold::start_before(w, a, false), a.max(next), f);
            return;
        }
        // Hits starting before `b`.
        let end = (b + m - 1).min(w.len());
        let mut stop = false;
        scan_bytes(&w[a..end], n, ci, false, |k| {
            let at = a + k;
            if !ascii_ends_sequence(w, at + m) {
                Hit::Skip
            } else if f(at, at + m) {
                next = at + m;
                Hit::Take
            } else {
                stop = true;
                Hit::Stop
            }
        });
        if stop {
            return;
        }
        a = b.max(next);
        size = (size * 2).min(1 << 20);
    }
}

/// A search that folds the text as it goes, for a folded needle.
struct Folds<'n> {
    needle: &'n [u32],
    /// What the text is folded under.
    options: usize,
    /// Matches are whole characters rather than whole sequences.
    per_char: bool,
    anchored: bool,
}

impl Folds<'_> {
    /// Matches in `w` in order, from `start` (a sequence start) on, of those
    /// starting at byte `from` or later. Where the search has been is
    /// forgotten as it goes.
    fn forwards(&self, w: &[u8], start: usize, from: usize, f: &mut impl FnMut(usize, usize) -> bool) {
        let n = self.needle.len();
        let mut st = fold::Stream::new(&w[start..], self.options, self.per_char);
        let mut next = from - start;
        let mut prev = None;
        let mut k = 0;
        while let Some((fi, s)) = st.bound(k) {
            if s == st.len() {
                break;
            }
            // Of sequences sharing a folded index (those before folding to
            // nothing), the first counts.
            let first = prev != Some(fi);
            prev = Some(fi);
            if s >= next
                && first
                && st.matches(fi, self.needle)
                && let Some(e) = st.end_at(fi + n)
            {
                if !f(start + s, start + e) {
                    return;
                }
                next = e;
            }
            if self.anchored {
                return;
            }
            k += 1;
            st.forget_before(k);
        }
    }

    /// Matches in `w` from the end, of those starting before byte `upto`
    /// and ending by `limit`: found by folding a stretch before `upto` that
    /// grows fourfold until the search is done.
    fn backwards(&self, w: &[u8], mut limit: usize, mut upto: usize, f: &mut impl FnMut(usize, usize) -> bool) {
        let n = self.needle.len();
        let mut size = 4096;
        loop {
            let start = fold::start_before(w, upto.saturating_sub(size), self.per_char);
            let mut st = fold::Stream::new(&w[start..limit], self.options, self.per_char);
            let total = st.folded_len();
            if self.anchored {
                if total < n && start > 0 {
                    size = size.saturating_mul(4);
                    continue;
                }
                if let Some(fi) = total.checked_sub(n)
                    && let Some(s) = st.start_at(fi)
                    && st.matches(fi, self.needle)
                    && st.end_at(total) == Some(limit - start)
                {
                    f(start + s, limit);
                }
                return;
            }
            let (chars, bounds) = st.folded();
            for k in (0..bounds.len() - 1).rev() {
                let (fi, s) = bounds[k];
                if start + s >= upto || (k > 0 && bounds[k - 1].0 == fi) || chars.get(fi..fi + n) != Some(self.needle) {
                    continue;
                }
                let e = bounds.partition_point(|&(f, _)| f <= fi + n) - 1;
                let (fe, e) = bounds[e];
                if fe != fi + n || start + e > limit {
                    continue;
                }
                if !f(start + s, start + e) {
                    return;
                }
                limit = start + s;
            }
            if start == 0 {
                return;
            }
            upto = start;
            size = size.saturating_mul(4);
        }
    }
}

/// `each_find` for a needle holding a lone surrogate, which may match half
/// of a surrogate pair: UTF-16 units compared one by one, folding only
/// ASCII case.
fn each_unit_match(h: &Text, loc: usize, len: usize, n: &Text, options: usize, mut f: impl FnMut(Pos, Pos) -> bool) {
    let (s, e) = h.range(loc, len);
    let end_byte = if e.low { e.byte + 4 } else { e.byte };
    let mut hay: Vec<u16> = wtf8::units(&h.bytes[s.byte..end_byte]).collect();
    if e.low {
        hay.pop();
    }
    if s.low {
        hay.remove(0);
    }
    let needle: Vec<u16> = wtf8::units(n.bytes).collect();
    let m = needle.len();
    if m > hay.len() {
        return;
    }
    let ci = options & CASE_INSENSITIVE != 0;
    let fold = |u: u16| if ci && u < 0x80 { u16::from((u as u8).to_ascii_lowercase()) } else { u };
    let matches = |i: usize| hay[i..i + m].iter().zip(&needle).all(|(&a, &b)| fold(a) == fold(b));
    let mut report = |i: usize| f(h.pos(loc + i), h.pos(loc + i + m));
    let last = hay.len() - m;
    match (options & ANCHORED != 0, options & BACKWARDS != 0) {
        (true, backwards) => {
            let i = if backwards { last } else { 0 };
            if matches(i) {
                report(i);
            }
        }
        (false, false) => {
            let mut i = 0;
            while i <= last {
                if matches(i) {
                    if !report(i) {
                        return;
                    }
                    i += m;
                } else {
                    i += 1;
                }
            }
        }
        (false, true) => {
            let mut i = last + 1;
            while i > 0 {
                i -= 1;
                if matches(i) {
                    if !report(i) {
                        return;
                    }
                    i = i.saturating_sub(m - 1);
                }
            }
        }
    }
}

/// The first regular expression match in the range.
fn find_regex(hay: &Text, loc: usize, len: usize, pattern: &[u8], options: usize) -> Option<(usize, usize)> {
    let p = crate::regex::cached(pattern, regex_flags(options))?;
    let matching = if options & ANCHORED != 0 { crate::regex::MATCH_ANCHORED } else { 0 };
    let mut hit = None;
    crate::regex::each_found(&p, hay, NSRange::new(loc, len), matching, |found| {
        hit = found[0];
        false
    });
    hit.map(|(s, e)| to_utf16(hay, s, e))
}

/// Each match of `n` in the UTF-16 `range` of `h` under `options`, in
/// order and without overlaps, with what replaces it: `r`, or for a regular
/// expression `r` as a template expanded for the match. `f(start, end,
/// with)` gets positions in `h`. These are the edits behind both
/// `stringByReplacingOccurrencesOfString:withString:options:range:` and
/// NSMutableString's `replaceOccurrencesOfString:withString:options:range:`.
pub(crate) fn each_replacement(
    h: &Text,
    n: &Text,
    r: &Text,
    options: usize,
    range: NSRange,
    mut f: impl FnMut(Pos, Pos, &[u8]),
) {
    if options & REGULAR_EXPRESSION != 0 {
        let Some(p) = crate::regex::cached(n.bytes, regex_flags(options)) else { return };
        // Anchored, only a match at the start of the range counts.
        let anchored = options & ANCHORED != 0;
        let matching = if anchored { crate::regex::MATCH_ANCHORED } else { 0 };
        let mut with = Vec::new();
        crate::regex::each_found(&p, h, range, matching, |found| {
            with.clear();
            crate::regex::expand(r.bytes, found, h.bytes, &mut with);
            let (s, e) = found[0].expect("the whole match");
            f(Pos::at(s), Pos::at(e), &with);
            !anchored
        });
        return;
    }
    if options & BACKWARDS == 0 {
        each_find(h, range.location, range.length, n, options, |s, e| {
            f(s, e, r.bytes);
            true
        });
        return;
    }
    // Found from the end; made from the start.
    let mut hits = Vec::new();
    each_find(h, range.location, range.length, n, options, |s, e| {
        hits.push((s, e));
        true
    });
    for &(s, e) in hits.iter().rev() {
        f(s, e, r.bytes);
    }
}

/// `h` with every replacement `each_replacement` finds made, and how many
/// there were; `None` when there were none.
pub(crate) fn replaced(h: &Text, n: &Text, r: &Text, options: usize, range: NSRange) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let (mut last, mut count) = (Pos::at(0), 0);
    each_replacement(h, n, r, options, range, |s, e, with| {
        if count == 0 {
            out.reserve(h.bytes.len() + with.len());
        }
        wtf8::push(&mut out, &wtf8::slice(h.bytes, last, s));
        wtf8::push(&mut out, with);
        last = e;
        count += 1;
    });
    if count == 0 {
        return None;
    }
    wtf8::push(&mut out, &wtf8::slice(h.bytes, last, Pos::at(h.bytes.len())));
    Some((out, count))
}

/// `stringByReplacingOccurrencesOfString:withString:options:range:`.
fn replace_occurrences(
    obj: &AnyObject,
    target: &NSString,
    replacement: &NSString,
    options: usize,
    range: NSRange,
) -> Retained<NSString> {
    let (hv, nv, rv) = (view(obj), view(target), view(replacement));
    let h = hv.text();
    super::check_range("stringByReplacingOccurrencesOfString:withString:options:range:", range, h.utf16_len);
    match replaced(&h, &nv.text(), &rv.text(), options, range) {
        Some((out, _)) => {
            let flags = wtf8::flags_of(&out, true);
            inline::new(&out, wtf8::utf16_len(&out), flags)
        }
        None => keep(obj, &hv),
    }
}

/// The receiver, or an immutable copy of it, for edits that change nothing.
pub(crate) fn keep(obj: &AnyObject, v: &StrView) -> Retained<NSString> {
    let t = v.text();
    if v.is_immutable() {
        // SAFETY: the receiver is a string.
        unsafe { Retained::retain((obj as *const AnyObject).cast_mut().cast()) }.expect("non-null")
    } else {
        inline::new(t.bytes, t.utf16_len, t.flags)
    }
}

/// Whether `p`'s UTF-16 units begin (or end) `h`'s.
fn has_affix(h: &Text, p: &Text, suffix: bool) -> bool {
    if p.bytes.is_empty() || p.utf16_len > h.utf16_len {
        return false;
    }
    let lone = (h.flags | p.flags) & wtf8::HAS_SURROGATE != 0;
    if !lone {
        return if suffix { h.bytes.ends_with(p.bytes) } else { h.bytes.starts_with(p.bytes) };
    }
    let (hu, pu): (Vec<u16>, Vec<u16>) = (wtf8::units(h.bytes).collect(), wtf8::units(p.bytes).collect());
    if suffix { hu.ends_with(&pu) } else { hu.starts_with(&pu) }
}

/// `commonPrefixWithString:options:`: the receiver's longest prefix that
/// matches a prefix of `other`, in whole composed character sequences
/// unless literal.
pub(crate) fn common_prefix(obj: &AnyObject, other: &NSString, options: usize) -> Retained<NSString> {
    let (av, bv) = (view(obj), view(other));
    let (a, b) = (av.text(), bv.text());
    let end = if options & LITERAL != 0 && options & CASE_INSENSITIVE == 0 {
        let mut n = a.bytes.iter().zip(b.bytes).take_while(|(x, y)| x == y).count();
        while n > 0 && n < a.bytes.len() && a.bytes[n] & 0xC0 == 0x80 {
            n -= 1;
        }
        n
    } else if a.is_ascii() && b.is_ascii() {
        let ci = options & CASE_INSENSITIVE != 0;
        a.bytes.iter().zip(b.bytes).take_while(|(x, y)| if ci { x.eq_ignore_ascii_case(y) } else { x == y }).count()
    } else {
        let mut sa = fold::Stream::new(a.bytes, options, false);
        let mut sb = fold::Stream::new(b.bytes, options, false);
        let (mut end, mut from, mut k) = (0, 0, 1);
        // The receiver's sequences in turn: each one's folded code points
        // must continue the other's, and the prefix may end after it where
        // a sequence of the other ends too (one of the other's may span
        // several of the receiver's, as `ß` does `SS`).
        while let Some((to, e)) = sa.bound(k) {
            if !(from..to).all(|i| sa.char(i).is_some_and(|c| sb.char(i) == Some(c))) {
                break;
            }
            if sb.end_at(to).is_some() {
                end = e;
            }
            sa.forget_before(k);
            sb.forget_folded_before(to);
            (from, k) = (to, k + 1);
        }
        end
    };
    let bytes = &a.bytes[..end];
    inline::new(bytes, wtf8::utf16_len(bytes), wtf8::flags_of(bytes, true))
}

/// The range of the composed character sequence holding UTF-16 index `i`.
pub(crate) fn composed_at(t: &Text, i: usize) -> (usize, usize) {
    // Paragraph separators always end a sequence, so only the paragraph
    // around `i` needs segmenting.
    let pos = t.pos(i);
    let (ps, pe) = super::lines::paragraph_bytes(t.bytes, pos.byte, pos.byte);
    let bounds = fold::clusters(&t.bytes[ps..pe]);
    let rel = pos.byte - ps;
    let k = bounds.partition_point(|&b| b <= rel).saturating_sub(1);
    let (s, e) = (ps + bounds[k], ps + bounds.get(k + 1).copied().unwrap_or(pe - ps));
    to_utf16(t, s, e)
}

/// Search a range of `h` for a member of a character set.
fn find_char(h: &Text, set: &AnyObject, loc: usize, len: usize, options: usize) -> Option<(usize, usize)> {
    let m = charset::membership(set);
    let (from, to) = window(h, loc, len);
    let anchored = options & ANCHORED != 0;
    let mut hit = None;
    if options & BACKWARDS != 0 {
        let mut at = to;
        while at > from {
            let s = wtf8::prev_boundary(h.bytes, at);
            if m.contains(wtf8::decode(h.bytes, s).0) {
                hit = Some((s, at));
                break;
            }
            if anchored {
                break;
            }
            at = s;
        }
    } else {
        let mut at = from;
        while at < to {
            let (c, w) = wtf8::decode(h.bytes, at);
            if m.contains(c) {
                hit = Some((at, at + w));
                break;
            }
            if anchored {
                break;
            }
            at += w;
        }
    }
    hit.map(|(s, e)| to_utf16(h, s, e))
}

fn this(obj: &Helper) -> &AnyObject {
    obj
}

fn range_of(obj: &AnyObject, needle: &NSString, options: usize, range: Option<NSRange>, method: &str) -> NSRange {
    let (hv, nv) = (view(obj), view(needle));
    let (h, n) = (hv.text(), nv.text());
    let range = range.unwrap_or(NSRange::new(0, h.utf16_len));
    super::check_range(method, range, h.utf16_len);
    found(find(&h, range.location, range.length, &n, options))
}

fn is_found(r: NSRange) -> bool {
    r.location != NOT_FOUND.location
}

define_class!(
    // NSString's search methods, copied onto NSString when it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringSearch"]
    pub(crate) struct Helper;

    impl Helper {
        #[unsafe(method(rangeOfString:))]
        fn range_of_string(&self, needle: &NSString) -> NSRange {
            range_of(this(self), needle, 0, None, "rangeOfString:")
        }

        #[unsafe(method(rangeOfString:options:))]
        fn range_of_string_options(&self, needle: &NSString, options: NSStringCompareOptions) -> NSRange {
            range_of(this(self), needle, options.0, None, "rangeOfString:options:")
        }

        #[unsafe(method(rangeOfString:options:range:))]
        fn range_of_string_options_range(&self, needle: &NSString, options: NSStringCompareOptions, range: NSRange) -> NSRange {
            range_of(this(self), needle, options.0, Some(range), "rangeOfString:options:range:")
        }

        #[unsafe(method(rangeOfString:options:range:locale:))]
        fn range_of_string_locale(
            &self,
            needle: &NSString,
            options: NSStringCompareOptions,
            range: NSRange,
            _locale: Option<&AnyObject>,
        ) -> NSRange {
            range_of(this(self), needle, options.0, Some(range), "rangeOfString:options:range:locale:")
        }

        #[unsafe(method(containsString:))]
        fn contains_string(&self, needle: &NSString) -> bool {
            is_found(range_of(this(self), needle, 0, None, "containsString:"))
        }

        #[unsafe(method(localizedCaseInsensitiveContainsString:))]
        fn localized_ci_contains(&self, needle: &NSString) -> bool {
            is_found(range_of(this(self), needle, CASE_INSENSITIVE, None, "localizedCaseInsensitiveContainsString:"))
        }

        #[unsafe(method(localizedStandardContainsString:))]
        fn localized_standard_contains(&self, needle: &NSString) -> bool {
            let options = CASE_INSENSITIVE | DIACRITIC_INSENSITIVE;
            is_found(range_of(this(self), needle, options, None, "localizedStandardContainsString:"))
        }

        #[unsafe(method(localizedStandardRangeOfString:))]
        fn localized_standard_range(&self, needle: &NSString) -> NSRange {
            let options = CASE_INSENSITIVE | DIACRITIC_INSENSITIVE;
            range_of(this(self), needle, options, None, "localizedStandardRangeOfString:")
        }

        #[unsafe(method(hasPrefix:))]
        fn has_prefix(&self, prefix: &NSString) -> bool {
            has_affix(&view(this(self)).text(), &view(prefix).text(), false)
        }

        #[unsafe(method(hasSuffix:))]
        fn has_suffix(&self, suffix: &NSString) -> bool {
            has_affix(&view(this(self)).text(), &view(suffix).text(), true)
        }

        #[unsafe(method(rangeOfCharacterFromSet:))]
        fn range_of_character(&self, set: &NSCharacterSet) -> NSRange {
            let v = view(this(self));
            let t = v.text();
            found(find_char(&t, set, 0, t.utf16_len, 0))
        }

        #[unsafe(method(rangeOfCharacterFromSet:options:))]
        fn range_of_character_options(&self, set: &NSCharacterSet, options: NSStringCompareOptions) -> NSRange {
            let v = view(this(self));
            let t = v.text();
            found(find_char(&t, set, 0, t.utf16_len, options.0))
        }

        #[unsafe(method(rangeOfCharacterFromSet:options:range:))]
        fn range_of_character_range(&self, set: &NSCharacterSet, options: NSStringCompareOptions, range: NSRange) -> NSRange {
            let v = view(this(self));
            let t = v.text();
            super::check_range("rangeOfCharacterFromSet:options:range:", range, t.utf16_len);
            found(find_char(&t, set, range.location, range.length, options.0))
        }

        #[unsafe(method(rangeOfComposedCharacterSequenceAtIndex:))]
        fn composed_sequence_at(&self, index: NSUInteger) -> NSRange {
            let v = view(this(self));
            let t = v.text();
            if index >= t.utf16_len {
                super::index_panic("rangeOfComposedCharacterSequenceAtIndex:", index, t.utf16_len);
            }
            let (loc, len) = composed_at(&t, index);
            NSRange::new(loc, len)
        }

        #[unsafe(method(rangeOfComposedCharacterSequencesForRange:))]
        fn composed_sequences_for(&self, range: NSRange) -> NSRange {
            let v = view(this(self));
            let t = v.text();
            super::check_range("rangeOfComposedCharacterSequencesForRange:", range, t.utf16_len);
            if range.length == 0 && range.location == t.utf16_len {
                // An empty range at the end, a caret after the last
                // character, stays as it is.
                range
            } else {
                let (s, _) = composed_at(&t, range.location.min(t.utf16_len - 1));
                let last = if range.length > 0 { range.end() - 1 } else { range.location };
                let (e_loc, e_len) = composed_at(&t, last.min(t.utf16_len - 1));
                NSRange::new(s, e_loc + e_len - s)
            }
        }

        #[unsafe(method_id(stringByReplacingOccurrencesOfString:withString:))]
        fn replacing(&self, target: &NSString, replacement: &NSString) -> Retained<NSString> {
            let len = view(this(self)).text().utf16_len;
            replace_occurrences(this(self), target, replacement, 0, NSRange::new(0, len))
        }

        #[unsafe(method_id(stringByReplacingOccurrencesOfString:withString:options:range:))]
        fn replacing_options(
            &self,
            target: &NSString,
            replacement: &NSString,
            options: NSStringCompareOptions,
            range: NSRange,
        ) -> Retained<NSString> {
            replace_occurrences(this(self), target, replacement, options.0, range)
        }

        #[unsafe(method_id(stringByReplacingCharactersInRange:withString:))]
        fn replacing_characters(&self, range: NSRange, replacement: &NSString) -> Retained<NSString> {
            let hv = view(this(self));
            let h = hv.text();
            super::check_range("stringByReplacingCharactersInRange:withString:", range, h.utf16_len);
            let rv = view(replacement);
            let r = rv.text();
            let (s, e) = h.range(range.location, range.length);
            let mut out = h.bytes.to_vec();
            wtf8::splice(&mut out, s, e, r.bytes);
            let flags = wtf8::flags_of(&out, true);
            inline::new(&out, h.utf16_len - range.length + r.utf16_len, flags)
        }

        #[unsafe(method_id(stringByPaddingToLength:withString:startingAtIndex:))]
        fn padding(&self, new_length: NSUInteger, pad: &NSString, index: NSUInteger) -> Retained<NSString> {
            pad_to(this(self), new_length, pad, index)
        }
    }
);

/// `stringByPaddingToLength:withString:startingAtIndex:`.
fn pad_to(obj: &AnyObject, new_length: usize, pad: &NSString, index: usize) -> Retained<NSString> {
    let hv = view(obj);
    let h = hv.text();
    if new_length <= h.utf16_len {
        let (_, e) = h.range(0, new_length);
        let bytes = wtf8::slice(h.bytes, wtf8::Pos::at(0), e);
        return inline::new(&bytes, new_length, wtf8::flags_of(&bytes, true));
    }
    let pv = view(pad);
    let p = pv.text();
    if index >= p.utf16_len {
        super::index_panic("stringByPaddingToLength:withString:startingAtIndex:", index, p.utf16_len);
    }
    let pad_units: Vec<u16> = wtf8::units(p.bytes).collect();
    let mut units: Vec<u16> = wtf8::units(h.bytes).collect();
    let mut k = index;
    while units.len() < new_length {
        units.push(pad_units[k]);
        k = (k + 1) % pad_units.len();
    }
    let bytes = wtf8::from_utf16(units.iter().copied(), units.len());
    inline::new(&bytes, new_length, wtf8::flags_of(&bytes, true))
}

/// Add the search methods to NSString.
pub(crate) fn install(target: &AnyClass) {
    super::install::copy_methods(Helper::class(), target, false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ascii_fold(c: char, options: usize) -> Option<String> {
        let f = fold::fold_str(c.encode_utf8(&mut [0; 4]), options);
        f.is_ascii().then_some(f)
    }

    #[test]
    fn characters_folding_to_ascii() {
        let (mut plain, mut caseless) = (Vec::new(), Vec::new());
        for c in (0x80..=0x10FFFF).filter_map(char::from_u32) {
            if let Some(f) = all_ascii_fold(c, 0) {
                plain.push((c, f));
            }
            if let Some(f) = all_ascii_fold(c, CASE_INSENSITIVE) {
                caseless.push((c, f));
            }
        }
        let owned =
            |t: &[(&str, &str)]| t.iter().map(|&(c, f)| (c.chars().next().unwrap(), f.to_string())).collect::<Vec<_>>();
        assert_eq!(plain, owned(&CANONICAL_ASCII));
        assert_eq!(caseless, owned(&CASELESS_ASCII));
    }

    /// Random text from pieces that exercise sequence boundaries.
    fn text(seed: &mut u64, len: usize) -> Vec<u8> {
        #[rustfmt::skip]
        const PIECES: [&str; 34] = [
            "a", "b", "A", "B", "ab", "\r", "\n", " ", "\u{301}", "\u{600}", "\u{200D}", "\u{FE0F}", "é",
            "\u{1F389}", "\u{1F1EB}", "\u{1F1F7}", "\u{1100}", "\u{1161}", "漢", "\u{212A}", "ß", "\u{94D}",
            "\u{915}", "\u{903}", "\u{E33}", "\u{AC00}", "\u{AC01}", "\u{11A8}", "\u{1F3FB}", "\u{200C}",
            "\u{2764}", "\u{1F468}", "\u{0}", "\u{85}",
        ];
        let mut out = Vec::new();
        for _ in 0..len {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            let k = (*seed % (PIECES.len() as u64 + 1)) as usize;
            match PIECES.get(k) {
                Some(p) => out.extend_from_slice(p.as_bytes()),
                // A lone surrogate.
                None => out.extend_from_slice(&[0xED, 0xA0, 0x80]),
            }
        }
        out
    }

    #[test]
    fn clusters_match_the_segmenter() {
        let mut seed = 0x2545_F491_4F6C_DD1D;
        for _ in 0..50_000 {
            let len = (seed % 12) as usize;
            let t = text(&mut seed, len);
            // Consecutive prepended characters are the one place the fast
            // path is stricter: each stands alone.
            if t.windows(4).any(|w| w == "\u{600}\u{600}".as_bytes()) {
                continue;
            }
            assert_eq!(fold::clusters(&t), fold::clusters_of_pieces(&t), "{:?}", String::from_utf8_lossy(&t));
        }
    }

    /// The search as first written: fold all the text, then look.
    fn reference(w: &[u8], needle: &[u8], options: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let per_char = options & LITERAL != 0;
        if per_char && options & (CASE_INSENSITIVE | DIACRITIC_INSENSITIVE | WIDTH_INSENSITIVE) == 0 {
            each_ascii(w, needle, options, true, |s, e| {
                out.push((s, e));
                true
            });
            return out;
        }
        let fold_options = if per_char { options & CASE_INSENSITIVE } else { options };
        let hay = fold::Folded::new(w, fold_options, per_char);
        let needle = fold::fold_text(needle, fold_options);
        let n = needle.len();
        if n == 0 {
            return out;
        }
        let matches_at = |fi: usize| hay.chars.get(fi..fi + n) == Some(needle.as_slice());
        if options & ANCHORED != 0 {
            let try_at = |fi: usize| {
                if !matches_at(fi) {
                    return None;
                }
                Some((hay.source_at(fi)?, hay.source_end_at(fi + n)?))
            };
            let hit = if options & BACKWARDS != 0 {
                hay.chars.len().checked_sub(n).and_then(try_at).filter(|&(_, e)| e == w.len())
            } else {
                try_at(0)
            };
            out.extend(hit);
            return out;
        }
        let b = &hay.bounds;
        let try_k = |k: usize| -> Option<(usize, usize)> {
            let (fi, s) = b[k];
            let first = *hay.chars.get(fi)? == needle[0] && (k == 0 || b[k - 1].0 != fi);
            if !first || !matches_at(fi) {
                return None;
            }
            Some((s, hay.source_end_at(fi + n)?))
        };
        if options & BACKWARDS != 0 {
            let mut limit = w.len();
            for k in (0..b.len()).rev() {
                if let Some((s, e)) = try_k(k)
                    && e <= limit
                {
                    out.push((s, e));
                    limit = s;
                }
            }
        } else {
            let mut next = 0;
            for (k, &(_, at)) in b.iter().enumerate() {
                if at >= next
                    && let Some((s, e)) = try_k(k)
                {
                    out.push((s, e));
                    next = e;
                }
            }
        }
        out
    }

    /// Every match `each_find` reports, as byte ranges.
    fn found_all(t: &[u8], n: &[u8], options: usize) -> Vec<(usize, usize)> {
        let h = Text::plain(t, wtf8::utf16_len(t), wtf8::flags_of(t, true));
        let n = Text::plain(n, wtf8::utf16_len(n), wtf8::flags_of(n, true));
        let mut out = Vec::new();
        each_find(&h, 0, h.utf16_len, &n, options, |s, e| {
            assert!(!s.low && !e.low);
            out.push((s.byte, e.byte));
            true
        });
        out
    }

    #[test]
    fn searches_match_folding_everything_first() {
        let needles: [&str; 16] = [
            "a",
            "ab",
            "b ",
            "\n",
            "aB",
            "k",
            "aa",
            "ss",
            "st",
            "é",
            "ß",
            "\u{301}",
            "漢",
            "e\u{301}",
            "\u{1F389}",
            "\u{212A}",
        ];
        let options = [
            0,
            CASE_INSENSITIVE,
            BACKWARDS,
            ANCHORED,
            BACKWARDS | ANCHORED | CASE_INSENSITIVE,
            DIACRITIC_INSENSITIVE,
            DIACRITIC_INSENSITIVE | CASE_INSENSITIVE | BACKWARDS,
            WIDTH_INSENSITIVE | ANCHORED,
            LITERAL,
            LITERAL | CASE_INSENSITIVE,
            LITERAL | CASE_INSENSITIVE | BACKWARDS,
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15;
        // Fewer rounds unoptimized, where the reference is slow.
        let rounds = if cfg!(debug_assertions) { 1_200 } else { 6_000 };
        for round in 0..rounds {
            // Mostly short text; now and then long enough to cross the
            // stretches the searches check and fold at a time.
            let len = if round % 300 == 0 { 5_000 } else { (seed % 12) as usize };
            let mut t = text(&mut seed, len);
            if round % 400 == 150 {
                // A long ASCII run, then the rest.
                let mut long = b"xa ".repeat(3_000);
                long.extend_from_slice(&t);
                t = long;
            }
            for n in needles {
                for &o in &options {
                    assert_eq!(
                        found_all(&t, n.as_bytes(), o),
                        reference(&t, n.as_bytes(), o),
                        "{n:?} in {:?}, options {o}",
                        String::from_utf8_lossy(&t)
                    );
                }
            }
        }
    }

    #[test]
    fn streams_fold_as_everything_at_once_does() {
        let mut seed = 0x2545_F491_4F6C_DD1D;
        for round in 0..300 {
            let mut t = text(&mut seed, if round % 10 == 0 { 3_000 } else { 40 });
            if round % 7 == 0 {
                t.splice(0..0, b"ascii ".repeat(200));
            }
            for (options, per_char) in [(0, false), (CASE_INSENSITIVE | DIACRITIC_INSENSITIVE, false), (0, true)] {
                let whole = fold::Folded::new(&t, options, per_char);
                let mut st = fold::Stream::new(&t, options, per_char);
                let (chars, bounds) = st.folded();
                assert_eq!((chars, bounds), (&whole.chars[..], &whole.bounds[..]));
            }
        }
    }
}
