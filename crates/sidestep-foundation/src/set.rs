//! `NSSet` and `NSMutableSet`: unordered collections of distinct objects,
//! kept in the hash table of `table` with no values.
//!
//! They follow the dictionaries' design: an immutable set's table never
//! changes, a mutable one keeps its own in a `CowTable` with a count of
//! changes, methods that read a set are written once over a table held for
//! reading (or one gathered from a foreign subclass's `-objectEnumerator`),
//! a foreign subclass is changed through its `-addObject:` and
//! `-removeObject:`, and copies of a mutable set share its table until it
//! next changes. Unlike dictionary keys, members are retained, not copied,
//! and adding an object equal to a member keeps the member. Membership
//! tests never retain the member they find.

use std::ffi::c_ulong;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSEnumerationOptions, NSEnumerator, NSFastEnumerationState, NSMutableSet, NSSet, NSString, NSUInteger,
    NSZone,
};

use crate::enumerator::{self, Mutations, Source, immutable_mutations};
use crate::guarded::Reading;
use crate::table::{CowTable, Entry, Frozen, Probe, Table, shared};
use crate::util::{self, inherits, is_exactly, nil_argument};
use crate::{array, describe, sort_descriptor};

type Members = Table<()>;

const MUTABLE: &str = "NSMutableSet";

#[derive(Default)]
pub(crate) struct MutableSetIvars {
    table: CowTable<()>,
    mutations: Mutations,
}

/// Which storage a set object has.
enum Kind<'a> {
    Fixed(&'a NSSetImpl),
    Mutable(&'a NSMutableSetImpl),
    /// A subclass from outside Sidestep, reached through messages.
    Foreign,
}

fn kind(obj: &AnyObject) -> Kind<'_> {
    if is_exactly(obj, &crate::NSSET) {
        // SAFETY: an instance of exactly NSSetImpl.
        Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSSetImpl>() })
    } else if is_exactly(obj, &crate::NSMUTABLESET) {
        // SAFETY: an instance of exactly NSMutableSetImpl.
        Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableSetImpl>() })
    } else {
        Kind::Foreign
    }
}

/// The storage `obj` has from Sidestep's classes, a subclass's included:
/// what the inherited `-objectEnumerator` walks. `Foreign` for objects that
/// are not sets.
fn storage(obj: &AnyObject) -> Kind<'_> {
    match kind(obj) {
        Kind::Foreign if inherits(obj, &crate::NSMUTABLESET) => {
            // SAFETY: an instance of a subclass of NSMutableSetImpl, whose
            // ivars sit where the superclass's methods expect them, set by
            // the initializer it inherits.
            Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableSetImpl>() })
        }
        Kind::Foreign if inherits(obj, &crate::NSSET) => {
            // SAFETY: as above, for NSSetImpl.
            Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSSetImpl>() })
        }
        kind => kind,
    }
}

/// A set's table, held for reading.
enum Tables<'a> {
    Fixed(&'a Members),
    Mutable(Reading<'a, Arc<Members>>),
    Gathered(Members),
}

impl Deref for Tables<'_> {
    type Target = Members;

    fn deref(&self) -> &Members {
        match self {
            Tables::Fixed(table) => table,
            Tables::Mutable(table) => table,
            Tables::Gathered(table) => table,
        }
    }
}

fn table(obj: &AnyObject) -> Tables<'_> {
    match kind(obj) {
        Kind::Fixed(s) => Tables::Fixed(s.ivars()),
        Kind::Mutable(m) => Tables::Mutable(m.ivars().table.read()),
        Kind::Foreign => Tables::Gathered(gather(obj)),
    }
}

/// The members of a set subclass defined outside Sidestep, through its
/// primitive methods.
fn gather(obj: &AnyObject) -> Members {
    // SAFETY: NSSet's primitives: -objectEnumerator returns an enumerator of
    // the members.
    let members: Retained<NSEnumerator> = unsafe { msg_send![obj, objectEnumerator] };
    Table::of_members(std::iter::from_fn(|| members.nextObject()))
}

/// The member at position `index` of a set's own members, unretained (the
/// set keeps it alive), or `None` past the end.
pub(crate) fn member_at(obj: &AnyObject, index: usize) -> Option<*mut AnyObject> {
    let member = |table: &Members| table.entries().get(index).map(|e| Retained::as_ptr(&e.key).cast_mut());
    match storage(obj) {
        Kind::Fixed(s) => member(s.ivars()),
        // SAFETY: reading one pointer runs no other code.
        Kind::Mutable(m) => member(unsafe { m.ivars().table.peek() }),
        Kind::Foreign => None,
    }
}

/// A set's count of changes, and of its own members; `None` for objects
/// that aren't sets.
pub(crate) fn state(obj: &AnyObject) -> Option<(*mut c_ulong, usize)> {
    match storage(obj) {
        Kind::Fixed(s) => Some((immutable_mutations(), s.ivars().len())),
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

/// Whether any set has a member equal to `object`. Retains nothing.
pub(crate) fn contains(obj: &AnyObject, object: &AnyObject) -> bool {
    match kind(obj) {
        Kind::Fixed(s) => s.ivars().position(object).is_some(),
        Kind::Mutable(m) => m.ivars().table.position(object).is_some(),
        Kind::Foreign => {
            // SAFETY: -member: takes an object and returns one or nil.
            let found: *mut AnyObject = unsafe { msg_send![obj, member: object] };
            !found.is_null()
        }
    }
}

fn count_of(obj: &AnyObject) -> usize {
    match kind(obj) {
        Kind::Fixed(s) => s.ivars().len(),
        // SAFETY: reading the length runs no other code.
        Kind::Mutable(m) => unsafe { m.ivars().table.peek() }.len(),
        // SAFETY: -count takes nothing and returns NSUInteger.
        Kind::Foreign => unsafe { msg_send![obj, count] },
    }
}

fn members(obj: &AnyObject) -> Vec<Retained<AnyObject>> {
    table(obj).entries().iter().map(|e| e.key.clone()).collect()
}

/// `count` objects from a C array, retained, without duplicates.
///
/// # Safety
/// `objects` must point to `count` object pointers (or be anything when
/// `count` is 0).
unsafe fn from_c_array(receiver: &str, objects: *const *mut AnyObject, count: usize) -> Members {
    let mut table = Table::with_capacity(count);
    for i in 0..count {
        // SAFETY: guaranteed by the caller.
        let obj = unsafe { *objects.add(i) };
        // SAFETY: a live object from the caller, or null.
        let Some(obj) = (unsafe { obj.as_ref() }) else {
            panic!("*** -[{receiver} initWithObjects:count:]: attempt to insert nil object from objects[{i}]");
        };
        table.insert_first(obj.retain(), ());
    }
    table
}

fn from_array(array: &NSArray) -> Members {
    Table::of_members(array.to_vec())
}

/// The members of any set for a new set to hold: shared with it where
/// copy-on-write allows (`Ok`), else retained afresh (`Err`).
fn copy_members(obj: &AnyObject) -> Result<Arc<Members>, Members> {
    match kind(obj) {
        Kind::Fixed(s) => match s.ivars() {
            Frozen::Shared(table) => Ok(table.clone()),
            Frozen::Owned(table) => Err(table.clone()),
        },
        Kind::Mutable(m) => Ok(m.ivars().table.share()),
        Kind::Foreign => Err(gather(obj)),
    }
}

/// An immutable copy's table.
fn frozen_copy(obj: &AnyObject) -> Frozen<()> {
    match copy_members(obj) {
        Ok(shared) => Frozen::Shared(shared),
        Err(table) => Frozen::Owned(table),
    }
}

/// A mutable copy's table.
fn thawed_copy(obj: &AnyObject) -> Arc<Members> {
    copy_members(obj).unwrap_or_else(shared)
}

/// The members of any set, each copied with `-copy`
/// (`-initWithSet:copyItems:`).
fn copy_each(other: &AnyObject) -> Members {
    let table = table(other);
    Table::of_members(table.entries().iter().map(|e| util::copy_key(&e.key)))
}

/// A new immutable set of `members`.
pub(crate) fn make_of(members: Vec<Retained<AnyObject>>) -> Retained<NSSet> {
    make(Table::of_members(members))
}

/// A new immutable set owning `table`.
pub(crate) fn make(table: Members) -> Retained<NSSet> {
    make_from(table.into())
}

fn make_from(table: Frozen<()>) -> Retained<NSSet> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSSet::<AnyObject>::alloc();
    // SAFETY: NSSet's class is NSSetImpl, and an `Allocated` is a pointer to
    // its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSSet>, Allocated<NSSetImpl>>(this) };
    // SAFETY: NSSetImpl is the class registered as NSSet.
    unsafe { Retained::cast_unchecked(init_fixed(this, table)) }
}

/// A new mutable set owning `table`.
pub(crate) fn make_mutable(table: Members) -> Retained<NSMutableSet> {
    make_mutable_from(table.into())
}

fn make_mutable_from(table: CowTable<()>) -> Retained<NSMutableSet> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSMutableSet::<AnyObject>::alloc();
    // SAFETY: as in `make`, for NSMutableSetImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSMutableSet>, Allocated<NSMutableSetImpl>>(this) };
    // SAFETY: NSMutableSetImpl is the class registered as NSMutableSet.
    unsafe { Retained::cast_unchecked(init_mutable(this, table)) }
}

fn init_fixed(this: Allocated<NSSetImpl>, table: Frozen<()>) -> Retained<NSSetImpl> {
    let this = this.set_ivars(table);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn init_mutable(this: Allocated<NSMutableSetImpl>, table: CowTable<()>) -> Retained<NSMutableSetImpl> {
    let this = this.set_ivars(MutableSetIvars { table, mutations: Mutations::default() });
    // SAFETY: NSSet's initializer, which leaves its own table empty.
    unsafe { msg_send![super(this), init] }
}

fn description_of(obj: &AnyObject) -> Retained<NSString> {
    let table = table(obj);
    let mut out = String::new();
    describe::list(&mut out, "{(", ")}", table.entries().iter().map(|e| &*e.key), 0);
    drop(table);
    NSString::from_str(&out)
}

/// Whether every member of `a` is in `b`.
fn is_subset(a: &AnyObject, b: &AnyObject) -> bool {
    let table = table(a);
    table.len() <= count_of(b) && table.entries().iter().all(|e| contains(b, &e.key))
}

/// `-isEqualToSet:` for any two sets: the same members.
fn sets_equal(a: &AnyObject, b: &AnyObject) -> bool {
    ptr::eq(a, b) || (count_of(a) == count_of(b) && is_subset(a, b))
}

/// Call `each` with the members of any set until it returns false. An
/// immutable set's are read in place; any other's are the members as they
/// were when this began, each retained for the call, so `each` may change
/// the set, as Foundation allows.
fn for_each_member(obj: &AnyObject, mut each: impl FnMut(&AnyObject) -> bool) {
    if let Kind::Fixed(s) = kind(obj) {
        for e in s.ivars().entries() {
            if !each(&e.key) {
                break;
            }
        }
        return;
    }
    for member in &members(obj) {
        if !each(member) {
            break;
        }
    }
}

type MemberTest = DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<Bool>) -> Bool>;

/// The members `test` passes, until it sets its stop flag.
fn passing(obj: &AnyObject, test: &MemberTest) -> Members {
    let mut passed = Vec::new();
    let mut stop = Bool::NO;
    for_each_member(obj, |member| {
        if test.call((NonNull::from(member), NonNull::from(&mut stop))).as_bool() {
            passed.push(member.retain());
        }
        !stop.as_bool()
    });
    Table::of_members(passed)
}

type MemberBlock = DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<Bool>)>;

/// `-enumerateObjectsUsingBlock:` for any set.
fn enumerate(obj: &AnyObject, block: &MemberBlock) {
    let mut stop = Bool::NO;
    for_each_member(obj, |member| {
        block.call((NonNull::from(member), NonNull::from(&mut stop)));
        !stop.as_bool()
    });
}

fn perform_each(obj: &AnyObject, selector: Sel, argument: Option<Option<&AnyObject>>) {
    let mut checked = None;
    for member in members(obj) {
        array::perform(&member, selector, argument, &mut checked);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSSet"]
    #[ivars = Frozen<()>]
    pub(crate) struct NSSetImpl;

    impl NSSetImpl {
        #[unsafe(method_id(set))]
        fn set() -> Retained<NSSet> {
            make(Table::default())
        }

        #[unsafe(method_id(setWithObject:))]
        fn set_with_object(object: &AnyObject) -> Retained<NSSet> {
            make(Table::of_members([object.retain()]))
        }

        #[unsafe(method_id(setWithObjects:count:))]
        fn set_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSSet> {
            // SAFETY: the caller passes `count` objects.
            make(unsafe { from_c_array("NSSet", objects, count) })
        }

        #[unsafe(method_id(setWithArray:))]
        fn set_with_array(array: &NSArray) -> Retained<NSSet> {
            make(from_array(array))
        }

        #[unsafe(method_id(setWithSet:))]
        fn set_with_set(set: &NSSet) -> Retained<NSSet> {
            make_from(frozen_copy(set))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_fixed(this, Frozen::default())
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            init_fixed(this, unsafe { from_c_array("NSSet", objects, count) }.into())
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init_fixed(this, from_array(array).into())
        }

        #[unsafe(method_id(initWithSet:))]
        fn init_with_set(this: Allocated<Self>, set: &NSSet) -> Retained<Self> {
            init_fixed(this, frozen_copy(set))
        }

        #[unsafe(method_id(initWithSet:copyItems:))]
        fn init_with_set_copy(this: Allocated<Self>, set: &NSSet, copy: bool) -> Retained<Self> {
            init_fixed(this, if copy { copy_each(set).into() } else { frozen_copy(set) })
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().len()
        }

        /// Neither retained nor autoreleased: the set keeps it alive.
        #[unsafe(method(member:))]
        fn member(&self, object: Option<&AnyObject>) -> *mut AnyObject {
            match object.and_then(|o| self.ivars().get(o)) {
                Some(entry) => Retained::as_ptr(&entry.key).cast_mut(),
                None => ptr::null_mut(),
            }
        }

        #[unsafe(method(containsObject:))]
        fn contains_object(&self, object: Option<&AnyObject>) -> bool {
            object.is_some_and(|o| contains(self, o))
        }

        /// A subclass answers with the first member its own enumerator
        /// gives.
        #[unsafe(method(anyObject))]
        fn any_object(&self) -> *mut AnyObject {
            if let Kind::Foreign = kind(self) {
                // SAFETY: -objectEnumerator returns an enumerator of the
                // members, which the set keeps alive.
                let members: Retained<NSEnumerator> = unsafe { msg_send![self, objectEnumerator] };
                return members.nextObject().map_or(ptr::null_mut(), |m| Retained::as_ptr(&m).cast_mut());
            }
            member_at(self, 0).unwrap_or(ptr::null_mut())
        }

        #[unsafe(method_id(allObjects))]
        fn all_objects(&self) -> Retained<NSArray> {
            array::make(members(self))
        }

        /// A subclass that doesn't override this has its members in the
        /// storage it inherits.
        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            enumerator::make(Source::Members(util::upcast(self.retain())))
        }

        #[unsafe(method(enumerateObjectsUsingBlock:))]
        fn enumerate_objects(&self, block: &MemberBlock) {
            enumerate(self, block);
        }

        /// Concurrent enumeration runs on the calling thread, which the
        /// option permits; sets have no order to reverse.
        #[unsafe(method(enumerateObjectsWithOptions:usingBlock:))]
        fn enumerate_objects_with_options(&self, _options: NSEnumerationOptions, block: &MemberBlock) {
            enumerate(self, block);
        }

        #[unsafe(method(makeObjectsPerformSelector:))]
        fn make_objects_perform_selector(&self, selector: Sel) {
            perform_each(self, selector, None);
        }

        #[unsafe(method(makeObjectsPerformSelector:withObject:))]
        fn make_objects_perform_selector_with_object(&self, selector: Sel, argument: Option<&AnyObject>) {
            perform_each(self, selector, Some(argument));
        }

        #[unsafe(method_id(objectsPassingTest:))]
        fn objects_passing_test(&self, test: &MemberTest) -> Retained<NSSet> {
            make(passing(self, test))
        }

        /// Concurrent testing runs on the calling thread, which the option
        /// permits.
        #[unsafe(method_id(objectsWithOptions:passingTest:))]
        fn objects_with_options_passing_test(&self, _options: NSEnumerationOptions, test: &MemberTest) -> Retained<NSSet> {
            make(passing(self, test))
        }

        #[unsafe(method_id(sortedArrayUsingDescriptors:))]
        fn sorted_array_using_descriptors(&self, descriptors: &NSArray) -> Retained<NSArray> {
            sort_descriptor::sorted(descriptors, &members(self))
        }

        #[unsafe(method(isEqualToSet:))]
        fn is_equal_to_set(&self, other: &NSSet) -> bool {
            sets_equal(self, other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| {
                util::is_kind(other, NSSet::<AnyObject>::class()) && sets_equal(self, other)
            })
        }

        #[unsafe(method(isSubsetOfSet:))]
        fn is_subset_of_set(&self, other: &NSSet) -> bool {
            is_subset(self, other)
        }

        #[unsafe(method(intersectsSet:))]
        fn intersects_set(&self, other: &NSSet) -> bool {
            table(self).entries().iter().any(|e| contains(other, &e.key))
        }

        /// The count, as in Foundation.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            count_of(self)
        }

        #[unsafe(method_id(setByAddingObject:))]
        fn set_by_adding_object(&self, object: Option<&AnyObject>) -> Retained<NSSet> {
            let Some(object) = object else { nil_argument("NSSet", "setByAddingObject:", "object") };
            let mut table = table(self).clone();
            table.insert_first(object.retain(), ());
            make(table)
        }

        #[unsafe(method_id(setByAddingObjectsFromSet:))]
        fn set_by_adding_objects_from_set(&self, other: &NSSet) -> Retained<NSSet> {
            let mut table = table(self).clone();
            for member in members(other) {
                table.insert_first(member, ());
            }
            make(table)
        }

        #[unsafe(method_id(setByAddingObjectsFromArray:))]
        fn set_by_adding_objects_from_array(&self, other: &NSArray) -> Retained<NSSet> {
            let mut table = table(self).clone();
            for item in other.to_vec() {
                table.insert_first(item, ());
            }
            make(table)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSSet> {
            if is_exactly(self, &crate::NSSET) {
                // Immutable: a copy is the same object.
                // SAFETY: NSSetImpl is the class registered as NSSet.
                unsafe { Retained::cast_unchecked(self.retain()) }
            } else {
                make_from(frozen_copy(self))
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableSet> {
            make_mutable_from(thawed_copy(self).into())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            description_of(self)
        }

        #[unsafe(method_id(descriptionWithLocale:))]
        fn description_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            description_of(self)
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            let member = |entries: &[Entry<()>], i: usize| entries.get(i).map(|e| Retained::as_ptr(&e.key).cast_mut());
            match kind(self) {
                Kind::Fixed(s) => {
                    let entries = s.ivars().entries();
                    // SAFETY: the caller passes a valid state and buffer.
                    unsafe { enumerator::batch(state, buffer, len, immutable_mutations(), |i| member(entries, i)) }
                }
                Kind::Mutable(m) => {
                    // SAFETY: copying pointers into the buffer runs no other
                    // code.
                    let entries = unsafe { m.ivars().table.peek() }.entries();
                    let mutations = m.ivars().mutations.as_ptr();
                    // SAFETY: the caller passes a valid state and buffer.
                    unsafe { enumerator::batch(state, buffer, len, mutations, |i| member(entries, i)) }
                }
                Kind::Foreign => {
                    // SAFETY: the caller passes a valid state and buffer.
                    unsafe { enumerator::gathered(state, buffer, len, || array::make(members(self))) }
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for NSSetImpl {}
);

/// A mutable set subclass defined outside Sidestep, changed through the
/// primitives Foundation asks such a subclass to implement.
struct Subclass<'a>(&'a AnyObject);

// SAFETY (every method): NSMutableSet's primitives, with the argument types
// Foundation declares.
impl Subclass<'_> {
    fn add(&self, object: &AnyObject) {
        unsafe { msg_send![self.0, addObject: object] }
    }

    fn remove(&self, object: &AnyObject) {
        unsafe { msg_send![self.0, removeObject: object] }
    }
}

impl NSMutableSetImpl {
    /// This object, for failure messages.
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    /// A subclass defined outside Sidestep, which is changed through its
    /// primitives.
    fn subclass(&self) -> Option<Subclass<'_>> {
        (!is_exactly(self, &crate::NSMUTABLESET)).then(|| Subclass(self))
    }

    /// Put `new` in place of every member.
    fn set_all(&self, new: Arc<Members>) {
        if let Some(subclass) = self.subclass() {
            for member in members(self) {
                subclass.remove(&member);
            }
            for e in new.entries() {
                subclass.add(&e.key);
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

    /// Add `object` unless an equal member is there: `-addObject:` on this
    /// class's own storage.
    fn insert_own(&self, object: Retained<AnyObject>) {
        let probe = Probe::new(&object);
        let hash = probe.hash;
        // SAFETY: only the table changes.
        let located = unsafe { self.ivars().table.write_located(MUTABLE, self.obj(), &probe, Result::is_err) };
        let Some((table, Err(slot))) = located else { return };
        table.push(hash, object, (), slot);
        self.changed();
    }

    /// `-removeObject:` on this class's own storage.
    fn remove_own(&self, object: &AnyObject) {
        let table = &self.ivars().table;
        // SAFETY: only the table changes; what it removes is released after.
        if let Some((table, at)) = unsafe { table.write_identical(MUTABLE, self.obj(), object) } {
            let removed = table.remove(at);
            self.changed();
            drop(removed);
            return;
        }
        let probe = Probe::new(object);
        // SAFETY: only the table changes; the member is released after.
        let located = unsafe { self.ivars().table.write_located(MUTABLE, self.obj(), &probe, Result::is_ok) };
        let Some((table, Ok(at))) = located else { return };
        let removed = table.remove(at);
        self.changed();
        drop(removed);
    }

    fn insert(&self, object: Retained<AnyObject>) {
        match self.subclass() {
            Some(subclass) => subclass.add(&object),
            None => self.insert_own(object),
        }
    }

    fn remove(&self, object: &AnyObject) {
        match self.subclass() {
            Some(subclass) => subclass.remove(object),
            None => self.remove_own(object),
        }
    }
}

define_class!(
    #[unsafe(super(NSSet, NSObject))]
    #[name = "NSMutableSet"]
    #[ivars = MutableSetIvars]
    pub(crate) struct NSMutableSetImpl;

    impl NSMutableSetImpl {
        #[unsafe(method_id(set))]
        fn set() -> Retained<NSMutableSet> {
            make_mutable(Table::default())
        }

        #[unsafe(method_id(setWithCapacity:))]
        fn set_with_capacity(capacity: NSUInteger) -> Retained<NSMutableSet> {
            make_mutable(Table::with_capacity(capacity))
        }

        #[unsafe(method_id(setWithObject:))]
        fn set_with_object(object: &AnyObject) -> Retained<NSMutableSet> {
            make_mutable(Table::of_members([object.retain()]))
        }

        #[unsafe(method_id(setWithObjects:count:))]
        fn set_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSMutableSet> {
            // SAFETY: the caller passes `count` objects.
            make_mutable(unsafe { from_c_array(MUTABLE, objects, count) })
        }

        #[unsafe(method_id(setWithArray:))]
        fn set_with_array(array: &NSArray) -> Retained<NSMutableSet> {
            make_mutable(from_array(array))
        }

        #[unsafe(method_id(setWithSet:))]
        fn set_with_set(set: &NSSet) -> Retained<NSMutableSet> {
            make_mutable_from(thawed_copy(set).into())
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_mutable(this, CowTable::default())
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, capacity: NSUInteger) -> Retained<Self> {
            init_mutable(this, Table::with_capacity(capacity).into())
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            init_mutable(this, unsafe { from_c_array(MUTABLE, objects, count) }.into())
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init_mutable(this, from_array(array).into())
        }

        #[unsafe(method_id(initWithSet:))]
        fn init_with_set(this: Allocated<Self>, set: &NSSet) -> Retained<Self> {
            init_mutable(this, thawed_copy(set).into())
        }

        #[unsafe(method_id(initWithSet:copyItems:))]
        fn init_with_set_copy(this: Allocated<Self>, set: &NSSet, copy: bool) -> Retained<Self> {
            init_mutable(this, if copy { copy_each(set).into() } else { thawed_copy(set).into() })
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            // SAFETY: reading the length runs no other code.
            unsafe { self.ivars().table.peek() }.len()
        }

        /// Neither retained nor autoreleased: the set keeps it alive.
        #[unsafe(method(member:))]
        fn member(&self, object: Option<&AnyObject>) -> *mut AnyObject {
            let table = &self.ivars().table;
            match object.and_then(|o| table.position(o)) {
                // SAFETY: `position` just found the member, and nothing ran
                // since.
                Some(at) => Retained::as_ptr(&unsafe { table.peek() }.entries()[at].key).cast_mut(),
                None => ptr::null_mut(),
            }
        }

        #[unsafe(method(addObject:))]
        fn add_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { nil_argument(MUTABLE, "addObject:", "object") };
            self.insert_own(object.retain());
        }

        #[unsafe(method(removeObject:))]
        fn remove_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { nil_argument(MUTABLE, "removeObject:", "object") };
            self.remove_own(object);
        }

        #[unsafe(method(addObjectsFromArray:))]
        fn add_objects_from_array(&self, array: &NSArray) {
            for item in array.to_vec() {
                self.insert(item);
            }
        }

        #[unsafe(method(unionSet:))]
        fn union_set(&self, other: &NSSet) {
            // Gathered first: `other` may be this set.
            for member in members(other) {
                self.insert(member);
            }
        }

        #[unsafe(method(minusSet:))]
        fn minus_set(&self, other: &NSSet) {
            for member in members(other) {
                self.remove(&member);
            }
        }

        #[unsafe(method(intersectSet:))]
        fn intersect_set(&self, other: &NSSet) {
            // Gathered first: `other` may be this set.
            let others = table(other).clone();
            let doomed: Vec<Retained<AnyObject>> =
                table(self).entries().iter().filter(|e| others.position(&e.key).is_none()).map(|e| e.key.clone()).collect();
            for member in doomed {
                self.remove(&member);
            }
        }

        #[unsafe(method(setSet:))]
        fn set_set(&self, other: &NSSet) {
            // Taken first: `other` may be this set.
            self.set_all(thawed_copy(other));
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.set_all(shared(Table::default()));
        }
    }

    unsafe impl NSObjectProtocol for NSMutableSetImpl {}
);
