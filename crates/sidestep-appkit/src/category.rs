//! Methods that one area gives a class another area defines, as an
//! Objective-C category would: a helper `define_class!` type holds them,
//! and `install` copies them onto the class, from the class's
//! `static_class!` loader (lib.rs), so they exist before its first message.
//! The helper's Rust signatures give the encodings, so objc2's debug-mode
//! message checks still hold. Instances of the class are what the methods
//! see as `self`: a helper's methods must only use what the target's
//! instances have (its superclass chain and associated objects), never the
//! helper's own instance variables.
//!
//! Two areas giving a class the same selector is a mistake this catches:
//! debug builds panic when the class already has the method. (A later
//! runtime change moves this to a link-time section every area shares.)

use objc2::runtime::{AnyClass, Sel};

/// Copy `selectors`' instance methods from `helper` onto `target`.
pub(crate) fn install(helper: &AnyClass, target: &AnyClass, selectors: &[Sel]) {
    for &sel in selectors {
        let method = helper.instance_method(sel).unwrap_or_else(|| panic!("sidestep: {helper} has no -{sel}"));
        let own = target.instance_method(sel).is_some_and(|m| {
            // Inherited methods may be replaced; the class's own may not.
            target.superclass().and_then(|s| s.instance_method(sel)).is_none_or(|inherited| !std::ptr::eq(m, inherited))
        });
        debug_assert!(!own, "sidestep: {target} already has -{sel}; two areas define it");
        // SAFETY: the implementation treats its receiver as an instance of
        // `target` (see the module documentation), and the encoding is the
        // helper method's own.
        unsafe {
            objc2::ffi::class_addMethod(
                (target as *const AnyClass).cast_mut(),
                sel,
                method.implementation(),
                objc2::ffi::method_getTypeEncoding(method),
            );
        }
    }
}
