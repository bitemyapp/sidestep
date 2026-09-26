//! Replacing the root class's own `-retain` and `+alloc`: the runtime's
//! fast paths for them must step aside and call the replacements. This
//! changes NSObject for the whole process, so it has a file of its own and
//! a single test.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObject, Sel};
use objc2::{ClassType, msg_send, sel};

use sidestep as _;

static RETAINS: AtomicUsize = AtomicUsize::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_RETAIN: OnceLock<Imp> = OnceLock::new();
static ORIGINAL_ALLOC: OnceLock<Imp> = OnceLock::new();

unsafe extern "C-unwind" fn counting_retain(this: *mut AnyObject, sel: Sel) -> *mut AnyObject {
    RETAINS.fetch_add(1, Ordering::SeqCst);
    let original: unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject =
        unsafe { std::mem::transmute(*ORIGINAL_RETAIN.get().unwrap()) };
    unsafe { original(this, sel) }
}

unsafe extern "C-unwind" fn counting_alloc(cls: *const AnyClass, sel: Sel) -> *mut AnyObject {
    ALLOCS.fetch_add(1, Ordering::SeqCst);
    let original: unsafe extern "C-unwind" fn(*const AnyClass, Sel) -> *mut AnyObject =
        unsafe { std::mem::transmute(*ORIGINAL_ALLOC.get().unwrap()) };
    unsafe { original(cls, sel) }
}

#[test]
fn replaced_root_methods_are_called() {
    let obj = NSObject::new();
    let retain = NSObject::class().instance_method(sel!(retain)).unwrap();
    let alloc = NSObject::class().class_method(sel!(alloc)).unwrap();
    unsafe {
        let original = retain.set_implementation(std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> *mut AnyObject,
            Imp,
        >(counting_retain));
        ORIGINAL_RETAIN.set(original).unwrap();
        let original = alloc.set_implementation(std::mem::transmute::<
            unsafe extern "C-unwind" fn(*const AnyClass, Sel) -> *mut AnyObject,
            Imp,
        >(counting_alloc));
        ORIGINAL_ALLOC.set(original).unwrap();
    }

    let copy = obj.clone();
    assert_eq!(RETAINS.load(Ordering::SeqCst), 1);
    drop(copy);

    let fresh: Retained<NSObject> = unsafe { msg_send![NSObject::class(), new] };
    assert!(ALLOCS.load(Ordering::SeqCst) >= 1);
    drop(fresh);

    unsafe {
        retain.set_implementation(*ORIGINAL_RETAIN.get().unwrap());
        alloc.set_implementation(*ORIGINAL_ALLOC.get().unwrap());
    }
    let before = (RETAINS.load(Ordering::SeqCst), ALLOCS.load(Ordering::SeqCst));
    drop(obj.clone());
    drop(NSObject::new());
    assert_eq!(before, (RETAINS.load(Ordering::SeqCst), ALLOCS.load(Ordering::SeqCst)));
}
