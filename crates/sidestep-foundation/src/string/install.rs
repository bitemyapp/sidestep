//! Adding methods to a class after it is defined, the way a category does.
//!
//! `define_class!` wants every method of a class in one place, and a class
//! method can't see which class received it. So methods that live apart from
//! their class (NSString's search or path methods, one file each) or need
//! the receiving class (`+stringWithString:` must build an NSMutableString
//! when sent to NSMutableString) are defined on a private helper class, which
//! gives them the type encodings objc2 derives from their Rust signatures,
//! and then copied onto the target with `class_addMethod`.

use std::ffi::CStr;

use objc2::runtime::{AnyClass, ClassBuilder, Imp, NSObject, Sel};
use objc2::{ClassType, ffi};

unsafe extern "C-unwind" {
    // The runtime's, which objc2 declares without its signature. It goes
    // through the method cache, taking no lock once a method is cached.
    #[link_name = "class_getMethodImplementation"]
    fn method_implementation(cls: *const AnyClass, sel: Sel) -> Option<Imp>;
}

/// The implementation instances of `class` answer `sel` with, as an
/// address.
fn implementation(class: &AnyClass, sel: Sel) -> usize {
    // SAFETY: a class and a selector; the runtime resolves the method as a
    // message send would, without sending one.
    unsafe { method_implementation(class, sel) }.map_or(0, |imp| imp as usize)
}

/// Whether `class` is `ours` or a subclass of it that answers each of
/// `sels` with `ours`'s implementation: whether its instances keep our
/// storage, so Sidestep may read it directly rather than send the
/// messages. Fast enough to ask on every call: the class chain and the
/// method cache are read without locks.
pub(crate) fn keeps(class: &AnyClass, ours: &AnyClass, sels: &[Sel]) -> bool {
    crate::attributed::is_kind(class, ours) && sels.iter().all(|&s| implementation(class, s) == implementation(ours, s))
}

/// Copy every instance method of `helper` onto `target`, or onto its
/// metaclass as class methods when `class_side`. The implementations must
/// treat their receiver as the target's instances (or classes).
pub(crate) fn copy_methods(helper: &AnyClass, target: &AnyClass, class_side: bool) {
    let target = if class_side { target.metaclass() } else { target };
    for method in helper.instance_methods().iter() {
        // SAFETY: the helper's methods are written for the target's
        // receivers, and their encodings come from their signatures.
        let added = unsafe {
            ffi::class_addMethod(
                (target as *const AnyClass).cast_mut(),
                method.name(),
                method.implementation(),
                ffi::method_getTypeEncoding(*method),
            )
        };
        assert!(added.as_bool(), "sidestep: {} is defined twice on {}", method.name(), target.name().to_string_lossy());
    }
}

/// Define a helper class named `name` with the methods `add` adds, then copy
/// them onto `target` as class methods.
pub(crate) fn class_methods(target: &AnyClass, name: &CStr, add: impl FnOnce(&mut ClassBuilder)) {
    let mut builder = ClassBuilder::new(name, NSObject::class()).expect("sidestep: helper classes are defined once");
    add(&mut builder);
    copy_methods(builder.register(), target, true);
}
