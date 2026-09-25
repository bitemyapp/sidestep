//! Message dispatch in the libobjc2 style: `objc_msg_lookup` returns the
//! implementation and the caller calls it, so no assembly trampoline is
//! needed.

use std::mem::transmute;
use std::sync::atomic::Ordering;

use crate::class::{Class, INITIALIZED, class_ref, ensure_initialized, ensure_loaded, lookup_imp};
use crate::object::{Object, isa};
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

/// What unknown selectors resolve to. Panics with the message Apple's
/// runtime raises as an exception, which unwinds into the Rust caller.
unsafe extern "C-unwind" fn unrecognized_selector(receiver: Id, sel: Sel) {
    // SAFETY: called as a method, so the receiver is live.
    let cls = unsafe { isa(receiver) };
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
/// metaclass), falling back to the unrecognized-selector handler.
pub(crate) unsafe fn method_for(cls: &'static Class, sel: Sel) -> Imp {
    if let Some(imp) = lookup_imp(cls, sel) {
        return imp;
    }
    // SAFETY: forwarded contract.
    if let Some(imp) = unsafe { resolve(cls, sel) } {
        return imp;
    }
    // SAFETY: the handler is only called as a method.
    unsafe { transmute::<unsafe extern "C-unwind" fn(Id, Sel), Imp>(unrecognized_selector) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_msg_lookup(receiver: Id, sel: Sel) -> Option<Imp> {
    if receiver.is_null() {
        return Some(as_imp(nil_method));
    }
    // SAFETY: the caller passes a live object.
    let cls = unsafe { isa(receiver) };
    if cls.flags.load(Ordering::Acquire) & INITIALIZED == 0 {
        ensure_loaded(cls);
        ensure_initialized(cls.instance_class());
    }
    // SAFETY: `cls` is loaded.
    Some(unsafe { method_for(cls, sel) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_msg_lookup_super(sup: *const ObjcSuper, sel: Sel) -> Option<Imp> {
    // SAFETY: the caller passes a valid objc_super.
    let sup = unsafe { &*sup };
    if sup.receiver.is_null() {
        return Some(as_imp(nil_method));
    }
    // SAFETY: the caller passes a class.
    let cls = unsafe { class_ref(sup.super_class) }?;
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
