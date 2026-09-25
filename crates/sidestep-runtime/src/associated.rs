//! Associated objects: extra references hung off an object by key, released
//! when the object is freed.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::{LazyLock, Mutex};

use crate::arc::{objc_autorelease, objc_release, objc_retain};
use crate::object::{HAS_ASSOCIATED, Kind, Object, header, kind};
use crate::selector::known;
use crate::util::lock;

type Id = *mut Object;

const RETAIN: usize = 1;
const COPY: usize = 3;

#[derive(Clone, Copy)]
struct Entry {
    value: usize,
    owned: bool,
}

static TABLE: LazyLock<Mutex<HashMap<usize, HashMap<usize, Entry>>>> = LazyLock::new(Default::default);

unsafe fn copy(value: Id) -> Id {
    // SAFETY: -copy takes no arguments and returns a +1 object.
    unsafe {
        let sel = known().copy;
        let imp = crate::message::objc_msg_lookup(value, sel).expect("lookup never fails");
        let imp: unsafe extern "C-unwind" fn(Id, crate::selector::Sel) -> Id = std::mem::transmute(imp);
        imp(value, sel)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_setAssociatedObject(obj: Id, key: *const c_void, value: Id, policy: usize) {
    if obj.is_null() {
        return;
    }
    // SAFETY: the caller passes live objects.
    let (stored, owned) = unsafe {
        match policy & 0xff {
            _ if value.is_null() => (value, false),
            RETAIN => (objc_retain(value), true),
            COPY => (copy(value), true),
            // ASSIGN, and anything unknown.
            _ => (value, false),
        }
    };
    // Classes and blocks have no header to flag; their entries are simply
    // never cleaned up, like the objects themselves.
    // SAFETY: as above.
    if matches!(unsafe { kind(obj) }, Kind::Counted) {
        unsafe { header(obj) }.rc.fetch_or(HAS_ASSOCIATED, Ordering::AcqRel);
    }
    let old = {
        let mut table = lock(&TABLE);
        let entries = table.entry(obj as usize).or_default();
        if stored.is_null() {
            entries.remove(&(key as usize))
        } else {
            entries.insert(key as usize, Entry { value: stored as usize, owned })
        }
    };
    if let Some(Entry { value, owned: true }) = old {
        // SAFETY: the table owned this reference.
        unsafe { objc_release(value as Id) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_getAssociatedObject(obj: Id, key: *const c_void) -> Id {
    let entry = lock(&TABLE).get(&(obj as usize)).and_then(|e| e.get(&(key as usize)).copied());
    match entry {
        // SAFETY: the table keeps owned values alive; hand out an
        // autoreleased reference, as the atomic policies do.
        Some(Entry { value, owned: true }) => unsafe { objc_autorelease(objc_retain(value as Id)) },
        Some(Entry { value, owned: false }) => value as Id,
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_removeAssociatedObjects(obj: Id) {
    remove_all(obj);
}

/// Release everything associated with `obj`.
pub(crate) fn remove_all(obj: Id) {
    let entries = lock(&TABLE).remove(&(obj as usize));
    for entry in entries.into_iter().flat_map(|e| e.into_values()) {
        if entry.owned {
            // SAFETY: the table owned this reference.
            unsafe { objc_release(entry.value as Id) };
        }
    }
}
