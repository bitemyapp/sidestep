//! Object memory. Every instance is preceded by a 16-byte [`Header`] holding
//! its reference count; the object pointer itself starts with `isa`, as the
//! ABI requires.

use std::alloc::{Layout, alloc_zeroed, dealloc};
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
pub(crate) const RC_ONE: usize = 1 << 4;

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
    let Some(cls) = (unsafe { class_ref(cls) }) else {
        return std::ptr::null_mut();
    };
    let align = cls.instance_align().max(HEADER_SIZE);
    let size = cls.instance_size().max(size_of::<Object>()) + extra_bytes;
    let total = align + size;
    let layout = Layout::from_size_align(total, align).expect("sidestep: instance too large");
    // SAFETY: non-zero size.
    let base = unsafe { alloc_zeroed(layout) };
    if base.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    // SAFETY: `align` bytes precede the object and hold its header.
    unsafe {
        let obj = base.add(align).cast::<Object>();
        let header = obj.cast::<u8>().sub(HEADER_SIZE).cast::<Header>();
        header.write(Header {
            rc: AtomicUsize::new(0),
            total_size: u32::try_from(total).expect("sidestep: instance too large"),
            base_offset: align as u32,
        });
        (*obj).isa.store((cls as *const Class).cast_mut(), Ordering::Release);
        obj
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn object_dispose(obj: *mut Object) -> *mut Object {
    if obj.is_null() {
        return obj;
    }
    // SAFETY: the caller passes a live object it owns.
    let (bits, total, base_offset) = unsafe {
        let h = header(obj);
        (h.rc.load(Ordering::Acquire), h.total_size as usize, h.base_offset as usize)
    };
    if bits & IMMORTAL != 0 {
        return std::ptr::null_mut();
    }
    if bits & WEAKLY_REFERENCED != 0 {
        crate::arc::clear_weak(obj);
    }
    if bits & HAS_ASSOCIATED != 0 {
        crate::associated::remove_all(obj);
    }
    crate::sync::forget(obj);
    // SAFETY: the allocation was made by class_createInstance with this
    // layout.
    unsafe {
        let base = obj.cast::<u8>().sub(base_offset);
        dealloc(base, Layout::from_size_align_unchecked(total, base_offset));
    }
    std::ptr::null_mut()
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
