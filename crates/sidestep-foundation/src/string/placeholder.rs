//! `+[NSString alloc]` and the initializers.
//!
//! An immutable string's size depends on its text, which `+alloc` doesn't
//! know yet. So `+alloc` sent to NSString itself returns one static,
//! immortal placeholder, and the placeholder's `-init…` methods build the
//! real `_SidestepString` in a single allocation. Releasing the consumed
//! placeholder does nothing. Subclasses (NSMutableString, an app's own)
//! get NSObject's `+alloc`.
//!
//! The class methods (`+string`, `+stringWithString:`, …) send `+alloc`
//! and an initializer to the receiving class, so sent to NSMutableString
//! they build mutable strings.

use std::ffi::{c_char, c_void};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{ClassType, msg_send, sel};
use objc2_foundation::{NSData, NSString, NSStringEncoding};
use sidestep_runtime::StaticObject;

use super::create;
use super::encoding::{self, Decoded};
use super::view;

sidestep_runtime::static_class!(
    pub(crate) PLACEHOLDER_CLASS,
    PLACEHOLDER_META = "_SidestepStringPlaceholder",
    load
);

/// What `+[NSString alloc]` returns.
static PLACEHOLDER: StaticObject<()> = StaticObject::new(&PLACEHOLDER_CLASS, ());

fn build(decoded: Option<Decoded>) -> *mut AnyObject {
    match decoded {
        Some(d) => Retained::into_raw(super::inline::new(&d.bytes, d.utf16_len, d.flags)).cast(),
        None => std::ptr::null_mut(),
    }
}

// The initializers. The receiver is the placeholder, which is immortal, so
// consuming it needs no release.

extern "C-unwind" fn init(_: *mut AnyObject, _: Sel) -> *mut AnyObject {
    Retained::into_raw(super::empty()).cast()
}

extern "C-unwind" fn init_with_bytes(
    _: *mut AnyObject,
    _: Sel,
    bytes: *const c_void,
    length: usize,
    encoding: NSStringEncoding,
) -> *mut AnyObject {
    // SAFETY: the caller passes `length` readable bytes.
    build(unsafe { create::bytes(bytes, length, encoding::arg(encoding)) })
}

extern "C-unwind" fn init_with_data(
    _: *mut AnyObject,
    _: Sel,
    data: Option<&NSData>,
    encoding: NSStringEncoding,
) -> *mut AnyObject {
    // SAFETY: the bytes stay alive and unchanged during the call.
    let bytes = data.map_or(&[][..], |d| unsafe { d.as_bytes_unchecked() });
    // SAFETY: `bytes` is readable for its length.
    build(unsafe { create::bytes(bytes.as_ptr().cast(), bytes.len(), encoding::arg(encoding)) })
}

extern "C-unwind" fn init_with_bytes_no_copy(
    _: *mut AnyObject,
    _: Sel,
    bytes: NonNull<c_void>,
    length: usize,
    encoding: NSStringEncoding,
    free_when_done: Bool,
) -> *mut AnyObject {
    // SAFETY: as above.
    let result = build(unsafe { create::bytes(bytes.as_ptr(), length, encoding::arg(encoding)) });
    // A failed initializer leaves the buffer to the caller, who may try
    // another encoding, as on macOS.
    if free_when_done.as_bool() && !result.is_null() {
        // SAFETY: the caller handed over a malloc'ed buffer, and the new
        // string holds a copy of it.
        unsafe { libc::free(bytes.as_ptr()) };
    }
    result
}

extern "C-unwind" fn init_with_string(_: *mut AnyObject, _: Sel, string: &NSString) -> *mut AnyObject {
    let v = view::view(string);
    if v.is_immutable() {
        // An immutable string can stand for its copy.
        // SAFETY: a live object.
        return unsafe { objc2::ffi::objc_retain((string as *const NSString).cast_mut().cast()) };
    }
    let t = v.text();
    Retained::into_raw(super::inline::new(t.bytes, t.utf16_len, t.flags)).cast()
}

extern "C-unwind" fn init_with_utf8_string(_: *mut AnyObject, _: Sel, s: NonNull<c_char>) -> *mut AnyObject {
    // SAFETY: the caller passes a NUL-terminated string.
    build(unsafe { create::c_string(s.as_ptr(), encoding::UTF8) })
}

extern "C-unwind" fn init_with_c_string(
    _: *mut AnyObject,
    _: Sel,
    s: NonNull<c_char>,
    encoding: NSStringEncoding,
) -> *mut AnyObject {
    // SAFETY: as above.
    build(unsafe { create::c_string(s.as_ptr(), encoding::arg(encoding)) })
}

extern "C-unwind" fn init_with_characters(
    _: *mut AnyObject,
    _: Sel,
    chars: NonNull<u16>,
    length: usize,
) -> *mut AnyObject {
    // SAFETY: the caller passes `length` units.
    build(Some(unsafe { create::units(chars.as_ptr(), length) }))
}

extern "C-unwind" fn init_with_characters_no_copy(
    _: *mut AnyObject,
    _: Sel,
    chars: NonNull<u16>,
    length: usize,
    free_when_done: Bool,
) -> *mut AnyObject {
    // SAFETY: as above.
    let result = build(Some(unsafe { create::units(chars.as_ptr(), length) }));
    if free_when_done.as_bool() {
        // SAFETY: the caller hands over a malloc'ed buffer.
        unsafe { libc::free(chars.as_ptr().cast()) };
    }
    result
}

fn load() {
    let mut b = ClassBuilder::new(c"_SidestepStringPlaceholder", NSString::class())
        .expect("sidestep: the string placeholder class is defined once");
    // SAFETY: each function matches the selector's convention.
    unsafe {
        b.add_method(sel!(init), init as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(initWithBytes:length:encoding:), init_with_bytes as extern "C-unwind" fn(_, _, _, _, _) -> _);
        b.add_method(sel!(initWithData:encoding:), init_with_data as extern "C-unwind" fn(_, _, _, _) -> _);
        b.add_method(
            sel!(initWithBytesNoCopy:length:encoding:freeWhenDone:),
            init_with_bytes_no_copy as extern "C-unwind" fn(_, _, _, _, _, _) -> _,
        );
        b.add_method(sel!(initWithString:), init_with_string as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(initWithUTF8String:), init_with_utf8_string as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(initWithCString:encoding:), init_with_c_string as extern "C-unwind" fn(_, _, _, _) -> _);
        b.add_method(sel!(initWithCharacters:length:), init_with_characters as extern "C-unwind" fn(_, _, _, _) -> _);
        b.add_method(
            sel!(initWithCharactersNoCopy:length:freeWhenDone:),
            init_with_characters_no_copy as extern "C-unwind" fn(_, _, _, _, _) -> _,
        );
    }
    b.register();
}

// NSString's class methods.

/// Whether instances of `cls` are laid out by an initializer rather than
/// by `+alloc`: NSString itself and Sidestep's own immutable string
/// classes (what `[s class]` reports), whose instances an `+alloc` of
/// NSObject's would leave without their text.
fn built_by_init(cls: *const AnyClass) -> bool {
    let shells: [*const sidestep_runtime::Class; 4] = [
        &raw const crate::NSSTRING,
        &raw const super::inline::INLINE_CLASS,
        &raw const PLACEHOLDER_CLASS,
        &raw const crate::CONSTANT_STRING_CLASS,
    ];
    shells.iter().any(|&shell| std::ptr::eq(cls, shell.cast()))
}

extern "C-unwind" fn alloc(cls: *const AnyClass, _: Sel) -> *mut AnyObject {
    if built_by_init(cls) {
        PLACEHOLDER.as_object().cast()
    } else {
        // SAFETY: the receiver is a class; this is NSObject's +alloc.
        unsafe { objc2::ffi::class_createInstance(cls, 0) }
    }
}

extern "C-unwind" fn alloc_with_zone(cls: *const AnyClass, sel: Sel, _zone: *mut c_void) -> *mut AnyObject {
    alloc(cls, sel)
}

/// `[[cls alloc] <init>]`, autoreleased.
fn made(obj: *mut AnyObject) -> *mut AnyObject {
    // SAFETY: a +1 object or nil, returned autoreleased.
    unsafe { objc2::ffi::objc_autoreleaseReturnValue(obj) }
}

extern "C-unwind" fn string(cls: &AnyClass, _: Sel) -> *mut AnyObject {
    // SAFETY: every string class answers +alloc and -init.
    made(unsafe {
        let obj: *mut AnyObject = msg_send![cls, alloc];
        msg_send![obj, init]
    })
}

extern "C-unwind" fn string_with_string(cls: &AnyClass, _: Sel, s: &NSString) -> *mut AnyObject {
    // SAFETY: as above.
    made(unsafe {
        let obj: *mut AnyObject = msg_send![cls, alloc];
        msg_send![obj, initWithString: s]
    })
}

extern "C-unwind" fn string_with_utf8_string(cls: &AnyClass, _: Sel, s: NonNull<c_char>) -> *mut AnyObject {
    // SAFETY: as above.
    made(unsafe {
        let obj: *mut AnyObject = msg_send![cls, alloc];
        msg_send![obj, initWithUTF8String: s]
    })
}

extern "C-unwind" fn string_with_characters(
    cls: &AnyClass,
    _: Sel,
    chars: NonNull<u16>,
    length: usize,
) -> *mut AnyObject {
    // SAFETY: as above.
    made(unsafe {
        let obj: *mut AnyObject = msg_send![cls, alloc];
        msg_send![obj, initWithCharacters: chars, length: length]
    })
}

extern "C-unwind" fn string_with_c_string(
    cls: &AnyClass,
    _: Sel,
    s: NonNull<c_char>,
    encoding: NSStringEncoding,
) -> *mut AnyObject {
    // SAFETY: as above.
    made(unsafe {
        let obj: *mut AnyObject = msg_send![cls, alloc];
        msg_send![obj, initWithCString: s, encoding: encoding]
    })
}

extern "C-unwind" fn default_c_string_encoding(_: &AnyClass, _: Sel) -> NSStringEncoding {
    // C strings on Linux are UTF-8.
    encoding::raw(encoding::UTF8)
}

/// `+availableStringEncodings`: a 0-terminated list, of `int`s on GNUstep.
extern "C-unwind" fn available_string_encodings(_: &AnyClass, _: Sel) -> NonNull<NSStringEncoding> {
    static LIST: [NSStringEncoding; 12] = [
        encoding::ASCII_ENC as NSStringEncoding,
        encoding::UTF8 as NSStringEncoding,
        encoding::LATIN1 as NSStringEncoding,
        encoding::WINDOWS_1252 as NSStringEncoding,
        encoding::MAC_ROMAN as NSStringEncoding,
        encoding::UTF16 as NSStringEncoding,
        encoding::UTF16_BE as NSStringEncoding,
        encoding::UTF16_LE as NSStringEncoding,
        encoding::UTF32 as NSStringEncoding,
        encoding::UTF32_BE as NSStringEncoding,
        encoding::UTF32_LE as NSStringEncoding,
        0,
    ];
    NonNull::from(&LIST).cast()
}

/// `+[NSMutableString stringWithCapacity:]`, which must allocate the
/// receiving class. It is added to NSString's metaclass so that it exists
/// before NSMutableString can be used; NSString's placeholder answers no
/// `-initWithCapacity:`, so sent to NSString it fails as it does on macOS.
extern "C-unwind" fn string_with_capacity(cls: &AnyClass, _: Sel, capacity: usize) -> *mut AnyObject {
    // SAFETY: +alloc and -initWithCapacity: of a mutable string class.
    made(unsafe {
        let obj: *mut AnyObject = msg_send![cls, alloc];
        msg_send![obj, initWithCapacity: capacity]
    })
}

/// Add `+alloc` and the class methods to NSString's metaclass.
pub(crate) fn install(target: &AnyClass) {
    super::install::class_methods(target, c"_SidestepStringClassMethods", |b| {
        // SAFETY: each function matches the selector's convention.
        unsafe {
            b.add_method(sel!(alloc), alloc as extern "C-unwind" fn(_, _) -> _);
            b.add_method(sel!(allocWithZone:), alloc_with_zone as extern "C-unwind" fn(_, _, _) -> _);
            b.add_method(sel!(string), string as extern "C-unwind" fn(_, _) -> _);
            b.add_method(sel!(stringWithString:), string_with_string as extern "C-unwind" fn(_, _, _) -> _);
            b.add_method(sel!(stringWithCapacity:), string_with_capacity as extern "C-unwind" fn(_, _, _) -> _);
            b.add_method(sel!(stringWithUTF8String:), string_with_utf8_string as extern "C-unwind" fn(_, _, _) -> _);
            b.add_method(
                sel!(stringWithCharacters:length:),
                string_with_characters as extern "C-unwind" fn(_, _, _, _) -> _,
            );
            b.add_method(
                sel!(stringWithCString:encoding:),
                string_with_c_string as extern "C-unwind" fn(_, _, _, _) -> _,
            );
            b.add_method(sel!(defaultCStringEncoding), default_c_string_encoding as extern "C-unwind" fn(_, _) -> _);
            b.add_method(sel!(availableStringEncodings), available_string_encodings as extern "C-unwind" fn(_, _) -> _);
        }
    });
}
