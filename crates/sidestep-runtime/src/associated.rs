//! Associated objects: extra references hung off an object by key, released
//! when the object is freed.
//!
//! The table is sharded by object address (see `util::Sharded`), so threads
//! working with different objects rarely wait for each other. The low byte
//! of a policy says how the value is held: assigned, retained or copied.
//! The atomic policies (`OBJC_ASSOCIATION_RETAIN`, `OBJC_ASSOCIATION_COPY`)
//! also make the getter hand the value out retained and autoreleased,
//! taken under the table's lock, so it stays valid when another thread
//! replaces it; the nonatomic ones hand it out as it is.

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use crate::arc::{objc_autorelease, objc_release, objc_retain};
use crate::object::{HAS_ASSOCIATED, IMMORTAL, Kind, Object, header, kind};
use crate::selector::known;
use crate::util::{AddrMap, Sharded, lock};

type Id = *mut Object;

const RETAIN: usize = 1;
const COPY: usize = 3;
/// The getter bits of the atomic policies.
const ATOMIC: usize = 0x300;

#[derive(Clone, Copy)]
struct Entry {
    value: usize,
    /// The table holds a reference to `value`.
    owned: bool,
    /// The getter returns `value` retained and autoreleased.
    atomic: bool,
}

/// Object to key to value.
static TABLE: Sharded<AddrMap<Entry>> = Sharded::new();

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
    // Taken before locking: -copy is a message, which may do anything.
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
    if !stored.is_null() && matches!(unsafe { kind(obj) }, Kind::Counted) {
        // Immortal objects are never freed either, and their headers are
        // never written.
        // SAFETY: as above.
        let rc = unsafe { &header(obj).rc };
        if rc.load(Ordering::Relaxed) & (HAS_ASSOCIATED | IMMORTAL) == 0 {
            rc.fetch_or(HAS_ASSOCIATED, Ordering::Relaxed);
        }
    }
    let (obj, key) = (obj as usize, key as usize);
    let old = {
        let mut shard = lock(TABLE.shard(obj));
        if stored.is_null() {
            let old = shard.get_mut(&obj).and_then(|entries| entries.remove(&key));
            if shard.get(&obj).is_some_and(AddrMap::is_empty) {
                shard.remove(&obj);
            }
            old
        } else {
            let entry = Entry { value: stored as usize, owned, atomic: policy & ATOMIC != 0 };
            shard.entry(obj).or_default().insert(key, entry)
        }
    };
    if let Some(Entry { value, owned: true, .. }) = old {
        // SAFETY: the table owned this reference.
        unsafe { objc_release(value as Id) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_getAssociatedObject(obj: Id, key: *const c_void) -> Id {
    if obj.is_null() {
        return std::ptr::null_mut();
    }
    let found = {
        let shard = lock(TABLE.shard(obj as usize));
        let entry = shard.get(&(obj as usize)).and_then(|e| e.get(&(key as usize)).copied());
        if let Some(Entry { value, owned: true, atomic: true }) = entry {
            // Retained under the lock, so a concurrent setter can't free it
            // first.
            // SAFETY: the table keeps owned values alive.
            unsafe { objc_retain(value as Id) };
        }
        entry
    };
    match found {
        // SAFETY: retained above, and handed out autoreleased.
        Some(Entry { value, owned: true, atomic: true }) => unsafe { objc_autorelease(value as Id) },
        Some(Entry { value, .. }) => value as Id,
        None => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_removeAssociatedObjects(obj: Id) {
    remove_all(obj);
}

/// Release everything associated with `obj`.
pub(crate) fn remove_all(obj: Id) {
    let entries = lock(TABLE.shard(obj as usize)).remove(&(obj as usize));
    for entry in entries.into_iter().flat_map(|e| e.into_values()) {
        if entry.owned {
            // SAFETY: the table owned this reference.
            unsafe { objc_release(entry.value as Id) };
        }
    }
}
