//! Finding framework classes by name. Each file in tests/ is its own
//! process, and nothing here names a Foundation or AppKit class by its
//! objc2 type until the name lookups are done, so every lookup is the
//! process's first contact with the class.

use std::ffi::{CStr, c_int, c_uint};

use objc2::ffi;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject};
use objc2::{ClassType, msg_send};

use sidestep as _;
// Links AppKit on macOS; Sidestep's AppKit comes with `sidestep`.
use objc2_app_kit as _;

fn class(name: &CStr) -> &'static AnyClass {
    AnyClass::get(name).unwrap_or_else(|| panic!("no class named {name:?}"))
}

fn names(list: &[*const AnyClass]) -> Vec<&'static CStr> {
    // SAFETY: the runtime hands out classes, which have names.
    list.iter().map(|&c| unsafe { CStr::from_ptr(ffi::class_getName(c)) }).collect()
}

#[test]
fn framework_classes_by_name_before_first_use() {
    for name in [c"NSString", c"NSDictionary", c"NSThread", c"NSView", c"NSScrollView", c"NSBezierPath"] {
        let cls = class(name);
        assert_eq!(cls.name(), name);
        assert!(!cls.is_metaclass());
        // SAFETY: plain C strings.
        let (looked_up, required) =
            unsafe { (ffi::objc_lookUpClass(name.as_ptr()), ffi::objc_getRequiredClass(name.as_ptr())) };
        assert_eq!(looked_up, cls as *const AnyClass);
        assert_eq!(required, cls as *const AnyClass);
        // SAFETY: as above.
        let meta = unsafe { ffi::objc_getMetaClass(name.as_ptr()) };
        // SAFETY: a metaclass from the runtime.
        let meta = unsafe { meta.as_ref() }.expect("a metaclass");
        assert!(meta.is_metaclass());
        assert_eq!(meta, cls.metaclass());
        assert_eq!(meta.name(), name);
    }

    // Found by name, the classes are complete.
    let view = class(c"NSView");
    assert_eq!(view.superclass(), Some(class(c"NSResponder")));
    assert_eq!(class(c"NSResponder").superclass(), Some(NSObject::class()));
    assert_eq!(class(c"NSScrollView").superclass(), Some(view));
    let dictionary = class(c"NSDictionary");
    assert!(dictionary.instance_method(objc2::sel!(count)).is_some());
    // SAFETY: +new returns a retained, empty dictionary.
    let empty: Retained<AnyObject> = unsafe { msg_send![dictionary, new] };
    let count: usize = unsafe { msg_send![&*empty, count] };
    assert_eq!(count, 0);
    let is_dictionary: bool = unsafe { msg_send![&*empty, isKindOfClass: dictionary] };
    assert!(is_dictionary);

    // The class found by name is the one objc2 links against.
    assert_eq!(dictionary, objc2_foundation::NSDictionary::<AnyObject, AnyObject>::class());
}

#[test]
fn unknown_names() {
    assert!(AnyClass::get(c"SidestepNoSuchClass").is_none());
    // SAFETY: a C string.
    assert!(unsafe { ffi::objc_lookUpClass(c"SidestepNoSuchClass".as_ptr()) }.is_null());
    // SAFETY: as above.
    assert!(unsafe { ffi::objc_getMetaClass(c"SidestepNoSuchClass".as_ptr()) }.is_null());
}

#[test]
fn class_lists_include_unused_framework_classes() {
    let mut len: c_uint = 0;
    // SAFETY: the list is freed below.
    let list = unsafe { ffi::objc_copyClassList(&mut len) };
    assert!(!list.is_null());
    // SAFETY: the runtime returned `len` classes.
    let copied = names(unsafe { std::slice::from_raw_parts(list, len as usize) });
    // SAFETY: allocated by the runtime with malloc.
    unsafe { ffi::free(list.cast()) };
    for name in [c"NSObject", c"NSString", c"NSTimer", c"NSRunLoop", c"NSWindow", c"NSColor"] {
        assert!(copied.contains(&name), "{name:?} missing from objc_copyClassList");
    }
    // A listed class nothing has used is a complete object: its metaclass,
    // like every metaclass, is an instance of the root metaclass.
    let timer = listed_class(c"NSTimer");
    // SAFETY: classes and metaclasses are objects.
    let meta = unsafe { ffi::object_getClass(timer.cast()) };
    assert!(!meta.is_null());
    // SAFETY: as above.
    let root_meta = unsafe { ffi::object_getClass(meta.cast()) };
    assert_eq!(root_meta, NSObject::class().metaclass() as *const AnyClass);

    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { ffi::objc_getClassList(std::ptr::null_mut(), 0) };
    assert!(count > 0);
    let mut buffer = vec![std::ptr::null::<AnyClass>(); count as usize + 16];
    // SAFETY: the buffer has room for `buffer.len()` classes.
    let filled = unsafe { ffi::objc_getClassList(buffer.as_mut_ptr(), buffer.len() as c_int) };
    let filled = (filled as usize).min(buffer.len());
    let listed = names(&buffer[..filled]);
    for name in [c"NSObject", c"NSDictionary", c"NSApplication", c"NSEvent"] {
        assert!(listed.contains(&name), "{name:?} missing from objc_getClassList");
    }
}

/// The class named `name`, found by walking `objc_copyClassList`, which
/// loads nothing.
fn listed_class(name: &CStr) -> *const AnyClass {
    let mut len: c_uint = 0;
    // SAFETY: the list is freed below.
    let list = unsafe { ffi::objc_copyClassList(&mut len) };
    // SAFETY: the runtime returned `len` classes.
    let classes = unsafe { std::slice::from_raw_parts(list, len as usize) }.to_vec();
    // SAFETY: allocated by the runtime with malloc.
    unsafe { ffi::free(list.cast()) };
    let at = names(&classes).iter().position(|&n| n == name).unwrap_or_else(|| panic!("{name:?} not listed"));
    classes[at]
}

block2::global_block! {
    static GLOBAL = || {};
}

/// The runtime's block classes have names too, and blocks are instances.
#[test]
fn block_classes_by_name() {
    let global = class(c"__NSGlobalBlock__");
    let block: *const block2::Block<dyn Fn()> = &*GLOBAL;
    // SAFETY: a block is an object.
    assert_eq!(unsafe { ffi::object_getClass(block.cast()) }, global as *const AnyClass);
    let heap = block2::RcBlock::new(|| {});
    // SAFETY: as above.
    let heap_class = unsafe { ffi::object_getClass(block2::RcBlock::as_ptr(&heap).cast()) };
    assert_eq!(heap_class, class(c"__NSMallocBlock__") as *const AnyClass);
    assert!(AnyClass::get(c"__NSStackBlock__").is_some());
}

#[test]
fn framework_class_names_are_taken() {
    // A class nothing has used yet still owns its name.
    assert!(ClassBuilder::new(c"NSNotification", NSObject::class()).is_none());
    assert!(ClassBuilder::new(c"NSFont", NSObject::class()).is_none());
}
