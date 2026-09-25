//! Small shared pieces: libc allocation for buffers handed to C callers, a
//! reentrant global lock for class loading, and pointer wrappers for tables.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

/// Allocate with libc `malloc`, as the runtime's `copy` functions must: the
/// caller releases the result with `free`.
pub(crate) fn c_malloc(size: usize) -> *mut c_void {
    // SAFETY: plain libc call.
    let ptr = unsafe { malloc(size.max(1)) };
    assert!(!ptr.is_null(), "sidestep: out of memory");
    ptr
}

/// Release memory from [`c_malloc`].
pub(crate) unsafe fn c_free(ptr: *mut c_void) {
    // SAFETY: the caller passes a pointer from `c_malloc`.
    unsafe { free(ptr) }
}

/// Copy `items` into a `malloc`ed array, writing its length to `out_len`.
pub(crate) unsafe fn malloc_array<T: Copy>(items: &[T], out_len: *mut u32) -> *mut T {
    if !out_len.is_null() {
        // SAFETY: the caller passes a valid or null pointer.
        unsafe { *out_len = items.len() as u32 };
    }
    if items.is_empty() {
        return std::ptr::null_mut();
    }
    let ptr = c_malloc(size_of_val(items)).cast::<T>();
    // SAFETY: freshly allocated with room for every item.
    unsafe { ptr.copy_from_nonoverlapping(items.as_ptr(), items.len()) };
    ptr
}

/// Copy `s` into a `malloc`ed, NUL-terminated string.
pub(crate) fn malloc_cstr(s: &[u8]) -> *mut c_char {
    let ptr = c_malloc(s.len() + 1).cast::<u8>();
    // SAFETY: room for the bytes and the terminator.
    unsafe {
        ptr.copy_from_nonoverlapping(s.as_ptr(), s.len());
        *ptr.add(s.len()) = 0;
    }
    ptr.cast()
}

/// Leak `s` as a C string that lives for the rest of the program.
pub(crate) fn leak_cstr(s: &CStr) -> &'static CStr {
    Box::leak(s.to_owned().into_boxed_c_str())
}

/// A non-zero token that is unique among live threads.
pub(crate) fn thread_token() -> usize {
    thread_local!(static TOKEN: u8 = const { 0 });
    TOKEN.with(|t| t as *const u8 as usize)
}

/// A raw pointer that tables may share between threads. The runtime's
/// classes, selectors, methods and protocols are never freed.
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct Shared<T>(pub(crate) *const T);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Shared<T> {}
// SAFETY: see the type's documentation.
unsafe impl<T> Send for Shared<T> {}
// SAFETY: see the type's documentation.
unsafe impl<T> Sync for Shared<T> {}

impl<T> Shared<T> {
    /// # Safety
    /// The pointee must live for the rest of the program.
    pub(crate) unsafe fn get(self) -> &'static T {
        // SAFETY: guaranteed by the caller.
        unsafe { &*self.0 }
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

static LOAD_LOCK: Mutex<()> = Mutex::new(());
static LOAD_OWNER: AtomicUsize = AtomicUsize::new(0);

/// Run `f` holding the class-loading lock. The lock is reentrant: loading a
/// class loads its superclass, and `+initialize` may touch further classes.
pub(crate) fn with_load_lock<R>(f: impl FnOnce() -> R) -> R {
    let me = thread_token();
    if LOAD_OWNER.load(Ordering::Relaxed) == me {
        return f();
    }
    let _guard = lock(&LOAD_LOCK);
    LOAD_OWNER.store(me, Ordering::Relaxed);
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            LOAD_OWNER.store(0, Ordering::Relaxed);
        }
    }
    let _release = Release;
    f()
}

/// A C string for diagnostics, tolerating null.
pub(crate) unsafe fn cstr_or(ptr: *const c_char, fallback: &CStr) -> &CStr {
    if ptr.is_null() {
        fallback
    } else {
        // SAFETY: the caller passes a valid C string or null.
        unsafe { CStr::from_ptr(ptr) }
    }
}
