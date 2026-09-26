//! Folding text for comparison and search, and composed character
//! sequences.
//!
//! Foundation compares and searches non-literally by default: canonically
//! equivalent text matches (`é` and `e` + U+0301), and the options add case
//! folding (full folding, so `ß` matches `ss`), diacritic removal and width
//! folding. Sidestep does this by folding both sides into code points (NFD,
//! then the requested foldings) and comparing those, a piece at a time
//! (`Stream`), so a walk that stops early pays only for what it looked at.
//! A match must start and end on the receiver's composed character sequence
//! boundaries, so `e` doesn't match the start of `e` + U+0301.
//!
//! Composed character sequences are grapheme clusters as ICU4X finds them,
//! adjusted the way Foundation's are: CR and LF are separate sequences, and
//! a combining mark joins the character before it even when that is a
//! control character.

use icu_casemap::CaseMapperBorrowed;
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
use icu_properties::CodePointMapData;
use icu_properties::props::{GeneralCategory, GeneralCategoryGroup, GraphemeClusterBreak};
use icu_segmenter::GraphemeClusterSegmenter;

use super::index::Text;
use super::wtf8;

pub(crate) const CASE_INSENSITIVE: usize = 1;
pub(crate) const LITERAL: usize = 2;
pub(crate) const BACKWARDS: usize = 4;
pub(crate) const ANCHORED: usize = 8;
pub(crate) const NUMERIC: usize = 64;
pub(crate) const DIACRITIC_INSENSITIVE: usize = 128;
pub(crate) const WIDTH_INSENSITIVE: usize = 256;
pub(crate) const FORCED_ORDERING: usize = 512;
pub(crate) const REGULAR_EXPRESSION: usize = 1024;

/// The first character of `c`'s canonical decomposition, when that differs
/// from `c`: `é` gives `e`. Lossy conversions write it for characters an
/// encoding lacks.
pub(crate) fn base_letter(c: u32) -> Option<u32> {
    let c = char::from_u32(c)?;
    let base = DecomposingNormalizerBorrowed::new_nfd().normalize_iter(std::iter::once(c)).next()?;
    (base != c).then_some(u32::from(base))
}

pub(crate) fn general_category(c: u32) -> GeneralCategory {
    CodePointMapData::<GeneralCategory>::new().get32(c)
}

fn is_mark(c: u32) -> bool {
    GeneralCategoryGroup::Mark.contains(general_category(c))
}

/// Width folding: the fullwidth and halfwidth forms map to their ordinary
/// counterparts.
fn fold_width(c: char, out: &mut String) {
    match u32::from(c) {
        0x3000 => out.push(' '),
        0xFF01..=0xFF5E => out.push(char::from_u32(u32::from(c) - 0xFF01 + 0x21).unwrap_or(c)),
        0xFF5F..=0xFFEF => out.extend(DecomposingNormalizerBorrowed::new_nfkd().normalize_iter(std::iter::once(c))),
        _ => out.push(c),
    }
}

/// Fold `s` for comparison under `options`: canonical decomposition, then
/// case folding, diacritic removal and width folding as asked.
pub(crate) fn fold_str(s: &str, options: usize) -> String {
    let nfd = DecomposingNormalizerBorrowed::new_nfd();
    let mut text = nfd.normalize(s).into_owned();
    if options & WIDTH_INSENSITIVE != 0 {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            fold_width(c, &mut out);
        }
        text = nfd.normalize(&out).into_owned();
    }
    if options & CASE_INSENSITIVE != 0 {
        text = CaseMapperBorrowed::new().fold_string(&text).into_owned();
        text = nfd.normalize(&text).into_owned();
    }
    if options & DIACRITIC_INSENSITIVE != 0 {
        text.retain(|c| general_category(u32::from(c)) != GeneralCategory::NonspacingMark);
    }
    text
}

/// Fold WTF-8 text into code points; lone surrogates pass through
/// unchanged.
pub(crate) fn fold_text(bytes: &[u8], options: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len());
    Memo::new(options, bytes.len()).fold(bytes, &mut out);
    out
}

/// The end of the run of ASCII (or, with `ascii` false, non-ASCII) bytes
/// starting at `at`.
fn run_end(bytes: &[u8], at: usize, ascii: bool) -> usize {
    bytes[at..].iter().position(|b| b.is_ascii() != ascii).map_or(bytes.len(), |k| at + k)
}

/// Fold a stretch of text without the memo.
fn fold_stretch(bytes: &[u8], options: usize, out: &mut Vec<u32>) {
    for_each_piece(bytes, |piece| match piece {
        Piece::Str(s) if options & (CASE_INSENSITIVE | WIDTH_INSENSITIVE) == 0 => {
            // Decomposing alone needs no string of its own.
            let marks_go = options & DIACRITIC_INSENSITIVE != 0;
            let nfd = DecomposingNormalizerBorrowed::new_nfd().normalize_iter(s.chars());
            out.extend(
                nfd.filter(|&c| !marks_go || general_category(u32::from(c)) != GeneralCategory::NonspacingMark)
                    .map(u32::from),
            );
        }
        Piece::Str(s) => out.extend(fold_str(s, options).chars().map(u32::from)),
        Piece::Surrogate(u) => out.push(u),
    });
}

pub(crate) enum Piece<'a> {
    Str(&'a str),
    Surrogate(u32),
}

/// Split WTF-8 into its UTF-8 stretches and its lone surrogates.
pub(crate) fn for_each_piece(bytes: &[u8], mut f: impl FnMut(Piece)) {
    let mut start = 0;
    let mut at = 0;
    while at < bytes.len() {
        let w = wtf8::width(bytes[at]);
        if bytes[at] == 0xED && bytes.get(at + 1).is_some_and(|&b| b >= 0xA0) {
            if start < at {
                // SAFETY: a stretch without surrogates is UTF-8.
                f(Piece::Str(unsafe { std::str::from_utf8_unchecked(&bytes[start..at]) }));
            }
            f(Piece::Surrogate(wtf8::decode(bytes, at).0));
            start = at + w;
        }
        at += w;
    }
    if start < bytes.len() {
        // SAFETY: as above.
        f(Piece::Str(unsafe { std::str::from_utf8_unchecked(&bytes[start..]) }));
    }
}

/// The byte offsets where composed character sequences start in `s`, and
/// its length at the end.
pub(crate) fn cluster_bounds(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    // A mark after a control character joins it, where UAX #29 would
    // start a new cluster.
    let icu: Vec<usize> = GraphemeClusterSegmenter::new()
        .segment_str(s)
        .filter(|&at| at == 0 || at == s.len() || !s[at..].chars().next().is_some_and(|c| is_mark(u32::from(c))))
        .collect();
    let mut out = Vec::with_capacity(icu.len() + 1);
    for (k, &at) in icu.iter().enumerate() {
        out.push(at);
        // A prepended character (U+0600 and the like) stands alone.
        if let Some(&next) = icu.get(k + 1)
            && let Some(c) = s[at..next].chars().next()
            && c.len_utf8() < next - at
            && CodePointMapData::<GraphemeClusterBreak>::new().get(c) == GraphemeClusterBreak::Prepend
        {
            out.push(at + c.len_utf8());
        }
        // CR LF is two sequences.
        if let Some(&next) = icu.get(k + 1)
            && next > at + 1
            && b[at] == b'\r'
            && b[at + 1] == b'\n'
        {
            out.push(at + 1);
        }
    }
    out
}

/// The composed character sequences of WTF-8 text, as byte offsets (each
/// start, and the end). A lone surrogate is a sequence of its own.
pub(crate) fn clusters(bytes: &[u8]) -> Vec<usize> {
    let mut out = Vec::with_capacity(bytes.len() / 2 + 2);
    cluster_starts(bytes, |at| out.push(at));
    out.push(bytes.len());
    out
}

/// Whether a character stands alone: no rule joins it to the characters
/// on either side unless they are marks, joiners, jamo or the like.
fn stands_alone(c: u32) -> bool {
    let gcb = CodePointMapData::<GraphemeClusterBreak>::new().get32(c);
    (0xD800..0xE000).contains(&c)
        || (matches!(gcb, GraphemeClusterBreak::Other | GraphemeClusterBreak::LV | GraphemeClusterBreak::LVT)
            && !is_mark(c))
}

/// Call `f` with the start of each composed character sequence, in order.
pub(crate) fn cluster_starts(bytes: &[u8], mut f: impl FnMut(usize)) {
    let mut at = 0;
    while at < bytes.len() {
        at = next_starts(bytes, at, usize::MAX, &mut f);
    }
}

/// Call `f` with the composed character sequence starts in the next piece
/// of `bytes` from `at`, itself a start, and return where the piece ends,
/// again a start (or the end): a run of ASCII, cut after `cap` bytes, and,
/// if the run ended, the stretch of other characters after it.
///
/// A sequence always starts at an ASCII character (nothing joins onto one:
/// CR LF and prepended characters are split, as above), so each ASCII
/// character is a sequence to itself unless what follows joins it. Between
/// ASCII runs, a stretch of characters that each stand alone is a sequence
/// per character; any other stretch goes to the segmenter with the ASCII
/// character before it, which a mark can join.
pub(crate) fn next_starts(bytes: &[u8], at: usize, cap: usize, mut f: impl FnMut(usize)) -> usize {
    let limit = at.saturating_add(cap).min(bytes.len());
    let end = bytes[at..limit].iter().position(|b| !b.is_ascii()).map_or(limit, |k| at + k);
    (at..end).for_each(&mut f);
    if end == bytes.len() || bytes[end].is_ascii() {
        return end;
    }
    let stop = run_end(bytes, end, false);
    let mut k = end;
    while k < stop && stands_alone(wtf8::decode(bytes, k).0) {
        k += wtf8::width(bytes[k]);
    }
    if k == stop {
        let mut k = end;
        while k < stop {
            f(k);
            k += wtf8::width(bytes[k]);
        }
        return stop;
    }
    // Segment the stretch with the ASCII character before it, if this piece
    // has one; that character's start is already out, and the stretch's
    // end is the next piece's.
    let from = if end > at { end - 1 } else { end };
    let inner = clusters_of_pieces(&bytes[from..stop]);
    inner[..inner.len() - 1].iter().skip(usize::from(from < end)).for_each(|&b| f(from + b));
    stop
}

/// A composed character sequence start at or before byte `at` (a character
/// boundary) of `bytes`, found without segmenting from the beginning: an
/// ASCII character, or a character that stands alone after another that
/// does. With `per_char` every character starts one.
pub(crate) fn start_before(bytes: &[u8], at: usize, per_char: bool) -> usize {
    let mut at = at.min(bytes.len());
    while at > 0 && at < bytes.len() && bytes[at] & 0xC0 == 0x80 {
        at -= 1;
    }
    if per_char {
        return at;
    }
    while at > 0 && at < bytes.len() {
        if bytes[at].is_ascii() {
            return at;
        }
        let before = wtf8::prev_boundary(bytes, at);
        if stands_alone(wtf8::decode(bytes, at).0) && stands_alone(wtf8::decode(bytes, before).0) {
            return at;
        }
        at = before;
    }
    at
}

/// `clusters` by the segmenter alone, piece by piece.
pub(crate) fn clusters_of_pieces(bytes: &[u8]) -> Vec<usize> {
    let mut out = vec![0];
    let mut base = 0;
    for_each_piece(bytes, |piece| match piece {
        Piece::Str(s) => {
            out.extend(cluster_bounds(s).into_iter().skip(1).map(|b| base + b));
            base += s.len();
        }
        Piece::Surrogate(_) => {
            base += 3;
            out.push(base);
        }
    });
    out.dedup();
    out
}

/// Text folded a piece at a time, as a search or comparison walks it: the
/// folded code points and composed character sequence bounds found so far.
/// A walk that stops early pays only for what it looked at, and what it has
/// passed can be dropped, so a walk over long text needs little memory.
///
/// Folded indices and bound numbers count from the start of the text;
/// asking for one that was dropped panics.
pub(crate) struct Stream<'a> {
    bytes: &'a [u8],
    /// Every character is a boundary (literal searches), rather than every
    /// composed character sequence.
    per_char: bool,
    memo: Memo,
    /// Bytes folded so far; a sequence start.
    at: usize,
    chars: Vec<u32>,
    /// The folded index of `chars[0]`.
    chars_base: usize,
    /// (folded index, byte) at each sequence start, then at the end.
    bounds: Vec<(usize, usize)>,
    /// The number of `bounds[0]`.
    bounds_base: usize,
    starts: Vec<usize>,
    done: bool,
}

/// How many ASCII bytes (or, per character, characters) a piece folds.
const PIECE: usize = 256;

impl<'a> Stream<'a> {
    /// A walk over `bytes` (WTF-8, starting on a sequence start) folded
    /// under `options`.
    pub(crate) fn new(bytes: &'a [u8], options: usize, per_char: bool) -> Self {
        Stream {
            bytes,
            per_char,
            memo: Memo::new(options, bytes.len()),
            at: 0,
            chars: Vec::new(),
            chars_base: 0,
            bounds: Vec::new(),
            bounds_base: 0,
            starts: Vec::new(),
            done: false,
        }
    }

    /// Fold the next piece; false once everything is folded.
    fn grow(&mut self) -> bool {
        if self.done {
            return false;
        }
        let Stream { bytes, per_char, memo, at, chars, chars_base, bounds, starts, done, .. } = self;
        starts.clear();
        let next = if *per_char {
            let mut k = *at;
            while k < bytes.len() && starts.len() < PIECE {
                starts.push(k);
                k += wtf8::width(bytes[k]);
            }
            k
        } else {
            next_starts(bytes, *at, PIECE, |s| starts.push(s))
        };
        for (i, &s) in starts.iter().enumerate() {
            bounds.push((*chars_base + chars.len(), s));
            memo.fold(&bytes[s..starts.get(i + 1).copied().unwrap_or(next)], chars);
        }
        *at = next;
        if next >= bytes.len() {
            bounds.push((*chars_base + chars.len(), bytes.len()));
            *done = true;
        }
        true
    }

    /// Fold everything.
    pub(crate) fn finish(&mut self) {
        while self.grow() {}
    }

    /// The length of the text walked.
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The number of folded code points, once everything is folded.
    pub(crate) fn folded_len(&mut self) -> usize {
        self.finish();
        self.chars_base + self.chars.len()
    }

    /// Folded code point `i`, if the text folds to that many.
    #[inline]
    pub(crate) fn char(&mut self, i: usize) -> Option<u32> {
        while i >= self.chars_base + self.chars.len() {
            if !self.grow() {
                return None;
            }
        }
        Some(self.chars[i - self.chars_base])
    }

    /// Bound `k`: the folded index and byte where the kth sequence starts,
    /// or after the last one, the end.
    #[inline]
    pub(crate) fn bound(&mut self, k: usize) -> Option<(usize, usize)> {
        while k >= self.bounds_base + self.bounds.len() {
            if !self.grow() {
                return None;
            }
        }
        Some(self.bounds[k - self.bounds_base])
    }

    /// Whether the folded text from index `i` on begins with `needle`.
    pub(crate) fn matches(&mut self, i: usize, needle: &[u32]) -> bool {
        needle.iter().enumerate().all(|(j, &c)| self.char(i + j) == Some(c))
    }

    /// The byte where a match ending at folded index `i` ends: the last
    /// sequence bound at `i`, if there is one (clusters that fold to nothing
    /// join the match before them).
    pub(crate) fn end_at(&mut self, i: usize) -> Option<usize> {
        // Every bound at `i` is known once folding has passed it.
        while self.chars_base + self.chars.len() <= i && self.grow() {}
        let k = self.bounds.partition_point(|&(f, _)| f <= i);
        k.checked_sub(1).map(|k| self.bounds[k]).filter(|&(f, _)| f == i).map(|(_, b)| b)
    }

    /// The byte where the first sequence at folded index `i` starts, if one
    /// does; for a stream folded to the end.
    pub(crate) fn start_at(&mut self, i: usize) -> Option<usize> {
        self.finish();
        let k = self.bounds.partition_point(|&(f, _)| f < i);
        self.bounds.get(k).filter(|&&(f, _)| f == i).map(|&(_, b)| b)
    }

    /// Drop what lies before folded index `i`, which the walk has passed.
    pub(crate) fn forget_folded_before(&mut self, i: usize) {
        let drop = i.saturating_sub(self.chars_base).min(self.chars.len());
        if drop < 4 * PIECE {
            return;
        }
        let k = self.bounds.partition_point(|&(f, _)| f < i);
        self.bounds.drain(..k);
        self.bounds_base += k;
        self.chars.drain(..drop);
        self.chars_base += drop;
    }

    /// The folded text and its bounds, for a stream folded to the end from
    /// which nothing was dropped.
    pub(crate) fn folded(&mut self) -> (&[u32], &[(usize, usize)]) {
        self.finish();
        assert!(self.chars_base == 0 && self.bounds_base == 0, "sidestep: part of the folded text was dropped");
        (&self.chars, &self.bounds)
    }

    /// Drop what lies before bound `k`, which the walk has passed.
    pub(crate) fn forget_before(&mut self, k: usize) {
        let drop = k.saturating_sub(self.bounds_base);
        if drop < 4 * PIECE || drop >= self.bounds.len() {
            return;
        }
        let keep = self.bounds[drop].0;
        self.bounds.drain(..drop);
        self.bounds_base = k;
        self.chars.drain(..keep - self.chars_base);
        self.chars_base = keep;
    }
}

/// Text folded cluster by cluster all at once, remembering where each
/// cluster starts in both the folded code points and the source: what
/// `Stream` must agree with, for tests.
#[cfg(test)]
pub(crate) struct Folded {
    pub chars: Vec<u32>,
    /// (index into `chars`, byte offset in the source) at every cluster
    /// start, and at the end.
    pub bounds: Vec<(usize, usize)>,
}

#[cfg(test)]
impl Folded {
    /// Fold `bytes` (WTF-8, whole characters) under `options`, by composed
    /// character sequence or with `per_char` by character.
    pub(crate) fn new(bytes: &[u8], options: usize, per_char: bool) -> Folded {
        let mut chars = Vec::new();
        let mut bounds = Vec::new();
        let mut starts = Vec::new();
        if per_char {
            starts.extend(wtf8::code_points(bytes).map(|(at, _)| at));
        } else {
            cluster_starts(bytes, |s| starts.push(s));
        }
        for (i, &s) in starts.iter().enumerate() {
            bounds.push((chars.len(), s));
            let e = starts.get(i + 1).copied().unwrap_or(bytes.len());
            fold_stretch(&bytes[s..e], options, &mut chars);
        }
        bounds.push((chars.len(), bytes.len()));
        Folded { chars, bounds }
    }

    /// The source byte offset of folded index `i`, if a cluster starts
    /// there. Clusters that fold to nothing share an index with the next;
    /// the first one counts.
    pub(crate) fn source_at(&self, i: usize) -> Option<usize> {
        let k = self.bounds.partition_point(|&(f, _)| f < i);
        self.bounds.get(k).filter(|&&(f, _)| f == i).map(|&(_, b)| b)
    }

    /// The source byte offset where the cluster ending at folded index `i`
    /// ends: the last cluster boundary at `i`.
    pub(crate) fn source_end_at(&self, i: usize) -> Option<usize> {
        let k = self.bounds.partition_point(|&(f, _)| f <= i);
        k.checked_sub(1).map(|k| self.bounds[k]).filter(|&(f, _)| f == i).map(|(_, b)| b)
    }
}

/// Folding, with the folds of recently seen non-ASCII stretches.
///
/// ASCII folds to itself, lowercased when folding case. Every fold is
/// context-free across an ASCII character (it is a canonical starter, and
/// case folding looks at no context), so the rest folds stretch by stretch
/// between ASCII runs. Text repeats its stretches (a word's accented
/// letter, a CJK word) and folding one costs several normalizer passes, so
/// for longer text a small direct-mapped table keyed by a short stretch's
/// bytes remembers the last folds.
pub(crate) struct Memo {
    options: usize,
    /// Stretches to fold before remembering any: the table costs more than
    /// it saves on short text (never, then), or on a walk that stops early.
    wait: u32,
    slots: Option<Box<[Slot; SLOTS]>>,
}

/// (key, fold, fold length); key 0 is an empty slot, since every key holds
/// a nonzero length.
type Slot = (u64, [u32; 6], u8);

/// The memo's size: text repeats few short stretches, and a small table is
/// quick to set up.
const SLOTS: usize = 64;

impl Memo {
    /// A memo for folding about `len` bytes under `options`.
    pub(crate) fn new(options: usize, len: usize) -> Memo {
        Memo { options, wait: if len >= 256 { 2 } else { u32::MAX }, slots: None }
    }

    /// Append the fold of `bytes` (whole characters) to `out`.
    pub(crate) fn fold(&mut self, bytes: &[u8], out: &mut Vec<u32>) {
        let lower = self.options & CASE_INSENSITIVE != 0;
        if let [b] = bytes
            && b.is_ascii()
        {
            out.push(u32::from(if lower { b.to_ascii_lowercase() } else { *b }));
            return;
        }
        let mut at = 0;
        while at < bytes.len() {
            let end = run_end(bytes, at, true);
            if lower {
                out.extend(bytes[at..end].iter().map(|b| u32::from(b.to_ascii_lowercase())));
            } else {
                out.extend(bytes[at..end].iter().map(|&b| u32::from(b)));
            }
            if end == bytes.len() {
                break;
            }
            at = run_end(bytes, end, false);
            self.stretch(&bytes[end..at], out);
        }
    }

    fn stretch(&mut self, s: &[u8], out: &mut Vec<u32>) {
        if self.wait > 0 || s.len() > 7 {
            if self.wait != u32::MAX {
                self.wait = self.wait.saturating_sub(1);
            }
            fold_stretch(s, self.options, out);
            return;
        }
        let key = s.iter().enumerate().fold(s.len() as u64, |k, (i, &b)| k | u64::from(b) << (8 * (i + 1)));
        let slots = self.slots.get_or_insert_with(|| Box::new([(0, [0; 6], 0); SLOTS]));
        let slot = &mut slots[(key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SLOTS.trailing_zeros())) as usize];
        if slot.0 == key {
            out.extend_from_slice(&slot.1[..usize::from(slot.2)]);
            return;
        }
        let start = out.len();
        fold_stretch(s, self.options, out);
        let n = out.len() - start;
        if n <= slot.1.len() {
            slot.0 = key;
            slot.1[..n].copy_from_slice(&out[start..]);
            slot.2 = n as u8;
        }
    }
}

/// Compose text canonically (NFC), for results that should look like the
/// input apart from the folding asked for.
pub(crate) fn compose(s: &str) -> String {
    ComposingNormalizerBorrowed::new_nfc().normalize(s).into_owned()
}

/// Whether `text` is all ASCII, so literal and non-literal comparison
/// agree and case folding is ASCII's.
pub(crate) fn ascii(text: &Text) -> bool {
    text.is_ascii() || text.bytes.is_ascii()
}
