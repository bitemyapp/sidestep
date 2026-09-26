//! `_SidestepString`, the class of every immutable string Sidestep creates
//! at run time.
//!
//! A string is one allocation: the object, then a header, then its WTF-8
//! bytes and a NUL, all in the extra bytes `class_createInstance` provides.
//! Creating one is a single allocation and a copy, with nothing to
//! initialize lazily until it is used: the hash, the UTF-16 cursor and the
//! crumbs (see `index`) are computed on first use and are safe to share
//! between threads.
//!
//! The class overrides the methods that matter per character or per
//! lookup, so they read the header directly instead of going through
//! `NSString`'s general path.

use std::ffi::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, NSObject, Sel};
use objc2::{ClassType, sel};
use objc2_foundation::{NSRange, NSString, NSZone};

use super::index::{IndexRef, SharedIndex, Text, indexable};
use super::wtf8::{self, HAS_SURROGATE};

sidestep_runtime::static_class!(pub(crate) INLINE_CLASS, INLINE_META = "_SidestepString", load);

/// The header in front of a string's bytes.
#[repr(C)]
pub(crate) struct Inline {
    utf8_len: usize,
    utf16_len: usize,
    flags: u8,
    /// `-hash`, computed on first use; 0 until then.
    hash: AtomicUsize,
    index: SharedIndex,
}

/// Where the header starts, past the object's `isa` (the class has no
/// instance variables; `load` checks).
const HEADER_OFFSET: usize = size_of::<*const AnyClass>();

impl Inline {
    #[inline(always)]
    fn bytes_ptr(&self) -> *const u8 {
        // SAFETY: the bytes follow the header in the same allocation.
        unsafe { (self as *const Inline).add(1).cast::<u8>() }
    }

    #[inline(always)]
    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: `utf8_len` bytes were written after the header.
        unsafe { std::slice::from_raw_parts(self.bytes_ptr(), self.utf8_len) }
    }

    #[inline(always)]
    pub(crate) fn text(&self) -> Text<'_> {
        let index = if indexable(self.utf8_len) { IndexRef::Shared(&self.index) } else { IndexRef::None };
        Text { bytes: self.bytes(), utf16_len: self.utf16_len, flags: self.flags, index }
    }

    #[inline]
    pub(crate) fn hash(&self) -> usize {
        match self.hash.load(Ordering::Relaxed) {
            0 => {
                let hash = super::hash_bytes(self.bytes());
                self.hash.store(hash, Ordering::Relaxed);
                hash
            }
            hash => hash,
        }
    }

    /// The text as `&str`, unless it holds a lone surrogate.
    #[inline]
    pub(crate) fn as_str(&self) -> Option<&str> {
        if self.flags & HAS_SURROGATE != 0 {
            return None;
        }
        // SAFETY: without surrogates the bytes are UTF-8.
        Some(unsafe { std::str::from_utf8_unchecked(self.bytes()) })
    }
}

/// The header of a `_SidestepString`.
///
/// # Safety
/// `obj` must be an instance of exactly `_SidestepString`.
#[inline(always)]
pub(crate) unsafe fn header<'a>(obj: *const AnyObject) -> &'a Inline {
    // SAFETY: the header sits right after the isa, as `new` lays it out.
    unsafe { &*obj.cast::<u8>().add(HEADER_OFFSET).cast::<Inline>() }
}

#[inline(always)]
pub(crate) fn is_inline(obj: &AnyObject) -> bool {
    // SAFETY: every object starts with its class pointer.
    let class = unsafe { *(obj as *const AnyObject).cast::<*const sidestep_runtime::Class>() };
    std::ptr::eq(class, &INLINE_CLASS)
}

fn class() -> *const AnyClass {
    (&raw const INLINE_CLASS).cast()
}

/// A new immutable string holding `bytes`, which must be canonical WTF-8
/// whose UTF-16 length and flags are `utf16_len` and `flags`. Empty text
/// gives the shared empty string.
pub(crate) fn new(bytes: &[u8], utf16_len: usize, flags: u8) -> Retained<NSString> {
    if bytes.is_empty() {
        return super::empty();
    }
    debug_assert_eq!(wtf8::utf16_len(bytes), utf16_len);
    debug_assert!(flags & HAS_SURROGATE != 0 || std::str::from_utf8(bytes).is_ok());
    let extra = size_of::<Inline>() + bytes.len() + 1;
    // SAFETY: a registered class; the runtime zeroes the object.
    let obj = unsafe { objc2::ffi::class_createInstance(class(), extra) };
    assert!(!obj.is_null(), "sidestep: out of memory");
    // SAFETY: the extra bytes hold the header, the bytes and the NUL, and
    // nothing else refers to the object yet.
    unsafe {
        let header = obj.cast::<u8>().add(HEADER_OFFSET).cast::<Inline>();
        header.write(Inline {
            utf8_len: bytes.len(),
            utf16_len,
            flags,
            hash: AtomicUsize::new(0),
            index: SharedIndex::default(),
        });
        let dst = header.add(1).cast::<u8>();
        dst.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        *dst.add(bytes.len()) = 0;
        Retained::from_raw(obj.cast()).unwrap_unchecked()
    }
}

/// A new string with the concatenation of two WTF-8 texts, in one
/// allocation unless a surrogate pair joins across them.
pub(crate) fn concat(a: &Text, b: &Text) -> Retained<NSString> {
    if wtf8::joins(a.bytes, b.bytes) {
        let mut v = a.bytes.to_vec();
        wtf8::push(&mut v, b.bytes);
        let flags = wtf8::flags_of(&v, true);
        return new(&v, a.utf16_len + b.utf16_len, flags);
    }
    if a.bytes.is_empty() || b.bytes.is_empty() {
        let only = if a.bytes.is_empty() { b } else { a };
        return new(only.bytes, only.utf16_len, only.flags);
    }
    let len = a.bytes.len() + b.bytes.len();
    let extra = size_of::<Inline>() + len + 1;
    let flags = (a.flags & b.flags & wtf8::ASCII) | ((a.flags | b.flags) & HAS_SURROGATE);
    // SAFETY: as in `new`.
    unsafe {
        let obj = objc2::ffi::class_createInstance(class(), extra);
        assert!(!obj.is_null(), "sidestep: out of memory");
        let header = obj.cast::<u8>().add(HEADER_OFFSET).cast::<Inline>();
        header.write(Inline {
            utf8_len: len,
            utf16_len: a.utf16_len + b.utf16_len,
            flags,
            hash: AtomicUsize::new(0),
            index: SharedIndex::default(),
        });
        let dst = header.add(1).cast::<u8>();
        dst.copy_from_nonoverlapping(a.bytes.as_ptr(), a.bytes.len());
        dst.add(a.bytes.len()).copy_from_nonoverlapping(b.bytes.as_ptr(), b.bytes.len());
        *dst.add(len) = 0;
        Retained::from_raw(obj.cast()).unwrap_unchecked()
    }
}

// The overrides. Each is only ever installed on `_SidestepString`, so the
// receiver is one.

fn this(obj: &AnyObject) -> &Inline {
    // SAFETY: see above.
    unsafe { header(obj) }
}

extern "C-unwind" fn length(obj: &AnyObject, _: Sel) -> usize {
    this(obj).utf16_len
}

extern "C-unwind" fn character_at_index(obj: &AnyObject, _: Sel, index: usize) -> u16 {
    let s = this(obj);
    if index >= s.utf16_len {
        super::index_panic("characterAtIndex:", index, s.utf16_len);
    }
    if s.flags & wtf8::ASCII != 0 {
        // SAFETY: in bounds, checked above.
        return u16::from(unsafe { *s.bytes_ptr().add(index) });
    }
    s.text().unit(index)
}

extern "C-unwind" fn get_characters_range(obj: &AnyObject, _: Sel, buffer: *mut u16, range: NSRange) {
    let s = this(obj);
    super::check_range("getCharacters:range:", range, s.utf16_len);
    if range.length == 0 {
        return;
    }
    // SAFETY: the caller passes room for `range.length` units.
    let out = unsafe { std::slice::from_raw_parts_mut(buffer, range.length) };
    s.text().copy_units(range.location, range.length, out);
}

extern "C-unwind" fn utf8_string(obj: &AnyObject, _: Sel) -> *const c_char {
    let s = this(obj);
    // A lone surrogate has no UTF-8 form; macOS answers NULL.
    match s.as_str() {
        Some(_) => s.bytes_ptr().cast(),
        None => std::ptr::null(),
    }
}

extern "C-unwind" fn length_of_bytes(obj: &AnyObject, _: Sel, encoding: i32) -> usize {
    let s = this(obj);
    if encoding as u32 == super::encoding::UTF8 && s.flags & HAS_SURROGATE == 0 {
        return s.utf8_len;
    }
    super::encoding::byte_length(&s.text(), encoding as u32)
}

extern "C-unwind" fn hash(obj: &AnyObject, _: Sel) -> usize {
    this(obj).hash()
}

extern "C-unwind" fn is_equal(obj: &AnyObject, _: Sel, other: Option<&AnyObject>) -> Bool {
    let Some(other) = other else { return Bool::NO };
    if std::ptr::eq(obj, other) {
        return Bool::YES;
    }
    if is_inline(other) {
        let (a, b) = (this(obj), this(other));
        return Bool::new(a.utf8_len == b.utf8_len && a.bytes() == b.bytes());
    }
    match other.downcast_ref::<NSString>() {
        Some(other) => Bool::new(super::equal_bytes(this(obj).bytes(), other)),
        None => Bool::NO,
    }
}

extern "C-unwind" fn is_equal_to_string(obj: &AnyObject, _: Sel, other: &NSString) -> Bool {
    if is_inline(other) {
        let (a, b) = (this(obj), this(other));
        return Bool::new(a.utf8_len == b.utf8_len && a.bytes() == b.bytes());
    }
    Bool::new(super::equal_bytes(this(obj).bytes(), other))
}

extern "C-unwind" fn copy_with_zone(obj: *mut AnyObject, _: Sel, _zone: *mut NSZone) -> *mut AnyObject {
    // Immutable: the copy is the same object.
    // SAFETY: a live object.
    unsafe { objc2::ffi::objc_retain(obj) }
}

extern "C-unwind" fn description(obj: *mut AnyObject, _: Sel) -> *mut AnyObject {
    // SAFETY: returned +0, as -description is; the receiver keeps it alive.
    obj
}

unsafe extern "C-unwind" fn dealloc(obj: *mut AnyObject, _: Sel) {
    // SAFETY: the object is being destroyed, so nothing else reads its
    // index; the runtime frees the memory.
    unsafe {
        let header = obj.cast::<u8>().add(HEADER_OFFSET).cast::<Inline>();
        (*header).index.free();
        #[allow(deprecated)]
        objc2::ffi::object_dispose(obj);
    }
}

fn load() {
    let mut b =
        ClassBuilder::new(c"_SidestepString", NSString::class()).expect("sidestep: the string class is defined once");
    // SAFETY: each function matches the selector's convention.
    unsafe {
        b.add_method(sel!(length), length as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(characterAtIndex:), character_at_index as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(getCharacters:range:), get_characters_range as extern "C-unwind" fn(_, _, _, _));
        b.add_method(sel!(UTF8String), utf8_string as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(lengthOfBytesUsingEncoding:), length_of_bytes as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(hash), hash as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(isEqual:), is_equal as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(isEqualToString:), is_equal_to_string as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(copyWithZone:), copy_with_zone as extern "C-unwind" fn(_, _, _) -> _);
        b.add_method(sel!(description), description as extern "C-unwind" fn(_, _) -> _);
        b.add_method(sel!(dealloc), dealloc as unsafe extern "C-unwind" fn(_, _));
    }
    let cls = b.register();
    assert_eq!(cls.instance_size(), NSObject::class().instance_size(), "sidestep: NSString has no ivars");
}
