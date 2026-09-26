//! An open-addressed index of positions by hash, for collections that keep
//! their entries in a vector of their own: ordered sets (whose vector is
//! their order) and the hash and map tables.
//!
//! It is the index of `table.rs` on its own: slots hold positions in the
//! owner's vector, the index is kept at most half full, probing is linear
//! from a spread of the hash, and removal closes the gap by shifting later
//! slots back, so there are no tombstones. The owner keeps each entry's
//! hash and passes a way to read it, since the index stores only
//! positions. It never compares keys itself: lookups take a test for the
//! position found, so owners decide what equality means (`-isEqual:`, the
//! same pointer, a weak reference still alive).

use objc2_foundation::NSUInteger;

/// A free slot.
const FREE: u32 = u32::MAX;

/// Where a hash starts probing. Hashes can be weak in their low bits
/// (addresses, small integers), so they are spread first.
#[inline]
fn home(hash: NSUInteger, mask: usize) -> usize {
    ((hash as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32) as usize & mask
}

#[derive(Clone, Default)]
pub(crate) struct Index {
    /// A power-of-two table of positions, or empty for no index.
    slots: Box<[u32]>,
}

impl Index {
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
        self.slots = Box::default();
    }

    /// The position whose entry `matches`, among those with `hash`, or the
    /// free slot where one with `hash` would go.
    #[inline]
    pub(crate) fn find(&self, hash: NSUInteger, mut matches: impl FnMut(usize) -> bool) -> Result<usize, usize> {
        let mask = self.slots.len() - 1;
        let mut slot = home(hash, mask);
        loop {
            match self.slots[slot] {
                FREE => return Err(slot),
                at if matches(at as usize) => return Ok(at as usize),
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
        let mask = self.slots.len() - 1;
        let mut slot = home(hash, mask);
        loop {
            match self.slots[slot] {
                FREE => return Some(Err(slot)),
                at if matches(at as usize)? => return Some(Ok(at as usize)),
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// Record `at`, whose entry has `hash`, in the first free slot.
    pub(crate) fn add(&mut self, hash: NSUInteger, at: usize) {
        let slot = self.free_slot(hash);
        self.slots[slot] = at as u32;
    }

    /// An index of `size` slots (a power of two) for the `len` entries
    /// whose hashes `hash_of` gives.
    pub(crate) fn rebuild(&mut self, size: usize, len: usize, hash_of: impl Fn(usize) -> NSUInteger) {
        self.slots = vec![FREE; size].into();
        for at in 0..len {
            self.add(hash_of(at), at);
        }
    }

    /// An index of `size` slots (a power of two) for the entries at the
    /// positions `entries` gives, with their hashes: for owners whose
    /// entries have holes between them.
    pub(crate) fn rebuild_with(&mut self, size: usize, entries: impl Iterator<Item = (usize, NSUInteger)>) {
        self.slots = vec![FREE; size].into();
        for (at, hash) in entries {
            self.add(hash, at);
        }
    }

    /// The size an index for `len` entries starts at.
    pub(crate) fn size_for(len: usize) -> usize {
        (len.max(1) * 2).next_power_of_two()
    }

    fn free_slot(&self, hash: NSUInteger) -> usize {
        let mask = self.slots.len() - 1;
        let mut slot = home(hash, mask);
        while self.slots[slot] != FREE {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// The slot holding `at`, whose entry has `hash`.
    fn slot_of(&self, hash: NSUInteger, at: usize) -> usize {
        let mask = self.slots.len() - 1;
        let mut slot = home(hash, mask);
        while self.slots[slot] != at as u32 {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// Forget `at`, whose entry has `hash`, moving back later slots of the
    /// same run whose probe sequence passes through its slot. `hash_of`
    /// gives the hashes of the entries still indexed.
    pub(crate) fn remove(&mut self, hash: NSUInteger, at: usize, hash_of: impl Fn(usize) -> NSUInteger) {
        let mask = self.slots.len() - 1;
        let mut hole = self.slot_of(hash, at);
        self.slots[hole] = FREE;
        let mut slot = hole;
        loop {
            slot = (slot + 1) & mask;
            let next = self.slots[slot];
            if next == FREE {
                return;
            }
            let start = home(hash_of(next as usize), mask);
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
    pub(crate) fn moved(&mut self, hash: NSUInteger, from: usize, to: usize) {
        let slot = self.slot_of(hash, from);
        self.slots[slot] = to as u32;
    }

    /// Positions from `at` on move up by one (an entry was inserted at
    /// `at`, and is not indexed yet).
    pub(crate) fn shift_up(&mut self, at: usize) {
        for slot in self.slots.iter_mut() {
            if *slot != FREE && *slot as usize >= at {
                *slot += 1;
            }
        }
    }

    /// Positions after `at` move down by one (the entry at `at` was
    /// removed from the index and its vector).
    pub(crate) fn shift_down(&mut self, at: usize) {
        for slot in self.slots.iter_mut() {
            if *slot != FREE && *slot as usize > at {
                *slot -= 1;
            }
        }
    }
}
