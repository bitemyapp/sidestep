//! Foundation's C functions for memory and types: zones (gone, as on
//! macOS: every zone is the default one, named `DefaultMallocZone`, and
//! allocates with `malloc`), memory pages, `NSGetSizeAndAlignment`, and the
//! uncaught exception handler (kept for programs that read it back:
//! Sidestep panics where Foundation raises, so nothing calls it).

use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};

use objc2::rc::Retained;
use objc2_foundation::{NSString, NSUInteger};

/// The one zone's address: a zone is an opaque pointer programs pass back.
static ZONE: u8 = 0;

fn zone() -> *mut c_void {
    (&raw const ZONE).cast_mut().cast()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSDefaultMallocZone() -> *mut c_void {
    zone()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSCreateZone(_start: NSUInteger, _granularity: NSUInteger, _can_free: u8) -> *mut c_void {
    zone()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRecycleZone(_zone: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSSetZoneName(_zone: *mut c_void, _name: *mut c_void) {}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSZoneName(_zone: *mut c_void) -> *mut NSString {
    Retained::autorelease_return(NSString::from_str("DefaultMallocZone"))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSZoneFromPointer(_ptr: *mut c_void) -> *mut c_void {
    zone()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSZoneMalloc(_zone: *mut c_void, size: NSUInteger) -> *mut c_void {
    // SAFETY: malloc takes any size.
    unsafe { libc::malloc(size) }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSZoneCalloc(_zone: *mut c_void, count: NSUInteger, size: NSUInteger) -> *mut c_void {
    // SAFETY: calloc takes any sizes.
    unsafe { libc::calloc(count, size) }
}

/// # Safety
///
/// `ptr` is null or came from these functions (or `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSZoneRealloc(_zone: *mut c_void, ptr: *mut c_void, size: NSUInteger) -> *mut c_void {
    // SAFETY: forwarded contract.
    unsafe { libc::realloc(ptr, size) }
}

/// # Safety
///
/// `ptr` came from these functions (or `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSZoneFree(_zone: *mut c_void, ptr: *mut c_void) {
    // SAFETY: forwarded contract.
    unsafe { libc::free(ptr) }
}

/// Garbage collection is long gone: plain memory, zeroed when asked for
/// (`NSScannedOption` and `NSCollectorDisabledOption` mean nothing now).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSAllocateCollectable(size: NSUInteger, _options: NSUInteger) -> *mut c_void {
    // SAFETY: calloc takes any size.
    unsafe { libc::calloc(1, size) }
}

/// # Safety
///
/// `ptr` is null or came from these functions (or `malloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSReallocateCollectable(
    ptr: *mut c_void,
    size: NSUInteger,
    _options: NSUInteger,
) -> *mut c_void {
    // SAFETY: forwarded contract.
    unsafe { libc::realloc(ptr, size) }
}

fn page_size() -> NSUInteger {
    // SAFETY: sysconf has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size > 0 { size as NSUInteger } else { 4096 }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSPageSize() -> NSUInteger {
    page_size()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSLogPageSize() -> NSUInteger {
    page_size().trailing_zeros() as NSUInteger
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRoundUpToMultipleOfPageSize(bytes: NSUInteger) -> NSUInteger {
    bytes.next_multiple_of(page_size())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRoundDownToMultipleOfPageSize(bytes: NSUInteger) -> NSUInteger {
    bytes - bytes % page_size()
}

/// Zeroed pages.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSAllocateMemoryPages(bytes: NSUInteger) -> *mut c_void {
    let size = NSRoundUpToMultipleOfPageSize(bytes).max(page_size());
    let mut out = std::ptr::null_mut();
    // SAFETY: room for the result; the alignment is a power of two.
    if unsafe { libc::posix_memalign(&mut out, page_size(), size) } != 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: `size` bytes just allocated.
    unsafe { std::ptr::write_bytes(out.cast::<u8>(), 0, size) };
    out
}

/// # Safety
///
/// `ptr` came from `NSAllocateMemoryPages`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSDeallocateMemoryPages(ptr: *mut c_void, _bytes: NSUInteger) {
    // SAFETY: forwarded contract.
    unsafe { libc::free(ptr) }
}

/// # Safety
///
/// Both point to `bytes` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSCopyMemoryPages(source: *const c_void, dest: *mut c_void, bytes: NSUInteger) {
    // SAFETY: forwarded contract; the pages may overlap.
    unsafe { std::ptr::copy(source.cast::<u8>(), dest.cast::<u8>(), bytes) }
}

/// The machine's physical memory.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSRealMemoryAvailable() -> NSUInteger {
    // SAFETY: sysconf has no preconditions.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    if pages > 0 { pages as NSUInteger * page_size() } else { 0 }
}

/// The size and alignment of the first type in `type_ptr`, and where the
/// rest of the encoding starts.
///
/// # Safety
///
/// `type_ptr` is a NUL-terminated type encoding; `sizep` and `alignp` are
/// null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSGetSizeAndAlignment(
    type_ptr: NonNull<c_char>,
    sizep: *mut NSUInteger,
    alignp: *mut NSUInteger,
) -> *const c_char {
    // SAFETY: forwarded contract.
    let encoding = unsafe { CStr::from_ptr(type_ptr.as_ptr()) }.to_bytes();
    let Some((size, align, rest)) = crate::value::layout(encoding) else {
        panic!("*** NSGetSizeAndAlignment(): unsupported type encoding spec '{}'", String::from_utf8_lossy(encoding));
    };
    // SAFETY: forwarded contract.
    unsafe {
        if !sizep.is_null() {
            sizep.write(size);
        }
        if !alignp.is_null() {
            alignp.write(align);
        }
        type_ptr.as_ptr().add(encoding.len() - rest.len())
    }
}

/// The handler `NSSetUncaughtExceptionHandler` installed.
static HANDLER: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSGetUncaughtExceptionHandler() -> *mut c_void {
    HANDLER.load(Ordering::Acquire)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn NSSetUncaughtExceptionHandler(handler: *mut c_void) {
    HANDLER.store(handler, Ordering::Release);
}
