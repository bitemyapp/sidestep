//! The text of a text storage: paragraphs, each its UTF-8 text and its
//! attribute runs over UTF-16 units, kept in chunks.
//!
//! `NSTextStorage` indexes text by UTF-16 unit, as `NSString` does, while
//! the text engine lays out UTF-8 a paragraph at a time. So the text is
//! kept as the engine wants it, cut into paragraphs, and each paragraph
//! knows its length in UTF-16 units:
//!
//! - A paragraph ([`Para`]) is its text with the separator that ends it
//!   (`\n`, `\r`, `\r\n`, which is never split, or U+2029), its UTF-16
//!   length, and its attribute runs, which cover it, separator included.
//!   Every text ends in a paragraph without a separator, empty when the
//!   text is empty or ends in a separator: the paragraph an insertion point
//!   after the last separator is in.
//! - Paragraphs sit in chunks of at most [`MAX_CHUNK`], each chunk with the
//!   sum of its paragraphs' lengths, and the storage keeps the sums of the
//!   chunks before each one. Finding the paragraph at an index halves over
//!   the chunks and then walks one chunk, so a 200 000-paragraph document
//!   finds any index in a few hundred steps, and an edit inside one
//!   paragraph touches that paragraph, its chunk and the sums after it.
//! - A UTF-16 index inside a paragraph is found by walking its text, except
//!   in ASCII paragraphs, where units are bytes, and in long paragraphs
//!   ([`LONG`] bytes or more), which keep a sparse index of checkpoints
//!   made when first needed.
//!
//! Attributes are interned per storage (`attrs`), so runs hold a small
//! number ([`AttrId`]) and runs with equal attributes merge.
//!
//! `NSString` can hold lone surrogates; UTF-8 can't. An edit that splits a
//! surrogate pair leaves U+FFFD for the half that remains, and text with a
//! lone surrogate stores U+FFFD for it: one UTF-16 unit either way, so
//! every length and index is as it would be.

use std::cell::OnceCell;
use std::ops::Range;

use smallvec::SmallVec;

/// An interned attribute dictionary (`attrs::AttrTable`).
pub(crate) type AttrId = u32;

/// A run of attributes over `len` UTF-16 units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub len: u32,
    pub attrs: AttrId,
}

/// The most paragraphs a chunk holds, and the fewest it keeps before it
/// joins a neighbour.
const MAX_CHUNK: usize = 128;
const MIN_CHUNK: usize = 16;

/// Paragraphs this long (in bytes) keep an index of UTF-16 checkpoints.
const LONG: usize = 4096;

/// Bytes between checkpoints.
const CHECKPOINT: usize = 256;

/// A paragraph: its text, separator included, and its runs.
#[derive(Clone, Debug)]
pub(crate) struct Para {
    text: String,
    len16: u32,
    /// The separator's length in UTF-16 units and in bytes.
    sep16: u8,
    sep8: u8,
    /// The text is ASCII, so units are bytes.
    ascii: bool,
    runs: SmallVec<[Run; 2]>,
    /// (UTF-16 offset, byte) every [`CHECKPOINT`] bytes or so, for long
    /// paragraphs that aren't ASCII.
    index: OnceCell<Box<[(u32, u32)]>>,
}

impl Para {
    fn new(text: String, runs: SmallVec<[Run; 2]>) -> Para {
        let len16 = utf16_len(&text);
        let (sep8, sep16) = separator_at_end(&text);
        let ascii = text.is_ascii();
        debug_assert_eq!(runs.iter().map(|r| r.len).sum::<u32>(), len16, "runs cover the paragraph");
        Para { text, len16, sep16, sep8, ascii, runs, index: OnceCell::new() }
    }

    /// The text, separator included.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text without its separator.
    pub fn content(&self) -> &str {
        &self.text[..self.text.len() - usize::from(self.sep8)]
    }

    pub fn len16(&self) -> u32 {
        self.len16
    }

    /// UTF-16 units of separator ending the paragraph: 0, 1, or 2 for
    /// `\r\n`.
    pub fn sep16(&self) -> u8 {
        self.sep16
    }

    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// The byte where UTF-16 offset `u` falls, and whether it falls inside
    /// a character (between the halves of a surrogate pair), in which case
    /// the byte is that character's start.
    pub fn byte_of(&self, u: u32) -> (usize, bool) {
        if self.ascii || u == 0 {
            return (u as usize, false);
        }
        if u >= self.len16 {
            return (self.text.len(), false);
        }
        let (mut unit, mut byte) = if self.text.len() >= LONG {
            let index = self.index.get_or_init(|| checkpoints(&self.text));
            let at = index.partition_point(|&(cu, _)| cu <= u).saturating_sub(1);
            index.get(at).map_or((0, 0), |&(cu, cb)| (cu, cb as usize))
        } else {
            (0, 0)
        };
        for c in self.text[byte..].chars() {
            let n = c.len_utf16() as u32;
            if unit + n > u {
                return (byte, unit != u);
            }
            unit += n;
            byte += c.len_utf8();
        }
        (self.text.len(), false)
    }

    /// The UTF-16 offset of byte `b`, a character boundary.
    pub fn unit_of(&self, b: usize) -> u32 {
        if self.ascii {
            return b as u32;
        }
        let (unit, byte) = if self.text.len() >= LONG {
            let index = self.index.get_or_init(|| checkpoints(&self.text));
            let at = index.partition_point(|&(_, cb)| cb as usize <= b).saturating_sub(1);
            index.get(at).map_or((0, 0), |&(cu, cb)| (cu, cb as usize))
        } else {
            (0, 0)
        };
        unit + utf16_len(&self.text[byte..b])
    }

    /// The UTF-16 unit at offset `u`.
    fn unit_at(&self, u: u32) -> u16 {
        if self.ascii {
            return u16::from(self.text.as_bytes()[u as usize]);
        }
        let (byte, inside) = self.byte_of(u);
        let c = self.text[byte..].chars().next().expect("a character at an offset inside the paragraph");
        let mut units = [0u16; 2];
        let enc = c.encode_utf16(&mut units);
        enc[usize::from(inside)]
    }

    /// The attribute at offset `u` (< `len16`), and the run holding it,
    /// from the paragraph's start.
    fn run_at(&self, u: u32) -> (AttrId, Range<u32>) {
        let mut start = 0;
        for run in &self.runs {
            if u < start + run.len {
                return (run.attrs, start..start + run.len);
            }
            start += run.len;
        }
        unreachable!("sidestep: offset {u} past a paragraph of {}", self.len16)
    }

    /// Whether the whole paragraph is one run of `attrs` (or empty).
    fn only(&self, attrs: AttrId) -> bool {
        self.runs.iter().all(|r| r.attrs == attrs)
    }
}

/// UTF-16 offsets and bytes every [`CHECKPOINT`] bytes, at character
/// boundaries.
fn checkpoints(text: &str) -> Box<[(u32, u32)]> {
    let mut out = Vec::with_capacity(text.len() / CHECKPOINT + 1);
    let (mut unit, mut next) = (0u32, 0usize);
    for (b, c) in text.char_indices() {
        if b >= next {
            out.push((unit, b as u32));
            next = b + CHECKPOINT;
        }
        unit += c.len_utf16() as u32;
    }
    out.into_boxed_slice()
}

pub(crate) fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        return s.len() as u32;
    }
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

/// The separator ending `text`, if any: its length in bytes and in UTF-16
/// units.
fn separator_at_end(text: &str) -> (u8, u8) {
    let b = text.as_bytes();
    match b.last() {
        Some(b'\n') if b.len() >= 2 && b[b.len() - 2] == b'\r' => (2, 2),
        Some(b'\n' | b'\r') => (1, 1),
        _ if text.ends_with('\u{2029}') => (3, 1),
        _ => (0, 0),
    }
}

/// Where the first paragraph of `text` ends: after its separator, or `None`
/// if it has none.
fn paragraph_end(text: &str) -> Option<usize> {
    let at = text.find(['\n', '\r', '\u{2029}'])?;
    Some(match text.as_bytes()[at] {
        b'\r' if text.as_bytes().get(at + 1) == Some(&b'\n') => at + 2,
        b'\n' | b'\r' => at + 1,
        _ => at + 3,
    })
}

/// Whether `text` holds a paragraph separator.
pub(crate) fn has_separator(text: &str) -> bool {
    text.contains(['\n', '\r', '\u{2029}'])
}

/// Replace `range` of `runs` (UTF-16, from their start) with `len` units of
/// `attrs`, merging equal neighbours.
fn splice_runs(runs: &mut SmallVec<[Run; 2]>, range: Range<u32>, len: u32, attrs: AttrId) {
    let mut out: SmallVec<[Run; 2]> = SmallVec::with_capacity(runs.len() + 2);
    let push = |out: &mut SmallVec<[Run; 2]>, run: Run| {
        if run.len == 0 {
            return;
        }
        match out.last_mut() {
            Some(last) if last.attrs == run.attrs => last.len += run.len,
            _ => out.push(run),
        }
    };
    let mut start = 0;
    let mut placed = false;
    for &run in runs.iter() {
        let end = start + run.len;
        // The part before the range.
        if start < range.start {
            push(&mut out, Run { len: end.min(range.start) - start, attrs: run.attrs });
        }
        if !placed && end >= range.start {
            push(&mut out, Run { len, attrs });
            placed = true;
        }
        // The part after the range.
        if end > range.end {
            push(&mut out, Run { len: end - start.max(range.end), attrs: run.attrs });
        }
        start = end;
    }
    if !placed {
        push(&mut out, Run { len, attrs });
    }
    *runs = out;
}

/// Sums over paragraphs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Sum {
    u16: usize,
    u8: usize,
    paras: usize,
}

impl Sum {
    fn of(p: &Para) -> Sum {
        Sum { u16: p.len16 as usize, u8: p.text.len(), paras: 1 }
    }

    fn add(self, o: Sum) -> Sum {
        Sum { u16: self.u16 + o.u16, u8: self.u8 + o.u8, paras: self.paras + o.paras }
    }
}

#[derive(Clone, Debug)]
struct Chunk {
    paras: Vec<Para>,
    sum: Sum,
    /// The attributes every paragraph of the chunk has throughout, if they
    /// have one: lets an effective range cross the chunk in one step.
    uniform: Option<AttrId>,
}

impl Chunk {
    fn new(paras: Vec<Para>) -> Chunk {
        let mut c = Chunk { paras, sum: Sum::default(), uniform: None };
        c.resum();
        c
    }

    fn resum(&mut self) {
        self.sum = self.paras.iter().fold(Sum::default(), |s, p| s.add(Sum::of(p)));
        let first = self.paras.iter().find_map(|p| p.runs.first()).map(|r| r.attrs);
        self.uniform = first.filter(|&a| self.paras.iter().all(|p| p.only(a)));
    }
}

/// Where a paragraph is: its chunk, its place there, its number in the
/// text, and its first UTF-16 unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct At {
    chunk: usize,
    slot: usize,
    pub para: usize,
    pub start: usize,
}

/// A text's paragraphs.
#[derive(Clone, Debug)]
pub(crate) struct Storage {
    chunks: Vec<Chunk>,
    /// The sums of the chunks before each chunk.
    starts: Vec<Sum>,
    /// Bumped by every edit.
    generation: u64,
}

impl Default for Storage {
    fn default() -> Self {
        Storage::new()
    }
}

impl Storage {
    /// An empty text: one empty paragraph.
    pub fn new() -> Storage {
        let chunks = vec![Chunk::new(vec![Para::new(String::new(), SmallVec::new())])];
        Storage { starts: vec![Sum::default()], chunks, generation: 0 }
    }

    /// A text of `text`, all with `attrs`.
    pub fn with_text(text: &str, attrs: AttrId) -> Storage {
        let mut s = Storage::new();
        s.replace(0..0, text, attrs);
        s
    }

    /// The length in UTF-16 units.
    pub fn len(&self) -> usize {
        self.total().u16
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The length in UTF-8 bytes.
    pub fn len8(&self) -> usize {
        self.total().u8
    }

    pub fn paragraph_count(&self) -> usize {
        self.total().paras
    }

    /// Changes with every edit.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn total(&self) -> Sum {
        let last = self.chunks.len() - 1;
        self.starts[last].add(self.chunks[last].sum)
    }

    /// Work out the sums before each chunk from chunk `from` on (those
    /// before it hold).
    fn restart(&mut self, from: usize) {
        let from = from.min(self.chunks.len());
        self.starts.truncate(from);
        let mut sum = match from.checked_sub(1) {
            Some(prev) => self.starts[prev].add(self.chunks[prev].sum),
            None => Sum::default(),
        };
        for c in &self.chunks[from..] {
            self.starts.push(sum);
            sum = sum.add(c.sum);
        }
    }

    /// The paragraph holding UTF-16 unit `u` (`u` ≤ the length; the text's
    /// end is in its last paragraph).
    pub fn locate(&self, u: usize) -> At {
        let c = self.starts.partition_point(|s| s.u16 <= u).saturating_sub(1);
        // A chunk ending exactly at `u` hands `u` to the next one, except
        // at the text's end.
        let mut start = self.starts[c].u16;
        let paras = &self.chunks[c].paras;
        for (slot, p) in paras.iter().enumerate() {
            if u < start + p.len16 as usize || slot + 1 == paras.len() {
                return At { chunk: c, slot, para: self.starts[c].paras + slot, start };
            }
            start += p.len16 as usize;
        }
        unreachable!("sidestep: a chunk without paragraphs")
    }

    /// Where paragraph `n` is.
    pub fn locate_paragraph(&self, n: usize) -> At {
        let n = n.min(self.paragraph_count() - 1);
        let c = self.starts.partition_point(|s| s.paras <= n).saturating_sub(1);
        let slot = n - self.starts[c].paras;
        let start = self.starts[c].u16 + self.chunks[c].paras[..slot].iter().map(|p| p.len16 as usize).sum::<usize>();
        At { chunk: c, slot, para: n, start }
    }

    pub fn para(&self, at: At) -> &Para {
        &self.chunks[at.chunk].paras[at.slot]
    }

    /// The paragraph after `at`, if there is one.
    pub fn next(&self, at: At) -> Option<At> {
        let p = self.para(at);
        let start = at.start + p.len16 as usize;
        if at.slot + 1 < self.chunks[at.chunk].paras.len() {
            Some(At { slot: at.slot + 1, para: at.para + 1, start, ..at })
        } else if at.chunk + 1 < self.chunks.len() {
            Some(At { chunk: at.chunk + 1, slot: 0, para: at.para + 1, start })
        } else {
            None
        }
    }

    /// The paragraph before `at`, if there is one.
    pub fn prev(&self, at: At) -> Option<At> {
        let (chunk, slot) = if at.slot > 0 {
            (at.chunk, at.slot - 1)
        } else if at.chunk > 0 {
            (at.chunk - 1, self.chunks[at.chunk - 1].paras.len() - 1)
        } else {
            return None;
        };
        let start = at.start - self.chunks[chunk].paras[slot].len16 as usize;
        Some(At { chunk, slot, para: at.para - 1, start })
    }

    /// The paragraphs from the one holding `u` on.
    pub fn paragraphs_from(&self, u: usize) -> impl Iterator<Item = (At, &Para)> {
        let first = self.locate(u);
        std::iter::successors(Some(first), move |&at| self.next(at)).map(move |at| (at, self.para(at)))
    }

    /// The UTF-16 range of the paragraph holding `u`, separator included.
    pub fn paragraph_range(&self, u: usize) -> Range<usize> {
        let at = self.locate(u);
        at.start..at.start + self.para(at).len16 as usize
    }

    /// The UTF-16 unit at `u` (< the length).
    pub fn unit_at(&self, u: usize) -> u16 {
        let at = self.locate(u);
        self.para(at).unit_at((u - at.start) as u32)
    }

    /// The UTF-16 units of `range`.
    pub fn units(&self, range: Range<usize>, out: &mut Vec<u16>) {
        if range.is_empty() {
            return;
        }
        for (at, p) in self.paragraphs_from(range.start) {
            if at.start >= range.end {
                break;
            }
            let from = range.start.saturating_sub(at.start) as u32;
            let to = ((range.end - at.start) as u32).min(p.len16);
            if p.ascii {
                out.extend(p.text.as_bytes()[from as usize..to as usize].iter().map(|&b| u16::from(b)));
                continue;
            }
            let (b0, inside) = p.byte_of(from);
            let mut unit = from - u32::from(inside);
            let mut buf = [0u16; 2];
            for c in p.text[b0..].chars() {
                if unit >= to {
                    break;
                }
                for &w in c.encode_utf16(&mut buf).iter() {
                    if unit >= from && unit < to {
                        out.push(w);
                    }
                    unit += 1;
                }
            }
        }
    }

    /// The text of `range`, as UTF-8; the half of a surrogate pair the
    /// range cuts becomes U+FFFD.
    pub fn text(&self, range: Range<usize>) -> String {
        let mut out = String::new();
        self.for_each_slice(range, |s| out.push_str(s));
        out
    }

    /// Call `f` with the text of `range`, a paragraph's piece at a time
    /// (with U+FFFD for a cut surrogate pair's half).
    pub fn for_each_slice(&self, range: Range<usize>, mut f: impl FnMut(&str)) {
        if range.is_empty() {
            return;
        }
        for (at, p) in self.paragraphs_from(range.start) {
            if at.start >= range.end {
                break;
            }
            let from = range.start.saturating_sub(at.start) as u32;
            let to = ((range.end - at.start) as u32).min(p.len16);
            let (mut b0, cut0) = p.byte_of(from);
            let (b1, cut1) = p.byte_of(to);
            if cut0 {
                f("\u{FFFD}");
                b0 += 4;
            }
            if b1 > b0 {
                f(&p.text[b0..b1]);
            }
            if cut1 {
                f("\u{FFFD}");
            }
        }
    }

    /// The whole text.
    pub fn string(&self) -> String {
        let mut out = String::with_capacity(self.len8());
        for c in &self.chunks {
            for p in &c.paras {
                out.push_str(&p.text);
            }
        }
        out
    }

    /// The attributes at `u` (< the length) and the range of the run
    /// holding them. With `extend`, the range goes on over the paragraphs
    /// around while their runs have the same attributes, as a run of an
    /// attributed string's would; otherwise it ends at the paragraph's
    /// ends.
    pub fn attrs_at(&self, u: usize, extend: bool) -> (AttrId, Range<usize>) {
        let at = self.locate(u);
        let p = self.para(at);
        let (attrs, run) = p.run_at((u - at.start) as u32);
        let (mut start, mut end) = (at.start + run.start as usize, at.start + run.end as usize);
        if extend {
            if run.start == 0 {
                start = self.extend_back(at, attrs);
            }
            if run.end == p.len16 {
                end = self.extend_forward(at, attrs);
            }
        }
        (attrs, start..end)
    }

    /// Where a run of `attrs` reaching back to the start of paragraph `at`
    /// starts.
    fn extend_back(&self, at: At, attrs: AttrId) -> usize {
        let mut start = at.start;
        let (mut c, mut slot) = (at.chunk, at.slot);
        loop {
            // Earlier paragraphs of this chunk.
            while slot > 0 {
                slot -= 1;
                let p = &self.chunks[c].paras[slot];
                match p.runs.last() {
                    Some(r) if r.attrs == attrs && p.runs.len() == 1 => start -= p.len16 as usize,
                    Some(r) if r.attrs == attrs => return start - r.len as usize,
                    Some(_) => return start,
                    None => {}
                }
            }
            // Whole chunks at a time while they are uniform.
            while c > 0 && self.chunks[c - 1].uniform == Some(attrs) {
                c -= 1;
                start -= self.chunks[c].sum.u16;
            }
            if c == 0 {
                return start;
            }
            c -= 1;
            slot = self.chunks[c].paras.len();
        }
    }

    /// Where a run of `attrs` reaching to the end of paragraph `at` ends.
    fn extend_forward(&self, at: At, attrs: AttrId) -> usize {
        let mut end = at.start + self.para(at).len16 as usize;
        let (mut c, mut slot) = (at.chunk, at.slot + 1);
        loop {
            while slot < self.chunks[c].paras.len() {
                let p = &self.chunks[c].paras[slot];
                match p.runs.first() {
                    Some(r) if r.attrs == attrs && p.runs.len() == 1 => end += p.len16 as usize,
                    Some(r) if r.attrs == attrs => return end + r.len as usize,
                    Some(_) => return end,
                    None => {}
                }
                slot += 1;
            }
            while c + 1 < self.chunks.len() && self.chunks[c + 1].uniform == Some(attrs) {
                c += 1;
                end += self.chunks[c].sum.u16;
            }
            if c + 1 >= self.chunks.len() {
                return end;
            }
            c += 1;
            slot = 0;
        }
    }

    /// Call `f` with each run over `range` (clipped to it), in order.
    pub fn for_each_run(&self, range: Range<usize>, mut f: impl FnMut(Range<usize>, AttrId)) {
        if range.is_empty() {
            return;
        }
        for (at, p) in self.paragraphs_from(range.start) {
            if at.start >= range.end {
                break;
            }
            let mut start = at.start;
            for run in &p.runs {
                let end = start + run.len as usize;
                let (s, e) = (start.max(range.start), end.min(range.end));
                if s < e {
                    f(s..e, run.attrs);
                }
                start = end;
            }
        }
    }

    /// Replace `range` (UTF-16) with `text`, which gets `attrs`.
    pub fn replace(&mut self, range: Range<usize>, text: &str, attrs: AttrId) {
        assert!(range.start <= range.end && range.end <= self.len(), "sidestep: an edit outside the text");
        if range.is_empty() && text.is_empty() {
            return;
        }
        self.generation += 1;
        // Everything replaced: the new text's paragraphs, from scratch.
        if range.start == 0 && range.end == self.len() {
            let runs = [Run { len: utf16_len(text), attrs }];
            let paras = split(text, &runs[..usize::from(runs[0].len > 0)], true);
            self.chunks = paras.chunks(MAX_CHUNK / 2).map(|c| Chunk::new(c.to_vec())).collect();
            self.restart(0);
            return;
        }
        let at = self.locate(range.start);
        let p = self.para(at);
        let (rel, rel_end) = ((range.start - at.start) as u32, (range.end - at.start) as u32);
        // Inside one paragraph's content, bringing no separator: edit it in
        // place. (Not at the start of a paragraph after a "\r", which
        // could join what follows into "\r\n".)
        let after_cr = rel == 0 && self.prev(at).is_some_and(|prev| self.para(prev).text.ends_with('\r'));
        if rel_end <= p.len16 - u32::from(p.sep16) && !has_separator(text) && !after_cr {
            let chunk = &mut self.chunks[at.chunk];
            edit_para(&mut chunk.paras[at.slot], rel..rel_end, text, attrs);
            chunk.resum();
            self.restart(at.chunk + 1);
            return;
        }
        self.replace_paragraphs(at, range, text, attrs);
    }

    /// An edit that may make or join paragraphs: the paragraphs it touches
    /// are joined, edited and cut into paragraphs again.
    fn replace_paragraphs(&mut self, mut first: At, range: Range<usize>, text: &str, attrs: AttrId) {
        // A "\r" ending the paragraph before may join a "\n" the edit brings
        // (or leaves) at the start.
        if range.start == first.start
            && let Some(prev) = self.prev(first)
            && self.para(prev).text.ends_with('\r')
        {
            first = prev;
        }
        let last = self.locate(range.end);
        // The paragraphs from `first` through `last`, as one.
        let mut joined = String::new();
        let mut runs: SmallVec<[Run; 2]> = SmallVec::new();
        let mut at = Some(first);
        let mut count = 0;
        while let Some(a) = at {
            let p = self.para(a);
            joined.push_str(&p.text);
            runs.extend(p.runs.iter().copied());
            count += 1;
            if a.para == last.para {
                break;
            }
            at = self.next(a);
        }
        let mut para = Para::new(joined, runs);
        let base = first.start;
        edit_para(&mut para, (range.start - base) as u32..(range.end - base) as u32, text, attrs);
        let (mut joined, mut runs) = (para.text, para.runs);
        // The joined text must end in a separator (or the text): take in
        // the paragraphs after it until it does, and a "\n" after a "\r".
        let mut after = self.next(last);
        while let Some(a) = after {
            let p = self.para(a);
            let ends = separator_at_end(&joined).0 > 0;
            if ends && !(joined.ends_with('\r') && p.text.starts_with('\n')) {
                break;
            }
            joined.push_str(&p.text);
            runs.extend(p.runs.iter().copied());
            count += 1;
            after = self.next(a);
        }
        let new = split(&joined, &runs, after.is_none());
        self.splice(first, count, new);
    }

    /// Replace the `count` paragraphs from `first` with `new`.
    fn splice(&mut self, first: At, count: usize, new: Vec<Para>) {
        let (c0, s0) = (first.chunk, first.slot);
        // Take the paragraphs out of their chunks, keeping what follows the
        // last of them in the first chunk.
        let mut left = count;
        let mut tail: Vec<Para> = Vec::new();
        let mut c = c0;
        let mut slot = s0;
        let mut emptied = Vec::new();
        while left > 0 {
            let chunk = &mut self.chunks[c];
            let take = (chunk.paras.len() - slot).min(left);
            chunk.paras.drain(slot..slot + take);
            left -= take;
            if c != c0 {
                if left == 0 {
                    tail = std::mem::take(&mut chunk.paras);
                }
                emptied.push(c);
            }
            c += 1;
            slot = 0;
        }
        let chunk = &mut self.chunks[c0];
        let rest: Vec<Para> = chunk.paras.drain(s0..).collect();
        chunk.paras.extend(new);
        chunk.paras.extend(rest);
        chunk.paras.extend(tail);
        // Chunks emptied along the way go.
        for &e in emptied.iter().rev() {
            self.chunks.remove(e);
        }
        self.rebalance(c0);
        self.restart(c0.saturating_sub(1));
    }

    /// Keep chunk `c` between the sizes allowed: split one that grew, join
    /// one that shrank with the next, drop an empty one.
    fn rebalance(&mut self, c: usize) {
        let len = self.chunks[c].paras.len();
        if len > MAX_CHUNK {
            let paras = std::mem::take(&mut self.chunks[c].paras);
            let pieces = len.div_ceil(MAX_CHUNK / 2);
            let size = len.div_ceil(pieces);
            let mut it = paras.into_iter();
            let chunks: Vec<Chunk> = (0..pieces).map(|_| Chunk::new(it.by_ref().take(size).collect())).collect();
            self.chunks.splice(c..=c, chunks.into_iter().filter(|c| !c.paras.is_empty()));
            return;
        }
        if len == 0 && self.chunks.len() > 1 {
            self.chunks.remove(c);
            return;
        }
        if len < MIN_CHUNK && c + 1 < self.chunks.len() && len + self.chunks[c + 1].paras.len() <= MAX_CHUNK {
            let next = self.chunks.remove(c + 1);
            self.chunks[c].paras.extend(next.paras);
        }
        self.chunks[c].resum();
    }

    /// Give `range` the attributes `attrs`.
    pub fn set_attrs(&mut self, range: Range<usize>, attrs: AttrId) {
        assert!(range.start <= range.end && range.end <= self.len(), "sidestep: attributes outside the text");
        if range.is_empty() {
            return;
        }
        self.generation += 1;
        let first = self.locate(range.start);
        let mut at = Some(first);
        let mut last_chunk = first.chunk;
        while let Some(a) = at {
            if a.start >= range.end {
                break;
            }
            let p = &mut self.chunks[a.chunk].paras[a.slot];
            let len = p.len16;
            let from = range.start.saturating_sub(a.start) as u32;
            let to = ((range.end - a.start) as u32).min(len);
            if from < to {
                splice_runs(&mut p.runs, from..to, to - from, attrs);
            }
            last_chunk = a.chunk;
            at = self.next(a);
        }
        for c in &mut self.chunks[first.chunk..=last_chunk] {
            c.resum();
        }
    }

    /// Map every run's attributes through `f` (after the attribute table
    /// has been compacted).
    pub fn remap(&mut self, mut f: impl FnMut(AttrId) -> AttrId) {
        for c in &mut self.chunks {
            for p in &mut c.paras {
                for r in &mut p.runs {
                    r.attrs = f(r.attrs);
                }
                // Runs that differed may now be equal.
                let runs = std::mem::take(&mut p.runs);
                for r in runs {
                    match p.runs.last_mut() {
                        Some(last) if last.attrs == r.attrs => last.len += r.len,
                        _ => p.runs.push(r),
                    }
                }
            }
            c.resum();
        }
    }

    /// Every attribute in use.
    pub fn for_each_attrs(&self, mut f: impl FnMut(AttrId)) {
        for c in &self.chunks {
            for p in &c.paras {
                for r in &p.runs {
                    f(r.attrs);
                }
            }
        }
    }
}

/// Edit a paragraph's text and runs: `range` (UTF-16, from its start)
/// becomes `text` with `attrs`. A surrogate pair the range cuts leaves
/// U+FFFD for the half outside it.
fn edit_para(p: &mut Para, range: Range<u32>, text: &str, attrs: AttrId) {
    let (b0, cut0) = p.byte_of(range.start);
    let (b1, cut1) = p.byte_of(range.end);
    let inserted = utf16_len(text);
    let long = cut0 || cut1;
    if long {
        let mut s = String::with_capacity(text.len() + 6);
        if cut0 {
            s.push('\u{FFFD}');
        }
        s.push_str(text);
        if cut1 {
            s.push('\u{FFFD}');
        }
        // The cut characters go whole, and their outside halves come back
        // as U+FFFD.
        let end = if cut1 { b1 + 4 } else { b1 };
        p.text.replace_range(b0..end, &s);
    } else {
        p.text.replace_range(b0..b1, text);
    }
    let old_len = range.end - range.start;
    p.len16 = p.len16 - old_len + inserted;
    // The replacement halves keep the attributes they had: only the
    // range's units change.
    splice_runs(&mut p.runs, range.clone(), inserted, attrs);
    let (sep8, sep16) = separator_at_end(&p.text);
    (p.sep8, p.sep16) = (sep8, sep16);
    p.ascii = if p.ascii { text.is_ascii() && !long } else { p.text.len() < LONG && p.text.is_ascii() };
    p.index = OnceCell::new();
    debug_assert_eq!(p.len16, utf16_len(&p.text));
}

/// Cut `text` (with `runs` covering it) into paragraphs. With `last`, the
/// text ends the storage, so what follows its last separator (perhaps
/// nothing) is a paragraph too; otherwise the text ends in a separator.
fn split(text: &str, runs: &[Run], last: bool) -> Vec<Para> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut runs = runs.iter().copied().peekable();
    let mut carry: Option<Run> = None;
    let mut take_runs = |mut len: u32| {
        let mut mine: SmallVec<[Run; 2]> = SmallVec::new();
        while len > 0 {
            let mut r = carry.take().or_else(|| runs.next()).expect("runs cover the text");
            if r.len > len {
                carry = Some(Run { len: r.len - len, attrs: r.attrs });
                r.len = len;
            }
            len -= r.len;
            match mine.last_mut() {
                Some(last) if last.attrs == r.attrs => last.len += r.len,
                _ => mine.push(r),
            }
        }
        mine
    };
    while let Some(end) = paragraph_end(rest) {
        let piece = &rest[..end];
        let len = utf16_len(piece);
        out.push(Para::new(piece.to_string(), take_runs(len)));
        rest = &rest[end..];
    }
    if last {
        let len = utf16_len(rest);
        out.push(Para::new(rest.to_string(), take_runs(len)));
    } else {
        debug_assert!(rest.is_empty(), "sidestep: paragraphs split short of a separator");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64*, enough for edit sequences.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    /// The naive model: UTF-16 units and an attribute per unit.
    #[derive(Default)]
    struct Model {
        units: Vec<u16>,
        attrs: Vec<AttrId>,
    }

    impl Model {
        fn replace(&mut self, range: Range<usize>, text: &str, attrs: AttrId) {
            if range.is_empty() && text.is_empty() {
                return;
            }
            let mut new: Vec<u16> = text.encode_utf16().collect();
            let n = new.len();
            // Halves of pairs the edit cuts become U+FFFD, as the storage
            // keeps them.
            let cut = |i: usize| i > 0 && is_high(self.units[i - 1]) && is_low_at(&self.units, i);
            let (cut0, cut1) = (cut(range.start), cut(range.end));
            if cut0 {
                self.units[range.start - 1] = 0xFFFD;
            }
            if cut1 {
                self.units[range.end] = 0xFFFD;
            }
            self.units.splice(range.clone(), new.drain(..));
            self.attrs.splice(range, std::iter::repeat_n(attrs, n));
        }

        fn string(&self) -> String {
            String::from_utf16_lossy(&self.units)
        }

        /// Paragraph ranges, as the storage should have them.
        fn paragraphs(&self) -> Vec<Range<usize>> {
            let mut out = Vec::new();
            let mut start = 0;
            let mut i = 0;
            while i < self.units.len() {
                let u = self.units[i];
                let sep = match u {
                    0x0D if self.units.get(i + 1) == Some(&0x0A) => 2,
                    0x0A | 0x0D | 0x2029 => 1,
                    _ => 0,
                };
                if sep > 0 {
                    out.push(start..i + sep);
                    start = i + sep;
                    i += sep;
                } else {
                    i += 1;
                }
            }
            out.push(start..self.units.len());
            out
        }
    }

    fn is_high(u: u16) -> bool {
        (0xD800..0xDC00).contains(&u)
    }

    fn is_low_at(units: &[u16], i: usize) -> bool {
        units.get(i).is_some_and(|&u| (0xDC00..0xE000).contains(&u))
    }

    fn check(s: &Storage, m: &Model) {
        assert_eq!(s.len(), m.units.len());
        assert_eq!(s.string(), m.string());
        assert_eq!(s.len8(), m.string().len());
        let mut units = Vec::new();
        s.units(0..s.len(), &mut units);
        assert_eq!(units, m.units);
        // Paragraphs.
        let paras = m.paragraphs();
        assert_eq!(s.paragraph_count(), paras.len(), "paragraphs of {:?}", m.string());
        for (n, r) in paras.iter().enumerate() {
            let at = s.locate_paragraph(n);
            assert_eq!(at.start, r.start);
            assert_eq!(s.para(at).len16 as usize, r.len());
        }
        // Attributes, unit by unit, and runs merged.
        for i in 0..m.units.len() {
            let (a, run) = s.attrs_at(i, false);
            assert_eq!(a, m.attrs[i], "attrs at {i}");
            assert!(run.contains(&i));
            let (a2, long) = s.attrs_at(i, true);
            assert_eq!(a2, a);
            // The extended run is the longest one.
            assert!(long.start == 0 || m.attrs[long.start - 1] != a);
            assert!(long.end == m.units.len() || m.attrs[long.end] != a);
            assert!(m.attrs[long.clone()].iter().all(|&x| x == a));
        }
        for c in &s.chunks {
            for p in &c.paras {
                assert!(p.runs.windows(2).all(|w| w[0].attrs != w[1].attrs), "runs merge");
                assert!(p.runs.iter().all(|r| r.len > 0));
            }
            assert!(!c.paras.is_empty() && c.paras.len() <= MAX_CHUNK);
        }
        // Units one at a time, sampled.
        for i in (0..m.units.len()).step_by(7) {
            assert_eq!(s.unit_at(i), m.units[i]);
        }
    }

    const PIECES: &[&str] = &[
        "a", "bc", "hello", " ", "\n", "\r", "\r\n", "\u{2029}", "é", "日本", "😀", "x😀y", "\n\n", "ab\ncd", "\r\r\n",
        "", "q\u{2029}", "long line of text ",
    ];

    fn random_edits(seed: u64, steps: usize) {
        let mut rng = Rng(seed);
        let mut s = Storage::new();
        let mut m = Model::default();
        for _ in 0..steps {
            let len = m.units.len();
            match rng.below(10) {
                0..=6 => {
                    let start = rng.below(len + 1);
                    let end = (start + rng.below(6)).min(len);
                    let mut text = String::new();
                    for _ in 0..rng.below(3) {
                        text.push_str(PIECES[rng.below(PIECES.len())]);
                    }
                    let attrs = rng.below(3) as AttrId;
                    s.replace(start..end, &text, attrs);
                    m.replace(start..end, &text, attrs);
                }
                _ => {
                    let start = rng.below(len + 1);
                    let end = (start + rng.below(12)).min(len);
                    let attrs = rng.below(3) as AttrId;
                    s.set_attrs(start..end, attrs);
                    m.attrs[start..end].iter_mut().for_each(|a| *a = attrs);
                }
            }
            check(&s, &m);
        }
    }

    #[test]
    fn random_edits_match_the_naive_model() {
        for seed in 1..40 {
            random_edits(seed * 0x9E37_79B9, 300);
        }
    }

    #[test]
    fn many_paragraphs_split_and_join_chunks() {
        let mut rng = Rng(7);
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let mut s = Storage::with_text(&text, 1);
        let mut m = Model::default();
        m.replace(0..0, &text, 1);
        check(&s, &m);
        assert!(s.chunks.len() > 8);
        for _ in 0..300 {
            let len = m.units.len();
            let start = rng.below(len + 1);
            let end = (start + rng.below(400)).min(len);
            let text = if rng.below(2) == 0 { "x\ny\n" } else { "" };
            s.replace(start..end, text, 2);
            m.replace(start..end, text, 2);
            let a = rng.below(m.units.len() + 1);
            let b = (a + rng.below(300)).min(m.units.len());
            s.set_attrs(a..b, 3);
            m.attrs[a..b].iter_mut().for_each(|x| *x = 3);
        }
        check(&s, &m);
    }

    #[test]
    fn long_paragraphs_index_utf16() {
        let text: String = (0..3000).map(|i| if i % 5 == 0 { "é😀" } else { "ab" }).collect();
        let mut s = Storage::with_text(&text, 0);
        let mut m = Model::default();
        m.replace(0..0, &text, 0);
        check(&s, &m);
        let mut rng = Rng(99);
        for _ in 0..200 {
            let len = m.units.len();
            let start = rng.below(len + 1);
            let end = (start + rng.below(5)).min(len);
            s.replace(start..end, "日", 1);
            m.replace(start..end, "日", 1);
        }
        check(&s, &m);
    }

    #[test]
    fn crlf_is_never_split() {
        let mut s = Storage::with_text("a\rb", 0);
        assert_eq!(s.paragraph_count(), 2);
        s.replace(2..2, "\n", 0);
        assert_eq!(s.string(), "a\r\nb");
        assert_eq!(s.paragraph_count(), 2);
        assert_eq!(s.paragraph_range(0), 0..3);
        s.replace(2..3, "", 0);
        assert_eq!(s.paragraph_count(), 2);
        assert_eq!(s.paragraph_range(0), 0..2);
    }

    #[test]
    fn extended_runs_cross_uniform_chunks() {
        let text: String = (0..5000).map(|_| "ab\n").collect();
        let mut s = Storage::with_text(&text, 4);
        assert_eq!(s.attrs_at(7000, true), (4, 0..s.len()));
        assert_eq!(s.attrs_at(7000, false).1, 6999..7002);
        s.set_attrs(3000..3001, 5);
        assert_eq!(s.attrs_at(7000, true), (4, 3001..s.len()));
        assert_eq!(s.attrs_at(10, true), (4, 0..3000));
    }
}
