//! Lines and paragraphs, and enumerating a string's pieces.
//!
//! A line ends at LF, CR, CR LF, NEL (U+0085), LINE SEPARATOR (U+2028) or
//! PARAGRAPH SEPARATOR (U+2029); a paragraph only at LF, CR, CR LF or
//! U+2029. Terminators are found in the UTF-8 bytes directly: each one
//! starts with a byte (`\n`, `\r`, `0xC2`, `0xE2`) that can't occur inside
//! another character's sequence.
//!
//! `enumerateSubstringsInRange:options:usingBlock:` finds the pieces first
//! and then calls the block for each with no borrow of the string held, so
//! the block may do anything; when it edits a mutable receiver, the pieces
//! after the edit are found again (see `walk`).

use std::ptr::NonNull;

use block2::DynBlock;
use icu_segmenter::options::{SentenceBreakInvariantOptions, WordBreakInvariantOptions};
use icu_segmenter::{SentenceSegmenter, WordSegmenter};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject};
use objc2::{ClassType, define_class, msg_send};
use objc2_foundation::{NSRange, NSString, NSStringEnumerationOptions, NSUInteger};

use super::index::Text;
use super::search::to_utf16;
use super::view::view;
use super::{fold, wtf8};

/// The terminator starting at byte `at`, as its length, if one does.
#[inline]
fn terminator_at(b: &[u8], at: usize, paragraphs: bool) -> Option<usize> {
    match b[at] {
        b'\n' => Some(1),
        b'\r' => Some(if b.get(at + 1) == Some(&b'\n') { 2 } else { 1 }),
        0xC2 if !paragraphs && b.get(at + 1) == Some(&0x85) => Some(2),
        0xE2 if b.get(at + 1) == Some(&0x80) => match b.get(at + 2) {
            Some(0xA8) if !paragraphs => Some(3),
            Some(0xA9) => Some(3),
            _ => None,
        },
        _ => None,
    }
}

/// The start of the line (or paragraph) holding byte `at`.
fn start_of(b: &[u8], at: usize, paragraphs: bool) -> usize {
    // The LF of a CR LF belongs to the line the CR ends.
    let mut i = if at < b.len() && at > 0 && b[at] == b'\n' && b[at - 1] == b'\r' { at - 1 } else { at };
    while i > 0 {
        let c = b[i - 1];
        let hit = match c {
            b'\n' | b'\r' => true,
            0x85 => !paragraphs && i >= 2 && b[i - 2] == 0xC2,
            0xA8 => !paragraphs && i >= 3 && b[i - 3] == 0xE2 && b[i - 2] == 0x80,
            0xA9 => i >= 3 && b[i - 3] == 0xE2 && b[i - 2] == 0x80,
            _ => false,
        };
        if hit {
            return i;
        }
        i -= 1;
    }
    0
}

/// The contents end and the end (past the terminator) of the line holding
/// byte `at`.
fn end_of(b: &[u8], at: usize, paragraphs: bool) -> (usize, usize) {
    let mut i = if at < b.len() && at > 0 && b[at] == b'\n' && b[at - 1] == b'\r' { at - 1 } else { at };
    while i < b.len() {
        if let Some(len) = terminator_at(b, i, paragraphs) {
            return (i, i + len);
        }
        i += 1;
    }
    (b.len(), b.len())
}

/// The bytes of the lines (or paragraphs) covering bytes `from..to`: start,
/// contents end, end.
fn covering(b: &[u8], from: usize, to: usize, paragraphs: bool) -> (usize, usize, usize) {
    let start = start_of(b, from, paragraphs);
    // The last character in the range decides the end; an empty range uses
    // its location.
    let last = if to > from { wtf8::prev_boundary(b, to) } else { from };
    let (contents, end) = end_of(b, last, paragraphs);
    (start, contents, end)
}

/// The paragraph around bytes `from..=to`, start and end.
pub(crate) fn paragraph_bytes(b: &[u8], from: usize, to: usize) -> (usize, usize) {
    let (s, _, e) = covering(b, from, to, true);
    (s, e)
}

/// `get{Line,Paragraph}Start:end:contentsEnd:forRange:` in UTF-16 units.
fn get_range(obj: &AnyObject, range: NSRange, paragraphs: bool, method: &str) -> (usize, usize, usize) {
    let v = view(obj);
    let t = v.text();
    super::check_range(method, range, t.utf16_len);
    let (from, to) = super::search::window(&t, range.location, range.length);
    let (s, c, e) = covering(t.bytes, from, to, paragraphs);
    (t.utf16_at(s), t.utf16_at(c), t.utf16_at(e))
}

fn write(ptr: *mut NSUInteger, value: usize) {
    if !ptr.is_null() {
        // SAFETY: the caller passes a valid pointer or null.
        unsafe { *ptr = value };
    }
}

/// A piece found by enumeration: its range and the range with what
/// belongs to it (a line's terminator, the space after a word).
#[derive(Clone, Copy)]
struct Piece {
    range: (usize, usize),
    enclosing: (usize, usize),
}

/// The pieces of `t` in the UTF-16 range, by `unit`, clipped to the range.
fn pieces(t: &Text, loc: usize, len: usize, unit: usize) -> Vec<Piece> {
    let (from, to) = super::search::window(t, loc, len);
    let b = t.bytes;
    let mut out = Vec::new();
    let clip = |s: usize, e: usize| (s.clamp(from, to), e.clamp(from, to));
    let mut push = |range: (usize, usize), enclosing: (usize, usize)| {
        let (rs, re) = clip(range.0, range.1);
        let (es, ee) = clip(enclosing.0, enclosing.1);
        let a = to_utf16(t, rs, re);
        let e = to_utf16(t, es, ee);
        out.push(Piece { range: a, enclosing: e });
    };
    match unit {
        0 | 1 => {
            let paragraphs = unit == 1;
            let mut at = from;
            while at < to {
                let start = start_of(b, at, paragraphs);
                let (contents, end) = end_of(b, at, paragraphs);
                push((start, contents), (start, end));
                at = end;
            }
        }
        3 | 4 => {
            // Segment the whole paragraphs around the range, for context.
            let (ps, pe) = paragraph_bytes(b, from, to);
            let text = wtf8::to_str_lossy(&b[ps..pe], t.flags);
            if unit == 3 {
                let words: Vec<(usize, usize)> = {
                    let seg = WordSegmenter::new_for_non_complex_scripts(WordBreakInvariantOptions::default());
                    let mut it = seg.segment_str(&text);
                    let mut prev = it.next().unwrap_or(0);
                    let mut spans = Vec::new();
                    while let Some(at) = it.next() {
                        if it.is_word_like() {
                            spans.push((ps + prev, ps + at));
                        }
                        prev = at;
                    }
                    spans
                };
                let inside: Vec<(usize, usize)> = words.into_iter().filter(|&(s, e)| e > from && s < to).collect();
                for (k, &(s, e)) in inside.iter().enumerate() {
                    let enclosing_start = if k == 0 { from.min(s) } else { s };
                    let enclosing_end = inside.get(k + 1).map_or(to.max(e), |next| next.0);
                    push((s, e), (enclosing_start, enclosing_end));
                }
            } else {
                let seg = SentenceSegmenter::new(SentenceBreakInvariantOptions::default());
                let bounds: Vec<usize> = seg.segment_str(&text).map(|at| ps + at).collect();
                for w in bounds.windows(2) {
                    if w[1] > from && w[0] < to {
                        push((w[0], w[1]), (w[0], w[1]));
                    }
                }
            }
        }
        _ => {
            // Composed character sequences; caret positions and deletion
            // clusters are the same pieces here.
            let (ps, pe) = paragraph_bytes(b, from, to.saturating_sub(1).max(from));
            let bounds = fold::clusters(&b[ps..pe.max(to)]);
            for w in bounds.windows(2) {
                let (s, e) = (ps + w[0], ps + w[1]);
                if e > from && s < to {
                    push((s, e), (s, e));
                }
            }
        }
    }
    out
}

/// Enumerate the pieces of `range` by `unit`, calling `f(substring, range,
/// enclosing range)` until it returns true (stop).
///
/// The pieces are found first and `f` runs with no borrow of the string
/// held. A mutable receiver may be edited inside the enclosing range `f` is
/// given, as Foundation allows: when its length changes, enumeration goes
/// on after that range (or, in reverse, before it), and the end of `range`
/// moves by as much as the length did.
fn walk(
    obj: &AnyObject,
    range: NSRange,
    unit: usize,
    reverse: bool,
    wants_substring: bool,
    mut f: impl FnMut(Option<&NSString>, NSRange, NSRange) -> bool,
) {
    let (mut len, mut found) = {
        let v = view(obj);
        let t = v.text();
        super::check_range("enumerateSubstringsInRange:options:usingBlock:", range, t.utf16_len);
        (t.utf16_len, pieces(&t, range.location, range.length, unit))
    };
    let fixed = super::view::native(obj).is_some_and(|v| v.is_immutable());
    let mut end = range.end();
    let mut k = 0;
    while k < found.len() {
        let p = if reverse { found[found.len() - 1 - k] } else { found[k] };
        k += 1;
        if p.enclosing.0 + p.enclosing.1 > len {
            // The block edited more than it may; stop rather than read past
            // the end.
            return;
        }
        let r = NSRange::new(p.range.0, p.range.1);
        let substring: Option<Retained<NSString>> = wants_substring.then(|| {
            // SAFETY: a string answers -substringWithRange:.
            unsafe { msg_send![obj, substringWithRange: r] }
        });
        if f(substring.as_deref(), r, NSRange::new(p.enclosing.0, p.enclosing.1)) {
            return;
        }
        if fixed {
            continue;
        }
        // SAFETY: every string answers -length.
        let now: usize = unsafe { msg_send![obj, length] };
        if now == len {
            continue;
        }
        let delta = now as isize - len as isize;
        len = now;
        end = end.saturating_add_signed(delta).clamp(range.location, len);
        if reverse {
            // What is left lies before the edit and hasn't moved.
            continue;
        }
        let from = (p.enclosing.0 + p.enclosing.1).saturating_add_signed(delta).clamp(range.location, end);
        let v = view(obj);
        found = pieces(&v.text(), from, end - from, unit);
        k = 0;
    }
}

fn this(obj: &Helper) -> &AnyObject {
    obj
}

define_class!(
    // NSString's line, paragraph and enumeration methods, copied onto
    // NSString when it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringLines"]
    pub(crate) struct Helper;

    impl Helper {
        #[unsafe(method(lineRangeForRange:))]
        fn line_range(&self, range: NSRange) -> NSRange {
            let (s, _, e) = get_range(this(self), range, false, "lineRangeForRange:");
            NSRange::new(s, e - s)
        }

        #[unsafe(method(paragraphRangeForRange:))]
        fn paragraph_range(&self, range: NSRange) -> NSRange {
            let (s, _, e) = get_range(this(self), range, true, "paragraphRangeForRange:");
            NSRange::new(s, e - s)
        }

        #[unsafe(method(getLineStart:end:contentsEnd:forRange:))]
        fn get_line_start(&self, start: *mut NSUInteger, end: *mut NSUInteger, contents_end: *mut NSUInteger, range: NSRange) {
            let (s, c, e) = get_range(this(self), range, false, "getLineStart:end:contentsEnd:forRange:");
            write(start, s);
            write(end, e);
            write(contents_end, c);
        }

        #[unsafe(method(getParagraphStart:end:contentsEnd:forRange:))]
        fn get_paragraph_start(
            &self,
            start: *mut NSUInteger,
            end: *mut NSUInteger,
            contents_end: *mut NSUInteger,
            range: NSRange,
        ) {
            let (s, c, e) = get_range(this(self), range, true, "getParagraphStart:end:contentsEnd:forRange:");
            write(start, s);
            write(end, e);
            write(contents_end, c);
        }

        #[unsafe(method(enumerateSubstringsInRange:options:usingBlock:))]
        fn enumerate_substrings(
            &self,
            range: NSRange,
            options: NSStringEnumerationOptions,
            block: &DynBlock<dyn Fn(*mut NSString, NSRange, NSRange, NonNull<Bool>)>,
        ) {
            let wants_substring = !options.contains(NSStringEnumerationOptions::SubstringNotRequired);
            let reverse = options.contains(NSStringEnumerationOptions::Reverse);
            walk(this(self), range, options.0 & 0xFF, reverse, wants_substring, |substring, r, enclosing| {
                let ptr = substring.map_or(std::ptr::null_mut(), |s| (s as *const NSString).cast_mut());
                let mut stop = Bool::NO;
                block.call((ptr, r, enclosing, NonNull::from(&mut stop)));
                stop.as_bool()
            });
        }

        #[unsafe(method(enumerateLinesUsingBlock:))]
        fn enumerate_lines(&self, block: &DynBlock<dyn Fn(NonNull<NSString>, NonNull<Bool>)>) {
            let obj = this(self);
            // SAFETY: every string answers -length.
            let len: usize = unsafe { msg_send![obj, length] };
            walk(obj, NSRange::new(0, len), 0, false, true, |line, _, _| {
                let mut stop = Bool::NO;
                block.call((NonNull::from(line.expect("a line")), NonNull::from(&mut stop)));
                stop.as_bool()
            });
        }
    }
);

/// Add the line methods to NSString.
pub(crate) fn install(target: &AnyClass) {
    super::install::copy_methods(Helper::class(), target, false);
}
