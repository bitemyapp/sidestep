//! Methods: adding and replacing them, and the `method_*` accessors.

use std::ffi::{CStr, c_char, c_uint, c_void};
use std::mem::transmute;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::class::{
    CUSTOM_ALLOC, CUSTOM_RR, Class, ROOT, bump_epoch, class_ref, find_method, recompute_override_flags, reset_cache,
    set_override_flag,
};
use crate::encoding;
use crate::selector::{Sel, known};
use crate::util::{Shared, leak_cstr, lock, malloc_array, malloc_cstr};
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

fn rr_selectors() -> [Sel; 4] {
    let k = known();
    [k.retain, k.release, k.autorelease, k.retain_count]
}

fn alloc_selectors() -> [Sel; 2] {
    let k = known();
    [k.alloc, k.alloc_with_zone]
}

fn is_rr_selector(sel: Sel) -> bool {
    rr_selectors().contains(&sel)
}

fn is_alloc_selector(sel: Sel) -> bool {
    alloc_selectors().contains(&sel)
}

/// The implementations root classes were given for the selectors the fast
/// paths stand in for, by method: the defaults those paths may skip.
static ROOT_DEFAULTS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// The override flags `cls` earns by its own methods, not counting its
/// superclasses': `CUSTOM_RR` if it implements `retain`, `release`,
/// `autorelease` or `retainCount`, `CUSTOM_ALLOC` if its metaclass
/// implements `alloc` or `allocWithZone:`. A root class's own methods
/// count only once their implementation differs from the one the class
/// was given.
pub(crate) fn own_overrides(cls: &'static Class) -> u32 {
    let root = cls.flags() & ROOT != 0;
    let defaults = if root { Some(lock(&ROOT_DEFAULTS)) } else { None };
    let overrides = |holder: &Class, sels: &[Sel]| {
        let table = holder.rt().methods.read().unwrap();
        sels.iter().filter_map(|&sel| table.by_sel.get(&(sel as usize))).any(|m| {
            // SAFETY: methods are never freed.
            let m = unsafe { m.get() };
            defaults.as_ref().is_none_or(|d| !d.contains(&(m as *const Method as usize, m.imp() as usize)))
        })
    };
    let mut flags = 0;
    if overrides(cls, &rr_selectors()) {
        flags |= CUSTOM_RR;
    }
    if overrides(cls.metaclass(), &alloc_selectors()) {
        flags |= CUSTOM_ALLOC;
    }
    flags
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
    // The root class's own implementations are the defaults the fast paths
    // stand in for.
    let rr = !cls.is_meta() && is_rr_selector(sel);
    let alloc = cls.is_meta() && is_alloc_selector(sel);
    if cls.flags() & ROOT != 0 {
        if rr || alloc {
            lock(&ROOT_DEFAULTS).push((method as *const Method as usize, imp as usize));
        }
    } else if rr {
        set_override_flag(cls, CUSTOM_RR);
    } else if alloc {
        set_override_flag(cls.peer(), CUSTOM_ALLOC);
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
    if old != imp as *mut c_void {
        implementation_changed(method);
    }
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
    if imp_a != imp_b {
        implementation_changed(a);
        implementation_changed(b);
    }
}

/// A method's implementation was replaced. If it is one the ARC and
/// allocation fast paths stand in for, every class's flags are worked out
/// again: replacing the root class's default takes the fast paths away
/// from the class and every class below, so the replacement is called, and
/// putting the default back returns them. Swizzling is rare, so this
/// simply visits every loaded class.
fn implementation_changed(method: &'static Method) {
    if is_rr_selector(method.sel) || is_alloc_selector(method.sel) {
        recompute_override_flags();
    }
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

#[cfg(test)]
mod tests {
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Imp, NSObject, Sel};
    use objc2::{ClassType, sel};

    use crate::class::{CUSTOM_ALLOC, CUSTOM_RR, Class};

    extern "C-unwind" fn retain_as_is(this: *mut AnyObject, _: Sel) -> *mut AnyObject {
        this
    }

    fn flags(cls: &AnyClass) -> u32 {
        // SAFETY: objc2's classes are the runtime's.
        unsafe { &*(cls as *const AnyClass).cast::<Class>() }.flags() & (CUSTOM_RR | CUSTOM_ALLOC)
    }

    /// Swizzling NSObject's `-retain` or `+alloc` takes the fast paths away
    /// from every class; putting the original back returns them, except to
    /// a class with an override of its own.
    #[test]
    fn restoring_a_swizzled_root_method_restores_the_fast_paths() {
        let plain = ClassBuilder::new(c"SidestepUnitPlain", NSObject::class()).unwrap().register();
        let mut custom = ClassBuilder::new(c"SidestepUnitCustomRetain", NSObject::class()).unwrap();
        // SAFETY: the signature matches `retain`'s.
        unsafe { custom.add_method(sel!(retain), retain_as_is as extern "C-unwind" fn(_, _) -> _) };
        let custom = custom.register();
        assert_eq!((flags(NSObject::class()), flags(plain), flags(custom)), (0, 0, CUSTOM_RR));

        let replacement: Imp =
            // SAFETY: only compared and stored, never called as anything else.
            unsafe { std::mem::transmute(retain_as_is as extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject) };
        let retain = NSObject::class().instance_method(sel!(retain)).unwrap();
        // SAFETY: the replacement has retain's signature; nothing is retained
        // while it is in place.
        let original = unsafe { retain.set_implementation(replacement) };
        assert_eq!((flags(NSObject::class()), flags(plain), flags(custom)), (CUSTOM_RR, CUSTOM_RR, CUSTOM_RR));
        // SAFETY: the original implementation.
        unsafe { retain.set_implementation(original) };
        assert_eq!((flags(NSObject::class()), flags(plain), flags(custom)), (0, 0, CUSTOM_RR));

        let alloc = NSObject::class().class_method(sel!(alloc)).unwrap();
        let alloc_imp = alloc.implementation();
        // SAFETY: set to what it already is: nothing changes.
        unsafe { alloc.set_implementation(alloc_imp) };
        assert_eq!(flags(plain), 0);
        // SAFETY: an implementation of the same signature, restored at once.
        let original = unsafe { alloc.set_implementation(replacement) };
        assert_eq!(flags(plain), CUSTOM_ALLOC);
        // SAFETY: the original implementation.
        unsafe { alloc.set_implementation(original) };
        assert_eq!(flags(plain), 0);
    }
}
