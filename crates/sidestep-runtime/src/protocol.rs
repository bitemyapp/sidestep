//! Protocols: construction, conformance and method descriptions.

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_uint};
use std::sync::{LazyLock, Mutex, RwLock};

use crate::class::{Class, class_ref};
use crate::property::{Attribute, Property};
use crate::selector::Sel;
use crate::util::{Shared, leak_cstr, lock, malloc_array};
use crate::{Bool, NO, YES};

#[derive(Clone, Copy)]
struct MethodDesc {
    sel: Sel,
    types: *const c_char,
    required: bool,
    instance: bool,
}

#[derive(Clone, Copy)]
struct PropertyDesc {
    property: Shared<Property>,
    required: bool,
    instance: bool,
}

#[derive(Default)]
struct Inner {
    registered: bool,
    protocols: Vec<Shared<Protocol>>,
    methods: Vec<MethodDesc>,
    properties: Vec<PropertyDesc>,
}

// SAFETY: selectors and type strings are immutable and never freed.
unsafe impl Send for Inner {}

/// A protocol. Starts with an `isa` slot like libobjc2's, left null.
#[repr(C)]
pub struct Protocol {
    isa: *const Class,
    name: *const c_char,
    inner: Mutex<Inner>,
}

// SAFETY: `name` is immutable; everything else is behind the mutex.
unsafe impl Sync for Protocol {}

/// `struct objc_method_description`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ObjcMethodDescription {
    name: Sel,
    types: *const c_char,
}

/// Every protocol ever allocated, registered or not, by name.
static PROTOCOLS: LazyLock<RwLock<HashMap<&'static CStr, Shared<Protocol>>>> = LazyLock::new(|| {
    let nsobject = new_protocol(c"NSObject");
    {
        let mut inner = lock(&nsobject.inner);
        inner.methods = NSOBJECT_METHODS
            .iter()
            .map(|&(name, types, required)| MethodDesc {
                sel: crate::selector::register(name),
                types: types.as_ptr(),
                required,
                instance: true,
            })
            .collect();
        inner.registered = true;
    }
    RwLock::new(HashMap::from([(c"NSObject", Shared(nsobject as *const Protocol))]))
});

/// The `NSObject` protocol's instance methods, so classes can say which
/// ones they override: name, type encoding, required.
const NSOBJECT_METHODS: &[(&CStr, &CStr, bool)] = &[
    (c"isEqual:", c"C@:@", true),
    (c"hash", c"Q@:", true),
    (c"superclass", c"#@:", true),
    (c"class", c"#@:", true),
    (c"self", c"@@:", true),
    (c"performSelector:", c"@@::", true),
    (c"performSelector:withObject:", c"@@::@", true),
    (c"performSelector:withObject:withObject:", c"@@::@@", true),
    (c"isProxy", c"C@:", true),
    (c"isKindOfClass:", c"C@:#", true),
    (c"isMemberOfClass:", c"C@:#", true),
    (c"conformsToProtocol:", c"C@:@", true),
    (c"respondsToSelector:", c"C@::", true),
    (c"retain", c"@@:", true),
    (c"release", c"v@:", true),
    (c"autorelease", c"@@:", true),
    (c"retainCount", c"Q@:", true),
    (c"zone", c"^{_NSZone=}@:", true),
    (c"description", c"@@:", true),
    (c"debugDescription", c"@@:", false),
];

fn new_protocol(name: &CStr) -> &'static Protocol {
    Box::leak(Box::new(Protocol {
        isa: std::ptr::null(),
        name: leak_cstr(name).as_ptr(),
        inner: Mutex::new(Inner::default()),
    }))
}

impl Protocol {
    fn name(&self) -> &'static CStr {
        // SAFETY: leaked C string.
        unsafe { CStr::from_ptr(self.name) }
    }

    fn property(&self, name: &CStr, instance: bool) -> Option<&'static Property> {
        let (own, parents) = {
            let inner = lock(&self.inner);
            let own = inner
                .properties
                .iter()
                // SAFETY: properties are never freed.
                .map(|d| (unsafe { d.property.get() }, d))
                .find(|(p, d)| d.required && d.instance == instance && p.name() == name)
                .map(|(p, _)| p);
            (own, inner.protocols.clone())
        };
        // SAFETY: protocols are never freed.
        own.or_else(|| parents.iter().find_map(|p| unsafe { p.get() }.property(name, instance)))
    }

    fn conforms_to(&self, other: &Protocol) -> bool {
        if std::ptr::eq(self, other) || self.name() == other.name() {
            return true;
        }
        let parents = lock(&self.inner).protocols.clone();
        // SAFETY: protocols are never freed.
        parents.iter().any(|p| unsafe { p.get() }.conforms_to(other))
    }
}

/// The built-in `NSObject` protocol.
pub(crate) fn nsobject_protocol() -> *const Protocol {
    PROTOCOLS.read().unwrap()[c"NSObject"].0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_getProtocol(name: *const c_char) -> *const Protocol {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a C string.
    let name = unsafe { CStr::from_ptr(name) };
    match PROTOCOLS.read().unwrap().get(name) {
        // SAFETY: protocols are never freed.
        Some(p) if lock(&unsafe { p.get() }.inner).registered => p.0,
        _ => std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_copyProtocolList(out_len: *mut c_uint) -> *mut *const Protocol {
    let list: Vec<*const Protocol> = PROTOCOLS
        .read()
        .unwrap()
        .values()
        // SAFETY: protocols are never freed.
        .filter(|p| lock(&unsafe { p.get() }.inner).registered)
        .map(|p| p.0)
        .collect();
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&list, out_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_allocateProtocol(name: *const c_char) -> *mut Protocol {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller passes a C string.
    let name = unsafe { CStr::from_ptr(name) };
    let mut table = PROTOCOLS.write().unwrap();
    if table.contains_key(name) {
        return std::ptr::null_mut();
    }
    let proto = new_protocol(name);
    table.insert(proto.name(), Shared(proto));
    (proto as *const Protocol).cast_mut()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn objc_registerProtocol(proto: *mut Protocol) {
    // SAFETY: the caller passes a protocol or null.
    if let Some(proto) = unsafe { proto.as_ref() } {
        lock(&proto.inner).registered = true;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_getName(proto: *const Protocol) -> *const c_char {
    // SAFETY: the caller passes a protocol or null.
    unsafe { proto.as_ref() }.map_or(std::ptr::null(), |p| p.name)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_isEqual(proto: *const Protocol, other: *const Protocol) -> Bool {
    // SAFETY: the caller passes protocols or null.
    match unsafe { (proto.as_ref(), other.as_ref()) } {
        (Some(a), Some(b)) if std::ptr::eq(a, b) || a.name() == b.name() => YES,
        _ => NO,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_conformsToProtocol(proto: *const Protocol, other: *const Protocol) -> Bool {
    // SAFETY: the caller passes protocols or null.
    match unsafe { (proto.as_ref(), other.as_ref()) } {
        (Some(a), Some(b)) if a.conforms_to(b) => YES,
        _ => NO,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_addMethodDescription(
    proto: *mut Protocol,
    sel: Sel,
    types: *const c_char,
    is_required: Bool,
    is_instance: Bool,
) {
    // SAFETY: the caller passes a protocol or null.
    let Some(proto) = (unsafe { proto.as_ref() }) else { return };
    let mut inner = lock(&proto.inner);
    if inner.registered || sel.is_null() {
        return;
    }
    // SAFETY: the caller passes a C string or null.
    let types = leak_cstr(unsafe { crate::util::cstr_or(types, c"") }).as_ptr();
    inner.methods.push(MethodDesc { sel, types, required: is_required != NO, instance: is_instance != NO });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_addProtocol(proto: *mut Protocol, addition: *const Protocol) {
    // SAFETY: the caller passes protocols or null.
    let (Some(proto), false) = (unsafe { proto.as_ref() }, addition.is_null()) else { return };
    let mut inner = lock(&proto.inner);
    if !inner.registered {
        inner.protocols.push(Shared(addition));
    }
}

/// Recorded only while the protocol is under construction, as on Apple's
/// runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_addProperty(
    proto: *mut Protocol,
    name: *const c_char,
    attributes: *const Attribute,
    count: c_uint,
    is_required: Bool,
    is_instance: Bool,
) {
    // SAFETY: the caller passes a protocol or null.
    let Some(proto) = (unsafe { proto.as_ref() }) else { return };
    let mut inner = lock(&proto.inner);
    if inner.registered {
        return;
    }
    // SAFETY: forwarded contract.
    if let Some(property) = unsafe { Property::new(name, attributes, count) } {
        inner.properties.push(PropertyDesc {
            property: Shared(property),
            required: is_required != NO,
            instance: is_instance != NO,
        });
    }
}

/// The protocol's own required instance properties.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_copyPropertyList(
    proto: *const Protocol,
    out_len: *mut c_uint,
) -> *mut *const Property {
    // SAFETY: the caller passes a protocol or null.
    let list: Vec<*const Property> = match unsafe { proto.as_ref() } {
        Some(p) => {
            lock(&p.inner).properties.iter().filter(|d| d.required && d.instance).map(|d| d.property.0).collect()
        }
        None => Vec::new(),
    };
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&list, out_len) }
}

/// Searches the protocol and the protocols it adopts. Apple's runtime
/// reports no optional properties here, and neither does this one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_getProperty(
    proto: *const Protocol,
    name: *const c_char,
    is_required: Bool,
    is_instance: Bool,
) -> *const Property {
    // SAFETY: the caller passes a protocol or null, and a C string or null.
    let (Some(proto), false) = (unsafe { proto.as_ref() }, name.is_null()) else { return std::ptr::null() };
    if is_required == NO {
        return std::ptr::null();
    }
    // SAFETY: as above.
    let name = unsafe { CStr::from_ptr(name) };
    proto.property(name, is_instance != NO).map_or(std::ptr::null(), |p| p as *const Property)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_copyProtocolList(
    proto: *const Protocol,
    out_len: *mut c_uint,
) -> *mut *const Protocol {
    // SAFETY: the caller passes a protocol or null.
    let list: Vec<*const Protocol> = match unsafe { proto.as_ref() } {
        Some(p) => lock(&p.inner).protocols.iter().map(|p| p.0).collect(),
        None => Vec::new(),
    };
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&list, out_len) }
}

fn descriptions(proto: *const Protocol, required: Bool, instance: Bool) -> Vec<ObjcMethodDescription> {
    // SAFETY: callers pass a protocol or null.
    let Some(proto) = (unsafe { proto.as_ref() }) else { return Vec::new() };
    lock(&proto.inner)
        .methods
        .iter()
        .filter(|m| m.required == (required != NO) && m.instance == (instance != NO))
        .map(|m| ObjcMethodDescription { name: m.sel, types: m.types })
        .collect()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_copyMethodDescriptionList(
    proto: *const Protocol,
    is_required: Bool,
    is_instance: Bool,
    out_len: *mut c_uint,
) -> *mut ObjcMethodDescription {
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&descriptions(proto, is_required, is_instance), out_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn protocol_getMethodDescription(
    proto: *const Protocol,
    sel: Sel,
    is_required: Bool,
    is_instance: Bool,
) -> ObjcMethodDescription {
    descriptions(proto, is_required, is_instance)
        .into_iter()
        .find(|d| d.name == sel)
        .unwrap_or(ObjcMethodDescription { name: std::ptr::null(), types: std::ptr::null() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_addProtocol(cls: *mut Class, proto: *const Protocol) -> Bool {
    // SAFETY: the caller passes a class or null.
    let (Some(cls), false) = (unsafe { class_ref(cls) }, proto.is_null()) else { return NO };
    // SAFETY: as above.
    if unsafe { class_conformsToProtocol(cls, proto) } != NO {
        return NO;
    }
    cls.rt().protocols.write().unwrap().push(Shared(proto));
    YES
}

/// Whether the class itself (not its superclasses) adopts `proto`, directly
/// or through a protocol it adopts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_conformsToProtocol(cls: *const Class, proto: *const Protocol) -> Bool {
    // SAFETY: the caller passes a class and a protocol, or null.
    let (Some(cls), Some(proto)) = (unsafe { class_ref(cls) }, unsafe { proto.as_ref() }) else {
        return NO;
    };
    let adopted = cls.rt().protocols.read().unwrap().clone();
    // SAFETY: protocols are never freed.
    if adopted.iter().any(|p| unsafe { p.get() }.conforms_to(proto)) { YES } else { NO }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_copyProtocolList(cls: *const Class, out_len: *mut c_uint) -> *mut *const Protocol {
    // SAFETY: the caller passes a class or null.
    let list: Vec<*const Protocol> = match unsafe { class_ref(cls) } {
        Some(cls) => cls.rt().protocols.read().unwrap().iter().map(|p| p.0).collect(),
        None => Vec::new(),
    };
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&list, out_len) }
}
