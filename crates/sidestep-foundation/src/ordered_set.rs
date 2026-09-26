//! `NSOrderedSet` and `NSMutableOrderedSet`: distinct objects in an order,
//! looked up by equality in constant time.
//!
//! An ordered set keeps its members in a vector, which is their order, with
//! each member's hash beside it and, past a handful of members, an index of
//! positions by hash (`hash_index.rs`), so `-indexOfObject:` and
//! `-containsObject:` cost a hash and usually one `-isEqual:`. Members lie
//! side by side, so fast enumeration hands out the vector itself, as an
//! array's does. Adding at the end costs constant time; a change in the
//! middle moves the members after it and renumbers the index, which costs
//! time in proportion to the set, as moving an array's elements does.
//!
//! The design is the arrays' and sets' (see `array.rs` and `set.rs`): an
//! immutable ordered set's members never change after `init`, so any thread
//! may read them; a mutable one keeps them in a [`Cow`] cell with a count of
//! changes, shared with copies until it next changes. Methods that read an
//! ordered set are written once over its members held for reading, or
//! gathered through the primitives (`-count`, `-objectAtIndex:`) of a
//! subclass defined outside Sidestep; such a mutable subclass is changed
//! through `-insertObject:atIndex:`, `-removeObjectAtIndex:` and
//! `-replaceObjectAtIndex:withObject:`, as Foundation asks. Adding an object
//! equal to a member changes nothing, wherever it was asked to go.
//!
//! `-array` and `-set` answer proxies that read the ordered set as it is
//! when asked, so they follow a mutable ordered set's changes, as
//! Foundation's do; copying one takes a snapshot.

use std::cmp::Ordering;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSBinarySearchingOptions, NSComparator, NSEnumerationOptions, NSEnumerator, NSFastEnumerationState,
    NSIndexSet, NSMutableOrderedSet, NSNotFound, NSOrderedSet, NSRange, NSSet, NSSortOptions, NSString, NSUInteger,
    NSZone,
};
use sidestep_runtime::Class;

use crate::enumerator::{self, Mutations, Source, immutable_mutations};
use crate::guarded::{Cow, Reading, counted};
use crate::hash_index::Index;
use crate::table::Probe;
use crate::util::{self, index_beyond, inherits, is_exactly, nil_argument};
use crate::{array, describe, index_set, set, sort_descriptor};

sidestep_runtime::static_class!(pub(crate) NSORDEREDSET, NSORDEREDSET_META = "NSOrderedSet", || {
    let _ = NSOrderedSetImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSMUTABLEORDEREDSET, NSMUTABLEORDEREDSET_META = "NSMutableOrderedSet", || {
    let _ = NSMutableOrderedSetImpl::class();
});

// The proxies `-array` and `-set` answer.
sidestep_runtime::static_class!(pub(crate) ARRAY_PROXY, ARRAY_PROXY_META = "_SidestepOrderedSetArray", || {
    let _ = ArrayProxy::class();
});

sidestep_runtime::static_class!(pub(crate) SET_PROXY, SET_PROXY_META = "_SidestepOrderedSetSet", || {
    let _ = SetProxy::class();
});

const FIXED: &str = "NSOrderedSet";
const MUTABLE: &str = "NSMutableOrderedSet";
/// What Foundation's failures call an empty receiver.
const NOUN: &str = "ordered set";
/// Up to this many members, an ordered set has no index: lookups look
/// through the members in order, first for the very object sought.
const SCAN: usize = 8;

type Items = Vec<Retained<AnyObject>>;

/// An ordered set's members, in order, with their hashes and an index of
/// their positions.
#[derive(Clone, Default)]
pub(crate) struct Members {
    items: Items,
    hashes: Vec<NSUInteger>,
    /// Empty while there are at most `SCAN` members.
    index: Index,
}

impl Members {
    /// `items` without duplicates: the first of equal objects stays.
    fn of(items: impl IntoIterator<Item = Retained<AnyObject>>) -> Members {
        let items = items.into_iter();
        let mut members =
            Members { items: Vec::with_capacity(items.size_hint().0), hashes: Vec::new(), index: Index::default() };
        for item in items {
            let probe = Probe::new(&item);
            if members.locate(&probe).is_none() {
                let (hash, at) = (probe.hash, members.len());
                members.insert(at, hash, item);
            }
        }
        members
    }

    /// Members known to be distinct, with their hashes.
    fn from_parts(items: Items, hashes: Vec<NSUInteger>) -> Members {
        let mut members = Members { items, hashes, index: Index::default() };
        members.reindex();
        members
    }

    #[inline]
    pub(crate) fn items(&self) -> &[Retained<AnyObject>] {
        &self.items
    }

    #[inline]
    fn len(&self) -> usize {
        self.items.len()
    }

    fn reindex(&mut self) {
        if self.items.len() <= SCAN {
            self.index.clear();
            return;
        }
        let hashes = &self.hashes;
        self.index.rebuild(Index::size_for(hashes.len()), hashes.len(), |at| hashes[at]);
    }

    /// The position of the very object `key`, in a set small enough to
    /// look through. Sends no messages.
    #[inline]
    fn identical(&self, key: &AnyObject) -> Option<usize> {
        if self.items.len() <= SCAN { self.items.iter().position(|e| ptr::eq(&**e, key)) } else { None }
    }

    /// The position of the member `probe` matches. May send `-isEqual:`.
    fn locate(&self, probe: &Probe) -> Option<usize> {
        if self.index.is_empty() {
            return (0..self.items.len()).find(|&i| probe.matches_key(self.hashes[i], &self.items[i]));
        }
        self.index.find(probe.hash, |at| probe.matches_key(self.hashes[at], &self.items[at])).ok()
    }

    /// `locate`, if it can be done without sending a message.
    fn locate_quietly(&self, probe: &Probe) -> Option<Option<usize>> {
        if self.index.is_empty() {
            for i in 0..self.items.len() {
                if probe.matches_key_quietly(self.hashes[i], &self.items[i])? {
                    return Some(Some(i));
                }
            }
            return Some(None);
        }
        self.index
            .find_quietly(probe.hash, |at| probe.matches_key_quietly(self.hashes[at], &self.items[at]))
            .map(|r| r.ok())
    }

    /// The position of a member equal to `key`. May send messages.
    fn position(&self, key: &AnyObject) -> Option<usize> {
        self.identical(key).or_else(|| self.locate(&Probe::new(key)))
    }

    /// Put `item`, known to be absent, at `at`. Sends no messages.
    fn insert(&mut self, at: usize, hash: NSUInteger, item: Retained<AnyObject>) {
        let len = self.items.len();
        self.items.insert(at, item);
        self.hashes.insert(at, hash);
        if self.index.is_empty() {
            if len + 1 > SCAN {
                self.reindex();
            }
        } else if self.index.full_for(len + 1) {
            self.reindex();
        } else {
            if at < len {
                self.index.shift_up(at);
            }
            self.index.add(hash, at);
        }
    }

    /// Take out the member at `at`. Sends no messages: the caller releases
    /// it.
    fn remove(&mut self, at: usize) -> Retained<AnyObject> {
        if !self.index.is_empty() {
            let hashes = &self.hashes;
            self.index.remove(hashes[at], at, |p| hashes[p]);
            if at + 1 < hashes.len() {
                self.index.shift_down(at);
            }
        }
        self.hashes.remove(at);
        self.items.remove(at)
    }

    /// Put `item`, known to be absent, in place of the member at `at`,
    /// which is returned for the caller to release.
    fn replace(&mut self, at: usize, hash: NSUInteger, item: Retained<AnyObject>) -> Retained<AnyObject> {
        if !self.index.is_empty() {
            let hashes = &self.hashes;
            self.index.remove(hashes[at], at, |p| hashes[p]);
            self.index.add(hash, at);
        }
        self.hashes[at] = hash;
        std::mem::replace(&mut self.items[at], item)
    }

    fn swap(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        if !self.index.is_empty() {
            // Through a position no member has, so the two never collide.
            let aside = self.items.len();
            self.index.moved(self.hashes[a], a, aside);
            self.index.moved(self.hashes[b], b, a);
            self.index.moved(self.hashes[a], aside, b);
        }
        self.items.swap(a, b);
        self.hashes.swap(a, b);
    }

    /// Keep the members `keep` flags, in order; the others are returned for
    /// the caller to release.
    fn keep(&mut self, keep: &[bool]) -> Items {
        let (items, hashes) = (std::mem::take(&mut self.items), std::mem::take(&mut self.hashes));
        let mut removed = Vec::new();
        for ((item, hash), &kept) in items.into_iter().zip(hashes).zip(keep) {
            if kept {
                self.items.push(item);
                self.hashes.push(hash);
            } else {
                removed.push(item);
            }
        }
        self.reindex();
        removed
    }

    /// Put the members in the order `order` gives: the member at
    /// `order[i]` moves to `i`. No reference counts change.
    fn permute(&mut self, order: &[usize]) {
        let mut items: Vec<Option<Retained<AnyObject>>> =
            std::mem::take(&mut self.items).into_iter().map(Some).collect();
        let hashes = std::mem::take(&mut self.hashes);
        self.items = order.iter().map(|&i| items[i].take().expect("a permutation")).collect();
        self.hashes = order.iter().map(|&i| hashes[i]).collect();
        self.reindex();
    }
}

/// An immutable ordered set's members: its own, or shared with the mutable
/// ordered set it was copied from.
pub(crate) enum Fixed {
    Owned(Members),
    Shared(Arc<Members>),
}

impl Default for Fixed {
    fn default() -> Self {
        Fixed::Owned(Members::default())
    }
}

impl Deref for Fixed {
    type Target = Members;

    #[inline]
    fn deref(&self) -> &Members {
        match self {
            Fixed::Owned(members) => members,
            Fixed::Shared(members) => members,
        }
    }
}

#[derive(Default)]
pub(crate) struct MutableIvars {
    /// Shared with copies until the next change.
    members: Cow<Members>,
    mutations: Mutations,
}

/// Which storage an ordered set object has.
enum Kind<'a> {
    Fixed(&'a NSOrderedSetImpl),
    Mutable(&'a NSMutableOrderedSetImpl),
    /// A subclass from outside Sidestep, reached through messages.
    Foreign,
}

fn kind(obj: &AnyObject) -> Kind<'_> {
    if is_exactly(obj, &NSORDEREDSET) {
        // SAFETY: an instance of exactly NSOrderedSetImpl.
        Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSOrderedSetImpl>() })
    } else if is_exactly(obj, &NSMUTABLEORDEREDSET) {
        // SAFETY: an instance of exactly NSMutableOrderedSetImpl.
        Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableOrderedSetImpl>() })
    } else {
        Kind::Foreign
    }
}

/// Whether `obj` is an ordered set of any class.
pub(crate) fn is_ordered_set(obj: &AnyObject) -> bool {
    matches!(kind(obj), Kind::Fixed(_) | Kind::Mutable(_)) || inherits(obj, &NSORDEREDSET)
}

/// An ordered set's members, held for reading.
enum Held<'a> {
    Fixed(&'a Members),
    Mutable(Reading<'a, Arc<Members>>),
    Gathered(Members),
}

impl Deref for Held<'_> {
    type Target = Members;

    fn deref(&self) -> &Members {
        match self {
            Held::Fixed(members) => members,
            Held::Mutable(members) => members,
            Held::Gathered(members) => members,
        }
    }
}

fn members(obj: &AnyObject) -> Held<'_> {
    match kind(obj) {
        Kind::Fixed(s) => Held::Fixed(s.ivars()),
        Kind::Mutable(m) => Held::Mutable(m.ivars().members.read()),
        Kind::Foreign => Held::Gathered(Members::of(gather(obj))),
    }
}

/// The members of an ordered set subclass defined outside Sidestep, in
/// order, through its primitive methods.
fn gather(obj: &AnyObject) -> Items {
    // SAFETY: NSOrderedSet's primitives: -count returns NSUInteger and
    // -objectAtIndex: an object for every index below it.
    unsafe {
        let count: NSUInteger = msg_send![obj, count];
        (0..count).map(|i| msg_send![obj, objectAtIndex: i]).collect()
    }
}

/// The count of any ordered set.
pub(crate) fn count_of(obj: &AnyObject) -> usize {
    match kind(obj) {
        Kind::Fixed(s) => s.ivars().len(),
        // SAFETY: reading the length runs no other code.
        Kind::Mutable(m) => unsafe { m.ivars().members.peek() }.len(),
        // SAFETY: -count takes nothing and returns NSUInteger.
        Kind::Foreign => unsafe { msg_send![obj, count] },
    }
}

/// The member at `index` of any ordered set, unretained (the set keeps it
/// alive), or `None` past the end.
pub(crate) fn element_at(obj: &AnyObject, index: usize) -> Option<*mut AnyObject> {
    let item = |members: &Members| members.items.get(index).map(|o| Retained::as_ptr(o).cast_mut());
    match kind(obj) {
        Kind::Fixed(s) => item(s.ivars()),
        // SAFETY: reading one pointer runs no other code.
        Kind::Mutable(m) => item(unsafe { m.ivars().members.peek() }),
        Kind::Foreign => {
            // SAFETY: as in `gather`; the set keeps the member alive.
            let count: NSUInteger = unsafe { msg_send![obj, count] };
            (index < count).then(|| unsafe { msg_send![obj, objectAtIndex: index] })
        }
    }
}

/// The position of a member of any ordered set equal to `key`. Retains
/// nothing.
fn index_of(obj: &AnyObject, key: &AnyObject) -> Option<usize> {
    match kind(obj) {
        Kind::Fixed(s) => s.ivars().position(key),
        Kind::Mutable(m) => m.find(key),
        Kind::Foreign => {
            // SAFETY: -indexOfObject: takes an object and returns an index.
            let at: NSUInteger = unsafe { msg_send![obj, indexOfObject: key] };
            (at != NSNotFound as NSUInteger).then_some(at)
        }
    }
}

/// The name Foundation's messages give a receiver.
fn name(obj: &AnyObject) -> &'static str {
    match kind(obj) {
        Kind::Mutable(_) => MUTABLE,
        _ if inherits(obj, &NSMUTABLEORDEREDSET) => MUTABLE,
        _ => FIXED,
    }
}

/// The name an ordered set's enumerator failures give it.
pub(crate) fn enumerator_name(obj: &AnyObject) -> &'static str {
    name(obj)
}

fn check_range(obj: &AnyObject, method: &str, range: NSRange, count: usize) {
    if range.location.checked_add(range.length).is_none_or(|end| end > count) {
        util::range_beyond(name(obj), method, range.location, range.length, count, NOUN);
    }
}

/// How an index set reaching past the end fails: most name the index "in
/// index set"; some don't.
#[derive(Clone, Copy)]
enum Past {
    InIndexSet,
    Index,
}

/// The ranges of `indexes`, after failing as Foundation does if they reach
/// past `count`.
fn checked_spans(receiver: &str, method: &str, indexes: &NSIndexSet, count: usize, past: Past) -> Vec<(usize, usize)> {
    let spans = index_set::spans(indexes);
    if let Some(&(_, end)) = spans.last()
        && end > count
    {
        match past {
            Past::InIndexSet => util::index_set_beyond(receiver, method, end - 1, count, NOUN),
            Past::Index => index_beyond(receiver, method, end - 1, count, NOUN),
        }
    }
    spans
}

/// A new immutable ordered set of `members`.
pub(crate) fn make(members: Members) -> Retained<NSOrderedSet> {
    make_from(Fixed::Owned(members))
}

fn make_from(members: Fixed) -> Retained<NSOrderedSet> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSOrderedSet::<AnyObject>::alloc();
    // SAFETY: NSOrderedSet's class is NSOrderedSetImpl, and an `Allocated`
    // is a pointer to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSOrderedSet>, Allocated<NSOrderedSetImpl>>(this) };
    // SAFETY: NSOrderedSetImpl is the class registered as NSOrderedSet.
    unsafe { Retained::cast_unchecked(init_fixed(this, members)) }
}

fn make_mutable(members: Cow<Members>) -> Retained<NSMutableOrderedSet> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSMutableOrderedSet::<AnyObject>::alloc();
    // SAFETY: as in `make_from`, for NSMutableOrderedSetImpl.
    let this =
        unsafe { std::mem::transmute::<Allocated<NSMutableOrderedSet>, Allocated<NSMutableOrderedSetImpl>>(this) };
    // SAFETY: NSMutableOrderedSetImpl is the class registered as
    // NSMutableOrderedSet.
    unsafe { Retained::cast_unchecked(init_mutable(this, members)) }
}

fn init_fixed(this: Allocated<NSOrderedSetImpl>, members: Fixed) -> Retained<NSOrderedSetImpl> {
    let this = this.set_ivars(members);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn init_mutable(this: Allocated<NSMutableOrderedSetImpl>, members: Cow<Members>) -> Retained<NSMutableOrderedSetImpl> {
    let this = this.set_ivars(MutableIvars { members, mutations: Mutations::default() });
    // SAFETY: NSOrderedSet's initializer, which leaves its own storage
    // empty.
    unsafe { msg_send![super(this), init] }
}

/// The members of any ordered set for a new one to hold: shared where
/// copy-on-write allows (`Ok`), else retained afresh (`Err`).
fn copy_members(obj: &AnyObject) -> Result<Arc<Members>, Members> {
    match kind(obj) {
        Kind::Fixed(s) => match s.ivars() {
            Fixed::Shared(members) => Ok(members.clone()),
            Fixed::Owned(members) => Err(members.clone()),
        },
        Kind::Mutable(m) => Ok(m.ivars().members.share()),
        Kind::Foreign => Err(Members::of(gather(obj))),
    }
}

fn fixed_copy(obj: &AnyObject) -> Fixed {
    match copy_members(obj) {
        Ok(shared) => Fixed::Shared(shared),
        Err(members) => Fixed::Owned(members),
    }
}

fn growable_copy(obj: &AnyObject) -> Cow<Members> {
    match copy_members(obj) {
        Ok(shared) => Cow::shared(shared),
        Err(members) => Cow::fresh(members),
    }
}

/// `count` objects from a C array, retained. A nil among them fails as
/// Foundation's `method` does.
///
/// # Safety
/// `objects` must point to `count` object pointers (or be anything when
/// `count` is 0).
unsafe fn from_c_array(receiver: &str, method: &str, objects: *const *mut AnyObject, count: usize) -> Items {
    (0..count)
        .map(|i| {
            // SAFETY: guaranteed by the caller.
            let obj = unsafe { *objects.add(i) };
            match NonNull::new(obj) {
                // SAFETY: a live object from the caller.
                Some(obj) => unsafe { obj.as_ref() }.retain(),
                None => panic!("*** -[{receiver} {method}]: attempt to insert nil object from objects[{i}]"),
            }
        })
        .collect()
}

/// `items`, each copied with `-copy` if `copy` says so.
fn copied(items: Items, copy: bool) -> Items {
    if copy { items.iter().map(|o| util::copy_key(o)).collect() } else { items }
}

/// The members of `array` in `range` (checked as `method` checks it).
fn array_range(receiver: &str, method: &str, array: &NSArray, range: NSRange) -> Items {
    let items = array.to_vec();
    if range.location.checked_add(range.length).is_none_or(|end| end > items.len()) {
        util::range_out_of_bounds(receiver, method, range.location, range.length, items.len());
    }
    items[range.location..range.location + range.length].to_vec()
}

/// The members of the ordered set `other` in `range`.
fn ordered_range(receiver: &str, method: &str, other: &AnyObject, range: NSRange) -> Items {
    let held = members(other);
    let items = held.items();
    if range.location.checked_add(range.length).is_none_or(|end| end > items.len()) {
        util::range_beyond(receiver, method, range.location, range.length, items.len(), NOUN);
    }
    items[range.location..range.location + range.length].to_vec()
}

/// The members of any `NSSet`, in its order.
fn set_members(other: &NSSet) -> Items {
    other.to_vec()
}

/// Whether `a` and `b` are ordered sets with equal members in the same
/// order.
fn ordered_equal(a: &AnyObject, b: &AnyObject) -> bool {
    if ptr::eq(a, b) {
        return true;
    }
    let (x, y) = (members(a), members(b));
    x.len() == y.len() && x.items.iter().zip(&y.items).all(|(p, q)| util::equal(p, q))
}

/// Append the description of any ordered set at `level`.
fn describe_at(obj: &AnyObject, level: usize) -> Retained<NSString> {
    let held = members(obj);
    let mut out = String::new();
    describe::list(&mut out, "{(", ")}", held.items.iter().map(|o| &**o), level);
    drop(held);
    NSString::from_str(&out)
}

/// Whether `other` (an ordered set, or a set) has a member equal to
/// `object`.
fn in_other(other: &AnyObject, object: &AnyObject) -> bool {
    if is_ordered_set(other) { index_of(other, object).is_some() } else { set::contains(other, object) }
}

fn intersects(obj: &AnyObject, other: &AnyObject) -> bool {
    members(obj).items.iter().any(|o| in_other(other, o))
}

fn is_subset(obj: &AnyObject, other: &AnyObject) -> bool {
    members(obj).items.iter().all(|o| in_other(other, o))
}

type ElementBlock = DynBlock<dyn Fn(NonNull<AnyObject>, NSUInteger, NonNull<Bool>)>;
type ElementTest = DynBlock<dyn Fn(NonNull<AnyObject>, NSUInteger, NonNull<Bool>) -> Bool>;

/// Call `each` with the members at `spans`' indexes, each retained for the
/// call, until it returns false: the callee may change the set.
fn walk_members(
    obj: &AnyObject,
    spans: &[(usize, usize)],
    reverse: bool,
    mut each: impl FnMut(&AnyObject, usize) -> bool,
) {
    if let Kind::Fixed(s) = kind(obj) {
        // Nothing can change an immutable ordered set: no retains needed.
        let items = &s.ivars().items;
        for i in index_set::walk(spans, reverse) {
            if !each(&items[i], i) {
                break;
            }
        }
        return;
    }
    for i in index_set::walk(spans, reverse) {
        // SAFETY: the set keeps the member alive until retained.
        let Some(item) = element_at(obj, i).map(|p| unsafe { Retained::retain(p) }.expect("non-null")) else {
            break;
        };
        if !each(&item, i) {
            break;
        }
    }
}

fn enumerate(obj: &AnyObject, spans: &[(usize, usize)], options: NSEnumerationOptions, block: &ElementBlock) {
    let mut stop = Bool::NO;
    walk_members(obj, spans, options.contains(NSEnumerationOptions::Reverse), |item, i| {
        block.call((NonNull::from(item), i, NonNull::from(&mut stop)));
        !stop.as_bool()
    });
}

/// Call `test` on the members at `spans` until it stops; `on_pass` hears
/// of those that pass and says whether to go on.
fn test_members(
    obj: &AnyObject,
    spans: &[(usize, usize)],
    options: NSEnumerationOptions,
    test: &ElementTest,
    mut on_pass: impl FnMut(usize) -> bool,
) {
    let mut stop = Bool::NO;
    walk_members(obj, spans, options.contains(NSEnumerationOptions::Reverse), |item, i| {
        if test.call((NonNull::from(item), i, NonNull::from(&mut stop))).as_bool() && !on_pass(i) {
            return false;
        }
        !stop.as_bool()
    });
}

fn first_passing(
    obj: &AnyObject,
    spans: &[(usize, usize)],
    options: NSEnumerationOptions,
    test: &ElementTest,
) -> NSUInteger {
    let mut first = NSNotFound as NSUInteger;
    test_members(obj, spans, options, test, |i| {
        first = i;
        false
    });
    first
}

fn all_passing(
    obj: &AnyObject,
    spans: &[(usize, usize)],
    options: NSEnumerationOptions,
    test: &ElementTest,
) -> Retained<NSIndexSet> {
    let mut passed = Vec::new();
    test_members(obj, spans, options, test, |i| {
        passed.push(i);
        true
    });
    index_set::make(index_set::Ranges::of_indexes(passed))
}

fn everything(obj: &AnyObject) -> Vec<(usize, usize)> {
    vec![(0, count_of(obj))]
}

/// The members of `obj` in the order `order` gives, stably, as an array.
fn sorted_array(
    obj: &AnyObject,
    mut order: impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering,
) -> Retained<NSArray> {
    let held = members(obj);
    let mut objects = array::pointers(&held.items);
    array::sort_stable(&mut objects, &mut order);
    // SAFETY: the pointers are the set's members, alive while it is held.
    let sorted = objects.into_iter().map(|p| unsafe { Retained::retain(p) }.expect("non-null")).collect();
    drop(held);
    array::make(sorted)
}

/// The class `+alloc` is sent to for a private class's instances: its
/// shell, which the first message loads.
fn shell(class: &'static Class) -> &'static AnyClass {
    // SAFETY: a static class shell is a class object; the runtime loads it
    // on its first message.
    unsafe { &*(class as *const Class).cast::<AnyClass>() }
}

/// The live array view of `obj` that `-array` answers.
fn array_proxy(obj: &AnyObject) -> Retained<NSArray> {
    // SAFETY: +alloc on the proxy's shell returns an instance of the
    // proxy's class.
    let this: Allocated<ArrayProxy> = unsafe { msg_send![shell(&ARRAY_PROXY), alloc] };
    let this = this.set_ivars(obj.retain());
    // SAFETY: NSArray's initializer, which leaves its own storage empty.
    let this: Retained<ArrayProxy> = unsafe { msg_send![super(this), init] };
    // SAFETY: the proxy's class is a subclass of NSArray.
    unsafe { Retained::cast_unchecked(this) }
}

/// The live set view of `obj` that `-set` answers.
fn set_proxy(obj: &AnyObject) -> Retained<NSSet> {
    // SAFETY: as in `array_proxy`.
    let this: Allocated<SetProxy> = unsafe { msg_send![shell(&SET_PROXY), alloc] };
    let this = this.set_ivars(obj.retain());
    // SAFETY: NSSet's initializer, which leaves its own storage empty.
    let this: Retained<SetProxy> = unsafe { msg_send![super(this), init] };
    // SAFETY: the proxy's class is a subclass of NSSet.
    unsafe { Retained::cast_unchecked(this) }
}

/// Fast enumeration of any ordered set, from its own storage where it has
/// one.
///
/// # Safety
/// `state` must be valid and `buffer` must have room for `len` pointers.
unsafe fn fast_enumerate(
    obj: &AnyObject,
    state: NonNull<NSFastEnumerationState>,
    buffer: NonNull<*mut AnyObject>,
    len: usize,
) -> NSUInteger {
    match kind(obj) {
        Kind::Fixed(s) => {
            let items = &s.ivars().items;
            // SAFETY: the storage never changes; `Retained` is a pointer.
            unsafe { enumerator::whole(state, items.as_ptr().cast(), items.len(), immutable_mutations()) }
        }
        Kind::Mutable(m) => {
            // SAFETY: reading the vector's place runs no other code.
            let items = &unsafe { m.ivars().members.peek() }.items;
            // SAFETY: the vector stays put until the set next changes,
            // which bumps the count the caller watches.
            unsafe { enumerator::whole(state, items.as_ptr().cast(), items.len(), m.ivars().mutations.as_ptr()) }
        }
        // SAFETY: the caller passes a valid state and buffer.
        Kind::Foreign => unsafe { enumerator::gathered(state, buffer, len, || array::make(gather(obj))) },
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSOrderedSet"]
    #[ivars = Fixed]
    pub(crate) struct NSOrderedSetImpl;

    impl NSOrderedSetImpl {
        #[unsafe(method_id(orderedSet))]
        fn ordered_set() -> Retained<NSOrderedSet> {
            make(Members::default())
        }

        #[unsafe(method_id(orderedSetWithObject:))]
        fn ordered_set_with_object(object: &AnyObject) -> Retained<NSOrderedSet> {
            make(Members::of([object.retain()]))
        }

        #[unsafe(method_id(orderedSetWithObjects:count:))]
        fn ordered_set_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSOrderedSet> {
            // SAFETY: the caller passes `count` objects.
            make(Members::of(unsafe { from_c_array(FIXED, "initWithObjects:count:", objects, count) }))
        }

        #[unsafe(method_id(orderedSetWithOrderedSet:))]
        fn ordered_set_with_ordered_set(other: &NSOrderedSet) -> Retained<NSOrderedSet> {
            make_from(fixed_copy(other))
        }

        #[unsafe(method_id(orderedSetWithOrderedSet:range:copyItems:))]
        fn ordered_set_with_ordered_set_range(other: &NSOrderedSet, range: NSRange, copy: bool) -> Retained<NSOrderedSet> {
            let items = ordered_range(FIXED, "initWithOrderedSet:range:copyItems:", other, range);
            make(Members::of(copied(items, copy)))
        }

        #[unsafe(method_id(orderedSetWithArray:))]
        fn ordered_set_with_array(array: &NSArray) -> Retained<NSOrderedSet> {
            make(Members::of(array.to_vec()))
        }

        #[unsafe(method_id(orderedSetWithArray:range:copyItems:))]
        fn ordered_set_with_array_range(array: &NSArray, range: NSRange, copy: bool) -> Retained<NSOrderedSet> {
            let items = array_range(FIXED, "initWithArray:range:copyItems:", array, range);
            make(Members::of(copied(items, copy)))
        }

        #[unsafe(method_id(orderedSetWithSet:))]
        fn ordered_set_with_set(other: &NSSet) -> Retained<NSOrderedSet> {
            make(Members::of(set_members(other)))
        }

        #[unsafe(method_id(orderedSetWithSet:copyItems:))]
        fn ordered_set_with_set_copy(other: &NSSet, copy: bool) -> Retained<NSOrderedSet> {
            make(Members::of(copied(set_members(other), copy)))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_fixed(this, Fixed::default())
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            let items = unsafe { from_c_array(FIXED, "initWithObjects:count:", objects, count) };
            init_fixed(this, Fixed::Owned(Members::of(items)))
        }

        #[unsafe(method_id(initWithObject:))]
        fn init_with_object(this: Allocated<Self>, object: &AnyObject) -> Retained<Self> {
            init_fixed(this, Fixed::Owned(Members::of([object.retain()])))
        }

        #[unsafe(method_id(initWithOrderedSet:))]
        fn init_with_ordered_set(this: Allocated<Self>, other: &NSOrderedSet) -> Retained<Self> {
            init_fixed(this, fixed_copy(other))
        }

        #[unsafe(method_id(initWithOrderedSet:copyItems:))]
        fn init_with_ordered_set_copy(this: Allocated<Self>, other: &NSOrderedSet, copy: bool) -> Retained<Self> {
            let members = if copy { Fixed::Owned(Members::of(copied(gather_any(other), true))) } else { fixed_copy(other) };
            init_fixed(this, members)
        }

        #[unsafe(method_id(initWithOrderedSet:range:copyItems:))]
        fn init_with_ordered_set_range(this: Allocated<Self>, other: &NSOrderedSet, range: NSRange, copy: bool) -> Retained<Self> {
            let items = ordered_range(FIXED, "initWithOrderedSet:range:copyItems:", other, range);
            init_fixed(this, Fixed::Owned(Members::of(copied(items, copy))))
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init_fixed(this, Fixed::Owned(Members::of(array.to_vec())))
        }

        #[unsafe(method_id(initWithArray:copyItems:))]
        fn init_with_array_copy(this: Allocated<Self>, array: &NSArray, copy: bool) -> Retained<Self> {
            init_fixed(this, Fixed::Owned(Members::of(copied(array.to_vec(), copy))))
        }

        #[unsafe(method_id(initWithArray:range:copyItems:))]
        fn init_with_array_range(this: Allocated<Self>, array: &NSArray, range: NSRange, copy: bool) -> Retained<Self> {
            let items = array_range(FIXED, "initWithArray:range:copyItems:", array, range);
            init_fixed(this, Fixed::Owned(Members::of(copied(items, copy))))
        }

        #[unsafe(method_id(initWithSet:))]
        fn init_with_set(this: Allocated<Self>, other: &NSSet) -> Retained<Self> {
            init_fixed(this, Fixed::Owned(Members::of(set_members(other))))
        }

        #[unsafe(method_id(initWithSet:copyItems:))]
        fn init_with_set_copy(this: Allocated<Self>, other: &NSSet, copy: bool) -> Retained<Self> {
            init_fixed(this, Fixed::Owned(Members::of(copied(set_members(other), copy))))
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().len()
        }

        /// Neither retained nor autoreleased: the set keeps it alive.
        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut AnyObject {
            let items = &self.ivars().items;
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => index_beyond(FIXED, "objectAtIndex:", index, items.len(), NOUN),
            }
        }

        #[unsafe(method(indexOfObject:))]
        fn index_of_object(&self, object: Option<&AnyObject>) -> NSUInteger {
            object.and_then(|o| self.ivars().position(o)).unwrap_or(NSNotFound as NSUInteger)
        }

        /// A subclass answers through its own `-objectAtIndex:`.
        #[unsafe(method(objectAtIndexedSubscript:))]
        fn object_at_indexed_subscript(&self, index: NSUInteger) -> *mut AnyObject {
            if !matches!(kind(self), Kind::Fixed(_) | Kind::Mutable(_)) {
                // SAFETY: -objectAtIndex: takes an index and returns an
                // object the set keeps alive, or fails.
                return unsafe { msg_send![self, objectAtIndex: index] };
            }
            match element_at(self, index) {
                Some(obj) => obj,
                None => index_beyond(name(self), "objectAtIndex:", index, count_of(self), NOUN),
            }
        }

        #[unsafe(method(firstObject))]
        fn first_object(&self) -> *mut AnyObject {
            element_at(self, 0).unwrap_or(ptr::null_mut())
        }

        #[unsafe(method(lastObject))]
        fn last_object(&self) -> *mut AnyObject {
            count_of(self).checked_sub(1).and_then(|last| element_at(self, last)).unwrap_or(ptr::null_mut())
        }

        #[unsafe(method(containsObject:))]
        fn contains_object(&self, object: Option<&AnyObject>) -> bool {
            object.is_some_and(|o| index_of(self, o).is_some())
        }

        #[unsafe(method(getObjects:range:))]
        fn get_objects_range(&self, objects: *mut *mut AnyObject, range: NSRange) {
            let held = members(self);
            check_range(self, "getObjects:range:", range, held.len());
            for (i, obj) in held.items[range.location..range.location + range.length].iter().enumerate() {
                // SAFETY: the caller passes room for `range.length` objects.
                unsafe { *objects.add(i) = Retained::as_ptr(obj).cast_mut() };
            }
        }

        #[unsafe(method_id(objectsAtIndexes:))]
        fn objects_at_indexes(&self, indexes: &NSIndexSet) -> Retained<NSArray> {
            let held = members(self);
            let spans = checked_spans(FIXED, "objectsAtIndexes:", indexes, held.len(), Past::InIndexSet);
            let picked = index_set::walk(&spans, false).map(|i| held.items[i].clone()).collect();
            drop(held);
            array::make(picked)
        }

        #[unsafe(method(isEqualToOrderedSet:))]
        fn is_equal_to_ordered_set(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| ordered_equal(self, other))
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| is_ordered_set(other) && ordered_equal(self, other))
        }

        /// The count, as in Foundation.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            count_of(self)
        }

        #[unsafe(method(intersectsOrderedSet:))]
        fn intersects_ordered_set(&self, other: &NSOrderedSet) -> bool {
            intersects(self, other)
        }

        #[unsafe(method(intersectsSet:))]
        fn intersects_set(&self, other: &NSSet) -> bool {
            intersects(self, other)
        }

        #[unsafe(method(isSubsetOfOrderedSet:))]
        fn is_subset_of_ordered_set(&self, other: &NSOrderedSet) -> bool {
            is_subset(self, other)
        }

        #[unsafe(method(isSubsetOfSet:))]
        fn is_subset_of_set(&self, other: &NSSet) -> bool {
            is_subset(self, other)
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            enumerator::make(Source::Ordered(util::upcast(self.retain()), count_of(self)))
        }

        #[unsafe(method_id(reverseObjectEnumerator))]
        fn reverse_object_enumerator(&self) -> Retained<NSEnumerator> {
            enumerator::make(Source::ReverseOrdered(util::upcast(self.retain()), count_of(self)))
        }

        #[unsafe(method_id(reversedOrderedSet))]
        fn reversed_ordered_set(&self) -> Retained<NSOrderedSet> {
            let held = members(self);
            let (items, hashes) = (held.items.iter().rev().cloned().collect(), held.hashes.iter().rev().copied().collect());
            drop(held);
            make(Members::from_parts(items, hashes))
        }

        #[unsafe(method_id(array))]
        fn array(&self) -> Retained<NSArray> {
            array_proxy(self)
        }

        #[unsafe(method_id(set))]
        fn set(&self) -> Retained<NSSet> {
            set_proxy(self)
        }

        #[unsafe(method(enumerateObjectsUsingBlock:))]
        fn enumerate_objects(&self, block: &ElementBlock) {
            enumerate(self, &everything(self), NSEnumerationOptions(0), block);
        }

        /// Concurrent enumeration runs on the calling thread, which the
        /// option permits.
        #[unsafe(method(enumerateObjectsWithOptions:usingBlock:))]
        fn enumerate_objects_with_options(&self, options: NSEnumerationOptions, block: &ElementBlock) {
            enumerate(self, &everything(self), options, block);
        }

        #[unsafe(method(enumerateObjectsAtIndexes:options:usingBlock:))]
        fn enumerate_objects_at_indexes(&self, indexes: &NSIndexSet, options: NSEnumerationOptions, block: &ElementBlock) {
            let method = "enumerateObjectsAtIndexes:options:usingBlock:";
            let spans = checked_spans(FIXED, method, indexes, count_of(self), Past::Index);
            enumerate(self, &spans, options, block);
        }

        #[unsafe(method(indexOfObjectPassingTest:))]
        fn index_of_object_passing_test(&self, test: &ElementTest) -> NSUInteger {
            first_passing(self, &everything(self), NSEnumerationOptions(0), test)
        }

        #[unsafe(method(indexOfObjectWithOptions:passingTest:))]
        fn index_of_object_with_options(&self, options: NSEnumerationOptions, test: &ElementTest) -> NSUInteger {
            first_passing(self, &everything(self), options, test)
        }

        #[unsafe(method(indexOfObjectAtIndexes:options:passingTest:))]
        fn index_of_object_at_indexes(&self, indexes: &NSIndexSet, options: NSEnumerationOptions, test: &ElementTest) -> NSUInteger {
            let method = "indexOfObjectAtIndexes:options:passingTest:";
            let spans = checked_spans(FIXED, method, indexes, count_of(self), Past::Index);
            first_passing(self, &spans, options, test)
        }

        #[unsafe(method_id(indexesOfObjectsPassingTest:))]
        fn indexes_of_objects_passing_test(&self, test: &ElementTest) -> Retained<NSIndexSet> {
            all_passing(self, &everything(self), NSEnumerationOptions(0), test)
        }

        #[unsafe(method_id(indexesOfObjectsWithOptions:passingTest:))]
        fn indexes_of_objects_with_options(&self, options: NSEnumerationOptions, test: &ElementTest) -> Retained<NSIndexSet> {
            all_passing(self, &everything(self), options, test)
        }

        #[unsafe(method_id(indexesOfObjectsAtIndexes:options:passingTest:))]
        fn indexes_of_objects_at_indexes(
            &self,
            indexes: &NSIndexSet,
            options: NSEnumerationOptions,
            test: &ElementTest,
        ) -> Retained<NSIndexSet> {
            let method = "indexesOfObjectsAtIndexes:options:passingTest:";
            let spans = checked_spans(FIXED, method, indexes, count_of(self), Past::Index);
            all_passing(self, &spans, options, test)
        }

        #[unsafe(method(indexOfObject:inSortedRange:options:usingComparator:))]
        fn index_of_object_in_sorted_range(
            &self,
            object: &AnyObject,
            range: NSRange,
            options: NSBinarySearchingOptions,
            comparator: NSComparator,
        ) -> NSUInteger {
            let held = members(self);
            check_range(self, "indexOfObject:inSortedRange:options:usingComparator:", range, held.len());
            array::binary_search(&held.items, object, range, options, comparator)
        }

        #[unsafe(method_id(sortedArrayUsingComparator:))]
        fn sorted_array_using_comparator(&self, comparator: NSComparator) -> Retained<NSArray> {
            sorted_array(self, array::block_order(comparator))
        }

        /// Sorts are always stable, which both options allow.
        #[unsafe(method_id(sortedArrayWithOptions:usingComparator:))]
        fn sorted_array_with_options(&self, _options: NSSortOptions, comparator: NSComparator) -> Retained<NSArray> {
            sorted_array(self, array::block_order(comparator))
        }

        #[unsafe(method_id(sortedArrayUsingDescriptors:))]
        fn sorted_array_using_descriptors(&self, descriptors: &NSArray) -> Retained<NSArray> {
            let held = members(self);
            let sorted = sort_descriptor::sorted(descriptors, &held.items);
            drop(held);
            sorted
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSOrderedSet> {
            if is_exactly(self, &NSORDEREDSET) {
                // Immutable: a copy is the same object.
                // SAFETY: NSOrderedSetImpl is the class registered as
                // NSOrderedSet.
                unsafe { Retained::cast_unchecked(self.retain()) }
            } else {
                make_from(fixed_copy(self))
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableOrderedSet> {
            make_mutable(growable_copy(self))
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            describe_at(self, 0)
        }

        #[unsafe(method_id(descriptionWithLocale:))]
        fn description_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            describe_at(self, 0)
        }

        #[unsafe(method_id(descriptionWithLocale:indent:))]
        fn description_with_locale_indent(&self, _locale: Option<&AnyObject>, level: NSUInteger) -> Retained<NSString> {
            describe_at(self, level)
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            // SAFETY: the caller passes a valid state and buffer.
            unsafe { fast_enumerate(self, state, buffer, len) }
        }
    }

    unsafe impl NSObjectProtocol for NSOrderedSetImpl {}
);

/// The members of any ordered set, retained, in order.
fn gather_any(obj: &AnyObject) -> Items {
    members(obj).items.clone()
}

/// A mutable ordered set subclass defined outside Sidestep, changed through
/// the primitives Foundation asks such a subclass to implement.
struct Subclass<'a>(&'a AnyObject);

// SAFETY (every method): NSMutableOrderedSet's primitives, with the argument
// types Foundation declares; indexes are checked by the caller.
impl Subclass<'_> {
    fn insert(&self, object: &AnyObject, index: usize) {
        unsafe { msg_send![self.0, insertObject: object, atIndex: index] }
    }

    fn remove(&self, index: usize) {
        unsafe { msg_send![self.0, removeObjectAtIndex: index] }
    }

    fn replace(&self, index: usize, object: &AnyObject) {
        unsafe { msg_send![self.0, replaceObjectAtIndex: index, withObject: object] }
    }
}

impl NSMutableOrderedSetImpl {
    /// This object, for failure messages.
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    /// A subclass defined outside Sidestep, which is changed through its
    /// primitives.
    fn subclass(&self) -> Option<Subclass<'_>> {
        (!is_exactly(self, &NSMUTABLEORDEREDSET)).then(|| Subclass(self))
    }

    fn changed(&self) {
        self.ivars().mutations.bump();
    }

    /// The members, owned by this set alone, for changing.
    ///
    /// # Safety
    /// As for `Guarded::write`: no other code runs while the result is used.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    unsafe fn storage(&self) -> &mut Members {
        loop {
            // SAFETY: guaranteed by the caller; the reference is returned or
            // not used.
            if let Some(members) = unsafe { self.ivars().members.write_alone(MUTABLE, self.obj()) } {
                return members;
            }
            self.ivars().members.unshare(MUTABLE, self.obj());
            // The vector moved, which fast enumeration in progress must
            // notice even if the change itself then fails.
            self.changed();
        }
    }

    /// The position of a member equal to `key` in this set's own storage.
    fn find(&self, key: &AnyObject) -> Option<usize> {
        let cow = &self.ivars().members;
        // SAFETY: looking for the very object runs no other code.
        if let Some(at) = unsafe { cow.peek() }.identical(key) {
            return Some(at);
        }
        // Hashed with nothing held: -hash may run any code.
        let probe = Probe::new(key);
        // SAFETY: a search that sends no messages runs no other code.
        match unsafe { cow.peek() }.locate_quietly(&probe) {
            Some(found) => found,
            None => cow.read().locate(&probe),
        }
    }

    /// Put `object` at `index` of this set's own storage unless an equal
    /// member is there; `index` must be at most the count.
    fn put_own(&self, index: usize, object: &AnyObject) -> bool {
        let probe = Probe::new(object);
        // SAFETY: a search that sends no messages runs no other code.
        let found = match unsafe { self.ivars().members.peek() }.locate_quietly(&probe) {
            Some(found) => found,
            None => self.ivars().members.read().locate(&probe),
        };
        if found.is_some() {
            return false;
        }
        let object = object.retain();
        // SAFETY: only the storage is touched; the member was retained
        // before.
        let members = unsafe { self.storage() };
        let index = index.min(members.len());
        members.insert(index, probe.hash, object);
        self.changed();
        true
    }

    /// Insert `object` at `index` (checked) unless an equal member is
    /// there, through a subclass's primitives where it has them.
    fn put(&self, index: usize, object: &AnyObject) -> bool {
        match self.subclass() {
            Some(sub) => {
                if index_of(self, object).is_some() {
                    return false;
                }
                sub.insert(object, index);
                true
            }
            None => self.put_own(index, object),
        }
    }

    /// Append `object` unless an equal member is there.
    fn add(&self, object: &AnyObject) {
        self.put(count_of(self), object);
    }

    fn remove_own(&self, index: usize) {
        // SAFETY: only the storage is touched; the member is released after.
        let members = unsafe { self.storage() };
        if index >= members.len() {
            index_beyond(MUTABLE, "removeObjectAtIndex:", index, members.len(), NOUN);
        }
        let removed = members.remove(index);
        self.changed();
        drop(removed);
    }

    /// Remove the member at `index` (checked).
    fn take(&self, index: usize) {
        match self.subclass() {
            Some(sub) => sub.remove(index),
            None => self.remove_own(index),
        }
    }

    /// Put `object` in place of the member at `index` (checked), unless
    /// another member equals it, which Foundation leaves as it is.
    fn replace_own(&self, index: usize, object: &AnyObject, method: &str) {
        let count = count_of(self);
        if index >= count {
            index_beyond(MUTABLE, method, index, count, NOUN);
        }
        let probe = Probe::new(object);
        // SAFETY: a search that sends no messages runs no other code.
        let found = match unsafe { self.ivars().members.peek() }.locate_quietly(&probe) {
            Some(found) => found,
            None => self.ivars().members.read().locate(&probe),
        };
        if found.is_some_and(|at| at != index) {
            return;
        }
        let object = object.retain();
        // SAFETY: only the storage is touched; the old member is released
        // after.
        let old = unsafe { self.storage() }.replace(index, probe.hash, object);
        self.changed();
        drop(old);
    }

    fn replace(&self, index: usize, object: &AnyObject, method: &str) {
        match self.subclass() {
            Some(sub) => {
                let count = count_of(self);
                if index >= count {
                    index_beyond(MUTABLE, method, index, count, NOUN);
                }
                if index_of(self, object).is_some_and(|at| at != index) {
                    return;
                }
                sub.replace(index, object);
            }
            None => self.replace_own(index, object, method),
        }
    }

    /// Keep the members `keep` flags, in one pass.
    fn keep(&self, keep: &[bool]) {
        if !keep.contains(&false) {
            return;
        }
        if let Some(sub) = self.subclass() {
            for (i, _) in keep.iter().enumerate().rev().filter(|(_, k)| !**k) {
                sub.remove(i);
            }
            return;
        }
        // SAFETY: only the storage is touched; the members are released
        // after.
        let removed = unsafe { self.storage() }.keep(keep);
        self.changed();
        drop(removed);
    }

    /// For each member, `test` of it, with the set read meanwhile.
    fn mark(&self, mut test: impl FnMut(usize, &AnyObject) -> bool) -> Vec<bool> {
        match self.subclass() {
            Some(_) => gather(self).iter().enumerate().map(|(i, e)| test(i, e)).collect(),
            None => self.ivars().members.read().items.iter().enumerate().map(|(i, e)| test(i, e)).collect(),
        }
    }

    /// Remove every member.
    fn clear(&self) {
        if let Some(sub) = self.subclass() {
            for i in (0..count_of(self)).rev() {
                sub.remove(i);
            }
            return;
        }
        let old = self.ivars().members.replace(counted(Members::default()), MUTABLE, self.obj());
        self.changed();
        drop(old);
    }

    /// Insert `objects` at `index` and on, in order, skipping any equal to
    /// a member (which leaves no gap). Fails if `index` is past the end.
    fn insert_run(&self, index: usize, objects: &[Retained<AnyObject>], method: &str) {
        let count = count_of(self);
        if index > count {
            index_beyond(MUTABLE, method, index, count, NOUN);
        }
        let mut at = index;
        for object in objects {
            if self.put(at, object) {
                at += 1;
            }
        }
    }

    /// Insert `objects` at `indexes`, each run of indexes taking the next
    /// objects, as `-insertObjects:atIndexes:` does.
    fn insert_at_indexes(&self, objects: Items, indexes: &NSIndexSet, method: &str) {
        let wanted = index_set::count(indexes);
        let spans = checked_spans(MUTABLE, method, indexes, count_of(self) + objects.len(), Past::InIndexSet);
        if objects.len() != wanted {
            panic!(
                "*** -[{MUTABLE} {method}]: count of array ({}) differs from count of index set ({wanted})",
                objects.len()
            );
        }
        let mut rest = &objects[..];
        for (start, end) in spans {
            let (run, after) = rest.split_at(end - start);
            self.insert_run(start, run, method);
            rest = after;
        }
    }

    /// Put the members within `range` in the order `order` gives, stably.
    fn sort_range(
        &self,
        range: std::ops::Range<usize>,
        mut order: impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering,
    ) {
        self.arrange(range, |items| array::sorted_positions(items, &mut order));
    }

    /// Put the members within `range` in the order `arrange` gives: it
    /// returns their positions, in their new order.
    fn arrange(&self, range: std::ops::Range<usize>, arrange: impl FnOnce(&[Retained<AnyObject>]) -> Vec<usize>) {
        if let Some(sub) = self.subclass() {
            let items = gather(self);
            let positions = arrange(&items[range.clone()]);
            // Clear the range, then put the members back in order: a
            // replacement by a member elsewhere in the set is refused.
            for i in range.clone().rev() {
                sub.remove(i);
            }
            for (k, at) in positions.into_iter().enumerate() {
                sub.insert(&items[range.start + at], range.start + k);
            }
            return;
        }
        // The set is read while comparisons run, so they can't change it.
        let positions = {
            let held = self.ivars().members.read();
            arrange(&held.items[range.clone()])
        };
        // SAFETY: the members are only moved among their places.
        let members = unsafe { self.storage() };
        if members.len() >= range.end {
            let whole: Vec<usize> = (0..range.start)
                .chain(positions.into_iter().map(|at| range.start + at))
                .chain(range.end..members.len())
                .collect();
            members.permute(&whole);
        }
        self.changed();
    }

    fn sort_all(&self, order: impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering) {
        self.sort_range(0..count_of(self), order);
    }
}

define_class!(
    #[unsafe(super(NSOrderedSet, NSObject))]
    #[name = "NSMutableOrderedSet"]
    #[ivars = MutableIvars]
    pub(crate) struct NSMutableOrderedSetImpl;

    impl NSMutableOrderedSetImpl {
        #[unsafe(method_id(orderedSet))]
        fn ordered_set() -> Retained<NSMutableOrderedSet> {
            make_mutable(Cow::default())
        }

        #[unsafe(method_id(orderedSetWithCapacity:))]
        fn ordered_set_with_capacity(_capacity: NSUInteger) -> Retained<NSMutableOrderedSet> {
            make_mutable(Cow::default())
        }

        #[unsafe(method_id(orderedSetWithObject:))]
        fn ordered_set_with_object(object: &AnyObject) -> Retained<NSMutableOrderedSet> {
            make_mutable(Cow::fresh(Members::of([object.retain()])))
        }

        #[unsafe(method_id(orderedSetWithObjects:count:))]
        fn ordered_set_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSMutableOrderedSet> {
            // SAFETY: the caller passes `count` objects.
            let items = unsafe { from_c_array(MUTABLE, "initWithObjects:count:", objects, count) };
            make_mutable(Cow::fresh(Members::of(items)))
        }

        #[unsafe(method_id(orderedSetWithOrderedSet:))]
        fn ordered_set_with_ordered_set(other: &NSOrderedSet) -> Retained<NSMutableOrderedSet> {
            make_mutable(growable_copy(other))
        }

        #[unsafe(method_id(orderedSetWithOrderedSet:range:copyItems:))]
        fn ordered_set_with_ordered_set_range(other: &NSOrderedSet, range: NSRange, copy: bool) -> Retained<NSMutableOrderedSet> {
            let items = ordered_range(MUTABLE, "initWithOrderedSet:range:copyItems:", other, range);
            make_mutable(Cow::fresh(Members::of(copied(items, copy))))
        }

        #[unsafe(method_id(orderedSetWithArray:))]
        fn ordered_set_with_array(array: &NSArray) -> Retained<NSMutableOrderedSet> {
            make_mutable(Cow::fresh(Members::of(array.to_vec())))
        }

        #[unsafe(method_id(orderedSetWithArray:range:copyItems:))]
        fn ordered_set_with_array_range(array: &NSArray, range: NSRange, copy: bool) -> Retained<NSMutableOrderedSet> {
            let items = array_range(MUTABLE, "initWithArray:range:copyItems:", array, range);
            make_mutable(Cow::fresh(Members::of(copied(items, copy))))
        }

        #[unsafe(method_id(orderedSetWithSet:))]
        fn ordered_set_with_set(other: &NSSet) -> Retained<NSMutableOrderedSet> {
            make_mutable(Cow::fresh(Members::of(set_members(other))))
        }

        #[unsafe(method_id(orderedSetWithSet:copyItems:))]
        fn ordered_set_with_set_copy(other: &NSSet, copy: bool) -> Retained<NSMutableOrderedSet> {
            make_mutable(Cow::fresh(Members::of(copied(set_members(other), copy))))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_mutable(this, Cow::default())
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, _capacity: NSUInteger) -> Retained<Self> {
            init_mutable(this, Cow::default())
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            let items = unsafe { from_c_array(MUTABLE, "initWithObjects:count:", objects, count) };
            init_mutable(this, Cow::fresh(Members::of(items)))
        }

        #[unsafe(method_id(initWithObject:))]
        fn init_with_object(this: Allocated<Self>, object: &AnyObject) -> Retained<Self> {
            init_mutable(this, Cow::fresh(Members::of([object.retain()])))
        }

        #[unsafe(method_id(initWithOrderedSet:))]
        fn init_with_ordered_set(this: Allocated<Self>, other: &NSOrderedSet) -> Retained<Self> {
            init_mutable(this, growable_copy(other))
        }

        #[unsafe(method_id(initWithOrderedSet:copyItems:))]
        fn init_with_ordered_set_copy(this: Allocated<Self>, other: &NSOrderedSet, copy: bool) -> Retained<Self> {
            let members = if copy { Cow::fresh(Members::of(copied(gather_any(other), true))) } else { growable_copy(other) };
            init_mutable(this, members)
        }

        #[unsafe(method_id(initWithOrderedSet:range:copyItems:))]
        fn init_with_ordered_set_range(this: Allocated<Self>, other: &NSOrderedSet, range: NSRange, copy: bool) -> Retained<Self> {
            let items = ordered_range(MUTABLE, "initWithOrderedSet:range:copyItems:", other, range);
            init_mutable(this, Cow::fresh(Members::of(copied(items, copy))))
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init_mutable(this, Cow::fresh(Members::of(array.to_vec())))
        }

        #[unsafe(method_id(initWithArray:copyItems:))]
        fn init_with_array_copy(this: Allocated<Self>, array: &NSArray, copy: bool) -> Retained<Self> {
            init_mutable(this, Cow::fresh(Members::of(copied(array.to_vec(), copy))))
        }

        #[unsafe(method_id(initWithArray:range:copyItems:))]
        fn init_with_array_range(this: Allocated<Self>, array: &NSArray, range: NSRange, copy: bool) -> Retained<Self> {
            let items = array_range(MUTABLE, "initWithArray:range:copyItems:", array, range);
            init_mutable(this, Cow::fresh(Members::of(copied(items, copy))))
        }

        #[unsafe(method_id(initWithSet:))]
        fn init_with_set(this: Allocated<Self>, other: &NSSet) -> Retained<Self> {
            init_mutable(this, Cow::fresh(Members::of(set_members(other))))
        }

        #[unsafe(method_id(initWithSet:copyItems:))]
        fn init_with_set_copy(this: Allocated<Self>, other: &NSSet, copy: bool) -> Retained<Self> {
            init_mutable(this, Cow::fresh(Members::of(copied(set_members(other), copy))))
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            // SAFETY: reading the length runs no other code.
            unsafe { self.ivars().members.peek() }.len()
        }

        /// Neither retained nor autoreleased: the set keeps it alive.
        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut AnyObject {
            // SAFETY: reading one pointer runs no other code.
            let items = &unsafe { self.ivars().members.peek() }.items;
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => index_beyond(MUTABLE, "objectAtIndex:", index, items.len(), NOUN),
            }
        }

        #[unsafe(method(indexOfObject:))]
        fn index_of_object(&self, object: Option<&AnyObject>) -> NSUInteger {
            object.and_then(|o| self.find(o)).unwrap_or(NSNotFound as NSUInteger)
        }

        #[unsafe(method(insertObject:atIndex:))]
        fn insert_object(&self, object: Option<&AnyObject>, index: NSUInteger) {
            let Some(object) = object else { nil_argument(MUTABLE, "insertObject:atIndex:", "object") };
            let count = count_of(self);
            if index > count {
                index_beyond(MUTABLE, "insertObject:atIndex:", index, count, NOUN);
            }
            self.put_own(index, object);
        }

        #[unsafe(method(removeObjectAtIndex:))]
        fn remove_object_at_index(&self, index: NSUInteger) {
            self.remove_own(index);
        }

        #[unsafe(method(replaceObjectAtIndex:withObject:))]
        fn replace_object_at_index(&self, index: NSUInteger, object: Option<&AnyObject>) {
            let method = "replaceObjectAtIndex:withObject:";
            let Some(object) = object else { nil_argument(MUTABLE, method, "object") };
            self.replace_own(index, object, method);
        }

        #[unsafe(method(addObject:))]
        fn add_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { nil_argument(MUTABLE, "addObject:", "object") };
            self.add(object);
        }

        #[unsafe(method(addObjects:count:))]
        fn add_objects_count(&self, objects: *const *mut AnyObject, count: NSUInteger) {
            // SAFETY: the caller passes `count` objects.
            let items = unsafe { from_c_array(MUTABLE, "addObjects:count:", objects, count) };
            for item in &items {
                self.add(item);
            }
        }

        #[unsafe(method(addObjectsFromArray:))]
        fn add_objects_from_array(&self, array: &NSArray) {
            for item in array.to_vec() {
                self.add(&item);
            }
        }

        #[unsafe(method(exchangeObjectAtIndex:withObjectAtIndex:))]
        fn exchange_objects(&self, a: NSUInteger, b: NSUInteger) {
            let count = count_of(self);
            if let Some(bad) = [a, b].into_iter().find(|&i| i >= count) {
                index_beyond(MUTABLE, "exchangeObjectAtIndex:withObjectAtIndex:", bad, count, NOUN);
            }
            if let Some(sub) = self.subclass() {
                if a != b {
                    let items = gather(self);
                    let (low, high) = (a.min(b), a.max(b));
                    sub.remove(high);
                    sub.remove(low);
                    sub.insert(&items[high], low);
                    sub.insert(&items[low], high);
                }
                return;
            }
            // SAFETY: only the storage is touched.
            unsafe { self.storage() }.swap(a, b);
            self.changed();
        }

        #[unsafe(method(moveObjectsAtIndexes:toIndex:))]
        fn move_objects(&self, indexes: &NSIndexSet, index: NSUInteger) {
            let method = "moveObjectsAtIndexes:toIndex:";
            let count = count_of(self);
            let spans = checked_spans(MUTABLE, method, indexes, count, Past::InIndexSet);
            let moving = index_set::count(indexes);
            if index > count - moving {
                index_beyond(MUTABLE, method, index, count - moving, NOUN);
            }
            let items = gather_any(self);
            let moved: Items = index_set::walk(&spans, false).map(|i| items[i].clone()).collect();
            let mut keep = vec![true; count];
            for (s, e) in spans {
                keep[s..e].fill(false);
            }
            self.keep(&keep);
            self.insert_run(index, &moved, method);
        }

        #[unsafe(method(insertObjects:atIndexes:))]
        fn insert_objects_at_indexes(&self, objects: &NSArray, indexes: &NSIndexSet) {
            self.insert_at_indexes(objects.to_vec(), indexes, "insertObjects:atIndexes:");
        }

        #[unsafe(method(setObject:atIndex:))]
        fn set_object_at_index(&self, object: Option<&AnyObject>, index: NSUInteger) {
            let method = "setObject:atIndex:";
            let Some(object) = object else { nil_argument(MUTABLE, method, "object") };
            if index == count_of(self) { self.add(object) } else { self.replace(index, object, method) }
        }

        #[unsafe(method(setObject:atIndexedSubscript:))]
        fn set_object_at_indexed_subscript(&self, object: Option<&AnyObject>, index: NSUInteger) {
            let method = "setObject:atIndexedSubscript:";
            let Some(object) = object else { nil_argument(MUTABLE, method, "object") };
            if index == count_of(self) { self.add(object) } else { self.replace(index, object, method) }
        }

        #[unsafe(method(replaceObjectsInRange:withObjects:count:))]
        fn replace_objects_in_range(&self, range: NSRange, objects: *const *mut AnyObject, count: NSUInteger) {
            let method = "replaceObjectsInRange:withObjects:count:";
            check_range(self, method, range, count_of(self));
            // SAFETY: the caller passes `count` objects.
            let items = unsafe { from_c_array(MUTABLE, method, objects, count) };
            let mut keep = vec![true; count_of(self)];
            keep[range.location..range.location + range.length].fill(false);
            self.keep(&keep);
            self.insert_run(range.location, &items, method);
        }

        #[unsafe(method(replaceObjectsAtIndexes:withObjects:))]
        fn replace_objects_at_indexes(&self, indexes: &NSIndexSet, objects: &NSArray) {
            let method = "replaceObjectsAtIndexes:withObjects:";
            let objects = objects.to_vec();
            let count = count_of(self);
            checked_spans(MUTABLE, method, indexes, count + objects.len(), Past::InIndexSet);
            let wanted = index_set::count(indexes);
            if objects.len() != wanted {
                panic!(
                    "*** -[{MUTABLE} {method}]: count of array ({}) differs from count of index set ({wanted})",
                    objects.len()
                );
            }
            // Foundation removes the members there, then inserts the new
            // objects as -insertObjects:atIndexes: does.
            let spans = checked_spans(MUTABLE, method, indexes, count, Past::InIndexSet);
            let mut keep = vec![true; count];
            for (s, e) in spans {
                keep[s..e].fill(false);
            }
            self.keep(&keep);
            self.insert_at_indexes(objects, indexes, method);
        }

        #[unsafe(method(removeObjectsInRange:))]
        fn remove_objects_in_range(&self, range: NSRange) {
            let count = count_of(self);
            check_range(self, "removeObjectsInRange:", range, count);
            let mut keep = vec![true; count];
            keep[range.location..range.location + range.length].fill(false);
            self.keep(&keep);
        }

        #[unsafe(method(removeObjectsAtIndexes:))]
        fn remove_objects_at_indexes(&self, indexes: &NSIndexSet) {
            let count = count_of(self);
            let spans = checked_spans(MUTABLE, "removeObjectsAtIndexes:", indexes, count, Past::InIndexSet);
            let mut keep = vec![true; count];
            for (s, e) in spans {
                keep[s..e].fill(false);
            }
            self.keep(&keep);
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.clear();
        }

        #[unsafe(method(removeObject:))]
        fn remove_object(&self, object: Option<&AnyObject>) {
            if let Some(at) = object.and_then(|o| index_of(self, o)) {
                self.take(at);
            }
        }

        #[unsafe(method(removeObjectsInArray:))]
        fn remove_objects_in_array(&self, array: &NSArray) {
            // Gathered first: the array may be this set's proxy.
            let others = array.to_vec();
            let doomed = Members::of(others);
            let keep = self.mark(|_, e| doomed.position(e).is_none());
            self.keep(&keep);
        }

        #[unsafe(method(intersectOrderedSet:))]
        fn intersect_ordered_set(&self, other: &NSOrderedSet) {
            let others = Members::of(gather_any(other));
            let keep = self.mark(|_, e| others.position(e).is_some());
            self.keep(&keep);
        }

        #[unsafe(method(minusOrderedSet:))]
        fn minus_ordered_set(&self, other: &NSOrderedSet) {
            let others = Members::of(gather_any(other));
            let keep = self.mark(|_, e| others.position(e).is_none());
            self.keep(&keep);
        }

        #[unsafe(method(unionOrderedSet:))]
        fn union_ordered_set(&self, other: &NSOrderedSet) {
            // Gathered first: `other` may be this set.
            for item in gather_any(other) {
                self.add(&item);
            }
        }

        #[unsafe(method(intersectSet:))]
        fn intersect_set(&self, other: &NSSet) {
            let others = Members::of(set_members(other));
            let keep = self.mark(|_, e| others.position(e).is_some());
            self.keep(&keep);
        }

        #[unsafe(method(minusSet:))]
        fn minus_set(&self, other: &NSSet) {
            let others = Members::of(set_members(other));
            let keep = self.mark(|_, e| others.position(e).is_none());
            self.keep(&keep);
        }

        #[unsafe(method(unionSet:))]
        fn union_set(&self, other: &NSSet) {
            for item in set_members(other) {
                self.add(&item);
            }
        }

        #[unsafe(method(sortUsingComparator:))]
        fn sort_using_comparator(&self, comparator: NSComparator) {
            self.sort_all(array::block_order(comparator));
        }

        /// Sorts are always stable, which both options allow.
        #[unsafe(method(sortWithOptions:usingComparator:))]
        fn sort_with_options(&self, _options: NSSortOptions, comparator: NSComparator) {
            self.sort_all(array::block_order(comparator));
        }

        #[unsafe(method(sortRange:options:usingComparator:))]
        fn sort_range_with_options(&self, range: NSRange, _options: NSSortOptions, comparator: NSComparator) {
            check_range(self, "sortRange:options:usingComparator:", range, count_of(self));
            self.sort_range(range.location..range.location + range.length, array::block_order(comparator));
        }

        #[unsafe(method(sortUsingDescriptors:))]
        fn sort_using_descriptors(&self, descriptors: &NSArray) {
            self.arrange(0..count_of(self), |items| sort_descriptor::positions(descriptors, items));
        }
    }

    unsafe impl NSObjectProtocol for NSMutableOrderedSetImpl {}
);

define_class!(
    /// `-[NSOrderedSet array]`: the ordered set's members as they are when
    /// asked.
    #[unsafe(super(NSArray, NSObject))]
    #[name = "_SidestepOrderedSetArray"]
    #[ivars = Retained<AnyObject>]
    pub(crate) struct ArrayProxy;

    impl ArrayProxy {
        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            count_of(self.ivars())
        }

        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut AnyObject {
            let set = self.ivars();
            match element_at(set, index) {
                Some(obj) => obj,
                None => util::index_out_of_bounds("NSArray", "objectAtIndex:", index, count_of(set)),
            }
        }

        #[unsafe(method(indexOfObject:))]
        fn index_of_object(&self, object: Option<&AnyObject>) -> NSUInteger {
            object.and_then(|o| index_of(self.ivars(), o)).unwrap_or(NSNotFound as NSUInteger)
        }

        #[unsafe(method(containsObject:))]
        fn contains_object(&self, object: Option<&AnyObject>) -> bool {
            object.is_some_and(|o| index_of(self.ivars(), o).is_some())
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSArray> {
            array::make(gather_any(self.ivars()))
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            // SAFETY: the caller passes a valid state and buffer.
            unsafe { fast_enumerate(self.ivars(), state, buffer, len) }
        }
    }
);

define_class!(
    /// `-[NSOrderedSet set]`: the ordered set's members as they are when
    /// asked.
    #[unsafe(super(NSSet, NSObject))]
    #[name = "_SidestepOrderedSetSet"]
    #[ivars = Retained<AnyObject>]
    pub(crate) struct SetProxy;

    impl SetProxy {
        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            count_of(self.ivars())
        }

        /// Neither retained nor autoreleased: the ordered set keeps it
        /// alive.
        #[unsafe(method(member:))]
        fn member(&self, object: Option<&AnyObject>) -> *mut AnyObject {
            let set = self.ivars();
            object.and_then(|o| index_of(set, o)).and_then(|at| element_at(set, at)).unwrap_or(ptr::null_mut())
        }

        #[unsafe(method(containsObject:))]
        fn contains_object(&self, object: Option<&AnyObject>) -> bool {
            object.is_some_and(|o| index_of(self.ivars(), o).is_some())
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            let set = self.ivars();
            enumerator::make(Source::Ordered(set.clone(), count_of(set)))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSSet> {
            set::make_of(gather_any(self.ivars()))
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            // SAFETY: the caller passes a valid state and buffer.
            unsafe { fast_enumerate(self.ivars(), state, buffer, len) }
        }
    }
);
