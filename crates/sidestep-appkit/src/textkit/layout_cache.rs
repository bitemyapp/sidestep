//! What a layout manager knows of each paragraph: its lines, once laid
//! out, or an estimate of its height until then.
//!
//! The entries parallel the text storage's paragraphs, in chunks of at most
//! [`MAX_CHUNK`] with each chunk's total height, the left and right edges
//! of its used rects and count of paragraphs laid out. The tops of the
//! chunks are summed lazily: an update marks the sums stale from its chunk
//! on, and the next question about a position sums them again, so laying
//! out a thousand paragraphs in a row costs one pass over the chunks, not
//! a thousand. How far from the start every paragraph is laid out is
//! remembered too (lowered by each change), so asking whether the
//! paragraphs up to one are laid out, as contiguous layout does before
//! every question about a line, costs nothing once they are.
//!
//! Paragraphs not laid out may be estimated alike, many at a time: a chunk
//! can hold a number of paragraphs and one entry for all of them (text
//! taken in whole, which the text storage hasn't cut into paragraphs yet,
//! is estimated so, a stretch of the storage at a time). Positions in such
//! a chunk are multiples of its entry's extent, and setting an entry in it
//! gives the paragraphs around it (half a chunk's worth) entries of their
//! own, so laying out goes on from there as through any other chunk.
//!
//! A paragraph's extent down the page is the spacing before it (none for
//! the first paragraph, as string drawing places text), its lines, and the
//! line spacing and paragraph spacing after it; the text's height leaves
//! out the last paragraph's trailing spacing. A paragraph among text blocks
//! has a place (`blocks::Place`) that says how far it moves the text below
//! down and where its top is from its flow top (the extents above it):
//! the paragraphs of a table row share their flow top, and all but the
//! row's last move nothing down.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use super::blocks::Place;
use crate::text::lines::ParagraphLines;

const MAX_CHUNK: usize = 128;

/// As many entries as a chunk made at once holds.
const HALF: usize = MAX_CHUNK / 2;

/// A paragraph's layout.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// Spacing before the paragraph (0 for the first), its lines' height,
    /// and the spacing after it.
    pub lead: f32,
    pub height: f32,
    pub trail: f32,
    /// The left and right edges of its lines' used rects, from the
    /// container's left (padding included): nothing (infinity, 0) until
    /// laid out.
    pub left: f32,
    pub right: f32,
    /// The lines, once laid out.
    pub lines: Option<Arc<ParagraphLines>>,
    /// Where it goes among text blocks, when it is in any.
    pub place: Option<Arc<Place>>,
}

impl Entry {
    pub fn estimate(height: f32) -> Entry {
        Entry { lead: 0.0, height, trail: 0.0, left: f32::INFINITY, right: 0.0, lines: None, place: None }
    }

    /// How far it moves the text below down.
    pub fn extent(&self) -> f64 {
        match &self.place {
            Some(p) => f64::from(p.advance),
            None => f64::from(self.lead) + f64::from(self.height) + f64::from(self.trail),
        }
    }

    /// From the flow top to the top of the paragraph's first line.
    fn offset(&self) -> f64 {
        self.place.as_ref().map_or(0.0, |p| f64::from(p.pre)) + f64::from(self.lead)
    }

    /// What the text's height leaves out when this paragraph is last.
    fn tail(&self) -> f64 {
        self.place.as_ref().map_or(f64::from(self.trail), |p| f64::from(p.tail))
    }

    /// In a table row, but not its last paragraph.
    fn continues_row(&self) -> bool {
        self.place.as_ref().is_some_and(|p| p.row && !p.row_end)
    }

    pub fn is_laid(&self) -> bool {
        self.lines.is_some()
    }

    /// Whether it is an estimate just like `other`.
    fn same_estimate(&self, other: &Entry) -> bool {
        !self.is_laid()
            && !other.is_laid()
            && self.place.is_none()
            && other.place.is_none()
            && (self.lead, self.height, self.trail) == (other.lead, other.height, other.trail)
    }
}

/// Entries to put in: a paragraph's (`count` 1), or `count` paragraphs'
/// estimated alike.
#[derive(Clone, Debug)]
pub(crate) struct Piece {
    pub count: usize,
    pub entry: Entry,
}

impl Piece {
    pub fn one(entry: Entry) -> Piece {
        Piece { count: 1, entry }
    }
}

#[derive(Debug)]
enum Body {
    Each(Vec<Entry>),
    /// Paragraphs estimated alike: how many, and the entry each has.
    Alike(usize, Entry),
}

#[derive(Debug)]
struct Chunk {
    body: Body,
    extent: f64,
    left: f32,
    right: f32,
    laid: usize,
}

impl Chunk {
    fn each(entries: Vec<Entry>) -> Chunk {
        Chunk::with(Body::Each(entries))
    }

    fn with(body: Body) -> Chunk {
        let mut c = Chunk { body, extent: 0.0, left: f32::INFINITY, right: 0.0, laid: 0 };
        c.resum();
        c
    }

    fn len(&self) -> usize {
        match &self.body {
            Body::Each(v) => v.len(),
            Body::Alike(n, _) => *n,
        }
    }

    fn get(&self, i: usize) -> &Entry {
        match &self.body {
            Body::Each(v) => &v[i],
            Body::Alike(_, e) => e,
        }
    }

    /// The extents of the entries before entry `i`.
    fn before(&self, i: usize) -> f64 {
        match &self.body {
            Body::Each(v) => v[..i].iter().map(Entry::extent).sum(),
            Body::Alike(_, e) => i as f64 * e.extent(),
        }
    }

    /// The first entry from `from` on that isn't laid out.
    fn first_unlaid(&self, from: usize) -> Option<usize> {
        match &self.body {
            Body::Each(v) => v[from..].iter().position(|e| !e.is_laid()).map(|i| from + i),
            Body::Alike(n, e) => (from < *n && !e.is_laid()).then_some(from),
        }
    }

    fn resum(&mut self) {
        match &self.body {
            Body::Each(v) => {
                self.extent = v.iter().map(Entry::extent).sum();
                self.left = v.iter().map(|e| e.left).fold(f32::INFINITY, f32::min);
                self.right = v.iter().map(|e| e.right).fold(0.0, f32::max);
                self.laid = v.iter().filter(|e| e.is_laid()).count();
            }
            Body::Alike(n, e) => {
                self.extent = *n as f64 * e.extent();
                (self.left, self.right) = if *n > 0 { (e.left, e.right) } else { (f32::INFINITY, 0.0) };
                self.laid = if e.is_laid() { *n } else { 0 };
            }
        }
    }

    /// Cut it in two before entry `i` (0 < `i` < its length): the second
    /// part.
    fn split_off(&mut self, i: usize) -> Chunk {
        let rest = match &mut self.body {
            Body::Each(v) => Body::Each(v.split_off(i)),
            Body::Alike(n, e) => {
                let rest = Body::Alike(*n - i, e.clone());
                *n = i;
                rest
            }
        };
        self.resum();
        Chunk::with(rest)
    }
}

/// Where a paragraph's entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slot {
    chunk: usize,
    index: usize,
    pub para: usize,
}

#[derive(Debug, Default)]
pub(crate) struct LayoutCache {
    chunks: Vec<Chunk>,
    /// Per chunk: the paragraphs before it, and (lazily) the extent before
    /// it, valid for chunks before `stale`.
    counts: Vec<usize>,
    tops: RefCell<Vec<f64>>,
    stale: Cell<usize>,
    /// Every paragraph before this one is laid out.
    laid_prefix: Cell<usize>,
}

impl LayoutCache {
    /// Entries for the paragraphs `pieces` give.
    pub fn new(pieces: Vec<Piece>) -> LayoutCache {
        let mut c = LayoutCache { chunks: into_chunks(pieces), ..LayoutCache::default() };
        if c.chunks.is_empty() {
            c.chunks.push(Chunk::each(Vec::new()));
        }
        c.recount(0);
        c
    }

    pub fn len(&self) -> usize {
        let last = self.chunks.len() - 1;
        self.counts[last] + self.chunks[last].len()
    }

    /// The left and right edges of the used rects, over the paragraphs
    /// laid out: (infinity, 0) when none is.
    pub fn used_x(&self) -> (f32, f32) {
        let left = self.chunks.iter().map(|c| c.left).fold(f32::INFINITY, f32::min);
        (left, self.chunks.iter().map(|c| c.right).fold(0.0, f32::max))
    }

    fn recount(&mut self, from: usize) {
        let from = from.min(self.chunks.len());
        self.counts.truncate(from);
        let mut n = match from.checked_sub(1) {
            Some(p) => self.counts[p] + self.chunks[p].len(),
            None => 0,
        };
        for c in &self.chunks[from..] {
            self.counts.push(n);
            n += c.len();
        }
        self.stale.set(self.stale.get().min(from));
    }

    /// The extents before each chunk, summed again where stale.
    fn tops(&self) -> std::cell::Ref<'_, Vec<f64>> {
        let stale = self.stale.get();
        if stale < self.chunks.len() || self.tops.borrow().len() != self.chunks.len() {
            let mut tops = self.tops.borrow_mut();
            let from = stale.min(tops.len());
            tops.truncate(from);
            let mut y = match from.checked_sub(1) {
                Some(p) => tops[p] + self.chunks[p].extent,
                None => 0.0,
            };
            for c in &self.chunks[from..] {
                tops.push(y);
                y += c.extent;
            }
            self.stale.set(self.chunks.len());
        }
        self.tops.borrow()
    }

    pub fn slot(&self, para: usize) -> Slot {
        let para = para.min(self.len().saturating_sub(1));
        let chunk = self.counts.partition_point(|&n| n <= para).saturating_sub(1);
        Slot { chunk, index: para - self.counts[chunk], para }
    }

    pub fn get(&self, para: usize) -> &Entry {
        let s = self.slot(para);
        self.chunks[s.chunk].get(s.index)
    }

    /// Replace paragraph `para`'s entry.
    pub fn set(&mut self, para: usize, entry: Entry) {
        if !entry.is_laid() {
            self.lower_prefix(para);
        }
        let s = self.open(self.slot(para));
        let chunk = &mut self.chunks[s.chunk];
        if let Body::Each(v) = &mut chunk.body {
            v[s.index] = entry;
        }
        chunk.resum();
        self.stale.set(self.stale.get().min(s.chunk + 1));
    }

    /// Give the paragraph at `s` an entry of its own, and the paragraphs
    /// around it (in a chunk of estimates alike, half a chunk's worth): where
    /// it is now.
    fn open(&mut self, s: Slot) -> Slot {
        let n = self.chunks[s.chunk].len();
        if matches!(self.chunks[s.chunk].body, Body::Each(_)) {
            return s;
        }
        let i0 = s.index / HALF * HALF;
        let w = HALF.min(n - i0);
        let mut c = s.chunk;
        if i0 > 0 {
            self.cut_chunk(c, i0);
            c += 1;
        }
        if w < n - i0 {
            self.cut_chunk(c, w);
        }
        let chunk = &mut self.chunks[c];
        let entry = chunk.get(0).clone();
        chunk.body = Body::Each(vec![entry; w]);
        chunk.resum();
        Slot { chunk: c, index: s.index - i0, para: s.para }
    }

    /// Cut chunk `c` in two before its entry `i` (0 < `i` < its length).
    fn cut_chunk(&mut self, c: usize, i: usize) {
        let rest = self.chunks[c].split_off(i);
        self.chunks.insert(c + 1, rest);
        self.counts.insert(c + 1, self.counts[c] + i);
        let mut tops = self.tops.borrow_mut();
        tops.truncate(c + 1);
        self.stale.set(self.stale.get().min(c + 1));
    }

    /// A chunk boundary at paragraph `para` (cutting the chunk holding it):
    /// the index of the chunk that starts there (the number of chunks, at
    /// the end).
    fn boundary(&mut self, para: usize) -> usize {
        if para >= self.len() {
            return self.chunks.len();
        }
        let s = self.slot(para);
        if s.index == 0 {
            return s.chunk;
        }
        self.cut_chunk(s.chunk, s.index);
        s.chunk + 1
    }

    /// Replace the `count` entries from paragraph `first` with those
    /// `new` gives.
    pub fn splice(&mut self, first: usize, count: usize, new: Vec<Piece>) {
        self.lower_prefix(first);
        let first = first.min(self.len());
        if new.iter().all(|p| p.count == 1) && self.splice_within(first, count, &new) {
            return;
        }
        let a = self.boundary(first);
        let b = self.boundary(first + count);
        let chunks = into_chunks(new);
        let k = chunks.len();
        self.chunks.splice(a..b, chunks);
        // Join the chunks at the seams where they are small enough, and
        // drop empty ones.
        let mut join = |c: usize| {
            if c + 1 >= self.chunks.len() {
                return;
            }
            let (l, r) = (self.chunks[c].len(), self.chunks[c + 1].len());
            if r == 0 {
                self.chunks.remove(c + 1);
            } else if l == 0 {
                self.chunks.remove(c);
            } else if l + r <= MAX_CHUNK
                && let (Body::Each(_), Body::Each(_)) = (&self.chunks[c].body, &self.chunks[c + 1].body)
            {
                let next = self.chunks.remove(c + 1);
                if let (Body::Each(v), Body::Each(w)) = (&mut self.chunks[c].body, next.body) {
                    v.extend(w);
                }
                self.chunks[c].resum();
            }
        };
        if k > 0 {
            join(a + k - 1);
        }
        if a > 0 {
            join(a - 1);
        }
        if self.chunks.len() > 1 && self.chunks.last().is_some_and(|c| c.len() == 0) {
            self.chunks.pop();
        }
        if self.chunks.is_empty() {
            self.chunks.push(Chunk::each(Vec::new()));
        }
        let from = a.saturating_sub(1);
        self.tops.borrow_mut().truncate(from);
        self.recount(from);
    }

    /// [`splice`](Self::splice) of single entries inside one chunk of
    /// entries (an edit's few paragraphs, as typing makes): done in place,
    /// the chunks left where they are. Whether it could be.
    fn splice_within(&mut self, first: usize, count: usize, new: &[Piece]) -> bool {
        let (c, i) = if first == self.len() {
            let last = self.chunks.len() - 1;
            (last, self.chunks[last].len())
        } else {
            let s = self.slot(first);
            (s.chunk, s.index)
        };
        let Body::Each(v) = &mut self.chunks[c].body else { return false };
        if i + count > v.len() {
            return false;
        }
        v.splice(i..i + count, new.iter().map(|p| p.entry.clone()));
        let n = v.len();
        if n > MAX_CHUNK {
            let entries = std::mem::take(v);
            self.chunks.splice(c..=c, into_chunks(entries.into_iter().map(Piece::one).collect()));
        } else if n == 0 && self.chunks.len() > 1 {
            self.chunks.remove(c);
        } else {
            self.chunks[c].resum();
        }
        self.tops.borrow_mut().truncate(c);
        self.recount(c);
        true
    }

    /// The top of paragraph `para`'s first line (after its leading
    /// spacing).
    pub fn top(&self, para: usize) -> f64 {
        if para >= self.len() {
            return self.height();
        }
        self.flow_top(para) + self.get(para).offset()
    }

    /// Where paragraph `para` starts down the text: the extents above it.
    pub fn flow_top(&self, para: usize) -> f64 {
        if para >= self.len() {
            return self.height();
        }
        let s = self.slot(para);
        let base = self.tops()[s.chunk];
        base + self.chunks[s.chunk].before(s.index)
    }

    /// The first paragraph of the table row `para` ends (itself, when it
    /// isn't a row's last).
    pub fn row_start(&self, para: usize) -> usize {
        let mut p = para.min(self.len().saturating_sub(1));
        while p > 0 && self.get(p - 1).continues_row() {
            p -= 1;
        }
        p
    }

    /// Mark paragraph `para` for layout again, keeping its extent.
    pub fn unlay(&mut self, para: usize) {
        if para < self.len() && self.get(para).is_laid() {
            let e = Entry { lines: None, ..self.get(para).clone() };
            self.set(para, e);
        }
    }

    /// The text's height: to the last paragraph's last line's bottom.
    pub fn height(&self) -> f64 {
        let last = self.chunks.len() - 1;
        let tops = self.tops();
        let chunk = &self.chunks[last];
        let total = tops[last] + chunk.extent;
        let tail = chunk.len().checked_sub(1).map_or(0.0, |i| chunk.get(i).tail());
        (total - tail).max(0.0)
    }

    /// The paragraph whose extent holds `y` (the first above the text, the
    /// last below it).
    pub fn para_at_y(&self, y: f64) -> usize {
        let tops = self.tops();
        let c = tops.partition_point(|&t| t <= y).saturating_sub(1);
        let mut top = tops[c];
        drop(tops);
        let chunk = &self.chunks[c];
        match &chunk.body {
            Body::Each(v) => {
                for (i, e) in v.iter().enumerate() {
                    top += e.extent();
                    if y < top || i + 1 == v.len() {
                        return self.counts[c] + i;
                    }
                }
                self.counts[c]
            }
            Body::Alike(n, e) => {
                let h = e.extent();
                let i = if h > 0.0 { ((y - top) / h).floor().max(0.0) as usize } else { n - 1 };
                self.counts[c] + i.min(n - 1)
            }
        }
    }

    fn lower_prefix(&self, para: usize) {
        self.laid_prefix.set(self.laid_prefix.get().min(para));
    }

    /// The first paragraph from `from` on, and before `end`, that isn't
    /// laid out.
    pub fn next_unlaid(&self, from: usize, end: usize) -> Option<usize> {
        let end = end.min(self.len());
        let prefix = self.laid_prefix.get();
        let start = if from <= prefix { prefix } else { from };
        if start >= end {
            return None;
        }
        let s = self.slot(start);
        let mut found = None;
        for (c, chunk) in self.chunks.iter().enumerate().skip(s.chunk) {
            if self.counts[c] >= end {
                break;
            }
            if chunk.laid == chunk.len() {
                continue;
            }
            let first = if c == s.chunk { s.index } else { 0 };
            if let Some(i) = chunk.first_unlaid(first) {
                found = Some(self.counts[c] + i).filter(|&p| p < end);
                break;
            }
        }
        // Everything from the prefix to what was found (or to the end
        // looked at) is laid out.
        if from <= prefix {
            self.laid_prefix.set(found.unwrap_or(end));
        }
        found
    }

    /// Entries from paragraph `from` on, with their numbers.
    #[cfg(test)]
    pub fn iter_from(&self, from: usize) -> impl Iterator<Item = (usize, &Entry)> {
        (from..self.len()).map(move |p| (p, self.get(p)))
    }
}

/// Chunks for `pieces`: single entries half a chunk at a time, and runs of
/// estimates alike as they are (joined when alike too).
fn into_chunks(pieces: Vec<Piece>) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut each: Vec<Entry> = Vec::new();
    for p in pieces {
        match p.count {
            0 => {}
            1 => {
                each.push(p.entry);
                if each.len() == HALF {
                    out.push(Chunk::each(std::mem::take(&mut each)));
                }
            }
            n => {
                if !each.is_empty() {
                    out.push(Chunk::each(std::mem::take(&mut each)));
                }
                match out.last_mut() {
                    Some(Chunk { body: Body::Alike(m, e), .. }) if e.same_estimate(&p.entry) => {
                        *m += n;
                        out.last_mut().expect("just matched").resum();
                    }
                    _ => out.push(Chunk::with(Body::Alike(n, p.entry))),
                }
            }
        }
    }
    if !each.is_empty() {
        out.push(Chunk::each(each));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn est(h: f32) -> Entry {
        Entry::estimate(h)
    }

    #[test]
    fn tops_and_heights_follow_edits() {
        let mut c = LayoutCache::new((0..1000).map(|i| Piece::one(est(10.0 + (i % 3) as f32))).collect());
        let naive = |c: &LayoutCache| {
            let mut y = 0.0;
            let mut tops = Vec::new();
            for i in 0..c.len() {
                let e = c.get(i);
                tops.push(y + f64::from(e.lead));
                y += e.extent();
            }
            tops
        };
        let check = |c: &LayoutCache| {
            let tops = naive(c);
            for (i, &t) in tops.iter().enumerate().step_by(37) {
                assert!((c.top(i) - t).abs() < 1e-6, "top of {i}");
                assert_eq!(c.para_at_y(t + 0.5), i);
            }
        };
        check(&c);
        c.set(500, Entry { lead: 2.0, height: 40.0, trail: 3.0, left: 1.0, right: 7.0, lines: None, place: None });
        check(&c);
        assert_eq!(c.used_x(), (1.0, 7.0));
        c.splice(10, 300, (0..5).map(|_| Piece::one(est(1.0))).collect());
        assert_eq!(c.len(), 705);
        check(&c);
        c.splice(700, 5, (0..400).map(|_| Piece::one(est(2.0))).collect());
        assert_eq!(c.len(), 1100);
        check(&c);
        c.splice(c.len(), 0, vec![Piece::one(est(5.0))]);
        assert_eq!(c.len(), 1101);
        check(&c);
        assert_eq!(c.next_unlaid(0, c.len()), Some(0));
        let collected: Vec<usize> = c.iter_from(1095).map(|(i, _)| i).collect();
        assert_eq!(collected, (1095..1101).collect::<Vec<_>>());
    }

    fn laid() -> Entry {
        let lines = ParagraphLines {
            lines: Vec::new(),
            len: 0,
            bytes: 0,
            complete: true,
            reach: 0.0,
            spacing: crate::text::lines::Spacing::default(),
            style: crate::text::layout::Paragraph::default(),
            rtl: false,
        };
        Entry { lines: Some(Arc::new(lines)), ..est(10.0) }
    }

    /// What `next_unlaid` finds, with the prefix it remembers, against a
    /// plain search, as paragraphs are laid out, unlaid and spliced.
    #[test]
    fn next_unlaid_follows_changes() {
        let mut c = LayoutCache::new((0..700).map(|_| Piece::one(est(10.0))).collect());
        let naive = |c: &LayoutCache, from: usize, end: usize| (from..end.min(c.len())).find(|&p| !c.get(p).is_laid());
        let check = |c: &LayoutCache| {
            for (from, end) in [(0, usize::MAX), (0, 300), (250, 260), (650, 700), (699, 700), (10, 10)] {
                assert_eq!(c.next_unlaid(from, end), naive(c, from, end), "from {from} to {end}");
            }
        };
        check(&c);
        for p in 0..400 {
            c.set(p, laid());
        }
        check(&c);
        assert_eq!(c.next_unlaid(0, 400), None);
        c.unlay(120);
        check(&c);
        c.set(120, laid());
        check(&c);
        c.splice(50, 3, [est(1.0), laid(), est(2.0), laid()].into_iter().map(Piece::one).collect());
        check(&c);
        for p in 0..c.len() {
            c.set(p, laid());
        }
        check(&c);
        c.splice(c.len(), 0, vec![Piece::one(est(5.0))]);
        check(&c);
    }

    /// Chunks of estimates alike, against a plain list of entries, through
    /// sets (which give the paragraphs around entries of their own),
    /// splices of single entries and runs of alike ones, and unlaying.
    #[test]
    fn alike_estimates_follow_edits() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut below = |n: usize| {
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) % n.max(1) as u64) as usize
        };
        let pieces = |below: &mut dyn FnMut(usize) -> usize| -> Vec<Piece> {
            (0..below(4))
                .map(|_| match below(3) {
                    0 => Piece::one(est(1.0 + below(20) as f32)),
                    1 => Piece::one(laid()),
                    _ => Piece { count: 1 + below(700), entry: est(3.0 + below(2) as f32) },
                })
                .collect()
        };
        let expand = |ps: &[Piece]| -> Vec<Entry> {
            ps.iter().flat_map(|p| std::iter::repeat_n(p.entry.clone(), p.count)).collect()
        };
        let first = vec![
            Piece { count: 1000, entry: est(10.0) },
            Piece::one(est(4.0)),
            Piece { count: 500, entry: est(20.0) },
            Piece { count: 300, entry: est(20.0) },
        ];
        let mut model = expand(&first);
        let mut c = LayoutCache::new(first);
        for step in 0..400 {
            match below(4) {
                0 => {
                    let p = below(model.len());
                    let e = if below(2) == 0 { laid() } else { est(below(30) as f32) };
                    c.set(p, e.clone());
                    model[p] = e;
                }
                1 | 2 => {
                    let at = below(model.len() + 1);
                    let n = below(900).min(model.len() - at);
                    let new = pieces(&mut below);
                    model.splice(at..at + n, expand(&new));
                    c.splice(at, n, new);
                }
                _ => {
                    let p = below(model.len().max(1));
                    c.unlay(p);
                    if let Some(e) = model.get_mut(p) {
                        e.lines = None;
                    }
                }
            }
            assert_eq!(c.len(), model.len(), "step {step}");
            let mut y = 0.0;
            let tops: Vec<f64> = model
                .iter()
                .map(|e| {
                    let t = y;
                    y += e.extent();
                    t
                })
                .collect();
            let height = (y - model.last().map_or(0.0, |e| e.tail())).max(0.0);
            assert!((c.height() - height).abs() < 1e-6, "height at step {step}");
            for _ in 0..20 {
                if model.is_empty() {
                    break;
                }
                let p = below(model.len());
                assert!((c.flow_top(p) - tops[p]).abs() < 1e-6, "top of {p} at step {step}");
                assert_eq!(c.get(p).height, model[p].height);
                assert_eq!(c.get(p).is_laid(), model[p].is_laid());
                if model[p].extent() > 0.0 {
                    assert_eq!(c.para_at_y(tops[p] + model[p].extent() / 2.0), p, "at y of {p}, step {step}");
                }
                let from = below(model.len());
                let naive = (from..model.len()).find(|&q| !model[q].is_laid());
                assert_eq!(c.next_unlaid(from, usize::MAX), naive);
            }
            assert!(c.chunks.iter().all(|ch| ch.len() > 0 || c.chunks.len() == 1));
        }
    }
}
