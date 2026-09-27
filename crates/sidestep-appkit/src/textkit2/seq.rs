//! A sequence of items covering a text, each some UTF-16 units long, in
//! chunks with their sums: where an offset or a height falls is a binary
//! search over the chunks and a walk through one, and replacing items
//! costs the chunks it touches. The sums before each chunk are worked out
//! again lazily, from the first chunk that changed, when a position is
//! next asked for.
//!
//! Heights come in two parts, so estimates can be rescaled without
//! touching the items: a fixed part (laid out, or kept from before) and a
//! raw estimate that a factor scales (see `layout_manager::Seg`).

/// What an item adds to its chunk's sums.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Metrics {
    pub fixed: f64,
    pub raw: f64,
    /// Items laid out, and the left and right edges of what they laid out
    /// (infinity and minus infinity for none).
    pub laid: usize,
    pub left: f64,
    pub right: f64,
}

impl Default for Metrics {
    fn default() -> Metrics {
        Metrics { fixed: 0.0, raw: 0.0, laid: 0, left: f64::INFINITY, right: f64::NEG_INFINITY }
    }
}

impl Metrics {
    fn add(self, o: Metrics) -> Metrics {
        Metrics {
            fixed: self.fixed + o.fixed,
            raw: self.raw + o.raw,
            laid: self.laid + o.laid,
            left: self.left.min(o.left),
            right: self.right.max(o.right),
        }
    }

    pub fn height(&self, factor: f64) -> f64 {
        self.fixed + factor * self.raw
    }
}

pub(crate) trait Item {
    fn len(&self) -> usize;

    fn metrics(&self) -> Metrics {
        Metrics::default()
    }
}

/// Items made at once go this many to a chunk.
const CHUNK: usize = 64;

struct Chunk<T> {
    items: Vec<T>,
    len: usize,
    m: Metrics,
}

impl<T: Item> Chunk<T> {
    fn new(items: Vec<T>) -> Chunk<T> {
        let mut c = Chunk { items, len: 0, m: Metrics::default() };
        c.resum();
        c
    }

    fn resum(&mut self) {
        self.len = self.items.iter().map(Item::len).sum();
        self.m = self.items.iter().fold(Metrics::default(), |m, i| m.add(i.metrics()));
    }
}

/// Where an item is: its chunk and place there, the offset it starts at,
/// and the heights before it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pos {
    pub chunk: usize,
    pub index: usize,
    pub start: usize,
    pub before: Metrics,
}

impl Pos {
    pub fn top(&self, factor: f64) -> f64 {
        self.before.height(factor)
    }
}

pub(crate) struct Seq<T> {
    chunks: Vec<Chunk<T>>,
    /// The offset and sums before each chunk, good up to `synced`.
    starts: Vec<(usize, Metrics)>,
    synced: usize,
}

impl<T: Item> Seq<T> {
    /// A sequence of `items`, which must not be empty.
    pub fn new(items: Vec<T>) -> Seq<T> {
        assert!(!items.is_empty(), "sidestep: a sequence holds an item at least");
        let mut chunks = Vec::with_capacity(items.len() / CHUNK + 1);
        let mut items = items.into_iter().peekable();
        while items.peek().is_some() {
            chunks.push(Chunk::new(items.by_ref().take(CHUNK).collect()));
        }
        let mut s = Seq { starts: Vec::new(), chunks, synced: 0 };
        s.sync();
        s
    }

    fn sync(&mut self) {
        if self.synced >= self.chunks.len() && self.starts.len() == self.chunks.len() {
            return;
        }
        let from = self.synced.min(self.chunks.len());
        self.starts.truncate(from);
        let (mut off, mut m) = match from.checked_sub(1) {
            Some(p) => (self.starts[p].0 + self.chunks[p].len, self.starts[p].1.add(self.chunks[p].m)),
            None => (0, Metrics::default()),
        };
        for c in &self.chunks[from..] {
            self.starts.push((off, m));
            off += c.len;
            m = m.add(c.m);
        }
        self.synced = self.chunks.len();
    }

    fn stale(&mut self, chunk: usize) {
        self.synced = self.synced.min(chunk);
    }

    /// The total length and sums.
    pub fn total(&mut self) -> (usize, Metrics) {
        self.sync();
        let last = self.chunks.len() - 1;
        (self.starts[last].0 + self.chunks[last].len, self.starts[last].1.add(self.chunks[last].m))
    }

    pub fn len(&mut self) -> usize {
        self.total().0
    }

    /// The item holding offset `o`: the last one starting at or before it
    /// and ending after it (an empty item at `o` is passed over for the
    /// next); past the end, the last item.
    pub fn locate(&mut self, o: usize) -> Pos {
        self.sync();
        let mut c = self.starts.partition_point(|s| s.0 <= o).saturating_sub(1);
        // Chunks that end at `o` hand it on.
        while c + 1 < self.chunks.len() && self.starts[c + 1].0 <= o {
            c += 1;
        }
        let (mut start, mut before) = self.starts[c];
        let items = &self.chunks[c].items;
        for (i, item) in items.iter().enumerate() {
            let end = start + item.len();
            if o < end || (i + 1 == items.len() && c + 1 == self.chunks.len()) {
                return Pos { chunk: c, index: i, start, before };
            }
            start = end;
            before = before.add(item.metrics());
        }
        // `o` is this chunk's end: the next chunk's first item.
        let n = c + 1;
        if n < self.chunks.len() {
            return Pos { chunk: n, index: 0, start: self.starts[n].0, before: self.starts[n].1 };
        }
        self.last()
    }

    /// The item at height `y` (with estimates scaled by `factor`): the
    /// first above the top, the last below the bottom.
    pub fn locate_y(&mut self, y: f64, factor: f64) -> Pos {
        self.sync();
        let c = self.starts.partition_point(|s| s.1.height(factor) <= y).saturating_sub(1);
        let (mut start, mut before) = self.starts[c];
        let items = &self.chunks[c].items;
        for (i, item) in items.iter().enumerate() {
            let next = before.add(item.metrics());
            if y < next.height(factor) || i + 1 == items.len() {
                // An item of no height at `y` is passed over for the next
                // one with height, within the chunk.
                return Pos { chunk: c, index: i, start, before };
            }
            start += item.len();
            before = next;
        }
        unreachable!("sidestep: a chunk without items")
    }

    #[cfg(test)]
    pub fn first(&mut self) -> Pos {
        self.sync();
        Pos { chunk: 0, index: 0, start: 0, before: Metrics::default() }
    }

    pub fn last(&mut self) -> Pos {
        self.sync();
        let c = self.chunks.len() - 1;
        let (mut start, mut before) = self.starts[c];
        let items = &self.chunks[c].items;
        for item in &items[..items.len() - 1] {
            start += item.len();
            before = before.add(item.metrics());
        }
        Pos { chunk: c, index: items.len() - 1, start, before }
    }

    pub fn get(&self, p: Pos) -> &T {
        &self.chunks[p.chunk].items[p.index]
    }

    /// Change the item at `p` (its length must stay).
    pub fn update(&mut self, p: Pos, f: impl FnOnce(&mut T)) {
        let chunk = &mut self.chunks[p.chunk];
        let before = chunk.items[p.index].len();
        f(&mut chunk.items[p.index]);
        debug_assert_eq!(before, chunk.items[p.index].len(), "sidestep: an update changed an item's length");
        chunk.m = chunk.items.iter().fold(Metrics::default(), |m, i| m.add(i.metrics()));
        self.stale(p.chunk + 1);
    }

    /// The items from the one holding `a` on as far as `b`: the first's
    /// position, how many, and where the last ends.
    pub fn cover(&mut self, a: usize, b: usize) -> (Pos, usize, usize) {
        let first = self.locate(a);
        let (mut last, mut count) = (first, 1);
        let mut end = first.start + self.get(first).len();
        while end < b {
            let Some(n) = self.next(last) else { break };
            last = n;
            count += 1;
            end += self.get(n).len();
        }
        (first, count, end)
    }

    /// The item after `p`.
    pub fn next(&mut self, p: Pos) -> Option<Pos> {
        let item = self.get(p);
        let (start, before) = (p.start + item.len(), p.before.add(item.metrics()));
        if p.index + 1 < self.chunks[p.chunk].items.len() {
            return Some(Pos { index: p.index + 1, start, before, ..p });
        }
        (p.chunk + 1 < self.chunks.len()).then_some(Pos { chunk: p.chunk + 1, index: 0, start, before })
    }

    /// The item before `p`: its sums counted forward from its chunk's
    /// start (edges don't subtract).
    pub fn prev(&mut self, p: Pos) -> Option<Pos> {
        let (c, index) = match (p.index, p.chunk) {
            (0, 0) => return None,
            (0, c) => (c - 1, self.chunks[c - 1].items.len() - 1),
            (i, c) => (c, i - 1),
        };
        self.sync();
        let (mut start, mut before) = self.starts[c];
        for item in &self.chunks[c].items[..index] {
            start += item.len();
            before = before.add(item.metrics());
        }
        Some(Pos { chunk: c, index, start, before })
    }

    /// Replace `count` items from `p` on with `new` (none left is not
    /// allowed: `new` or other items must remain).
    pub fn splice(&mut self, p: Pos, count: usize, new: Vec<T>) {
        // The chunks the items run through, gathered and cut again.
        let mut end_chunk = p.chunk;
        let mut left = count + p.index;
        while left > self.chunks[end_chunk].items.len() && end_chunk + 1 < self.chunks.len() {
            left -= self.chunks[end_chunk].items.len();
            end_chunk += 1;
        }
        let mut gathered: Vec<T> = Vec::new();
        for c in self.chunks.drain(p.chunk..=end_chunk) {
            gathered.extend(c.items);
        }
        let tail = gathered.split_off((p.index + count).min(gathered.len()));
        gathered.truncate(p.index);
        gathered.extend(new);
        gathered.extend(tail);
        let mut rebuilt = Vec::with_capacity(gathered.len() / CHUNK + 1);
        let mut it = gathered.into_iter().peekable();
        while it.peek().is_some() {
            rebuilt.push(Chunk::new(it.by_ref().take(CHUNK).collect()));
        }
        self.chunks.splice(p.chunk..p.chunk, rebuilt);
        assert!(!self.chunks.is_empty(), "sidestep: a sequence lost all its items");
        self.stale(p.chunk);
    }

    /// The first item whose metrics `f` accepts, skipping chunks that hold
    /// none.
    pub fn first_where(&mut self, f: impl Fn(&Metrics) -> bool) -> Option<Pos> {
        self.sync();
        let c = (0..self.chunks.len()).find(|&c| f(&self.chunks[c].m))?;
        let (mut start, mut before) = self.starts[c];
        for (i, item) in self.chunks[c].items.iter().enumerate() {
            let m = item.metrics();
            if f(&m) {
                return Some(Pos { chunk: c, index: i, start, before });
            }
            start += item.len();
            before = before.add(m);
        }
        None
    }

    /// The last item whose metrics `f` accepts, skipping chunks that hold
    /// none.
    pub fn last_where(&mut self, f: impl Fn(&Metrics) -> bool) -> Option<Pos> {
        self.sync();
        let c = (0..self.chunks.len()).rev().find(|&c| f(&self.chunks[c].m))?;
        let (mut start, mut before) = self.starts[c];
        let mut found = None;
        for (i, item) in self.chunks[c].items.iter().enumerate() {
            let m = item.metrics();
            if f(&m) {
                found = Some(Pos { chunk: c, index: i, start, before });
            }
            start += item.len();
            before = before.add(m);
        }
        found
    }

    /// Every item, in order, with where it starts.
    pub fn for_each(&self, mut f: impl FnMut(usize, &T)) {
        let mut start = 0;
        for c in &self.chunks {
            for item in &c.items {
                f(start, item);
                start += item.len();
            }
        }
    }

    /// Change every item (lengths must stay).
    pub fn for_each_mut(&mut self, mut f: impl FnMut(&mut T)) {
        for c in &mut self.chunks {
            for item in &mut c.items {
                f(item);
            }
            c.resum();
        }
        self.stale(0);
    }

    /// The number of items.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.chunks.iter().map(|c| c.items.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct I(usize, f64);

    impl Item for I {
        fn len(&self) -> usize {
            self.0
        }
        fn metrics(&self) -> Metrics {
            Metrics { fixed: self.1, ..Metrics::default() }
        }
    }

    fn seq(n: usize) -> Seq<I> {
        Seq::new((0..n).map(|i| I(i % 3 + 1, (i % 5) as f64 + 1.0)).collect())
    }

    #[test]
    fn locate_matches_a_walk() {
        let mut s = seq(500);
        let mut items = Vec::new();
        s.for_each(|start, i| items.push((start, i.clone())));
        let total = s.len();
        for o in 0..=total {
            let p = s.locate(o);
            let want = items.iter().rposition(|(st, i)| *st <= o && o < st + i.0).unwrap_or(items.len() - 1);
            assert_eq!(p.start, items[want].0, "offset {o}");
        }
        let mut y = 0.0;
        for (start, i) in &items {
            let p = s.locate_y(y + 0.5, 1.0);
            assert_eq!(p.start, *start);
            assert!((p.top(1.0) - y).abs() < 1e-9);
            y += i.1;
        }
    }

    #[test]
    fn splice_and_walk() {
        let mut s = seq(300);
        let p = s.locate(100);
        let start = p.start;
        s.splice(p, 70, vec![I(5, 2.0)]);
        let q = s.locate(start);
        assert_eq!(q.start, start);
        assert_eq!(s.get(q).0, 5);
        // Walk forward and back.
        let mut p = s.first();
        let mut n = 1;
        while let Some(q) = s.next(p) {
            let back = s.prev(q).expect("an item before");
            assert_eq!((back.start, back.before), (p.start, p.before));
            p = q;
            n += 1;
        }
        assert_eq!(n, s.count());
    }

    #[test]
    fn cover_spans_the_items_a_range_meets() {
        let mut s = seq(300);
        let mut items = Vec::new();
        s.for_each(|start, i| items.push((start, i.0)));
        for (a, b) in [(0, 1), (5, 40), (17, 17), (100, 400)] {
            let (p, count, end) = s.cover(a, b);
            let first = items.iter().rposition(|&(st, len)| st <= a && a < st + len).expect("an item");
            assert_eq!(p.start, items[first].0);
            let last = first + count - 1;
            assert_eq!(end, items[last].0 + items[last].1);
            assert!(end >= b.min(s.len()) && (count == 1 || items[last].0 < b));
        }
    }
}
