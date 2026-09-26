//! Message dispatch in the libobjc2 style: `objc_msg_lookup` returns the
//! implementation and the caller calls it, so no assembly trampoline is
//! needed except to forward a message to another object (see `forward`).

use std::hint::cold_path;
use std::mem::transmute;
use std::sync::atomic::Ordering;

use crate::class::{Class, INITIALIZED, class_ref, ensure_initialized, ensure_loaded, lookup_imp};
use crate::forward;
use crate::object::{Object, isa, isa_relaxed};
use crate::selector::{self, Sel, known};
use crate::{Bool, Imp, NO, YES};

type Id = *mut Object;

/// `objc_super` from the ABI.
#[repr(C)]
pub struct ObjcSuper {
    receiver: Id,
    super_class: *const Class,
}

/// What messages to nil resolve to: returns zero.
unsafe extern "C-unwind" fn nil_method(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

/// What unknown selectors resolve to: sends `-doesNotRecognizeSelector:`,
/// which raises (panics), as Apple's runtime does.
///
/// On x86_64 a message returning a struct in memory passes the address to
/// write it to first, so the receiver and selector arrive one register
/// later; the second register is then the receiver, never a selector.
#[cfg(target_arch = "x86_64")]
unsafe extern "C-unwind" fn unrecognized_selector(first: usize, second: usize, third: usize) {
    let (receiver, sel) = if selector::is_selector(second) { (first, second) } else { (second, third) };
    // SAFETY: called as a method, so the receiver is live and the
    // selector valid.
    unsafe { does_not_recognize(receiver as Id, sel as Sel) }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe extern "C-unwind" fn unrecognized_selector(receiver: Id, sel: Sel) {
    // SAFETY: called as a method, so the receiver is live and the
    // selector valid.
    unsafe { does_not_recognize(receiver, sel) }
}

/// `[receiver doesNotRecognizeSelector:sel]`, which raises (panics). A
/// class overriding it to return has nothing to return to, and a root
/// class without it gets the same panic from here.
///
/// # Safety
/// `receiver` must be live and `sel` a selector.
pub(crate) unsafe fn does_not_recognize(receiver: Id, sel: Sel) -> ! {
    // SAFETY: guaranteed by the caller.
    let cls = unsafe { isa(receiver) };
    let report = known().does_not_recognize;
    // Found without resolving or forwarding, which could come back here.
    if let Some(imp) = lookup_imp(cls, report) {
        // SAFETY: the method takes a selector and returns nothing.
        unsafe {
            let imp: unsafe extern "C-unwind" fn(Id, Sel, Sel) = transmute(imp);
            imp(receiver, report, sel);
        }
    }
    let (prefix, what) = if cls.is_meta() { ('+', "class") } else { ('-', "instance") };
    // SAFETY: the selector came from the runtime.
    let sel = unsafe { selector::name(sel) };
    panic!(
        "{prefix}[{} {}]: unrecognized selector sent to {what} {receiver:p}",
        cls.instance_class().name().to_string_lossy(),
        sel.to_string_lossy(),
    );
}

fn as_imp(f: unsafe extern "C-unwind" fn(Id, Sel) -> Id) -> Imp {
    // SAFETY: all IMPs are called through a cast to their real signature.
    unsafe { transmute(f) }
}

/// Ask the class to add a method for `sel` through
/// `+resolveInstanceMethod:` or `+resolveClassMethod:`.
unsafe fn resolve(cls: &'static Class, sel: Sel) -> Option<Imp> {
    let k = known();
    let (class_object, resolver) =
        if cls.is_meta() { (cls.peer(), k.resolve_class_method) } else { (cls, k.resolve_instance_method) };
    let imp = lookup_imp(class_object.metaclass(), resolver)?;
    // SAFETY: the resolvers take a selector and return BOOL.
    let imp: unsafe extern "C-unwind" fn(*const Class, Sel, Sel) -> Bool = unsafe { transmute(imp) };
    // SAFETY: `class_object` is a loaded class.
    if unsafe { imp(class_object, resolver, sel) } != NO { lookup_imp(cls, sel) } else { None }
}

/// Resolve `sel` for instances of `cls` (or for class objects, if `cls` is a
/// metaclass): its method, one `+resolveInstanceMethod:` or
/// `+resolveClassMethod:` adds, the forwarding trampoline if the class
/// forwards, or else the unrecognized-selector handler.
pub(crate) unsafe fn method_for(cls: &'static Class, sel: Sel) -> Imp {
    // Any cached implementation, the forwarding trampoline included: this
    // is what a message would call.
    if let Some(imp) = crate::cache::probe(cls, sel) {
        return imp;
    }
    let epoch = crate::cache::epoch();
    if let Some(imp) = lookup_imp(cls, sel) {
        return imp;
    }
    // SAFETY: forwarded contract.
    if let Some(imp) = unsafe { resolve(cls, sel) } {
        return imp;
    }
    if let Some(trampoline) = forward::trampoline()
        && forward::forwards(cls)
    {
        // Cached like a method, so the next message goes straight to the
        // trampoline; the target itself is asked for each time.
        crate::cache::remember(cls, sel, trampoline, epoch);
        return trampoline;
    }
    // SAFETY: the handler is only called as a method.
    unsafe { transmute::<*const (), Imp>(unrecognized_selector as *const ()) }
}

/// Every message goes through here. The hit path is a handful of
/// instructions with no stack frame: the receiver's class, its cache word,
/// one slot (see `cache`). A class only has a cache once it is loaded and
/// initialized, so everything else is on the miss path.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_msg_lookup(receiver: Id, sel: Sel) -> Option<Imp> {
    if receiver.is_null() {
        cold_path();
        return Some(as_imp(nil_method));
    }
    // SAFETY: the caller passes a live object.
    let cls = unsafe { isa_relaxed(receiver) };
    match crate::cache::probe(cls, sel) {
        Some(imp) => Some(imp),
        None => {
            cold_path();
            // SAFETY: forwarded contract.
            unsafe { lookup_miss(receiver, sel) }
        }
    }
}

#[cold]
#[inline(never)]
unsafe fn lookup_miss(receiver: Id, sel: Sel) -> Option<Imp> {
    // SAFETY: the caller passes a live object.
    let cls = unsafe { isa(receiver) };
    if cls.flags.load(Ordering::Acquire) & INITIALIZED == 0 {
        ensure_loaded(cls);
        ensure_initialized(cls.instance_class());
    }
    // SAFETY: `cls` is loaded.
    Some(unsafe { method_for(cls, sel) })
}

/// Like `objc_msg_lookup`, with the same frameless hit path: a class with a
/// cache is loaded and initialized, so the cache is tried first.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_msg_lookup_super(sup: *const ObjcSuper, sel: Sel) -> Option<Imp> {
    // SAFETY: the caller passes a valid objc_super.
    let sup = unsafe { &*sup };
    if sup.receiver.is_null() {
        cold_path();
        return Some(as_imp(nil_method));
    }
    // SAFETY: the caller passes a class or null.
    if let Some(cls) = unsafe { sup.super_class.as_ref() }
        && let Some(imp) = crate::cache::probe(cls, sel)
    {
        return Some(imp);
    }
    cold_path();
    // SAFETY: forwarded contract.
    unsafe { super_miss(sup.super_class, sel) }
}

#[cold]
#[inline(never)]
unsafe fn super_miss(cls: *const Class, sel: Sel) -> Option<Imp> {
    // SAFETY: the caller passes a class or null.
    let cls = unsafe { class_ref(cls) }?;
    ensure_initialized(cls.instance_class());
    // SAFETY: `cls` is loaded.
    Some(unsafe { method_for(cls, sel) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn class_getMethodImplementation(cls: *const Class, sel: Sel) -> Option<Imp> {
    // SAFETY: the caller passes a class or null.
    let cls = unsafe { class_ref(cls) }?;
    // SAFETY: `cls` is loaded.
    Some(unsafe { method_for(cls, sel) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn class_respondsToSelector(cls: *const Class, sel: Sel) -> Bool {
    // SAFETY: the caller passes a class or null.
    let Some(cls) = (unsafe { class_ref(cls) }) else { return NO };
    // SAFETY: `cls` is loaded.
    if lookup_imp(cls, sel).is_some() || unsafe { resolve(cls, sel) }.is_some() { YES } else { NO }
}
