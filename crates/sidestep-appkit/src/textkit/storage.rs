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
//! Long text taken in whole (a text set all at once, a large paste) isn't
//! cut into paragraphs at once. It is copied once into a shared buffer and
//! cut into raw chunks of whole paragraphs, a few kilobytes each, whose
//! sums are counted with a pass over the bytes; a raw chunk is cut into
//! its paragraphs, which slice the shared buffer until they are edited,
//! when something first reads them. So setting 11 MB of text costs a copy
//! and a count, and the paragraphs get made as layout or a question
//! reaches them. What needs no paragraphs reads a raw chunk as it is: its
//! text, its runs (which may cross its separators), and setting
//! attributes over it. When most of such a text is deleted, so the shared
//! buffers would hold much more than the text left, what is left is copied
//! into buffers of its own and the old ones go.
//!
//! Attributes are interned per storage (`attrs`), so runs hold a small
//! number ([`AttrId`]) and runs with equal attributes merge. Each chunk
//! notes how much of it, from its start, `NSTextStorage` has fixed the
//! attributes of (its fixing, often lazy, reads that no more); any change
//! of the chunk clears the note.
//!
//! `NSString` can hold lone surrogates; UTF-8 can't. An edit that splits a
//! surrogate pair leaves U+FFFD for the half that remains, and text with a
//! lone surrogate stores U+FFFD for it: one UTF-16 unit either way, so
//! every length and index is as it would be.

use std::cell::OnceCell;
use std::ops::Range;
use std::sync::Arc;

use smallvec::SmallVec;

/// An interned attribute dictionary (`attrs::AttrTable`).
pub(crate) type AttrId = u32;

/// A run of attributes over `len` UTF-16 units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub len: u32,
    pub attrs: AttrId,
}

/// A paragraph's runs, or a raw chunk's.
pub(crate) type Runs = SmallVec<[Run; 2]>;

/// The most paragraphs a chunk holds, and the fewest it keeps before it
/// joins a neighbour.
const MAX_CHUNK: usize = 128;
const MIN_CHUNK: usize = 16;

/// Paragraphs this long (in bytes) keep an index of UTF-16 checkpoints.
const LONG: usize = 4096;

/// Bytes between checkpoints.
const CHECKPOINT: usize = 256;

/// About how many bytes the first raw chunk of a text takes; the ones after
/// it take as many as held half [`MAX_CHUNK`] paragraphs before.
const RAW_BYTES: usize = 4096;

/// Text of this many bytes or more that an edit brings in goes into raw
/// chunks; shorter text is cut into paragraphs at once.
const RAW_MIN: usize = 16 * 1024;

/// A paragraph's text: a slice of text shared with the paragraphs it came
/// in with, until the paragraph is edited, or its own.
#[derive(Clone, Debug)]
enum Text {
    Shared { buf: Arc<str>, start: u32, end: u32 },
    Own(String),
}

impl Text {
    fn as_str(&self) -> &str {
        match self {
            Text::Shared { buf, start, end } => &buf[*start as usize..*end as usize],
            Text::Own(s) => s,
        }
    }

    /// The text as a string of its own, to edit.
    fn to_mut(&mut self) -> &mut String {
        if let Text::Shared { .. } = self {
            *self = Text::Own(self.as_str().to_owned());
        }
        match self {
            Text::Own(s) => s,
            Text::Shared { .. } => unreachable!("made its own above"),
        }
    }

    fn into_string(self) -> String {
        match self {
            Text::Own(s) => s,
            shared => shared.as_str().to_owned(),
        }
    }
}

/// A paragraph: its text, separator included, and its runs.
#[derive(Clone, Debug)]
pub(crate) struct Para {
    text: Text,
    len16: u32,
    /// The separator's length in UTF-16 units and in bytes.
    sep16: u8,
    sep8: u8,
    /// The text is ASCII, so units are bytes.
    ascii: bool,
    runs: Runs,
    /// (UTF-16 offset, byte) every [`CHECKPOINT`] bytes or so, for long
    /// paragraphs that aren't ASCII.
    index: OnceCell<Box<[(u32, u32)]>>,
}

impl Para {
    fn new(text: String, runs: Runs) -> Para {
        let len16 = utf16_len(&text);
        Para::with_len(Text::Own(text), runs, len16)
    }

    /// [`Para::new`], its UTF-16 length known.
    fn with_len(text: Text, runs: Runs, len16: u32) -> Para {
        let s = text.as_str();
        let (sep8, sep16) = separator_at_end(s);
        // As many bytes as units: every character is one byte.
        let ascii = len16 as usize == s.len();
        debug_assert_eq!(runs.iter().map(|r| r.len).sum::<u32>(), len16, "runs cover the paragraph");
        Para { text, len16, sep16, sep8, ascii, runs, index: OnceCell::new() }
    }

    /// The text, separator included.
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    pub fn len16(&self) -> u32 {
        self.len16
    }

    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// The byte where UTF-16 offset `u` falls, and whether it falls inside
    /// a character (between the halves of a surrogate pair), in which case
    /// the byte is that character's start.
    pub fn byte_of(&self, u: u32) -> (usize, bool) {
        let text = self.text();
        if self.ascii || u == 0 {
            return (u as usize, false);
        }
        if u >= self.len16 {
            return (text.len(), false);
        }
        let (mut unit, mut byte) = if text.len() >= LONG {
            let index = self.index.get_or_init(|| checkpoints(text));
            let at = index.partition_point(|&(cu, _)| cu <= u).saturating_sub(1);
            index.get(at).map_or((0, 0), |&(cu, cb)| (cu, cb as usize))
        } else {
            (0, 0)
        };
        for c in text[byte..].chars() {
            let n = c.len_utf16() as u32;
            if unit + n > u {
                return (byte, unit != u);
            }
            unit += n;
            byte += c.len_utf8();
        }
        (text.len(), false)
    }

    /// The UTF-16 unit at offset `u`.
    fn unit_at(&self, u: u32) -> u16 {
        if self.ascii {
            return u16::from(self.text().as_bytes()[u as usize]);
        }
        let (byte, inside) = self.byte_of(u);
        let c = self.text()[byte..].chars().next().expect("a character at an offset inside the paragraph");
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

/// Byte counters side by side, emptied before they overflow: a shape the
/// compiler turns into vector instructions.
const LANES: usize = 32;

/// How many UTF-16 units `s` takes: one for each character, two for each
/// of four bytes.
pub(crate) fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        return s.len() as u32;
    }
    let mut n = 0usize;
    for block in s.as_bytes().chunks(LANES * 255) {
        let (mut starts, mut fours) = ([0u8; LANES], [0u8; LANES]);
        let (words, rest) = block.as_chunks::<LANES>();
        for w in words {
            for i in 0..LANES {
                // Every byte but a continuation byte (10xxxxxx) starts a
                // character; 11110xxx starts one of four bytes.
                starts[i] += u8::from((w[i] as i8) >= -0x40);
                fours[i] += u8::from(w[i] >= 0xF0);
            }
        }
        n += starts.iter().chain(&fours).map(|&c| usize::from(c)).sum::<usize>();
        n += rest.iter().map(|&b| usize::from((b as i8) >= -0x40) + usize::from(b >= 0xF0)).sum::<usize>();
    }
    n as u32
}

/// How many of `b`'s bytes are `\n`, `\r` and 0xE2 (which U+2029 starts
/// with).
fn count_separator_bytes(b: &[u8]) -> [usize; 3] {
    let mut total = [0usize; 3];
    for block in b.chunks(LANES * 255) {
        let mut acc = [[0u8; LANES]; 3];
        let (words, rest) = block.as_chunks::<LANES>();
        for w in words {
            for i in 0..LANES {
                acc[0][i] += u8::from(w[i] == b'\n');
                acc[1][i] += u8::from(w[i] == b'\r');
                acc[2][i] += u8::from(w[i] == 0xE2);
            }
        }
        for (t, a) in total.iter_mut().zip(&acc) {
            *t += a.iter().map(|&c| usize::from(c)).sum::<usize>();
        }
        for &c in rest {
            total[0] += usize::from(c == b'\n');
            total[1] += usize::from(c == b'\r');
            total[2] += usize::from(c == 0xE2);
        }
    }
    total
}

/// How many paragraph separators `b` holds (`\r\n` is one).
fn count_separators(b: &[u8]) -> usize {
    let [nl, cr, e2] = count_separator_bytes(b);
    let mut n = nl + cr;
    if cr > 0 {
        n -= b.windows(2).filter(|w| w[0] == b'\r' && w[1] == b'\n').count();
    }
    if e2 > 0 {
        n += b.windows(3).filter(|w| w == b"\xE2\x80\xA9").count();
    }
    n
}

/// Where the first paragraph separator at or after byte `from` ends, if
/// there is one.
fn separator_end(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(k) = memchr::memchr3(b'\n', b'\r', 0xE2, &b[i..]) {
        let at = i + k;
        match b[at] {
            b'\n' => return Some(at + 1),
            b'\r' => return Some(if b.get(at + 1) == Some(&b'\n') { at + 2 } else { at + 1 }),
            _ if b[at + 1..].starts_with(b"\x80\xA9") => return Some(at + 3),
            _ => i = at + 1,
        }
    }
    None
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

/// Whether `text` holds a paragraph separator.
pub(crate) fn has_separator(text: &str) -> bool {
    text.contains(['\n', '\r', '\u{2029}'])
}

/// Add `run` at the end of `out`, merging it with an equal neighbour.
fn push_run(out: &mut Runs, run: Run) {
    if run.len == 0 {
        return;
    }
    match out.last_mut() {
        Some(last) if last.attrs == run.attrs => last.len += run.len,
        _ => out.push(run),
    }
}

/// Replace `range` of `runs` (UTF-16, from their start) with the runs
/// `new`, merging equal neighbours.
fn splice_runs(runs: &mut Runs, range: Range<u32>, new: &[Run]) {
    let mut out = Runs::with_capacity(runs.len() + new.len() + 1);
    let mut start = 0;
    let mut placed = false;
    for &run in runs.iter() {
        let end = start + run.len;
        // The part before the range.
        if start < range.start {
            push_run(&mut out, Run { len: end.min(range.start) - start, attrs: run.attrs });
        }
        if !placed && end >= range.start {
            new.iter().for_each(|&r| push_run(&mut out, r));
            placed = true;
        }
        // The part after the range.
        if end > range.end {
            push_run(&mut out, Run { len: end - start.max(range.end), attrs: run.attrs });
        }
        start = end;
    }
    if !placed {
        new.iter().for_each(|&r| push_run(&mut out, r));
    }
    *runs = out;
}

/// The attributes all of `runs` have, if they have one.
fn uniform_of(runs: &[Run]) -> Option<AttrId> {
    let first = runs.first()?.attrs;
    runs.iter().all(|r| r.attrs == first).then_some(first)
}

/// Runs taken from the front of a sequence a length at a time, cutting the
/// one that straddles.
struct RunCursor<'a> {
    runs: std::slice::Iter<'a, Run>,
    carry: Option<Run>,
}

impl RunCursor<'_> {
    fn new(runs: &[Run]) -> RunCursor<'_> {
        RunCursor { runs: runs.iter(), carry: None }
    }

    fn take(&mut self, mut len: u32) -> Runs {
        let mut mine = Runs::new();
        while len > 0 {
            let mut r = self.carry.take().or_else(|| self.runs.next().copied()).expect("runs cover the text");
            if r.len > len {
                self.carry = Some(Run { len: r.len - len, attrs: r.attrs });
                r.len = len;
            }
            len -= r.len;
            push_run(&mut mine, r);
        }
        mine
    }
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
        Sum { u16: p.len16 as usize, u8: p.text().len(), paras: 1 }
    }

    fn add(self, o: Sum) -> Sum {
        Sum { u16: self.u16 + o.u16, u8: self.u8 + o.u8, paras: self.paras + o.paras }
    }
}

/// Whole paragraphs of a shared text, not yet cut apart, and the runs over
/// them.
#[derive(Clone, Debug)]
struct Raw {
    buf: Arc<str>,
    start: usize,
    end: usize,
    /// The text is ASCII, so units are bytes.
    ascii: bool,
    runs: Runs,
    /// It ends the storage's text: what follows its last separator (perhaps
    /// nothing) is a paragraph too.
    last: bool,
}

impl Raw {
    fn text(&self) -> &str {
        &self.buf[self.start..self.end]
    }

    /// Its paragraphs, slicing the shared text.
    fn cut(&self) -> Vec<Para> {
        paragraphs(self.text(), &self.runs, self.last, self.ascii, |r| {
            if r.is_empty() {
                Text::Own(String::new())
            } else {
                Text::Shared {
                    buf: self.buf.clone(),
                    start: (self.start + r.start) as u32,
                    end: (self.start + r.end) as u32,
                }
            }
        })
    }
}

#[derive(Clone, Debug)]
struct Chunk {
    /// The paragraphs; a raw chunk's are cut when first read.
    paras: OnceCell<Vec<Para>>,
    /// Text not cut into paragraphs yet. Once `paras` is made it says the
    /// same, and goes with the first change.
    raw: Option<Raw>,
    sum: Sum,
    /// The attributes every paragraph of the chunk has throughout, if they
    /// have one: lets an effective range cross the chunk in one step.
    uniform: Option<AttrId>,
    /// How many of its units, from its start, have their attributes fixed
    /// (fixing them again would change nothing), as fixing noted
    /// ([`Storage::mark_fixed`]); any change of the chunk makes it none.
    fixed: usize,
}

impl Chunk {
    fn built(paras: Vec<Para>) -> Chunk {
        let mut c = Chunk { paras: OnceCell::from(paras), raw: None, sum: Sum::default(), uniform: None, fixed: 0 };
        c.resum();
        c
    }

    fn raw(raw: Raw, sum: Sum) -> Chunk {
        let uniform = uniform_of(&raw.runs);
        Chunk { paras: OnceCell::new(), raw: Some(raw), sum, uniform, fixed: 0 }
    }

    /// The paragraphs, cut from the raw text if they haven't been.
    fn paras(&self) -> &[Para] {
        self.paras.get_or_init(|| self.raw.as_ref().expect("sidestep: a chunk without text").cut())
    }

    /// The paragraphs, to change.
    fn paras_mut(&mut self) -> &mut Vec<Para> {
        self.paras();
        self.raw = None;
        self.fixed = 0;
        self.paras.get_mut().expect("made above")
    }

    fn into_paras(mut self) -> Vec<Para> {
        std::mem::take(self.paras_mut())
    }

    /// The raw text, while it hasn't been cut into paragraphs.
    fn uncut(&self) -> Option<&Raw> {
        if self.paras.get().is_some() { None } else { self.raw.as_ref() }
    }

    fn count(&self) -> usize {
        self.sum.paras
    }

    /// The paragraphs it holds now (a chunk being rebalanced has been cut).
    fn count_now(&self) -> usize {
        self.paras.get().map_or(self.sum.paras, Vec::len)
    }

    /// Work out the sums and the uniform attributes again (after a change
    /// of paragraphs, or of a raw chunk's runs).
    fn resum(&mut self) {
        self.fixed = 0;
        if let Some(raw) = self.uncut() {
            self.uniform = uniform_of(&raw.runs);
            return;
        }
        let paras = self.paras.get().expect("cut");
        self.sum = paras.iter().fold(Sum::default(), |s, p| s.add(Sum::of(p)));
        let first = paras.iter().find_map(|p| p.runs.first()).map(|r| r.attrs);
        self.uniform = first.filter(|&a| paras.iter().all(|p| p.only(a)));
    }
}

/// Cut `text` into raw chunks of whole paragraphs (the text's last one too
/// when it ends the storage, `last`), with `runs` covering it: one pass
/// counts each chunk's paragraphs and, unless the text is ASCII, its UTF-16
/// units. A chunk takes about as many bytes as half [`MAX_CHUNK`]
/// paragraphs took in the chunk before, and fewer where paragraphs get
/// short, so none holds more than [`MAX_CHUNK`] (unless one paragraph is
/// longer than that).
fn raw_chunks(buf: &Arc<str>, runs: &[Run], last: bool) -> Vec<Chunk> {
    let b = buf.as_bytes();
    let n = b.len();
    let ascii = buf.is_ascii();
    let mut out = Vec::with_capacity(n / RAW_BYTES + 1);
    let mut cursor = RunCursor::new(runs);
    let (mut pos, mut target) = (0, RAW_BYTES);
    while pos < n {
        let (end, paras) = loop {
            let want = pos + target;
            let end = if want >= n { n } else { separator_end(b, want).unwrap_or(n) };
            let paras = count_separators(&b[pos..end]) + usize::from(last && end == n);
            if paras <= MAX_CHUNK || target <= 64 {
                break (end, paras);
            }
            target /= 2;
        };
        let bytes = &b[pos..end];
        let chunk_ascii = ascii || bytes.is_ascii();
        let len16 = if chunk_ascii { bytes.len() } else { utf16_len(&buf[pos..end]) as usize };
        let raw = Raw {
            buf: buf.clone(),
            start: pos,
            end,
            ascii: chunk_ascii,
            runs: cursor.take(len16 as u32),
            last: last && end == n,
        };
        out.push(Chunk::raw(raw, Sum { u16: len16, u8: end - pos, paras }));
        target = ((end - pos) * (MAX_CHUNK / 2) / paras.max(1)).clamp(64, 64 * RAW_BYTES);
        pos = end;
    }
    if out.is_empty() && last {
        out.push(Chunk::built(vec![Para::new(String::new(), Runs::new())]));
    }
    out
}

/// Paragraphs, or chunks of them, to put in place of others.
enum New {
    Paras(Vec<Para>),
    Chunks(Vec<Chunk>),
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

/// What estimating the layout of paragraphs needs to know of them: of one,
/// or of a raw chunk's all at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extent {
    One { len16: u32, attrs: Option<AttrId> },
    Many { count: usize, len16: usize, attrs: Option<AttrId> },
}

/// A text's paragraphs.
#[derive(Clone, Debug)]
pub(crate) struct Storage {
    chunks: Vec<Chunk>,
    /// The sums of the chunks before each chunk.
    starts: Vec<Sum>,
    /// Bumped by every change of the text (not of its attributes).
    generation: u64,
    /// Text this long or longer is taken in raw ([`RAW_MIN`]; tests lower
    /// it).
    raw_min: usize,
    /// At least as many bytes as the shared buffers the text slices hold:
    /// counted as they are made, and again when it looks like they hold
    /// much more than the text (see [`Storage::release_shared`]).
    shared: usize,
}

impl Default for Storage {
    fn default() -> Self {
        Storage::new()
    }
}

impl Storage {
    /// An empty text: one empty paragraph.
    pub fn new() -> Storage {
        let chunks = vec![Chunk::built(vec![Para::new(String::new(), Runs::new())])];
        Storage { starts: vec![Sum::default()], chunks, generation: 0, raw_min: RAW_MIN, shared: 0 }
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

    /// Changes with every change of the text.
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

    /// The chunk holding UTF-16 unit `u` (the last one that starts at or
    /// before it).
    fn chunk_at(&self, u: usize) -> usize {
        self.starts.partition_point(|s| s.u16 <= u).saturating_sub(1)
    }

    /// The chunks `range` (UTF-16, not empty) touches, with the units each
    /// starts at.
    fn chunks_over(&self, range: Range<usize>) -> impl Iterator<Item = (usize, usize)> + '_ {
        (self.chunk_at(range.start)..self.chunks.len())
            .map(|c| (c, self.starts[c].u16))
            .take_while(move |&(_, start)| start < range.end)
    }

    /// The paragraph holding UTF-16 unit `u` (`u` ≤ the length; the text's
    /// end is in its last paragraph).
    pub fn locate(&self, u: usize) -> At {
        let c = self.chunk_at(u);
        // A chunk ending exactly at `u` hands `u` to the next one, except
        // at the text's end.
        let mut start = self.starts[c].u16;
        let paras = self.chunks[c].paras();
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
        let before = self.chunks[c].paras()[..slot].iter().map(|p| p.len16 as usize).sum::<usize>();
        At { chunk: c, slot, para: n, start: self.starts[c].u16 + before }
    }

    pub fn para(&self, at: At) -> &Para {
        &self.chunks[at.chunk].paras()[at.slot]
    }

    /// The paragraph after `at`, if there is one.
    pub fn next(&self, at: At) -> Option<At> {
        let p = self.para(at);
        let start = at.start + p.len16 as usize;
        if at.slot + 1 < self.chunks[at.chunk].count() {
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
            (at.chunk - 1, self.chunks[at.chunk - 1].count() - 1)
        } else {
            return None;
        };
        let start = at.start - self.chunks[chunk].paras()[slot].len16 as usize;
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
                out.extend(p.text().as_bytes()[from as usize..to as usize].iter().map(|&b| u16::from(b)));
                continue;
            }
            let (b0, inside) = p.byte_of(from);
            let mut unit = from - u32::from(inside);
            let mut buf = [0u16; 2];
            for c in p.text()[b0..].chars() {
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

    /// Call `f` with the text of `range`, a piece at a time (with U+FFFD
    /// for a cut surrogate pair's half).
    pub fn for_each_slice(&self, range: Range<usize>, mut f: impl FnMut(&str)) {
        if range.is_empty() {
            return;
        }
        for (c, start) in self.chunks_over(range.clone()) {
            let chunk = &self.chunks[c];
            let end = start + chunk.sum.u16;
            // A raw chunk the range holds whole is read as it is.
            if let Some(raw) = chunk.uncut()
                && range.start <= start
                && end <= range.end
            {
                f(raw.text());
                continue;
            }
            let mut at = start;
            for p in chunk.paras() {
                let p_end = at + p.len16 as usize;
                if p_end > range.start && at < range.end {
                    let from = range.start.saturating_sub(at) as u32;
                    let to = ((range.end - at) as u32).min(p.len16);
                    let (mut b0, cut0) = p.byte_of(from);
                    let (b1, cut1) = p.byte_of(to);
                    if cut0 {
                        f("\u{FFFD}");
                        b0 += 4;
                    }
                    if b1 > b0 {
                        f(&p.text()[b0..b1]);
                    }
                    if cut1 {
                        f("\u{FFFD}");
                    }
                }
                at = p_end;
            }
        }
    }

    /// The whole text.
    pub fn string(&self) -> String {
        let mut out = String::with_capacity(self.len8());
        for c in &self.chunks {
            match c.uncut() {
                Some(raw) => out.push_str(raw.text()),
                None => c.paras().iter().for_each(|p| out.push_str(p.text())),
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
            match self.chunks[c].uncut() {
                // A raw chunk, from its end: its runs.
                Some(raw) if slot > 0 => {
                    for r in raw.runs.iter().rev() {
                        if r.attrs != attrs {
                            return start;
                        }
                        start -= r.len as usize;
                    }
                }
                Some(_) => {}
                // Earlier paragraphs of this chunk.
                None => {
                    while slot > 0 {
                        slot -= 1;
                        let p = &self.chunks[c].paras()[slot];
                        match p.runs.last() {
                            Some(r) if r.attrs == attrs && p.runs.len() == 1 => start -= p.len16 as usize,
                            Some(r) if r.attrs == attrs => return start - r.len as usize,
                            Some(_) => return start,
                            None => {}
                        }
                    }
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
            slot = self.chunks[c].count();
        }
    }

    /// Where a run of `attrs` reaching to the end of paragraph `at` ends.
    fn extend_forward(&self, at: At, attrs: AttrId) -> usize {
        let mut end = at.start + self.para(at).len16 as usize;
        let (mut c, mut slot) = (at.chunk, at.slot + 1);
        loop {
            match self.chunks[c].uncut() {
                Some(raw) if slot == 0 => {
                    for r in &raw.runs {
                        if r.attrs != attrs {
                            return end;
                        }
                        end += r.len as usize;
                    }
                }
                Some(_) => {}
                None => {
                    let paras = self.chunks[c].paras();
                    while slot < paras.len() {
                        let p = &paras[slot];
                        match p.runs.first() {
                            Some(r) if r.attrs == attrs && p.runs.len() == 1 => end += p.len16 as usize,
                            Some(r) if r.attrs == attrs => return end + r.len as usize,
                            Some(_) => return end,
                            None => {}
                        }
                        slot += 1;
                    }
                }
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

    /// Call `f` with each run over `range` (clipped to it), in order. A raw
    /// chunk's runs may cross its paragraphs' ends.
    pub fn for_each_run(&self, range: Range<usize>, mut f: impl FnMut(Range<usize>, AttrId)) {
        if range.is_empty() {
            return;
        }
        let mut clipped = |s: usize, e: usize, attrs| {
            let (s, e) = (s.max(range.start), e.min(range.end));
            if s < e {
                f(s..e, attrs);
            }
        };
        for (c, mut start) in self.chunks_over(range.clone()) {
            let chunk = &self.chunks[c];
            match chunk.uncut() {
                Some(raw) => {
                    for r in &raw.runs {
                        clipped(start, start + r.len as usize, r.attrs);
                        start += r.len as usize;
                    }
                }
                None => {
                    for r in chunk.paras().iter().flat_map(|p| p.runs.iter()) {
                        clipped(start, start + r.len as usize, r.attrs);
                        start += r.len as usize;
                    }
                }
            }
        }
    }

    /// For fixing attributes: each run over `range` (clipped to it) with
    /// the attributes its paragraph starts with, but for what is marked
    /// fixed ([`Storage::mark_fixed`]). A chunk whose paragraphs all have
    /// one set of attributes throughout comes as one run, cut into
    /// paragraphs or not.
    pub fn for_each_run_with_first(&self, range: Range<usize>, mut f: impl FnMut(Range<usize>, AttrId, AttrId)) {
        if range.is_empty() {
            return;
        }
        for (c, mut start) in self.chunks_over(range.clone()) {
            let chunk = &self.chunks[c];
            let (from, to) = (range.start.max(start + chunk.fixed), range.end);
            let mut clipped = |s: usize, e: usize, attrs, first| {
                let (s, e) = (s.max(from), e.min(to));
                if s < e {
                    f(s..e, attrs, first);
                }
            };
            if from >= to.min(start + chunk.sum.u16) {
                continue;
            }
            if let Some(a) = chunk.uniform {
                clipped(start, start + chunk.sum.u16, a, a);
                continue;
            }
            for p in chunk.paras() {
                let p_end = start + p.len16 as usize;
                if p_end > from && start < to {
                    let first = p.runs.first().map_or(0, |r| r.attrs);
                    let mut at = start;
                    for r in &p.runs {
                        clipped(at, at + r.len as usize, r.attrs, first);
                        at += r.len as usize;
                    }
                }
                start = p_end;
            }
        }
    }

    /// Note that the attributes of `range` are fixed: each chunk it covers
    /// from where the chunk's fixed units end (or before) is fixed as far
    /// as the range goes, so fixing needn't read it again until it changes.
    pub fn mark_fixed(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        for c in self.chunk_at(range.start)..self.chunks.len() {
            let start = self.starts[c].u16;
            if start >= range.end {
                break;
            }
            let chunk = &mut self.chunks[c];
            let (from, to) = (range.start.saturating_sub(start), (range.end - start).min(chunk.sum.u16));
            if from <= chunk.fixed {
                chunk.fixed = chunk.fixed.max(to);
            }
        }
    }

    /// Call `f` with each run from unit `from` on (the first clipped to
    /// start there), in order, until it returns false. A chunk with one
    /// set of attributes throughout comes as one run; raw chunks aren't cut.
    pub fn runs_after(&self, from: usize, mut f: impl FnMut(Range<usize>, AttrId) -> bool) {
        if from >= self.len() {
            return;
        }
        for c in self.chunk_at(from)..self.chunks.len() {
            let chunk = &self.chunks[c];
            let mut at = self.starts[c].u16;
            if let Some(a) = chunk.uniform {
                if !f(at.max(from)..at + chunk.sum.u16, a) {
                    return;
                }
                continue;
            }
            let mut visit = |r: &Run| {
                let (s, e) = (at, at + r.len as usize);
                at = e;
                e <= from || f(s.max(from)..e, r.attrs)
            };
            let on = match chunk.uncut() {
                Some(raw) => raw.runs.iter().all(&mut visit),
                None => chunk.paras().iter().flat_map(|p| &p.runs).all(&mut visit),
            };
            if !on {
                return;
            }
        }
    }

    /// Call `f` with each run before unit `to` (the last clipped to end
    /// there), last first, until it returns false; as [`Storage::runs_after`]
    /// does, the other way.
    pub fn runs_before(&self, to: usize, mut f: impl FnMut(Range<usize>, AttrId) -> bool) {
        let to = to.min(self.len());
        if to == 0 {
            return;
        }
        for c in (0..=self.chunk_at(to - 1)).rev() {
            let chunk = &self.chunks[c];
            let start = self.starts[c].u16;
            let mut at = start + chunk.sum.u16;
            if let Some(a) = chunk.uniform {
                if !f(start..at.min(to), a) {
                    return;
                }
                continue;
            }
            let mut visit = |r: &Run| {
                let (s, e) = (at - r.len as usize, at);
                at = s;
                s >= to || f(s..e.min(to), r.attrs)
            };
            let on = match chunk.uncut() {
                Some(raw) => raw.runs.iter().rev().all(&mut visit),
                None => chunk.paras().iter().rev().flat_map(|p| p.runs.iter().rev()).all(&mut visit),
            };
            if !on {
                return;
            }
        }
    }

    /// What layout estimates need of paragraphs `paras`: a raw chunk's
    /// paragraphs all at once, where the range holds the chunk whole.
    pub fn for_each_extent(&self, paras: Range<usize>, mut f: impl FnMut(Extent)) {
        if paras.is_empty() {
            return;
        }
        let first_chunk = self.starts.partition_point(|s| s.paras <= paras.start).saturating_sub(1);
        for c in first_chunk..self.chunks.len() {
            let first = self.starts[c].paras;
            if first >= paras.end {
                break;
            }
            let chunk = &self.chunks[c];
            if let Some(raw) = chunk.uncut()
                && paras.start <= first
                && first + chunk.count() <= paras.end
            {
                let attrs = raw.runs.first().map(|r| r.attrs);
                f(Extent::Many { count: chunk.count(), len16: chunk.sum.u16, attrs });
                continue;
            }
            for (i, p) in chunk.paras().iter().enumerate() {
                let k = first + i;
                if k >= paras.start && k < paras.end {
                    f(Extent::One { len16: p.len16, attrs: p.runs.first().map(|r| r.attrs) });
                }
            }
        }
    }

    /// Replace `range` (UTF-16) with `text`, which gets `attrs`.
    pub fn replace(&mut self, range: Range<usize>, text: &str, attrs: AttrId) {
        self.replace_runs(range, text, &[Run { len: utf16_len(text), attrs }]);
    }

    /// Replace `range` (UTF-16) with `text`, with the runs `runs` over it.
    pub fn replace_runs(&mut self, range: Range<usize>, text: &str, runs: &[Run]) {
        assert!(range.start <= range.end && range.end <= self.len(), "sidestep: an edit outside the text");
        if range.is_empty() && text.is_empty() {
            return;
        }
        debug_assert_eq!(runs.iter().map(|r| r.len).sum::<u32>(), utf16_len(text), "runs cover the text");
        self.generation += 1;
        // Everything replaced: the new text's paragraphs, from scratch.
        if range.start == 0 && range.end == self.len() {
            self.shared = 0;
            self.chunks = match self.cut(text.into(), runs, true) {
                New::Chunks(chunks) => chunks,
                // Moved into chunks, not copied.
                New::Paras(paras) => {
                    let mut chunks = Vec::with_capacity(paras.len().div_ceil(MAX_CHUNK / 2));
                    let mut it = paras.into_iter();
                    while it.len() > 0 {
                        chunks.push(Chunk::built(it.by_ref().take(MAX_CHUNK / 2).collect()));
                    }
                    chunks
                }
            };
            self.restart(0);
            return;
        }
        let at = self.locate(range.start);
        let p = self.para(at);
        let (rel, rel_end) = ((range.start - at.start) as u32, (range.end - at.start) as u32);
        // Inside one paragraph's content, bringing no separator: edit it in
        // place. (Not at the start of a paragraph after a "\r", which
        // could join what follows into "\r\n".)
        let after_cr = rel == 0 && self.prev(at).is_some_and(|prev| self.para(prev).text().ends_with('\r'));
        if rel_end <= p.len16 - u32::from(p.sep16) && !has_separator(text) && !after_cr {
            let chunk = &mut self.chunks[at.chunk];
            edit_para(&mut chunk.paras_mut()[at.slot], rel..rel_end, text, runs);
            chunk.resum();
            self.restart(at.chunk + 1);
        } else {
            self.replace_paragraphs(at, range, text, runs);
        }
        self.release_shared();
    }

    /// When the shared buffers the text slices may hold much more than the
    /// text (most of a long text deleted), count what they hold, and if
    /// they do, copy what the text slices of them into buffers of its own,
    /// a chunk's to each, so the rest goes.
    fn release_shared(&mut self) {
        let wasteful = |shared: usize, s: &Storage| shared > 2 * s.len8() + s.raw_min;
        if !wasteful(self.shared, self) {
            return;
        }
        self.shared = self.shared_bytes();
        if !wasteful(self.shared, self) {
            return;
        }
        for c in &mut self.chunks {
            if let Some(raw) = c.raw.as_mut().filter(|_| c.paras.get().is_none()) {
                let own: Arc<str> = Arc::from(&raw.buf[raw.start..raw.end]);
                (raw.buf, raw.start, raw.end) = (own.clone(), 0, own.len());
                continue;
            }
            c.raw = None;
            let Some(paras) = c.paras.get_mut() else { continue };
            let mut own = String::new();
            for p in paras.iter() {
                if let Text::Shared { .. } = p.text {
                    own.push_str(p.text());
                }
            }
            let own: Arc<str> = Arc::from(own);
            let mut at = 0;
            for p in paras.iter_mut() {
                if let Text::Shared { start, end, .. } = p.text {
                    let len = end - start;
                    p.text = Text::Shared { buf: own.clone(), start: at, end: at + len };
                    at += len;
                }
            }
        }
        self.shared = self.shared_bytes();
    }

    /// How many bytes the shared buffers the text slices hold, each once.
    fn shared_bytes(&self) -> usize {
        let mut bufs: Vec<(*const u8, usize)> = Vec::new();
        for c in &self.chunks {
            if let Some(raw) = &c.raw {
                bufs.push((raw.buf.as_ptr(), raw.buf.len()));
            }
            for p in c.paras.get().into_iter().flatten() {
                if let Text::Shared { buf, .. } = &p.text {
                    bufs.push((buf.as_ptr(), buf.len()));
                }
            }
        }
        bufs.sort_unstable();
        bufs.dedup();
        bufs.iter().map(|b| b.1).sum()
    }

    /// Text in paragraphs, or in raw chunks when it is long (with `last`,
    /// it ends the storage; otherwise it ends in a separator).
    fn cut(&mut self, text: std::borrow::Cow<'_, str>, runs: &[Run], last: bool) -> New {
        if text.len() >= self.raw_min && u32::try_from(text.len()).is_ok() {
            let buf: Arc<str> = match text {
                std::borrow::Cow::Borrowed(s) => Arc::from(s),
                std::borrow::Cow::Owned(s) => Arc::from(s),
            };
            self.shared += buf.len();
            New::Chunks(raw_chunks(&buf, runs, last))
        } else {
            New::Paras(split(&text, runs, last))
        }
    }

    /// An edit that may make or join paragraphs: the first paragraph it
    /// touches up to the edit, the new text and the last one after the
    /// edit are joined and cut into paragraphs again.
    fn replace_paragraphs(&mut self, mut first: At, range: Range<usize>, text: &str, runs: &[Run]) {
        // A "\r" ending the paragraph before may join a "\n" the edit brings
        // (or leaves) at the start.
        if range.start == first.start
            && let Some(prev) = self.prev(first)
            && self.para(prev).text().ends_with('\r')
        {
            first = prev;
        }
        let last = self.locate(range.end);
        let rel = (range.start - first.start) as u32;
        let (mut joined, mut jruns) = if first.para == last.para {
            let mut p = self.para(first).clone();
            edit_para(&mut p, rel..(range.end - first.start) as u32, text, runs);
            (p.text.into_string(), p.runs)
        } else {
            let mut head = self.para(first).clone();
            let whole = head.len16;
            edit_para(&mut head, rel..whole, text, runs);
            let mut tail = self.para(last).clone();
            edit_para(&mut tail, 0..(range.end - last.start) as u32, "", &[]);
            let mut joined = head.text.into_string();
            joined.push_str(tail.text());
            let mut r = head.runs;
            tail.runs.iter().for_each(|&t| push_run(&mut r, t));
            (joined, r)
        };
        let mut count = last.para + 1 - first.para;
        // The joined text must end in a separator (or the text): take in
        // the paragraphs after it until it does, and a "\n" after a "\r".
        let mut after = self.next(last);
        while let Some(a) = after {
            let p = self.para(a);
            let ends = separator_at_end(&joined).0 > 0;
            if ends && !(joined.ends_with('\r') && p.text().starts_with('\n')) {
                break;
            }
            joined.push_str(p.text());
            p.runs.iter().for_each(|&r| push_run(&mut jruns, r));
            count += 1;
            after = self.next(a);
        }
        let new = self.cut(joined.into(), &jruns, after.is_none());
        self.splice(first, count, new);
    }

    /// Replace the `count` paragraphs from `first` with `new`.
    fn splice(&mut self, first: At, count: usize, new: New) {
        let (c0, s0) = (first.chunk, first.slot);
        let n0 = self.chunks[c0].count();
        // Take the paragraphs out: chunk c0 keeps those before them, `tail`
        // takes those after them in the chunk where they end, and the
        // chunks in `gone` go (whole chunks between aren't cut to go).
        let mut tail = Vec::new();
        let mut gone = c0 + 1..c0 + 1;
        if s0 + count <= n0 {
            let paras = self.chunks[c0].paras_mut();
            tail = paras.split_off(s0 + count);
            paras.truncate(s0);
        } else {
            self.chunks[c0].paras_mut().truncate(s0);
            let mut left = count - (n0 - s0);
            let mut c = c0 + 1;
            while left > 0 && left >= self.chunks[c].count() {
                left -= self.chunks[c].count();
                c += 1;
            }
            if left > 0 {
                tail = self.chunks[c].paras_mut().split_off(left);
                c += 1;
            }
            gone = c0 + 1..c;
        }
        match new {
            New::Paras(paras) => {
                let kept = self.chunks[c0].paras_mut();
                kept.extend(paras);
                kept.extend(tail);
                self.chunks.drain(gone);
                self.rebalance(c0);
            }
            New::Chunks(mut chunks) => {
                let k = chunks.len();
                let has_tail = !tail.is_empty();
                if has_tail {
                    chunks.push(Chunk::built(tail));
                }
                self.chunks.splice(gone, chunks);
                // The paragraphs around the new chunks, in chunks of their
                // own: the one after first, so c0 stays where it is.
                if has_tail {
                    self.rebalance(c0 + 1 + k);
                }
                self.rebalance(c0);
            }
        }
        self.restart(c0.saturating_sub(1));
    }

    /// Keep chunk `c` between the sizes allowed: split one that grew, join
    /// one that shrank with the next, drop an empty one.
    fn rebalance(&mut self, c: usize) {
        let len = self.chunks[c].count_now();
        if len > MAX_CHUNK {
            let paras = std::mem::take(self.chunks[c].paras_mut());
            let pieces = len.div_ceil(MAX_CHUNK / 2);
            let size = len.div_ceil(pieces);
            let mut it = paras.into_iter();
            let chunks: Vec<Chunk> = (0..pieces).map(|_| Chunk::built(it.by_ref().take(size).collect())).collect();
            self.chunks.splice(c..=c, chunks.into_iter().filter(|c| c.count() > 0));
            return;
        }
        if len == 0 && self.chunks.len() > 1 {
            self.chunks.remove(c);
            return;
        }
        if len < MIN_CHUNK && c + 1 < self.chunks.len() && len + self.chunks[c + 1].count() <= MAX_CHUNK {
            let next = self.chunks.remove(c + 1).into_paras();
            self.chunks[c].paras_mut().extend(next);
        }
        self.chunks[c].resum();
    }

    /// Give `range` the attributes `attrs`.
    pub fn set_attrs(&mut self, range: Range<usize>, attrs: AttrId) {
        assert!(range.start <= range.end && range.end <= self.len(), "sidestep: attributes outside the text");
        if range.is_empty() {
            return;
        }
        let chunks: Vec<(usize, usize)> = self.chunks_over(range.clone()).collect();
        for (c, start) in chunks {
            let chunk = &mut self.chunks[c];
            let from = range.start.saturating_sub(start);
            let to = (range.end - start).min(chunk.sum.u16);
            if from >= to {
                continue;
            }
            let run = [Run { len: (to - from) as u32, attrs }];
            if chunk.uncut().is_some() {
                // A raw chunk's runs, left uncut.
                let raw = chunk.raw.as_mut().expect("uncut");
                splice_runs(&mut raw.runs, from as u32..to as u32, &run);
            } else {
                let mut at = 0;
                for p in chunk.paras_mut() {
                    let len = p.len16 as usize;
                    let (a, b) = (from.max(at), to.min(at + len));
                    if a < b {
                        splice_runs(
                            &mut p.runs,
                            (a - at) as u32..(b - at) as u32,
                            &[Run { len: (b - a) as u32, attrs }],
                        );
                    }
                    at += len;
                }
            }
            chunk.resum();
        }
    }

    /// Map every run's attributes through `f` (after the attribute table
    /// has been compacted).
    pub fn remap(&mut self, mut f: impl FnMut(AttrId) -> AttrId) {
        let mut map = |runs: &mut Runs| {
            for r in runs.iter_mut() {
                r.attrs = f(r.attrs);
            }
            // Runs that differed may now be equal.
            let old = std::mem::take(runs);
            old.into_iter().for_each(|r| push_run(runs, r));
        };
        for c in &mut self.chunks {
            // Renumbered, the attributes are as fixed as they were.
            let fixed = c.fixed;
            if c.uncut().is_some() {
                map(&mut c.raw.as_mut().expect("uncut").runs);
            } else {
                c.raw = None;
                c.paras_mut().iter_mut().for_each(|p| map(&mut p.runs));
            }
            c.resum();
            c.fixed = fixed;
        }
    }

    /// Every attribute in use.
    pub fn for_each_attrs(&self, mut f: impl FnMut(AttrId)) {
        for c in &self.chunks {
            match c.uncut() {
                Some(raw) => raw.runs.iter().for_each(|r| f(r.attrs)),
                None => c.paras().iter().flat_map(|p| p.runs.iter()).for_each(|r| f(r.attrs)),
            }
        }
    }
}

/// Edit a paragraph's text and runs: `range` (UTF-16, from its start)
/// becomes `text` with the runs `runs`. A surrogate pair the range cuts
/// leaves U+FFFD for the half outside it.
fn edit_para(p: &mut Para, range: Range<u32>, text: &str, runs: &[Run]) {
    let (b0, cut0) = p.byte_of(range.start);
    let (b1, cut1) = p.byte_of(range.end);
    let inserted = runs.iter().map(|r| r.len).sum::<u32>();
    let long = cut0 || cut1;
    let s = p.text.to_mut();
    if long {
        let mut t = String::with_capacity(text.len() + 6);
        if cut0 {
            t.push('\u{FFFD}');
        }
        t.push_str(text);
        if cut1 {
            t.push('\u{FFFD}');
        }
        // The cut characters go whole, and their outside halves come back
        // as U+FFFD.
        let end = if cut1 { b1 + 4 } else { b1 };
        s.replace_range(b0..end, &t);
    } else {
        s.replace_range(b0..b1, text);
    }
    let old_len = range.end - range.start;
    p.len16 = p.len16 - old_len + inserted;
    // The replacement halves keep the attributes they had: only the
    // range's units change.
    splice_runs(&mut p.runs, range, runs);
    let text_now = p.text.as_str();
    let (sep8, sep16) = separator_at_end(text_now);
    (p.sep8, p.sep16) = (sep8, sep16);
    p.ascii = if p.ascii { text.is_ascii() && !long } else { text_now.len() < LONG && text_now.is_ascii() };
    p.index = OnceCell::new();
    debug_assert_eq!(p.len16, utf16_len(p.text()));
}

/// Cut `text` (with `runs` covering it) into paragraphs of their own. With
/// `last`, the text ends the storage, so what follows its last separator
/// (perhaps nothing) is a paragraph too; otherwise the text ends in a
/// separator.
fn split(text: &str, runs: &[Run], last: bool) -> Vec<Para> {
    paragraphs(text, runs, last, false, |r| Text::Own(text[r].to_string()))
}

/// Cut `src` (with `runs` covering it) into paragraphs, each with the text
/// `text` makes of its bytes. With `last`, what follows the last separator
/// (perhaps nothing) is a paragraph too; otherwise `src` ends in a
/// separator. `ascii`: `src` is ASCII, so units are bytes.
fn paragraphs(
    src: &str,
    runs: &[Run],
    last: bool,
    ascii: bool,
    mut text: impl FnMut(Range<usize>) -> Text,
) -> Vec<Para> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut runs = RunCursor::new(runs);
    let mut piece = |r: Range<usize>| {
        let len = if ascii { r.len() as u32 } else { utf16_len(&src[r.clone()]) };
        Para::with_len(text(r), runs.take(len), len)
    };
    let mut pos = 0;
    while let Some(end) = separator_end(b, pos) {
        out.push(piece(pos..end));
        pos = end;
    }
    if last {
        out.push(piece(pos..b.len()));
    } else {
        debug_assert_eq!(pos, b.len(), "sidestep: paragraphs cut short of a separator");
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

    /// The naive model: UTF-16 units and an attribute per unit, and for
    /// each unit marked fixed, its attributes and its paragraph's first
    /// ones as they were then.
    #[derive(Default)]
    struct Model {
        units: Vec<u16>,
        attrs: Vec<AttrId>,
        marked: Vec<Option<(AttrId, AttrId)>>,
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
            self.attrs.splice(range.clone(), std::iter::repeat_n(attrs, n));
            self.marked.splice(range, std::iter::repeat_n(None, n));
        }

        /// What fixing reads of each unit: its attributes and its
        /// paragraph's first ones.
        fn fixing_reads(&self) -> Vec<(AttrId, AttrId)> {
            let paras = self.paragraphs();
            paras.iter().flat_map(|p| p.clone().map(|u| (self.attrs[u], self.attrs[p.start]))).collect()
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
            for p in c.paras() {
                assert!(p.runs.windows(2).all(|w| w[0].attrs != w[1].attrs), "runs merge");
                assert!(p.runs.iter().all(|r| r.len > 0));
            }
            assert!(!c.paras().is_empty() && c.paras().len() <= MAX_CHUNK);
            assert_eq!(c.count(), c.paras().len());
        }
        // Units one at a time, sampled.
        for i in (0..m.units.len()).step_by(7) {
            assert_eq!(s.unit_at(i), m.units[i]);
        }
    }

    const PIECES: &[&str] = &[
        "a",
        "bc",
        "hello",
        " ",
        "\n",
        "\r",
        "\r\n",
        "\u{2029}",
        "é",
        "日本",
        "😀",
        "x😀y",
        "\n\n",
        "ab\ncd",
        "\r\r\n",
        "",
        "q\u{2029}",
        "long line of text ",
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

    impl Storage {
        /// An empty text that takes text of `n` bytes or more in raw.
        fn raw_from(n: usize) -> Storage {
            Storage { raw_min: n, ..Storage::new() }
        }

        fn uncut_chunks(&self) -> usize {
            self.chunks.iter().filter(|c| c.uncut().is_some()).count()
        }
    }

    /// What reads a raw chunk as it is, on the storage as it is (a clone,
    /// so the storage's chunks stay uncut): the text, its pieces, its runs,
    /// the runs with their paragraphs' first attributes, and the extents.
    fn check_uncut(s: &Storage, m: &Model, rng: &mut Rng) {
        let s = s.clone();
        assert_eq!(s.len(), m.units.len());
        assert_eq!(s.string(), m.string());
        assert_eq!(s.len8(), m.string().len());
        let paras = m.paragraphs();
        assert_eq!(s.paragraph_count(), paras.len());
        let mut attrs = Vec::new();
        s.for_each_run(0..s.len(), |r, a| {
            assert_eq!(r.start, attrs.len());
            attrs.extend(std::iter::repeat_n(a, r.len()));
        });
        assert_eq!(attrs, m.attrs);
        let start_of: Vec<usize> = paras.iter().flat_map(|p| std::iter::repeat_n(p.start, p.len())).collect();
        // What is left out was marked fixed, and what fixing reads of it
        // is as it was then.
        let reads = m.fixing_reads();
        let unchanged = |r: Range<usize>| r.clone().all(|u| m.marked[u] == Some(reads[u]));
        let mut at = 0;
        s.for_each_run_with_first(0..s.len(), |r, a, first| {
            assert!(r.start >= at && unchanged(at..r.start), "{at}..{} left out", r.start);
            at = r.end;
            assert!(m.attrs[r.clone()].iter().all(|&x| x == a));
            // Each unit's paragraph starts with `first`.
            assert!(r.into_iter().all(|u| m.attrs[start_of[u]] == first));
        });
        assert!(unchanged(at..s.len()), "{at}.. left out");
        // The runs walked each way from a point, a few or all of them.
        for _ in 0..3 {
            let from = rng.below(m.units.len() + 1);
            let most = rng.below(6) + 1;
            let (mut at, mut n) = (from, 0);
            s.runs_after(from, |r, a| {
                assert_eq!(r.start, at);
                assert!(!r.is_empty() && m.attrs[r.clone()].iter().all(|&x| x == a));
                at = r.end;
                n += 1;
                n < most
            });
            assert!(n == most || at == m.units.len(), "runs after {from} end at {at}");
            let (mut at, mut n) = (from, 0);
            s.runs_before(from, |r, a| {
                assert_eq!(r.end, at);
                assert!(!r.is_empty() && m.attrs[r.clone()].iter().all(|&x| x == a));
                at = r.start;
                n += 1;
                n < most
            });
            assert!(n == most || at == 0, "runs before {from} start at {at}");
        }
        let (mut count, mut len16) = (0, 0);
        s.for_each_extent(0..s.paragraph_count(), |e| match e {
            Extent::One { len16: l, attrs } => {
                assert_eq!(attrs, (l > 0).then(|| m.attrs[paras[count].start]));
                count += 1;
                len16 += l as usize;
            }
            Extent::Many { count: n, len16: l, attrs } => {
                assert_eq!(attrs, Some(m.attrs[paras[count].start]));
                count += n;
                len16 += l;
            }
        });
        assert_eq!((count, len16), (paras.len(), s.len()));
        for _ in 0..4 {
            let a = rng.below(m.units.len() + 1);
            let b = (a + rng.below(3000)).min(m.units.len());
            assert_eq!(s.text(a..b), String::from_utf16_lossy(&m.units[a..b]), "text of {a}..{b}");
        }
    }

    const LINES: &[&str] = &[
        "one line\n",
        "é😀 and so on\r\n",
        "q\u{2029}",
        "\n",
        "a longer line of text, fifty or so bytes long\n",
        "\r",
    ];

    fn text_of(rng: &mut Rng, units: usize) -> String {
        let mut t = String::new();
        while t.len() < units {
            t.push_str(LINES[rng.below(LINES.len())]);
        }
        t
    }

    /// Edits of text taken in raw, against the model: whole texts, long
    /// pastes (raw chunks among cut ones), small edits, and attributes set
    /// over raw and cut chunks alike.
    fn raw_edits(seed: u64, steps: usize) {
        let mut rng = Rng(seed);
        let mut s = Storage::raw_from(64);
        let mut m = Model::default();
        let first = text_of(&mut rng, 20_000);
        s.replace(0..0, &first, 1);
        m.replace(0..0, &first, 1);
        assert!(s.uncut_chunks() > 3, "long text goes in raw");
        for step in 0..steps {
            let len = m.units.len();
            match rng.below(12) {
                0 => {
                    // All of it, in runs.
                    let text = text_of(&mut rng, 20_000);
                    let n = utf16_len(&text);
                    let mut runs = Vec::new();
                    let mut left = n;
                    while left > 0 {
                        let l = (rng.below(40) as u32 + 1).min(left);
                        runs.push(Run { len: l, attrs: rng.below(3) as AttrId });
                        left -= l;
                    }
                    // Not in the middle of a character.
                    let units: Vec<u16> = text.encode_utf16().collect();
                    let mut fixed = Vec::new();
                    let mut at = 0usize;
                    for r in runs {
                        let mut end = at + r.len as usize;
                        if end < units.len() && (0xDC00..0xE000).contains(&units[end]) {
                            end += 1;
                        }
                        let end = end.min(units.len());
                        if end > at {
                            fixed.push(Run { len: (end - at) as u32, attrs: r.attrs });
                        }
                        at = end;
                    }
                    s.replace_runs(0..len, &text, &fixed);
                    m.marked = vec![None; units.len()];
                    m.units = units;
                    m.attrs = fixed.iter().flat_map(|r| std::iter::repeat_n(r.attrs, r.len as usize)).collect();
                }
                1..=2 => {
                    // A long paste.
                    let start = rng.below(len + 1);
                    let end = (start + rng.below(300)).min(len);
                    let size = 200 + rng.below(800);
                    let text = text_of(&mut rng, size);
                    s.replace(start..end, &text, 2);
                    m.replace(start..end, &text, 2);
                }
                3..=6 => {
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
                7 => {
                    // A long deletion.
                    let start = rng.below(len + 1);
                    let end = (start + rng.below(2000)).min(len);
                    s.replace(start..end, "", 0);
                    m.replace(start..end, "", 0);
                }
                8..=9 => {
                    let start = rng.below(len + 1);
                    let end = (start + rng.below(1500)).min(len);
                    let attrs = rng.below(3) as AttrId;
                    s.set_attrs(start..end, attrs);
                    m.attrs[start..end].iter_mut().for_each(|a| *a = attrs);
                }
                _ => {
                    // Fixing notes what it fixed.
                    let start = rng.below(len + 1);
                    let end = (start + rng.below(20_000)).min(len);
                    s.mark_fixed(start..end);
                    let reads = m.fixing_reads();
                    (start..end).for_each(|u| m.marked[u] = Some(reads[u]));
                }
            }
            check_uncut(&s, &m, &mut rng);
            if step % 5 == 0 {
                check(&s.clone(), &m);
            }
            // Now and then something reads a paragraph, cutting its chunk.
            if rng.below(3) == 0 && !m.units.is_empty() {
                let _ = s.locate(rng.below(m.units.len()));
            }
        }
        check(&s, &m);
    }

    #[test]
    fn raw_edits_match_the_naive_model() {
        for seed in 1..7 {
            raw_edits(seed * 0x5851_F42D, 80);
        }
    }

    #[test]
    fn random_edits_through_raw_chunks() {
        for seed in 1..20 {
            let mut rng = Rng(seed * 0x9E37_79B9);
            let mut s = Storage::raw_from(0);
            let mut m = Model::default();
            for _ in 0..200 {
                let len = m.units.len();
                let start = rng.below(len + 1);
                let end = (start + rng.below(6)).min(len);
                let mut text = String::new();
                for _ in 0..rng.below(4) {
                    text.push_str(PIECES[rng.below(PIECES.len())]);
                }
                let attrs = rng.below(3) as AttrId;
                s.replace(start..end, &text, attrs);
                m.replace(start..end, &text, attrs);
                check_uncut(&s, &m, &mut rng);
                check(&s.clone(), &m);
            }
        }
    }

    /// Runs marked fixed are left out of fixing until their chunk changes;
    /// renumbering the attributes keeps the marks.
    #[test]
    fn marked_fixed_is_left_out_until_changed() {
        let text: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let mut s = Storage::with_text(&text, 1);
        s.set_attrs(10..20, 2);
        let len = s.len();
        let seen = |s: &Storage, r: Range<usize>| {
            let mut n = 0;
            s.for_each_run_with_first(r, |r, _, _| n += r.len());
            n
        };
        assert_eq!(seen(&s, 0..len), len);
        s.mark_fixed(0..30_000);
        assert_eq!(seen(&s, 0..30_000), 0);
        assert_eq!(seen(&s, 0..len), len - 30_000);
        // A mark starting past what is fixed of a chunk doesn't join it.
        let c = s.chunk_at(40_000);
        let (start, end) = (s.starts[c].u16, s.starts[c].u16 + s.chunks[c].sum.u16);
        s.mark_fixed(start + 5..end);
        assert_eq!(seen(&s, start..end), end - start);
        s.mark_fixed(start..start + 10);
        s.mark_fixed(start + 5..end);
        assert_eq!(seen(&s, start..end), 0);
        // An edit makes its chunk's none fixed, not the others'.
        s.replace(100..101, "x", 1);
        let c = s.chunk_at(100);
        let edited = s.starts[c].u16..s.starts[c].u16 + s.chunks[c].sum.u16;
        assert_eq!(seen(&s, 0..30_000), edited.len());
        s.set_attrs(20_000..20_001, 3);
        let c = s.chunk_at(20_000);
        assert_eq!(seen(&s, 0..30_000), edited.len() + s.chunks[c].sum.u16);
        s.mark_fixed(0..30_000);
        s.remap(|id| id + 10);
        assert_eq!(seen(&s, 0..30_000), 0);
    }

    /// Deleting most of a long text lets go of the shared text it came in:
    /// what is left copies what it slices.
    #[test]
    fn deleting_most_of_a_long_text_lets_it_go() {
        let text: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        let mut s = Storage::with_text(&text, 1);
        let mut m = Model::default();
        m.replace(0..0, &text, 1);
        // A chunk cut into paragraphs slicing the text, and raw ones.
        let _ = s.locate(s.len() - 20);
        let _ = s.locate(s.len() / 2);
        assert_eq!(s.shared_bytes(), text.len());
        // Some of it deleted, bit by bit: the text still slices it.
        let stop = s.len() * 6 / 10;
        while s.len() > stop {
            let at = s.len() / 3;
            s.replace(at..at + 8000, "", 1);
            m.replace(at..at + 8000, "", 1);
        }
        assert_eq!(s.shared_bytes(), text.len());
        check(&s.clone(), &m);
        let keep = s.len() - 30;
        s.replace(10..keep, "", 1);
        m.replace(10..keep, "", 1);
        assert!(s.shared_bytes() <= 2 * s.len8() + s.raw_min, "{} bytes kept for {}", s.shared_bytes(), s.len8());
        check(&s, &m);
    }

    #[test]
    fn long_text_is_cut_when_read() {
        let text: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        let s = Storage::with_text(&text, 3);
        let n = s.chunks.len();
        assert_eq!(s.uncut_chunks(), n);
        assert!(s.chunks.iter().all(|c| c.count() <= MAX_CHUNK));
        assert_eq!(s.paragraph_count(), 20_001);
        // Reading a paragraph cuts its chunk alone.
        let at = s.locate(s.len() / 2);
        assert_eq!(s.para(at).text(), format!("line {}\n", at.para));
        assert_eq!(s.uncut_chunks(), n - 1);
        // The runs and the whole text read the rest as it is.
        assert_eq!(s.attrs_at(10, true), (3, 0..s.len()));
        assert_eq!(s.string(), text);
        assert!(s.uncut_chunks() >= n - 3);
    }

    #[test]
    fn counting_matches_the_plain_ways() {
        let mut rng = Rng(5);
        let alphabet: &[&str] = &["a", "\n", "\r", "\r\n", "\u{2029}", "é", "😀", "日", "\u{2028}", "\u{2020}"];
        for _ in 0..300 {
            let s: String = (0..rng.below(200)).map(|_| alphabet[rng.below(alphabet.len())]).collect();
            assert_eq!(utf16_len(&s) as usize, s.encode_utf16().count());
            let mut expected = 0;
            let mut rest = s.as_str();
            while let Some(i) = rest.find(['\n', '\r', '\u{2029}']) {
                expected += 1;
                let step = if rest[i..].starts_with("\r\n") { 2 } else { rest[i..].chars().next().unwrap().len_utf8() };
                rest = &rest[i + step..];
            }
            assert_eq!(count_separators(s.as_bytes()), expected, "{s:?}");
        }
    }
}
