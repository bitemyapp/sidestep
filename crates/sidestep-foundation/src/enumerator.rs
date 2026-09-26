//! `NSEnumerator` over Sidestep's collections, and the fast enumeration
//! (`countByEnumeratingWithState:objects:count:`) they all answer.
//!
//! An enumerator retains its collection and walks it by position, so it
//! reads a mutable collection as it is at each step, as Foundation's do.
//! Fast enumeration hands out a collection's own storage when its elements
//! lie side by side (arrays), or copies batches into the caller's buffer
//! (dictionaries, sets, enumerators). Either way `mutationsPtr` points at
//! the collection's mutation count, which every change to a mutable
//! collection bumps, so objc2's iterators (and compiled `for ... in`
//! loops) see a mutation before they read an element it may have freed.

use std::cell::Cell;
use std::ffi::c_ulong;
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSEnumerator, NSFastEnumerationState, NSUInteger};

use crate::util::is_exactly;
use crate::{array, dictionary, set};

/// The mutation count of a collection that never changes.
static IMMUTABLE: c_ulong = 0;

/// Where fast enumeration of an immutable collection points
/// `mutationsPtr`. Readers only ever read through it.
pub(crate) fn immutable_mutations() -> *mut c_ulong {
    (&raw const IMMUTABLE).cast_mut()
}

/// A mutable collection's count of changes.
#[derive(Default)]
pub(crate) struct Mutations(Cell<c_ulong>);

impl Mutations {
    pub(crate) fn bump(&self) {
        self.0.set(self.0.get().wrapping_add(1));
    }

    pub(crate) fn as_ptr(&self) -> *mut c_ulong {
        self.0.as_ptr()
    }
}

/// Fast enumeration over elements that lie side by side: all of them in
/// one batch, straight from the collection's storage.
///
/// # Safety
/// `state` must be valid, and `items` must point to `count` object
/// pointers that stay put until the collection next changes.
pub(crate) unsafe fn whole(
    state: NonNull<NSFastEnumerationState>,
    items: *const *mut AnyObject,
    count: usize,
    mutations: *mut c_ulong,
) -> NSUInteger {
    // SAFETY: guaranteed by the caller.
    let state = unsafe { &mut *state.as_ptr() };
    if state.state != 0 || count == 0 {
        return 0;
    }
    state.state = 1;
    state.itemsPtr = items.cast_mut();
    state.mutationsPtr = mutations;
    count
}

/// Fast enumeration by copying up to `len` elements into the caller's
/// buffer, continuing from where the last batch ended.
///
/// # Safety
/// `state` must be valid and `buffer` must have room for `len` pointers.
pub(crate) unsafe fn batch(
    state: NonNull<NSFastEnumerationState>,
    buffer: NonNull<*mut AnyObject>,
    len: usize,
    mutations: *mut c_ulong,
    mut element: impl FnMut(usize) -> Option<*mut AnyObject>,
) -> NSUInteger {
    // SAFETY: guaranteed by the caller.
    let state = unsafe { &mut *state.as_ptr() };
    let start = state.state as usize;
    let mut n = 0;
    while n < len {
        let Some(obj) = element(start + n) else { break };
        // SAFETY: `n < len`, within the caller's buffer.
        unsafe { buffer.as_ptr().add(n).write(obj) };
        n += 1;
    }
    state.state = (start + n) as c_ulong;
    state.itemsPtr = buffer.as_ptr();
    state.mutationsPtr = mutations;
    n
}

/// What an enumerator walks.
pub(crate) enum Source {
    Nothing,
    Array(Retained<AnyObject>),
    /// An array from its end; the count is the array's when enumeration
    /// began.
    ReverseArray(Retained<AnyObject>, usize),
    Keys(Retained<AnyObject>),
    Values(Retained<AnyObject>),
    Members(Retained<AnyObject>),
}

pub(crate) struct EnumeratorIvars {
    source: Source,
    /// How many elements have been handed out.
    taken: Cell<usize>,
}

impl EnumeratorIvars {
    /// The element after the last one handed out, unretained: the
    /// collection keeps it alive.
    fn next(&self) -> Option<*mut AnyObject> {
        let i = self.taken.get();
        let obj = match &self.source {
            Source::Nothing => None,
            Source::Array(a) => array::element_at(a, i),
            Source::ReverseArray(a, count) => count.checked_sub(i + 1).and_then(|at| array::element_at(a, at)),
            Source::Keys(d) => dictionary::entry_at(d, i).map(|(k, _)| k),
            Source::Values(d) => dictionary::entry_at(d, i).map(|(_, v)| v),
            Source::Members(s) => set::member_at(s, i),
        }?;
        self.taken.set(i + 1);
        Some(obj)
    }

    fn mutations(&self) -> *mut c_ulong {
        match &self.source {
            Source::Nothing => immutable_mutations(),
            Source::Array(a) | Source::ReverseArray(a, _) => array::mutations(a),
            Source::Keys(d) | Source::Values(d) => dictionary::mutations(d),
            Source::Members(s) => set::mutations(s),
        }
    }
}

/// A new enumerator over `source`.
pub(crate) fn make(source: Source) -> Retained<NSEnumerator> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSEnumerator::<AnyObject>::alloc();
    // SAFETY: NSEnumerator's class is NSEnumeratorImpl, and an `Allocated`
    // is a pointer to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSEnumerator>, Allocated<NSEnumeratorImpl>>(this) };
    let this = this.set_ivars(EnumeratorIvars { source, taken: Cell::new(0) });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSEnumeratorImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSEnumeratorImpl is the class registered as NSEnumerator.
    unsafe { Retained::cast_unchecked(this) }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSEnumerator"]
    #[ivars = EnumeratorIvars]
    pub(crate) struct NSEnumeratorImpl;

    impl NSEnumeratorImpl {
        /// For subclasses, which override `-nextObject`.
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(EnumeratorIvars { source: Source::Nothing, taken: Cell::new(0) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(nextObject))]
        fn next_object(&self) -> *mut AnyObject {
            self.ivars().next().unwrap_or(std::ptr::null_mut())
        }

        /// The elements not yet handed out; the enumerator is spent after.
        #[unsafe(method_id(allObjects))]
        fn all_objects(&self) -> Retained<NSArray> {
            let mut rest = Vec::new();
            if is_exactly(self, &crate::NSENUMERATOR) {
                while let Some(obj) = self.ivars().next() {
                    // SAFETY: the collection keeps the element alive.
                    rest.push(unsafe { Retained::retain(obj) }.expect("non-null"));
                }
            } else {
                // SAFETY: -nextObject takes nothing and returns an object or nil.
                while let Some(obj) = unsafe { msg_send![self, nextObject] } {
                    rest.push(obj);
                }
            }
            array::make(rest)
        }

        #[unsafe(method(countByEnumeratingWithState:objects:count:))]
        fn count_by_enumerating(
            &self,
            state: NonNull<NSFastEnumerationState>,
            buffer: NonNull<*mut AnyObject>,
            len: NSUInteger,
        ) -> NSUInteger {
            if is_exactly(self, &crate::NSENUMERATOR) {
                let ivars = self.ivars();
                // The enumerator keeps its own position; `state` only
                // records that enumeration has begun.
                // SAFETY: the caller passes a valid state and buffer.
                unsafe { batch(state, buffer, len, ivars.mutations(), |_| ivars.next()) }
            } else {
                // SAFETY: as above; -nextObject returns an object or nil,
                // kept alive by the subclass's collection.
                unsafe {
                    batch(state, buffer, len, immutable_mutations(), |_| {
                        let obj: *mut AnyObject = msg_send![self, nextObject];
                        (!obj.is_null()).then_some(obj)
                    })
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for NSEnumeratorImpl {}
);
