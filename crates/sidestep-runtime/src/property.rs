//! Declared properties: what `class_addProperty` and `protocol_addProperty`
//! record, for introspection. The runtime only describes properties; their
//! accessors are ordinary methods.

use std::ffi::{CStr, CString, c_char, c_uint};

use crate::class::{Class, class_ref};
use crate::util::{Shared, cstr_or, leak_cstr, malloc_array, malloc_cstr};
use crate::{Bool, NO, YES};

/// `objc_property_attribute_t`: one attribute, such as `T` (the type) with
/// the value `@"NSString"`, or `N` (nonatomic) with an empty value.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Attribute {
    name: *const c_char,
    value: *const c_char,
}

/// A property: its name, its attributes, and the attribute string they
/// make, such as `T@"NSString",C,N,V_name`. Never changed or freed once
/// made, so pointers to it stay valid, as C callers expect.
pub struct Property {
    name: &'static CStr,
    attributes: &'static CStr,
    list: Box<[Attribute]>,
}

// SAFETY: immutable after construction; the attribute pointers are leaked
// C strings.
unsafe impl Sync for Property {}
// SAFETY: as above.
unsafe impl Send for Property {}

impl Property {
    /// A property from C arguments, or `None` for a null name.
    ///
    /// # Safety
    /// `name` must be a C string or null, and `attributes` must point to
    /// `count` attributes (or be null if `count` is zero) whose names and
    /// values are C strings or null.
    pub(crate) unsafe fn new(
        name: *const c_char,
        attributes: *const Attribute,
        count: c_uint,
    ) -> Option<&'static Property> {
        if name.is_null() {
            return None;
        }
        // SAFETY: guaranteed by the caller.
        let name = leak_cstr(unsafe { CStr::from_ptr(name) });
        let given: &[Attribute] = match count {
            0 => &[],
            // SAFETY: guaranteed by the caller.
            n => unsafe { std::slice::from_raw_parts(attributes, n as usize) },
        };
        let mut joined = Vec::new();
        let list = given
            .iter()
            .map(|a| {
                // SAFETY: guaranteed by the caller.
                let (n, v) = unsafe { (cstr_or(a.name, c""), cstr_or(a.value, c"")) };
                if !joined.is_empty() {
                    joined.push(b',');
                }
                joined.extend_from_slice(n.to_bytes());
                joined.extend_from_slice(v.to_bytes());
                Attribute { name: leak_cstr(n).as_ptr(), value: leak_cstr(v).as_ptr() }
            })
            .collect();
        let attributes = leak_cstr(&CString::new(joined).expect("C strings contain no NUL"));
        Some(Box::leak(Box::new(Property { name, attributes, list })))
    }

    pub(crate) fn name(&self) -> &'static CStr {
        self.name
    }

    fn value(&self, attribute: &CStr) -> Option<&'static CStr> {
        self.list.iter().find_map(|a| {
            // SAFETY: leaked C strings.
            let (n, v) = unsafe { (CStr::from_ptr(a.name), CStr::from_ptr(a.value)) };
            (n == attribute).then_some(v)
        })
    }
}

/// The class's own property named `name`.
fn own(cls: &Class, name: &CStr) -> Option<&'static Property> {
    let properties = cls.rt().properties.read().unwrap();
    // SAFETY: properties are never freed.
    properties.iter().map(|p| unsafe { p.get() }).find(|p| p.name == name)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_addProperty(
    cls: *mut Class,
    name: *const c_char,
    attributes: *const Attribute,
    count: c_uint,
) -> Bool {
    // SAFETY: the caller passes a class or null.
    let Some(cls) = (unsafe { class_ref(cls) }) else { return NO };
    // SAFETY: the caller passes a C string or null.
    if name.is_null() || own(cls, unsafe { CStr::from_ptr(name) }).is_some() {
        return NO;
    }
    // SAFETY: forwarded contract.
    let Some(property) = (unsafe { Property::new(name, attributes, count) }) else { return NO };
    cls.rt().properties.write().unwrap().push(Shared(property));
    YES
}

/// Replaces the class's own property named `name`, or adds it. Pointers to
/// the old property stay valid and keep describing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_replaceProperty(
    cls: *mut Class,
    name: *const c_char,
    attributes: *const Attribute,
    count: c_uint,
) {
    // SAFETY: the caller passes a class or null.
    let Some(cls) = (unsafe { class_ref(cls) }) else { return };
    // SAFETY: forwarded contract.
    let Some(property) = (unsafe { Property::new(name, attributes, count) }) else { return };
    let mut properties = cls.rt().properties.write().unwrap();
    // SAFETY: properties are never freed.
    match properties.iter_mut().find(|p| unsafe { p.get() }.name == property.name) {
        Some(slot) => *slot = Shared(property),
        None => properties.push(Shared(property)),
    }
}

/// Searches the class and its superclasses.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_getProperty(cls: *const Class, name: *const c_char) -> *const Property {
    // SAFETY: the caller passes a class or null.
    let mut cls = unsafe { class_ref(cls) };
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the caller passes a C string.
    let name = unsafe { CStr::from_ptr(name) };
    while let Some(c) = cls {
        if let Some(property) = own(c, name) {
            return property;
        }
        cls = c.superclass();
    }
    std::ptr::null()
}

/// The class's own properties, not its superclasses'.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_copyPropertyList(cls: *const Class, out_len: *mut c_uint) -> *mut *const Property {
    // SAFETY: the caller passes a class or null.
    let list: Vec<*const Property> = match unsafe { class_ref(cls) } {
        Some(cls) => cls.rt().properties.read().unwrap().iter().map(|p| p.0).collect(),
        None => Vec::new(),
    };
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(&list, out_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_getName(property: *const Property) -> *const c_char {
    // SAFETY: the caller passes a property or null.
    unsafe { property.as_ref() }.map_or(std::ptr::null(), |p| p.name.as_ptr())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_getAttributes(property: *const Property) -> *const c_char {
    // SAFETY: the caller passes a property or null.
    unsafe { property.as_ref() }.map_or(std::ptr::null(), |p| p.attributes.as_ptr())
}

/// The array is the caller's to `free`; the strings it points to live for
/// the rest of the program.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_copyAttributeList(property: *const Property, out_len: *mut c_uint) -> *mut Attribute {
    // SAFETY: the caller passes a property or null.
    let list = unsafe { property.as_ref() }.map_or(&[][..], |p| &p.list);
    // SAFETY: the caller passes a valid or null pointer.
    unsafe { malloc_array(list, out_len) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_copyAttributeValue(property: *const Property, name: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a property or null, and a C string or null.
    let (Some(property), false) = (unsafe { property.as_ref() }, name.is_null()) else {
        return std::ptr::null_mut();
    };
    // SAFETY: as above.
    match property.value(unsafe { CStr::from_ptr(name) }) {
        Some(value) => malloc_cstr(value.to_bytes()),
        None => std::ptr::null_mut(),
    }
}
