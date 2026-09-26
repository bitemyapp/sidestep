//! `NSIndexSet` and `NSMutableIndexSet`: sets of array indexes.
//!
//! The indexes are kept as sorted, disjoint, non-adjacent ranges, so a set
//! of a million consecutive rows is one range and membership is a binary
//! search. As with the other collections, the immutable class owns its
//! ranges outright and the mutable subclass keeps its own in a `RefCell`;
//! readers are written once over borrowed ranges, or ranges gathered from a
//! subclass defined outside Sidestep through `-firstIndex` and
//! `-indexGreaterThanIndex:`.

use std::cell::{Ref, RefCell, RefMut};
use std::ops::Deref;
use std::ptr::{self, NonNull};

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSEnumerationOptions, NSIndexSet, NSInteger, NSMutableIndexSet, NSNotFound, NSRange, NSString, NSUInteger, NSZone,
};

use crate::util::{self, is_exactly};

/// Indexes stop short of `NSNotFound`.
const LIMIT: usize = NSNotFound as usize;

/// Half-open ranges of indexes, sorted, neither overlapping nor touching.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Ranges(Vec<(usize, usize)>);

impl Ranges {
    /// The ranges of ascending `indexes`.
    pub(crate) fn of_indexes(indexes: impl IntoIterator<Item = usize>) -> Ranges {
        let mut out = Ranges::default();
        for i in indexes {
            out.insert(i, i + 1);
        }
        out
    }

    fn single(start: usize, end: usize) -> Ranges {
        Ranges(if start < end { vec![(start, end)] } else { Vec::new() })
    }

    fn count(&self) -> usize {
        self.0.iter().map(|&(s, e)| e - s).sum()
    }

    fn first(&self) -> Option<usize> {
        self.0.first().map(|r| r.0)
    }

    fn last(&self) -> Option<usize> {
        self.0.last().map(|r| r.1 - 1)
    }

    /// The position of the range holding `i`, if any.
    fn find(&self, i: usize) -> Option<usize> {
        let at = self.0.partition_point(|r| r.0 <= i).checked_sub(1)?;
        (i < self.0[at].1).then_some(at)
    }

    fn contains(&self, i: usize) -> bool {
        self.find(i).is_some()
    }

    /// The least index at or above `i`.
    fn at_or_above(&self, i: usize) -> Option<usize> {
        let at = self.0.partition_point(|r| r.1 <= i);
        self.0.get(at).map(|r| r.0.max(i))
    }

    /// The greatest index at or below `i`.
    fn at_or_below(&self, i: usize) -> Option<usize> {
        let at = self.0.partition_point(|r| r.0 <= i).checked_sub(1)?;
        Some((self.0[at].1 - 1).min(i))
    }

    /// The indexes in `start..end`, as ranges clipped to it.
    fn within(&self, start: usize, end: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let from = self.0.partition_point(|r| r.1 <= start);
        self.0[from..].iter().take_while(move |r| r.0 < end).map(move |&(s, e)| (s.max(start), e.min(end)))
    }

    fn insert(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        // Ranges that overlap or touch the new one merge with it.
        let from = self.0.partition_point(|r| r.1 < start);
        let to = self.0.partition_point(|r| r.0 <= end);
        let (mut s, mut e) = (start, end);
        if from < to {
            s = s.min(self.0[from].0);
            e = e.max(self.0[to - 1].1);
        }
        self.0.splice(from..to, [(s, e)]);
    }

    fn remove(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let from = self.0.partition_point(|r| r.1 <= start);
        let to = self.0.partition_point(|r| r.0 < end);
        if from >= to {
            return;
        }
        let mut keep = Vec::with_capacity(2);
        if self.0[from].0 < start {
            keep.push((self.0[from].0, start));
        }
        if self.0[to - 1].1 > end {
            keep.push((end, self.0[to - 1].1));
        }
        self.0.splice(from..to, keep);
    }

    /// `-shiftIndexesStartingAtIndex:by:`: indexes from `index` on move by
    /// `delta`. Moving down, they replace the indexes they land on, and any
    /// that would go below zero are dropped.
    fn shift(&mut self, index: usize, delta: NSInteger) {
        let (below, above): (Vec<_>, Vec<_>) = {
            let mut below = Vec::new();
            let mut above = Vec::new();
            for &(s, e) in &self.0 {
                if e <= index {
                    below.push((s, e));
                } else if s >= index {
                    above.push((s, e));
                } else {
                    below.push((s, index));
                    above.push((index, e));
                }
            }
            (below, above)
        };
        let mut out = Ranges(below);
        let magnitude = delta.unsigned_abs();
        if delta < 0 {
            out.remove(index.saturating_sub(magnitude), index);
        }
        for (s, e) in above {
            let (s, e) = if delta < 0 {
                (s.saturating_sub(magnitude), e.saturating_sub(magnitude))
            } else {
                (s.saturating_add(magnitude).min(LIMIT), e.saturating_add(magnitude).min(LIMIT))
            };
            out.insert(s, e);
        }
        *self = out;
    }

    /// Every index, in order.
    fn indexes(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.0.iter().flat_map(|&(s, e)| s..e)
    }
}

pub(crate) struct MutableIndexSetIvars {
    ranges: RefCell<Ranges>,
}

/// Which storage an index set object has.
enum Kind<'a> {
    Fixed(&'a NSIndexSetImpl),
    Mutable(&'a NSMutableIndexSetImpl),
    Foreign,
}

fn kind(obj: &AnyObject) -> Kind<'_> {
    if is_exactly(obj, &crate::NSINDEXSET) {
        // SAFETY: an instance of exactly NSIndexSetImpl.
        Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSIndexSetImpl>() })
    } else if is_exactly(obj, &crate::NSMUTABLEINDEXSET) {
        // SAFETY: an instance of exactly NSMutableIndexSetImpl.
        Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableIndexSetImpl>() })
    } else {
        Kind::Foreign
    }
}

/// An index set's ranges, borrowed for reading.
pub(crate) enum Borrowed<'a> {
    Fixed(&'a Ranges),
    Mutable(Ref<'a, Ranges>),
    Gathered(Ranges),
}

impl Deref for Borrowed<'_> {
    type Target = Ranges;

    fn deref(&self) -> &Ranges {
        match self {
            Borrowed::Fixed(r) => r,
            Borrowed::Mutable(r) => r,
            Borrowed::Gathered(r) => r,
        }
    }
}

/// The ranges of any index set.
pub(crate) fn ranges(obj: &AnyObject) -> Borrowed<'_> {
    match kind(obj) {
        Kind::Fixed(s) => Borrowed::Fixed(s.ivars()),
        Kind::Mutable(m) => Borrowed::Mutable(m.ivars().ranges.borrow()),
        Kind::Foreign => Borrowed::Gathered(gather(obj)),
    }
}

/// The indexes of an index set subclass defined outside Sidestep.
fn gather(obj: &AnyObject) -> Ranges {
    let mut out = Ranges::default();
    // SAFETY: NSIndexSet's methods take and return NSUInteger.
    let mut i: NSUInteger = unsafe { msg_send![obj, firstIndex] };
    while i != NSNotFound as NSUInteger {
        out.insert(i, i + 1);
        // SAFETY: as above.
        i = unsafe { msg_send![obj, indexGreaterThanIndex: i] };
    }
    out
}

/// The indexes of an index set, all of them, for use elsewhere.
pub(crate) fn indexes_of(obj: &NSIndexSet) -> Vec<usize> {
    ranges(obj).indexes().collect()
}

/// A new immutable index set.
pub(crate) fn make(ranges: Ranges) -> Retained<NSIndexSet> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSIndexSet::alloc();
    // SAFETY: NSIndexSet's class is NSIndexSetImpl, and an `Allocated` is a
    // pointer to its object whatever its type.
    let this = unsafe { std::mem::transmute::<Allocated<NSIndexSet>, Allocated<NSIndexSetImpl>>(this) };
    // SAFETY: NSIndexSetImpl is the class registered as NSIndexSet.
    unsafe { Retained::cast_unchecked(init_fixed(this, ranges)) }
}

fn make_mutable(ranges: Ranges) -> Retained<NSMutableIndexSet> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSMutableIndexSet::alloc();
    // SAFETY: as in `make`, for NSMutableIndexSetImpl.
    let this = unsafe { std::mem::transmute::<Allocated<NSMutableIndexSet>, Allocated<NSMutableIndexSetImpl>>(this) };
    // SAFETY: NSMutableIndexSetImpl is the class registered as
    // NSMutableIndexSet.
    unsafe { Retained::cast_unchecked(init_mutable(this, ranges)) }
}

fn init_fixed(this: Allocated<NSIndexSetImpl>, ranges: Ranges) -> Retained<NSIndexSetImpl> {
    let this = this.set_ivars(ranges);
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn init_mutable(this: Allocated<NSMutableIndexSetImpl>, ranges: Ranges) -> Retained<NSMutableIndexSetImpl> {
    let this = this.set_ivars(MutableIndexSetIvars { ranges: RefCell::new(ranges) });
    // SAFETY: NSIndexSet's initializer, which leaves its own ranges empty.
    unsafe { msg_send![super(this), init] }
}

/// The ranges `range` covers, failing as Foundation does for one that
/// reaches `NSNotFound`.
fn checked(range: NSRange, receiver: &str, method: &str) -> (usize, usize) {
    match range.location.checked_add(range.length) {
        Some(end) if end <= LIMIT => (range.location, end),
        _ => panic!(
            "*** -[{receiver} {method}]: Range {{{}, {}}} exceeds maximum index value of NSNotFound - 1",
            range.location, range.length
        ),
    }
}

fn not_found(i: Option<usize>) -> NSUInteger {
    i.unwrap_or(NSNotFound as NSUInteger)
}

type IndexBlock = DynBlock<dyn Fn(NSUInteger, NonNull<Bool>)>;
type IndexTest = DynBlock<dyn Fn(NSUInteger, NonNull<Bool>) -> Bool>;
type RangeBlock = DynBlock<dyn Fn(NSRange, NonNull<Bool>)>;

/// The indexes of any index set in `start..end`, in order or reversed, as a
/// snapshot: blocks called with them may change the set.
fn snapshot(obj: &AnyObject, start: usize, end: usize, reverse: bool) -> Vec<usize> {
    let ranges = ranges(obj);
    let mut all: Vec<usize> = ranges.within(start, end).flat_map(|(s, e)| s..e).collect();
    if reverse {
        all.reverse();
    }
    all
}

fn enumerate(obj: &AnyObject, start: usize, end: usize, options: NSEnumerationOptions, block: &IndexBlock) {
    let mut stop = Bool::NO;
    for i in snapshot(obj, start, end, options.contains(NSEnumerationOptions::Reverse)) {
        block.call((i, NonNull::from(&mut stop)));
        if stop.as_bool() {
            break;
        }
    }
}

/// The indexes `test` passes; with `first`, only the first of them.
fn passing(
    obj: &AnyObject,
    range: (usize, usize),
    options: NSEnumerationOptions,
    test: &IndexTest,
    first: bool,
) -> Ranges {
    let mut out = Ranges::default();
    let mut stop = Bool::NO;
    for i in snapshot(obj, range.0, range.1, options.contains(NSEnumerationOptions::Reverse)) {
        if test.call((i, NonNull::from(&mut stop))).as_bool() {
            out.insert(i, i + 1);
            if first {
                break;
            }
        }
        if stop.as_bool() {
            break;
        }
    }
    out
}

fn enumerate_ranges(obj: &AnyObject, start: usize, end: usize, options: NSEnumerationOptions, block: &RangeBlock) {
    let mut ranges: Vec<(usize, usize)> = ranges(obj).within(start, end).collect();
    if options.contains(NSEnumerationOptions::Reverse) {
        ranges.reverse();
    }
    let mut stop = Bool::NO;
    for (s, e) in ranges {
        block.call((NSRange::new(s, e - s), NonNull::from(&mut stop)));
        if stop.as_bool() {
            break;
        }
    }
}

/// `<NSIndexSet: 0x…>[number of indexes: 5 (in 3 ranges), indexes: (1-3 5 10)]`.
fn description(obj: &AnyObject) -> Retained<NSString> {
    use std::fmt::Write;
    let ranges = ranges(obj);
    let mut out = format!("<{}: {:p}>", obj.class().name().to_string_lossy(), obj);
    if ranges.0.is_empty() {
        out.push_str("(no indexes)");
    } else {
        let _ = write!(out, "[number of indexes: {} (in {} ranges), indexes: (", ranges.count(), ranges.0.len());
        for (n, &(s, e)) in ranges.0.iter().enumerate() {
            if n > 0 {
                out.push(' ');
            }
            if e - s == 1 {
                let _ = write!(out, "{s}");
            } else {
                let _ = write!(out, "{s}-{}", e - 1);
            }
        }
        out.push_str(")]");
    }
    NSString::from_str(&out)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSIndexSet"]
    #[ivars = Ranges]
    pub(crate) struct NSIndexSetImpl;

    impl NSIndexSetImpl {
        #[unsafe(method_id(indexSet))]
        fn index_set() -> Retained<NSIndexSet> {
            make(Ranges::default())
        }

        #[unsafe(method_id(indexSetWithIndex:))]
        fn index_set_with_index(index: NSUInteger) -> Retained<NSIndexSet> {
            let (s, e) = checked(NSRange::new(index, 1), "NSIndexSet", "initWithIndexesInRange:");
            make(Ranges::single(s, e))
        }

        #[unsafe(method_id(indexSetWithIndexesInRange:))]
        fn index_set_with_indexes_in_range(range: NSRange) -> Retained<NSIndexSet> {
            let (s, e) = checked(range, "NSIndexSet", "initWithIndexesInRange:");
            make(Ranges::single(s, e))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_fixed(this, Ranges::default())
        }

        #[unsafe(method_id(initWithIndex:))]
        fn init_with_index(this: Allocated<Self>, index: NSUInteger) -> Retained<Self> {
            let (s, e) = checked(NSRange::new(index, 1), "NSIndexSet", "initWithIndexesInRange:");
            init_fixed(this, Ranges::single(s, e))
        }

        #[unsafe(method_id(initWithIndexesInRange:))]
        fn init_with_indexes_in_range(this: Allocated<Self>, range: NSRange) -> Retained<Self> {
            let (s, e) = checked(range, "NSIndexSet", "initWithIndexesInRange:");
            init_fixed(this, Ranges::single(s, e))
        }

        #[unsafe(method_id(initWithIndexSet:))]
        fn init_with_index_set(this: Allocated<Self>, other: &NSIndexSet) -> Retained<Self> {
            let copy = ranges(other).clone();
            init_fixed(this, copy)
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.ivars().count()
        }

        #[unsafe(method(containsIndex:))]
        fn contains_index(&self, index: NSUInteger) -> bool {
            self.ivars().contains(index)
        }

        #[unsafe(method(firstIndex))]
        fn first_index(&self) -> NSUInteger {
            not_found(ranges(self).first())
        }

        #[unsafe(method(lastIndex))]
        fn last_index(&self) -> NSUInteger {
            not_found(ranges(self).last())
        }

        #[unsafe(method(indexGreaterThanIndex:))]
        fn index_greater_than(&self, index: NSUInteger) -> NSUInteger {
            not_found(index.checked_add(1).and_then(|i| ranges(self).at_or_above(i)))
        }

        #[unsafe(method(indexGreaterThanOrEqualToIndex:))]
        fn index_greater_than_or_equal(&self, index: NSUInteger) -> NSUInteger {
            not_found(ranges(self).at_or_above(index))
        }

        #[unsafe(method(indexLessThanIndex:))]
        fn index_less_than(&self, index: NSUInteger) -> NSUInteger {
            not_found(index.checked_sub(1).and_then(|i| ranges(self).at_or_below(i)))
        }

        #[unsafe(method(indexLessThanOrEqualToIndex:))]
        fn index_less_than_or_equal(&self, index: NSUInteger) -> NSUInteger {
            not_found(ranges(self).at_or_below(index))
        }

        /// Fills `buffer` with up to `capacity` indexes from `*range` (or
        /// from all of them), and moves `*range` past the last one given.
        #[unsafe(method(getIndexes:maxCount:inIndexRange:))]
        fn get_indexes(&self, buffer: NonNull<NSUInteger>, capacity: NSUInteger, range: *mut NSRange) -> NSUInteger {
            // SAFETY: the caller passes a valid range pointer or null.
            let (start, end) = match unsafe { range.as_ref() } {
                Some(r) => (r.location, r.location.saturating_add(r.length)),
                None => (0, usize::MAX),
            };
            let ranges = ranges(self);
            let mut n = 0;
            // What is left starts after the last index given, as in
            // Foundation.
            let mut next = end;
            for i in ranges.within(start, end).flat_map(|(s, e)| s..e).take(capacity) {
                // SAFETY: the caller passes room for `capacity` indexes.
                unsafe { buffer.as_ptr().add(n).write(i) };
                n += 1;
                next = i + 1;
            }
            // SAFETY: as above.
            if let Some(r) = unsafe { range.as_mut() } {
                *r = NSRange::new(next, end - next);
            }
            n
        }

        #[unsafe(method(countOfIndexesInRange:))]
        fn count_of_indexes_in_range(&self, range: NSRange) -> NSUInteger {
            let end = range.location.saturating_add(range.length);
            ranges(self).within(range.location, end).map(|(s, e)| e - s).sum()
        }

        #[unsafe(method(containsIndexesInRange:))]
        fn contains_indexes_in_range(&self, range: NSRange) -> bool {
            let ranges = ranges(self);
            range.length > 0
                && ranges.find(range.location).is_some_and(|at| ranges.0[at].1 >= range.location.saturating_add(range.length))
        }

        #[unsafe(method(containsIndexes:))]
        fn contains_indexes(&self, other: &NSIndexSet) -> bool {
            let (mine, theirs) = (ranges(self), ranges(other));
            theirs.0.iter().all(|&(s, e)| mine.find(s).is_some_and(|at| mine.0[at].1 >= e))
        }

        #[unsafe(method(intersectsIndexesInRange:))]
        fn intersects_indexes_in_range(&self, range: NSRange) -> bool {
            let end = range.location.saturating_add(range.length);
            ranges(self).within(range.location, end).next().is_some()
        }

        #[unsafe(method(isEqualToIndexSet:))]
        fn is_equal_to_index_set(&self, other: &NSIndexSet) -> bool {
            *ranges(self) == *ranges(other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| util::is_kind(other, <NSIndexSet as objc2::ClassType>::class()) && *ranges(self) == *ranges(other))
        }

        /// The first and last index and the count: equal sets agree.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            let ranges = ranges(self);
            match (ranges.first(), ranges.last()) {
                (Some(first), Some(last)) => first.wrapping_add(last).wrapping_add(ranges.count()),
                _ => 0,
            }
        }

        #[unsafe(method(enumerateIndexesUsingBlock:))]
        fn enumerate_indexes(&self, block: &IndexBlock) {
            enumerate(self, 0, usize::MAX, NSEnumerationOptions(0), block);
        }

        /// Concurrent enumeration runs on the calling thread, one index at
        /// a time, which the option permits.
        #[unsafe(method(enumerateIndexesWithOptions:usingBlock:))]
        fn enumerate_indexes_with_options(&self, options: NSEnumerationOptions, block: &IndexBlock) {
            enumerate(self, 0, usize::MAX, options, block);
        }

        #[unsafe(method(enumerateIndexesInRange:options:usingBlock:))]
        fn enumerate_indexes_in_range(&self, range: NSRange, options: NSEnumerationOptions, block: &IndexBlock) {
            enumerate(self, range.location, range.location.saturating_add(range.length), options, block);
        }

        #[unsafe(method(indexPassingTest:))]
        fn index_passing_test(&self, test: &IndexTest) -> NSUInteger {
            not_found(passing(self, (0, usize::MAX), NSEnumerationOptions(0), test, true).first())
        }

        #[unsafe(method(indexWithOptions:passingTest:))]
        fn index_with_options_passing_test(&self, options: NSEnumerationOptions, test: &IndexTest) -> NSUInteger {
            not_found(passing(self, (0, usize::MAX), options, test, true).first())
        }

        #[unsafe(method(indexInRange:options:passingTest:))]
        fn index_in_range_passing_test(&self, range: NSRange, options: NSEnumerationOptions, test: &IndexTest) -> NSUInteger {
            let range = (range.location, range.location.saturating_add(range.length));
            not_found(passing(self, range, options, test, true).first())
        }

        #[unsafe(method_id(indexesPassingTest:))]
        fn indexes_passing_test(&self, test: &IndexTest) -> Retained<NSIndexSet> {
            make(passing(self, (0, usize::MAX), NSEnumerationOptions(0), test, false))
        }

        #[unsafe(method_id(indexesWithOptions:passingTest:))]
        fn indexes_with_options_passing_test(&self, options: NSEnumerationOptions, test: &IndexTest) -> Retained<NSIndexSet> {
            make(passing(self, (0, usize::MAX), options, test, false))
        }

        #[unsafe(method_id(indexesInRange:options:passingTest:))]
        fn indexes_in_range_passing_test(
            &self,
            range: NSRange,
            options: NSEnumerationOptions,
            test: &IndexTest,
        ) -> Retained<NSIndexSet> {
            let range = (range.location, range.location.saturating_add(range.length));
            make(passing(self, range, options, test, false))
        }

        #[unsafe(method(enumerateRangesUsingBlock:))]
        fn enumerate_ranges(&self, block: &RangeBlock) {
            enumerate_ranges(self, 0, usize::MAX, NSEnumerationOptions(0), block);
        }

        #[unsafe(method(enumerateRangesWithOptions:usingBlock:))]
        fn enumerate_ranges_with_options(&self, options: NSEnumerationOptions, block: &RangeBlock) {
            enumerate_ranges(self, 0, usize::MAX, options, block);
        }

        #[unsafe(method(enumerateRangesInRange:options:usingBlock:))]
        fn enumerate_ranges_in_range(&self, range: NSRange, options: NSEnumerationOptions, block: &RangeBlock) {
            enumerate_ranges(self, range.location, range.location.saturating_add(range.length), options, block);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSIndexSet> {
            if is_exactly(self, &crate::NSINDEXSET) {
                // Immutable: a copy is the same object.
                // SAFETY: NSIndexSetImpl is the class registered as
                // NSIndexSet.
                unsafe { Retained::cast_unchecked(self.retain()) }
            } else {
                make(ranges(self).clone())
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableIndexSet> {
            make_mutable(ranges(self).clone())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            description(self)
        }
    }

    unsafe impl NSObjectProtocol for NSIndexSetImpl {}
);

impl NSMutableIndexSetImpl {
    fn read(&self) -> Ref<'_, Ranges> {
        self.ivars().ranges.borrow()
    }

    /// The ranges, for changing. Changing the set from inside a block one
    /// of its own methods calls works on a snapshot, so this can't fail
    /// from there.
    fn write(&self) -> RefMut<'_, Ranges> {
        match self.ivars().ranges.try_borrow_mut() {
            Ok(ranges) => ranges,
            Err(_) => util::mutated_while_reading("NSMutableIndexSet", ptr::from_ref(self).cast()),
        }
    }
}

define_class!(
    #[unsafe(super(NSIndexSet, NSObject))]
    #[name = "NSMutableIndexSet"]
    #[ivars = MutableIndexSetIvars]
    pub(crate) struct NSMutableIndexSetImpl;

    impl NSMutableIndexSetImpl {
        #[unsafe(method_id(indexSet))]
        fn index_set() -> Retained<NSMutableIndexSet> {
            make_mutable(Ranges::default())
        }

        #[unsafe(method_id(indexSetWithIndex:))]
        fn index_set_with_index(index: NSUInteger) -> Retained<NSMutableIndexSet> {
            let (s, e) = checked(NSRange::new(index, 1), "NSMutableIndexSet", "initWithIndexesInRange:");
            make_mutable(Ranges::single(s, e))
        }

        #[unsafe(method_id(indexSetWithIndexesInRange:))]
        fn index_set_with_indexes_in_range(range: NSRange) -> Retained<NSMutableIndexSet> {
            let (s, e) = checked(range, "NSMutableIndexSet", "initWithIndexesInRange:");
            make_mutable(Ranges::single(s, e))
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            init_mutable(this, Ranges::default())
        }

        #[unsafe(method_id(initWithIndex:))]
        fn init_with_index(this: Allocated<Self>, index: NSUInteger) -> Retained<Self> {
            let (s, e) = checked(NSRange::new(index, 1), "NSMutableIndexSet", "initWithIndexesInRange:");
            init_mutable(this, Ranges::single(s, e))
        }

        #[unsafe(method_id(initWithIndexesInRange:))]
        fn init_with_indexes_in_range(this: Allocated<Self>, range: NSRange) -> Retained<Self> {
            let (s, e) = checked(range, "NSMutableIndexSet", "initWithIndexesInRange:");
            init_mutable(this, Ranges::single(s, e))
        }

        #[unsafe(method_id(initWithIndexSet:))]
        fn init_with_index_set(this: Allocated<Self>, other: &NSIndexSet) -> Retained<Self> {
            let copy = ranges(other).clone();
            init_mutable(this, copy)
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            self.read().count()
        }

        #[unsafe(method(containsIndex:))]
        fn contains_index(&self, index: NSUInteger) -> bool {
            self.read().contains(index)
        }

        #[unsafe(method(addIndex:))]
        fn add_index(&self, index: NSUInteger) {
            let (s, e) = checked(NSRange::new(index, 1), "NSMutableIndexSet", "addIndexesInRange:");
            self.write().insert(s, e);
        }

        #[unsafe(method(addIndexesInRange:))]
        fn add_indexes_in_range(&self, range: NSRange) {
            let (s, e) = checked(range, "NSMutableIndexSet", "addIndexesInRange:");
            self.write().insert(s, e);
        }

        #[unsafe(method(addIndexes:))]
        fn add_indexes(&self, other: &NSIndexSet) {
            // Gathered first: `other` may be this set.
            let theirs = ranges(other).clone();
            let mut mine = self.write();
            for &(s, e) in &theirs.0 {
                mine.insert(s, e);
            }
        }

        #[unsafe(method(removeIndex:))]
        fn remove_index(&self, index: NSUInteger) {
            self.write().remove(index, index.saturating_add(1));
        }

        #[unsafe(method(removeIndexesInRange:))]
        fn remove_indexes_in_range(&self, range: NSRange) {
            self.write().remove(range.location, range.location.saturating_add(range.length));
        }

        #[unsafe(method(removeIndexes:))]
        fn remove_indexes(&self, other: &NSIndexSet) {
            // Gathered first: `other` may be this set.
            let theirs = ranges(other).clone();
            let mut mine = self.write();
            for &(s, e) in &theirs.0 {
                mine.remove(s, e);
            }
        }

        #[unsafe(method(removeAllIndexes))]
        fn remove_all_indexes(&self) {
            self.write().0.clear();
        }

        #[unsafe(method(shiftIndexesStartingAtIndex:by:))]
        fn shift_indexes(&self, index: NSUInteger, delta: NSInteger) {
            self.write().shift(index, delta);
        }
    }

    unsafe impl NSObjectProtocol for NSMutableIndexSetImpl {}
);

#[cfg(test)]
mod tests {
    use super::Ranges;

    fn of(ranges: &[(usize, usize)]) -> Ranges {
        let mut r = Ranges::default();
        for &(s, e) in ranges {
            r.insert(s, e);
        }
        r
    }

    #[test]
    fn inserting_merges_touching_ranges() {
        assert_eq!(of(&[(1, 3), (3, 4), (6, 7)]).0, [(1, 4), (6, 7)]);
        assert_eq!(of(&[(6, 7), (1, 2), (2, 7)]).0, [(1, 7)]);
        assert_eq!(of(&[(0, 10), (2, 3)]).0, [(0, 10)]);
        assert_eq!(of(&[(5, 6), (1, 2), (3, 4)]).0, [(1, 2), (3, 4), (5, 6)]);
    }

    #[test]
    fn removing_splits_ranges() {
        let mut r = of(&[(0, 10), (20, 30)]);
        r.remove(5, 25);
        assert_eq!(r.0, [(0, 5), (25, 30)]);
        r.remove(2, 3);
        assert_eq!(r.0, [(0, 2), (3, 5), (25, 30)]);
        r.remove(0, 100);
        assert!(r.0.is_empty());
    }

    #[test]
    fn shifting_matches_foundation() {
        let mut r = of(&[(0, 10)]);
        r.shift(5, -2);
        assert_eq!(r.0, [(0, 8)]);
        let mut r = of(&[(0, 3), (7, 8)]);
        r.shift(1, 3);
        assert_eq!(r.0, [(0, 1), (4, 6), (10, 11)]);
        let mut r = of(&[(2, 3)]);
        r.shift(1, -5);
        assert!(r.0.is_empty());
    }

    #[test]
    fn neighbours() {
        let r = of(&[(1, 4), (5, 6), (10, 11)]);
        assert_eq!(r.at_or_above(4), Some(5));
        assert_eq!(r.at_or_above(11), None);
        assert_eq!(r.at_or_below(4), Some(3));
        assert_eq!(r.at_or_below(0), None);
        assert_eq!(r.count(), 5);
        assert_eq!(r.within(2, 6).collect::<Vec<_>>(), [(2, 4), (5, 6)]);
    }
}
