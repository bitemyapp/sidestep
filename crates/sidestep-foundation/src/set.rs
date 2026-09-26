//! `NSSet` and `NSMutableSet`: unordered collections of distinct objects,
//! kept in the hash table of `table` with no values.
//!
//! They follow the dictionaries' design: an immutable set's table never
//! changes, a mutable one keeps its own in a `RefCell` with a count of
//! changes, methods that read a set are written once over a borrowed table
//! (or one gathered from a foreign subclass's `-objectEnumerator`), and
//! copies of a mutable set share its table until it next changes. Unlike
//! dictionary keys, members are retained, not copied, and adding an object
//! equal to a member keeps the member.

use std::cell::{Ref, RefMut};
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
use crate::table::{CowTable, Frozen, Probe, Table, Thawed};
use crate::util::{self, is_exactly, nil_argument};
use crate::{array, describe};

type Members = Table<()>;

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

/// A set's table, borrowed for reading.
enum Tables<'a> {
    Fixed(&'a Members),
    Mutable(Ref<'a, Members>),
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
    // SAFETY: NSSet's primitives: -count, and -objectEnumerator returning an
    // enumerator of the members.
    unsafe {
        let count: NSUInteger = msg_send![obj, count];
        let members: Retained<NSEnumerator> = msg_send![obj, objectEnumerator];
        let mut table = Table::with_capacity(count);
        while let Some(member) = members.nextObject() {
            add(&mut table, member);
        }
        table
    }
}

/// Add `member` unless an equal one is there.
fn add(table: &mut Members, member: Retained<AnyObject>) {
    let probe = Probe::new(&member);
    if let Err(slot) = table.locate(&probe) {
        let hash = probe.hash;
        table.push(hash, member, (), slot);
    }
}

fn from_items(items: impl IntoIterator<Item = Retained<AnyObject>>, capacity: usize) -> Members {
    let mut table = Table::with_capacity(capacity);
    for item in items {
        add(&mut table, item);
    }
    table
}

/// The member at position `index` of a Sidestep set, unretained (the set
/// keeps it alive), or `None` past the end.
pub(crate) fn member_at(obj: &AnyObject, index: usize) -> Option<*mut AnyObject> {
    let member = |table: &Members| table.entries().get(index).map(|e| Retained::as_ptr(&e.key).cast_mut());
    match kind(obj) {
        Kind::Fixed(s) => member(s.ivars()),
        Kind::Mutable(m) => member(&m.ivars().table.read()),
        Kind::Foreign => None,
    }
}

/// The mutation count fast enumeration of `obj` watches.
pub(crate) fn mutations(obj: &AnyObject) -> *mut c_ulong {
    match kind(obj) {
        Kind::Mutable(m) => m.ivars().mutations.as_ptr(),
        _ => immutable_mutations(),
    }
}

/// The member of any set equal to `object`, retained.
fn member(obj: &AnyObject, object: &AnyObject) -> Option<Retained<AnyObject>> {
    match kind(obj) {
        Kind::Fixed(s) => s.ivars().get(object).map(|e| e.key.clone()),
        Kind::Mutable(m) => m.ivars().table.read().get(object).map(|e| e.key.clone()),
        // SAFETY: -member: takes an object and returns one or nil.
        Kind::Foreign => unsafe { msg_send![obj, member: object] },
    }
}

fn count_of(obj: &AnyObject) -> usize {
    match kind(obj) {
        Kind::Fixed(s) => s.ivars().len(),
        Kind::Mutable(m) => m.ivars().table.read().len(),
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
        add(&mut table, obj.retain());
    }
    table
}

fn from_array(array: &NSArray) -> Members {
    let items = array.to_vec();
    let count = items.len();
    from_items(items, count)
}

/// The members of any set for a new set to hold: shared with it where
/// copy-on-write allows (`Ok`), else retained afresh (`Err`).
fn copy_members(obj: &AnyObject) -> Result<Arc<Members>, Members> {
    match kind(obj) {
        Kind::Fixed(s) => match s.ivars() {
            Frozen::Shared(table) => Ok(table.clone()),
            Frozen::Owned(table) => Err(table.clone()),
        },
        Kind::Mutable(m) => m.ivars().table.share().ok_or_else(|| m.ivars().table.read().clone()),
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
fn thawed_copy(obj: &AnyObject) -> Thawed<()> {
    match copy_members(obj) {
        Ok(shared) => Thawed::Shared(shared),
        Err(table) => Thawed::Own(table),
    }
}

/// The members of any set, each copied with `-copy`
/// (`-initWithSet:copyItems:`).
fn copy_each(other: &AnyObject) -> Members {
    let table = table(other);
    from_items(table.entries().iter().map(|e| util::copy_key(&e.key)), table.len())
}

/// A new immutable set of `members`.
pub(crate) fn make_of(members: Vec<Retained<AnyObject>>) -> Retained<NSSet> {
    let count = members.len();
    make(from_items(members, count))
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
    NSString::from_str(&out)
}

/// Whether every member of `a` is in `b`.
fn is_subset(a: &AnyObject, b: &AnyObject) -> bool {
    let table = table(a);
    table.len() <= count_of(b) && table.entries().iter().all(|e| member(b, &e.key).is_some())
}

/// `-isEqualToSet:` for any two sets: the same members.
fn sets_equal(a: &AnyObject, b: &AnyObject) -> bool {
    ptr::eq(a, b) || (count_of(a) == count_of(b) && is_subset(a, b))
}

type MemberTest = DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<Bool>) -> Bool>;

/// The members `test` passes, until it sets its stop flag.
fn passing(obj: &AnyObject, test: &MemberTest) -> Members {
    let members = members(obj);
    let mut table = Table::with_capacity(members.len());
    let mut stop = Bool::NO;
    for member in members {
        if test.call((NonNull::from(&*member), NonNull::from(&mut stop))).as_bool() {
            add(&mut table, member);
        }
        if stop.as_bool() {
            break;
        }
    }
    table
}

type MemberBlock = DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<Bool>)>;

/// `-enumerateObjectsUsingBlock:` for any set: the members as they were
/// when it began, each retained for the call, so the block may change the
/// set, as Foundation allows.
fn enumerate(obj: &AnyObject, block: &MemberBlock) {
    let members = members(obj);
    let mut stop = Bool::NO;
    for member in &members {
        block.call((NonNull::from(&**member), NonNull::from(&mut stop)));
        if stop.as_bool() {
            break;
        }
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
            make(from_items([object.retain()], 1))
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
            object.is_some_and(|o| member(self, o).is_some())
        }

        #[unsafe(method(anyObject))]
        fn any_object(&self) -> *mut AnyObject {
            member_at(self, 0).unwrap_or(ptr::null_mut())
        }

        #[unsafe(method_id(allObjects))]
        fn all_objects(&self) -> Retained<NSArray> {
            array::make(members(self))
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            match kind(self) {
                Kind::Foreign => enumerator::make(Source::Array(util::upcast(array::make(members(self))))),
                _ => enumerator::make(Source::Members(util::upcast(self.retain()))),
            }
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
            for member in members(self) {
                array::perform(&member, selector, None);
            }
        }

        #[unsafe(method(makeObjectsPerformSelector:withObject:))]
        fn make_objects_perform_selector_with_object(&self, selector: Sel, argument: Option<&AnyObject>) {
            for member in members(self) {
                array::perform(&member, selector, Some(argument));
            }
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
            table(self).entries().iter().any(|e| member(other, &e.key).is_some())
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
            add(&mut table, object.retain());
            make(table)
        }

        #[unsafe(method_id(setByAddingObjectsFromSet:))]
        fn set_by_adding_objects_from_set(&self, other: &NSSet) -> Retained<NSSet> {
            let mut table = table(self).clone();
            for member in members(other) {
                add(&mut table, member);
            }
            make(table)
        }

        #[unsafe(method_id(setByAddingObjectsFromArray:))]
        fn set_by_adding_objects_from_array(&self, other: &NSArray) -> Retained<NSSet> {
            let mut table = table(self).clone();
            for item in other.to_vec() {
                add(&mut table, item);
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
            if let Kind::Foreign = kind(self) {
                return foreign_members(self, state, buffer, len);
            }
            // SAFETY: the caller passes a valid state and buffer.
            unsafe { enumerator::batch(state, buffer, len, mutations(self), |i| member_at(self, i)) }
        }
    }

    unsafe impl NSObjectProtocol for NSSetImpl {}
);

/// Fast enumeration of a foreign subclass's members: an array of them,
/// gathered on the first call and kept (autoreleased) in the state.
fn foreign_members(
    obj: &AnyObject,
    state: NonNull<NSFastEnumerationState>,
    buffer: NonNull<*mut AnyObject>,
    len: NSUInteger,
) -> NSUInteger {
    // SAFETY: the caller passes a valid state.
    let st = unsafe { &mut *state.as_ptr() };
    if st.state == 0 {
        st.extra[0] = Retained::autorelease_ptr(array::make(members(obj))) as usize as c_ulong;
    }
    // SAFETY: the array, autoreleased into the caller's pool, outlives its
    // enumeration loop.
    let items = unsafe { &*(st.extra[0] as usize as *const AnyObject) };
    // SAFETY: the caller passes a valid state and buffer.
    unsafe { enumerator::batch(state, buffer, len, immutable_mutations(), |i| array::element_at(items, i)) }
}

impl NSMutableSetImpl {
    fn read(&self) -> Ref<'_, Members> {
        self.ivars().table.read()
    }

    /// The table, for changing. Changing the set from inside one of its
    /// own reading methods (a member's `-isEqual:`) fails here.
    fn write(&self) -> RefMut<'_, Members> {
        self.ivars().table.write("NSMutableSet", ptr::from_ref(self).cast())
    }

    /// Put `new` in place of every member.
    fn set_all(&self, new: Thawed<()>) {
        let old = self.ivars().table.replace(new, "NSMutableSet", ptr::from_ref(self).cast());
        self.changed();
        drop(old);
    }

    fn changed(&self) {
        self.ivars().mutations.bump();
    }

    /// Add `object` unless an equal member is there.
    fn insert(&self, object: Retained<AnyObject>) {
        let probe = Probe::new(&object);
        let hash = probe.hash;
        let found = self.read().locate(&probe);
        if let Err(slot) = found {
            self.write().push(hash, object, (), slot);
            self.changed();
        }
    }

    fn remove(&self, object: &AnyObject) {
        let found = self.read().position(object);
        if let Some(at) = found {
            let removed = self.write().remove(at);
            self.changed();
            drop(removed);
        }
    }

    /// Keep only the members `keep` approves; it may send messages, so it
    /// runs before anything changes.
    fn retain_where(&self, mut keep: impl FnMut(&AnyObject) -> bool) {
        let doomed: Vec<Retained<AnyObject>> =
            self.read().entries().iter().filter(|e| !keep(&e.key)).map(|e| e.key.clone()).collect();
        for member in doomed {
            self.remove(&member);
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
            make_mutable(from_items([object.retain()], 1))
        }

        #[unsafe(method_id(setWithObjects:count:))]
        fn set_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSMutableSet> {
            // SAFETY: the caller passes `count` objects.
            make_mutable(unsafe { from_c_array("NSMutableSet", objects, count) })
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
            init_mutable(this, unsafe { from_c_array("NSMutableSet", objects, count) }.into())
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
            self.read().len()
        }

        /// Neither retained nor autoreleased: the set keeps it alive.
        #[unsafe(method(member:))]
        fn member(&self, object: Option<&AnyObject>) -> *mut AnyObject {
            match object.and_then(|o| self.read().get(o).map(|e| Retained::as_ptr(&e.key).cast_mut())) {
                Some(member) => member,
                None => ptr::null_mut(),
            }
        }

        #[unsafe(method(addObject:))]
        fn add_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { nil_argument("NSMutableSet", "addObject:", "object") };
            self.insert(object.retain());
        }

        #[unsafe(method(removeObject:))]
        fn remove_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { nil_argument("NSMutableSet", "removeObject:", "object") };
            self.remove(object);
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
            self.retain_where(|m| others.get(m).is_some());
        }

        #[unsafe(method(setSet:))]
        fn set_set(&self, other: &NSSet) {
            // Taken first: `other` may be this set.
            self.set_all(thawed_copy(other));
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.set_all(Thawed::Own(Table::default()));
        }
    }

    unsafe impl NSObjectProtocol for NSMutableSetImpl {}
);
