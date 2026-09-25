//! Instance variables: layout at class construction and the `ivar_*`,
//! `object_*Ivar` accessors.

use std::ffi::{CStr, c_char, c_uint, c_void};
use std::sync::atomic::Ordering;

use crate::class::{Class, class_ref};
use crate::object::{Object, object_getClass};
use crate::util::{Shared, leak_cstr, malloc_array};
use crate::{Bool, NO, YES};

/// An instance variable, laid out like the start of libobjc2's
/// `struct objc_ivar`.
#[repr(C)]
pub struct Ivar {
    name: *const c_char,
    types: *const c_char,
    offset: isize,
    size: usize,
}

impl Ivar {
    fn name(&self) -> &'static CStr {
        // SAFETY: leaked C string.
        unsafe { CStr::from_ptr(self.name) }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_addIvar(
    cls: *mut Class,
    name: *const c_char,
    size: usize,
    log2_align: u8,
    types: *const c_char,
) -> Bool {
    // SAFETY: the caller passes a class or null.
    let Some(cls) = (unsafe { class_ref(cls) }) else { return NO };
    if name.is_null() || cls.is_meta() || cls.is_loaded() {
        return NO;
    }
    // SAFETY: the caller passes C strings.
    let (name, types) = unsafe { (CStr::from_ptr(name), crate::util::cstr_or(types, c"")) };
    let rt = cls.rt();
    let mut ivars = rt.ivars.write().unwrap();
    // SAFETY: ivars are never freed.
    if ivars.iter().any(|i| unsafe { i.get() }.name() == name) {
        return NO;
    }
    let align = 1usize << log2_align;
    let offset = rt.instance_size.load(Ordering::Acquire).next_multiple_of(align);
    rt.instance_size.store(offset + size, Ordering::Release);
    rt.instance_align.fetch_max(align, Ordering::AcqRel);
    let ivar: &'static Ivar = Box::leak(Box::new(Ivar {
        name: leak_cstr(name).as_ptr(),
        types: leak_cstr(types).as_ptr(),
        offset: offset as isize,
        size,
    }));
    ivars.push(Shared(ivar));
    YES
}

fn find_ivar(cls: &'static Class, name: &CStr) -> Option<&'static Ivar> {
    let mut cls = Some(cls);
    while let Some(c) = cls {
        // SAFETY: ivars are never freed.
        let found = c.rt().ivars.read().unwrap().iter().map(|i| unsafe { i.get() }).find(|i| i.name() == name);
        if found.is_some() {
            return found;
        }
        cls = c.superclass();
    }
    None
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_getInstanceVariable(cls: *const Class, name: *const c_char) -> *const Ivar {
    // SAFETY: the caller passes a class or null, and a C string.
    match (unsafe { class_ref(cls) }, name.is_null()) {
        (Some(cls), false) => {
            find_ivar(cls, unsafe { CStr::from_ptr(name) }).map_or(std::ptr::null(), |i| i as *const Ivar)
        }
        _ => std::ptr::null(),
    }
}

/// Class variables are not supported by the modern runtimes either.
#[unsafe(no_mangle)]
pub extern "C" fn class_getClassVariable(_cls: *const Class, _name: *const c_char) -> *const Ivar {
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_copyIvarList(cls: *const Class, out_len: *mut c_uint) -> *mut *const Ivar {
    // SAFETY: the caller passes a class or null.
    let ivars: Vec<*const Ivar> = match unsafe { class_ref(cls) } {
        Some(cls) => cls.rt().ivars.read().unwrap().iter().map(|i| i.0).collect(),
        None => Vec::new(),
    };
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&ivars, out_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ivar_getName(ivar: *const Ivar) -> *const c_char {
    // SAFETY: the caller passes an ivar or null.
    unsafe { ivar.as_ref() }.map_or(std::ptr::null(), |i| i.name)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ivar_getTypeEncoding(ivar: *const Ivar) -> *const c_char {
    // SAFETY: the caller passes an ivar or null.
    unsafe { ivar.as_ref() }.map_or(std::ptr::null(), |i| i.types)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ivar_getOffset(ivar: *const Ivar) -> isize {
    // SAFETY: the caller passes an ivar or null.
    unsafe { ivar.as_ref() }.map_or(0, |i| i.offset)
}

unsafe fn slot(obj: *const Object, ivar: *const Ivar) -> Option<*mut *mut Object> {
    // SAFETY: the caller passes an ivar or null.
    let ivar = unsafe { ivar.as_ref() }?;
    if obj.is_null() {
        return None;
    }
    // SAFETY: the ivar belongs to the object's class.
    Some(unsafe { obj.cast::<u8>().offset(ivar.offset) }.cast_mut().cast())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_getIvar(obj: *const Object, ivar: *const Ivar) -> *const Object {
    // SAFETY: forwarded contract.
    match unsafe { slot(obj, ivar) } {
        // SAFETY: as above.
        Some(slot) => unsafe { *slot },
        None => std::ptr::null(),
    }
}

/// Stores without retaining, as for an `__unsafe_unretained` ivar.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_setIvar(obj: *mut Object, ivar: *const Ivar, value: *mut Object) {
    // SAFETY: forwarded contract.
    if let Some(slot) = unsafe { slot(obj, ivar) } {
        // SAFETY: as above.
        unsafe { *slot = value };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_getInstanceVariable(
    obj: *const Object,
    name: *const c_char,
    out_value: *mut *mut c_void,
) -> *const Ivar {
    // SAFETY: forwarded contract.
    let ivar = unsafe { class_getInstanceVariable(object_getClass(obj), name) };
    if !out_value.is_null() {
        // SAFETY: forwarded contract.
        unsafe { *out_value = object_getIvar(obj, ivar).cast_mut().cast() };
    }
    ivar
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn object_setInstanceVariable(
    obj: *mut Object,
    name: *const c_char,
    value: *mut c_void,
) -> *const Ivar {
    // SAFETY: forwarded contract.
    let ivar = unsafe { class_getInstanceVariable(object_getClass(obj), name) };
    // SAFETY: forwarded contract.
    unsafe { object_setIvar(obj, ivar, value.cast()) };
    ivar
}
