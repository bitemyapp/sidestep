//! The hash table behind `NSDictionary` and `NSSet`.
//!
//! Entries sit in a vector with each key's hash, computed once. The smallest
//! tables are searched in order; larger ones add an open-addressed index
//! of positions in the vector, kept at most half full. In small tables
//! (attribute dictionaries, mostly) a lookup first looks for the very key
//! object, which CFDictionary also treats as a match. Otherwise it compares
//! hashes and sends `-isEqual:` only when they match. Keys whose class is
//! exactly one of Sidestep's strings or numbers are hashed and compared
//! without sending messages.
//!
//! Removal moves the last entry into the hole and closes the gap in the
//! index by shifting later slots back, so the entries stay dense (fast to
//! enumerate) and the index needs no tombstones.
//!
//! Mutable collections keep their table in a [`CowTable`]: copying one
//! shares the table with the copy, and the next change copies it first.

use std::cell::{Ref, RefCell, RefMut};
use std::ops::Deref;
use std::ptr;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::NSUInteger;

use crate::number::{Number, fast_value};
use crate::string::fast_parts;

/// Up to this many entries, a table has no index.
const SCAN: usize = 4;
/// Up to this many entries, a lookup first looks for the very key object,
/// which needs no hashing.
const IDENTITY: usize = 8;
/// A free slot in the index.
const FREE: u32 = u32::MAX;

pub(crate) struct Entry<V> {
    pub(crate) hash: NSUInteger,
    pub(crate) key: Retained<AnyObject>,
    pub(crate) value: V,
}

pub(crate) struct Table<V> {
    entries: Vec<Entry<V>>,
    /// Positions in `entries`, by hash: a power-of-two table at most half
    /// full, or empty for tables searched in order.
    index: Box<[u32]>,
}

impl<V> Default for Table<V> {
    fn default() -> Self {
        Table { entries: Vec::new(), index: Box::default() }
    }
}

/// What a key is, when it can be compared without messages.
enum Fast<'a> {
    Text(&'a str),
    Number(Number),
    Other,
}

/// A key to look for: its hash and, for Sidestep's own strings and
/// numbers, its value.
pub(crate) struct Probe<'a> {
    key: &'a AnyObject,
    pub(crate) hash: NSUInteger,
    fast: Fast<'a>,
}

impl<'a> Probe<'a> {
    pub(crate) fn new(key: &'a AnyObject) -> Self {
        if let Some((text, hash)) = fast_parts(key) {
            Probe { key, hash, fast: Fast::Text(text) }
        } else if let Some(n) = fast_value(key) {
            Probe { key, hash: n.hash(), fast: Fast::Number(n) }
        } else {
            // SAFETY: -hash takes nothing and returns NSUInteger.
            Probe { key, hash: unsafe { objc2::msg_send![key, hash] }, fast: Fast::Other }
        }
    }

    fn matches<V>(&self, entry: &Entry<V>) -> bool {
        if entry.hash != self.hash {
            return false;
        }
        if ptr::eq(&*entry.key, self.key) {
            return true;
        }
        match self.fast {
            Fast::Text(text) => {
                if let Some((other, _)) = fast_parts(&entry.key) {
                    return text == other;
                }
            }
            Fast::Number(n) => {
                if let Some(other) = fast_value(&entry.key) {
                    return n.equals(&other);
                }
            }
            Fast::Other => {}
        }
        // SAFETY: -isEqual: takes an object and returns BOOL.
        unsafe { objc2::msg_send![&*entry.key, isEqual: self.key] }
    }
}

/// Where a hash starts probing. Keys' own hashes can be weak in their low
/// bits (addresses, small integers), so they are spread first.
fn home(hash: NSUInteger, mask: usize) -> usize {
    ((hash as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32) as usize & mask
}

impl<V> Table<V> {
    pub(crate) fn with_capacity(count: usize) -> Self {
        let index = if count > SCAN { vec![FREE; (count * 2).next_power_of_two()].into() } else { Box::default() };
        Table { entries: Vec::with_capacity(count), index }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn entries(&self) -> &[Entry<V>] {
        &self.entries
    }

    /// The entry matching `probe`, or else the free index slot where it
    /// would go (meaningless without an index). May send `-isEqual:` to
    /// the keys, so callers holding a mutable table must not be mutating
    /// it meanwhile.
    pub(crate) fn locate(&self, probe: &Probe) -> Result<usize, usize> {
        if self.index.is_empty() {
            return self.entries.iter().position(|e| probe.matches(e)).ok_or(0);
        }
        let mask = self.index.len() - 1;
        let mut slot = home(probe.hash, mask);
        loop {
            match self.index[slot] {
                FREE => return Err(slot),
                at if probe.matches(&self.entries[at as usize]) => return Ok(at as usize),
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    pub(crate) fn get(&self, key: &AnyObject) -> Option<&Entry<V>> {
        self.position(key).map(|i| &self.entries[i])
    }

    pub(crate) fn position(&self, key: &AnyObject) -> Option<usize> {
        if self.entries.len() <= IDENTITY {
            // Callers mostly look up with the key object they stored.
            if let Some(i) = self.entries.iter().position(|e| ptr::eq(&*e.key, key)) {
                return Some(i);
            }
        }
        self.locate(&Probe::new(key)).ok()
    }

    /// Add an entry for a key known to be absent, `located` being what
    /// `locate` returned for it. Sends no messages.
    pub(crate) fn push(&mut self, hash: NSUInteger, key: Retained<AnyObject>, value: V, located: usize) {
        let mut slot = located;
        let len = self.entries.len();
        if self.index.is_empty() {
            if len >= SCAN {
                self.rebuild(((len + 1) * 2).next_power_of_two());
                slot = self.free_slot(hash);
            }
        } else if (len + 1) * 2 > self.index.len() {
            self.rebuild(self.index.len() * 2);
            slot = self.free_slot(hash);
        }
        if !self.index.is_empty() {
            self.index[slot] = len as u32;
        }
        self.entries.push(Entry { hash, key, value });
    }

    /// Add a pair; an equal key already present keeps its place, and its
    /// value is replaced and returned.
    pub(crate) fn insert(&mut self, key: Retained<AnyObject>, value: V) -> Option<V> {
        let probe = Probe::new(&key);
        let (hash, found) = (probe.hash, self.locate(&probe));
        match found {
            Ok(i) => Some(std::mem::replace(&mut self.entries[i].value, value)),
            Err(slot) => {
                self.push(hash, key, value, slot);
                None
            }
        }
    }

    pub(crate) fn value_mut(&mut self, at: usize) -> &mut V {
        &mut self.entries[at].value
    }

    /// Take out the entry at `at`; the last entry moves into its place.
    /// Sends no messages: the caller releases the entry.
    pub(crate) fn remove(&mut self, at: usize) -> Entry<V> {
        if !self.index.is_empty() {
            let hole = self.slot_of(at);
            self.close(hole);
            let last = self.entries.len() - 1;
            if at != last {
                let slot = self.slot_of(last);
                self.index[slot] = at as u32;
            }
        }
        self.entries.swap_remove(at)
    }

    /// The first free slot from `hash`'s home.
    fn free_slot(&self, hash: NSUInteger) -> usize {
        let mask = self.index.len() - 1;
        let mut slot = home(hash, mask);
        while self.index[slot] != FREE {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// The index slot holding entry `at`.
    fn slot_of(&self, at: usize) -> usize {
        let mask = self.index.len() - 1;
        let mut slot = home(self.entries[at].hash, mask);
        while self.index[slot] != at as u32 {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// Free `hole`, moving back later slots of the same run whose probe
    /// sequence passes through it.
    fn close(&mut self, mut hole: usize) {
        let mask = self.index.len() - 1;
        self.index[hole] = FREE;
        let mut slot = hole;
        loop {
            slot = (slot + 1) & mask;
            let at = self.index[slot];
            if at == FREE {
                return;
            }
            let start = home(self.entries[at as usize].hash, mask);
            // The entry may move back if the hole lies between its home and
            // where it sits now.
            if hole.wrapping_sub(start) & mask < slot.wrapping_sub(start) & mask {
                self.index[hole] = at;
                self.index[slot] = FREE;
                hole = slot;
            }
        }
    }

    fn rebuild(&mut self, size: usize) {
        self.index = vec![FREE; size].into();
        for i in 0..self.entries.len() {
            let slot = self.free_slot(self.entries[i].hash);
            self.index[slot] = i as u32;
        }
    }
}

impl<V: Clone> Clone for Table<V> {
    fn clone(&self) -> Self {
        let entries =
            self.entries.iter().map(|e| Entry { hash: e.hash, key: e.key.clone(), value: e.value.clone() }).collect();
        Table { entries, index: self.index.clone() }
    }
}

/// An immutable collection's table: its own, or shared with the mutable
/// collection it was copied from (and that one's other copies).
pub(crate) enum Frozen<V> {
    Owned(Table<V>),
    Shared(Arc<Table<V>>),
}

impl<V> Default for Frozen<V> {
    fn default() -> Self {
        Frozen::Owned(Table::default())
    }
}

impl<V> From<Table<V>> for Frozen<V> {
    fn from(table: Table<V>) -> Self {
        Frozen::Owned(table)
    }
}

impl<V> Deref for Frozen<V> {
    type Target = Table<V>;

    #[inline]
    fn deref(&self) -> &Table<V> {
        match self {
            Frozen::Owned(table) => table,
            Frozen::Shared(table) => table,
        }
    }
}

/// A mutable collection's table: its own, or, since a copy, shared with
/// the copies until the next change.
pub(crate) enum Thawed<V> {
    Own(Table<V>),
    Shared(Arc<Table<V>>),
}

impl<V> Deref for Thawed<V> {
    type Target = Table<V>;

    #[inline]
    fn deref(&self) -> &Table<V> {
        match self {
            Thawed::Own(table) => table,
            Thawed::Shared(table) => table,
        }
    }
}

/// The table of a mutable collection, copied on write.
///
/// Reads borrow it. A change first copies a table that copies still share
/// (retaining every key and value, with only a read borrow held, so the
/// retains may run code that reads the collection), then borrows it
/// mutably with no other code running. A table no copy shares any more is
/// taken back without copying.
pub(crate) struct CowTable<V>(RefCell<Thawed<V>>);

impl<V> Default for CowTable<V> {
    fn default() -> Self {
        CowTable(RefCell::new(Thawed::Own(Table::default())))
    }
}

impl<V> From<Table<V>> for CowTable<V> {
    fn from(table: Table<V>) -> Self {
        CowTable(RefCell::new(Thawed::Own(table)))
    }
}

impl<V> From<Thawed<V>> for CowTable<V> {
    fn from(table: Thawed<V>) -> Self {
        CowTable(RefCell::new(table))
    }
}

impl<V: Clone> CowTable<V> {
    #[inline]
    pub(crate) fn read(&self) -> Ref<'_, Table<V>> {
        Ref::map(self.0.borrow(), |table| &**table)
    }

    /// The table for changing. `owner` names the collection for the
    /// failure when a change comes from inside one of its reading methods.
    #[inline]
    pub(crate) fn write(&self, owner: &str, obj: *const AnyObject) -> RefMut<'_, Table<V>> {
        let storage = self.storage(owner, obj);
        if let Thawed::Own(_) = &*storage {
            return RefMut::map(storage, |table| match table {
                Thawed::Own(table) => table,
                Thawed::Shared(_) => unreachable!("just checked"),
            });
        }
        drop(storage);
        self.unshare(owner, obj)
    }

    #[cold]
    fn unshare(&self, owner: &str, obj: *const AnyObject) -> RefMut<'_, Table<V>> {
        let copy = match &*self.0.borrow() {
            Thawed::Shared(shared) if Arc::strong_count(shared) > 1 => Some(Table::clone(shared)),
            _ => None,
        };
        if let Some(copy) = copy {
            let old = std::mem::replace(&mut *self.storage(owner, obj), Thawed::Own(copy));
            // Released with no borrow held: the copies may be gone by now.
            drop(old);
        }
        RefMut::map(self.storage(owner, obj), |table| {
            if let Thawed::Shared(shared) = table {
                *table = Thawed::Own(std::mem::take(Arc::make_mut(shared)));
            }
            match table {
                Thawed::Own(table) => table,
                Thawed::Shared(_) => unreachable!("just owned"),
            }
        })
    }

    /// Put `new` in place of the whole table, returning the old one for
    /// the caller to release once nothing is borrowed.
    pub(crate) fn replace(&self, new: Thawed<V>, owner: &str, obj: *const AnyObject) -> Thawed<V> {
        std::mem::replace(&mut *self.storage(owner, obj), new)
    }

    /// The table, shared for a copy to hold, without retaining anything.
    /// `None` while the collection is being read, when the copy takes a
    /// table of its own instead.
    pub(crate) fn share(&self) -> Option<Arc<Table<V>>> {
        let mut storage = self.0.try_borrow_mut().ok()?;
        if let Thawed::Own(table) = &mut *storage {
            let table = std::mem::take(table);
            // Atomically counted although the entries aren't `Send`: an
            // immutable copy may be released on any thread, as Objective-C
            // allows, and objc_release is thread-safe.
            #[allow(clippy::arc_with_non_send_sync)]
            let shared = Arc::new(table);
            *storage = Thawed::Shared(shared);
        }
        match &*storage {
            Thawed::Shared(shared) => Some(shared.clone()),
            Thawed::Own(_) => unreachable!("just shared"),
        }
    }

    fn storage(&self, owner: &str, obj: *const AnyObject) -> RefMut<'_, Thawed<V>> {
        match self.0.try_borrow_mut() {
            Ok(storage) => storage,
            Err(_) => crate::util::mutated_while_reading(owner, obj),
        }
    }
}
