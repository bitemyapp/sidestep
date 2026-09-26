//! `NSEnumerator` over Sidestep's collections, and the fast enumeration
//! (`countByEnumeratingWithState:objects:count:`) they all answer.
//!
//! Enumerators follow what Foundation's do when their collection changes:
//!
//! - An array enumerator retains its array and walks it by position, up to
//!   the count the array had when the enumerator was made. So the array
//!   may change meanwhile: removing the element just handed out while
//!   enumerating in reverse is the classic case. Reading past the end of a
//!   reverse enumerator's shrunken array fails as `-objectAtIndex:` does.
//! - A dictionary or set enumerator hands out entries in batches of 16,
//!   retained. When it fetches the next batch, if the collection changed
//!   since the first and entries are left that the enumerator hasn't seen
//!   or that were added, it fails with Foundation's "was mutated while
//!   being enumerated"; otherwise it ends. So removing the entry just handed
//!   out works for a small collection, as it does in Foundation, and no
//!   change ever makes an enumerator skip entries silently.
//!
//! Fast enumeration hands out a collection's own storage when its elements
//! lie side by side (arrays), or copies batches into the caller's buffer
//! (dictionaries, sets, enumerators). `mutationsPtr` points at the
//! collection's mutation count, which every change to a mutable collection
//! bumps, so objc2's iterators (and compiled `for ... in` loops) see a
//! mutation before they read an element it may have freed. An array
//! enumerator hands out one element per call and a count that never
//! changes, since its array may change during the loop.

use std::cell::{Cell, RefCell};
use std::ffi::c_ulong;
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSArray, NSEnumerator, NSFastEnumerationState, NSUInteger};

use crate::util::{self, is_exactly};
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

/// Fast enumeration of a collection subclass defined outside Sidestep:
/// `gather` makes an array of its elements on the first call, which the
/// state keeps (autoreleased) for the rest of the loop.
///
/// # Safety
/// As for `batch`.
pub(crate) unsafe fn gathered(
    state: NonNull<NSFastEnumerationState>,
    buffer: NonNull<*mut AnyObject>,
    len: usize,
    gather: impl FnOnce() -> Retained<NSArray>,
) -> NSUInteger {
    // SAFETY: the caller passes a valid state.
    let st = unsafe { &mut *state.as_ptr() };
    if st.state == 0 {
        st.extra[0] = Retained::autorelease_ptr(gather()) as usize as c_ulong;
    }
    // SAFETY: the array, autoreleased into the caller's pool, outlives its
    // enumeration loop.
    let items = unsafe { &*(st.extra[0] as usize as *const AnyObject) };
    // SAFETY: guaranteed by the caller.
    unsafe { batch(state, buffer, len, immutable_mutations(), |i| array::element_at(items, i)) }
}

/// What an enumerator walks.
pub(crate) enum Source {
    Nothing,
    /// An array from its start; the count is the array's when the
    /// enumerator was made.
    Array(Retained<AnyObject>, usize),
    /// An array from its end; the count is the array's when the enumerator
    /// was made.
    ReverseArray(Retained<AnyObject>, usize),
    Keys(Retained<AnyObject>),
    Values(Retained<AnyObject>),
    Members(Retained<AnyObject>),
}

/// How many entries of a dictionary or set an enumerator retains at once.
const BATCH: usize = 16;

/// A dictionary or set enumerator's place.
#[derive(Default)]
struct Batch {
    /// Entries fetched, retained, and how many of them were handed out.
    items: Vec<Retained<AnyObject>>,
    next: usize,
    /// The collection's count of changes and entries at the first fetch.
    first: Option<(c_ulong, usize)>,
    /// Whether the enumerator has run out, for good.
    done: bool,
}

pub(crate) struct EnumeratorIvars {
    source: Source,
    /// How many elements have been handed out, or for a dictionary or set,
    /// fetched.
    taken: Cell<usize>,
    batch: RefCell<Batch>,
}

impl EnumeratorIvars {
    fn new(source: Source) -> Self {
        EnumeratorIvars { source, taken: Cell::new(0), batch: RefCell::default() }
    }

    /// The element after the last one handed out, unretained: the
    /// collection, or the enumerator's batch, keeps it alive until the
    /// next call.
    fn next(&self) -> Option<*mut AnyObject> {
        let i = self.taken.get();
        let obj = match &self.source {
            Source::Nothing => None,
            Source::Array(a, count) => {
                let obj = if i < *count { array::element_at(a, i) } else { None };
                if obj.is_none() {
                    // Spent for good, even if the array grows again.
                    self.taken.set(*count);
                }
                obj
            }
            Source::ReverseArray(a, count) => {
                let at = count.checked_sub(i + 1)?;
                match array::element_at(a, at) {
                    Some(obj) => Some(obj),
                    None => {
                        let (name, now) = (array_name(a), array::count_of(a));
                        util::index_out_of_bounds(name, "objectAtIndex:", at, now)
                    }
                }
            }
            Source::Keys(c) => return self.next_hashed(c, |c, i| dictionary::entry_at(c, i).map(|(k, _)| k)),
            Source::Values(c) => return self.next_hashed(c, |c, i| dictionary::entry_at(c, i).map(|(_, v)| v)),
            Source::Members(c) => return self.next_hashed(c, set::member_at),
        }?;
        self.taken.set(i + 1);
        Some(obj)
    }

    /// The next entry of a dictionary or set, through `entry`.
    fn next_hashed(
        &self,
        collection: &AnyObject,
        entry: impl Fn(&AnyObject, usize) -> Option<*mut AnyObject>,
    ) -> Option<*mut AnyObject> {
        let mut batch = self.batch.borrow_mut();
        if batch.done {
            return None;
        }
        if batch.next == batch.items.len() {
            let start = self.taken.get();
            let (changes, count) = hashed_state(collection);
            let mut items = Vec::with_capacity(BATCH);
            while items.len() < BATCH {
                let Some(obj) = entry(collection, start + items.len()) else { break };
                // SAFETY: the collection keeps the entry alive until
                // retained.
                items.push(unsafe { Retained::retain(obj) }.expect("non-null"));
            }
            match batch.first {
                None => batch.first = Some((changes, count)),
                // Changed since the first fetch: an error unless nothing
                // is left to see.
                Some((first, total)) if first != changes && (start < total || !items.is_empty()) => {
                    drop(items);
                    util::mutated_while_reading(collection_name(collection), collection);
                }
                Some(_) => {}
            }
            if items.is_empty() {
                batch.done = true;
                return None;
            }
            self.taken.set(start + items.len());
            let old = std::mem::replace(&mut batch.items, items);
            batch.next = 0;
            // Released before handing out: the caller has retained, or no
            // longer needs, what came from the last batch.
            drop(batch);
            drop(old);
            batch = self.batch.borrow_mut();
        }
        let obj = Retained::as_ptr(&batch.items[batch.next]).cast_mut();
        batch.next += 1;
        Some(obj)
    }

    fn mutations(&self) -> *mut c_ulong {
        match &self.source {
            // Arrays may change during a loop over their enumerators.
            Source::Nothing | Source::Array(..) | Source::ReverseArray(..) => immutable_mutations(),
            Source::Keys(d) | Source::Values(d) => dictionary::mutations(d),
            Source::Members(s) => set::mutations(s),
        }
    }

    /// Whether fast enumeration should hand out one element at a time.
    fn singly(&self) -> bool {
        matches!(self.source, Source::Array(..) | Source::ReverseArray(..))
    }
}

/// A dictionary's or set's count of changes and of entries.
fn hashed_state(collection: &AnyObject) -> (c_ulong, usize) {
    let (changes, count) = match dictionary::state(collection) {
        Some(state) => state,
        None => set::state(collection).unwrap_or((immutable_mutations(), 0)),
    };
    // SAFETY: the count lives as long as the collection.
    (unsafe { *changes }, count)
}

fn array_name(obj: &AnyObject) -> &'static str {
    if is_exactly(obj, &crate::NSMUTABLEARRAY) { "NSMutableArray" } else { "NSArray" }
}

fn collection_name(obj: &AnyObject) -> &'static str {
    if dictionary::state(obj).is_some() { "NSMutableDictionary" } else { "NSMutableSet" }
}

/// A new enumerator over `source`.
pub(crate) fn make(source: Source) -> Retained<NSEnumerator> {
    // Sending +alloc through the binding loads the class on first use.
    let this = NSEnumerator::<AnyObject>::alloc();
    // SAFETY: NSEnumerator's class is NSEnumeratorImpl, and an `Allocated`
    // is a pointer to its object whatever its type parameter.
    let this = unsafe { std::mem::transmute::<Allocated<NSEnumerator>, Allocated<NSEnumeratorImpl>>(this) };
    let this = this.set_ivars(EnumeratorIvars::new(source));
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
            let this = this.set_ivars(EnumeratorIvars::new(Source::Nothing));
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
                    // SAFETY: the collection or batch keeps the element
                    // alive until retained.
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
                let len = if ivars.singly() { len.min(1) } else { len };
                // The enumerator keeps its own position; `state` only
                // records that enumeration has begun.
                // SAFETY: the caller passes a valid state and buffer.
                unsafe { batch(state, buffer, len, ivars.mutations(), |_| ivars.next()) }
            } else {
                // One at a time: the subclass's collection may change
                // during the loop, and nothing else keeps the object
                // alive past the next call.
                // SAFETY: as above; -nextObject returns an object or nil.
                unsafe {
                    batch(state, buffer, len.min(1), immutable_mutations(), |_| {
                        let obj: *mut AnyObject = msg_send![self, nextObject];
                        (!obj.is_null()).then_some(obj)
                    })
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for NSEnumeratorImpl {}
);
