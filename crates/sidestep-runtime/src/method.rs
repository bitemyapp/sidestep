//! Methods: adding and replacing them, and the `method_*` accessors.

use std::ffi::{CStr, c_char, c_uint, c_void};
use std::mem::transmute;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::class::{CUSTOM_RR, Class, ROOT, bump_epoch, class_ref, find_method, reset_cache};
use crate::encoding;
use crate::selector::{Sel, known};
use crate::util::{Shared, leak_cstr, malloc_array, malloc_cstr};
use crate::{Bool, Imp, NO, YES};

/// A method, laid out like libobjc2's `struct objc_method`.
#[repr(C)]
pub struct Method {
    imp: AtomicPtr<c_void>,
    sel: Sel,
    types: *const c_char,
}

impl Method {
    pub(crate) fn imp(&self) -> Imp {
        // SAFETY: only valid IMPs are stored.
        unsafe { transmute::<*mut c_void, Imp>(self.imp.load(Ordering::Acquire)) }
    }

    pub(crate) fn types(&self) -> &'static CStr {
        // SAFETY: leaked C string.
        unsafe { CStr::from_ptr(self.types) }
    }
}

fn is_rr_selector(sel: Sel) -> bool {
    let k = known();
    sel == k.retain || sel == k.release || sel == k.autorelease || sel == k.retain_count
}

fn add_method(cls: &'static Class, sel: Sel, imp: Imp, types: &CStr) -> Option<&'static Method> {
    let mut table = cls.rt().methods.write().unwrap();
    if table.by_sel.contains_key(&(sel as usize)) {
        return None;
    }
    let method: &'static Method =
        Box::leak(Box::new(Method { imp: AtomicPtr::new(imp as *mut c_void), sel, types: leak_cstr(types).as_ptr() }));
    table.by_sel.insert(sel as usize, Shared(method));
    table.order.push(Shared(method));
    drop(table);
    if is_rr_selector(sel) && cls.flags() & ROOT == 0 && !cls.is_meta() {
        cls.flags.fetch_or(CUSTOM_RR, Ordering::AcqRel);
    }
    if cls.is_loaded() {
        // Subclasses may have cached what this method now overrides.
        bump_epoch();
    } else {
        reset_cache(cls);
    }
    Some(method)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_addMethod(cls: *mut Class, sel: Sel, imp: Option<Imp>, types: *const c_char) -> Bool {
    // SAFETY: the caller passes a class or null; classes under construction
    // are allowed.
    let (Some(cls), Some(imp)) = (unsafe { class_ref(cls) }, imp) else {
        return NO;
    };
    if sel.is_null() {
        return NO;
    }
    // SAFETY: the caller passes a C string or null.
    let types = unsafe { crate::util::cstr_or(types, c"") };
    if add_method(cls, sel, imp, types).is_some() { YES } else { NO }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_replaceMethod(
    cls: *mut Class,
    sel: Sel,
    imp: Option<Imp>,
    types: *const c_char,
) -> Option<Imp> {
    // SAFETY: the caller passes a class or null.
    let (Some(cls), Some(imp)) = (unsafe { class_ref(cls) }, imp) else {
        return None;
    };
    let existing = cls.rt().methods.read().unwrap().by_sel.get(&(sel as usize)).copied();
    match existing {
        // SAFETY: methods are never freed.
        Some(method) => unsafe { method_setImplementation(method.0, imp) },
        None => {
            // SAFETY: the caller passes a C string or null.
            let types = unsafe { crate::util::cstr_or(types, c"") };
            add_method(cls, sel, imp, types);
            None
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn class_getInstanceMethod(cls: *const Class, sel: Sel) -> *const Method {
    // SAFETY: the caller passes a class or null.
    match unsafe { class_ref(cls) } {
        Some(cls) => find_method(cls, sel).map_or(std::ptr::null(), |m| m as *const Method),
        None => std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn class_getClassMethod(cls: *const Class, sel: Sel) -> *const Method {
    // SAFETY: the caller passes a class or null.
    match unsafe { class_ref(cls) } {
        // SAFETY: a loaded class has a metaclass.
        Some(cls) => unsafe { class_getInstanceMethod(cls.metaclass(), sel) },
        None => std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_copyMethodList(cls: *const Class, out_len: *mut c_uint) -> *mut *const Method {
    // SAFETY: the caller passes a class or null.
    let methods: Vec<*const Method> = match unsafe { class_ref(cls) } {
        Some(cls) => cls.rt().methods.read().unwrap().order.iter().map(|m| m.0).collect(),
        None => Vec::new(),
    };
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&methods, out_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_getName(method: *const Method) -> Sel {
    // SAFETY: the caller passes a method or null.
    unsafe { method.as_ref() }.map_or(std::ptr::null(), |m| m.sel)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_getImplementation(method: *const Method) -> Option<Imp> {
    // SAFETY: the caller passes a method or null.
    unsafe { method.as_ref() }.map(Method::imp)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_getTypeEncoding(method: *const Method) -> *const c_char {
    // SAFETY: the caller passes a method or null.
    unsafe { method.as_ref() }.map_or(std::ptr::null(), |m| m.types)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_setImplementation(method: *const Method, imp: Imp) -> Option<Imp> {
    // SAFETY: the caller passes a method or null.
    let method = unsafe { method.as_ref() }?;
    let old = method.imp.swap(imp as *mut c_void, Ordering::AcqRel);
    bump_epoch();
    // SAFETY: only IMPs are stored.
    Some(unsafe { transmute::<*mut c_void, Imp>(old) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_exchangeImplementations(a: *mut Method, b: *mut Method) {
    // SAFETY: the caller passes methods or null.
    let (Some(a), Some(b)) = (unsafe { a.as_ref() }, unsafe { b.as_ref() }) else { return };
    let imp_a = a.imp.load(Ordering::Acquire);
    let imp_b = b.imp.swap(imp_a, Ordering::AcqRel);
    a.imp.store(imp_b, Ordering::Release);
    bump_epoch();
}

fn types_of<'a>(method: *const Method) -> Vec<&'a [u8]> {
    // SAFETY: callers pass a method or null.
    match unsafe { method.as_ref() } {
        Some(m) => encoding::split(m.types().to_bytes()),
        None => Vec::new(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_getNumberOfArguments(method: *const Method) -> c_uint {
    types_of(method).len().saturating_sub(1) as c_uint
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_copyReturnType(method: *const Method) -> *mut c_char {
    let types = types_of(method);
    malloc_cstr(types.first().copied().unwrap_or(b""))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_copyArgumentType(method: *const Method, index: c_uint) -> *mut c_char {
    match types_of(method).get(index as usize + 1) {
        Some(t) => malloc_cstr(t),
        None => std::ptr::null_mut(),
    }
}

unsafe fn write_type(t: Option<&[u8]>, dst: *mut c_char, dst_len: usize) {
    if dst.is_null() || dst_len == 0 {
        return;
    }
    let t = t.unwrap_or(b"");
    let n = t.len().min(dst_len - 1);
    // SAFETY: the caller passes room for `dst_len` bytes.
    unsafe {
        dst.cast::<u8>().copy_from_nonoverlapping(t.as_ptr(), n);
        std::ptr::write_bytes(dst.add(n), 0, dst_len - n);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_getReturnType(method: *const Method, dst: *mut c_char, dst_len: usize) {
    // SAFETY: forwarded contract.
    unsafe { write_type(types_of(method).first().copied(), dst, dst_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn method_getArgumentType(
    method: *const Method,
    index: c_uint,
    dst: *mut c_char,
    dst_len: usize,
) {
    let types = types_of(method);
    // SAFETY: forwarded contract.
    unsafe { write_type(types.get(index as usize + 1).copied(), dst, dst_len) }
}
