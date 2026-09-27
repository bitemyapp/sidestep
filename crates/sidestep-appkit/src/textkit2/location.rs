//! Locations and ranges: `NSTextLocation` objects and `NSTextRange`.
//!
//! A content storage counts its locations in UTF-16 units from the start
//! of its text, as `NSCountableTextLocation`s (the class AppKit's content
//! storage vends too, as measured on macOS): an immutable offset that
//! compares, hashes and describes itself by its number ("12"). A range
//! holds two locations, its end no earlier than its start; ranges of
//! countable locations keep their offsets as well, so comparing, containing
//! and intersecting them costs no messages. Locations of other classes (a
//! content manager of a program's own) are compared with `compare:`.
//!
//! Measured on macOS (`conformance/tests/textkit2.rs`): a range whose end
//! comes before its start is nil; a range contains its start but not its
//! end, and an empty range contains nothing (it is contained in a range
//! whose start it sits at, not in one whose end it sits at); ranges that
//! only touch don't intersect, and their intersection is nil; a union
//! spans the gap between ranges; the description is "start...end".

use std::cmp::Ordering;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_app_kit::NSTextRange;
use objc2_foundation::{NSComparisonResult, NSString, NSZone};

sidestep_runtime::static_class!(pub(crate) NSTEXTRANGE, NSTEXTRANGE_META = "NSTextRange", || {
    let _ = NSTextRangeImpl::class();
});

define_class!(
    /// A location counted in UTF-16 units from the start of a text.
    #[unsafe(super(NSObject))]
    #[name = "NSCountableTextLocation"]
    #[ivars = usize]
    pub(crate) struct CountableLocation;

    impl CountableLocation {
        #[unsafe(method(compare:))]
        fn compare(&self, other: &AnyObject) -> NSComparisonResult {
            match offset_of(other) {
                Some(o) => result(self.ivars().cmp(&o)),
                // Another kind of location: its own order, reversed.
                None => {
                    // SAFETY: compare: takes a location and returns a
                    // comparison result.
                    let r: NSComparisonResult = unsafe { msg_send![other, compare: self] };
                    result(ordering(r).reverse())
                }
            }
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(offset_of).is_some_and(|o| o == *self.ivars())
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            *self.ivars()
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().to_string())
        }

        #[unsafe(method(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> *mut Self {
            // Immutable: a copy is the same location.
            Retained::into_raw(objc2::Message::retain(self))
        }
    }

    unsafe impl NSObjectProtocol for CountableLocation {}
);

/// The location `offset` units from the start of a text.
pub(crate) fn location(offset: usize) -> Retained<AnyObject> {
    let this = CountableLocation::alloc().set_ivars(offset);
    // SAFETY: NSObject's initializer.
    let this: Retained<CountableLocation> = unsafe { msg_send![super(this), init] };
    Retained::into_super(Retained::into_super(this))
}

/// The offset of a countable location; `None` for another kind.
pub(crate) fn offset_of(location: &AnyObject) -> Option<usize> {
    location.downcast_ref::<CountableLocation>().map(|l| *l.ivars())
}

fn result(o: Ordering) -> NSComparisonResult {
    match o {
        Ordering::Less => NSComparisonResult::Ascending,
        Ordering::Equal => NSComparisonResult::Same,
        Ordering::Greater => NSComparisonResult::Descending,
    }
}

fn ordering(r: NSComparisonResult) -> Ordering {
    match r {
        NSComparisonResult::Ascending => Ordering::Less,
        NSComparisonResult::Descending => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

/// How location `a` compares with `b`: by offset for countable ones, else
/// by `compare:`.
pub(crate) fn compare(a: &AnyObject, b: &AnyObject) -> Ordering {
    if let (Some(x), Some(y)) = (offset_of(a), offset_of(b)) {
        return x.cmp(&y);
    }
    // SAFETY: compare: takes a location and returns a comparison result.
    let r: NSComparisonResult = unsafe { msg_send![a, compare: b] };
    ordering(r)
}

pub(crate) struct RangeIvars {
    start: Retained<AnyObject>,
    end: Retained<AnyObject>,
    /// Both ends' offsets, when both are countable.
    span: Option<(usize, usize)>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextRange"]
    #[ivars = RangeIvars]
    pub(crate) struct NSTextRangeImpl;

    impl NSTextRangeImpl {
        #[unsafe(method_id(initWithLocation:endLocation:))]
        fn init_with_location_end(
            this: Allocated<Self>,
            start: &AnyObject,
            end: Option<&AnyObject>,
        ) -> Option<Retained<Self>> {
            let end = end.unwrap_or(start);
            if compare(end, start) == Ordering::Less {
                None
            } else {
                let span = offset_of(start).zip(offset_of(end));
                let this = this.set_ivars(RangeIvars {
                    start: objc2::Message::retain(start),
                    end: objc2::Message::retain(end),
                    span,
                });
                // SAFETY: NSObject's initializer.
                Some(unsafe { msg_send![super(this), init] })
            }
        }

        #[unsafe(method_id(initWithLocation:))]
        fn init_with_location(this: Allocated<Self>, start: &AnyObject) -> Retained<Self> {
            let span = offset_of(start).map(|o| (o, o));
            let this = this.set_ivars(RangeIvars {
                start: objc2::Message::retain(start),
                end: objc2::Message::retain(start),
                span,
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let zero = location(0);
            // SAFETY: the initializer above, with a location.
            unsafe { msg_send![this, initWithLocation: &*zero] }
        }

        #[unsafe(method(isEmpty))]
        fn is_empty(&self) -> bool {
            let iv = self.ivars();
            match iv.span {
                Some((a, b)) => a == b,
                None => compare(&iv.start, &iv.end) == Ordering::Equal,
            }
        }

        #[unsafe(method_id(location))]
        fn location(&self) -> Retained<AnyObject> {
            self.ivars().start.clone()
        }

        #[unsafe(method_id(endLocation))]
        fn end_location(&self) -> Retained<AnyObject> {
            self.ivars().end.clone()
        }

        #[unsafe(method(isEqualToTextRange:))]
        fn is_equal_to_text_range(&self, other: &NSTextRange) -> bool {
            equal(self.as_range(), other)
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSTextRange>()).is_some_and(|o| equal(self.as_range(), o))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            match self.ivars().span {
                Some((a, b)) => a.rotate_left(16) ^ b,
                None => {
                    // SAFETY: hash takes nothing.
                    let h: usize = unsafe { msg_send![&*self.ivars().start, hash] };
                    h
                }
            }
        }

        #[unsafe(method(containsLocation:))]
        fn contains_location(&self, location: &AnyObject) -> bool {
            let iv = self.ivars();
            match (iv.span, offset_of(location)) {
                (Some((a, b)), Some(o)) => a <= o && o < b,
                _ => compare(&iv.start, location) != Ordering::Greater && compare(location, &iv.end) == Ordering::Less,
            }
        }

        #[unsafe(method(containsRange:))]
        fn contains_range(&self, other: &NSTextRange) -> bool {
            let iv = self.ivars();
            // Its start is in this range, and its end no further.
            match (iv.span, span_of(other)) {
                (Some((a, b)), Some((s, e))) => a <= s && s < b && e <= b,
                _ => {
                    let (s, e) = ends(other);
                    let start_in =
                        compare(&iv.start, &s) != Ordering::Greater && compare(&s, &iv.end) == Ordering::Less;
                    start_in && compare(&e, &iv.end) != Ordering::Greater
                }
            }
        }

        #[unsafe(method(intersectsWithTextRange:))]
        fn intersects_with_text_range(&self, other: &NSTextRange) -> bool {
            overlap(self.ivars(), other)
        }

        #[unsafe(method_id(textRangeByIntersectingWithTextRange:))]
        fn text_range_by_intersecting(&self, other: &NSTextRange) -> Option<Retained<NSTextRange>> {
            let (s, e) = ends(other);
            let iv = self.ivars();
            if overlap(iv, other) {
                let start = if compare(&iv.start, &s) == Ordering::Less { s } else { iv.start.clone() };
                let end = if compare(&iv.end, &e) == Ordering::Greater { e } else { iv.end.clone() };
                new_range(&start, &end)
            } else {
                None
            }
        }

        #[unsafe(method_id(textRangeByFormingUnionWithTextRange:))]
        fn text_range_by_forming_union(&self, other: &NSTextRange) -> Retained<NSTextRange> {
            let (s, e) = ends(other);
            let iv = self.ivars();
            let start = if compare(&s, &iv.start) == Ordering::Less { s } else { iv.start.clone() };
            let end = if compare(&e, &iv.end) == Ordering::Greater { e } else { iv.end.clone() };
            new_range(&start, &end).expect("a union ends after it starts")
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let iv = self.ivars();
            let text = match iv.span {
                Some((a, b)) => format!("{a}...{b}"),
                None => {
                    let d = |o: &AnyObject| {
                        // SAFETY: description takes nothing and returns a
                        // string.
                        let s: Retained<NSString> = unsafe { msg_send![o, description] };
                        s.to_string()
                    };
                    format!("{}...{}", d(&iv.start), d(&iv.end))
                }
            };
            NSString::from_str(&text)
        }
    }

    unsafe impl NSObjectProtocol for NSTextRangeImpl {}
);

impl NSTextRangeImpl {
    fn as_range(&self) -> &NSTextRange {
        // SAFETY: NSTextRange is this class.
        unsafe { &*(self as *const Self).cast::<NSTextRange>() }
    }
}

/// A range's ends: from the instance variables for Sidestep's class (and
/// subclasses keeping the accessors), else by message.
fn ends(r: &NSTextRange) -> (Retained<AnyObject>, Retained<AnyObject>) {
    if let Some(iv) = own(r) {
        return (iv.start.clone(), iv.end.clone());
    }
    // SAFETY: location and endLocation take nothing and return locations.
    unsafe { (msg_send![r, location], msg_send![r, endLocation]) }
}

fn own(r: &NSTextRange) -> Option<&RangeIvars> {
    let ours = <NSTextRange as ClassType>::class();
    // SAFETY: an instance of the class or a subclass.
    crate::textkit::is_kind(r.class(), ours)
        .then(|| unsafe { &*(r as *const NSTextRange).cast::<NSTextRangeImpl>() }.ivars())
}

/// A range's offsets, when both ends are countable.
pub(crate) fn span_of(r: &NSTextRange) -> Option<(usize, usize)> {
    match own(r) {
        Some(iv) => iv.span,
        None => {
            let (s, e) = ends(r);
            offset_of(&s).zip(offset_of(&e))
        }
    }
}

/// Whether two ranges share a location: neither empty, each starting
/// before the other ends.
fn overlap(iv: &RangeIvars, other: &NSTextRange) -> bool {
    let (s, e) = ends(other);
    let empty = |a: &AnyObject, b: &AnyObject| compare(a, b) != Ordering::Less;
    !empty(&iv.start, &iv.end)
        && !empty(&s, &e)
        && compare(&iv.start, &e) == Ordering::Less
        && compare(&s, &iv.end) == Ordering::Less
}

fn equal(a: &NSTextRange, b: &NSTextRange) -> bool {
    if let (Some(x), Some(y)) = (span_of(a), span_of(b)) {
        return x == y;
    }
    let (s0, e0) = ends(a);
    let (s1, e1) = ends(b);
    compare(&s0, &s1) == Ordering::Equal && compare(&e0, &e1) == Ordering::Equal
}

/// A new range from `start` to `end` (nil if `end` comes first).
pub(crate) fn new_range(start: &AnyObject, end: &AnyObject) -> Option<Retained<NSTextRange>> {
    crate::load_shell::<NSTextRange>();
    // SAFETY: the designated initializer, with two locations.
    unsafe { msg_send![NSTextRange::alloc(), initWithLocation: start, endLocation: end] }
}

/// The range of offsets `a..b`, in countable locations.
pub(crate) fn range(a: usize, b: usize) -> Retained<NSTextRange> {
    new_range(&location(a), &location(b.max(a))).expect("a range ends after it starts")
}
