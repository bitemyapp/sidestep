//! The cell a mutable collection keeps its storage in.
//!
//! Foundation lets any number of threads read a collection at once, as long
//! as none of them changes it, and it lets a collection's elements run
//! arbitrary code (`-isEqual:`, `-hash`, comparators, `-dealloc`) in the
//! middle of the collection's own methods: code that may try to change the
//! collection. A `RefCell` catches the second but breaks the first, because
//! every read writes its borrow flag without synchronization.
//!
//! A [`Guarded`] keeps the value in an `UnsafeCell` beside an atomic count of
//! the readers that run other code while they hold it. Reads that run no
//! other code (`-count`, `-objectAtIndex:`, a lookup of one of Sidestep's
//! strings) take a plain reference and write nothing ([`Guarded::peek`]).
//! Reads that send messages hold a [`Reading`], which is counted. A change
//! checks that nothing is counted, so a callback that changes the collection
//! it was called from panics with Foundation's message instead of freeing
//! what its caller is reading.
//!
//! Changing a collection while another thread reads it is a data race here,
//! as it is in Foundation; the count only makes some such races fail loudly.
//!
//! A mutable collection's storage also sits behind a reference count, as a
//! [`Cow`], so that copying the collection shares it: a copy is a reference
//! count, and whichever holder changes next takes a copy of its own. Taking
//! a copy of the reference is a read, so threads may copy a collection at
//! once as they may read it.

use std::cell::UnsafeCell;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use objc2::runtime::AnyObject;

pub(crate) struct Guarded<T> {
    value: UnsafeCell<T>,
    /// Readers holding the value while other code runs, on any thread.
    readers: AtomicUsize,
}

impl<T: Default> Default for Guarded<T> {
    fn default() -> Self {
        Guarded::new(T::default())
    }
}

impl<T> Guarded<T> {
    pub(crate) fn new(value: T) -> Self {
        Guarded { value: UnsafeCell::new(value), readers: AtomicUsize::new(0) }
    }

    /// The value, for a read that runs no other code.
    ///
    /// # Safety
    /// Until the caller's last use of the reference, no other code may run on
    /// this thread: no messages, and no retains or releases, which may send
    /// messages.
    #[inline]
    pub(crate) unsafe fn peek(&self) -> &T {
        // SAFETY: `write` hands out the value only to callers that run no
        // other code while they hold it, so nothing on this thread changes it
        // while the caller reads; another thread changing it meanwhile is a
        // race Foundation doesn't allow either.
        unsafe { &*self.value.get() }
    }

    /// The value, for a read that may run other code (messages to the
    /// elements) while it holds it. Changes fail until it is dropped.
    #[inline]
    pub(crate) fn read(&self) -> Reading<'_, T> {
        self.readers.fetch_add(1, Ordering::Acquire);
        Reading(self)
    }

    /// The value, for changing. Fails, naming `owner` and `obj` as
    /// Foundation does, while a reader runs other code.
    ///
    /// # Safety
    /// Until the caller's last use of the reference, no other code may run on
    /// this thread (see `peek`), and the caller may use no other reference to
    /// the value.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn write(&self, owner: &str, obj: *const AnyObject) -> &mut T {
        if self.readers.load(Ordering::Acquire) != 0 {
            crate::util::mutated_while_reading(owner, obj);
        }
        // SAFETY: no reader holds the value while running code, readers that
        // run none have finished (nothing else ran on this thread since they
        // began), and the caller promises the rest.
        unsafe { &mut *self.value.get() }
    }
}

/// A counted read of a [`Guarded`] value.
pub(crate) struct Reading<'a, T>(&'a Guarded<T>);

impl<T> Deref for Reading<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: while this reader is counted, `write` hands the value to no
        // one on this thread.
        unsafe { &*self.0.value.get() }
    }
}

impl<T> Drop for Reading<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::Release);
    }
}

/// Storage shared with copies until the next change, in a [`Guarded`] cell.
pub(crate) struct Cow<T> {
    cell: Guarded<Arc<T>>,
    /// Whether the storage may have been shared since this collection last
    /// found it its own. While it is false, a change needs no look at the
    /// reference count.
    maybe_shared: AtomicBool,
}

/// A count of references to storage that isn't `Send`: an immutable copy
/// may be released on any thread, as Objective-C allows, and objc_release
/// is thread-safe.
#[allow(clippy::arc_with_non_send_sync)]
pub(crate) fn counted<T>(value: T) -> Arc<T> {
    Arc::new(value)
}

impl<T: Default> Default for Cow<T> {
    fn default() -> Self {
        Cow::fresh(T::default())
    }
}

impl<T> Cow<T> {
    /// New storage, this collection's alone.
    pub(crate) fn fresh(value: T) -> Self {
        Cow { cell: Guarded::new(counted(value)), maybe_shared: AtomicBool::new(false) }
    }

    /// Storage that copies may share.
    pub(crate) fn shared(value: Arc<T>) -> Self {
        Cow { cell: Guarded::new(value), maybe_shared: AtomicBool::new(true) }
    }

    /// As for `Guarded::peek`.
    ///
    /// # Safety
    /// As for `Guarded::peek`.
    #[inline]
    pub(crate) unsafe fn peek(&self) -> &T {
        // SAFETY: guaranteed by the caller.
        unsafe { self.cell.peek() }
    }

    /// As for `Guarded::read`.
    #[inline]
    pub(crate) fn read(&self) -> Reading<'_, Arc<T>> {
        self.cell.read()
    }

    /// The storage, shared for a copy to hold, without retaining anything.
    #[inline]
    pub(crate) fn share(&self) -> Arc<T> {
        self.maybe_shared.store(true, Ordering::Relaxed);
        // SAFETY: counting another reference runs no other code.
        Arc::clone(unsafe { self.cell.peek() })
    }

    /// Put `new` in place of the storage, returning the old one for the
    /// caller to release once nothing is held. Fails like `write`.
    pub(crate) fn replace(&self, new: Arc<T>, owner: &str, obj: *const AnyObject) -> Arc<T> {
        self.maybe_shared.store(true, Ordering::Relaxed);
        // SAFETY: the storage is swapped, running nothing.
        std::mem::replace(unsafe { self.cell.write(owner, obj) }, new)
    }

    /// The storage for changing, if this collection alone holds it; `None`
    /// if copies share it. Fails, as `Guarded::write` does, while a reader
    /// runs other code.
    ///
    /// # Safety
    /// As for `Guarded::write`.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn write_alone(&self, owner: &str, obj: *const AnyObject) -> Option<&mut T> {
        // SAFETY: guaranteed by the caller.
        let storage = unsafe { self.cell.write(owner, obj) };
        if !self.maybe_shared.load(Ordering::Relaxed) {
            // SAFETY: the storage wasn't shared since this collection last
            // found it its own (`share` is the only place a reference to it
            // is copied), so this is the only reference, and the caller
            // holds the cell for changing.
            return Some(unsafe { &mut *Arc::as_ptr(storage).cast_mut() });
        }
        let alone = Arc::get_mut(storage)?;
        self.maybe_shared.store(false, Ordering::Relaxed);
        Some(alone)
    }
}

impl<T: Clone> Cow<T> {
    /// Before a change: copies share the storage, so take a copy of it
    /// (retaining its elements while it is only read, so the retains may
    /// run code that reads the collection).
    #[cold]
    pub(crate) fn unshare(&self, owner: &str, obj: *const AnyObject) {
        let copy = counted(T::clone(&self.read()));
        // SAFETY: nothing runs until `old` is released below.
        let old = std::mem::replace(unsafe { self.cell.write(owner, obj) }, copy);
        self.maybe_shared.store(false, Ordering::Relaxed);
        // Released with nothing held: the copies may be gone by now.
        drop(old);
    }

    /// The storage for changing, this collection's alone, copied first if
    /// copies share it.
    ///
    /// # Safety
    /// As for `Guarded::write`.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn write(&self, owner: &str, obj: *const AnyObject) -> &mut T {
        loop {
            // SAFETY: guaranteed by the caller; the reference is returned or
            // not used.
            if let Some(storage) = unsafe { self.write_alone(owner, obj) } {
                return storage;
            }
            self.unshare(owner, obj);
        }
    }
}
