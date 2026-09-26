//! `NSArray` and `NSMutableArray`.
//!
//! An immutable array keeps its elements in a boxed slice (or a buffer
//! shared with the mutable array it was copied from). A mutable one keeps
//! them in a vector in a `RefCell`, with a count of changes that fast
//! enumeration watches. Methods that read an array are written
//! once, on `NSArray`, over a slice of the elements: an immutable array's
//! own, a mutable array's while it is borrowed, or, for a subclass defined
//! outside Sidestep, one gathered through its `-count` and
//! `-objectAtIndex:`. The primitive readers (`-count`, `-objectAtIndex:`)
//! are defined on each class directly and send no messages.
//!
//! A mutable array stays borrowed while a method reading it sends messages
//! to its elements (`-isEqual:`, comparators, `-description`), so a
//! callback that mutates the array fails loudly rather than freeing
//! elements out from under the loop. Mutations never run other code while
//! they hold the array: they retain what they add before, and release what
//! they remove after. `-enumerateObjectsUsingBlock:` lets its block mutate
//! the array, as Foundation does; it retains each element for the call.
//!
//! Copies are copy-on-write, as in Foundation: a copy (immutable or
//! mutable) of a mutable array shares its element buffer, and whichever
//! mutable array changes next takes a private copy of the buffer first. So
//! copying a mutable array takes the same time however large it is, which
//! matters because returning `[items copy]` is idiomatic.

use std::cell::{Ref, RefCell, RefMut};
use std::cmp::Ordering;
use std::ffi::{c_ulong, c_void};
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, MessageReceiver, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSArray, NSBinarySearchingOptions, NSComparator, NSEnumerationOptions, NSEnumerator, NSFastEnumerationState,
    NSIndexSet, NSInteger, NSMutableArray, NSNotFound, NSRange, NSSortOptions, NSString, NSUInteger, NSZone,
};

use crate::enumerator::{self, Mutations, Source, immutable_mutations};
use crate::util::{self, Needle, equal, index_out_of_bounds, is_exactly, nil_argument, range_out_of_bounds};
use crate::{describe, index_set, string};

type Items = Vec<Retained<AnyObject>>;

/// The block `-enumerateObjectsUsingBlock:` calls with each element, its
/// index and a flag to stop.
type ElementBlock = DynBlock<dyn Fn(NonNull<AnyObject>, NSUInteger, NonNull<Bool>)>;

/// An immutable array's elements: its own, or shared with the mutable
/// array it was copied from (and that array's other copies).
pub(crate) enum Store {
    Owned(Box<[Retained<AnyObject>]>),
    Shared(Arc<Items>),
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

/// A mutable array's elements: its own, or, since a copy, shared with the
/// copies until the next change.
enum Growable {
    Own(Items),
    Shared(Arc<Items>),
}

impl Default for Growable {
    fn default() -> Self {
        Growable::Own(Vec::new())
    }
}

impl Deref for Growable {
    type Target = Items;

    #[inline]
    fn deref(&self) -> &Items {
        match self {
            Growable::Own(items) => items,
            Growable::Shared(items) => items,
        }
    }
}

impl Growable {
    /// The elements, owned. A buffer no copy shares any more is taken back
    /// as it is; `unshare` has already copied one that copies still share.
    fn own(&mut self) -> &mut Items {
        if let Growable::Shared(shared) = self {
            *self = Growable::Own(std::mem::take(Arc::make_mut(shared)));
        }
        match self {
            Growable::Own(items) => items,
            Growable::Shared(_) => unreachable!("just owned"),
        }
    }
}

#[derive(Default)]
pub(crate) struct ArrayIvars {
    items: Store,
}

#[derive(Default)]
pub(crate) struct MutableArrayIvars {
    items: RefCell<Growable>,
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

/// An array's elements, borrowed for reading.
enum Elements<'a> {
    Fixed(&'a [Retained<AnyObject>]),
    Mutable(Ref<'a, Items>),
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
        Kind::Mutable(m) => Elements::Mutable(m.items()),
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
        Kind::Mutable(m) => item(&m.items()),
        Kind::Foreign => {
            // SAFETY: as in `gather`.
            let count: NSUInteger = unsafe { msg_send![obj, count] };
            // SAFETY: as in `gather`; the array keeps the element alive.
            (index < count).then(|| unsafe { msg_send![obj, objectAtIndex: index] })
        }
    }
}

/// The count of any array.
fn count_of(obj: &AnyObject) -> usize {
    match kind(obj) {
        Kind::Fixed(a) => a.ivars().items.len(),
        Kind::Mutable(m) => m.items().len(),
        // SAFETY: -count takes nothing and returns NSUInteger.
        Kind::Foreign => unsafe { msg_send![obj, count] },
    }
}

/// The mutation count fast enumeration of `obj` watches.
pub(crate) fn mutations(obj: &AnyObject) -> *mut c_ulong {
    match kind(obj) {
        Kind::Mutable(m) => m.ivars().mutations.as_ptr(),
        _ => immutable_mutations(),
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
    make_mutable_from(Growable::Own(items))
}

fn make_mutable_from(items: Growable) -> Retained<NSMutableArray> {
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

fn init_mutable(this: Allocated<NSMutableArrayImpl>, items: Growable) -> Retained<NSMutableArrayImpl> {
    let this = this.set_ivars(MutableArrayIvars { items: RefCell::new(items), mutations: Mutations::default() });
    // SAFETY: NSArray's initializer, which leaves its own storage empty.
    unsafe { msg_send![super(this), init] }
}

/// The elements of any array for a new array to hold: shared with it where
/// copy-on-write allows (`Ok`), else retained afresh (`Err`).
fn copy_elements(obj: &AnyObject) -> Result<Arc<Items>, Items> {
    match kind(obj) {
        Kind::Fixed(a) => match &a.ivars().items {
            Store::Shared(items) => Ok(items.clone()),
            Store::Owned(items) => Err(items.to_vec()),
        },
        Kind::Mutable(m) => m.share().ok_or_else(|| m.items().to_vec()),
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
fn growable_copy(obj: &AnyObject) -> Growable {
    match copy_elements(obj) {
        Ok(shared) => Growable::Shared(shared),
        Err(items) => Growable::Own(items),
    }
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

type CompareFn = unsafe extern "C-unwind" fn(NonNull<AnyObject>, NonNull<AnyObject>, *mut c_void) -> NSInteger;

fn function_order(compare: CompareFn, context: *mut c_void) -> impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering {
    move |a, b| {
        // SAFETY: the caller passes a comparison function for the elements;
        // elements are never null.
        unsafe { compare(NonNull::new_unchecked(a), NonNull::new_unchecked(b), context) }.cmp(&0)
    }
}

pub(crate) fn selector_order(selector: Sel) -> impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering {
    move |a, b| {
        // SAFETY: the caller passes a comparison selector the elements
        // answer, taking an object and returning NSComparisonResult.
        let order: NSInteger = unsafe { MessageReceiver::send_message(a, selector, (b,)) };
        order.cmp(&0)
    }
}

/// The elements of `obj` sorted, retained, as a new array.
fn sorted(obj: &AnyObject, mut order: impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering) -> Retained<NSArray> {
    let items = elements(obj);
    let mut pointers: Vec<*mut AnyObject> = items.iter().map(|o| Retained::as_ptr(o).cast_mut()).collect();
    sort_stable(&mut pointers, &mut order);
    // SAFETY: the pointers are the array's elements, alive while it is
    // borrowed.
    let sorted = pointers.into_iter().map(|p| unsafe { Retained::retain(p) }.expect("non-null")).collect();
    drop(items);
    make(sorted)
}

/// `-indexOfObject:inSortedRange:options:usingComparator:` over sorted
/// `items`: where `object` is, or with `InsertionIndex` where it would go;
/// with `FirstEqual` or `LastEqual`, the first or last of equal elements.
fn binary_search(
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

/// The elements at `indexes`, retained, failing as Foundation does for an
/// index past the end.
fn at_indexes(items: &[Retained<AnyObject>], indexes: &NSIndexSet, receiver: &str, method: &str) -> Items {
    let chosen = index_set::indexes_of(indexes);
    if let Some(&last) = chosen.last()
        && last >= items.len()
    {
        util::index_set_out_of_bounds(receiver, method, last, items.len());
    }
    chosen.into_iter().map(|i| items[i].clone()).collect()
}

/// A test block for elements: element, index and a flag to stop.
type ElementTest = DynBlock<dyn Fn(NonNull<AnyObject>, NSUInteger, NonNull<Bool>) -> Bool>;

/// The positions to test, in the order to test them: `indexes`, or all.
fn positions(obj: &AnyObject, indexes: Option<&NSIndexSet>, options: NSEnumerationOptions) -> Vec<usize> {
    let mut chosen = match indexes {
        Some(indexes) => index_set::indexes_of(indexes),
        None => (0..count_of(obj)).collect(),
    };
    if options.contains(NSEnumerationOptions::Reverse) {
        chosen.reverse();
    }
    chosen
}

/// Call `test` on the elements at `indexes` (or all) until it stops,
/// passing each element retained; `on_pass` hears of those that pass and
/// says whether to go on.
fn test_elements(
    obj: &AnyObject,
    indexes: Option<&NSIndexSet>,
    options: NSEnumerationOptions,
    test: &ElementTest,
    mut on_pass: impl FnMut(usize) -> bool,
) {
    let mut stop = Bool::NO;
    for i in positions(obj, indexes, options) {
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
    indexes: Option<&NSIndexSet>,
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
    indexes: Option<&NSIndexSet>,
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

/// `[obj description]` as Rust text, for `-componentsJoinedByString:`.
fn text_of(obj: &AnyObject) -> String {
    util::description(obj)
}

/// `-isEqualToArray:` for any two arrays: the same count, and each element
/// `-isEqual:` to the other's at its index.
fn arrays_equal(a: &AnyObject, b: &AnyObject) -> bool {
    if ptr::eq(a, b) {
        return true;
    }
    let (items, others) = (elements(a), elements(b));
    // Arrays of the very same objects are common (copies), and comparing
    // the pointers wholesale is a memcmp.
    items.len() == others.len()
        && (addresses(&items) == addresses(&others) || items.iter().zip(others.iter()).all(|(x, y)| equal(x, y)))
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

/// `-enumerateObjectsWithOptions:usingBlock:` for any array.
fn enumerate(obj: &AnyObject, reverse: bool, block: &ElementBlock) {
    let mut stop = Bool::NO;
    let mut call = |item: &AnyObject, i: usize| {
        block.call((NonNull::from(item), i, NonNull::from(&mut stop)));
        stop.as_bool()
    };
    match kind(obj) {
        Kind::Fixed(a) => {
            let items = &a.ivars().items;
            let count = items.len();
            for step in 0..count {
                let i = if reverse { count - 1 - step } else { step };
                if call(&items[i], i) {
                    break;
                }
            }
        }
        _ => {
            // The block may mutate the array: take each element afresh,
            // retained, and stop at the end of what is left.
            let count = count_of(obj);
            for step in 0..count {
                let i = if reverse { count - 1 - step } else { step };
                // SAFETY: the array keeps the element alive until retained.
                let Some(item) = element_at(obj, i).map(|p| unsafe { Retained::retain(p) }.expect("non-null")) else {
                    break;
                };
                if call(&item, i) {
                    break;
                }
            }
        }
    }
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

        #[unsafe(method(objectAtIndexedSubscript:))]
        fn object_at_indexed_subscript(&self, index: NSUInteger) -> *mut AnyObject {
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
        fn index_of_object_passing_test(
            &self,
            test: &DynBlock<dyn Fn(NonNull<AnyObject>, NSUInteger, NonNull<Bool>) -> Bool>,
        ) -> NSUInteger {
            let items = elements(self);
            let mut stop = Bool::NO;
            for (i, obj) in items.iter().enumerate() {
                if test.call((NonNull::from(&**obj), i, NonNull::from(&mut stop))).as_bool() {
                    return i;
                }
                if stop.as_bool() {
                    break;
                }
            }
            NSNotFound as NSUInteger
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
            let picked = at_indexes(&items, indexes, "NSArray", "objectsAtIndexes:");
            drop(items);
            make(picked)
        }

        #[unsafe(method(enumerateObjectsAtIndexes:options:usingBlock:))]
        fn enumerate_objects_at_indexes(&self, indexes: &NSIndexSet, options: NSEnumerationOptions, block: &ElementBlock) {
            let mut chosen = index_set::indexes_of(indexes);
            if let Some(&last) = chosen.last() {
                let count = count_of(self);
                if last >= count {
                    util::index_set_out_of_bounds("NSArray", "enumerateObjectsAtIndexes:options:usingBlock:", last, count);
                }
            }
            if options.contains(NSEnumerationOptions::Reverse) {
                chosen.reverse();
            }
            let mut stop = Bool::NO;
            for i in chosen {
                // SAFETY: the array keeps the element alive until retained.
                let Some(item) = element_at(self, i).map(|p| unsafe { Retained::retain(p) }.expect("non-null")) else {
                    break;
                };
                block.call((NonNull::from(&*item), i, NonNull::from(&mut stop)));
                if stop.as_bool() {
                    break;
                }
            }
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
            indexes_passing(self, Some(indexes), options, test)
        }

        #[unsafe(method(indexOfObjectWithOptions:passingTest:))]
        fn index_of_object_with_options(&self, options: NSEnumerationOptions, test: &ElementTest) -> NSUInteger {
            first_passing(self, None, options, test)
        }

        #[unsafe(method(indexOfObjectAtIndexes:options:passingTest:))]
        fn index_of_object_at_indexes(&self, indexes: &NSIndexSet, options: NSEnumerationOptions, test: &ElementTest) -> NSUInteger {
            first_passing(self, Some(indexes), options, test)
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
                    None => out.push_str(&text_of(obj)),
                }
            }
            NSString::from_str(&out)
        }

        #[unsafe(method_id(objectEnumerator))]
        fn object_enumerator(&self) -> Retained<NSEnumerator> {
            enumerator::make(Source::Array(util::upcast(self.retain())))
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

        #[unsafe(method(makeObjectsPerformSelector:))]
        fn make_objects_perform_selector(&self, selector: Sel) {
            for obj in snapshot(self).iter() {
                perform(obj, selector, None);
            }
        }

        #[unsafe(method(makeObjectsPerformSelector:withObject:))]
        fn make_objects_perform_selector_with_object(&self, selector: Sel, argument: Option<&AnyObject>) {
            for obj in snapshot(self).iter() {
                perform(obj, selector, Some(argument));
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
                    let items = m.ivars().items.borrow();
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

/// The elements of any array, for calls that may change it: a mutable
/// array's are retained afresh, so the calls see (and change) the array
/// itself, not the loop.
fn snapshot(obj: &AnyObject) -> Elements<'_> {
    match elements(obj) {
        Elements::Mutable(items) => Elements::Gathered(items.to_vec()),
        items => items,
    }
}

/// Send `selector` to `obj`, with or without an argument, ignoring any
/// result, as `-makeObjectsPerformSelector:` does.
pub(crate) fn perform(obj: &AnyObject, selector: Sel, argument: Option<Option<&AnyObject>>) {
    // A struct result may be returned through memory the caller provides,
    // which a call that ignores the result doesn't; such methods are not
    // for this.
    if let Some(method) = obj.class().instance_method(selector) {
        let result = method.return_type();
        if matches!(result.to_bytes().first(), Some(b'{' | b'(' | b'[')) {
            panic!(
                "*** -[NSArray makeObjectsPerformSelector:]: {selector} returns a struct ({})",
                result.to_string_lossy()
            );
        }
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

impl NSMutableArrayImpl {
    fn items(&self) -> Ref<'_, Items> {
        Ref::map(self.ivars().items.borrow(), |items| &**items)
    }

    /// The storage, for changing. Mutating the array from inside one of
    /// its own reading methods (an element's `-isEqual:`, a comparator)
    /// fails here.
    fn storage_mut(&self) -> RefMut<'_, Growable> {
        match self.ivars().items.try_borrow_mut() {
            Ok(items) => items,
            Err(_) => util::mutated_while_reading("NSMutableArray", ptr::from_ref(self).cast()),
        }
    }

    /// The elements, for changing, owned by this array alone.
    #[inline]
    fn items_mut(&self) -> RefMut<'_, Items> {
        let storage = self.storage_mut();
        if let Growable::Shared(_) = &*storage {
            drop(storage);
            self.unshare();
            // The element buffer may have moved, which fast enumeration in
            // progress must notice even if the change itself then fails.
            self.changed();
            return RefMut::map(self.storage_mut(), Growable::own);
        }
        RefMut::map(storage, Growable::own)
    }

    /// Before a change: if copies share the elements, copy them, retaining
    /// each, with the array only borrowed for reading meanwhile.
    fn unshare(&self) {
        let copy = match &*self.ivars().items.borrow() {
            Growable::Shared(shared) if Arc::strong_count(shared) > 1 => (**shared).clone(),
            _ => return,
        };
        let old = std::mem::replace(&mut *self.storage_mut(), Growable::Own(copy));
        // Released with no borrow held: the copies may have gone meanwhile.
        drop(old);
    }

    /// The elements, shared for a copy to hold: the buffer moves behind a
    /// reference count, without retaining anything. `None` while the array
    /// is being read, when the copy takes elements of its own instead.
    fn share(&self) -> Option<Arc<Items>> {
        let mut storage = self.ivars().items.try_borrow_mut().ok()?;
        if let Growable::Own(items) = &mut *storage {
            // The element buffer itself stays where it is, so fast
            // enumeration in progress is unaffected.
            let items = std::mem::take(items);
            // Atomically counted although the elements aren't `Send`: an
            // immutable copy may be released on any thread, as Objective-C
            // allows, and objc_release is thread-safe.
            #[allow(clippy::arc_with_non_send_sync)]
            let shared = Arc::new(items);
            *storage = Growable::Shared(shared);
        }
        match &*storage {
            Growable::Shared(shared) => Some(shared.clone()),
            Growable::Own(_) => unreachable!("just shared"),
        }
    }

    fn changed(&self) {
        self.ivars().mutations.bump();
    }

    fn insert(&self, index: usize, object: Option<&AnyObject>) {
        let Some(object) = object else { nil_argument("NSMutableArray", "insertObject:atIndex:", "object") };
        let object = object.retain();
        let mut items = self.items_mut();
        if index > items.len() {
            let count = items.len();
            drop(items);
            index_out_of_bounds("NSMutableArray", "insertObject:atIndex:", index, count);
        }
        items.insert(index, object);
        drop(items);
        self.changed();
    }

    fn remove_at(&self, index: usize) {
        let mut items = self.items_mut();
        if index >= items.len() {
            let count = items.len();
            drop(items);
            range_out_of_bounds("NSMutableArray", "removeObjectsInRange:", index, 1, count);
        }
        let removed = items.remove(index);
        drop(items);
        self.changed();
        drop(removed);
    }

    fn replace(&self, index: usize, object: Option<&AnyObject>, method: &str) {
        let Some(object) = object else { nil_argument("NSMutableArray", method, "object") };
        let object = object.retain();
        let mut items = self.items_mut();
        let Some(slot) = items.get_mut(index) else {
            let count = items.len();
            drop(items);
            index_out_of_bounds("NSMutableArray", method, index, count);
        };
        let old = std::mem::replace(slot, object);
        drop(items);
        self.changed();
        drop(old);
    }

    /// Remove the elements `marked` flags, in one pass.
    fn remove_marked(&self, marked: &[bool]) {
        if !marked.contains(&true) {
            return;
        }
        let mut items = self.items_mut();
        let mut i = 0;
        let removed: Items = items
            .extract_if(.., |_| {
                i += 1;
                marked.get(i - 1).copied().unwrap_or(false)
            })
            .collect();
        drop(items);
        self.changed();
        drop(removed);
    }

    /// Replace every element with `new`. The old ones (or the share of
    /// them) are released afterwards; there is nothing to unshare.
    fn set_all(&self, new: Growable) {
        let old = std::mem::replace(&mut *self.storage_mut(), new);
        self.changed();
        drop(old);
    }

    /// Put the elements in the order `order` gives, stably.
    fn sort(&self, mut order: impl FnMut(*mut AnyObject, *mut AnyObject) -> Ordering) {
        let items = self.items();
        let mut pointers: Vec<*mut AnyObject> = items.iter().map(|o| Retained::as_ptr(o).cast_mut()).collect();
        // The array stays borrowed, so the comparator can read it but not
        // change it.
        sort_stable(&mut pointers, &mut order);
        drop(items);
        let mut items = self.items_mut();
        debug_assert_eq!(items.len(), pointers.len());
        for (slot, p) in items.iter_mut().zip(pointers) {
            // SAFETY: `pointers` is a permutation of the elements, each held
            // once by the vector, so rewriting the slots with it moves each
            // strong reference without changing any retain count. Nothing
            // ran between the sort and here.
            unsafe { ptr::write(slot as *mut Retained<AnyObject> as *mut *mut AnyObject, p) };
        }
        drop(items);
        self.changed();
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
            init_mutable(this, Growable::default())
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, capacity: NSUInteger) -> Retained<Self> {
            init_mutable(this, Growable::Own(Vec::with_capacity(capacity)))
        }

        #[unsafe(method_id(initWithObjects:count:))]
        fn init_with_objects(this: Allocated<Self>, objects: *const *mut AnyObject, count: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `count` objects.
            init_mutable(this, Growable::Own(unsafe { from_c_array("NSMutableArray", objects, count) }))
        }

        #[unsafe(method_id(initWithArray:))]
        fn init_with_array(this: Allocated<Self>, array: &NSArray) -> Retained<Self> {
            init_mutable(this, growable_copy(array))
        }

        #[unsafe(method_id(initWithArray:copyItems:))]
        fn init_with_array_copy(this: Allocated<Self>, array: &NSArray, copy: bool) -> Retained<Self> {
            let items = if copy { Growable::Own(copy_each(&elements(array))) } else { growable_copy(array) };
            init_mutable(this, items)
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.items().len()
        }

        /// Neither retained nor autoreleased: the array keeps it alive.
        #[unsafe(method(objectAtIndex:))]
        fn object_at_index(&self, index: NSUInteger) -> *mut AnyObject {
            let items = self.items();
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => {
                    let count = items.len();
                    drop(items);
                    index_out_of_bounds("NSMutableArray", "objectAtIndex:", index, count)
                }
            }
        }

        #[unsafe(method(objectAtIndexedSubscript:))]
        fn object_at_indexed_subscript(&self, index: NSUInteger) -> *mut AnyObject {
            let items = self.items();
            match items.get(index) {
                Some(obj) => Retained::as_ptr(obj).cast_mut(),
                None => {
                    let count = items.len();
                    drop(items);
                    index_out_of_bounds("NSMutableArray", "objectAtIndexedSubscript:", index, count)
                }
            }
        }

        #[unsafe(method(addObject:))]
        fn add_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { nil_argument("NSMutableArray", "insertObject:atIndex:", "object") };
            let object = object.retain();
            self.items_mut().push(object);
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
            let removed = self.items_mut().pop();
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
            if index == self.items().len() {
                self.insert(index, object);
            } else {
                self.replace(index, object, "setObject:atIndexedSubscript:");
            }
        }

        #[unsafe(method(exchangeObjectAtIndex:withObjectAtIndex:))]
        fn exchange_objects(&self, a: NSUInteger, b: NSUInteger) {
            let mut items = self.items_mut();
            let count = items.len();
            if let Some(bad) = [a, b].into_iter().find(|&i| i >= count) {
                drop(items);
                index_out_of_bounds("NSMutableArray", "exchangeObjectAtIndex:withObjectAtIndex:", bad, count);
            }
            items.swap(a, b);
            drop(items);
            self.changed();
        }

        #[unsafe(method(removeObject:))]
        fn remove_object(&self, object: Option<&AnyObject>) {
            let Some(object) = object else { return };
            let needle = Needle::new(object);
            let marked: Vec<bool> = self.items().iter().map(|e| needle.matches(e)).collect();
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObject:inRange:))]
        fn remove_object_in_range(&self, object: Option<&AnyObject>, range: NSRange) {
            let items = self.items();
            check_range(self, "removeObject:inRange:", range, items.len());
            let Some(object) = object else { return };
            let needle = Needle::new(object);
            let end = range.location + range.length;
            let marked: Vec<bool> =
                items.iter().enumerate().map(|(i, e)| (range.location..end).contains(&i) && needle.matches(e)).collect();
            drop(items);
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectIdenticalTo:))]
        fn remove_object_identical_to(&self, object: *const AnyObject) {
            let marked: Vec<bool> = self.items().iter().map(|e| ptr::eq(&**e, object)).collect();
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectIdenticalTo:inRange:))]
        fn remove_object_identical_to_in_range(&self, object: *const AnyObject, range: NSRange) {
            let items = self.items();
            check_range(self, "removeObjectIdenticalTo:inRange:", range, items.len());
            let end = range.location + range.length;
            let marked: Vec<bool> =
                items.iter().enumerate().map(|(i, e)| (range.location..end).contains(&i) && ptr::eq(&**e, object)).collect();
            drop(items);
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectsInRange:))]
        fn remove_objects_in_range(&self, range: NSRange) {
            let mut items = self.items_mut();
            if range.location.checked_add(range.length).is_none_or(|end| end > items.len()) {
                let count = items.len();
                drop(items);
                range_out_of_bounds("NSMutableArray", "removeObjectsInRange:", range.location, range.length, count);
            }
            let removed: Items = items.drain(range.location..range.location + range.length).collect();
            drop(items);
            if !removed.is_empty() {
                self.changed();
            }
            drop(removed);
        }

        #[unsafe(method(removeObjectsInArray:))]
        fn remove_objects_in_array(&self, other: &NSArray) {
            let others = elements(other).to_vec();
            let marked: Vec<bool> =
                self.items().iter().map(|e| others.iter().any(|o| equal(o, e))).collect();
            self.remove_marked(&marked);
        }

        #[unsafe(method(removeObjectsAtIndexes:))]
        fn remove_objects_at_indexes(&self, indexes: &NSIndexSet) {
            let chosen = index_set::indexes_of(indexes);
            let count = self.items().len();
            if let Some(&last) = chosen.last()
                && last >= count
            {
                util::index_set_out_of_bounds("NSMutableArray", "removeObjectsAtIndexes:", last, count);
            }
            let mut marked = vec![false; count];
            for i in chosen {
                marked[i] = true;
            }
            self.remove_marked(&marked);
        }

        /// Inserts in index order, each index a position in the result.
        #[unsafe(method(insertObjects:atIndexes:))]
        fn insert_objects_at_indexes(&self, objects: &NSArray, indexes: &NSIndexSet) {
            let chosen = index_set::indexes_of(indexes);
            let objects = elements(objects).to_vec();
            if objects.len() != chosen.len() {
                panic!(
                    "*** -[NSMutableArray insertObjects:atIndexes:]: count of array ({}) differs from count of index set ({})",
                    objects.len(),
                    chosen.len()
                );
            }
            let mut items = self.items_mut();
            let final_count = items.len() + objects.len();
            if let Some(&last) = chosen.last()
                && last >= final_count
            {
                drop(items);
                util::index_set_out_of_bounds("NSMutableArray", "insertObjects:atIndexes:", last, final_count);
            }
            for (i, object) in chosen.into_iter().zip(objects) {
                items.insert(i, object);
            }
            drop(items);
            self.changed();
        }

        #[unsafe(method(replaceObjectsAtIndexes:withObjects:))]
        fn replace_objects_at_indexes(&self, indexes: &NSIndexSet, objects: &NSArray) {
            let chosen = index_set::indexes_of(indexes);
            let objects = elements(objects).to_vec();
            if objects.len() != chosen.len() {
                panic!(
                    "*** -[NSMutableArray replaceObjectsAtIndexes:withObjects:]: count of array ({}) differs from count of index set ({})",
                    objects.len(),
                    chosen.len()
                );
            }
            let mut items = self.items_mut();
            if let Some(&last) = chosen.last()
                && last >= items.len()
            {
                let count = items.len();
                drop(items);
                util::index_set_out_of_bounds("NSMutableArray", "replaceObjectsAtIndexes:withObjects:", last, count);
            }
            let replaced: Items = chosen.into_iter().zip(objects).map(|(i, o)| std::mem::replace(&mut items[i], o)).collect();
            drop(items);
            self.changed();
            drop(replaced);
        }

        #[unsafe(method(removeAllObjects))]
        fn remove_all_objects(&self) {
            self.set_all(Growable::default());
        }

        #[unsafe(method(addObjectsFromArray:))]
        fn add_objects_from_array(&self, other: &NSArray) {
            // Gathered first: `other` may be this array.
            let others = elements(other).to_vec();
            self.items_mut().extend(others);
            self.changed();
        }

        #[unsafe(method(replaceObjectsInRange:withObjectsFromArray:))]
        fn replace_objects_in_range(&self, range: NSRange, other: &NSArray) {
            let others = elements(other).to_vec();
            let mut items = self.items_mut();
            if range.location.checked_add(range.length).is_none_or(|end| end > items.len()) {
                let count = items.len();
                drop(items);
                range_out_of_bounds(
                    "NSMutableArray",
                    "replaceObjectsInRange:withObjectsFromArray:",
                    range.location,
                    range.length,
                    count,
                );
            }
            let removed: Items = items.splice(range.location..range.location + range.length, others).collect();
            drop(items);
            self.changed();
            drop(removed);
        }

        #[unsafe(method(setArray:))]
        fn set_array(&self, other: &NSArray) {
            // Taken first: `other` may be this array.
            self.set_all(growable_copy(other));
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
    }

    unsafe impl NSObjectProtocol for NSMutableArrayImpl {}
);
