//! Attribute dictionaries interned per text storage.
//!
//! A text storage's runs hold an [`AttrId`] rather than a dictionary, so a
//! paragraph's runs are plain numbers, runs with equal attributes merge
//! (as AppKit's text storage uniques its attribute dictionaries), and the
//! layout manager can resolve each distinct dictionary into the text
//! engine's attributes once.
//!
//! Interning looks a dictionary up by address first, which is how most
//! arrive (an app setting the same dictionary on many ranges, typing
//! attributes, a run's own dictionary handed back), then by a hash of its
//! keys and values and `isEqual:`. The table keeps an immutable copy of
//! each dictionary it interns, and keeps every dictionary it answers by
//! address alive, so no address is reused for another dictionary while the
//! table remembers it.
//!
//! Ids are never reused while runs may hold them. When the table has grown
//! well past what the text uses, the storage compacts it
//! ([`AttrTable::compact`]) and renumbers its runs; the table's epoch
//! changes, which tells a layout manager to forget what it resolved.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{Message, msg_send};
use objc2_foundation::{NSDictionary, NSString};
use smallvec::SmallVec;

use super::storage::AttrId;
use crate::text::layout::FxBuild;

pub(crate) type Dict = NSDictionary<NSString, AnyObject>;

/// The empty dictionary's id, which every table has.
pub(crate) const EMPTY: AttrId = 0;

/// Remembered addresses beyond the table's own dictionaries, at most.
const MAX_ALIASES: usize = 4096;

struct Entry {
    dict: Retained<Dict>,
    hash: u64,
}

pub(crate) struct AttrTable {
    entries: Vec<Entry>,
    /// Addresses of dictionaries known, the table's and aliases.
    by_address: HashMap<usize, AttrId, FxBuild>,
    /// Dictionaries answered by address that aren't the table's own copy,
    /// kept alive so their addresses stay theirs.
    aliases: Vec<Retained<Dict>>,
    by_hash: HashMap<u64, SmallVec<[AttrId; 2]>, FxBuild>,
    /// Changes when ids are renumbered.
    epoch: u64,
    /// Compact when the table grows past this.
    next_compaction: usize,
}

impl AttrTable {
    pub fn new() -> AttrTable {
        let empty = Dict::new();
        let mut t = AttrTable {
            entries: Vec::new(),
            by_address: HashMap::default(),
            aliases: Vec::new(),
            by_hash: HashMap::default(),
            epoch: 0,
            next_compaction: 256,
        };
        t.add(empty, 0);
        t
    }

    /// The dictionary of `id`.
    pub fn dict(&self, id: AttrId) -> &Retained<Dict> {
        &self.entries[id as usize].dict
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    fn add(&mut self, dict: Retained<Dict>, hash: u64) -> AttrId {
        let id = self.entries.len() as AttrId;
        self.by_address.insert(Retained::as_ptr(&dict) as usize, id);
        self.by_hash.entry(hash).or_default().push(id);
        self.entries.push(Entry { dict, hash });
        id
    }

    /// The id of a dictionary known by its address.
    pub fn by_address(&self, dict: &Dict) -> Option<AttrId> {
        self.by_address.get(&(dict as *const Dict as usize)).copied()
    }

    /// The dictionaries with `hash`, which may equal one that has it.
    fn candidates(&self, hash: u64) -> SmallVec<[(AttrId, Retained<Dict>); 2]> {
        self.by_hash
            .get(&hash)
            .map(|ids| ids.iter().map(|&id| (id, self.entries[id as usize].dict.clone())).collect())
            .unwrap_or_default()
    }

    /// Answer `dict`'s address with `id` from now on.
    fn alias(&mut self, dict: Retained<Dict>, id: AttrId) {
        if self.aliases.len() >= MAX_ALIASES {
            for old in self.aliases.drain(..) {
                self.by_address.remove(&(Retained::as_ptr(&old) as usize));
            }
        }
        self.by_address.insert(Retained::as_ptr(&dict) as usize, id);
        self.aliases.push(dict);
    }

    /// Whether the table has grown enough that compacting may pay.
    pub fn wants_compaction(&self) -> bool {
        self.entries.len() > self.next_compaction
    }

    /// Keep only the ids `used` marks (the empty one always), numbering
    /// them anew: the map from old ids to new ones.
    pub fn compact(&mut self, used: &[bool]) -> Vec<AttrId> {
        let old = std::mem::take(&mut self.entries);
        self.by_address.clear();
        self.by_hash.clear();
        self.aliases.clear();
        let mut map = vec![EMPTY; old.len()];
        for (i, entry) in old.into_iter().enumerate() {
            if i == EMPTY as usize || used.get(i).copied().unwrap_or(false) {
                map[i] = self.add(entry.dict, entry.hash);
            }
        }
        self.epoch += 1;
        self.next_compaction = (self.entries.len() * 2).max(256);
        map
    }
}

/// The id of `dict`'s attributes in `table` (the empty one for nil or an
/// empty dictionary), interning them if they are new. The messages this
/// sends (`copy`, `hash`, `isEqual:`) go with no borrow of the table held.
pub(crate) fn intern(table: &RefCell<AttrTable>, dict: Option<&Dict>) -> AttrId {
    let Some(dict) = dict else { return EMPTY };
    if let Some(id) = table.borrow().by_address(dict) {
        return id;
    }
    if dict.count() == 0 {
        return EMPTY;
    }
    // An immutable copy: an immutable dictionary is its own copy, and a
    // mutable one the app may change later is copied.
    // SAFETY: -copy of a dictionary is an immutable dictionary.
    let copy: Retained<Dict> = unsafe { msg_send![dict, copy] };
    let hash = hash_of(&copy);
    let candidates = table.borrow().candidates(hash);
    let found = candidates.into_iter().find_map(|(id, known)| {
        // SAFETY: isEqual: takes an object.
        let equal: bool = unsafe { msg_send![&*known, isEqual: &*copy] };
        equal.then_some(id)
    });
    let immutable = std::ptr::eq(&*copy, dict);
    let mut t = table.borrow_mut();
    match found {
        Some(id) => {
            if immutable {
                t.alias(copy, id);
            }
            id
        }
        None => t.add(copy, hash),
    }
}

/// For each distinct id among `ids`, the id of the dictionary `f` makes of
/// its dictionary: a map for an edit that changes every run's attributes
/// the same way (adding an attribute to a range, say).
pub(crate) fn map_each(
    table: &RefCell<AttrTable>,
    ids: impl IntoIterator<Item = AttrId>,
    mut f: impl FnMut(&Dict) -> Retained<Dict>,
) -> Vec<(AttrId, AttrId)> {
    let mut out: Vec<(AttrId, AttrId)> = Vec::new();
    for id in ids {
        if out.iter().any(|&(old, _)| old == id) {
            continue;
        }
        let dict = table.borrow().dict(id).clone();
        let new = f(&dict);
        out.push((id, intern(table, Some(&new))));
    }
    out
}

/// A hash of a dictionary's keys and values, the same whatever order they
/// come in: equal dictionaries hash equally where their values' hashes
/// follow `isEqual:`, as Foundation asks.
fn hash_of(dict: &Dict) -> u64 {
    let (keys, values) = dict.to_vecs();
    let mut h = keys.len() as u64;
    for (k, v) in keys.iter().zip(&values) {
        // SAFETY: every object answers -hash.
        let (kh, vh): (usize, usize) = unsafe { (msg_send![&**k, hash], msg_send![&**v, hash]) };
        let pair = (kh as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (vh as u64).rotate_left(29);
        h = h.wrapping_add(pair.wrapping_mul(0xBF58_476D_1CE4_E5B9));
    }
    h
}

/// The attributes `base` has, with `key` set to `value` (or removed, for
/// none).
pub(crate) fn with_value(base: &Dict, key: &NSString, value: Option<&AnyObject>) -> Retained<Dict> {
    // SAFETY: -mutableCopy of a dictionary is a mutable dictionary with its
    // entries.
    let m: Retained<objc2_foundation::NSMutableDictionary<NSString, AnyObject>> =
        unsafe { msg_send![base, mutableCopy] };
    // SAFETY: setObject:forKey: copies the key and retains the value;
    // removeObjectForKey: takes a key.
    unsafe {
        match value {
            Some(v) => {
                let _: () = msg_send![&*m, setObject: v, forKey: key];
            }
            None => {
                let _: () = msg_send![&*m, removeObjectForKey: key];
            }
        }
    }
    // SAFETY: an NSMutableDictionary is an NSDictionary.
    unsafe { Retained::cast_unchecked(m) }
}

/// The attributes `base` has, with every entry of `extra` added.
pub(crate) fn merged(base: &Dict, extra: &Dict) -> Retained<Dict> {
    if base.count() == 0 {
        return extra.retain();
    }
    // SAFETY: as in `with_value`.
    let m: Retained<objc2_foundation::NSMutableDictionary<NSString, AnyObject>> =
        unsafe { msg_send![base, mutableCopy] };
    // SAFETY: addEntriesFromDictionary: takes a dictionary.
    let _: () = unsafe { msg_send![&*m, addEntriesFromDictionary: extra] };
    // SAFETY: an NSMutableDictionary is an NSDictionary.
    unsafe { Retained::cast_unchecked(m) }
}
