//! The root class, `NSObject`, defined with objc2's own `ClassBuilder` so its
//! method encodings are exactly the ones objc2 checks messages against.
//!
//! Methods that need Foundation (`-description`) are added by Foundation.

use std::ffi::c_void;

use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Imp, Sel};
use objc2::sel;

use crate::class::{Class, lookup_imp};
use crate::message::method_for;
use crate::object::{Object, class_createInstance, isa, object_dispose};
use crate::selector;

crate::static_class!(pub NSOBJECT_CLASS, NSOBJECT_METACLASS = "NSObject", load);

type Id = *mut AnyObject;

fn obj(this: Id) -> *mut Object {
    this.cast()
}

fn class_of(this: Id) -> &'static Class {
    // SAFETY: methods are only called on live receivers.
    unsafe { isa(obj(this)) }
}

fn as_class(cls: *const AnyClass) -> &'static Class {
    // SAFETY: class methods are only called on class objects.
    unsafe { &*cls.cast::<Class>() }
}

/// objc2's `Sel` is a transparent wrapper around the runtime's selector
/// pointer.
fn raw(sel: Sel) -> crate::selector::Sel {
    // SAFETY: same representation.
    unsafe { std::mem::transmute::<Sel, crate::selector::Sel>(sel) }
}

fn bool(b: bool) -> Bool {
    Bool::new(b)
}

/// Send a message with no arguments and an object result.
unsafe fn send0(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the caller passes a live receiver.
    unsafe {
        let cls = isa(obj(receiver));
        let imp = method_for(cls, raw(sel));
        let imp: unsafe extern "C-unwind" fn(Id, Sel) -> Id = std::mem::transmute(imp);
        imp(receiver, sel)
    }
}

// Instance methods.

extern "C-unwind" fn init(this: Id, _: Sel) -> Id {
    this
}

unsafe extern "C-unwind" fn dealloc(this: Id, _: Sel) {
    // SAFETY: -dealloc is sent once, when the last reference goes away.
    unsafe { object_dispose(obj(this)) };
}

unsafe extern "C-unwind" fn retain(this: Id, _: Sel) -> Id {
    // SAFETY: a live, counted receiver. Classes reach this method through
    // the root metaclass and are left alone.
    if !class_of(this).is_meta() {
        unsafe { crate::arc::raw_retain(obj(this)) };
    }
    this
}

unsafe extern "C-unwind" fn release(this: Id, _: Sel) {
    if !class_of(this).is_meta() {
        // SAFETY: the caller owns a reference.
        unsafe { crate::arc::raw_release(obj(this)) };
    }
}

extern "C-unwind" fn autorelease(this: Id, _: Sel) -> Id {
    if !class_of(this).is_meta() {
        crate::arc::pool_add(obj(this));
    }
    this
}

unsafe extern "C-unwind" fn retain_count(this: Id, _: Sel) -> usize {
    if class_of(this).is_meta() {
        return usize::MAX;
    }
    // SAFETY: a live, counted receiver.
    unsafe { crate::arc::retain_count(obj(this)) }
}

extern "C-unwind" fn class(this: Id, _: Sel) -> *const AnyClass {
    (class_of(this) as *const Class).cast()
}

extern "C-unwind" fn superclass(this: Id, _: Sel) -> *const AnyClass {
    class_of(this).superclass().map_or(std::ptr::null(), |c| (c as *const Class).cast())
}

extern "C-unwind" fn self_(this: Id, _: Sel) -> Id {
    this
}

extern "C-unwind" fn is_kind_of_class(this: Id, _: Sel, cls: *const AnyClass) -> Bool {
    bool(!cls.is_null() && class_of(this).is_subclass_of(as_class(cls)))
}

extern "C-unwind" fn is_member_of_class(this: Id, _: Sel, cls: *const AnyClass) -> Bool {
    bool(std::ptr::eq(class_of(this), cls.cast::<Class>()))
}

extern "C-unwind" fn responds_to_selector(this: Id, _: Sel, sel: Sel) -> Bool {
    bool(lookup_imp(class_of(this), raw(sel)).is_some())
}

fn conforms(mut cls: Option<&'static Class>, proto: *const AnyProtocol) -> bool {
    while let Some(c) = cls {
        // SAFETY: a loaded class and a protocol pointer.
        if unsafe { crate::protocol::class_conformsToProtocol(c, proto.cast()) } != 0 {
            return true;
        }
        cls = c.superclass();
    }
    false
}

extern "C-unwind" fn conforms_to_protocol(this: Id, _: Sel, proto: *const AnyProtocol) -> Bool {
    bool(conforms(Some(class_of(this).instance_class()), proto))
}

extern "C-unwind" fn hash(this: Id, _: Sel) -> usize {
    this as usize
}

extern "C-unwind" fn is_equal(this: Id, _: Sel, other: Id) -> Bool {
    bool(this == other)
}

extern "C-unwind" fn is_proxy(_: Id, _: Sel) -> Bool {
    bool(false)
}

extern "C-unwind" fn zone(_: Id, _: Sel) -> *mut c_void {
    std::ptr::null_mut()
}

/// `-description`: `<ClassName: 0x…>`, autoreleased. Strings belong to
/// Foundation, so the string comes from whichever class is registered as
/// `NSString`; before one is, the description is nil.
unsafe extern "C-unwind" fn description(this: Id, _: Sel) -> Id {
    let Some(string_class) = crate::class::lookup_name(c"NSString") else { return std::ptr::null_mut() };
    let text = format!("<{}: {:p}>", class_of(this).name().to_string_lossy(), this);
    // SAFETY: +alloc and -initWithBytes:length:encoding: (UTF-8 is 4, an
    // int in this ABI, see docs/abi.md) are NSString's.
    unsafe {
        let string = send0((string_class as *const Class).cast_mut().cast(), sel!(alloc));
        let sel = sel!(initWithBytes:length:encoding:);
        let imp = method_for(class_of(string), raw(sel));
        let imp: unsafe extern "C-unwind" fn(Id, Sel, *const c_void, usize, i32) -> Id = std::mem::transmute(imp);
        crate::arc::objc_autorelease(imp(string, sel, text.as_ptr().cast(), text.len(), 4).cast()).cast()
    }
}

unsafe extern "C-unwind" fn copy(this: Id, _: Sel) -> Id {
    // SAFETY: -copyWithZone: takes a zone and returns a +1 object.
    unsafe {
        let sel = sel!(copyWithZone:);
        let imp = method_for(class_of(this), raw(sel));
        let imp: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Id = std::mem::transmute(imp);
        imp(this, sel, std::ptr::null_mut())
    }
}

unsafe extern "C-unwind" fn mutable_copy(this: Id, _: Sel) -> Id {
    // SAFETY: as for copy.
    unsafe {
        let sel = sel!(mutableCopyWithZone:);
        let imp = method_for(class_of(this), raw(sel));
        let imp: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Id = std::mem::transmute(imp);
        imp(this, sel, std::ptr::null_mut())
    }
}

extern "C-unwind" fn does_not_recognize_selector(this: Id, _: Sel, sel: Sel) {
    let cls = class_of(this);
    let (prefix, what) = if cls.is_meta() { ('+', "class") } else { ('-', "instance") };
    panic!(
        "{prefix}[{} {sel}]: unrecognized selector sent to {what} {this:p}",
        cls.instance_class().name().to_string_lossy(),
    );
}

unsafe extern "C-unwind" fn perform_selector(this: Id, _: Sel, sel: Sel) -> Id {
    // SAFETY: -performSelector: requires an object-returning, no-argument
    // method.
    unsafe { send0(this, sel) }
}

unsafe extern "C-unwind" fn perform_selector_with(this: Id, _: Sel, sel: Sel, a: Id) -> Id {
    // SAFETY: the method takes one object and returns an object.
    unsafe {
        let imp = method_for(class_of(this), raw(sel));
        let imp: unsafe extern "C-unwind" fn(Id, Sel, Id) -> Id = std::mem::transmute(imp);
        imp(this, sel, a)
    }
}

unsafe extern "C-unwind" fn perform_selector_with_with(this: Id, _: Sel, sel: Sel, a: Id, b: Id) -> Id {
    // SAFETY: the method takes two objects and returns an object.
    unsafe {
        let imp = method_for(class_of(this), raw(sel));
        let imp: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Id = std::mem::transmute(imp);
        imp(this, sel, a, b)
    }
}

unsafe extern "C-unwind" fn method_for_selector(this: Id, _: Sel, sel: Sel) -> Option<Imp> {
    // SAFETY: the receiver is live.
    Some(unsafe { method_for(class_of(this), raw(sel)) })
}

extern "C-unwind" fn forwarding_target(_: Id, _: Sel, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

// Class methods.

extern "C-unwind" fn initialize(_: &AnyClass, _: Sel) {}

unsafe extern "C-unwind" fn alloc(cls: *const AnyClass, _: Sel) -> Id {
    // SAFETY: the receiver is a class.
    unsafe { class_createInstance(cls.cast(), 0).cast() }
}

unsafe extern "C-unwind" fn alloc_with_zone(cls: *const AnyClass, _: Sel, _zone: *mut c_void) -> Id {
    // SAFETY: as above.
    unsafe { class_createInstance(cls.cast(), 0).cast() }
}

unsafe extern "C-unwind" fn new(cls: *const AnyClass, _: Sel) -> Id {
    // SAFETY: +alloc and -init follow the usual conventions.
    unsafe {
        let obj = send0(cls.cast_mut().cast(), sel!(alloc));
        send0(obj, sel!(init))
    }
}

extern "C-unwind" fn class_self(cls: *const AnyClass, _: Sel) -> *const AnyClass {
    cls
}

extern "C-unwind" fn class_superclass(cls: *const AnyClass, _: Sel) -> *const AnyClass {
    as_class(cls).superclass().map_or(std::ptr::null(), |c| (c as *const Class).cast())
}

extern "C-unwind" fn instances_respond_to_selector(cls: *const AnyClass, _: Sel, sel: Sel) -> Bool {
    bool(lookup_imp(as_class(cls), raw(sel)).is_some())
}

extern "C-unwind" fn is_subclass_of_class(cls: *const AnyClass, _: Sel, other: *const AnyClass) -> Bool {
    bool(!other.is_null() && as_class(cls).is_subclass_of(as_class(other)))
}

extern "C-unwind" fn resolve_method(_: *const AnyClass, _: Sel, _sel: Sel) -> Bool {
    bool(false)
}

unsafe extern "C-unwind" fn instance_method_for_selector(cls: *const AnyClass, _: Sel, sel: Sel) -> Option<Imp> {
    // SAFETY: the receiver is a loaded class.
    Some(unsafe { method_for(as_class(cls), raw(sel)) })
}

extern "C-unwind" fn class_conforms_to_protocol(cls: *const AnyClass, _: Sel, proto: *const AnyProtocol) -> Bool {
    bool(conforms(Some(as_class(cls)), proto))
}

fn load() {
    let mut builder = ClassBuilder::root(c"NSObject", initialize as extern "C-unwind" fn(_, _))
        .expect("sidestep: NSObject is defined once");
    let this_sel = Sel::register(c"self");
    // SAFETY: each function's signature matches the selector's convention.
    unsafe {
        builder.add_method(sel!(init), init as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(dealloc), dealloc as unsafe extern "C-unwind" fn(_, _));
        builder.add_method(sel!(retain), retain as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(release), release as unsafe extern "C-unwind" fn(_, _));
        builder.add_method(sel!(autorelease), autorelease as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(retainCount), retain_count as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(class), class as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(superclass), superclass as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(this_sel, self_ as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(isKindOfClass:), is_kind_of_class as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(isMemberOfClass:), is_member_of_class as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(respondsToSelector:), responds_to_selector as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(conformsToProtocol:), conforms_to_protocol as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(hash), hash as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(isEqual:), is_equal as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(isProxy), is_proxy as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(zone), zone as extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(description), description as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(copy), copy as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_method(sel!(mutableCopy), mutable_copy as unsafe extern "C-unwind" fn(_, _) -> _);
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
        builder.add_method(sel!(methodForSelector:), method_for_selector as unsafe extern "C-unwind" fn(_, _, _) -> _);
        builder.add_method(sel!(forwardingTargetForSelector:), forwarding_target as extern "C-unwind" fn(_, _, _) -> _);

        builder.add_class_method(sel!(alloc), alloc as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(sel!(allocWithZone:), alloc_with_zone as unsafe extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(sel!(new), new as unsafe extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(sel!(class), class_self as extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(sel!(superclass), class_superclass as extern "C-unwind" fn(_, _) -> _);
        builder.add_class_method(
            sel!(instancesRespondToSelector:),
            instances_respond_to_selector as extern "C-unwind" fn(_, _, _) -> _,
        );
        builder.add_class_method(sel!(isSubclassOfClass:), is_subclass_of_class as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(sel!(resolveInstanceMethod:), resolve_method as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(sel!(resolveClassMethod:), resolve_method as extern "C-unwind" fn(_, _, _) -> _);
        builder.add_class_method(
            sel!(instanceMethodForSelector:),
            instance_method_for_selector as unsafe extern "C-unwind" fn(_, _, _) -> _,
        );
        builder.add_class_method(
            sel!(conformsToProtocol:),
            class_conforms_to_protocol as extern "C-unwind" fn(_, _, _) -> _,
        );
    }
    let cls = builder.register();
    // SAFETY: the NSObject protocol is built in.
    unsafe {
        crate::protocol::class_addProtocol(
            (cls as *const AnyClass).cast_mut().cast(),
            crate::protocol::nsobject_protocol(),
        )
    };
    let _ = selector::known();
}
