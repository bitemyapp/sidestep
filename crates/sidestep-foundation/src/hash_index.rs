//! An open-addressed index of positions by hash, for collections that keep
//! their entries in a vector of their own: dictionaries and sets
//! (`table.rs`), ordered sets (whose vector is their order), the hash and
//! map tables, and caches.
//!
//! Slots hold positions in the owner's vector; the index is kept at most
//! half full, probing is linear from a spread of the hash, and removal
//! closes the gap by shifting later slots back, so there are no
//! tombstones. The owner keeps each entry's hash and passes a way to read
//! it, since the index stores only positions. It never compares keys
//! itself: lookups take a test for the position found, so owners decide
//! what equality means (`-isEqual:`, the same pointer, a weak reference
//! still alive).
//!
//! Owners whose entries keep their order (ordered sets) move every entry
//! after a change in the middle. Their index (`Index<true>`) then
//! renumbers whichever side of the change has fewer entries (`open`,
//! `close`), finding each through its hash, or passing over every slot
//! once they are many. Positions are stored offset by a base, so
//! renumbering the entries before a change moves the base instead of the
//! entries after it: a change at either end renumbers nothing. Other
//! owners' indexes store positions as they are, which costs their lookups
//! nothing.

use objc2_foundation::NSUInteger;

/// A free slot.
const FREE: u32 = u32::MAX;
/// Renumbering more than one entry per this many slots passes over the
/// slots instead of finding each entry (see `Index::many`).
const SHIFT_BY_SCAN: usize = 8;

/// Where a hash starts probing. Hashes can be weak in their low bits
/// (addresses, small integers), so they are spread first.
#[inline]
fn home(hash: NSUInteger, mask: usize) -> usize {
    ((hash as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32) as usize & mask
}

/// The size an index for `len` entries starts at.
pub(crate) fn size_for(len: usize) -> usize {
    (len.max(1) * 2).next_power_of_two()
}

/// An index of positions; `SHIFTS` for owners that open and close gaps in
/// their order.
#[derive(Clone, Default)]
pub(crate) struct Index<const SHIFTS: bool = false> {
    /// A power-of-two table of stored positions, or empty for no index.
    slots: Box<[u32]>,
    /// What position 0 is stored as; always 0 without `SHIFTS`.
    base: u32,
}

impl<const SHIFTS: bool> Index<SHIFTS> {
    /// What position 0 is stored as, known to be 0 without `SHIFTS`.
    #[inline]
    fn base(&self) -> u32 {
        if SHIFTS { self.base } else { 0 }
    }

    /// Whether there is an index at all (owners go without one while they
    /// are small enough to search in order).
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Whether `len` entries need a larger index than this one.
    #[inline]
    pub(crate) fn full_for(&self, len: usize) -> bool {
        len * 2 > self.slots.len()
    }

    /// Drop the index, for an owner small enough to search in order.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// How position `at` is stored.
    #[inline]
    fn stored(&self, at: usize) -> u32 {
        self.base().wrapping_add(at as u32)
    }

    /// The position whose entry `matches`, among those with `hash`, or the
    /// free slot where one with `hash` would go.
    #[inline]
    pub(crate) fn find(&self, hash: NSUInteger, mut matches: impl FnMut(usize) -> bool) -> Result<usize, usize> {
        let (slots, base) = (&*self.slots, self.base());
        let mask = slots.len() - 1;
        let mut slot = home(hash, mask);
        loop {
            match slots[slot] {
                FREE => return Err(slot),
                at if matches(at.wrapping_sub(base) as usize) => return Ok(at.wrapping_sub(base) as usize),
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// `find`, with a test that may be unable to decide (`None`), which
    /// ends the search undecided.
    #[inline]
    pub(crate) fn find_quietly(
        &self,
        hash: NSUInteger,
        mut matches: impl FnMut(usize) -> Option<bool>,
    ) -> Option<Result<usize, usize>> {
        let (slots, base) = (&*self.slots, self.base());
        let mask = slots.len() - 1;
        let mut slot = home(hash, mask);
        loop {
            match slots[slot] {
                FREE => return Some(Err(slot)),
                at if matches(at.wrapping_sub(base) as usize)? => return Some(Ok(at.wrapping_sub(base) as usize)),
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// Record `at`, whose entry has `hash`, in the first free slot.
    #[inline]
    pub(crate) fn add(&mut self, hash: NSUInteger, at: usize) {
        let slot = self.free_slot(hash);
        self.slots[slot] = self.stored(at);
    }

    /// Record `at` in `slot`, the free slot `find` returned for its entry's
    /// hash, the index unchanged since.
    #[inline]
    pub(crate) fn put(&mut self, slot: usize, at: usize) {
        debug_assert_eq!(self.slots[slot], FREE);
        self.slots[slot] = self.stored(at);
    }

    /// An empty index of `size` slots (a power of two).
    pub(crate) fn with_size(size: usize) -> Self {
        Index { slots: vec![FREE; size].into(), base: 0 }
    }

    /// An index of `size` slots (a power of two) for the `len` entries
    /// whose hashes `hash_of` gives.
    pub(crate) fn rebuild(&mut self, size: usize, len: usize, hash_of: impl Fn(usize) -> NSUInteger) {
        self.rebuild_with(size, (0..len).map(|at| (at, hash_of(at))));
    }

    /// An index of `size` slots (a power of two) for the entries at the
    /// positions `entries` gives, with their hashes: for owners whose
    /// entries have holes between them.
    pub(crate) fn rebuild_with(&mut self, size: usize, entries: impl Iterator<Item = (usize, NSUInteger)>) {
        *self = Self::with_size(size);
        for (at, hash) in entries {
            self.add(hash, at);
        }
    }

    #[inline]
    fn free_slot(&self, hash: NSUInteger) -> usize {
        let mask = self.slots.len() - 1;
        let mut slot = home(hash, mask);
        while self.slots[slot] != FREE {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// The slot holding `stored`, a stored position whose entry has `hash`.
    #[inline]
    fn slot_of(&self, hash: NSUInteger, stored: u32) -> usize {
        let mask = self.slots.len() - 1;
        let mut slot = home(hash, mask);
        while self.slots[slot] != stored {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// Forget `at`, whose entry has `hash`, moving back later slots of the
    /// same run whose probe sequence passes through its slot. `hash_of`
    /// gives the hashes of the entries still indexed.
    #[inline]
    pub(crate) fn remove(&mut self, hash: NSUInteger, at: usize, hash_of: impl Fn(usize) -> NSUInteger) {
        let (mask, base) = (self.slots.len() - 1, self.base());
        let mut hole = self.slot_of(hash, self.stored(at));
        self.slots[hole] = FREE;
        let mut slot = hole;
        loop {
            slot = (slot + 1) & mask;
            let next = self.slots[slot];
            if next == FREE {
                return;
            }
            let start = home(hash_of(next.wrapping_sub(base) as usize), mask);
            // The entry may move back if the hole lies between its home and
            // where it sits now.
            if hole.wrapping_sub(start) & mask < slot.wrapping_sub(start) & mask {
                self.slots[hole] = next;
                self.slots[slot] = FREE;
                hole = slot;
            }
        }
    }

    /// The entry at `from`, whose entry has `hash`, is now at `to`.
    #[inline]
    pub(crate) fn moved(&mut self, hash: NSUInteger, from: usize, to: usize) {
        self.renumber(hash, self.stored(from), self.stored(to));
    }

    /// The entry with `hash` stored as `from` is stored as `to` from now on.
    #[inline]
    fn renumber(&mut self, hash: NSUInteger, from: u32, to: u32) {
        let slot = self.slot_of(hash, from);
        self.slots[slot] = to;
    }
}

impl Index<true> {
    /// Make room for `count` entries at position `at` among `len`: the
    /// entries from `at` on move up by `count`. `hash_of` gives the hashes
    /// of the entries by their positions before the move.
    pub(crate) fn open(&mut self, at: usize, len: usize, count: usize, hash_of: impl Fn(usize) -> NSUInteger) {
        let n = count as u32;
        if len - at <= at {
            if self.many(len - at) {
                self.shift(self.stored(at), (len - at) as u32, n);
                return;
            }
            // The entries after, from the last down, so that each moves to
            // a stored position no entry holds.
            for p in (at..len).rev() {
                let from = self.stored(p);
                self.renumber(hash_of(p), from, from + n);
            }
        } else {
            if self.base < n {
                // Room below the stored positions, for as many changes at
                // the front as there are slots before the next rewrite.
                self.rebase(n + self.slots.len() as u32);
            }
            // The entries before keep their positions while the base moves
            // the rest up: from the first on, for the same reason.
            if self.many(at) {
                self.shift(self.base, at as u32, n.wrapping_neg());
            } else {
                for p in 0..at {
                    let from = self.stored(p);
                    self.renumber(hash_of(p), from, from - n);
                }
            }
            self.base -= n;
        }
    }

    /// Close the gap the `count` entries at position `at` leave among
    /// `len` (counting them), once they are forgotten (`remove`): the
    /// entries after them move down by `count`. `hash_of` gives the hashes
    /// of the entries by their positions before the move.
    pub(crate) fn close(&mut self, at: usize, len: usize, count: usize, hash_of: impl Fn(usize) -> NSUInteger) {
        let n = count as u32;
        let after = len - at - count;
        if after <= at {
            if self.many(after) {
                self.shift(self.stored(at + count), after as u32, n.wrapping_neg());
                return;
            }
            // The entries after, from the first on (see `open`).
            for p in at + count..len {
                let from = self.stored(p);
                self.renumber(hash_of(p), from, from - n);
            }
        } else {
            if self.base > u32::MAX / 2 {
                // Keep stored positions far from `FREE`: a rewrite once per
                // two billion changes at the front.
                self.rebase(0);
            }
            // The entries before keep their positions while the base moves
            // the rest down: from the last down.
            if self.many(at) {
                self.shift(self.base, at as u32, n);
            } else {
                for p in (0..at).rev() {
                    let from = self.stored(p);
                    self.renumber(hash_of(p), from, from + n);
                }
            }
            self.base += n;
        }
    }

    /// Whether renumbering `count` entries one by one, each found through
    /// its hash, costs more than a pass over every slot.
    #[inline]
    fn many(&self, count: usize) -> bool {
        count * SHIFT_BY_SCAN >= self.slots.len()
    }

    /// Add `delta` to the stored positions in `from..from + count`, in one
    /// pass over the slots, which moves them all at once.
    fn shift(&mut self, from: u32, count: u32, delta: u32) {
        for slot in self.slots.iter_mut() {
            let inside = *slot != FREE && slot.wrapping_sub(from) < count;
            *slot = slot.wrapping_add(if inside { delta } else { 0 });
        }
    }

    /// Store positions offset by `base` from now on, rewriting every slot.
    fn rebase(&mut self, base: u32) {
        let delta = base.wrapping_sub(self.base);
        for slot in self.slots.iter_mut() {
            if *slot != FREE {
                *slot = slot.wrapping_add(delta);
            }
        }
        self.base = base;
    }
}

#[cfg(test)]
mod tests {
    use super::{Index, size_for};

    /// Random runs inserted and removed, at the front more often than not,
    /// checked by finding every entry at its position after each change.
    /// Hashes repeat, so probe runs are long.
    #[test]
    fn open_and_close_keep_positions() {
        let mut hashes: Vec<usize> = Vec::new();
        let mut index = Index::<true>::default();
        let mut next = 0usize;
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        for step in 0..5_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = hashes.len();
            if index.is_empty() || index.full_for(len + 3) {
                index.rebuild(size_for(len + 64), len, |at| hashes[at]);
            }
            let at = if x.is_multiple_of(4) || len == 0 { 0 } else { (x >> 8) as usize % (len + 1) };
            if (x.is_multiple_of(3) || len > 300) && len > 0 {
                let at = at.min(len - 1);
                let count = (((x >> 20) % 3) as usize + 1).min(len - at);
                for p in at..at + count {
                    index.remove(hashes[p], p, |q| hashes[q]);
                }
                index.close(at, len, count, |p| hashes[p]);
                hashes.drain(at..at + count);
            } else {
                let count = ((x >> 24) % 3) as usize + 1;
                let fresh: Vec<usize> = (0..count)
                    .map(|_| {
                        next += 1;
                        next % 97
                    })
                    .collect();
                index.open(at, len, count, |p| hashes[p]);
                for (k, &hash) in fresh.iter().enumerate() {
                    index.add(hash, at + k);
                }
                hashes.splice(at..at, fresh);
            }
            for (at, &hash) in hashes.iter().enumerate() {
                assert_eq!(index.find(hash, |p| p == at), Ok(at), "step {step}");
            }
        }
    }
}
