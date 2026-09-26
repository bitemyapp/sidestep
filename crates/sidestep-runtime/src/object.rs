//! Object memory. Every instance is preceded by a 16-byte [`Header`] holding
//! its reference count; the object pointer itself starts with `isa`, as the
//! ABI requires.

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::class::{BLOCK, Class, META, class_getName, class_ref};

/// The start of every object: its class.
#[repr(C)]
pub struct Object {
    pub(crate) isa: AtomicPtr<Class>,
}

/// Bits of [`Header::rc`]. The remaining bits count retains beyond the
/// first, in steps of [`RC_ONE`].
pub(crate) const DEALLOCATING: usize = 1 << 0;
pub(crate) const WEAKLY_REFERENCED: usize = 1 << 1;
pub(crate) const HAS_ASSOCIATED: usize = 1 << 2;
/// Statically allocated objects, which are never freed.
pub(crate) const IMMORTAL: usize = 1 << 3;
/// The object has been locked with `objc_sync_enter`.
pub(crate) const SYNCHRONIZED: usize = 1 << 4;
pub(crate) const RC_ONE: usize = 1 << 5;
/// Side tables that must forget an object when it is freed.
const IN_SIDE_TABLES: usize = WEAKLY_REFERENCED | HAS_ASSOCIATED | SYNCHRONIZED;

/// Sits immediately before each object.
#[repr(C)]
pub(crate) struct Header {
    pub(crate) rc: AtomicUsize,
    /// Size of the whole allocation, header and padding included.
    total_size: u32,
    /// Distance from the start of the allocation to the object; also the
    /// allocation's alignment.
    base_offset: u32,
}

pub(crate) const HEADER_SIZE: usize = size_of::<Header>();
const _: () = assert!(HEADER_SIZE == 16);

/// The header of a heap or immortal object.
///
/// # Safety
/// `obj` must be a live object that is neither a class nor a block.
pub(crate) unsafe fn header<'a>(obj: *const Object) -> &'a Header {
    // SAFETY: guaranteed by the caller.
    unsafe { &*obj.cast::<u8>().sub(HEADER_SIZE).cast::<Header>() }
}

/// The class of a non-null object.
///
/// # Safety
/// `obj` must be a live object.
pub(crate) unsafe fn isa(obj: *const Object) -> &'static Class {
    // SAFETY: every object's isa is a class, and classes are never freed.
    unsafe { &*(*obj).isa.load(Ordering::Acquire) }
}

/// [`isa`] without ordering, for message dispatch: a receiver's class was
/// published along with the receiver, and changing it while other threads
/// message the object is a race on Apple's runtime too.
///
/// # Safety
/// `obj` must be a live object.
#[inline(always)]
pub(crate) unsafe fn isa_relaxed(obj: *const Object) -> &'static Class {
    // SAFETY: as for `isa`.
    unsafe { &*(*obj).isa.load(Ordering::Relaxed) }
}

/// What kind of memory an object lives in, which decides how it is counted.
pub(crate) enum Kind {
    /// A class object: never counted, never freed.
    Class,
    /// A block: counted through the blocks ABI.
    Block,
    /// A regular object with a [`Header`].
    Counted,
}

/// # Safety
/// `obj` must be a live, non-null object.
pub(crate) unsafe fn kind(obj: *const Object) -> Kind {
    // SAFETY: guaranteed by the caller.
    let flags = unsafe { isa(obj) }.flags();
    if flags & META != 0 {
        Kind::Class
    } else if flags & BLOCK != 0 {
        Kind::Block
    } else {
        Kind::Counted
    }
}

/// An object that lives in static memory and ignores retain and release.
/// Framework crates use it for constant strings.
#[repr(C)]
pub struct StaticObject<T> {
    header: Header,
    isa: *const Class,
    pub body: T,
}

// SAFETY: the header's count is never written for immortal objects, and the
// isa is immutable.
unsafe impl<T: Sync> Sync for StaticObject<T> {}

impl<T> StaticObject<T> {
    pub const fn new(class: &'static Class, body: T) -> Self {
        StaticObject {
            header: Header { rc: AtomicUsize::new(IMMORTAL), total_size: 0, base_offset: 0 },
            isa: class as *const Class,
            body,
        }
    }

    pub fn as_object(&'static self) -> *mut Object {
        (&raw const self.isa).cast_mut().cast()
    }

    /// The object pointer, usable in a `static` initializer.
    pub const fn object_ref(&'static self) -> ObjectRef {
        ObjectRef((&raw const self.isa).cast())
    }

    /// The object's body, given a pointer to the object.
    ///
    /// # Safety
    /// `obj` must point to a `StaticObject<T>`'s object.
    pub unsafe fn body_of<'a>(obj: *const Object) -> &'a T {
        // SAFETY: the body follows the isa pointer, as laid out above.
        unsafe { &*obj.cast::<*const Class>().add(1).cast::<u8>().add(Self::BODY_PAD).cast::<T>() }
    }

    const BODY_PAD: usize = std::mem::offset_of!(StaticObject<T>, body)
        - std::mem::offset_of!(StaticObject<T>, isa)
        - size_of::<*const Class>();
}

/// A pointer to an object in static memory, laid out as a plain pointer so
/// it can be exported as an Objective-C constant such as
/// `NSFontAttributeName`.
#[repr(transparent)]
pub struct ObjectRef(*const Object);

// SAFETY: points to an immutable, immortal object.
unsafe impl Sync for ObjectRef {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_createInstance(cls: *const Class, extra_bytes: usize) -> *mut Object {
    // SAFETY: the caller passes a class or null.
    match unsafe { class_ref(cls) } {
        Some(cls) => create_instance(cls, extra_bytes),
        None => std::ptr::null_mut(),
    }
}

/// The allocation for an instance of `cls` with `extra_bytes` more:
/// `(total size, offset of the object)`, or `None` if it can't be
/// described.
///
/// Each allocation holds the header, then the object: `offset` bytes (at
/// least the header's 16, and the instance's alignment) followed by the
/// instance, rounded up to whole words. Allocators hand out 16-byte
/// multiples anyway, and whole words let `create_instance` zero it with
/// word stores.
pub(crate) fn instance_layout(cls: &Class, extra_bytes: usize) -> Option<(usize, usize)> {
    let offset = cls.instance_align().max(HEADER_SIZE);
    let size = cls.instance_size().max(size_of::<Object>()).checked_add(extra_bytes)?;
    let total = offset.checked_add(size.checked_next_multiple_of(size_of::<u64>())?)?;
    Layout::from_size_align(total, offset).ok()?;
    u32::try_from(total).ok()?;
    Some((total, offset))
}

/// A new instance of `cls`, a loaded class, with its reference count at
/// one, its instance variables zeroed and `extra_bytes` more zeroed bytes
/// after them.
///
/// The zeroing is done here rather than by `calloc`, which would zero the
/// header and `isa` only for them to be written again, and word by word,
/// because a typical instance is a few words and a `memset` call costs
/// more than the stores. For a plain `NSObject` there is nothing to zero.
#[inline]
pub(crate) fn create_instance(cls: &'static Class, extra_bytes: usize) -> *mut Object {
    // Worked out when the class was registered, which the caller's acquire
    // load of its flags made visible.
    let packed = cls.alloc_layout.load(Ordering::Relaxed);
    let (total, offset) = if packed != 0 && extra_bytes == 0 {
        ((packed >> 32) as usize, packed as u32 as usize)
    } else {
        layout_slow(cls, extra_bytes)
    };
    // SAFETY: `instance_layout` checked that this is a valid layout.
    let layout = unsafe { Layout::from_size_align_unchecked(total, offset) };
    // SAFETY: non-zero size.
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: `offset` bytes precede the object and hold its header, and
    // the rest of the allocation, a whole number of words, is the object.
    // `offset` is at least 16, so the words are aligned.
    unsafe {
        let obj = base.add(offset).cast::<Object>();
        let header = obj.cast::<u8>().sub(HEADER_SIZE).cast::<Header>();
        header.write(Header { rc: AtomicUsize::new(0), total_size: total as u32, base_offset: offset as u32 });
        // The object is not shared yet: whoever it is handed to gets it
        // through a synchronizing operation of their own.
        obj.cast::<*const Class>().write(cls);
        let words = obj.cast::<u64>();
        let n = (total - offset) / size_of::<u64>();
        if n <= 16 {
            // Volatile only so the compiler keeps the stores rather than
            // turning the loop back into a `memset` call.
            for i in 1..n {
                words.add(i).write_volatile(0);
            }
        } else {
            words.add(1).write_bytes(0, n - 1);
        }
        obj
    }
}

#[cold]
fn layout_slow(cls: &Class, extra_bytes: usize) -> (usize, usize) {
    instance_layout(cls, extra_bytes)
        .unwrap_or_else(|| panic!("sidestep: an instance of {:?} is too large to allocate", cls.name()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn object_dispose(obj: *mut Object) -> *mut Object {
    if obj.is_null() {
        return obj;
    }
    // SAFETY: the caller passes a live object it owns, so nothing else
    // touches its header any more.
    let (bits, total, base_offset) = unsafe {
        let h = header(obj);
        (h.rc.load(Ordering::Relaxed), h.total_size as usize, h.base_offset as usize)
    };
    if bits & IMMORTAL != 0 {
        return std::ptr::null_mut();
    }
    if bits & IN_SIDE_TABLES != 0 {
        forget(obj, bits);
    }
    // SAFETY: the allocation was made by `create_instance` with this
    // layout.
    unsafe {
        let base = obj.cast::<u8>().sub(base_offset);
        dealloc(base, Layout::from_size_align_unchecked(total, base_offset));
    }
    std::ptr::null_mut()
}

/// Remove an object that is being freed from the side tables it is in.
#[cold]
fn forget(obj: *mut Object, bits: usize) {
    if bits & WEAKLY_REFERENCED != 0 {
        crate::arc::clear_weak(obj);
    }
    if bits & HAS_ASSOCIATED != 0 {
        crate::associated::remove_all(obj);
    }
    if bits & SYNCHRONIZED != 0 {
        crate::sync::forget(obj);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_getClass(obj: *const Object) -> *const Class {
    if obj.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a live object.
    unsafe { (*obj).isa.load(Ordering::Acquire) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_setClass(obj: *mut Object, cls: *const Class) -> *const Class {
    if obj.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a live object and a class.
    if let Some(cls) = unsafe { class_ref(cls) } {
        crate::class::ensure_initialized(cls.instance_class());
    }
    // SAFETY: as above.
    unsafe { (*obj).isa.swap(cls.cast_mut(), Ordering::AcqRel) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_getClassName(obj: *const Object) -> *const c_char {
    // SAFETY: forwarded contract.
    unsafe { class_getName(object_getClass(obj)) }
}

/// The extra bytes requested from `class_createInstance`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_getIndexedIvars(obj: *const Object) -> *const c_void {
    if obj.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a live object.
    let size = unsafe { isa(obj) }.instance_size().max(size_of::<Object>());
    // SAFETY: the extra bytes follow the instance variables.
    unsafe { obj.cast::<u8>().add(size).cast() }
}
