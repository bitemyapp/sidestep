//! `NSDictionary` and `NSMutableDictionary`: maps kept in the hash table of
//! `table`.
//!
//! An immutable dictionary's table never changes after `init` (it may be
//! shared, read-only, with the mutable dictionary it was copied from), so
//! readers on any thread need no locking. A mutable one keeps its table in
//! a `CowTable`, with a count of changes that fast enumeration watches.
//! As with arrays, methods that read a dictionary are written once, on
//! `NSDictionary`, over a table: the immutable one's, the mutable one's
//! while it is read, or one gathered through the primitives of a subclass
//! defined outside Sidestep. Lookups and changes are defined on each class
//! directly, and a subclass's non-primitive changes go through its
//! `-setObject:forKey:` and `-removeObjectForKey:`.
//!
//! A change first finds its key with the table only read (the keys'
//! `-isEqual:` may run arbitrary code, which may read the dictionary), then
//! applies the change with no other code running, and releases anything it
//! replaced or removed afterwards. Copies of a mutable dictionary share its
//! table until it next changes (see `CowTable`).

use std::ffi::c_ulong;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSComparator, NSCopying, NSDictionary, NSEnumerationOptions, NSEnumerator, NSFastEnumerationState,
    NSMutableDictionary, NSSet, NSSortOptions, NSString, NSUInteger, NSZone,
};

use crate::enumerator::{self, Mutations, Source, immutable_mutations};
use crate::guarded::Reading;
use crate::table::{CowTable, Entry, Frozen, Probe, Table, shared};
use crate::util::{self, equal, inherits, is_exactly};
use crate::{array, describe, set};

type Values = Table<Retained<AnyObject>>;

const MUTABLE: &str = "NSMutableDictionary";

#[derive(Default)]
pub(crate) struct MutableDictionaryIvars {
    table: CowTable<Retained<AnyObject>>,
    mutations: Mutations,
}

/// Which storage a dictionary object has.
enum Kind<'a> {
    Fixed(&'a NSDictionaryImpl),
    Mutable(&'a NSMutableDictionaryImpl),
    /// A subclass from outside Sidestep, reached through messages.
    Foreign,
}

fn kind(obj: &AnyObject) -> Kind<'_> {
    if is_exactly(obj, &crate::NSDICTIONARY) {
        // SAFETY: an instance of exactly NSDictionaryImpl.
        Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSDictionaryImpl>() })
    } else if is_exactly(obj, &crate::NSMUTABLEDICTIONARY) {
        // SAFETY: an instance of exactly NSMutableDictionaryImpl.
        Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableDictionaryImpl>() })
    } else {
        Kind::Foreign
    }
}

/// The storage `obj` has from Sidestep's classes, a subclass's included:
/// what the inherited `-keyEnumerator` and `-objectEnumerator` walk.
/// `Foreign` for objects that are not dictionaries.
fn storage(obj: &AnyObject) -> Kind<'_> {
    match kind(obj) {
        Kind::Foreign if inherits(obj, &crate::NSMUTABLEDICTIONARY) => {
            // SAFETY: an instance of a subclass of NSMutableDictionaryImpl,
            // whose ivars sit where the superclass's methods expect them,
            // set by the initializer it inherits.
            Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableDictionaryImpl>() })
        }
        Kind::Foreign if inherits(obj, &crate::NSDICTIONARY) => {
            // SAFETY: as above, for NSDictionaryImpl.
            Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSDictionaryImpl>() })
        }
        kind => kind,
    }
}

/// A dictionary's table, held for reading.
enum Tables<'a> {
    Fixed(&'a Values),
    Mutable(Reading<'a, Arc<Values>>),
    Gathered(Values),
}

impl Deref for Tables<'_> {
    type Target = Values;

    fn deref(&self) -> &Values {
        match self {
            Tables::Fixed(table) => table,
            Tables::Mutable(table) => table,
            Tables::Gathered(table) => table,
        }
    }
}

fn table(obj: &AnyObject) -> Tables<'_> {
    match kind(obj) {
        Kind::Fixed(d) => Tables::Fixed(d.ivars()),
        Kind::Mutable(m) => Tables::Mutable(m.ivars().table.read()),
        Kind::Foreign => Tables::Gathered(gather(obj)),
    }
}

/// The entries of a dictionary subclass defined outside Sidestep, through
/// its primitive methods.
fn gather(obj: &AnyObject) -> Values {
    // SAFETY: NSDictionary's primitives: -count, -keyEnumerator returning an
    // enumerator of the keys, and -objectForKey: returning an object for
    // each.
    unsafe {
        let count: NSUInteger = msg_send![obj, count];
        let keys: Retained<NSEnumerator> = msg_send![obj, keyEnumerator];
        let mut table = Table::with_capacity(count);
        while let Some(key) = keys.nextObject() {
            let value: Option<Retained<AnyObject>> = msg_send![obj, objectForKey: &*key];
            if let Some(value) = value {
                table.insert(key, value);
            }
        }
        table
    }
}

/// The key and value at position `index` of a dictionary's own entries,
/// unretained (the dictionary keeps them alive), or `None` past the end.
pub(crate) fn entry_at(obj: &AnyObject, index: usize) -> Option<(*mut AnyObject, *mut AnyObject)> {
    let entry = |table: &Values| {
        table.entries().get(index).map(|e| (Retained::as_ptr(&e.key).cast_mut(), Retained::as_ptr(&e.value).cast_mut()))
    };
    match storage(obj) {
        Kind::Fixed(d) => entry(d.ivars()),
        // SAFETY: reading two pointers runs no other code.
        Kind::Mutable(m) => entry(unsafe { m.ivars().table.peek() }),
        Kind::Foreign => None,
    }
}

/// A dictionary's count of changes, and of its own entries; `None` for
/// objects that aren't dictionaries.
pub(crate) fn state(obj: &AnyObject) -> Option<(*mut c_ulong, usize)> {
    match storage(obj) {
        Kind::Fixed(d) => Some((immutable_mutations(), d.ivars().len())),
        // SAFETY: reading the length runs no other code.
        Kind::Mutable(m) => Some((m.ivars().mutations.as_ptr(), unsafe { m.ivars().table.peek() }.len())),
        Kind::Foreign => None,
    }
}

/// The mutation count fast enumeration of `obj` watches.
pub(crate) fn mutations(obj: &AnyObject) -> *mut c_ulong {
    match storage(obj) {
        Kind::Mutable(m) => m.ivars().mutations.as_ptr(),
        _ => immutable_mutations(),
    }
}

/// The value for `key` in any dictionary, retained.
fn lookup(obj: &AnyObject, key: &AnyObject) -> Option<Retained<AnyObject>> {
    match kind(obj) {
        Kind::Fixed(d) => d.ivars().get(key).map(|e| e.value.clone()),
        Kind::Mutable(m) => {
            let found = m.find(key);
            // SAFETY: the element is alive; retaining it is all that runs.
            found.map(|v| unsafe { Retained::retain(v) }.expect("non-null"))
        }
        // SAFETY: -objectForKey: takes an object and returns one or nil.
        Kind::Foreign => unsafe { msg_send![obj, objectForKey: key] },
    }
}

fn count_of(obj: &AnyObject) -> usize {
    match kind(obj) {
        Kind::Fixed(d) => d.ivars().len(),
        // SAFETY: reading the length runs no other code.
        Kind::Mutable(m) => unsafe { m.ivars().table.peek() }.len(),
        // SAFETY: -count takes nothing and returns NSUInteger.
        Kind::Foreign => unsafe { msg_send![obj, count] },
    }
}

/// How a new dictionary takes equal keys: an immutable one keeps the first
/// pair, as Foundation's do; a mutable one is built as by `-setObject:forKey:`,
/// keeping the first key with the last value.
#[derive(Clone, Copy, PartialEq)]
enum Duplicates {
    FirstWins,
    LastValueWins,
}

impl Duplicates {
    fn add(self, table: &mut Values, key: Retained<AnyObject>, value: Retained<AnyObject>) {
        let spare = match self {
            Duplicates::FirstWins => table.insert_first(key, value).map(|(k, v)| (Some(k), v)),
            Duplicates::LastValueWins => table.insert(key, value).map(|v| (None, v)),
        };
        drop(spare);
    }
}

/// A table of `count` objects and keys from C arrays, keys copied.
///
/// # Safety
/// `objects` and `keys` must each point to `count` object pointers (or be
/// anything when `count` is 0).
unsafe fn from_c_arrays(
    receiver: &str,
    objects: *const *mut AnyObject,
    keys: *const *mut AnyObject,
    count: usize,
    duplicates: Duplicates,
) -> Values {
    let mut table = Table::with_capacity(count);
    for i in 0..count {
        // SAFETY: guaranteed by the caller.
        let (key, object) = unsafe { (*keys.add(i), *objects.add(i)) };
        // SAFETY: live objects from the caller, or null.
        let (Some(key), Some(object)) = (unsafe { key.as_ref() }, unsafe { object.as_ref() }) else {
            panic!("*** -[{receiver} initWithObjects:forKeys:count:]: attempt to insert nil object from objects[{i}]");
        };
        duplicates.add(&mut table, util::copy_key(key), object.retain());
    }
    table
}

/// The entries of any dictionary for a new dictionary to hold: shared with
/// it where copy-on-write allows (`Ok`), else retained afresh (`Err`).
fn copy_entries(obj: &AnyObject) -> Result<Arc<Values>, Values> {
    match kind(obj) {
        Kind::Fixed(d) => match d.ivars() {
            Frozen::Shared(table) => Ok(table.clone()),
            Frozen::Owned(table) => Err(table.clone()),
        },
        Kind::Mutable(m) => Ok(m.ivars().table.share()),
        Kind::Foreign => Err(gather(obj)),
    }
}

/// An immutable copy's table.
fn frozen_copy(obj: &AnyObject) -> Frozen<Retained<AnyObject>> {
    match copy_entries(obj) {
        Ok(shared) => Frozen::Shared(shared),
        Err(table) => Frozen::Owned(table),
    }
}

/// A mutable copy's table.
fn thawed_copy(obj: &AnyObject) -> Arc<Values> {
    copy_entries(obj).unwrap_or_else(shared)
}

/// The entries of any dictionary as a new table, the values copied with
/// `-copy` (`-initWithDictionary:copyItems:`).
fn copy_values(other: &AnyObject) -> Values {
    let table = table(other);
    let mut out = Table::with_capacity(table.len());
    for e in table.entries() {
        out.insert(e.key.clone(), util::copy_key(&e.value));
    }
    out
}

/// A table from parallel arrays of objects and keys.
fn from_arrays(receiver: &str, objects: &NSArray, keys: &NSArray, duplicates: Duplicates) -> Values {
    let (objects, keys) = (objects.to_vec(), keys.to_vec());
    if objects.len() != keys.len() {
        panic!(
            "*** -[{receiver} initWithObjects:forKeys:]: count of objects ({}) differs from count of keys ({})",
            objects.len(),
            keys.len()
        );
    }
    let mut table = Table::with_capacity(keys.len());
    for (key, object) in keys.iter().zip(objects) {
        duplicates.add(&mut table, util::copy_key(key), object);
    }
    table
}

/// A new immutable dictionary owning `table`.
pub(crate) fn make(table: Values) -> Retained<NSDictionary> {
    make_from(table.into())
}

fn make_from(table: Frozen<Retained<AnyObject>>) -> Retained<NSDictionary> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSDictionary::<AnyObject, AnyObject>::alloc();
    // SAFETY: NSDictionary's class is NSDictionaryImpl, and an `Allocated`
    // is a pointer to its object whatever its type parameters.
    let this = unsafe { std::mem::transmute::<Allocated<NSDictionary>, Allocated<NSDictionaryImpl>>(this) };
    // SAFETY: NSDictionaryImpl is the class registered as NSDictionary.
    unsafe { Retained::cast_unchecked(init_fixed(this, table)) }
}

/// A new mutable dictionary owning `table`.
pub(crate) fn make_mutable(table: Values) -> Retained<NSMutableDictionary> {
    make_mutable_from(table.into())
}

fn make_mutable_from(table: CowTable<Retained<AnyObject>>) -> Retained<NSMutableDictionary> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSMutableDictionary::<AnyObject, AnyObject>::alloc();
    // SAFETY: as in `make`, for NSMutableDictionaryImpl.
    let this =
        unsafe { std::mem::transmute::<Allocated<NSMutableDictionary>, Allocated<NSMutableDictionaryImpl>>(this) };
    // SAFETY: NSMutableDictionaryImpl is the class registered as
    // NSMutableDictionary.
    unsafe { Retained::cast_unchecked(init_mutable(this, table)) }
}

fn init_fixed(this: Allocated<NSDictionaryImpl>, table: Frozen<Retained<AnyObject>>) -> Retained<NSDictionaryImpl> {
    let this = this.set_ivars(table);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn init_mutable(
    this: Allocated<NSMutableDictionaryImpl>,
    table: CowTable<Retained<AnyObject>>,
) -> Retained<NSMutableDictionaryImpl> {
    let this = this.set_ivars(MutableDictionaryIvars { table, mutations: Mutations::default() });
    // SAFETY: NSDictionary's initializer, which leaves its own table empty.
    unsafe { msg_send![super(this), init] }
}

/// Append the description of any dictionary at `level`.
pub(crate) fn describe(out: &mut String, obj: &AnyObject, level: usize) {
    let table = table(obj);
    describe::map(out, table.entries().iter().map(|e| (&*e.key, &*e.value)), level);
}

/// Write up to `count` objects and keys, unretained, into either array.
fn fill(obj: &AnyObject, objects: *mut *mut AnyObject, keys: *mut *mut AnyObject, count: usize) {
    let table = table(obj);
    for (i, entry) in table.entries().iter().take(count).enumerate() {
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

type PairBlock = DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<AnyObject>, NonNull<Bool>)>;

/// Call `each` with the entries of any dictionary until it returns false.
/// An immutable dictionary's are read in place; any other's are the
/// entries as they were when this began, each retained for the call, so
/// `each` may change the dictionary, as Foundation allows.
fn for_each_entry(obj: &AnyObject, mut each: impl FnMut(&AnyObject, &AnyObject) -> bool) {
    if let Kind::Fixed(d) = kind(obj) {
        for e in d.ivars().entries() {
            if !each(&e.key, &e.value) {
                break;
            }
        }
        return;
    }
    let pairs: Vec<(Retained<AnyObject>, Retained<AnyObject>)> =
        table(obj).entries().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
    for (key, value) in &pairs {
        if !each(key, value) {
            break;
        }
    }
}

/// `-enumerateKeysAndObjectsUsingBlock:` for any dictionary.
fn enumerate(obj: &AnyObject, block: &PairBlock) {
    let mut stop = Bool::NO;
    for_each_entry(obj, |key, value| {
        block.call((NonNull::from(key), NonNull::from(value), NonNull::from(&mut stop)));
        !stop.as_bool()
    });
}

type PairTest = DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<AnyObject>, NonNull<Bool>) -> Bool>;

/// The keys of the entries `test` passes, until it sets its stop flag.
fn keys_passing(obj: &AnyObject, test: &PairTest) -> Retained<NSSet> {
    let mut stop = Bool::NO;
    let mut keys = Vec::new();
    for_each_entry(obj, |key, value| {
        if test.call((NonNull::from(key), NonNull::from(value), NonNull::from(&mut stop))).as_bool() {
            keys.push(key.retain());
        }
        !stop.as_bool()
    });
    set::make_of(keys)
}

/// `-isEqualToDictionary:` for any two dictionaries: the same keys, with
/// equal values. Values are compared where they are, without retaining
/// them, unless the other dictionary is reached through messages.
fn dictionaries_equal(a: &AnyObject, b: &AnyObject) -> bool {
    if ptr::eq(a, b) {
        return true;
    }
    let table = table(a);
    if table.len() != count_of(b) {
        return false;
    }
    let in_table = |theirs: &Values| {
        table.entries().iter().all(|e| theirs.get(&e.key).is_some_and(|other| equal(&e.value, &other.value)))
    };
    match kind(b) {
        Kind::Fixed(d) => in_table(d.ivars()),
        Kind::Mutable(m) => in_table(&m.ivars().table.read()),
        Kind::Foreign => table.entries().iter().all(|e| lookup(b, &e.key).is_some_and(|v| equal(&e.value, &v))),
    }
}

/// The keys, ordered by their values as `order` compares them.
fn keys_sorted(
    obj: &AnyObject,
    mut order: impl FnMut(*mut AnyObject, *mut AnyObject) -> std::cmp::Ordering,
) -> Retained<NSArray> {
    let table = table(obj);
    let entries = table.entries();
    let mut positions: Vec<usize> = (0..entries.len()).collect();
    let value = |i: usize| Retained::as_ptr(&entries[i].value).cast_mut();
    array::sort_stable(&mut positions, &mut |a, b| order(value(a), value(b)));
    let keys = positions.into_iter().map(|i| entries[i].key.clone()).collect();
    drop(table);
    array::make(keys)
}

fn all_keys(obj: &AnyObject) -> Retained<NSArray> {
    let keys = table(obj).entries().iter().map(|e| e.key.clone()).collect();
    array::make(keys)
}

fn all_values(obj: &AnyObject) -> Retained<NSArray> {
    let values = table(obj).entries().iter().map(|e| e.value.clone()).collect();
    array::make(values)
}

fn description_at(obj: &AnyObject, level: usize) -> Retained<NSString> {
    let mut out = String::new();
    describe(&mut out, obj, level);
    NSString::from_str(&out)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSDictionary"]
    #[ivars = Frozen<Retained<AnyObject>>]
    pub(crate) struct NSDictionaryImpl;

    impl NSDictionaryImpl {
        #[unsafe(method_id(dictionary))]
        fn dictionary() -> Retained<NSDictionary> {
            make(Table::default())
        }

        #[unsafe(method_id(dictionaryWithObject:forKey:))]
        fn dictionary_with_object(object: &AnyObject, key: &ProtocolObject<dyn NSCopying>) -> Retained<NSDictionary> {
            let mut table = Table::with_capacity(1);
            table.insert(util::copy_key(key.as_ref()), object.retain());
            make(table)
        }

        #[unsafe(method_id(dictionaryWithObjects:forKeys:count:))]
        fn dictionary_with_objects(
            objects: *const *mut AnyObject,
            keys: *const *mut AnyObject,
            count: NSUInteger,
        ) -> Retained<NSDictionary> {
            // SAFETY: the caller passes `count` keys and objects.
            make(unsafe { from_c_arrays("NSDictionary", objects, keys, count, Duplicates::FirstWins) })
        }

        #[unsafe(method_id(dictionaryWithDictionary:))]
        fn dictionary_with_dictionary(other: &NSDictionary) -> Retained<NSDictionary> {
            make_from(frozen_copy(other))
        }

        #[unsafe(method_id(dictionaryWithObjects:forKeys:))]
        fn dictionary_with_arrays(objects: &NSArray, keys: &NSArray) -> Retained<NSDictionary> {
            make(from_arrays("NSDictionary", objects, keys, Duplicates::FirstWins))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_fixed(this, Frozen::default())
        }

        #[unsafe(method_id(initWithObjects:forKeys:count:))]
        fn init_with_objects(
            this: Allocated<Self>,
            objects: *const *mut AnyObject,
            keys: *const *mut AnyObject,
            count: NSUInteger,
        ) -> Retained<Self> {
            // SAFETY: the caller passes `count` keys and objects.
            let table = unsafe { from_c_arrays("NSDictionary", objects, keys, count, Duplicates::FirstWins) };
            init_fixed(this, table.into())
        }

        #[unsafe(method_id(initWithDictionary:))]
        fn init_with_dictionary(this: Allocated<Self>, other: &NSDictionary) -> Retained<Self> {
            init_fixed(this, frozen_copy(other))
        }

        #[unsafe(method_id(initWithDictionary:copyItems:))]
        fn init_with_dictionary_copy(this: Allocated<Self>, other: &NSDictionary, copy: bool) -> Retained<Self> {
            init_fixed(this, if copy { copy_values(other).into() } else { frozen_copy(other) })
        }

        #[unsafe(method_id(initWithObjects:forKeys:))]
        fn init_with_arrays(this: Allocated<Self>, objects: &NSArray, keys: &NSArray) -> Retained<Self> {
            init_fixed(this, from_arrays("NSDictionary", objects, keys, Duplicates::FirstWins).into())
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().len()
        }

        /// Neither retained nor autoreleased: the dictionary keeps it alive.
        #[unsafe(method(objectForKey:))]
        fn object_for_key(&self, key: Option<&AnyObject>) -> *mut AnyObject {
            match key.and_then(|k| self.ivars().get(k)) {
                Some(entry) => Retained::as_ptr(&entry.value).cast_mut(),
                None => ptr::null_mut(),
            }
        }

        /// A subclass answers through its own `-objectForKey:`.
        #[unsafe(method(objectForKeyedSubscript:))]
        fn object_for_keyed_subscript(&self, key: Option<&AnyObject>) -> *mut AnyObject {
            if !is_exactly(self, &crate::NSDICTIONARY) {
                // SAFETY: -objectForKey: takes an object (or nil) and returns
                // one the dictionary keeps alive, or nil.
                return unsafe { msg_send![self, objectForKey: key] };
            }
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

        #[unsafe(method_id(allKeys))]
        fn all_keys(&self) -> Retained<NSArray> {
            all_keys(self)
        }

        #[unsafe(method_id(allValues))]
        fn all_values(&self) -> Retained<NSArray> {
            all_values(self)
        }

        #[unsafe(method_id(allKeysForObject:))]
        fn all_keys_for_object(&self, object: &AnyObject) -> Retained<NSArray> {
            let needle = util::Needle::new(object);
            let keys = table(self).entries().iter().filter(|e| needle.matches(&e.value)).map(|e| e.key.clone()).collect();
            array::make(keys)
        }

        #[unsafe(method_id(objectsForKeys:notFoundMarker:))]
        fn objects_for_keys(&self, keys: &NSArray, marker: &AnyObject) -> Retained<NSArray> {
            let objects =
                keys.to_vec().iter().map(|key| lookup(self, key).unwrap_or_else(|| marker.retain())).collect();
            array::make(objects)
        }

        #[unsafe(method_id(keysSortedByValueUsingComparator:))]
        fn keys_sorted_by_value(&self, comparator: NSComparator) -> Retained<NSArray> {
            keys_sorted(self, array::block_order(comparator))
        }

        /// Sorts are always stable, which both options allow.
        #[unsafe(method_id(keysSortedByValueWithOptions:usingComparator:))]
        fn keys_sorted_by_value_with_options(&self, _options: NSSortOptions, comparator: NSComparator) -> Retained<NSArray> {
            keys_sorted(self, array::block_order(comparator))
        }

        #[unsafe(method_id(keysSortedByValueUsingSelector:))]
        fn keys_sorted_by_value_using_selector(&self, selector: Sel) -> Retained<NSArray> {
            keys_sorted(self, array::selector_order(selector))
        }

        /// A subclass that doesn't override this has its keys in the
        /// storage it inherits.
        #[unsafe(method_id(keyEnumerator))]
        fn key_enumerator(&self) -> Retained<NSEnumerator> {
            enumerator::make(Source::Keys(util::upcast(self.retain())))
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            match kind(self) {
                Kind::Foreign => {
                    let values = all_values(self);
                    let count = values.count();
                    enumerator::make(Source::Array(util::upcast(values), count))
                }
                _ => enumerator::make(Source::Values(util::upcast(self.retain()))),
            }
        }

        #[unsafe(method(enumerateKeysAndObjectsUsingBlock:))]
        fn enumerate_keys_and_objects(&self, block: &PairBlock) {
            enumerate(self, block);
        }

        /// Concurrent enumeration runs on the calling thread, which the
        /// option permits; dictionaries have no order to reverse.
        #[unsafe(method(enumerateKeysAndObjectsWithOptions:usingBlock:))]
        fn enumerate_keys_and_objects_with_options(&self, _options: NSEnumerationOptions, block: &PairBlock) {
            enumerate(self, block);
        }

        #[unsafe(method_id(keysOfEntriesPassingTest:))]
        fn keys_of_entries_passing_test(&self, test: &PairTest) -> Retained<NSSet> {
            keys_passing(self, test)
        }

        /// Concurrent testing runs on the calling thread, which the option
        /// permits.
        #[unsafe(method_id(keysOfEntriesWithOptions:passingTest:))]
        fn keys_of_entries_with_options_passing_test(
            &self,
            _options: NSEnumerationOptions,
            test: &PairTest,
        ) -> Retained<NSSet> {
            keys_passing(self, test)
        }

        #[unsafe(method(isEqualToDictionary:))]
        fn is_equal_to_dictionary(&self, other: &NSDictionary) -> bool {
            dictionaries_equal(self, other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| {
                util::is_kind(other, NSDictionary::<AnyObject, AnyObject>::class())
                    && dictionaries_equal(self, other)
            })
        }

        /// The count, as in Foundation.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            count_of(self)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSDictionary> {
            if is_exactly(self, &crate::NSDICTIONARY) {
                // Immutable: a copy is the same object.
                // SAFETY: NSDictionaryImpl is the class registered as
                // NSDictionary.
                unsafe { Retained::cast_unchecked(self.retain()) }
            } else {
                make_from(frozen_copy(self))
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableDictionary> {
            make_mutable_from(thawed_copy(self).into())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            description_at(self, 0)
        }

        #[unsafe(method_id(descriptionWithLocale:))]
        fn description_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            description_at(self, 0)
        }

        #[unsafe(method_id(descriptionWithLocale:indent:))]
        fn description_with_locale_indent(&self, _locale: Option<&AnyObject>, level: NSUInteger) -> Retained<NSString> {
            description_at(self, level)
        }

        /// Enumerates the keys.
        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            let key = |entries: &[Entry<Retained<AnyObject>>], i: usize| {
                entries.get(i).map(|e| Retained::as_ptr(&e.key).cast_mut())
            };
            match kind(self) {
                Kind::Fixed(d) => {
                    let entries = d.ivars().entries();
                    // SAFETY: the caller passes a valid state and buffer.
                    unsafe { enumerator::batch(state, buffer, len, immutable_mutations(), |i| key(entries, i)) }
                }
                Kind::Mutable(m) => {
                    // SAFETY: copying pointers into the buffer runs no other
                    // code.
                    let entries = unsafe { m.ivars().table.peek() }.entries();
                    let mutations = m.ivars().mutations.as_ptr();
                    // SAFETY: the caller passes a valid state and buffer.
                    unsafe { enumerator::batch(state, buffer, len, mutations, |i| key(entries, i)) }
                }
                // SAFETY: the caller passes a valid state and buffer.
                Kind::Foreign => unsafe { enumerator::gathered(state, buffer, len, || all_keys(self)) },
            }
        }
    }

    unsafe impl NSObjectProtocol for NSDictionaryImpl {}
);

/// A mutable dictionary subclass defined outside Sidestep, changed through
/// the primitives Foundation asks such a subclass to implement.
struct Subclass<'a>(&'a AnyObject);

// SAFETY (every method): NSMutableDictionary's primitives, with the argument
// types Foundation declares.
impl Subclass<'_> {
    fn set(&self, object: &AnyObject, key: &AnyObject) {
        unsafe { msg_send![self.0, setObject: object, forKey: key] }
    }

    fn remove(&self, key: &AnyObject) {
        unsafe { msg_send![self.0, removeObjectForKey: key] }
    }

    fn remove_all(&self) {
        for key in gather(self.0).entries() {
            self.remove(&key.key);
        }
    }
}

impl NSMutableDictionaryImpl {
    /// This object, for failure messages.
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    /// A subclass defined outside Sidestep, which is changed through its
    /// primitives.
    fn subclass(&self) -> Option<Subclass<'_>> {
        (!is_exactly(self, &crate::NSMUTABLEDICTIONARY)).then(|| Subclass(self))
    }

    /// The value for `key`, unretained.
    fn find(&self, key: &AnyObject) -> Option<*mut AnyObject> {
        let table = &self.ivars().table;
        let at = table.position(key)?;
        // SAFETY: `position` just found the entry, and nothing ran since.
        Some(Retained::as_ptr(&unsafe { table.peek() }.entries()[at].value).cast_mut())
    }

    /// Put `new` in place of every entry.
    fn set_all(&self, new: Arc<Values>) {
        if let Some(subclass) = self.subclass() {
            subclass.remove_all();
            for e in new.entries() {
                subclass.set(&e.value, &e.key);
            }
            return;
        }
        let old = self.ivars().table.replace(new, MUTABLE, self.obj());
        self.changed();
        drop(old);
    }

    fn changed(&self) {
        self.ivars().mutations.bump();
    }

    /// Set `value` for `key` (already copied). An equal key already present
    /// stays, and only its value changes.
    fn set(&self, key: Retained<AnyObject>, value: Retained<AnyObject>) {
        let probe = Probe::new(&key);
        let hash = probe.hash;
        // SAFETY: only the table changes; what it replaces is released
        // after.
        let located = unsafe { self.ivars().table.write_located(MUTABLE, self.obj(), &probe, |_| true) };
        let Some((table, found)) = located else { return };
        let spare = match found {
            Ok(at) => (Some(std::mem::replace(table.value_mut(at), value)), Some(key)),
            Err(slot) => {
                table.push(hash, key, value, slot);
                (None, None)
            }
        };
        self.changed();
        drop(spare);
    }

    fn remove(&self, key: &AnyObject) {
        match self.subclass() {
            Some(subclass) => subclass.remove(key),
            None => self.remove_own(key),
        }
    }

    /// `-removeObjectForKey:` on this class's own storage.
    fn remove_own(&self, key: &AnyObject) {
        let table = &self.ivars().table;
        // SAFETY: only the table changes; what it removes is released after.
        if let Some((table, at)) = unsafe { table.write_identical(MUTABLE, self.obj(), key) } {
            let removed = table.remove(at);
            self.changed();
            drop(removed);
            return;
        }
        let probe = Probe::new(key);
        // SAFETY: only the table changes; the entry is released after.
        let located = unsafe { self.ivars().table.write_located(MUTABLE, self.obj(), &probe, Result::is_ok) };
        let Some((table, Ok(at))) = located else { return };
        let removed = table.remove(at);
        self.changed();
        drop(removed);
    }

    /// The entries of `other`, retained, before this dictionary changes:
    /// `other` may be this dictionary.
    fn taken(other: &NSDictionary) -> Vec<(Retained<AnyObject>, Retained<AnyObject>)> {
        table(other).entries().iter().map(|e| (e.key.clone(), e.value.clone())).collect()
    }
}

define_class!(
    #[unsafe(super(NSDictionary, NSObject))]
    #[name = "NSMutableDictionary"]
    #[ivars = MutableDictionaryIvars]
    pub(crate) struct NSMutableDictionaryImpl;

    impl NSMutableDictionaryImpl {
        #[unsafe(method_id(dictionary))]
        fn dictionary() -> Retained<NSMutableDictionary> {
            make_mutable(Table::default())
        }

        #[unsafe(method_id(dictionaryWithCapacity:))]
        fn dictionary_with_capacity(capacity: NSUInteger) -> Retained<NSMutableDictionary> {
            make_mutable(Table::with_capacity(capacity))
        }

        #[unsafe(method_id(dictionaryWithObject:forKey:))]
        fn dictionary_with_object(
            object: &AnyObject,
            key: &ProtocolObject<dyn NSCopying>,
        ) -> Retained<NSMutableDictionary> {
            let mut table = Table::with_capacity(1);
            table.insert(util::copy_key(key.as_ref()), object.retain());
            make_mutable(table)
        }

        #[unsafe(method_id(dictionaryWithObjects:forKeys:count:))]
        fn dictionary_with_objects(
            objects: *const *mut AnyObject,
            keys: *const *mut AnyObject,
            count: NSUInteger,
        ) -> Retained<NSMutableDictionary> {
            // SAFETY: the caller passes `count` keys and objects.
            make_mutable(unsafe { from_c_arrays(MUTABLE, objects, keys, count, Duplicates::LastValueWins) })
        }

        #[unsafe(method_id(dictionaryWithDictionary:))]
        fn dictionary_with_dictionary(other: &NSDictionary) -> Retained<NSMutableDictionary> {
            make_mutable_from(thawed_copy(other).into())
        }

        #[unsafe(method_id(dictionaryWithObjects:forKeys:))]
        fn dictionary_with_arrays(objects: &NSArray, keys: &NSArray) -> Retained<NSMutableDictionary> {
            make_mutable(from_arrays(MUTABLE, objects, keys, Duplicates::LastValueWins))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_mutable(this, CowTable::default())
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, capacity: NSUInteger) -> Retained<Self> {
            init_mutable(this, Table::with_capacity(capacity).into())
        }

        #[unsafe(method_id(initWithObjects:forKeys:count:))]
        fn init_with_objects(
            this: Allocated<Self>,
            objects: *const *mut AnyObject,
            keys: *const *mut AnyObject,
            count: NSUInteger,
        ) -> Retained<Self> {
            // SAFETY: the caller passes `count` keys and objects.
            let table = unsafe { from_c_arrays(MUTABLE, objects, keys, count, Duplicates::LastValueWins) };
            init_mutable(this, table.into())
        }

        #[unsafe(method_id(initWithDictionary:))]
        fn init_with_dictionary(this: Allocated<Self>, other: &NSDictionary) -> Retained<Self> {
            init_mutable(this, thawed_copy(other).into())
        }

        #[unsafe(method_id(initWithDictionary:copyItems:))]
        fn init_with_dictionary_copy(this: Allocated<Self>, other: &NSDictionary, copy: bool) -> Retained<Self> {
            init_mutable(this, if copy { copy_values(other).into() } else { thawed_copy(other).into() })
        }

        #[unsafe(method_id(initWithObjects:forKeys:))]
        fn init_with_arrays(this: Allocated<Self>, objects: &NSArray, keys: &NSArray) -> Retained<Self> {
            init_mutable(this, from_arrays(MUTABLE, objects, keys, Duplicates::LastValueWins).into())
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            // SAFETY: reading the length runs no other code.
            unsafe { self.ivars().table.peek() }.len()
        }

        /// Neither retained nor autoreleased: the dictionary keeps it alive.
        #[unsafe(method(objectForKey:))]
        fn object_for_key(&self, key: Option<&AnyObject>) -> *mut AnyObject {
            key.and_then(|key| self.find(key)).unwrap_or(ptr::null_mut())
        }

        /// A subclass answers through its own `-objectForKey:`.
        #[unsafe(method(objectForKeyedSubscript:))]
        fn object_for_keyed_subscript(&self, key: Option<&AnyObject>) -> *mut AnyObject {
            if self.subclass().is_some() {
                // SAFETY: -objectForKey: takes an object (or nil) and returns
                // one the dictionary keeps alive, or nil.
                return unsafe { msg_send![self, objectForKey: key] };
            }
            key.and_then(|key| self.find(key)).unwrap_or(ptr::null_mut())
        }

        #[unsafe(method(setObject:forKey:))]
        fn set_object_for_key(&self, object: Option<&AnyObject>, key: Option<&AnyObject>) {
            let Some(key) = key else { util::nil_argument(MUTABLE, "setObject:forKey:", "key") };
            let Some(object) = object else {
                panic!("*** -[NSMutableDictionary setObject:forKey:]: object cannot be nil (key: {})", util::description(key))
            };
            self.set(util::copy_key(key), object.retain());
        }

        /// A nil object removes the key.
        #[unsafe(method(setObject:forKeyedSubscript:))]
        fn set_object_for_keyed_subscript(&self, object: Option<&AnyObject>, key: Option<&AnyObject>) {
            let Some(key) = key else { util::nil_argument(MUTABLE, "setObject:forKeyedSubscript:", "key") };
            match (object, self.subclass()) {
                (Some(object), Some(subclass)) => subclass.set(object, key),
                (Some(object), None) => self.set(util::copy_key(key), object.retain()),
                (None, _) => self.remove(key),
            }
        }

        #[unsafe(method(removeObjectForKey:))]
        fn remove_object_for_key(&self, key: Option<&AnyObject>) {
            let Some(key) = key else { util::nil_argument(MUTABLE, "removeObjectForKey:", "key") };
            // The primitive: this class's own storage, whatever the class.
            self.remove_own(key);
        }

        #[unsafe(method(removeObjectsForKeys:))]
        fn remove_objects_for_keys(&self, keys: &NSArray) {
            for key in keys.to_vec() {
                self.remove(&key);
            }
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.set_all(shared(Table::default()));
        }

        #[unsafe(method(addEntriesFromDictionary:))]
        fn add_entries_from_dictionary(&self, other: &NSDictionary) {
            let pairs = Self::taken(other);
            let subclass = self.subclass();
            for (key, value) in pairs {
                match &subclass {
                    Some(subclass) => subclass.set(&value, &key),
                    None => self.set(util::copy_key(&key), value),
                }
            }
        }

        #[unsafe(method(setDictionary:))]
        fn set_dictionary(&self, other: &NSDictionary) {
            // Taken first: `other` may be this dictionary.
            self.set_all(thawed_copy(other));
        }
    }

    unsafe impl NSObjectProtocol for NSMutableDictionaryImpl {}
);
