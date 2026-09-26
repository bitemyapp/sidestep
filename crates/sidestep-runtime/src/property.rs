//! Declared properties: what `class_addProperty` and `protocol_addProperty`
//! record, for introspection. The runtime only describes properties; their
//! accessors are ordinary methods.

use std::ffi::{CStr, CString, c_char, c_uint};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::class::{Class, class_ref};
use crate::util::{Shared, c_malloc, cstr_or, leak_cstr, malloc_array, malloc_cstr};
use crate::{Bool, NO, YES};

/// `objc_property_attribute_t`: one attribute, such as `T` (the type) with
/// the value `@"NSString"`, or `N` (nonatomic) with an empty value.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Attribute {
    name: *const c_char,
    value: *const c_char,
}

/// A property: its name and its attribute string, such as
/// `T@"NSString",C,N,V_name`.
///
/// The string is the whole description, as on Apple's runtime: an
/// attribute given with a null value is left out, a name longer than one
/// character is written in double quotes, and the attribute list and
/// values are read back out of the string. `class_replaceProperty` changes
/// the string in place, so pointers to the property see the new
/// attributes; the old string is kept, since callers may still hold it.
pub struct Property {
    name: &'static CStr,
    attributes: AtomicPtr<c_char>,
}

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
        // SAFETY: guaranteed by the caller.
        let attributes = unsafe { attribute_string(attributes, count) };
        Some(Box::leak(Box::new(Property { name, attributes: AtomicPtr::new(attributes) })))
    }

    pub(crate) fn name(&self) -> &'static CStr {
        self.name
    }

    fn attributes(&self) -> &'static CStr {
        // SAFETY: attribute strings are leaked C strings.
        unsafe { CStr::from_ptr(self.attributes.load(Ordering::Acquire)) }
    }

    /// Give the property new attributes.
    ///
    /// # Safety
    /// As for [`Property::new`].
    unsafe fn replace(&self, attributes: *const Attribute, count: c_uint) {
        // SAFETY: forwarded contract.
        let string = unsafe { attribute_string(attributes, count) };
        self.attributes.store(string, Ordering::Release);
    }
}

/// The attribute string for `count` attributes, leaked.
///
/// # Safety
/// As for [`Property::new`].
unsafe fn attribute_string(attributes: *const Attribute, count: c_uint) -> *mut c_char {
    let given: &[Attribute] = match count {
        0 => &[],
        // SAFETY: guaranteed by the caller.
        n => unsafe { std::slice::from_raw_parts(attributes, n as usize) },
    };
    let mut joined = Vec::new();
    for attribute in given {
        if attribute.value.is_null() {
            continue;
        }
        // SAFETY: guaranteed by the caller.
        let (name, value) = unsafe { (cstr_or(attribute.name, c""), CStr::from_ptr(attribute.value)) };
        if !joined.is_empty() {
            joined.push(b',');
        }
        let name = name.to_bytes();
        if name.len() > 1 {
            joined.push(b'"');
            joined.extend_from_slice(name);
            joined.push(b'"');
        } else {
            joined.extend_from_slice(name);
        }
        joined.extend_from_slice(value.to_bytes());
    }
    CString::new(joined).expect("C strings contain no NUL").into_raw()
}

/// The attributes an attribute string describes, as (name, value) pairs:
/// the name is the first character of each comma-separated item, or the
/// text between the double quotes it starts with.
fn parse(attributes: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    attributes.split(|&b| b == b',').filter(|item| !item.is_empty()).map(|item| {
        if let Some(rest) = item.strip_prefix(b"\"") {
            let end = rest.iter().position(|&b| b == b'"').unwrap_or(rest.len());
            (&rest[..end], rest.get(end + 1..).unwrap_or_default())
        } else {
            item.split_at(1)
        }
    })
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

/// Replaces the attributes of the class's own property named `name`, in
/// place, or adds the property.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_replaceProperty(
    cls: *mut Class,
    name: *const c_char,
    attributes: *const Attribute,
    count: c_uint,
) {
    // SAFETY: the caller passes a class or null.
    let Some(cls) = (unsafe { class_ref(cls) }) else { return };
    if name.is_null() {
        return;
    }
    let mut properties = cls.rt().properties.write().unwrap();
    // SAFETY: the caller passes a C string; properties are never freed.
    let existing = properties.iter().map(|p| unsafe { p.get() }).find(|p| p.name == unsafe { CStr::from_ptr(name) });
    match existing {
        // SAFETY: forwarded contract.
        Some(property) => unsafe { property.replace(attributes, count) },
        // SAFETY: forwarded contract.
        None => properties.extend(unsafe { Property::new(name, attributes, count) }.map(|p| Shared(p))),
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

/// The class's own properties, not its superclasses', newest first as on
/// Apple's runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn class_copyPropertyList(cls: *const Class, out_len: *mut c_uint) -> *mut *const Property {
    // SAFETY: the caller passes a class or null.
    let list: Vec<*const Property> = match unsafe { class_ref(cls) } {
        Some(cls) => cls.rt().properties.read().unwrap().iter().rev().map(|p| p.0).collect(),
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
    unsafe { property.as_ref() }.map_or(std::ptr::null(), |p| p.attributes().as_ptr())
}

/// One `malloc` block for the caller to `free`: the attributes, followed by
/// the strings they point to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_copyAttributeList(property: *const Property, out_len: *mut c_uint) -> *mut Attribute {
    // SAFETY: the caller passes a property or null.
    let attributes = unsafe { property.as_ref() }.map_or(&[][..], |p| p.attributes().to_bytes());
    let pairs: Vec<_> = parse(attributes).collect();
    if !out_len.is_null() {
        // SAFETY: the caller passes a valid or null pointer.
        unsafe { *out_len = pairs.len() as c_uint };
    }
    if pairs.is_empty() {
        return std::ptr::null_mut();
    }
    let head = pairs.len() * size_of::<Attribute>();
    let text: usize = pairs.iter().map(|(n, v)| n.len() + v.len() + 2).sum();
    let block = c_malloc(head + text).cast::<u8>();
    // SAFETY: the block has room for the attributes and then every name
    // and value with its terminator; `malloc` aligns it for pointers.
    unsafe {
        let list = block.cast::<Attribute>();
        let mut at = block.add(head);
        let mut put = |s: &[u8]| {
            let start = at;
            start.copy_from_nonoverlapping(s.as_ptr(), s.len());
            *start.add(s.len()) = 0;
            at = start.add(s.len() + 1);
            start.cast::<c_char>().cast_const()
        };
        for (i, (name, value)) in pairs.iter().enumerate() {
            let attribute = Attribute { name: put(name), value: put(value) };
            list.add(i).write(attribute);
        }
        list
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn property_copyAttributeValue(property: *const Property, name: *const c_char) -> *mut c_char {
    // SAFETY: the caller passes a property or null, and a C string or null.
    let (Some(property), false) = (unsafe { property.as_ref() }, name.is_null()) else {
        return std::ptr::null_mut();
    };
    // SAFETY: as above.
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    match parse(property.attributes().to_bytes()).find(|&(n, _)| n == name) {
        Some((_, value)) => malloc_cstr(value),
        None => std::ptr::null_mut(),
    }
}
