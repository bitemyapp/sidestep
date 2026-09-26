//! Where selection and editing commands go in text: characters (grapheme
//! clusters), words, paragraphs, all in UTF-16 indexes of a text storage.
//!
//! Boundaries are found in a window of text around the index, read from
//! the storage's paragraph tree when it is Sidestep's (one paragraph's
//! slice, no copy of the rest) and through `NSString` otherwise, so a
//! command in a long document costs what the text near the caret costs.
//! Characters are ICU4X grapheme clusters; words are ICU4X's word-like
//! segments (UAX #29), as AppKit's word moves go: to the end of the word
//! the caret is in or the next one, or to the start of the word it's in or
//! the one before, across paragraphs. Deleting a character goes by the
//! clusters of Unicode before 15.1, as AppKit's does: a conjunct of an
//! Indic script (consonants joined by a virama, one character to move
//! over) is deleted a consonant at a time.

use std::ops::Range;

use icu_segmenter::options::WordBreakInvariantOptions;
use icu_segmenter::{GraphemeClusterSegmenter, WordSegmenter};
use objc2::rc::Retained;
use objc2_app_kit::NSTextStorage;
use objc2_foundation::{NSRange, NSString};

/// Units looked at around an index for a character boundary.
const CHAR_WINDOW: usize = 64;

/// A stretch of text: where it starts in the storage, and its text.
pub(crate) struct Window {
    pub start: usize,
    pub text: String,
}

impl Window {
    /// The UTF-16 offset of byte `b` of the window's text.
    pub fn unit_of(&self, b: usize) -> usize {
        self.start + self.text[..b].encode_utf16().count()
    }

    fn end(&self) -> usize {
        self.start + self.text.encode_utf16().count()
    }
}

pub(crate) fn len(ts: &NSTextStorage) -> usize {
    match super::text_storage::native(ts) {
        Some(iv) => iv.text().len(),
        None => ts.length(),
    }
}

/// The text of `range`.
pub(crate) fn text(ts: &NSTextStorage, range: Range<usize>) -> String {
    let len = len(ts);
    let range = range.start.min(len)..range.end.min(len);
    match super::text_storage::native(ts) {
        Some(iv) => iv.text().text(range),
        None => {
            let s: Retained<NSString> = ts.string();
            s.substringWithRange(NSRange::new(range.start, range.end - range.start)).to_string()
        }
    }
}

/// The paragraph holding `index`, separator included.
pub(crate) fn paragraph(ts: &NSTextStorage, index: usize) -> Range<usize> {
    match super::text_storage::native(ts) {
        Some(iv) => iv.text().paragraph_range(index),
        None => {
            let s: Retained<NSString> = ts.string();
            let r = s.paragraphRangeForRange(NSRange::new(index.min(s.length()), 0));
            r.location..r.location + r.length
        }
    }
}

/// The paragraph's text without its separator's units: where its content
/// ends.
pub(crate) fn content_end(ts: &NSTextStorage, para: Range<usize>) -> usize {
    let tail = text(ts, para.end.saturating_sub(2).max(para.start)..para.end);
    let sep = if tail.ends_with("\r\n") {
        2
    } else if tail.ends_with(['\n', '\r', '\u{2029}']) {
        1
    } else {
        0
    };
    para.end - sep
}

fn window(ts: &NSTextStorage, range: Range<usize>) -> Window {
    Window { start: range.start, text: text(ts, range) }
}

/// The grapheme boundaries of a window's text, as storage indexes.
fn char_boundaries(w: &Window) -> Vec<usize> {
    let seg = GraphemeClusterSegmenter::new();
    seg.segment_str(&w.text).map(|b| w.unit_of(b)).collect()
}

/// The start of the character after the one at `index`.
pub(crate) fn next_char(ts: &NSTextStorage, index: usize) -> usize {
    let len = len(ts);
    if index >= len {
        return len;
    }
    let w = window(ts, index..(index + CHAR_WINDOW).min(len));
    char_boundaries(&w).into_iter().find(|&b| b > index).unwrap_or(len)
}

/// The start of the character before `index`.
pub(crate) fn prev_char(ts: &NSTextStorage, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    let from = index.saturating_sub(CHAR_WINDOW);
    let w = window(ts, from..index);
    char_boundaries(&w).into_iter().rev().find(|&b| b < index).unwrap_or(from)
}

/// Where deleting forward from `index` goes: the end of the character
/// there, as deletion counts characters.
pub(crate) fn next_deletable(ts: &NSTextStorage, index: usize) -> usize {
    let len = len(ts);
    if index >= len {
        return len;
    }
    let w = window(ts, index..(index + CHAR_WINDOW).min(len));
    deletion_boundaries(&w).into_iter().find(|&b| b > index).unwrap_or(len)
}

/// Where deleting back from `index` goes.
pub(crate) fn prev_deletable(ts: &NSTextStorage, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    let from = index.saturating_sub(CHAR_WINDOW);
    let w = window(ts, from..index);
    deletion_boundaries(&w).into_iter().rev().find(|&b| b < index).unwrap_or(from)
}

/// The viramas that join consonants into one grapheme since Unicode 15.1
/// (`Indic_Conjunct_Break=Linker`).
fn is_linker(c: char) -> bool {
    matches!(c, '\u{094D}' | '\u{09CD}' | '\u{0ACD}' | '\u{0B4D}' | '\u{0C4D}' | '\u{0D4D}')
}

/// The consonants they join (`Indic_Conjunct_Break=Consonant`).
fn is_conjunct_consonant(c: char) -> bool {
    matches!(c as u32,
        0x0915..=0x0939 | 0x0958..=0x095F | 0x0978..=0x097F
        | 0x0995..=0x09A8 | 0x09AA..=0x09B0 | 0x09B2 | 0x09B6..=0x09B9 | 0x09DC..=0x09DD | 0x09DF | 0x09F0..=0x09F1
        | 0x0A95..=0x0AA8 | 0x0AAA..=0x0AB0 | 0x0AB2..=0x0AB3 | 0x0AB5..=0x0AB9 | 0x0AF9
        | 0x0B15..=0x0B28 | 0x0B2A..=0x0B30 | 0x0B32..=0x0B33 | 0x0B35..=0x0B39 | 0x0B5C..=0x0B5D | 0x0B5F | 0x0B71
        | 0x0C15..=0x0C28 | 0x0C2A..=0x0C39 | 0x0C58..=0x0C5A
        | 0x0D15..=0x0D3A)
}

/// The grapheme boundaries of a window's text, and a boundary before each
/// consonant a virama (and perhaps a zero-width joiner) joins on: the
/// characters deletion counts.
fn deletion_boundaries(w: &Window) -> Vec<usize> {
    let mut out = char_boundaries(w);
    let mut before: [Option<char>; 2] = [None, None];
    let mut joined = false;
    for (b, c) in w.text.char_indices() {
        let linked = match before {
            [_, Some(p)] if is_linker(p) => true,
            [Some(p), Some('\u{200D}')] if is_linker(p) => true,
            _ => false,
        };
        if linked && is_conjunct_consonant(c) {
            out.push(w.unit_of(b));
            joined = true;
        }
        before = [before[1], Some(c)];
    }
    if joined {
        out.sort_unstable();
        out.dedup();
    }
    out
}

/// The word segments of a paragraph's text: ranges, and whether each is a
/// word (rather than spaces or punctuation).
fn words(w: &Window) -> Vec<(Range<usize>, bool)> {
    let seg = WordSegmenter::new_for_non_complex_scripts(WordBreakInvariantOptions::default());
    let mut it = seg.segment_str(&w.text);
    let mut out = Vec::new();
    let mut prev = it.next().unwrap_or(0);
    let mut unit_prev = w.unit_of(prev);
    while let Some(at) = it.next() {
        let unit = unit_prev + w.text[prev..at].encode_utf16().count();
        out.push((unit_prev..unit, it.is_word_like()));
        (prev, unit_prev) = (at, unit);
    }
    out
}

fn paragraph_window(ts: &NSTextStorage, index: usize) -> Window {
    window(ts, paragraph(ts, index))
}

/// The segment (word, or run of spaces or punctuation) holding `index`:
/// what double-clicking selects. At a paragraph's end, the segment before.
pub(crate) fn word_at(ts: &NSTextStorage, index: usize) -> Range<usize> {
    let len = len(ts);
    let w = paragraph_window(ts, index.min(len));
    let segs = words(&w);
    let hit = segs.iter().find(|(r, _)| r.start <= index && index < r.end).or(segs.last());
    match hit {
        Some((r, _)) => {
            // A paragraph separator on its own: just it.
            r.clone()
        }
        None => index..index,
    }
}

/// Where moving a word forward from `index` goes: the end of the word
/// holding it or the next one.
pub(crate) fn word_end_after(ts: &NSTextStorage, index: usize) -> usize {
    let len = len(ts);
    let mut at = index;
    while at < len {
        let w = paragraph_window(ts, at);
        if let Some((r, _)) = words(&w).into_iter().find(|(r, word)| *word && r.end > index) {
            return r.end;
        }
        at = w.end().max(at + 1);
    }
    len
}

/// Where moving a word back from `index` goes: the start of the word
/// holding it or the one before.
pub(crate) fn word_start_before(ts: &NSTextStorage, index: usize) -> usize {
    let mut at = index;
    loop {
        let w = paragraph_window(ts, at.saturating_sub(1).min(at));
        if let Some((r, _)) = words(&w).into_iter().rev().find(|(r, word)| *word && r.start < index) {
            return r.start;
        }
        if w.start == 0 {
            return 0;
        }
        at = w.start;
    }
}
