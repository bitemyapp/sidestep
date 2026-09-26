//! `NSArray` and `NSMutableArray`.
//!
//! An immutable array keeps its elements in a boxed slice (or a buffer
//! shared with the mutable array it was copied from). A mutable one keeps
//! them in a [`Deque`], which changes at either end in constant time, in a
//! [`Cow`] cell (see `guarded.rs`), with a count of changes that fast
//! enumeration watches. Methods that read an
//! array are written once, on `NSArray`, over a slice of the elements: an
//! immutable array's own, a mutable array's while it is read, or, for a
//! subclass defined outside Sidestep, one gathered through its `-count` and
//! `-objectAtIndex:`. The primitive readers (`-count`, `-objectAtIndex:`)
//! are defined on each class directly and send no messages.
//!
//! A mutable array counts as being read while a method reading it sends
//! messages to its elements (`-isEqual:`, comparators, `-description`), so
//! a callback that mutates the array fails loudly rather than freeing
//! elements out from under the loop; reads that send no messages write
//! nothing, so threads may read an array at once, as Foundation allows.
//! Mutations never run other code while they hold the array: they retain
//! what they add before, and release what they remove after.
//! `-enumerateObjectsUsingBlock:` lets its block mutate the array, as
//! Foundation does; it retains each element for the call.
//!
//! The non-primitive mutators of a subclass defined outside Sidestep go
//! through the primitives Foundation asks such a subclass to implement
//! (`-insertObject:atIndex:`, `-removeObjectAtIndex:`, `-addObject:`,
//! `-removeLastObject` and `-replaceObjectAtIndex:withObject:`).
//!
//! Copies are copy-on-write, as in Foundation: a copy (immutable or
//! mutable) of a mutable array shares its element buffer, and whichever
//! mutable array changes next takes a private copy of the buffer first. So
//! copying a mutable array takes the same time however large it is, which
//! matters because returning `[items copy]` is idiomatic.

use std::cmp::Ordering;
use std::ffi::c_void;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSBinarySearchingOptions, NSComparator, NSEnumerationOptions, NSEnumerator, NSFastEnumerationState,
    NSIndexSet, NSInteger, NSMutableArray, NSNotFound, NSRange, NSSortOptions, NSString, NSUInteger, NSZone,
};

use crate::deque::Deque;
use crate::enumerator::{self, Mutations, Source, immutable_mutations};
use crate::guarded::{Cow, Reading, counted};
use crate::table::{SCAN, Table};
use crate::util::{self, Needle, equal, index_out_of_bounds, is_exactly, nil_argument, range_out_of_bounds};
use crate::{describe, index_set, sort_descriptor, string};

type Items = Vec<Retained<AnyObject>>;
/// A mutable array's elements.
type Buffer = Deque<Retained<AnyObject>>;

/// The block `-enumerateObjectsUsingBlock:` calls with each element, its
/// index and a flag to stop.
type ElementBlock = DynBlock<dyn Fn(NonNull<AnyObject>, NSUInteger, NonNull<Bool>)>;

/// An immutable array's elements: its own, or shared with the mutable
/// array it was copied from (and that array's other copies).
pub(crate) enum Store {
    Owned(Box<[Retained<AnyObject>]>),
    Shared(Arc<Buffer>),
}

impl Default for Store {
    fn default() -> Self {
        Store::Owned(Box::default())
    }
}

impl Deref for Store {
    type Target = [Retained<AnyObject>];

    #[inline]
    fn deref(&self) -> &Self::Target {
        match self {
            Store::Owned(items) => items,
            Store::Shared(items) => items,
        }
    }
}

#[derive(Default)]
pub(crate) struct ArrayIvars {
    items: Store,
}

#[derive(Default)]
pub(crate) struct MutableArrayIvars {
    /// Shared with copies until the next change.
    items: Cow<Buffer>,
    mutations: Mutations,
}

/// Which storage an array object has.
enum Kind<'a> {
    Fixed(&'a NSArrayImpl),
    Mutable(&'a NSMutableArrayImpl),
    /// A subclass from outside Sidestep, reached through messages.
    Foreign,
}

fn kind(obj: &AnyObject) -> Kind<'_> {
    if is_exactly(obj, &crate::NSARRAY) {
        // SAFETY: an instance of exactly NSArrayImpl.
        Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSArrayImpl>() })
    } else if is_exactly(obj, &crate::NSMUTABLEARRAY) {
        // SAFETY: an instance of exactly NSMutableArrayImpl.
        Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableArrayImpl>() })
    } else {
        Kind::Foreign
    }
}

/// An array's elements, held for reading.
enum Elements<'a> {
    Fixed(&'a [Retained<AnyObject>]),
    Mutable(Reading<'a, Arc<Buffer>>),
    Gathered(Items),
}

impl Deref for Elements<'_> {
    type Target = [Retained<AnyObject>];

    fn deref(&self) -> &Self::Target {
        match self {
            Elements::Fixed(items) => items,
            Elements::Mutable(items) => items,
            Elements::Gathered(items) => items,
        }
    }
}

fn elements(obj: &AnyObject) -> Elements<'_> {
    match kind(obj) {
        Kind::Fixed(a) => Elements::Fixed(&a.ivars().items),
        Kind::Mutable(m) => Elements::Mutable(m.ivars().items.read()),
        Kind::Foreign => Elements::Gathered(gather(obj)),
    }
}

/// The elements of an array subclass defined outside Sidestep, through its
/// primitive methods.
fn gather(obj: &AnyObject) -> Items {
    // SAFETY: NSArray's primitives: -count returns NSUInteger and
    // -objectAtIndex: an object for every index below it.
    unsafe {
        let count: NSUInteger = msg_send![obj, count];
        (0..count).map(|i| msg_send![obj, objectAtIndex: i]).collect()
    }
}

/// The element at `index` of any array, unretained (the array keeps it
/// alive), or `None` past the end.
pub(crate) fn element_at(obj: &AnyObject, index: usize) -> Option<*mut AnyObject> {
    let item = |items: &[Retained<AnyObject>]| items.get(index).map(|o| Retained::as_ptr(o).cast_mut());
    match kind(obj) {
        Kind::Fixed(a) => item(&a.ivars().items),
        // SAFETY: reading one pointer runs no other code.
        Kind::Mutable(m) => item(unsafe { m.peek() }),
        Kind::Foreign => {
            // SAFETY: as in `gather`.
            let count: NSUInteger = unsafe { msg_send![obj, count] };
            // SAFETY: as in `gather`; the array keeps the element alive.
            (index < count).then(|| unsafe { msg_send![obj, objectAtIndex: index] })
        }
    }
}

/// The count of any array.
pub(crate) fn count_of(obj: &AnyObject) -> usize {
    match kind(obj) {
        Kind::Fixed(a) => a.ivars().items.len(),
        // SAFETY: reading the length runs no other code.
        Kind::Mutable(m) => unsafe { m.peek() }.len(),
        // SAFETY: -count takes nothing and returns NSUInteger.
        Kind::Foreign => unsafe { msg_send![obj, count] },
    }
}

/// A new immutable array owning `items`.
pub(crate) fn make(items: Items) -> Retained<NSArray> {
    make_from(Store::Owned(items.into_boxed_slice()))
}

fn make_from(items: Store) -> Retained<NSArray> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSArray::<AnyObject>::alloc();
    // SAFETY: NSArray's class is NSArrayImpl, and an `Allocated` is a pointer
    // to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSArray>, Allocated<NSArrayImpl>>(this) };
    let this = init_fixed(this, items);
    // SAFETY: NSArrayImpl is the class registered as NSArray.
    unsafe { Retained::cast_unchecked(this) }
}

/// A new mutable array owning `items`.
pub(crate) fn make_mutable(items: Items) -> Retained<NSMutableArray> {
    make_mutable_from(owned(items))
}

fn make_mutable_from(items: Cow<Buffer>) -> Retained<NSMutableArray> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSMutableArray::<AnyObject>::alloc();
    // SAFETY: as in `make`, for NSMutableArrayImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSMutableArray>, Allocated<NSMutableArrayImpl>>(this) };
    let this = init_mutable(this, items);
    // SAFETY: NSMutableArrayImpl is the class registered as NSMutableArray.
    unsafe { Retained::cast_unchecked(this) }
}

fn init_fixed(this: Allocated<NSArrayImpl>, items: Store) -> Retained<NSArrayImpl> {
    let this = this.set_ivars(ArrayIvars { items });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn init_mutable(this: Allocated<NSMutableArrayImpl>, items: Cow<Buffer>) -> Retained<NSMutableArrayImpl> {
    let this = this.set_ivars(MutableArrayIvars { items, mutations: Mutations::default() });
    // SAFETY: NSArray's initializer, which leaves its own storage empty.
    unsafe { msg_send![super(this), init] }
}

/// A mutable array's storage of `items`, its own.
fn owned(items: Items) -> Cow<Buffer> {
    Cow::fresh(Deque::from_vec(items))
}

/// The elements of any array for a new array to hold: shared with it where
/// copy-on-write allows (`Ok`), else retained afresh (`Err`).
fn copy_elements(obj: &AnyObject) -> Result<Arc<Buffer>, Items> {
    match kind(obj) {
        Kind::Fixed(a) => match &a.ivars().items {
            Store::Shared(items) => Ok(items.clone()),
            Store::Owned(items) => Err(items.to_vec()),
        },
        Kind::Mutable(m) => Ok(m.share()),
        Kind::Foreign => Err(gather(obj)),
    }
}

/// An immutable copy's storage.
fn fixed_copy(obj: &AnyObject) -> Store {
    match copy_elements(obj) {
        Ok(shared) => Store::Shared(shared),
        Err(items) => Store::Owned(items.into_boxed_slice()),
    }
}

/// A mutable copy's storage.
fn growable_copy(obj: &AnyObject) -> Cow<Buffer> {
    match copy_elements(obj) {
        Ok(shared) => Cow::shared(shared),
        Err(items) => owned(items),
    }
}

/// The elements of any array as storage for a mutable array to take.
fn shared_copy(obj: &AnyObject) -> Arc<Buffer> {
    copy_elements(obj).unwrap_or_else(|items| counted(Deque::from_vec(items)))
}

/// `count` objects from a C array, retained. A nil among them fails as
/// Foundation's `-initWithObjects:count:` does.
///
/// # Safety
/// `objects` must point to `count` object pointers (or be anything when
/// `count` is 0).
unsafe fn from_c_array(receiver: &str, objects: *const *mut AnyObject, count: usize) -> Items {
    (0..count)
        .map(|i| {
            // SAFETY: guaranteed by the caller.
            let obj = unsafe { *objects.add(i) };
            match NonNull::new(obj) {
                // SAFETY: a live object from the caller.
                Some(obj) => unsafe { obj.as_ref() }.retain(),
                None => {
                    panic!("*** -[{receiver} initWithObjects:count:]: attempt to insert nil object from objects[{i}]")
                }
            }
        })
        .collect()
}

/// `items`, each copied with `-copy`.
fn copy_each(items: &[Retained<AnyObject>]) -> Items {
    items.iter().map(|o| util::copy_key(o)).collect()
}

/// The name Foundation's messages give a receiver.
fn name(obj: &AnyObject) -> &'static str {
    match kind(obj) {
        Kind::Mutable(_) => "NSMutableArray",
        _ => "NSArray",
    }
}

fn check_range(obj: &AnyObject, method: &str, range: NSRange, count: usize) {
    if range.location.checked_add(range.length).is_none_or(|end| end > count) {
        range_out_of_bounds(name(obj), method, range.location, range.length, count);
    }
}

fn index_of(items: &[Retained<AnyObject>], object: &AnyObject, range: std::ops::Range<usize>) -> NSUInteger {
    // Foundation asks the object sought, not the element.
    let needle = Needle::new(object);
    items[range.clone()].iter().position(|e| needle.matches(e)).map_or(NSNotFound as NSUInteger, |i| i + range.start)
}

/// Sort `items` with `compare`, stably. A merge sort of our own: the
/// standard library's sorts may panic on comparators that are not a total
/// order, which Objective-C comparators often aren't (NaN), and Foundation
/// just produces some order.
pub(crate) fn sort_stable<T: Copy>(items: &mut [T], compare: &mut dyn FnMut(T, T) -> Ordering) {
    if items.len() < 2 {
        return;
    }
    let mut scratch = items.to_vec();
    merge_sort(items, &mut scratch, compare);
}

fn merge_sort<T: Copy>(items: &mut [T], scratch: &mut [T], compare: &mut dyn FnMut(T, T) -> Ordering) {
    let n = items.len();
    if n <= 8 {
        // Insertion sort for short runs.
        for i in 1..n {
            let mut j = i;
            while j > 0 && compare(items[j - 1], items[j]) == Ordering::Greater {
                items.swap(j - 1, j);
                j -= 1;
            }
        }
        return;
    }
    let mid = n / 2;
    merge_sort(&mut items[..mid], &mut scratch[..mid], compare);
    merge_sort(&mut items[mid..], &mut scratch[mid..], compare);
    if compare(items[mid - 1], items[mid]) != Ordering::Greater {
        return;
    }
    scratch[..n].copy_from_slice(items);
    let (mut a, mut b) = (0, mid);
    for slot in items.iter_mut() {
        if b >= n || (a < mid && compare(scratch[a], scratch[b]) != Ordering::Greater) {
            *slot = scratch[a];
            a += 1;
        } else {
            *slot = scratch[b];
            b += 1;
        }
    }
}

/// The order an `NSComparator` block gives, read as an integer so a
/// comparator returning any value Objective-C allows is fine.
pub(crate) fn block_order(cmp: NSComparator) -> impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering {
    // SAFETY: the caller passes a valid block. NSComparisonResult is an
    // NSInteger in the block ABI, so viewing it as `isize` is the same call.
    let block = unsafe { &*cmp.cast::<DynBlock<dyn Fn(NonNull<AnyObject>, NonNull<AnyObject>) -> isize>>() };
    move |a, b| {
        // SAFETY: elements are never null.
        let (a, b) = unsafe { (NonNull::new_unchecked(a), NonNull::new_unchecked(b)) };
        block.call((a, b)).cmp(&0)
    }
}

pub(crate) type CompareFn =
    unsafe extern "C-unwind" fn(NonNull<AnyObject>, NonNull<AnyObject>, *mut c_void) -> NSInteger;

pub(crate) fn function_order(
    compare: CompareFn,
    context: *mut c_void,
) -> impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering {
    move |a, b| {
        // SAFETY: the caller passes a comparison function for the elements;
        // elements are never null.
        unsafe { compare(NonNull::new_unchecked(a), NonNull::new_unchecked(b), context) }.cmp(&0)
    }
}

/// `[a selector b]` as an ordering. Sent through the runtime's lookup
/// directly, so a selector the elements don't answer fails as Foundation's
/// does (an unrecognized selector) rather than objc2's debug check.
pub(crate) fn selector_order(selector: Sel) -> impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering {
    move |a, b| {
        // SAFETY: the runtime returns the implementation for the selector
        // (or one that fails loudly); comparison selectors take an object
        // and return NSComparisonResult, an NSInteger.
        let order: NSInteger = unsafe {
            let imp = objc2::ffi::objc_msg_lookup(a, selector).expect("the runtime always finds an implementation");
            let imp: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> NSInteger =
                std::mem::transmute(imp);
            imp(a, selector, b)
        };
        order.cmp(&0)
    }
}

pub(crate) fn pointers(items: &[Retained<AnyObject>]) -> Vec<*mut AnyObject> {
    items.iter().map(|o| Retained::as_ptr(o).cast_mut()).collect()
}

/// The positions of `items` in the order `order` puts them, stably.
pub(crate) fn sorted_positions(
    items: &[Retained<AnyObject>],
    order: &mut dyn FnMut(*mut AnyObject, *mut AnyObject) -> Ordering,
) -> Vec<usize> {
    // Each element sorts with its position, so comparing needs no lookup.
    let mut pairs: Vec<(*mut AnyObject, usize)> =
        items.iter().enumerate().map(|(i, o)| (Retained::as_ptr(o).cast_mut(), i)).collect();
    sort_stable(&mut pairs, &mut |a, b| order(a.0, b.0));
    pairs.into_iter().map(|(_, i)| i).collect()
}

/// The elements of `obj` sorted, retained, as a new array.
fn sorted(obj: &AnyObject, mut order: impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering) -> Retained<NSArray> {
    let items = elements(obj);
    let mut objects = pointers(&items);
    sort_stable(&mut objects, &mut order);
    // SAFETY: the pointers are the array's elements, alive while it is held.
    let sorted = objects.into_iter().map(|p| unsafe { Retained::retain(p) }.expect("non-null")).collect();
    drop(items);
    make(sorted)
}

/// `-indexOfObject:inSortedRange:options:usingComparator:` over sorted
/// `items`: where `object` is, or with `InsertionIndex` where it would go;
/// with `FirstEqual` or `LastEqual`, the first or last of equal elements.
pub(crate) fn binary_search(
    items: &[Retained<AnyObject>],
    object: &AnyObject,
    range: NSRange,
    options: NSBinarySearchingOptions,
    comparator: NSComparator,
) -> NSUInteger {
    let (first, last) =
        (options.contains(NSBinarySearchingOptions::FirstEqual), options.contains(NSBinarySearchingOptions::LastEqual));
    if first && last {
        panic!(
            "-[NSArray indexOfObject:inSortedRange:options:usingComparator:]: both NSBinarySearchingFirstEqual \
             and NSBinarySearchingLastEqual options cannot be specified"
        );
    }
    let mut order = block_order(comparator);
    let object = ptr::from_ref(object).cast_mut();
    let mut compare = |i: usize| order(Retained::as_ptr(&items[i]).cast_mut(), object);
    let (start, end) = (range.location, range.location + range.length);
    let lower = bound(&mut compare, start, end, false);
    let found = lower < end && compare(lower) == Ordering::Equal;
    match (options.contains(NSBinarySearchingOptions::InsertionIndex), found) {
        (true, _) if last => bound(&mut compare, lower, end, true),
        (true, _) => lower,
        (false, false) => NSNotFound as NSUInteger,
        (false, true) if last => bound(&mut compare, lower, end, true) - 1,
        (false, true) => lower,
    }
}

/// The first index in `start..end` whose element is not less than the
/// object sought, or with `upper` the first that is greater.
fn bound(compare: &mut dyn FnMut(usize) -> Ordering, start: usize, end: usize, upper: bool) -> usize {
    let (mut low, mut high) = (start, end);
    while low < high {
        let mid = low + (high - low) / 2;
        let order = compare(mid);
        if order == Ordering::Less || (upper && order == Ordering::Equal) {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    low
}

/// How a method fails for an index set reaching past the end of an array:
/// most name the index "in index set"; some don't.
#[derive(Clone, Copy)]
enum Past {
    InIndexSet,
    Index,
}

/// The ranges of `indexes`, after failing as Foundation does if they reach
/// past `count`. Only the ranges are copied, never every index, so this
/// costs nothing more for a larger set.
fn checked_spans(receiver: &str, method: &str, indexes: &NSIndexSet, count: usize, past: Past) -> Vec<(usize, usize)> {
    let spans = index_set::spans(indexes);
    if let Some(&(_, end)) = spans.last()
        && end > count
    {
        match past {
            Past::InIndexSet => util::index_set_out_of_bounds(receiver, method, end - 1, count),
            Past::Index => index_out_of_bounds(receiver, method, end - 1, count),
        }
    }
    spans
}

/// A test block for elements: element, index and a flag to stop.
type ElementTest = DynBlock<dyn Fn(NonNull<AnyObject>, NSUInteger, NonNull<Bool>) -> Bool>;

/// Call `test` on the elements at `indexes` (or all) until it stops,
/// passing each element retained; `on_pass` hears of those that pass and
/// says whether to go on.
fn test_elements(
    obj: &AnyObject,
    indexes: Option<(&NSIndexSet, &str)>,
    options: NSEnumerationOptions,
    test: &ElementTest,
    mut on_pass: impl FnMut(usize) -> bool,
) {
    let count = count_of(obj);
    let spans = match indexes {
        Some((indexes, method)) => checked_spans("NSArray", method, indexes, count, Past::Index),
        None => vec![(0, count)],
    };
    let mut stop = Bool::NO;
    for i in index_set::walk(&spans, options.contains(NSEnumerationOptions::Reverse)) {
        // SAFETY: the array keeps the element alive until retained.
        let Some(item) = element_at(obj, i).map(|p| unsafe { Retained::retain(p) }.expect("non-null")) else {
            break;
        };
        if test.call((NonNull::from(&*item), i, NonNull::from(&mut stop))).as_bool() && !on_pass(i) {
            break;
        }
        if stop.as_bool() {
            break;
        }
    }
}

fn indexes_passing(
    obj: &AnyObject,
    indexes: Option<(&NSIndexSet, &str)>,
    options: NSEnumerationOptions,
    test: &ElementTest,
) -> Retained<NSIndexSet> {
    let mut passed = Vec::new();
    test_elements(obj, indexes, options, test, |i| {
        passed.push(i);
        true
    });
    index_set::make(index_set::Ranges::of_indexes(passed))
}

fn first_passing(
    obj: &AnyObject,
    indexes: Option<(&NSIndexSet, &str)>,
    options: NSEnumerationOptions,
    test: &ElementTest,
) -> NSUInteger {
    let mut first = NSNotFound as NSUInteger;
    test_elements(obj, indexes, options, test, |i| {
        first = i;
        false
    });
    first
}

/// The elements of one of Sidestep's arrays, for a read that runs no other
/// code; `None` for a subclass.
///
/// # Safety
/// As for `Guarded::peek`.
unsafe fn quietly(obj: &AnyObject) -> Option<&[Retained<AnyObject>]> {
    match kind(obj) {
        Kind::Fixed(a) => Some(&a.ivars().items),
        // SAFETY: guaranteed by the caller.
        Kind::Mutable(m) => Some(unsafe { m.peek() }),
        Kind::Foreign => None,
    }
}

/// `-isEqualToArray:` for any two arrays: the same count, and each element
/// `-isEqual:` to the other's at its index.
fn arrays_equal(a: &AnyObject, b: &AnyObject) -> bool {
    if ptr::eq(a, b) {
        return true;
    }
    // Arrays of the very same objects are common (copies), and comparing
    // the pointers wholesale is a memcmp that sends no messages.
    // SAFETY: comparing lengths and addresses runs no other code.
    if let (Some(x), Some(y)) = unsafe { (quietly(a), quietly(b)) } {
        if x.len() != y.len() {
            return false;
        }
        if addresses(x) == addresses(y) {
            return true;
        }
    }
    let (items, others) = (elements(a), elements(b));
    items.len() == others.len() && items.iter().zip(others.iter()).all(|(x, y)| equal(x, y))
}

/// Elements as plain addresses.
fn addresses(items: &[Retained<AnyObject>]) -> &[usize] {
    // SAFETY: a `Retained` is a non-null pointer and laid out as one.
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast::<usize>(), items.len()) }
}

/// Append the description of any array at `level`.
pub(crate) fn describe(out: &mut String, obj: &AnyObject, level: usize) {
    let items = elements(obj);
    describe::list(out, "(", ")", items.iter().map(|o| &**o), level);
}

/// Call `block` with the elements at `spans`' indexes, each retained for
/// the call: the block may change the array.
fn enumerate_at(obj: &AnyObject, spans: &[(usize, usize)], reverse: bool, block: &ElementBlock) {
    let mut stop = Bool::NO;
    for i in index_set::walk(spans, reverse) {
        // SAFETY: the array keeps the element alive until retained.
        let Some(item) = element_at(obj, i).map(|p| unsafe { Retained::retain(p) }.expect("non-null")) else {
            break;
        };
        block.call((NonNull::from(&*item), i, NonNull::from(&mut stop)));
        if stop.as_bool() {
            break;
        }
    }
}

/// `-enumerateObjectsWithOptions:usingBlock:` for any array.
fn enumerate(obj: &AnyObject, reverse: bool, block: &ElementBlock) {
    if let Kind::Fixed(a) = kind(obj) {
        // Nothing can change an immutable array: no retains needed.
        let items = &a.ivars().items;
        let count = items.len();
        let mut stop = Bool::NO;
        for step in 0..count {
            let i = if reverse { count - 1 - step } else { step };
            block.call((NonNull::from(&*items[i]), i, NonNull::from(&mut stop)));
            if stop.as_bool() {
                break;
            }
        }
        return;
    }
    // The block may mutate the array: take each element afresh, retained,
    // and stop at the end of what is left.
    enumerate_at(obj, &[(0, count_of(obj))], reverse, block);
}

/// The last class `makeObjectsPerformSelector:` checked, and whether the
/// selector returns a struct there.
type Checked = Option<(*const AnyClass, bool)>;

/// Send `selector` to `obj`, with or without an argument, ignoring any
/// result, as `-makeObjectsPerformSelector:` does. `checked` remembers the
/// last class looked at, as elements of one class are the usual case.
pub(crate) fn perform(obj: &AnyObject, selector: Sel, argument: Option<Option<&AnyObject>>, checked: &mut Checked) {
    // A struct result may be returned through memory the caller provides,
    // which a call that ignores the result doesn't; such methods are not
    // for this.
    let class = obj.class();
    let returns_struct = match *checked {
        Some((last, verdict)) if ptr::eq(last, class) => verdict,
        _ => {
            let verdict = class.instance_method(selector).is_some_and(|method| {
                let result = method.return_type();
                matches!(result.to_bytes().first(), Some(b'{' | b'(' | b'['))
            });
            *checked = Some((class, verdict));
            verdict
        }
    };
    if returns_struct {
        panic!("*** -[NSArray makeObjectsPerformSelector:]: {selector} returns a struct");
    }
    let receiver = ptr::from_ref(obj).cast_mut();
    // SAFETY: the runtime returns the implementation for the selector (or
    // one that fails loudly). Selectors given to -makeObjectsPerformSelector:
    // take at most the one object argument, and a result that is not a
    // struct comes back in registers, so it may be ignored.
    unsafe {
        let Some(imp) = objc2::ffi::objc_msg_lookup(receiver, selector) else { return };
        match argument {
            None => {
                let imp: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) = std::mem::transmute(imp);
                imp(receiver, selector);
            }
            Some(argument) => {
                let imp: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *const AnyObject) = std::mem::transmute(imp);
                imp(receiver, selector, argument.map_or(ptr::null(), ptr::from_ref));
            }
        }
    }
}

/// The elements of any array, for calls that may change it: a mutable
/// array's are retained afresh, so the calls see (and change) the array
/// itself, not the loop.
fn snapshot(obj: &AnyObject) -> Elements<'_> {
    match elements(obj) {
        Elements::Mutable(items) => Elements::Gathered(items.to_vec()),
        items => items,
    }
}

/// For each element of `items`, whether it equals one of `others`, asking
/// the element of `others` (`[other isEqual:element]`), as
/// `-removeObjectsInArray:` does. Long lists go into a hash table first.
fn marked_in(items: &[Retained<AnyObject>], others: Items) -> Vec<bool> {
    if others.len() <= SCAN {
        return items.iter().map(|e| others.iter().any(|o| equal(o, e))).collect();
    }
    let table = Table::of_members(others);
    items.iter().map(|e| table.position(e).is_some()).collect()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSArray"]
    #[ivars = ArrayIvars]
    pub(crate) struct NSArrayImpl;

    impl NSArrayImpl {
        #[unsafe(method_id(array))]
        fn array() -> Retained<NSArray> {
            make(Vec::new())
        }

        #[unsafe(method_id(arrayWithObject:))]
        fn array_with_object(object: &AnyObject) -> Retained<NSArray> {
            make(vec![object.retain()])
        }

        #[unsafe(method_id(arrayWithObjects:count:))]
        fn array_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSArray> {
            // SAFETY: the caller passes `count` objects.
            make(unsafe { from_c_array("NSArray", objects, count) })
        }

        #[unsafe(method_id(arrayWithArray:))]
        fn array_with_array(array: &NSArray) -> Retained<NSArray> {
            make_from(fixed_copy(array))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_fixed(this, Store::default())
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            let items = unsafe { from_c_array("NSArray", objects, count) };
            init_fixed(this, Store::Owned(items.into_boxed_slice()))
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init_fixed(this, fixed_copy(array))
        }

        #[unsafe(method_id(initWithArray:copyItems:))]
        fn init_with_array_copy(this: Allocated<Self>, array: &NSArray, copy: bool) -> Retained<Self> {
            let store = if copy {
                Store::Owned(copy_each(&elements(array)).into_boxed_slice())
            } else {
                fixed_copy(array)
            };
            init_fixed(this, store)
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().items.len()
        }

        /// Neither retained nor autoreleased: the array keeps it alive.
        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut AnyObject {
            let items = &self.ivars().items;
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => index_out_of_bounds("NSArray", "objectAtIndex:", index, items.len()),
            }
        }

        /// A subclass answers through its own `-objectAtIndex:`.
        #[unsafe(method(objectAtIndexedSubscript:))]
        fn object_at_indexed_subscript(&self, index: NSUInteger) -> *mut AnyObject {
            if !is_exactly(self, &crate::NSARRAY) {
                // SAFETY: -objectAtIndex: takes an index and returns an
                // object the array keeps alive, or fails.
                return unsafe { msg_send![self, objectAtIndex: index] };
            }
            let items = &self.ivars().items;
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => index_out_of_bounds("NSArray", "objectAtIndexedSubscript:", index, items.len()),
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

        #[unsafe(method(getObjects:range:))]
        fn get_objects_range(&self, objects: *mut *mut AnyObject, range: NSRange) {
            let items = elements(self);
            check_range(self, "getObjects:range:", range, items.len());
            for (i, obj) in items[range.location..range.location + range.length].iter().enumerate() {
                // SAFETY: the caller passes room for `range.length` objects.
                unsafe { *objects.add(i) = Retained::as_ptr(obj).cast_mut() };
            }
        }

        #[unsafe(method(getObjects:))]
        fn get_objects(&self, objects: *mut *mut AnyObject) {
            for (i, obj) in elements(self).iter().enumerate() {
                // SAFETY: the caller passes room for every object.
                unsafe { *objects.add(i) = Retained::as_ptr(obj).cast_mut() };
            }
        }

        #[unsafe(method(containsObject:))]
        fn contains_object(&self, object: Option<&AnyObject>) -> bool {
            object.is_some_and(|object| {
                let items = elements(self);
                index_of(&items, object, 0..items.len()) != NSNotFound as NSUInteger
            })
        }

        #[unsafe(method(indexOfObject:))]
        fn index_of_object(&self, object: Option<&AnyObject>) -> NSUInteger {
            let Some(object) = object else { return NSNotFound as NSUInteger };
            let items = elements(self);
            index_of(&items, object, 0..items.len())
        }

        #[unsafe(method(indexOfObject:inRange:))]
        fn index_of_object_in_range(&self, object: Option<&AnyObject>, range: NSRange) -> NSUInteger {
            let items = elements(self);
            check_range(self, "indexOfObject:inRange:", range, items.len());
            let Some(object) = object else { return NSNotFound as NSUInteger };
            index_of(&items, object, range.location..range.location + range.length)
        }

        #[unsafe(method(indexOfObjectIdenticalTo:))]
        fn index_of_object_identical_to(&self, object: *const AnyObject) -> NSUInteger {
            let items = elements(self);
            items.iter().position(|e| ptr::eq(&**e, object)).unwrap_or(NSNotFound as NSUInteger)
        }

        #[unsafe(method(indexOfObjectIdenticalTo:inRange:))]
        fn index_of_object_identical_to_in_range(&self, object: *const AnyObject, range: NSRange) -> NSUInteger {
            let items = elements(self);
            check_range(self, "indexOfObjectIdenticalTo:inRange:", range, items.len());
            items[range.location..range.location + range.length]
                .iter()
                .position(|e| ptr::eq(&**e, object))
                .map_or(NSNotFound as NSUInteger, |i| i + range.location)
        }

        #[unsafe(method(indexOfObjectPassingTest:))]
        fn index_of_object_passing_test(&self, test: &ElementTest) -> NSUInteger {
            first_passing(self, None, NSEnumerationOptions(0), test)
        }

        #[unsafe(method(indexOfObject:inSortedRange:options:usingComparator:))]
        fn index_of_object_in_sorted_range(
            &self,
            object: &AnyObject,
            range: NSRange,
            options: NSBinarySearchingOptions,
            comparator: NSComparator,
        ) -> NSUInteger {
            let items = elements(self);
            check_range(self, "indexOfObject:inSortedRange:options:usingComparator:", range, items.len());
            binary_search(&items, object, range, options, comparator)
        }

        #[unsafe(method_id(objectsAtIndexes:))]
        fn objects_at_indexes(&self, indexes: &NSIndexSet) -> Retained<NSArray> {
            let items = elements(self);
            let spans = checked_spans("NSArray", "objectsAtIndexes:", indexes, items.len(), Past::InIndexSet);
            let picked = index_set::walk(&spans, false).map(|i| items[i].clone()).collect();
            drop(items);
            make(picked)
        }

        #[unsafe(method(enumerateObjectsAtIndexes:options:usingBlock:))]
        fn enumerate_objects_at_indexes(&self, indexes: &NSIndexSet, options: NSEnumerationOptions, block: &ElementBlock) {
            let method = "enumerateObjectsAtIndexes:options:usingBlock:";
            let spans = checked_spans("NSArray", method, indexes, count_of(self), Past::Index);
            enumerate_at(self, &spans, options.contains(NSEnumerationOptions::Reverse), block);
        }

        #[unsafe(method_id(indexesOfObjectsPassingTest:))]
        fn indexes_of_objects_passing_test(&self, test: &ElementTest) -> Retained<NSIndexSet> {
            indexes_passing(self, None, NSEnumerationOptions(0), test)
        }

        /// Concurrent testing runs on the calling thread, which the option
        /// permits.
        #[unsafe(method_id(indexesOfObjectsWithOptions:passingTest:))]
        fn indexes_of_objects_with_options(&self, options: NSEnumerationOptions, test: &ElementTest) -> Retained<NSIndexSet> {
            indexes_passing(self, None, options, test)
        }

        #[unsafe(method_id(indexesOfObjectsAtIndexes:options:passingTest:))]
        fn indexes_of_objects_at_indexes(
            &self,
            indexes: &NSIndexSet,
            options: NSEnumerationOptions,
            test: &ElementTest,
        ) -> Retained<NSIndexSet> {
            indexes_passing(self, Some((indexes, "indexesOfObjectsAtIndexes:options:passingTest:")), options, test)
        }

        #[unsafe(method(indexOfObjectWithOptions:passingTest:))]
        fn index_of_object_with_options(&self, options: NSEnumerationOptions, test: &ElementTest) -> NSUInteger {
            first_passing(self, None, options, test)
        }

        #[unsafe(method(indexOfObjectAtIndexes:options:passingTest:))]
        fn index_of_object_at_indexes(&self, indexes: &NSIndexSet, options: NSEnumerationOptions, test: &ElementTest) -> NSUInteger {
            first_passing(self, Some((indexes, "indexOfObjectAtIndexes:options:passingTest:")), options, test)
        }

        #[unsafe(method_id(firstObjectCommonWithArray:))]
        fn first_object_common_with_array(&self, other: &NSArray) -> Option<Retained<AnyObject>> {
            let (items, others) = (elements(self), elements(other));
            items.iter().find(|e| index_of(&others, e, 0..others.len()) != NSNotFound as NSUInteger).cloned()
        }

        #[unsafe(method(isEqualToArray:))]
        fn is_equal_to_array(&self, other: &NSArray) -> bool {
            arrays_equal(self, other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| {
                util::is_kind(other, <NSArray as objc2::ClassType>::class())
                    && arrays_equal(self, other)
            })
        }

        /// The count, as in Foundation: cheap, and equal arrays agree on it.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            count_of(self)
        }

        #[unsafe(method_id(arrayByAddingObject:))]
        fn array_by_adding_object(&self, object: Option<&AnyObject>) -> Retained<NSArray> {
            let Some(object) = object else { nil_argument("NSArray", "arrayByAddingObject:", "object") };
            let items = elements(self);
            let mut all = Vec::with_capacity(items.len() + 1);
            all.extend_from_slice(&items);
            all.push(object.retain());
            drop(items);
            make(all)
        }

        #[unsafe(method_id(arrayByAddingObjectsFromArray:))]
        fn array_by_adding_objects_from_array(&self, other: &NSArray) -> Retained<NSArray> {
            let (items, others) = (elements(self), elements(other));
            let mut all = Vec::with_capacity(items.len() + others.len());
            all.extend_from_slice(&items);
            all.extend_from_slice(&others);
            drop((items, others));
            make(all)
        }

        #[unsafe(method_id(subarrayWithRange:))]
        fn subarray_with_range(&self, range: NSRange) -> Retained<NSArray> {
            let items = elements(self);
            check_range(self, "subarrayWithRange:", range, items.len());
            let sub = items[range.location..range.location + range.length].to_vec();
            drop(items);
            make(sub)
        }

        #[unsafe(method_id(componentsJoinedByString:))]
        fn components_joined_by_string(&self, separator: &NSString) -> Retained<NSString> {
            let separator = separator.to_string();
            let mut out = String::new();
            for (i, obj) in elements(self).iter().enumerate() {
                if i > 0 {
                    out.push_str(&separator);
                }
                match string::fast_parts(obj) {
                    Some((text, _)) => out.push_str(text),
                    None => out.push_str(&util::description(obj)),
                }
            }
            NSString::from_str(&out)
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            enumerator::make(Source::Array(util::upcast(self.retain()), count_of(self)))
        }

        #[unsafe(method_id(reverseObjectEnumerator))]
        fn reverse_object_enumerator(&self) -> Retained<NSEnumerator> {
            enumerator::make(Source::ReverseArray(util::upcast(self.retain()), count_of(self)))
        }

        #[unsafe(method(enumerateObjectsUsingBlock:))]
        fn enumerate_objects(&self, block: &ElementBlock) {
            enumerate(self, false, block);
        }

        /// Concurrent enumeration runs on the calling thread, one element
        /// at a time, which the option permits.
        #[unsafe(method(enumerateObjectsWithOptions:usingBlock:))]
        fn enumerate_objects_with_options(
            &self,
            options: NSEnumerationOptions,
            block: &ElementBlock,
        ) {
            enumerate(self, options.contains(NSEnumerationOptions::Reverse), block);
        }

        #[unsafe(method_id(sortedArrayUsingComparator:))]
        fn sorted_array_using_comparator(&self, comparator: NSComparator) -> Retained<NSArray> {
            sorted(self, block_order(comparator))
        }

        /// Sorts are always stable, which both options allow.
        #[unsafe(method_id(sortedArrayWithOptions:usingComparator:))]
        fn sorted_array_with_options(&self, _options: NSSortOptions, comparator: NSComparator) -> Retained<NSArray> {
            sorted(self, block_order(comparator))
        }

        #[unsafe(method_id(sortedArrayUsingFunction:context:))]
        fn sorted_array_using_function(&self, compare: CompareFn, context: *mut c_void) -> Retained<NSArray> {
            sorted(self, function_order(compare, context))
        }

        #[unsafe(method_id(sortedArrayUsingSelector:))]
        fn sorted_array_using_selector(&self, selector: Sel) -> Retained<NSArray> {
            sorted(self, selector_order(selector))
        }

        #[unsafe(method_id(sortedArrayUsingDescriptors:))]
        fn sorted_array_using_descriptors(&self, descriptors: &NSArray) -> Retained<NSArray> {
            let items = elements(self);
            let sorted = sort_descriptor::sorted(descriptors, &items);
            drop(items);
            sorted
        }

        #[unsafe(method(makeObjectsPerformSelector:))]
        fn make_objects_perform_selector(&self, selector: Sel) {
            let mut checked = None;
            for obj in snapshot(self).iter() {
                perform(obj, selector, None, &mut checked);
            }
        }

        #[unsafe(method(makeObjectsPerformSelector:withObject:))]
        fn make_objects_perform_selector_with_object(&self, selector: Sel, argument: Option<&AnyObject>) {
            let mut checked = None;
            for obj in snapshot(self).iter() {
                perform(obj, selector, Some(argument), &mut checked);
            }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSArray> {
            if is_exactly(self, &crate::NSARRAY) {
                // Immutable: a copy is the same object.
                // SAFETY: NSArrayImpl is the class registered as NSArray.
                unsafe { Retained::cast_unchecked(self.retain()) }
            } else {
                make_from(fixed_copy(self))
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableArray> {
            make_mutable_from(growable_copy(self))
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let mut out = String::new();
            describe(&mut out, self, 0);
            NSString::from_str(&out)
        }

        #[unsafe(method_id(descriptionWithLocale:))]
        fn description_with_locale(&self, _locale: Option<&AnyObject>) -> Retained<NSString> {
            let mut out = String::new();
            describe(&mut out, self, 0);
            NSString::from_str(&out)
        }

        #[unsafe(method_id(descriptionWithLocale:indent:))]
        fn description_with_locale_indent(&self, _locale: Option<&AnyObject>, level: NSUInteger) -> Retained<NSString> {
            let mut out = String::new();
            describe(&mut out, self, level);
            NSString::from_str(&out)
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            match kind(self) {
                Kind::Fixed(a) => {
                    let items = &a.ivars().items;
                    // SAFETY: the storage never changes; `Retained` is a
                    // pointer.
                    unsafe { enumerator::whole(state, items.as_ptr().cast(), items.len(), immutable_mutations()) }
                }
                Kind::Mutable(m) => {
                    // SAFETY: reading the buffer's place runs no other code.
                    let items = unsafe { m.peek() };
                    // SAFETY: the storage stays put until the array next
                    // changes, which bumps the count the caller watches.
                    unsafe { enumerator::whole(state, items.as_ptr().cast(), items.len(), m.ivars().mutations.as_ptr()) }
                }
                Kind::Foreign => {
                    // SAFETY: the caller passes a valid state and buffer.
                    unsafe { enumerator::batch(state, buffer, len, immutable_mutations(), |i| element_at(self, i)) }
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for NSArrayImpl {}
);

/// A mutable array subclass defined outside Sidestep, changed through the
/// primitives Foundation asks such a subclass to implement.
struct Subclass<'a>(&'a AnyObject);

// SAFETY (every method): NSMutableArray's primitives, with the argument and
// result types Foundation declares; indexes are checked by the subclass.
impl Subclass<'_> {
    fn count(&self) -> usize {
        unsafe { msg_send![self.0, count] }
    }

    fn at(&self, index: usize) -> Retained<AnyObject> {
        unsafe { msg_send![self.0, objectAtIndex: index] }
    }

    fn all(&self) -> Items {
        gather(self.0)
    }

    fn insert(&self, object: &AnyObject, index: usize) {
        unsafe { msg_send![self.0, insertObject: object, atIndex: index] }
    }

    fn remove(&self, index: usize) {
        unsafe { msg_send![self.0, removeObjectAtIndex: index] }
    }

    fn replace(&self, index: usize, object: &AnyObject) {
        unsafe { msg_send![self.0, replaceObjectAtIndex: index, withObject: object] }
    }

    fn add(&self, object: &AnyObject) {
        unsafe { msg_send![self.0, addObject: object] }
    }

    /// Remove the elements `marked` flags, from the last.
    fn remove_marked(&self, marked: &[bool]) {
        for (i, _) in marked.iter().enumerate().rev().filter(|(_, m)| **m) {
            self.remove(i);
        }
    }

    fn remove_all(&self) {
        for i in (0..self.count()).rev() {
            self.remove(i);
        }
    }

    /// Put the elements in the order `arrange` gives: it returns their
    /// positions, in their new order.
    fn arrange(&self, arrange: impl FnOnce(&[Retained<AnyObject>]) -> Vec<usize>) {
        let items = self.all();
        for (i, at) in arrange(&items).into_iter().enumerate() {
            self.replace(i, &items[at]);
        }
    }
}

impl NSMutableArrayImpl {
    /// The elements, for a read that runs no other code.
    ///
    /// # Safety
    /// As for `Guarded::peek`.
    #[inline]
    unsafe fn peek(&self) -> &[Retained<AnyObject>] {
        // SAFETY: guaranteed by the caller.
        unsafe { self.ivars().items.peek() }
    }

    fn read(&self) -> Reading<'_, Arc<Buffer>> {
        self.ivars().items.read()
    }

    /// This object, for failure messages.
    fn obj(&self) -> *const AnyObject {
        ptr::from_ref(self).cast()
    }

    /// A subclass defined outside Sidestep, which is changed through its
    /// primitives.
    fn subclass(&self) -> Option<Subclass<'_>> {
        (!is_exactly(self, &crate::NSMUTABLEARRAY)).then(|| Subclass(self))
    }

    /// The elements, owned by this array alone, for changing. If copies
    /// share them, they are copied first (see `unshare`).
    ///
    /// # Safety
    /// As for `Guarded::write`: no other code runs while the result is used.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    unsafe fn buffer(&self) -> &mut Buffer {
        loop {
            // SAFETY: guaranteed by the caller; the reference is returned or
            // not used.
            if let Some(buffer) = unsafe { self.ivars().items.write_alone("NSMutableArray", self.obj()) } {
                return buffer;
            }
            self.ivars().items.unshare("NSMutableArray", self.obj());
            // The element buffer moved, which fast enumeration in progress
            // must notice even if the change itself then fails.
            self.changed();
        }
    }

    /// The elements, shared for a copy to hold, without retaining anything.
    fn share(&self) -> Arc<Buffer> {
        self.ivars().items.share()
    }

    fn changed(&self) {
        self.ivars().mutations.bump();
    }

    fn insert(&self, index: usize, object: Option<&AnyObject>) {
        let Some(object) = object else { nil_argument("NSMutableArray", "insertObject:atIndex:", "object") };
        let object = object.retain();
        // SAFETY: only the buffer is touched; the element was retained
        // before.
        let buffer = unsafe { self.buffer() };
        if index > buffer.len() {
            index_out_of_bounds("NSMutableArray", "insertObject:atIndex:", index, buffer.len());
        }
        buffer.insert(index, object);
        self.changed();
    }

    fn remove_at(&self, index: usize) {
        // SAFETY: only the buffer is touched; the element is released after.
        let buffer = unsafe { self.buffer() };
        if index >= buffer.len() {
            range_out_of_bounds("NSMutableArray", "removeObjectsInRange:", index, 1, buffer.len());
        }
        let removed = buffer.remove(index);
        self.changed();
        drop(removed);
    }

    fn replace(&self, index: usize, object: Option<&AnyObject>, method: &str) {
        let Some(object) = object else { nil_argument("NSMutableArray", method, "object") };
        let object = object.retain();
        // SAFETY: only the buffer is touched; the old element is released
        // after.
        let buffer = unsafe { self.buffer() };
        let count = buffer.len();
        let Some(slot) = buffer.as_mut_slice().get_mut(index) else {
            index_out_of_bounds("NSMutableArray", method, index, count);
        };
        let old = std::mem::replace(slot, object);
        self.changed();
        drop(old);
    }

    /// Remove the elements `marked` flags, in one pass.
    fn remove_marked(&self, marked: &[bool]) {
        if !marked.contains(&true) {
            return;
        }
        if let Some(subclass) = self.subclass() {
            return subclass.remove_marked(marked);
        }
        // SAFETY: only the buffer is touched; the elements are released
        // after.
        let removed = unsafe { self.buffer() }.remove_marked(marked);
        self.changed();
        drop(removed);
    }

    /// For each element, `test` of it, with the array read meanwhile.
    fn mark(&self, mut test: impl FnMut(usize, &AnyObject) -> bool) -> Vec<bool> {
        match self.subclass() {
            Some(subclass) => subclass.all().iter().enumerate().map(|(i, e)| test(i, e)).collect(),
            None => self.read().iter().enumerate().map(|(i, e)| test(i, e)).collect(),
        }
    }

    /// Replace every element with `new`. The old ones (or the share of
    /// them) are released afterwards; there is nothing to unshare.
    fn set_all(&self, new: Arc<Buffer>) {
        if let Some(subclass) = self.subclass() {
            subclass.remove_all();
            for item in new.iter() {
                subclass.add(item);
            }
            return;
        }
        let old = self.ivars().items.replace(new, "NSMutableArray", self.obj());
        self.changed();
        drop(old);
    }

    /// Put the elements in the order `order` gives, stably.
    fn sort(&self, mut order: impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering) {
        self.arrange(|items| sorted_positions(items, &mut order));
    }

    /// Put the elements in the order `arrange` gives: it returns their
    /// positions, in their new order.
    fn arrange(&self, arrange: impl FnOnce(&[Retained<AnyObject>]) -> Vec<usize>) {
        if let Some(subclass) = self.subclass() {
            return subclass.arrange(arrange);
        }
        // The array is read while comparisons run, so they can't change it.
        let positions = arrange(&self.read());
        // SAFETY: the elements are only moved among their slots.
        let buffer = unsafe { self.buffer() };
        if buffer.len() == positions.len() {
            let before = pointers(buffer);
            for (slot, at) in buffer.as_mut_slice().iter_mut().zip(positions) {
                // SAFETY: `positions` is a permutation of the slots, so each
                // strong reference moves to exactly one slot and no retain
                // count changes.
                unsafe { ptr::write(ptr::from_mut(slot).cast::<*mut AnyObject>(), before[at]) };
            }
        }
        self.changed();
    }

    /// The elements of `other`, retained, before this array changes: `other`
    /// may be this array.
    fn taken(other: &NSArray) -> Items {
        elements(other).to_vec()
    }
}

define_class!(
    #[unsafe(super(NSArray, NSObject))]
    #[name = "NSMutableArray"]
    #[ivars = MutableArrayIvars]
    pub(crate) struct NSMutableArrayImpl;

    impl NSMutableArrayImpl {
        #[unsafe(method_id(array))]
        fn array() -> Retained<NSMutableArray> {
            make_mutable(Vec::new())
        }

        #[unsafe(method_id(arrayWithCapacity:))]
        fn array_with_capacity(capacity: NSUInteger) -> Retained<NSMutableArray> {
            make_mutable(Vec::with_capacity(capacity))
        }

        #[unsafe(method_id(arrayWithObject:))]
        fn array_with_object(object: &AnyObject) -> Retained<NSMutableArray> {
            make_mutable(vec![object.retain()])
        }

        #[unsafe(method_id(arrayWithObjects:count:))]
        fn array_with_objects(objects: *const *mut AnyObject, count: NSUInteger) -> Retained<NSMutableArray> {
            // SAFETY: the caller passes `count` objects.
            make_mutable(unsafe { from_c_array("NSMutableArray", objects, count) })
        }

        #[unsafe(method_id(arrayWithArray:))]
        fn array_with_array(array: &NSArray) -> Retained<NSMutableArray> {
            make_mutable_from(growable_copy(array))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_mutable(this, owned(Vec::new()))
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, capacity: NSUInteger) -> Retained<Self> {
            init_mutable(this, owned(Vec::with_capacity(capacity)))
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            init_mutable(this, owned(unsafe { from_c_array("NSMutableArray", objects, count) }))
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init_mutable(this, growable_copy(array))
        }

        #[unsafe(method_id(initWithArray:copyItems:))]
        fn init_with_array_copy(this: Allocated<Self>, array: &NSArray, copy: bool) -> Retained<Self> {
            let items = if copy { owned(copy_each(&elements(array))) } else { growable_copy(array) };
            init_mutable(this, items)
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            // SAFETY: reading the length runs no other code.
            unsafe { self.peek() }.len()
        }

        /// Neither retained nor autoreleased: the array keeps it alive.
        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut AnyObject {
            // SAFETY: reading one pointer runs no other code.
            let items = unsafe { self.peek() };
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => index_out_of_bounds("NSMutableArray", "objectAtIndex:", index, items.len()),
            }
        }

        /// A subclass answers through its own `-objectAtIndex:`.
        #[unsafe(method(objectAtIndexedSubscript:))]
        fn object_at_indexed_subscript(&self, index: NSUInteger) -> *mut AnyObject {
            if self.subclass().is_some() {
                // SAFETY: -objectAtIndex: takes an index and returns an
                // object the array keeps alive, or fails.
                return unsafe { msg_send![self, objectAtIndex: index] };
            }
            // SAFETY: reading one pointer runs no other code.
            let items = unsafe { self.peek() };
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => index_out_of_bounds("NSMutableArray", "objectAtIndexedSubscript:", index, items.len()),
            }
        }

        #[unsafe(method(addObject:))]
        fn add_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { nil_argument("NSMutableArray", "insertObject:atIndex:", "object") };
            let object = object.retain();
            // SAFETY: only the buffer is touched; the element was retained
            // before.
            unsafe { self.buffer() }.push(object);
            self.changed();
        }

        #[unsafe(method(insertObject:atIndex:))]
        fn insert_object(&self, object: Option<&AnyObject>, index: NSUInteger) {
            self.insert(index, object);
        }

        #[unsafe(method(removeObjectAtIndex:))]
        fn remove_object_at_index(&self, index: NSUInteger) {
            self.remove_at(index);
        }

        /// Nothing happens to an empty array, as in Foundation.
        #[unsafe(method(removeLastObject))]
        fn remove_last_object(&self) {
            // SAFETY: only the buffer is touched; the element is released
            // after.
            let removed = unsafe { self.buffer() }.pop();
            if removed.is_some() {
                self.changed();
            }
            drop(removed);
        }

        #[unsafe(method(replaceObjectAtIndex:withObject:))]
        fn replace_object_at_index(&self, index: NSUInteger, object: Option<&AnyObject>) {
            self.replace(index, object, "replaceObjectAtIndex:withObject:");
        }

        #[unsafe(method(setObject:atIndexedSubscript:))]
        fn set_object_at_indexed_subscript(&self, object: Option<&AnyObject>, index: NSUInteger) {
            if let Some(subclass) = self.subclass() {
                let Some(object) = object else {
                    nil_argument("NSMutableArray", "setObject:atIndexedSubscript:", "object")
                };
                return if index == subclass.count() { subclass.add(object) } else { subclass.replace(index, object) };
            }
            // SAFETY: reading the length runs no other code.
            if index == unsafe { self.peek() }.len() {
                self.insert(index, object);
            } else {
                self.replace(index, object, "setObject:atIndexedSubscript:");
            }
        }

        #[unsafe(method(exchangeObjectAtIndex:withObjectAtIndex:))]
        fn exchange_objects(&self, a: NSUInteger, b: NSUInteger) {
            if let Some(subclass) = self.subclass() {
                let (x, y) = (subclass.at(a), subclass.at(b));
                subclass.replace(a, &y);
                subclass.replace(b, &x);
                return;
            }
            // SAFETY: only the buffer is touched.
            let buffer = unsafe { self.buffer() };
            let count = buffer.len();
            if let Some(bad) = [a, b].into_iter().find(|&i| i >= count) {
                index_out_of_bounds("NSMutableArray", "exchangeObjectAtIndex:withObjectAtIndex:", bad, count);
            }
            buffer.as_mut_slice().swap(a, b);
            self.changed();
        }

        #[unsafe(method(removeObject:))]
        fn remove_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { return };
            let needle = Needle::new(object);
            let marked = self.mark(|_, e| needle.matches(e));
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObject:inRange:))]
        fn remove_object_in_range(&self, object: Option<&AnyObject>, range: NSRange) {
            check_range(self, "removeObject:inRange:", range, count_of(self));
            let Some(object) = object else { return };
            let needle = Needle::new(object);
            let within = range.location..range.location + range.length;
            let marked = self.mark(|i, e| within.contains(&i) && needle.matches(e));
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectIdenticalTo:))]
        fn remove_object_identical_to(&self, object: *const AnyObject) {
            let marked = self.mark(|_, e| ptr::eq(e, object));
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectIdenticalTo:inRange:))]
        fn remove_object_identical_to_in_range(&self, object: *const AnyObject, range: NSRange) {
            check_range(self, "removeObjectIdenticalTo:inRange:", range, count_of(self));
            let within = range.location..range.location + range.length;
            let marked = self.mark(|i, e| within.contains(&i) && ptr::eq(e, object));
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectsInRange:))]
        fn remove_objects_in_range(&self, range: NSRange) {
            let count = count_of(self);
            let Some(end) = range.location.checked_add(range.length).filter(|&end| end <= count) else {
                range_out_of_bounds("NSMutableArray", "removeObjectsInRange:", range.location, range.length, count);
            };
            if range.length == 0 {
                return;
            }
            if let Some(subclass) = self.subclass() {
                for i in (range.location..end).rev() {
                    subclass.remove(i);
                }
                return;
            }
            // SAFETY: only the buffer is touched; the elements are released
            // after.
            let removed = unsafe { self.buffer() }.drain(range.location..end);
            self.changed();
            drop(removed);
        }

        #[unsafe(method(removeObjectsInArray:))]
        fn remove_objects_in_array(&self, other: &NSArray) {
            let others = Self::taken(other);
            let marked = match self.subclass() {
                Some(subclass) => marked_in(&subclass.all(), others),
                None => marked_in(&self.read(), others),
            };
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectsAtIndexes:))]
        fn remove_objects_at_indexes(&self, indexes: &NSIndexSet) {
            let count = count_of(self);
            let spans = checked_spans("NSMutableArray", "removeObjectsAtIndexes:", indexes, count, Past::InIndexSet);
            let mut marked = vec![false; count];
            for (s, e) in spans {
                marked[s..e].fill(true);
            }
            self.remove_marked(&marked);
        }

        /// Inserts in index order, each index a position in the result.
        #[unsafe(method(insertObjects:atIndexes:))]
        fn insert_objects_at_indexes(&self, objects: &NSArray, indexes: &NSIndexSet) {
            let objects = Self::taken(objects);
            let wanted = index_set::count(indexes);
            if objects.len() != wanted {
                panic!(
                    "*** -[NSMutableArray insertObjects:atIndexes:]: count of array ({}) differs from count of index set ({wanted})",
                    objects.len(),
                );
            }
            let final_count = count_of(self) + objects.len();
            let method = "insertObjects:atIndexes:";
            let spans = checked_spans("NSMutableArray", method, indexes, final_count, Past::InIndexSet);
            if let Some(subclass) = self.subclass() {
                for (i, object) in index_set::walk(&spans, false).zip(&objects) {
                    subclass.insert(object, i);
                }
                return;
            }
            let mut objects = objects.into_iter();
            // SAFETY: only the buffer is touched; the elements were retained
            // before.
            let buffer = unsafe { self.buffer() };
            for (s, e) in spans {
                buffer.insert_all(s, objects.by_ref().take(e - s).collect());
            }
            self.changed();
        }

        #[unsafe(method(replaceObjectsAtIndexes:withObjects:))]
        fn replace_objects_at_indexes(&self, indexes: &NSIndexSet, objects: &NSArray) {
            let objects = Self::taken(objects);
            let wanted = index_set::count(indexes);
            if objects.len() != wanted {
                panic!(
                    "*** -[NSMutableArray replaceObjectsAtIndexes:withObjects:]: count of array ({}) differs from count of index set ({wanted})",
                    objects.len(),
                );
            }
            let method = "replaceObjectsAtIndexes:withObjects:";
            let spans = checked_spans("NSMutableArray", method, indexes, count_of(self), Past::InIndexSet);
            if let Some(subclass) = self.subclass() {
                for (i, object) in index_set::walk(&spans, false).zip(&objects) {
                    subclass.replace(i, object);
                }
                return;
            }
            // SAFETY: only the buffer is touched; the old elements are
            // released after.
            let slots = unsafe { self.buffer() }.as_mut_slice();
            let replaced: Items =
                index_set::walk(&spans, false).zip(objects).map(|(i, o)| std::mem::replace(&mut slots[i], o)).collect();
            self.changed();
            drop(replaced);
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.set_all(counted(Deque::new()));
        }

        #[unsafe(method(addObjectsFromArray:))]
        fn add_objects_from_array(&self, other: &NSArray) {
            let others = Self::taken(other);
            if let Some(subclass) = self.subclass() {
                for item in &others {
                    subclass.add(item);
                }
                return;
            }
            // SAFETY: only the buffer is touched; the elements were retained
            // before.
            unsafe { self.buffer() }.extend(others);
            self.changed();
        }

        #[unsafe(method(replaceObjectsInRange:withObjectsFromArray:))]
        fn replace_objects_in_range(&self, range: NSRange, other: &NSArray) {
            let others = Self::taken(other);
            let count = count_of(self);
            let Some(end) = range.location.checked_add(range.length).filter(|&end| end <= count) else {
                range_out_of_bounds(
                    "NSMutableArray",
                    "replaceObjectsInRange:withObjectsFromArray:",
                    range.location,
                    range.length,
                    count,
                );
            };
            if let Some(subclass) = self.subclass() {
                for i in (range.location..end).rev() {
                    subclass.remove(i);
                }
                for (k, item) in others.iter().enumerate() {
                    subclass.insert(item, range.location + k);
                }
                return;
            }
            // SAFETY: only the buffer is touched; the old elements are
            // released after.
            let buffer = unsafe { self.buffer() };
            let removed = buffer.drain(range.location..end);
            buffer.insert_all(range.location, others);
            self.changed();
            drop(removed);
        }

        #[unsafe(method(setArray:))]
        fn set_array(&self, other: &NSArray) {
            // Taken first: `other` may be this array.
            self.set_all(shared_copy(other));
        }

        #[unsafe(method(sortUsingComparator:))]
        fn sort_using_comparator(&self, comparator: NSComparator) {
            self.sort(block_order(comparator));
        }

        /// Sorts are always stable, which both options allow.
        #[unsafe(method(sortWithOptions:usingComparator:))]
        fn sort_with_options(&self, _options: NSSortOptions, comparator: NSComparator) {
            self.sort(block_order(comparator));
        }

        #[unsafe(method(sortUsingFunction:context:))]
        fn sort_using_function(&self, compare: CompareFn, context: *mut c_void) {
            self.sort(function_order(compare, context));
        }

        #[unsafe(method(sortUsingSelector:))]
        fn sort_using_selector(&self, selector: Sel) {
            self.sort(selector_order(selector));
        }

        #[unsafe(method(sortUsingDescriptors:))]
        fn sort_using_descriptors(&self, descriptors: &NSArray) {
            self.arrange(|items| sort_descriptor::positions(descriptors, items));
        }
    }

    unsafe impl NSObjectProtocol for NSMutableArrayImpl {}
);
