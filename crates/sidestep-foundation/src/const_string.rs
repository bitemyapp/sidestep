//! Constant strings in static memory, for exported Objective-C constants such
//! as `NSFontAttributeName`. Their class answers the messages `NSString`
//! does, reading the text from the static object rather than from ivars.

use std::ffi::c_char;

use objc2::runtime::{AnyObject, Bool, ClassBuilder, Sel};
use objc2::{ClassType, sel};
use objc2_foundation::{NSString, NSZone};
use sidestep_runtime::StaticObject;

/// The body of a constant string: UTF-8 followed by a NUL, and its length
/// in UTF-16 code units.
pub struct ConstStr {
    utf8_nul: &'static str,
    utf16_len: usize,
    pub(crate) hash: usize,
}

impl ConstStr {
    /// `utf8_nul` must end with `"\0"`; `constant_string!` arranges that.
    pub const fn new(utf8_nul: &'static str) -> Self {
        let text = utf8_nul.as_bytes().split_at(utf8_nul.len() - 1).0;
        ConstStr { utf8_nul, utf16_len: utf16_len(utf8_nul) - 1, hash: crate::string::hash_bytes(text) }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        &self.utf8_nul[..self.utf8_nul.len() - 1]
    }
}

const fn utf16_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < bytes.len() {
        let b = bytes[i];
        if b & 0xC0 != 0x80 {
            n += if b >= 0xF0 { 2 } else { 1 };
        }
        i += 1;
    }
    n
}

/// A constant string object.
pub type ConstantString = StaticObject<ConstStr>;

/// Export an `NSString` constant under an Objective-C symbol name:
///
/// ```ignore
/// sidestep_foundation::constant_string!(NSFontAttributeName = "NSFont");
/// ```
#[macro_export]
macro_rules! constant_string {
    ($(#[$m:meta])* $name:ident = $value:literal) => {
        $(#[$m])*
        #[unsafe(no_mangle)]
        pub static $name: $crate::__private::ObjectRef = {
            static OBJ: $crate::ConstantString = $crate::ConstantString::new(
                &$crate::CONSTANT_STRING_CLASS,
                $crate::ConstStr::new(concat!($value, "\0")),
            );
            OBJ.object_ref()
        };
    };
}

pub(crate) fn body(this: &AnyObject) -> &'static ConstStr {
    // SAFETY: instances of this class exist only as `ConstantString`s.
    unsafe { ConstantString::body_of((this as *const AnyObject).cast()) }
}

extern "C-unwind" fn length(this: &AnyObject, _: Sel) -> usize {
    body(this).utf16_len
}

extern "C-unwind" fn length_of_bytes(this: &AnyObject, _: Sel, encoding: i32) -> usize {
    crate::string::byte_length(body(this).as_str(), encoding as u32)
}

extern "C-unwind" fn utf8_string(this: &AnyObject, _: Sel) -> *const c_char {
    body(this).utf8_nul.as_ptr().cast()
}

extern "C-unwind" fn character_at_index(this: &AnyObject, _: Sel, index: usize) -> u16 {
    let s = body(this).as_str();
    match s.encode_utf16().nth(index) {
        Some(unit) => unit,
        None => {
            panic!("-[NSString characterAtIndex:]: index {index} out of bounds; string length {}", body(this).utf16_len)
        }
    }
}

extern "C-unwind" fn hash(this: &AnyObject, _: Sel) -> usize {
    body(this).hash
}

extern "C-unwind" fn is_equal(this: &AnyObject, _: Sel, other: Option<&AnyObject>) -> Bool {
    let other = other.and_then(|o| o.downcast_ref::<NSString>());
    Bool::new(other.is_some_and(|o| crate::string::equals(body(this).as_str(), o)))
}

extern "C-unwind" fn is_equal_to_string(this: &AnyObject, _: Sel, other: &NSString) -> Bool {
    Bool::new(crate::string::equals(body(this).as_str(), other))
}

extern "C-unwind" fn copy_with_zone(this: *mut AnyObject, _: Sel, _zone: *mut NSZone) -> *mut AnyObject {
    // Immortal and immutable: the copy is the same object.
    this
}

extern "C-unwind" fn description(this: *mut AnyObject, _: Sel) -> *mut AnyObject {
    this
}

pub(crate) fn load() {
    let mut b = ClassBuilder::new(c"_SidestepConstantString", NSString::class())
        .expect("sidestep: the constant string class is defined once");
    // SAFETY: each function matches the selector's convention.
    unsafe {
        b.add_method(sel!(length), length as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(lengthOfBytesUsingEncoding:), length_of_bytes as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(UTF8String), utf8_string as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(characterAtIndex:), character_at_index as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(hash), hash as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(isEqual:), is_equal as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(isEqualToString:), is_equal_to_string as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(copyWithZone:), copy_with_zone as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(description), description as extern "C-unwind" fn(_, _) -> _);
    }
    b.register();
}
