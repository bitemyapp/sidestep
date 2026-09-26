//! `NSProxy`, the other root class: an object that stands in for another
//! and forwards it the messages it gets.
//!
//! It implements only what makes it an object of its own: reference
//! counting, `-class`, `-superclass`, `-self`, `-hash` and `-isEqual:` by
//! identity, `-isProxy`, `-description`, and `-performSelector:` (which
//! sends the message to the proxy itself). Everything else is forwarded,
//! `-isKindOfClass:`, `-respondsToSelector:`, `-conformsToProtocol:` and
//! `-init` included, as on Apple's Foundation: the runtime's forwarding
//! sends `-methodSignatureForSelector:` and `-forwardInvocation:`, which a
//! subclass overrides and NSProxy's own raise. Class messages are the
//! class's own, as for any class.
//!
//! Its methods are NSObject's (see `nsobject`), for the same encodings and
//! the same reference-counting fast paths: a root class's own `-retain`
//! and `-release` are defaults the fast paths stand in for.

use objc2::runtime::{AnyClass, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::sel;

use crate::nsobject::{
    Id, alloc, alloc_with_zone, autorelease, bool, class, class_conforms_to_protocol, class_description, class_self,
    class_superclass, dealloc, debug_description, description, does_not_recognize_selector, hash, initialize,
    instances_respond_to_selector, is_equal, is_kind_of_class, is_member_of_class, is_subclass_of_class,
    perform_selector, perform_selector_with, perform_selector_with_with, release, responds_to_selector, retain,
    retain_count, self_, superclass, zone,
};

crate::static_class!(pub NSPROXY_CLASS, NSPROXY_METACLASS = "NSProxy", load);

extern "C-unwind" fn is_proxy(_: Id, _: Sel) -> Bool {
    bool(true)
}

extern "C-unwind" fn method_signature(_: Id, _: Sel, _sel: Sel) -> Id {
    panic!("*** -[NSProxy methodSignatureForSelector:] called!");
}

extern "C-unwind" fn forward_invocation(_: Id, _: Sel, _invocation: Id) {
    panic!("*** -[NSProxy forwardInvocation:] called!");
}

// NSObject's instance methods, taking a class object: they look at its
// class, the metaclass.

extern "C-unwind" fn class_responds_to_selector(cls: *const AnyClass, cmd: Sel, sel: Sel) -> Bool {
    responds_to_selector(cls.cast_mut().cast(), cmd, sel)
}

extern "C-unwind" fn class_is_kind_of_class(cls: *const AnyClass, cmd: Sel, other: *const AnyClass) -> Bool {
    is_kind_of_class(cls.cast_mut().cast(), cmd, other)
}

extern "C-unwind" fn class_is_member_of_class(cls: *const AnyClass, cmd: Sel, other: *const AnyClass) -> Bool {
    is_member_of_class(cls.cast_mut().cast(), cmd, other)
}

fn load() {
    let mut builder = ClassBuilder::root(c"NSProxy", initialize as extern "C-unwind" fn(_, _))
        .expect("sidestep: NSProxy is defined once");
    let this_sel = Sel::register(c"self");
    // SAFETY: each function's signature matches the selector's convention.
    unsafe {
        builder.add_method(sel!(dealloc), dealloc as unsafe extern "C-unwind" fn(_, _));
        builder.add_method(sel!(retain), retain as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(release), release as unsafe extern "C-unwind" fn(_, _));
        builder.add_method(sel!(autorelease), autorelease as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(retainCount), retain_count as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(class), class as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(superclass), superclass as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(this_sel, self_ as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(hash), hash as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(isEqual:), is_equal as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(isProxy), is_proxy as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(zone), zone as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(description), description as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(debugDescription), debug_description as unsafe extern "C-unwind" fn(_, _) -> _);
        builder
            .add_method(sel!(doesNotRecognizeSelector:), does_not_recognize_selector as extern "C-unwind" fn(_, _, _));
        builder.add_method(sel!(performSelector:), perform_selector as unsafe extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(
            sel!(performSelector:withObject:),
            perform_selector_with as unsafe extern "C-unwind" fn(_, _, _, _) -> _,
        );
        builder.add_method(
            sel!(performSelector:withObject:withObject:),
            perform_selector_with_with as unsafe extern "C-unwind" fn(_, _, _, _, _) -> _,
        );
        builder.add_method(sel!(methodSignatureForSelector:), method_signature as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(forwardInvocation:), forward_invocation as extern "C-unwind" fn(_, _, _));

        builder.add_class_method(sel!(alloc), alloc as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(sel!(allocWithZone:), alloc_with_zone as unsafe extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(sel!(class), class_self as extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(sel!(superclass), class_superclass as extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(sel!(description), class_description as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(sel!(debugDescription), class_description as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(
            sel!(respondsToSelector:),
            class_responds_to_selector as extern "C-unwind" fn(_, _, _) -> _,
        );
        builder.add_class_method(sel!(isKindOfClass:), class_is_kind_of_class as extern "C-unwind" fn(_, _, _) -> _);
        builder
            .add_class_method(sel!(isMemberOfClass:), class_is_member_of_class as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(
            sel!(instancesRespondToSelector:),
            instances_respond_to_selector as extern "C-unwind" fn(_, _, _) -> _,
        );
        builder.add_class_method(sel!(isSubclassOfClass:), is_subclass_of_class as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(
            sel!(conformsToProtocol:),
            class_conforms_to_protocol as extern "C-unwind" fn(_, _, _) -> _,
        );
    }
    // SAFETY: the NSObject protocol is built in and never freed.
    let adopted = builder.add_protocol(unsafe { &*crate::protocol::nsobject_protocol().cast::<AnyProtocol>() });
    debug_assert!(adopted);
    builder.register();
}
