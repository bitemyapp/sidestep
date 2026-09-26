//! `NSDictionary`: an immutable map.
//!
//! Entries sit in a vector with each key's hash, computed once. The smallest
//! dictionaries are searched in order; larger ones add an open-addressed
//! index. In small ones (attribute dictionaries, mostly) a lookup first
//! looks for the very key object, which CFDictionary also treats as a match.
//! Otherwise it compares hashes and sends `-isEqual:` only when they match. Keys whose class is exactly one of
//! Sidestep's strings are hashed and compared without sending messages.

use std::ptr::{self, NonNull};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSCopying, NSUInteger, NSZone};

use crate::string::fast_parts;

/// Up to this many entries, a dictionary has no index.
const SCAN: usize = 4;
/// Up to this many entries, a lookup first looks for the very key object,
/// which needs no hashing.
const IDENTITY: usize = 8;
/// A free slot in the index.
const FREE: u32 = u32::MAX;

struct Entry {
    hash: NSUInteger,
    key: Retained<AnyObject>,
    value: Retained<AnyObject>,
}

#[derive(Default)]
pub(crate) struct DictionaryIvars {
    entries: Vec<Entry>,
    /// Positions in `entries`, by hash: a power-of-two table at most half
    /// full, or empty for dictionaries searched in order.
    index: Box<[u32]>,
}

/// A key to look for: its hash and, for Sidestep's own strings, its text.
struct Probe<'a> {
    key: &'a AnyObject,
    hash: NSUInteger,
    text: Option<&'a str>,
}

impl<'a> Probe<'a> {
    fn new(key: &'a AnyObject) -> Self {
        match fast_parts(key) {
            Some((text, hash)) => Probe { key, hash, text: Some(text) },
            // SAFETY: -hash takes nothing and returns NSUInteger.
            None => Probe { key, hash: unsafe { msg_send![key, hash] }, text: None },
        }
    }

    fn matches(&self, entry: &Entry) -> bool {
        if entry.hash != self.hash {
            return false;
        }
        if ptr::eq(&*entry.key, self.key) {
            return true;
        }
        if let (Some(text), Some((other, _))) = (self.text, fast_parts(&entry.key)) {
            return text == other;
        }
        // SAFETY: -isEqual: takes an object and returns BOOL.
        unsafe { msg_send![&*entry.key, isEqual: self.key] }
    }
}

/// Where a hash starts probing. Keys' own hashes can be weak in their low
/// bits (addresses, small integers), so they are spread first.
fn home(hash: NSUInteger, mask: usize) -> usize {
    ((hash as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32) as usize & mask
}

impl DictionaryIvars {
    fn with_capacity(count: usize) -> Self {
        let index = if count > SCAN { vec![FREE; (count * 2).next_power_of_two()].into() } else { Box::default() };
        DictionaryIvars { entries: Vec::with_capacity(count), index }
    }

    /// The entry matching `probe`, or else the free index slot where it
    /// would go (meaningless without an index).
    fn locate(&self, probe: &Probe) -> Result<usize, usize> {
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

    fn get(&self, key: &AnyObject) -> Option<&Entry> {
        if self.entries.len() <= IDENTITY {
            // Callers mostly look up with the key object they stored.
            if let Some(entry) = self.entries.iter().find(|e| ptr::eq(&*e.key, key)) {
                return Some(entry);
            }
        }
        self.locate(&Probe::new(key)).ok().map(|i| &self.entries[i])
    }

    /// Add a pair; an equal key already present keeps its place and takes
    /// the new value.
    fn insert(&mut self, key: Retained<AnyObject>, value: Retained<AnyObject>) {
        let probe = Probe::new(&key);
        let (hash, found) = (probe.hash, self.locate(&probe));
        match found {
            Ok(i) => self.entries[i].value = value,
            Err(slot) => {
                if !self.index.is_empty() {
                    self.index[slot] = self.entries.len() as u32;
                }
                self.entries.push(Entry { hash, key, value });
            }
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSDictionary"]
    #[ivars = DictionaryIvars]
    pub(crate) struct NSDictionaryImpl;

    impl NSDictionaryImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(DictionaryIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithObjects:forKeys:count:))]
        fn init_with_objects(
            this: Allocated<Self>,
            objects: *mut NonNull<AnyObject>,
            keys: *mut NonNull<ProtocolObject<dyn NSCopying>>,
            count: NSUInteger,
        ) -> Retained<Self> {
            let mut ivars = DictionaryIvars::with_capacity(count);
            for i in 0..count {
                // SAFETY: the caller passes `count` keys and objects.
                let (key, object) = unsafe { ((*keys.add(i)).as_ref(), (*objects.add(i)).as_ref()) };
                let key: &AnyObject = key.as_ref();
                // Keys are copied, as Foundation promises. Sidestep's strings
                // are immutable, so a copy of one is itself.
                let key: Retained<AnyObject> = match fast_parts(key) {
                    Some(_) => key.retain(),
                    // SAFETY: keys conform to NSCopying.
                    None => unsafe { msg_send![key, copy] },
                };
                ivars.insert(key, object.retain());
            }
            let this = this.set_ivars(ivars);
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().entries.len()
        }

        /// Neither retained nor autoreleased: the dictionary keeps it alive.
        #[unsafe(method(objectForKey:))]
        fn object_for_key(&self, key: Option<&AnyObject>) -> *mut AnyObject {
            match key.and_then(|k| self.ivars().get(k)) {
                Some(entry) => Retained::as_ptr(&entry.value).cast_mut(),
                None => ptr::null_mut(),
            }
        }

        #[unsafe(method(getObjects:andKeys:count:))]
        fn get_objects_and_keys_count(
            &self,
            objects: *mut *mut AnyObject,
            keys: *mut *mut AnyObject,
            count: NSUInteger,
        ) {
            fill(self, objects, keys, count);
        }

        #[unsafe(method(getObjects:andKeys:))]
        fn get_objects_and_keys(&self, objects: *mut *mut AnyObject, keys: *mut *mut AnyObject) {
            fill(self, objects, keys, usize::MAX);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSDictionaryImpl {}
);

/// Write up to `count` objects and keys, unretained, into either array.
fn fill(dict: &NSDictionaryImpl, objects: *mut *mut AnyObject, keys: *mut *mut AnyObject, count: usize) {
    for (i, entry) in dict.ivars().entries.iter().take(count).enumerate() {
        // SAFETY: the caller passes room for `count` entries, or for all of
        // them when there is no count; either pointer may be null.
        unsafe {
            if !objects.is_null() {
                *objects.add(i) = Retained::as_ptr(&entry.value).cast_mut();
            }
            if !keys.is_null() {
                *keys.add(i) = Retained::as_ptr(&entry.key).cast_mut();
            }
        }
    }
}
