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
//! Lookups there read the table without counting themselves as readers
//! when they can decide without messages, which is whenever the keys they
//! meet are the key sought or Sidestep's own strings and numbers.

use std::ops::Deref;
use std::ptr;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::NSUInteger;

use crate::guarded::{Cow, counted};
use crate::number::{Number, fast_value};
use crate::string::fast_parts;

/// Up to this many entries, a table has no index.
pub(crate) const SCAN: usize = 4;
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
        match self.matches_quietly(entry) {
            Some(verdict) => verdict,
            // SAFETY: -isEqual: takes an object and returns BOOL.
            None => unsafe { objc2::msg_send![&*entry.key, isEqual: self.key] },
        }
    }

    /// Whether `entry`'s key matches, if that can be told without sending
    /// a message.
    #[inline]
    fn matches_quietly<V>(&self, entry: &Entry<V>) -> Option<bool> {
        if entry.hash != self.hash {
            return Some(false);
        }
        if ptr::eq(&*entry.key, self.key) {
            return Some(true);
        }
        match self.fast {
            Fast::Text(text) => fast_parts(&entry.key).map(|(other, _)| text == other),
            Fast::Number(n) => fast_value(&entry.key).map(|other| n.equals(&other)),
            Fast::Other => None,
        }
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

    /// `locate`, if it can be done without sending a message; `None` if
    /// some key would have to be asked.
    pub(crate) fn locate_quietly(&self, probe: &Probe) -> Option<Result<usize, usize>> {
        if self.index.is_empty() {
            for (i, e) in self.entries.iter().enumerate() {
                if probe.matches_quietly(e)? {
                    return Some(Ok(i));
                }
            }
            return Some(Err(0));
        }
        let mask = self.index.len() - 1;
        let mut slot = home(probe.hash, mask);
        loop {
            match self.index[slot] {
                FREE => return Some(Err(slot)),
                at if probe.matches_quietly(&self.entries[at as usize])? => return Some(Ok(at as usize)),
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// The position of the very object `key`, in a table small enough that
    /// looking for it first pays. Sends no messages.
    #[inline]
    pub(crate) fn identical(&self, key: &AnyObject) -> Option<usize> {
        if self.entries.len() <= IDENTITY { self.entries.iter().position(|e| ptr::eq(&*e.key, key)) } else { None }
    }

    pub(crate) fn get(&self, key: &AnyObject) -> Option<&Entry<V>> {
        self.position(key).map(|i| &self.entries[i])
    }

    pub(crate) fn position(&self, key: &AnyObject) -> Option<usize> {
        // Callers mostly look up with the key object they stored.
        self.identical(key).or_else(|| self.locate(&Probe::new(key)).ok())
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

    /// Add a pair unless an equal key is present, as Foundation's immutable
    /// collections are built: the first of equal keys, with its value,
    /// wins. Returns the pair when it was left out.
    pub(crate) fn insert_first(&mut self, key: Retained<AnyObject>, value: V) -> Option<(Retained<AnyObject>, V)> {
        let probe = Probe::new(&key);
        let (hash, found) = (probe.hash, self.locate(&probe));
        match found {
            Ok(_) => Some((key, value)),
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

impl Table<()> {
    /// A table of `members`, each kept unless an equal one came before.
    pub(crate) fn of_members(members: impl IntoIterator<Item = Retained<AnyObject>>) -> Table<()> {
        let members = members.into_iter();
        let mut table = Table::with_capacity(members.size_hint().0);
        for member in members {
            table.insert_first(member, ());
        }
        table
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

/// A table behind a count of references, as mutable collections and their
/// copies share it.
pub(crate) fn shared<V>(table: Table<V>) -> Arc<Table<V>> {
    counted(table)
}

/// The table of a mutable collection, copied on write (see [`Cow`]): a
/// change first copies a table that copies still share, then takes the
/// table with no other code running.
pub(crate) type CowTable<V> = Cow<Table<V>>;

impl<V> From<Table<V>> for Cow<Table<V>> {
    fn from(table: Table<V>) -> Self {
        Cow::fresh(table)
    }
}

impl<V> From<Arc<Table<V>>> for Cow<Table<V>> {
    fn from(table: Arc<Table<V>>) -> Self {
        Cow::shared(table)
    }
}

impl<V: Clone> Cow<Table<V>> {
    /// Where `probe`'s key is (`Ok`) or would go (`Err`), as
    /// `Table::locate`, reading the table without counting when no key
    /// needs a message. Valid until the table next changes.
    #[inline]
    pub(crate) fn locate(&self, probe: &Probe) -> Result<usize, usize> {
        // SAFETY: a search that sends no messages runs no other code.
        match unsafe { self.peek() }.locate_quietly(probe) {
            Some(found) => found,
            None => self.read().locate(probe),
        }
    }

    /// The position of an entry whose key equals `key`, as
    /// `Table::position`. Valid until the table next changes.
    #[inline]
    pub(crate) fn position(&self, key: &AnyObject) -> Option<usize> {
        // SAFETY: looking for the very object runs no other code.
        if let Some(at) = unsafe { self.peek() }.identical(key) {
            return Some(at);
        }
        // Hashed with nothing held: -hash may run any code.
        self.locate(&Probe::new(key)).ok()
    }

    /// The table for changing, with the position of the very object `key`
    /// in it, when the table is this collection's alone and small enough
    /// to look through: the common removal, which needs no hashing.
    ///
    /// # Safety
    /// As for `Guarded::write`: no other code runs while the table is used.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn write_identical(
        &self,
        owner: &str,
        obj: *const AnyObject,
        key: &AnyObject,
    ) -> Option<(&mut Table<V>, usize)> {
        // SAFETY: guaranteed by the caller; the table is only searched
        // without messages before it is returned.
        let table = unsafe { self.write_alone(owner, obj) }?;
        let at = table.identical(key)?;
        Some((table, at))
    }

    /// Where `probe`'s key is (`Ok`) or would go (`Err`), with the table for
    /// changing there, if `needed` says the caller changes it for what was
    /// found. When the table is this collection's alone and no key needs a
    /// message, that is one look; otherwise the key is found with the table
    /// read, and the table made this collection's alone.
    ///
    /// # Safety
    /// As for `Guarded::write`: no other code runs while the table is used.
    #[inline]
    #[allow(clippy::type_complexity, clippy::mut_from_ref)]
    pub(crate) unsafe fn write_located(
        &self,
        owner: &str,
        obj: *const AnyObject,
        probe: &Probe,
        needed: impl Fn(&Result<usize, usize>) -> bool,
    ) -> Option<(&mut Table<V>, Result<usize, usize>)> {
        // SAFETY: guaranteed by the caller; the table is only searched
        // without messages before it is returned.
        if let Some(table) = unsafe { self.write_alone(owner, obj) }
            && let Some(found) = table.locate_quietly(probe)
        {
            return needed(&found).then_some((table, found));
        }
        let mut found = self.locate(probe);
        if !needed(&found) {
            return None;
        }
        // SAFETY: the table is only looked at.
        if unsafe { self.write_alone(owner, obj) }.is_none() {
            self.unshare(owner, obj);
            // The copy has the same layout, but look again in case copying
            // ran code.
            found = self.locate(probe);
            if !needed(&found) {
                return None;
            }
        }
        // SAFETY: guaranteed by the caller; nothing ran since the key was
        // located.
        Some((unsafe { self.write(owner, obj) }, found))
    }
}
