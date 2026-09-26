//! `NSCountedSet`: a mutable set that counts how many times each member was
//! added, and keeps it until it has been removed as many times.
//!
//! It is a subclass of `NSMutableSet`, as in Foundation, with storage of its
//! own: the table of `table.rs` with each member's count as its value, in a
//! copy-on-write cell as a mutable set's table is. It implements the
//! primitives (`-count`, `-member:`, `-objectEnumerator`, `-addObject:`,
//! `-removeObject:`) and inherits the rest of `NSSet` and `NSMutableSet`,
//! which reach a subclass through them.
//!
//! Set algebra counts, as Foundation's does, taking a plain set as a
//! counted set whose counts are all one: `-unionSet:` adds the other set's
//! counts, `-minusSet:` takes them away, `-intersectSet:` keeps the smaller
//! of the two counts, `-setSet:` takes the other's counts,
//! `-isSubsetOfSet:` asks that no count be more than the other's, and
//! equality compares counts (on either side: a plain set equals a counted
//! set only if each count is one). Copies are counted sets with the counts,
//! sharing the table until either set changes.

use std::ptr::{self, NonNull};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSCountedSet, NSEnumerator, NSFastEnumerationState, NSMutableSet, NSSet, NSUInteger, NSZone,
};

use crate::array;
use crate::enumerator::{self, Mutations, Source};
use crate::table::{CowTable, Probe, Table, shared};
use crate::util::{self, inherits};

sidestep_runtime::static_class!(pub(crate) NSCOUNTEDSET, NSCOUNTEDSET_META = "NSCountedSet", || {
    let _ = NSCountedSetImpl::class();
});

const NAME: &str = "NSCountedSet";

type Counts = Table<usize>;

#[derive(Default)]
pub(crate) struct CountedSetIvars {
    counts: CowTable<usize>,
    mutations: Mutations,
}

/// A table counting `members`.
fn counting(members: impl IntoIterator<Item = Retained<AnyObject>>) -> Counts {
    let mut counts = Counts::default();
    for member in members {
        let probe = Probe::new(&member);
        match counts.locate(&probe) {
            Ok(at) => *counts.value_mut(at) += 1,
            Err(slot) => counts.push(probe.hash, member, 1, slot),
        }
    }
    counts
}

/// The members of any set, and for a counted set, their counts.
fn counts_of(obj: &AnyObject) -> Counts {
    match counted(obj) {
        Some(c) => (**c.ivars().counts.read()).clone(),
        None => {
            let members: Retained<NSArray> = if inherits(obj, &crate::NSSET) {
                // SAFETY: an NSSet answers -allObjects with an array.
                unsafe { msg_send![obj, allObjects] }
            } else {
                array::make(Vec::new())
            };
            counting(members.to_vec())
        }
    }
}

/// Sidestep's storage of a counted set, a subclass's included.
fn counted(obj: &AnyObject) -> Option<&NSCountedSetImpl> {
    // SAFETY: an instance of NSCountedSetImpl or a subclass, whose ivars sit
    // where NSCountedSetImpl's methods expect them.
    inherits(obj, &NSCOUNTEDSET).then(|| unsafe { &*(obj as *const AnyObject).cast::<NSCountedSetImpl>() })
}

fn make(counts: Counts) -> Retained<NSCountedSet> {
    make_with(counts.into())
}

fn make_with(counts: CowTable<usize>) -> Retained<NSCountedSet> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSCountedSet::<AnyObject>::alloc();
    // SAFETY: NSCountedSet's class is NSCountedSetImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSCountedSet>, Allocated<NSCountedSetImpl>>(this) };
    // SAFETY: NSCountedSetImpl is the class registered as NSCountedSet.
    unsafe { Retained::cast_unchecked(init(this, counts)) }
}

fn init(this: Allocated<NSCountedSetImpl>, counts: CowTable<usize>) -> Retained<NSCountedSetImpl> {
    let this = this.set_ivars(CountedSetIvars { counts, mutations: Mutations::default() });
    // SAFETY: NSMutableSet's initializer, which leaves its own table empty.
    unsafe { msg_send![super(this), init] }
}

impl NSCountedSetImpl {
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    fn changed(&self) {
        self.ivars().mutations.bump();
    }

    fn count_for(&self, object: &AnyObject) -> usize {
        let counts = &self.ivars().counts;
        match counts.position(object) {
            // SAFETY: `position` just found it, and nothing ran since.
            Some(at) => unsafe { counts.peek() }.entries()[at].value,
            None => 0,
        }
    }

    /// Count `object` `times` more.
    fn add(&self, object: &AnyObject, times: usize) {
        // Retained before the table is taken: retaining may run code.
        let object = object.retain();
        let probe = Probe::new(&object);
        let hash = probe.hash;
        // SAFETY: only the table changes.
        let located = unsafe { self.ivars().counts.write_located(NAME, self.obj(), &probe, |_| true) };
        match located {
            Some((table, Ok(at))) => *table.value_mut(at) += times,
            Some((table, Err(slot))) => table.push(hash, object, times, slot),
            None => return,
        }
        self.changed();
    }

    /// Count `object` `times` fewer, removing it at none.
    fn remove(&self, object: &AnyObject, times: usize) {
        let probe = Probe::new(object);
        // SAFETY: only the table changes; a member removed is released
        // after.
        let located = unsafe { self.ivars().counts.write_located(NAME, self.obj(), &probe, Result::is_ok) };
        let Some((table, Ok(at))) = located else { return };
        let removed = if table.entries()[at].value > times {
            *table.value_mut(at) -= times;
            None
        } else {
            Some(table.remove(at))
        };
        self.changed();
        drop(removed);
    }

    /// Put `counts` in place of the members and their counts.
    fn set_all(&self, counts: Counts) {
        let old = self.ivars().counts.replace(shared(counts), NAME, self.obj());
        self.changed();
        drop(old);
    }

    fn members(&self) -> Vec<Retained<AnyObject>> {
        self.ivars().counts.read().entries().iter().map(|e| e.key.clone()).collect()
    }
}

/// `[a isEqual:b]` for a counted set `a`: the same members, with the same
/// counts; a plain set's counts are all one.
fn equal_counts(a: &NSCountedSetImpl, b: &AnyObject) -> bool {
    if ptr::eq(a.obj(), b) {
        return true;
    }
    let (ours, theirs) = (counts_of(a), counts_of(b));
    ours.len() == theirs.len()
        && ours.entries().iter().all(|e| theirs.get(&e.key).is_some_and(|other| other.value == e.value))
}

/// Whether `obj` counts each member once: any set but a counted set with a
/// member counted more than once. A plain set equals a counted set only
/// then.
pub(crate) fn counts_once(obj: &AnyObject) -> bool {
    match counted(obj) {
        Some(c) => c.ivars().counts.read().entries().iter().all(|e| e.value == 1),
        None => true,
    }
}

define_class!(
    #[unsafe(super(NSMutableSet, NSSet, objc2::runtime::NSObject))]
    #[name = "NSCountedSet"]
    #[ivars = CountedSetIvars]
    pub(crate) struct NSCountedSetImpl;

    impl NSCountedSetImpl {
        #[unsafe(method_id(set))]
        fn set() -> Retained<NSCountedSet> {
            make(Counts::default())
        }

        #[unsafe(method_id(setWithCapacity:))]
        fn set_with_capacity(_capacity: NSUInteger) -> Retained<NSCountedSet> {
            make(Counts::default())
        }

        #[unsafe(method_id(setWithObject:))]
        fn set_with_object(object: &AnyObject) -> Retained<NSCountedSet> {
            make(counting([object.retain()]))
        }

        #[unsafe(method_id(setWithObjects:count:))]
        fn set_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSCountedSet> {
            // SAFETY: the caller passes `count` objects.
            make(counting(unsafe { array::from_c_array(NAME, "initWithObjects:count:", objects, count) }))
        }

        #[unsafe(method_id(setWithArray:))]
        fn set_with_array(array: &NSArray) -> Retained<NSCountedSet> {
            make(counting(array.to_vec()))
        }

        #[unsafe(method_id(setWithSet:))]
        fn set_with_set(other: &NSSet) -> Retained<NSCountedSet> {
            make(counts_of(other))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init(this, CowTable::default())
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, _capacity: NSUInteger) -> Retained<Self> {
            init(this, CowTable::default())
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init(this, counting(array.to_vec()).into())
        }

        /// A counted set's counts come along; a plain set's members count
        /// once.
        #[unsafe(method_id(initWithSet:))]
        fn init_with_set(this: Allocated<Self>, other: &NSSet) -> Retained<Self> {
            init(this, counts_of(other).into())
        }

        #[unsafe(method_id(initWithSet:copyItems:))]
        fn init_with_set_copy(this: Allocated<Self>, other: &NSSet, copy: bool) -> Retained<Self> {
            let mut counts = counts_of(other);
            if copy {
                let mut copied = Counts::with_capacity(counts.len());
                for e in counts.entries() {
                    let key = util::copy_key(&e.key);
                    let probe = Probe::new(&key);
                    if let Err(slot) = copied.locate(&probe) {
                        copied.push(probe.hash, key, e.value, slot);
                    }
                }
                counts = copied;
            }
            init(this, counts.into())
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            init(this, counting(unsafe { array::from_c_array(NAME, "initWithObjects:count:", objects, count) }).into())
        }

        /// How many distinct members there are.
        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            // SAFETY: reading the length runs no other code.
            unsafe { self.ivars().counts.peek() }.len()
        }

        #[unsafe(method(countForObject:))]
        fn count_for_object(&self, object: Option<&AnyObject>) -> NSUInteger {
            object.map_or(0, |o| self.count_for(o))
        }

        /// Neither retained nor autoreleased: the set keeps it alive.
        #[unsafe(method(member:))]
        fn member(&self, object: Option<&AnyObject>) -> *mut AnyObject {
            let counts = &self.ivars().counts;
            match object.and_then(|o| counts.position(o)) {
                // SAFETY: `position` just found it, and nothing ran since.
                Some(at) => Retained::as_ptr(&unsafe { counts.peek() }.entries()[at].key).cast_mut(),
                None => ptr::null_mut(),
            }
        }

        #[unsafe(method(containsObject:))]
        fn contains_object(&self, object: Option<&AnyObject>) -> bool {
            object.is_some_and(|o| self.ivars().counts.position(o).is_some())
        }

        #[unsafe(method(addObject:))]
        fn add_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { panic!("*** -[{NAME} addObject:]: attempt to insert nil") };
            self.add(object, 1);
        }

        #[unsafe(method(removeObject:))]
        fn remove_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { panic!("*** -[{NAME} removeObject:]: attempt to remove nil") };
            self.remove(object, 1);
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.set_all(Counts::default());
        }

        /// The other set's members and counts.
        #[unsafe(method(setSet:))]
        fn set_set(&self, other: &NSSet) {
            self.set_all(counts_of(other));
        }

        /// The other set's counts added. Taken first: `other` may be this
        /// set.
        #[unsafe(method(unionSet:))]
        fn union_set(&self, other: &NSSet) {
            for e in counts_of(other).entries() {
                self.add(&e.key, e.value);
            }
        }

        /// The other set's counts taken away.
        #[unsafe(method(minusSet:))]
        fn minus_set(&self, other: &NSSet) {
            for e in counts_of(other).entries() {
                self.remove(&e.key, e.value);
            }
        }

        /// The members both sets have, each at the smaller of its counts.
        #[unsafe(method(intersectSet:))]
        fn intersect_set(&self, other: &NSSet) {
            let (ours, theirs) = (counts_of(self), counts_of(other));
            let mut kept = Counts::with_capacity(ours.len());
            for e in ours.entries() {
                if let Some(other) = theirs.get(&e.key) {
                    let probe = Probe::new(&e.key);
                    if let Err(slot) = kept.locate(&probe) {
                        kept.push(probe.hash, e.key.clone(), e.value.min(other.value), slot);
                    }
                }
            }
            self.set_all(kept);
        }

        /// Whether the other set counts each member at least as often.
        #[unsafe(method(isSubsetOfSet:))]
        fn is_subset_of_set(&self, other: &NSSet) -> bool {
            let (ours, theirs) = (counts_of(self), counts_of(other));
            ours.entries().iter().all(|e| theirs.get(&e.key).is_some_and(|other| other.value >= e.value))
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            let snapshot = array::make(self.members());
            enumerator::make(Source::snapshot(snapshot, util::upcast(self.retain()), self.ivars().mutations.as_ptr()))
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| util::is_kind(other, NSSet::<AnyObject>::class()) && equal_counts(self, other))
        }

        #[unsafe(method(isEqualToSet:))]
        fn is_equal_to_set(&self, other: &NSSet) -> bool {
            equal_counts(self, other)
        }

        /// Shares the table until either set next changes.
        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSCountedSet> {
            make_with(self.ivars().counts.share().into())
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSCountedSet> {
            make_with(self.ivars().counts.share().into())
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            let ivars = self.ivars();
            // SAFETY: copying pointers into the buffer runs no other code.
            let entries = unsafe { ivars.counts.peek() }.entries();
            // SAFETY: the caller passes a valid state and buffer.
            unsafe {
                enumerator::batch(state, buffer, len, ivars.mutations.as_ptr(), |i| {
                    entries.get(i).map(|e| Retained::as_ptr(&e.key).cast_mut())
                })
            }
        }
    }

    unsafe impl NSObjectProtocol for NSCountedSetImpl {}
);
