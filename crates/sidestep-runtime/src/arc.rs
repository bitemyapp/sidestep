//! The ARC entry points: retain and release, autorelease pools, and weak
//! references.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::transmute;
use std::sync::atomic::{Ordering, fence};
use std::sync::{LazyLock, Mutex};

use crate::blocks;
use crate::class::CUSTOM_RR;
use crate::message::objc_msg_lookup;
use crate::object::{DEALLOCATING, IMMORTAL, Kind, Object, RC_ONE, WEAKLY_REFERENCED, header, isa, kind};
use crate::selector::{Sel, known};
use crate::util::lock;

type Id = *mut Object;

/// Send a message taking no arguments.
unsafe fn send<R>(obj: Id, sel: Sel) -> R {
    // SAFETY: the caller passes a live object; the method takes no
    // arguments and returns `R`.
    unsafe {
        let imp = objc_msg_lookup(obj, sel).expect("lookup never fails");
        let imp: unsafe extern "C-unwind" fn(Id, Sel) -> R = transmute(imp);
        imp(obj, sel)
    }
}

fn has_custom_rr(obj: Id) -> bool {
    // SAFETY: callers pass live, counted objects.
    unsafe { isa(obj) }.flags() & CUSTOM_RR != 0
}

/// Count one more reference without consulting overrides.
pub(crate) unsafe fn raw_retain(obj: Id) {
    // SAFETY: the caller passes a live, counted object.
    let h = unsafe { header(obj) };
    if h.rc.load(Ordering::Relaxed) & IMMORTAL == 0 {
        h.rc.fetch_add(RC_ONE, Ordering::Relaxed);
    }
}

/// Count one reference fewer, deallocating at zero, without consulting
/// overrides.
pub(crate) unsafe fn raw_release(obj: Id) {
    // SAFETY: the caller passes a live, counted object.
    let h = unsafe { header(obj) };
    let mut cur = h.rc.load(Ordering::Relaxed);
    loop {
        if cur & (IMMORTAL | DEALLOCATING) != 0 {
            return;
        }
        let new = if cur < RC_ONE { cur | DEALLOCATING } else { cur - RC_ONE };
        match h.rc.compare_exchange_weak(cur, new, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => cur = actual,
        }
    }
    if cur < RC_ONE {
        fence(Ordering::Acquire);
        // SAFETY: the last reference is gone; -dealloc frees the object.
        unsafe { send::<()>(obj, known().dealloc) };
    }
}

/// Retain unless the object is already deallocating. Used by weak loads.
unsafe fn try_retain(obj: Id) -> bool {
    // SAFETY: the caller passes a live, counted object.
    let h = unsafe { header(obj) };
    let mut cur = h.rc.load(Ordering::Relaxed);
    loop {
        if cur & IMMORTAL != 0 {
            return true;
        }
        if cur & DEALLOCATING != 0 {
            return false;
        }
        match h.rc.compare_exchange_weak(cur, cur + RC_ONE, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(actual) => cur = actual,
        }
    }
}

/// The number `-retainCount` reports.
pub(crate) unsafe fn retain_count(obj: Id) -> usize {
    // SAFETY: the caller passes a live, counted object.
    let bits = unsafe { header(obj) }.rc.load(Ordering::Relaxed);
    if bits & IMMORTAL != 0 { usize::MAX } else { bits / RC_ONE + 1 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retain(obj: Id) -> Id {
    if obj.is_null() {
        return obj;
    }
    // SAFETY: the caller passes a live object.
    match unsafe { kind(obj) } {
        Kind::Class => obj,
        // SAFETY: as above.
        Kind::Block => unsafe { blocks::retain(obj) },
        // SAFETY: as above.
        Kind::Counted if has_custom_rr(obj) => unsafe { send(obj, known().retain) },
        Kind::Counted => {
            // SAFETY: as above.
            unsafe { raw_retain(obj) };
            obj
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_release(obj: Id) {
    if obj.is_null() {
        return;
    }
    // SAFETY: the caller passes a live object it owns a reference to.
    match unsafe { kind(obj) } {
        Kind::Class => {}
        // SAFETY: as above.
        Kind::Block => unsafe { blocks::release(obj) },
        // SAFETY: as above.
        Kind::Counted if has_custom_rr(obj) => unsafe { send::<()>(obj, known().release) },
        // SAFETY: as above.
        Kind::Counted => unsafe { raw_release(obj) },
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainBlock(obj: Id) -> Id {
    // SAFETY: the caller passes a block or null.
    unsafe { blocks::_Block_copy(obj.cast()) }.cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_storeStrong(addr: *mut Id, value: Id) {
    // SAFETY: the caller passes a valid location and object.
    unsafe {
        let value = objc_retain(value);
        let old = addr.replace(value);
        objc_release(old);
    }
}

// Autorelease pools: one stack of pending releases per thread. A pool token
// is the stack height at push time, plus one so it is never null.

struct Pool(Vec<usize>);

impl Drop for Pool {
    fn drop(&mut self) {
        while let Some(obj) = self.0.pop() {
            // SAFETY: the pool owns one reference to each entry. Objects
            // autoreleased from here on can't be pooled and are leaked.
            unsafe { objc_release(obj as Id) };
        }
    }
}

thread_local!(static POOL: RefCell<Pool> = const { RefCell::new(Pool(Vec::new())) });

/// Add `obj` to the current pool, doing the counting part of -autorelease.
pub(crate) fn pool_add(obj: Id) {
    let _ = POOL.try_with(|pool| pool.borrow_mut().0.push(obj as usize));
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn objc_autoreleasePoolPush() -> *mut c_void {
    let height = POOL.try_with(|pool| pool.borrow().0.len()).unwrap_or(0);
    (height + 1) as *mut c_void
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn objc_autoreleasePoolPop(token: *mut c_void) {
    let height = (token as usize).saturating_sub(1);
    loop {
        let next = POOL.try_with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.0.len() > height { pool.0.pop() } else { None }
        });
        match next {
            // SAFETY: the pool owned this reference.
            Ok(Some(obj)) => unsafe { objc_release(obj as Id) },
            _ => break,
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_autorelease(obj: Id) -> Id {
    if obj.is_null() {
        return obj;
    }
    // SAFETY: the caller passes a live object it owns a reference to.
    match unsafe { kind(obj) } {
        Kind::Class => obj,
        // SAFETY: as above.
        Kind::Counted if has_custom_rr(obj) => unsafe { send(obj, known().autorelease) },
        _ => {
            pool_add(obj);
            obj
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_autoreleaseReturnValue(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(obj) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainAutorelease(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(objc_retain(obj)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainAutoreleaseReturnValue(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(objc_retain(obj)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_retainAutoreleasedReturnValue(obj: Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_retain(obj) }
}

// Weak references: a table from object to the locations that weakly refer
// to it. Deallocation zeroes them. Classes, blocks and immortal objects are
// never deallocated, so weak references to them are stored untracked.

static WEAK: LazyLock<Mutex<HashMap<usize, Vec<usize>>>> = LazyLock::new(Default::default);

unsafe fn is_tracked(obj: Id) -> bool {
    // SAFETY: the caller passes a live object.
    matches!(unsafe { kind(obj) }, Kind::Counted) && unsafe { header(obj) }.rc.load(Ordering::Relaxed) & IMMORTAL == 0
}

/// Zero every weak reference to `obj`, which is being freed.
pub(crate) fn clear_weak(obj: Id) {
    let locations = lock(&WEAK).remove(&(obj as usize));
    for location in locations.into_iter().flatten() {
        let location = location as *mut Id;
        // SAFETY: registered locations stay valid until objc_destroyWeak.
        unsafe {
            if *location == obj {
                *location = std::ptr::null_mut();
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_storeWeak(location: *mut Id, value: Id) -> Id {
    let mut table = lock(&WEAK);
    // SAFETY: the caller passes a valid location holding null or a weakly
    // stored object.
    let old = unsafe { *location };
    if !old.is_null() {
        if let Some(list) = table.get_mut(&(old as usize)) {
            list.retain(|&l| l != location as usize);
            if list.is_empty() {
                table.remove(&(old as usize));
            }
        }
    }
    // SAFETY: the caller passes a live object or null.
    let stored = if value.is_null() || !unsafe { is_tracked(value) } {
        value
    } else if unsafe { header(value) }.rc.load(Ordering::Acquire) & DEALLOCATING != 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: as above.
        unsafe { header(value) }.rc.fetch_or(WEAKLY_REFERENCED, Ordering::AcqRel);
        table.entry(value as usize).or_default().push(location as usize);
        value
    };
    // SAFETY: the caller passes a valid location.
    unsafe { *location = stored };
    stored
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_initWeak(location: *mut Id, value: Id) -> Id {
    // SAFETY: the caller passes a valid, uninitialized location.
    unsafe {
        *location = std::ptr::null_mut();
        objc_storeWeak(location, value)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_destroyWeak(location: *mut Id) {
    // SAFETY: forwarded contract.
    unsafe { objc_storeWeak(location, std::ptr::null_mut()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_loadWeakRetained(location: *mut Id) -> Id {
    let _table = lock(&WEAK);
    // SAFETY: the caller passes a valid location; holding the table lock
    // keeps a tracked object from being freed while we retain it.
    unsafe {
        let obj = *location;
        if obj.is_null() {
            return obj;
        }
        if !is_tracked(obj) {
            return objc_retain(obj);
        }
        if try_retain(obj) { obj } else { std::ptr::null_mut() }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_loadWeak(location: *mut Id) -> Id {
    // SAFETY: forwarded contract.
    unsafe { objc_autorelease(objc_loadWeakRetained(location)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_copyWeak(to: *mut Id, from: *mut Id) {
    // SAFETY: forwarded contract.
    unsafe {
        let obj = objc_loadWeakRetained(from);
        objc_initWeak(to, obj);
        objc_release(obj);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn objc_moveWeak(to: *mut Id, from: *mut Id) {
    // SAFETY: forwarded contract.
    unsafe {
        objc_copyWeak(to, from);
        objc_destroyWeak(from);
    }
}
