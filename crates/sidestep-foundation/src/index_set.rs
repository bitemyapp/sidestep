//! `NSIndexSet` and `NSMutableIndexSet`: sets of array indexes.
//!
//! The indexes are kept as sorted, disjoint, non-adjacent ranges, with their
//! total, so a set of a million consecutive rows is one range, membership
//! is a binary search and `-count` a field read. Methods that call a block
//! copy the ranges first (not the indexes, which may be far more) and walk
//! them lazily, so a block may change the set and a block that stops at
//! once costs nothing more for a larger set.
//!
//! As with the other collections, the immutable class owns its ranges
//! outright and the mutable subclass keeps its own in a [`Guarded`] cell.
//! No method runs other code while it holds a set's ranges. `NSIndexSet` is
//! a concrete class in Foundation, not a cluster, so a subclass keeps its
//! indexes in the storage it inherits, and inherited methods read it
//! directly; only objects outside the class hierarchy are asked through
//! `-firstIndex` and `-indexGreaterThanIndex:`.

use std::ptr::NonNull;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSEnumerationOptions, NSIndexSet, NSInteger, NSMutableIndexSet, NSNotFound, NSRange, NSString, NSUInteger, NSZone,
};

use crate::guarded::Guarded;
use crate::util::{self, inherits, is_exactly};

/// Indexes stop short of `NSNotFound`.
const LIMIT: usize = NSNotFound as usize;

/// Half-open ranges of indexes, sorted, neither overlapping nor touching,
/// and how many indexes they hold.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Ranges {
    spans: Vec<(usize, usize)>,
    count: usize,
}

impl Ranges {
    /// The ranges of `indexes`, in any order.
    pub(crate) fn of_indexes(mut indexes: Vec<usize>) -> Ranges {
        if !indexes.is_sorted() {
            indexes.sort_unstable();
        }
        let mut out = Ranges::default();
        for i in indexes {
            match out.spans.last_mut() {
                Some((_, end)) if i < *end => {}
                Some((_, end)) if i == *end => {
                    *end += 1;
                    out.count += 1;
                }
                _ => {
                    out.spans.push((i, i + 1));
                    out.count += 1;
                }
            }
        }
        out
    }

    fn single(start: usize, end: usize) -> Ranges {
        if start < end { Ranges { spans: vec![(start, end)], count: end - start } } else { Ranges::default() }
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    fn first(&self) -> Option<usize> {
        self.spans.first().map(|r| r.0)
    }

    pub(crate) fn last(&self) -> Option<usize> {
        self.spans.last().map(|r| r.1 - 1)
    }

    /// The position of the range holding `i`, if any.
    fn find(&self, i: usize) -> Option<usize> {
        let at = self.spans.partition_point(|r| r.0 <= i).checked_sub(1)?;
        (i < self.spans[at].1).then_some(at)
    }

    fn contains(&self, i: usize) -> bool {
        self.find(i).is_some()
    }

    /// The least index at or above `i`.
    fn at_or_above(&self, i: usize) -> Option<usize> {
        let at = self.spans.partition_point(|r| r.1 <= i);
        self.spans.get(at).map(|r| r.0.max(i))
    }

    /// The greatest index at or below `i`.
    fn at_or_below(&self, i: usize) -> Option<usize> {
        let at = self.spans.partition_point(|r| r.0 <= i).checked_sub(1)?;
        Some((self.spans[at].1 - 1).min(i))
    }

    /// The indexes in `start..end`, as ranges clipped to it.
    fn within(&self, start: usize, end: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let from = self.spans.partition_point(|r| r.1 <= start);
        self.spans[from..]
            .iter()
            .take_while(move |r| r.0 < end)
            .map(move |&(s, e)| (s.max(start), e.min(end)))
            .filter(|&(s, e)| s < e)
    }

    fn insert(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        // Ranges that overlap or touch the new one merge with it.
        let from = self.spans.partition_point(|r| r.1 < start);
        let to = self.spans.partition_point(|r| r.0 <= end);
        let (mut s, mut e) = (start, end);
        if from < to {
            s = s.min(self.spans[from].0);
            e = e.max(self.spans[to - 1].1);
        }
        let merged: usize = self.spans[from..to].iter().map(|&(s, e)| e - s).sum();
        self.count = self.count - merged + (e - s);
        self.spans.splice(from..to, [(s, e)]);
    }

    fn remove(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let from = self.spans.partition_point(|r| r.1 <= start);
        let to = self.spans.partition_point(|r| r.0 < end);
        if from >= to {
            return;
        }
        let mut keep = Vec::with_capacity(2);
        if self.spans[from].0 < start {
            keep.push((self.spans[from].0, start));
        }
        if self.spans[to - 1].1 > end {
            keep.push((end, self.spans[to - 1].1));
        }
        let dropped: usize = self.spans[from..to].iter().map(|&(s, e)| e - s).sum();
        let kept: usize = keep.iter().map(|&(s, e)| e - s).sum();
        self.count = self.count - dropped + kept;
        self.spans.splice(from..to, keep);
    }

    /// `-shiftIndexesStartingAtIndex:by:`: indexes from `index` on move by
    /// `delta`. Moving down, they replace the indexes they land on, and any
    /// that would go below zero are dropped. The caller checks that none
    /// would reach `NSNotFound`.
    fn shift(&mut self, index: usize, delta: NSInteger) {
        let mut below = Ranges::default();
        let mut above = Vec::new();
        for &(s, e) in &self.spans {
            if e <= index {
                below.insert(s, e);
            } else if s >= index {
                above.push((s, e));
            } else {
                below.insert(s, index);
                above.push((index, e));
            }
        }
        let magnitude = delta.unsigned_abs();
        if delta < 0 {
            below.remove(index.saturating_sub(magnitude), index);
        }
        for (s, e) in above {
            let (s, e) = if delta < 0 {
                (s.saturating_sub(magnitude), e.saturating_sub(magnitude))
            } else {
                (s + magnitude, e + magnitude)
            };
            below.insert(s, e);
        }
        *self = below;
    }

    fn clear(&mut self) {
        self.spans.clear();
        self.count = 0;
    }
}

/// The indexes of `spans` in order, or reversed, one at a time.
pub(crate) fn walk(spans: &[(usize, usize)], reverse: bool) -> impl Iterator<Item = usize> + '_ {
    let forward = (!reverse).then(|| spans.iter().flat_map(|&(s, e)| s..e));
    let backward = reverse.then(|| spans.iter().rev().flat_map(|&(s, e)| (s..e).rev()));
    forward.into_iter().flatten().chain(backward.into_iter().flatten())
}

pub(crate) struct MutableIndexSetIvars {
    ranges: Guarded<Ranges>,
}

/// Which storage an index set object has.
enum Kind<'a> {
    Fixed(&'a NSIndexSetImpl),
    Mutable(&'a NSMutableIndexSetImpl),
    /// Not one of Sidestep's index sets, reached through messages.
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
        inherited(obj)
    }
}

/// The storage of a subclass instance: the one it inherits.
#[cold]
fn inherited(obj: &AnyObject) -> Kind<'_> {
    if inherits(obj, &crate::NSMUTABLEINDEXSET) {
        // SAFETY: an instance of a subclass of NSMutableIndexSetImpl, whose
        // ivars sit where the superclass's methods expect them, set by the
        // initializer it inherits.
        Kind::Mutable(unsafe { &*(obj as *const AnyObject).cast::<NSMutableIndexSetImpl>() })
    } else if inherits(obj, &crate::NSINDEXSET) {
        // SAFETY: as above, for NSIndexSetImpl.
        Kind::Fixed(unsafe { &*(obj as *const AnyObject).cast::<NSIndexSetImpl>() })
    } else {
        Kind::Foreign
    }
}

/// An index set's ranges: borrowed, or gathered by message.
enum Borrowed<'a> {
    Own(&'a Ranges),
    Gathered(Ranges),
}

impl std::ops::Deref for Borrowed<'_> {
    type Target = Ranges;

    fn deref(&self) -> &Ranges {
        match self {
            Borrowed::Own(r) => r,
            Borrowed::Gathered(r) => r,
        }
    }
}

/// The ranges of any index set.
///
/// # Safety
/// No other code may run while the result is in use, as for
/// `Guarded::peek`. (Gathering the ranges of a foreign object sends it
/// messages, before this returns.)
unsafe fn ranges(obj: &AnyObject) -> Borrowed<'_> {
    match kind(obj) {
        Kind::Fixed(s) => Borrowed::Own(s.ivars()),
        // SAFETY: guaranteed by the caller.
        Kind::Mutable(m) => Borrowed::Own(unsafe { m.ivars().ranges.peek() }),
        Kind::Foreign => Borrowed::Gathered(gather(obj)),
    }
}

/// The indexes of an object that is not one of Sidestep's index sets.
fn gather(obj: &AnyObject) -> Ranges {
    let mut indexes = Vec::new();
    // SAFETY: NSIndexSet's methods take and return NSUInteger.
    let mut i: NSUInteger = unsafe { msg_send![obj, firstIndex] };
    while i != NSNotFound as NSUInteger {
        indexes.push(i);
        // SAFETY: as above.
        i = unsafe { msg_send![obj, indexGreaterThanIndex: i] };
    }
    Ranges::of_indexes(indexes)
}

/// A copy of any index set's ranges, as `(start, end)` pairs.
pub(crate) fn spans(obj: &AnyObject) -> Vec<(usize, usize)> {
    // SAFETY: copying runs no other code.
    unsafe { ranges(obj) }.spans.clone()
}

/// How many indexes any index set holds.
pub(crate) fn count(obj: &AnyObject) -> usize {
    // SAFETY: reading the count runs no other code.
    unsafe { ranges(obj) }.count()
}

/// A copy of any index set's ranges.
fn copy_of(obj: &AnyObject) -> Ranges {
    // SAFETY: copying runs no other code.
    unsafe { ranges(obj) }.clone()
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
    let this = this.set_ivars(MutableIndexSetIvars { ranges: Guarded::new(ranges) });
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

/// `range` as half-open bounds, clamped at the largest index.
fn bounds(range: NSRange) -> (usize, usize) {
    (range.location, range.location.saturating_add(range.length))
}

type IndexBlock = DynBlock<dyn Fn(NSUInteger, NonNull<Bool>)>;
type IndexTest = DynBlock<dyn Fn(NSUInteger, NonNull<Bool>) -> Bool>;
type RangeBlock = DynBlock<dyn Fn(NSRange, NonNull<Bool>)>;

/// The ranges of any index set within `start..end`, copied: blocks called
/// with them may change the set.
fn spans_within(obj: &AnyObject, (start, end): (usize, usize)) -> Vec<(usize, usize)> {
    // SAFETY: copying runs no other code.
    unsafe { ranges(obj) }.within(start, end).collect()
}

fn enumerate(obj: &AnyObject, range: (usize, usize), options: NSEnumerationOptions, block: &IndexBlock) {
    let spans = spans_within(obj, range);
    let mut stop = Bool::NO;
    for i in walk(&spans, options.contains(NSEnumerationOptions::Reverse)) {
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
    let spans = spans_within(obj, range);
    let mut passed = Vec::new();
    let mut stop = Bool::NO;
    for i in walk(&spans, options.contains(NSEnumerationOptions::Reverse)) {
        if test.call((i, NonNull::from(&mut stop))).as_bool() {
            passed.push(i);
            if first {
                break;
            }
        }
        if stop.as_bool() {
            break;
        }
    }
    Ranges::of_indexes(passed)
}

fn first_passing(
    obj: &AnyObject,
    range: (usize, usize),
    options: NSEnumerationOptions,
    test: &IndexTest,
) -> NSUInteger {
    not_found(passing(obj, range, options, test, true).first())
}

fn enumerate_ranges(obj: &AnyObject, range: (usize, usize), options: NSEnumerationOptions, block: &RangeBlock) {
    let mut spans = spans_within(obj, range);
    if options.contains(NSEnumerationOptions::Reverse) {
        spans.reverse();
    }
    let mut stop = Bool::NO;
    for (s, e) in spans {
        block.call((NSRange::new(s, e - s), NonNull::from(&mut stop)));
        if stop.as_bool() {
            break;
        }
    }
}

/// `<NSIndexSet: 0x…>[number of indexes: 5 (in 3 ranges), indexes: (1-3 5 10)]`.
fn description(obj: &AnyObject) -> Retained<NSString> {
    use std::fmt::Write;
    let mut out = format!("<{}: {:p}>", obj.class().name().to_string_lossy(), obj);
    {
        // SAFETY: formatting runs no other code.
        let ranges = unsafe { ranges(obj) };
        if ranges.spans.is_empty() {
            out.push_str("(no indexes)");
        } else {
            let _ =
                write!(out, "[number of indexes: {} (in {} ranges), indexes: (", ranges.count(), ranges.spans.len());
            for (n, &(s, e)) in ranges.spans.iter().enumerate() {
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
    }
    NSString::from_str(&out)
}

/// A reading of `obj`'s ranges that runs no other code.
fn with<R>(obj: &AnyObject, f: impl FnOnce(&Ranges) -> R) -> R {
    // SAFETY: the readers passed here only compute with the ranges.
    f(&*unsafe { ranges(obj) })
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
            init_fixed(this, copy_of(other))
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            with(self, Ranges::count)
        }

        #[unsafe(method(containsIndex:))]
        fn contains_index(&self, index: NSUInteger) -> bool {
            with(self, |r| r.contains(index))
        }

        #[unsafe(method(firstIndex))]
        fn first_index(&self) -> NSUInteger {
            not_found(with(self, Ranges::first))
        }

        #[unsafe(method(lastIndex))]
        fn last_index(&self) -> NSUInteger {
            not_found(with(self, Ranges::last))
        }

        #[unsafe(method(indexGreaterThanIndex:))]
        fn index_greater_than(&self, index: NSUInteger) -> NSUInteger {
            not_found(index.checked_add(1).and_then(|i| with(self, |r| r.at_or_above(i))))
        }

        #[unsafe(method(indexGreaterThanOrEqualToIndex:))]
        fn index_greater_than_or_equal(&self, index: NSUInteger) -> NSUInteger {
            not_found(with(self, |r| r.at_or_above(index)))
        }

        #[unsafe(method(indexLessThanIndex:))]
        fn index_less_than(&self, index: NSUInteger) -> NSUInteger {
            not_found(index.checked_sub(1).and_then(|i| with(self, |r| r.at_or_below(i))))
        }

        #[unsafe(method(indexLessThanOrEqualToIndex:))]
        fn index_less_than_or_equal(&self, index: NSUInteger) -> NSUInteger {
            not_found(with(self, |r| r.at_or_below(index)))
        }

        /// Fills `buffer` with up to `capacity` indexes from `*range` (or
        /// from all of them), and moves `*range` past the last one given.
        #[unsafe(method(getIndexes:maxCount:inIndexRange:))]
        fn get_indexes(&self, buffer: NonNull<NSUInteger>, capacity: NSUInteger, range: *mut NSRange) -> NSUInteger {
            // SAFETY: the caller passes a valid range pointer or null.
            let (start, end) = match unsafe { range.as_ref() } {
                Some(r) => bounds(*r),
                None => (0, usize::MAX),
            };
            let spans = spans_within(self, (start, end));
            let mut n = 0;
            // What is left starts after the last index given, as in
            // Foundation.
            let mut next = end;
            for i in walk(&spans, false).take(capacity) {
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
            let (start, end) = bounds(range);
            with(self, |r| r.within(start, end).map(|(s, e)| e - s).sum())
        }

        #[unsafe(method(containsIndexesInRange:))]
        fn contains_indexes_in_range(&self, range: NSRange) -> bool {
            let (start, end) = bounds(range);
            range.length > 0 && with(self, |r| r.find(start).is_some_and(|at| r.spans[at].1 >= end))
        }

        #[unsafe(method(containsIndexes:))]
        fn contains_indexes(&self, other: &NSIndexSet) -> bool {
            let theirs = spans(other);
            with(self, |mine| theirs.iter().all(|&(s, e)| mine.find(s).is_some_and(|at| mine.spans[at].1 >= e)))
        }

        #[unsafe(method(intersectsIndexesInRange:))]
        fn intersects_indexes_in_range(&self, range: NSRange) -> bool {
            let (start, end) = bounds(range);
            with(self, |r| r.within(start, end).next().is_some())
        }

        #[unsafe(method(isEqualToIndexSet:))]
        fn is_equal_to_index_set(&self, other: &NSIndexSet) -> bool {
            let theirs = copy_of(other);
            with(self, |mine| *mine == theirs)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.is_some_and(|other| {
                util::is_kind(other, <NSIndexSet as objc2::ClassType>::class()) && {
                    let theirs = copy_of(other);
                    with(self, |mine| *mine == theirs)
                }
            })
        }

        /// The first and last index and the count: equal sets agree.
        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            with(self, |r| match (r.first(), r.last()) {
                (Some(first), Some(last)) => first.wrapping_add(last).wrapping_add(r.count()),
                _ => 0,
            })
        }

        #[unsafe(method(enumerateIndexesUsingBlock:))]
        fn enumerate_indexes(&self, block: &IndexBlock) {
            enumerate(self, (0, usize::MAX), NSEnumerationOptions(0), block);
        }

        /// Concurrent enumeration runs on the calling thread, one index at
        /// a time, which the option permits.
        #[unsafe(method(enumerateIndexesWithOptions:usingBlock:))]
        fn enumerate_indexes_with_options(&self, options: NSEnumerationOptions, block: &IndexBlock) {
            enumerate(self, (0, usize::MAX), options, block);
        }

        #[unsafe(method(enumerateIndexesInRange:options:usingBlock:))]
        fn enumerate_indexes_in_range(&self, range: NSRange, options: NSEnumerationOptions, block: &IndexBlock) {
            enumerate(self, bounds(range), options, block);
        }

        #[unsafe(method(indexPassingTest:))]
        fn index_passing_test(&self, test: &IndexTest) -> NSUInteger {
            first_passing(self, (0, usize::MAX), NSEnumerationOptions(0), test)
        }

        #[unsafe(method(indexWithOptions:passingTest:))]
        fn index_with_options_passing_test(&self, options: NSEnumerationOptions, test: &IndexTest) -> NSUInteger {
            first_passing(self, (0, usize::MAX), options, test)
        }

        #[unsafe(method(indexInRange:options:passingTest:))]
        fn index_in_range_passing_test(&self, range: NSRange, options: NSEnumerationOptions, test: &IndexTest) -> NSUInteger {
            first_passing(self, bounds(range), options, test)
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
            make(passing(self, bounds(range), options, test, false))
        }

        #[unsafe(method(enumerateRangesUsingBlock:))]
        fn enumerate_ranges(&self, block: &RangeBlock) {
            enumerate_ranges(self, (0, usize::MAX), NSEnumerationOptions(0), block);
        }

        #[unsafe(method(enumerateRangesWithOptions:usingBlock:))]
        fn enumerate_ranges_with_options(&self, options: NSEnumerationOptions, block: &RangeBlock) {
            enumerate_ranges(self, (0, usize::MAX), options, block);
        }

        #[unsafe(method(enumerateRangesInRange:options:usingBlock:))]
        fn enumerate_ranges_in_range(&self, range: NSRange, options: NSEnumerationOptions, block: &RangeBlock) {
            enumerate_ranges(self, bounds(range), options, block);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSIndexSet> {
            if is_exactly(self, &crate::NSINDEXSET) {
                // Immutable: a copy is the same object.
                // SAFETY: NSIndexSetImpl is the class registered as
                // NSIndexSet.
                unsafe { Retained::cast_unchecked(self.retain()) }
            } else {
                make(copy_of(self))
            }
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableIndexSet> {
            make_mutable(copy_of(self))
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            description(self)
        }
    }

    unsafe impl NSObjectProtocol for NSIndexSetImpl {}
);

impl NSMutableIndexSetImpl {
    /// Change the ranges with `f`, which runs no other code.
    fn change<R>(&self, f: impl FnOnce(&mut Ranges) -> R) -> R {
        // SAFETY: `f` only computes with the ranges; blocks that could
        // change the set run on copies of them.
        f(unsafe { self.ivars().ranges.write("NSMutableIndexSet", (self as *const Self).cast()) })
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
            init_mutable(this, copy_of(other))
        }

        #[unsafe(method(count))]
        fn count(&self) -> NSUInteger {
            // SAFETY: reading the count runs no other code.
            unsafe { self.ivars().ranges.peek() }.count()
        }

        #[unsafe(method(containsIndex:))]
        fn contains_index(&self, index: NSUInteger) -> bool {
            // SAFETY: a binary search runs no other code.
            unsafe { self.ivars().ranges.peek() }.contains(index)
        }

        #[unsafe(method(addIndex:))]
        fn add_index(&self, index: NSUInteger) {
            let (s, e) = checked(NSRange::new(index, 1), "NSMutableIndexSet", "addIndexesInRange:");
            self.change(|r| r.insert(s, e));
        }

        #[unsafe(method(addIndexesInRange:))]
        fn add_indexes_in_range(&self, range: NSRange) {
            let (s, e) = checked(range, "NSMutableIndexSet", "addIndexesInRange:");
            self.change(|r| r.insert(s, e));
        }

        #[unsafe(method(addIndexes:))]
        fn add_indexes(&self, other: &NSIndexSet) {
            // Copied first: `other` may be this set.
            let theirs = spans(other);
            self.change(|mine| {
                for (s, e) in theirs {
                    mine.insert(s, e);
                }
            });
        }

        #[unsafe(method(removeIndex:))]
        fn remove_index(&self, index: NSUInteger) {
            self.change(|r| r.remove(index, index.saturating_add(1)));
        }

        #[unsafe(method(removeIndexesInRange:))]
        fn remove_indexes_in_range(&self, range: NSRange) {
            let (start, end) = bounds(range);
            self.change(|r| r.remove(start, end));
        }

        #[unsafe(method(removeIndexes:))]
        fn remove_indexes(&self, other: &NSIndexSet) {
            // Copied first: `other` may be this set.
            let theirs = spans(other);
            self.change(|mine| {
                for (s, e) in theirs {
                    mine.remove(s, e);
                }
            });
        }

        #[unsafe(method(removeAllIndexes))]
        fn remove_all_indexes(&self) {
            self.change(Ranges::clear);
        }

        /// Fails, as Foundation does, if the last index would reach
        /// `NSNotFound`, whether or not it is one that moves.
        #[unsafe(method(shiftIndexesStartingAtIndex:by:))]
        fn shift_indexes(&self, index: NSUInteger, delta: NSInteger) {
            if let (Some(last), Ok(up)) = (with(self, Ranges::last), usize::try_from(delta))
                && last.checked_add(up).is_none_or(|moved| moved >= LIMIT)
            {
                panic!(
                    "*** -[NSMutableIndexSet shiftIndexesStartingAtIndex:by:]: Incrementing by {delta} would push \
                     last index beyond maximum index value of NSNotFound - 1"
                );
            }
            self.change(|r| r.shift(index, delta));
        }
    }

    unsafe impl NSObjectProtocol for NSMutableIndexSetImpl {}
);

#[cfg(test)]
mod tests {
    use super::{Ranges, walk};

    fn of(ranges: &[(usize, usize)]) -> Ranges {
        let mut r = Ranges::default();
        for &(s, e) in ranges {
            r.insert(s, e);
        }
        r
    }

    fn total(r: &Ranges) -> usize {
        r.spans.iter().map(|&(s, e)| e - s).sum()
    }

    #[test]
    fn inserting_merges_touching_ranges() {
        assert_eq!(of(&[(1, 3), (3, 4), (6, 7)]).spans, [(1, 4), (6, 7)]);
        assert_eq!(of(&[(6, 7), (1, 2), (2, 7)]).spans, [(1, 7)]);
        assert_eq!(of(&[(0, 10), (2, 3)]).spans, [(0, 10)]);
        assert_eq!(of(&[(5, 6), (1, 2), (3, 4)]).spans, [(1, 2), (3, 4), (5, 6)]);
        for r in [of(&[(1, 3), (3, 4), (6, 7)]), of(&[(6, 7), (1, 2), (2, 7)]), of(&[(0, 10), (2, 3)])] {
            assert_eq!(r.count(), total(&r));
        }
    }

    #[test]
    fn removing_splits_ranges() {
        let mut r = of(&[(0, 10), (20, 30)]);
        r.remove(5, 25);
        assert_eq!(r.spans, [(0, 5), (25, 30)]);
        assert_eq!(r.count(), 10);
        r.remove(2, 3);
        assert_eq!(r.spans, [(0, 2), (3, 5), (25, 30)]);
        assert_eq!(r.count(), 9);
        r.remove(0, 100);
        assert!(r.spans.is_empty());
        assert_eq!(r.count(), 0);
    }

    #[test]
    fn shifting_matches_foundation() {
        let mut r = of(&[(0, 10)]);
        r.shift(5, -2);
        assert_eq!(r.spans, [(0, 8)]);
        assert_eq!(r.count(), 8);
        let mut r = of(&[(0, 3), (7, 8)]);
        r.shift(1, 3);
        assert_eq!(r.spans, [(0, 1), (4, 6), (10, 11)]);
        assert_eq!(r.count(), 4);
        let mut r = of(&[(2, 3)]);
        r.shift(1, -5);
        assert!(r.spans.is_empty());
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
        assert_eq!(r.within(3, 3).count(), 0, "an empty range holds nothing");
        assert_eq!(r.within(4, 5).count(), 0);
    }

    #[test]
    fn indexes_in_any_order() {
        let descending: Vec<usize> = (0..10_000).rev().step_by(2).collect();
        let r = Ranges::of_indexes(descending);
        assert_eq!(r.count(), 5000);
        assert_eq!(r.spans.len(), 5000);
        assert_eq!(Ranges::of_indexes(vec![3, 1, 2, 2, 7]).spans, [(1, 4), (7, 8)]);
        let spans = [(1, 3), (5, 6)];
        assert_eq!(walk(&spans, false).collect::<Vec<_>>(), [1, 2, 5]);
        assert_eq!(walk(&spans, true).collect::<Vec<_>>(), [5, 2, 1]);
    }
}
