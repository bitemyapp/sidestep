//! Selectors are interned by name. A `SEL` points to a [`Selector`], laid out
//! like libobjc2's `struct objc_selector`, and equal names always give the
//! same pointer, so selectors compare by address.

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::{LazyLock, OnceLock, RwLock};

use crate::util::{Shared, leak_cstr};
use crate::{Bool, NO, YES};

/// An interned selector.
#[repr(C)]
pub struct Selector {
    name: *const c_char,
    /// libobjc2 supports typed selectors; Sidestep's are all untyped.
    types: *const c_char,
}

pub(crate) type Sel = *const Selector;

static TABLE: LazyLock<RwLock<HashMap<&'static CStr, Shared<Selector>>>> = LazyLock::new(Default::default);

pub(crate) fn register(name: &CStr) -> Sel {
    if let Some(sel) = TABLE.read().unwrap().get(name) {
        return sel.0;
    }
    let mut table = TABLE.write().unwrap();
    if let Some(sel) = table.get(name) {
        return sel.0;
    }
    let name = leak_cstr(name);
    let sel: &'static Selector = Box::leak(Box::new(Selector { name: name.as_ptr(), types: std::ptr::null() }));
    table.insert(name, Shared(sel));
    sel
}

/// The name of `sel`, which must be null or a selector from this module.
pub(crate) unsafe fn name<'a>(sel: Sel) -> &'a CStr {
    if sel.is_null() {
        return c"<null selector>";
    }
    // SAFETY: interned selectors hold leaked C strings.
    unsafe { CStr::from_ptr((*sel).name) }
}

/// Selectors the runtime itself sends.
pub(crate) struct Known {
    pub dealloc: Sel,
    pub initialize: Sel,
    pub retain: Sel,
    pub release: Sel,
    pub autorelease: Sel,
    pub retain_count: Sel,
    pub copy: Sel,
    pub resolve_instance_method: Sel,
    pub resolve_class_method: Sel,
}
// SAFETY: selectors are immutable and never freed.
unsafe impl Send for Known {}
// SAFETY: as above.
unsafe impl Sync for Known {}

pub(crate) fn known() -> &'static Known {
    static KNOWN: OnceLock<Known> = OnceLock::new();
    KNOWN.get_or_init(|| Known {
        dealloc: register(c"dealloc"),
        initialize: register(c"initialize"),
        retain: register(c"retain"),
        release: register(c"release"),
        autorelease: register(c"autorelease"),
        retain_count: register(c"retainCount"),
        copy: register(c"copy"),
        resolve_instance_method: register(c"resolveInstanceMethod:"),
        resolve_class_method: register(c"resolveClassMethod:"),
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sel_registerName(name: *const c_char) -> Sel {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a C string.
    register(unsafe { CStr::from_ptr(name) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sel_getUid(name: *const c_char) -> Sel {
    // SAFETY: same contract.
    unsafe { sel_registerName(name) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sel_getName(sel: Sel) -> *const c_char {
    // SAFETY: the caller passes a selector or null.
    unsafe { name(sel) }.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn sel_isEqual(lhs: Sel, rhs: Sel) -> Bool {
    if lhs == rhs { YES } else { NO }
}
