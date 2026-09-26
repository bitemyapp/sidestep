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
}

#[derive(Debug, Default)]
struct Chunk {
    entries: Vec<Entry>,
    extent: f64,
    left: f32,
    right: f32,
    laid: usize,
}

impl Chunk {
    fn resum(&mut self) {
        self.extent = self.entries.iter().map(Entry::extent).sum();
        self.left = self.entries.iter().map(|e| e.left).fold(f32::INFINITY, f32::min);
        self.right = self.entries.iter().map(|e| e.right).fold(0.0, f32::max);
        self.laid = self.entries.iter().filter(|e| e.is_laid()).count();
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
    /// Entries for `n` paragraphs, each estimated with `estimate(i)`.
    pub fn new(entries: Vec<Entry>) -> LayoutCache {
        let mut c = LayoutCache { chunks: into_chunks(entries), ..LayoutCache::default() };
        if c.chunks.is_empty() {
            c.chunks.push(Chunk::default());
        }
        c.recount(0);
        c
    }

    pub fn len(&self) -> usize {
        let last = self.chunks.len() - 1;
        self.counts[last] + self.chunks[last].entries.len()
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
            Some(p) => self.counts[p] + self.chunks[p].entries.len(),
            None => 0,
        };
        for c in &self.chunks[from..] {
            self.counts.push(n);
            n += c.entries.len();
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
        &self.chunks[s.chunk].entries[s.index]
    }

    /// Replace paragraph `para`'s entry.
    pub fn set(&mut self, para: usize, entry: Entry) {
        if !entry.is_laid() {
            self.lower_prefix(para);
        }
        let s = self.slot(para);
        let chunk = &mut self.chunks[s.chunk];
        chunk.entries[s.index] = entry;
        chunk.resum();
        self.stale.set(self.stale.get().min(s.chunk + 1));
    }

    /// Replace the `count` entries from paragraph `first` with `new`.
    pub fn splice(&mut self, first: usize, count: usize, new: Vec<Entry>) {
        self.lower_prefix(first);
        let s = self.slot(first.min(self.len()));
        // At the end: after the last entry.
        let (c0, i0) = if first >= self.len() {
            let last = self.chunks.len() - 1;
            (last, self.chunks[last].entries.len())
        } else {
            (s.chunk, s.index)
        };
        let mut left = count;
        let mut tail = Vec::new();
        let mut c = c0;
        let mut i = i0;
        let mut emptied = Vec::new();
        while left > 0 && c < self.chunks.len() {
            let chunk = &mut self.chunks[c];
            let take = (chunk.entries.len() - i).min(left);
            chunk.entries.drain(i..i + take);
            left -= take;
            if c != c0 {
                if left == 0 {
                    tail = std::mem::take(&mut chunk.entries);
                }
                emptied.push(c);
            }
            c += 1;
            i = 0;
        }
        let chunk = &mut self.chunks[c0];
        let rest: Vec<Entry> = chunk.entries.drain(i0..).collect();
        chunk.entries.extend(new);
        chunk.entries.extend(rest);
        chunk.entries.extend(tail);
        for &e in emptied.iter().rev() {
            self.chunks.remove(e);
        }
        let len = self.chunks[c0].entries.len();
        if len > MAX_CHUNK {
            let entries = std::mem::take(&mut self.chunks[c0].entries);
            self.chunks.splice(c0..=c0, into_chunks(entries));
        } else if len == 0 && self.chunks.len() > 1 {
            self.chunks.remove(c0);
        } else {
            self.chunks[c0].resum();
        }
        self.tops.borrow_mut().truncate(c0);
        self.recount(c0);
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
        let chunk = &self.chunks[s.chunk];
        base + chunk.entries[..s.index].iter().map(Entry::extent).sum::<f64>()
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
        let total = tops[last] + self.chunks[last].extent;
        let tail = self.chunks[last].entries.last().map_or(0.0, Entry::tail);
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
        for (i, e) in chunk.entries.iter().enumerate() {
            top += e.extent();
            if y < top || i + 1 == chunk.entries.len() {
                return self.counts[c] + i;
            }
        }
        self.counts[c]
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
            if chunk.laid == chunk.entries.len() {
                continue;
            }
            let first = if c == s.chunk { s.index } else { 0 };
            if let Some(i) = chunk.entries[first..].iter().position(|e| !e.is_laid()) {
                found = Some(self.counts[c] + first + i).filter(|&p| p < end);
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
        let s = self.slot(from);
        let start = if from >= self.len() { self.chunks.len() } else { s.chunk };
        self.chunks[start.min(self.chunks.len())..].iter().enumerate().flat_map(move |(k, c)| {
            let chunk = start + k;
            let skip = if chunk == s.chunk { s.index } else { 0 };
            c.entries.iter().enumerate().skip(skip).map(move |(i, e)| (self.counts[chunk] + i, e))
        })
    }
}

fn new_chunk(entries: Vec<Entry>) -> Chunk {
    let mut c = Chunk { entries, ..Chunk::default() };
    c.resum();
    c
}

/// Chunks of half the most entries each, moved, not copied.
fn into_chunks(entries: Vec<Entry>) -> Vec<Chunk> {
    let mut out = Vec::with_capacity(entries.len().div_ceil(MAX_CHUNK / 2));
    let mut it = entries.into_iter();
    while it.len() > 0 {
        out.push(new_chunk(it.by_ref().take(MAX_CHUNK / 2).collect()));
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
        let mut c = LayoutCache::new((0..1000).map(|i| est(10.0 + (i % 3) as f32)).collect());
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
        c.splice(10, 300, (0..5).map(|_| est(1.0)).collect());
        assert_eq!(c.len(), 705);
        check(&c);
        c.splice(700, 5, (0..400).map(|_| est(2.0)).collect());
        assert_eq!(c.len(), 1100);
        check(&c);
        c.splice(c.len(), 0, vec![est(5.0)]);
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
        let mut c = LayoutCache::new((0..700).map(|_| est(10.0)).collect());
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
        c.splice(50, 3, vec![est(1.0), laid(), est(2.0), laid()]);
        check(&c);
        for p in 0..c.len() {
            c.set(p, laid());
        }
        check(&c);
        c.splice(c.len(), 0, vec![est(5.0)]);
        check(&c);
    }
}
