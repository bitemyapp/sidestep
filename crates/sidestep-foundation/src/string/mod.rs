//! `NSString` and `NSMutableString`.
//!
//! Text is stored as WTF-8 (`wtf8`): UTF-8, plus the lone surrogates
//! Foundation's UTF-16 model allows. Rust code, `UTF8String` and the text
//! engine read it without transcoding; UTF-16 indices go through a cursor
//! and crumbs (`index`), and are free for ASCII.
//!
//! The classes:
//!
//! - `NSString` is the public class. It has no instance variables and
//!   implements every selector once, over a view of the receiver's text
//!   (`view`), so its methods work for every string class, including an
//!   app's own subclasses, which it reads through their primitives.
//! - `_SidestepString` (`inline`) is what Sidestep creates: one allocation
//!   holding a header and the bytes. `+[NSString alloc]` returns a static
//!   placeholder whose initializers build one (`placeholder`).
//! - `_SidestepConstantString` (`crate::const_string`) holds static text.
//! - `NSMutableString` (`mutable`) keeps a growable buffer.
//!
//! Groups of methods (search, case, lines, paths, numbers) live in files of
//! their own as helper classes whose methods are copied onto NSString when
//! it loads (`install`).

use std::ffi::{c_char, c_void};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, NSObjectProtocol};
use objc2::{ClassType, Message, define_class, msg_send};
use objc2_foundation::{NSMutableString, NSRange, NSString, NSStringEncodingConversionOptions, NSUInteger, NSZone};

use crate::const_string::{ConstStr, ConstantString};

mod case;
mod compare;
mod components;
mod create;
pub(crate) mod encoding;
pub(crate) mod fold;
pub(crate) mod index;
pub(crate) mod inline;
pub(crate) mod install;
pub(crate) mod lines;
pub(crate) mod mutable;
pub(crate) mod numeric;
pub(crate) mod paths;
mod placeholder;
pub(crate) mod search;
pub(crate) mod view;
pub(crate) mod wtf8;

use index::Text;
use view::{StrView, native, view};

/// The empty string every empty result shares.
static EMPTY: ConstantString = ConstantString::new(&crate::CONSTANT_STRING_CLASS, ConstStr::new("\0"));

/// The shared empty string.
pub(crate) fn empty() -> Retained<NSString> {
    // SAFETY: an immortal constant string; retaining it does nothing.
    unsafe { Retained::retain(EMPTY.as_object().cast()) }.expect("static object")
}

/// The hash of a string's UTF-8 (WTF-8) bytes. Every string class's `-hash`
/// is this, and collections hash Sidestep's own strings with it directly. It
/// is never 0, which marks a hash not yet computed.
pub(crate) const fn hash_bytes(b: &[u8]) -> NSUInteger {
    // FxHash over 8-byte words, then MurmurHash3's finalizer so every bit
    // depends on every input bit. Fast rather than collision-resistant, as
    // Foundation's string hash is.
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h = b.len() as u64;
    let mut i = 0;
    while i + 8 <= b.len() {
        let w = u64::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3], b[i + 4], b[i + 5], b[i + 6], b[i + 7]]);
        h = (h.rotate_left(5) ^ w).wrapping_mul(K);
        i += 8;
    }
    let (mut w, mut shift) = (0u64, 0);
    while i < b.len() {
        w |= (b[i] as u64) << shift;
        shift += 8;
        i += 1;
    }
    h = (h.rotate_left(5) ^ w).wrapping_mul(K);
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 33;
    match h as NSUInteger {
        0 => 1,
        h => h,
    }
}

/// The text and hash of a string whose class is exactly one of Sidestep's
/// immutable ones, read directly. Other strings (mutable ones, subclasses
/// that may override `-hash` and `-isEqual:`, and text holding a lone
/// surrogate, which is no `&str`) get `None` and must be sent messages.
#[inline]
pub(crate) fn fast_parts(obj: &AnyObject) -> Option<(&str, NSUInteger)> {
    if inline::is_inline(obj) {
        // SAFETY: checked the class.
        let s = unsafe { inline::header(obj) };
        return s.as_str().map(|text| (text, s.hash()));
    }
    if crate::const_string::is_constant(obj) {
        let body = crate::const_string::body(obj);
        return Some((body.as_str(), body.hash));
    }
    None
}

/// `-lengthOfBytesUsingEncoding:` for UTF-8 text: 0 when it can't be
/// encoded.
pub(crate) fn byte_length(s: &str, encoding: u32) -> NSUInteger {
    let text = Text::plain(s.as_bytes(), wtf8::utf16_len(s.as_bytes()), wtf8::flags_of(s.as_bytes(), false));
    encoding::byte_length(&text, encoding)
}

/// Whether `other` holds exactly the text `this`.
pub(crate) fn equals(this: &str, other: &NSString) -> bool {
    equal_bytes(this.as_bytes(), other)
}

/// Whether `other` holds exactly the WTF-8 text `this`.
pub(crate) fn equal_bytes(this: &[u8], other: &NSString) -> bool {
    view(other).text().bytes == this
}

/// Run `f` on a string's text as `&str`: in place for Sidestep's immutable
/// strings, copied for others, and with each lone surrogate replaced by
/// U+FFFD.
#[doc(hidden)]
pub fn with_str<R>(s: &NSString, f: impl FnOnce(&str) -> R) -> R {
    match native(s) {
        Some(v @ (StrView::Inline(_) | StrView::Constant(_))) => {
            let t = v.text();
            match t.as_str() {
                Some(text) => f(text),
                None => f(&wtf8::to_str_lossy(t.bytes, t.flags)),
            }
        }
        _ => {
            // Copy, so `f` may do anything, including edit the string.
            let owned = {
                let v = view(s);
                let t = v.text();
                wtf8::to_str_lossy(t.bytes, t.flags).into_owned()
            };
            f(&owned)
        }
    }
}

#[cold]
#[track_caller]
pub(crate) fn index_panic(method: &str, index: usize, len: usize) -> ! {
    panic!("-[NSString {method}]: index {index} out of bounds; string length {len}");
}

/// Panic, in Foundation's words, unless `range` lies within `len` units.
#[inline]
#[track_caller]
pub(crate) fn check_range(method: &str, range: NSRange, len: usize) {
    if range.location.checked_add(range.length).is_none_or(|end| end > len) {
        range_panic(method, range, len);
    }
}

#[cold]
#[track_caller]
fn range_panic(method: &str, range: NSRange, len: usize) -> ! {
    panic!("-[NSString {method}]: Range {{{}, {}}} out of bounds; string length {len}", range.location, range.length);
}

/// A new immutable string with the UTF-16 range of `t`, or `whole` when the
/// range covers an immutable receiver entirely.
fn substring(obj: &AnyObject, v: &StrView, loc: usize, len: usize) -> Retained<NSString> {
    let t = v.text();
    if loc == 0 && len == t.utf16_len && v.is_immutable() {
        // SAFETY: the receiver is a string.
        return unsafe { Retained::retain((obj as *const AnyObject).cast_mut().cast()) }.expect("non-null");
    }
    let (start, end) = t.range(loc, len);
    let bytes = wtf8::slice(t.bytes, start, end);
    let flags = if t.is_ascii() {
        wtf8::ASCII
    } else {
        wtf8::flags_of(&bytes, start.low || end.low || t.flags & wtf8::HAS_SURROGATE != 0)
    };
    inline::new(&bytes, len, flags)
}

/// A new immutable string with the UTF-16 range of an app's own string
/// class, read through its primitives: only the range, not the whole text.
fn foreign_substring(obj: &AnyObject, method: &str, range: NSRange) -> Retained<NSString> {
    // SAFETY: -length is a primitive.
    let len: usize = unsafe { msg_send![obj, length] };
    check_range(method, range, len);
    let mut units = vec![0u16; range.length];
    if range.length > 0 {
        let buffer = units.as_mut_ptr();
        // SAFETY: room for `range.length` units, a range within the string.
        let _: () = unsafe { msg_send![obj, getCharacters: buffer, range: range] };
    }
    let bytes = wtf8::from_utf16(units.iter().copied(), range.length);
    inline::new(&bytes, range.length, wtf8::flags_of(&bytes, true))
}

/// Bytes kept alive by the current autorelease pool, for C strings in
/// encodings other than UTF-8.
fn autoreleased_bytes(bytes: &[u8], nul: usize) -> *const c_char {
    static CLASS: std::sync::OnceLock<&'static AnyClass> = std::sync::OnceLock::new();
    let class = CLASS.get_or_init(|| {
        ClassBuilder::new(c"_SidestepCStringBuffer", NSObject::class()).expect("defined once").register()
    });
    // SAFETY: a plain object with room for the bytes and terminator, handed
    // to the pool; the runtime zeroes the terminator.
    unsafe {
        let obj = objc2::ffi::class_createInstance(*class, bytes.len() + nul);
        assert!(!obj.is_null(), "sidestep: out of memory");
        let dst = objc2::ffi::object_getIndexedIvars(obj).cast::<u8>().cast_mut();
        dst.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        objc2::ffi::objc_autorelease(obj);
        dst.cast()
    }
}

/// A string's text in `encoding` as an autoreleased C string, or NULL.
fn c_string(obj: &AnyObject, encoding: u32) -> *const c_char {
    let v = view(obj);
    let t = v.text();
    if encoding == encoding::UTF8 && v.is_immutable() {
        // The bytes are NUL-terminated in place.
        return if t.as_str().is_some() { t.bytes.as_ptr().cast() } else { std::ptr::null() };
    }
    match encoding::encode_all(&t, encoding, false) {
        Some(bytes) => autoreleased_bytes(&bytes, encoding::nul_width(encoding)),
        None => std::ptr::null(),
    }
}

define_class!(
    // NSString's own methods, copied onto NSString when it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringBase"]
    pub(crate) struct NSStringImpl;

    impl NSStringImpl {
        #[unsafe(method(length))]
        fn length(&self) -> NSUInteger {
            match native(self) {
                Some(v) => v.text().utf16_len,
                None => abstract_method(self, "length"),
            }
        }

        #[unsafe(method(characterAtIndex:))]
        fn character_at_index(&self, index: NSUInteger) -> u16 {
            match native(self) {
                Some(v) => {
                    let t = v.text();
                    if index >= t.utf16_len {
                        index_panic("characterAtIndex:", index, t.utf16_len);
                    }
                    t.unit(index)
                }
                None => abstract_method(self, "characterAtIndex:"),
            }
        }

        #[unsafe(method(getCharacters:range:))]
        fn get_characters_range(&self, buffer: NonNull<u16>, range: NSRange) {
            match native(self) {
                Some(v) => {
                    let t = v.text();
                    check_range("getCharacters:range:", range, t.utf16_len);
                    // SAFETY: the caller passes room for `range.length` units.
                    let out = unsafe { std::slice::from_raw_parts_mut(buffer.as_ptr(), range.length) };
                    t.copy_units(range.location, range.length, out);
                }
                None => {
                    // A subclass with only the two primitives.
                    for i in 0..range.length {
                        // SAFETY: -characterAtIndex: is a primitive; the
                        // caller passes room for `range.length` units.
                        unsafe { *buffer.as_ptr().add(i) = msg_send![self, characterAtIndex: range.location + i] };
                    }
                }
            }
        }

        #[unsafe(method(getCharacters:))]
        fn get_characters(&self, buffer: NonNull<u16>) {
            // SAFETY: -length is a primitive.
            let len: usize = unsafe { msg_send![self, length] };
            // SAFETY: the caller passes room for the whole string.
            let _: () = unsafe { msg_send![self, getCharacters: buffer, range: NSRange::new(0, len)] };
        }

        #[unsafe(method(UTF8String))]
        fn utf8_string(&self) -> *const c_char {
            c_string(self, encoding::UTF8)
        }

        #[unsafe(method(lengthOfBytesUsingEncoding:))]
        fn length_of_bytes(&self, encoding: i32) -> NSUInteger {
            encoding::byte_length(&view(self).text(), encoding as u32)
        }

        #[unsafe(method(maximumLengthOfBytesUsingEncoding:))]
        fn maximum_length_of_bytes(&self, encoding: NSUInteger) -> NSUInteger {
            // SAFETY: -length is a primitive.
            let len: usize = unsafe { msg_send![self, length] };
            encoding::max_byte_length(len, encoding::arg(encoding))
        }

        #[unsafe(method(cStringUsingEncoding:))]
        fn c_string_using_encoding(&self, encoding: NSUInteger) -> *const c_char {
            c_string(self, encoding::arg(encoding))
        }

        #[unsafe(method(getCString:maxLength:encoding:))]
        fn get_c_string(&self, buffer: NonNull<c_char>, max: NSUInteger, encoding: NSUInteger) -> bool {
            let encoding = encoding::arg(encoding);
            let encoded = encoding::encode_all(&view(self).text(), encoding, false);
            let nul = encoding::nul_width(encoding);
            // SAFETY: the caller passes room for `max` bytes.
            let out = unsafe { std::slice::from_raw_parts_mut(buffer.as_ptr().cast::<u8>(), max) };
            match encoded {
                Some(bytes) if bytes.len() + nul <= max => {
                    out[..bytes.len()].copy_from_slice(&bytes);
                    out[bytes.len()..bytes.len() + nul].fill(0);
                    true
                }
                _ => {
                    if let Some(first) = out.first_mut() {
                        *first = 0;
                    }
                    false
                }
            }
        }

        #[unsafe(method(canBeConvertedToEncoding:))]
        fn can_be_converted(&self, encoding: NSUInteger) -> bool {
            encoding::can_convert(&view(self).text(), encoding::arg(encoding))
        }

        #[unsafe(method(fastestEncoding))]
        fn fastest_encoding(&self) -> NSUInteger {
            encoding::fastest(&view(self).text()) as NSUInteger
        }

        #[unsafe(method(smallestEncoding))]
        fn smallest_encoding(&self) -> NSUInteger {
            encoding::smallest(&view(self).text()) as NSUInteger
        }

        #[unsafe(method(getBytes:maxLength:usedLength:encoding:options:range:remainingRange:))]
        fn get_bytes(
            &self,
            buffer: *mut c_void,
            max: NSUInteger,
            used: *mut NSUInteger,
            encoding: NSUInteger,
            options: NSStringEncodingConversionOptions,
            range: NSRange,
            leftover: *mut NSRange,
        ) -> bool {
            let v = view(self);
            let t = v.text();
            check_range("getBytes:maxLength:usedLength:encoding:options:range:remainingRange:", range, t.utf16_len);
            let encoding = encoding::arg(encoding);
            // With no buffer (or no room) only the length is measured, as
            // on macOS.
            let out = (!buffer.is_null() && max > 0).then(|| {
                // SAFETY: the caller passes room for `max` bytes.
                unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u8>(), max) }
            });
            let done = if encoding::supported(encoding) {
                encoding::encode_units(&t, range.location, range.length, encoding, options.0, max, out)
            } else {
                encoding::Encoded { used: 0, consumed: 0 }
            };
            // SAFETY: each out-parameter is valid or null.
            unsafe {
                if !used.is_null() {
                    *used = done.used;
                }
                if !leftover.is_null() {
                    *leftover = NSRange::new(range.location + done.consumed, range.length - done.consumed);
                }
            }
            done.consumed > 0
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            match native(self) {
                Some(StrView::Inline(s)) => s.hash(),
                Some(StrView::Constant(c)) => c.hash,
                _ => hash_bytes(view(self).text().bytes),
            }
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            let this: &AnyObject = self;
            match other {
                Some(o) if std::ptr::eq(o, this) => true,
                Some(o) => match o.downcast_ref::<NSString>() {
                    Some(o) => equal_bytes(view(self).text().bytes, o),
                    None => false,
                },
                None => false,
            }
        }

        #[unsafe(method(isEqualToString:))]
        fn is_equal_to_string(&self, other: &NSString) -> bool {
            let (this, that): (&AnyObject, &AnyObject) = (self, other);
            std::ptr::eq(this, that) || equal_bytes(view(self).text().bytes, other)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSString> {
            let v = view(self);
            let t = v.text();
            substring(self, &v, 0, t.utf16_len)
        }

        #[unsafe(method_id(mutableCopyWithZone:))]
        fn mutable_copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMutableString> {
            mutable::new_mutable(&view(self).text())
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            // SAFETY: the receiver is a string.
            unsafe { Retained::cast_unchecked(self.retain()) }
        }

        #[unsafe(method_id(substringFromIndex:))]
        fn substring_from_index(&self, from: NSUInteger) -> Retained<NSString> {
            let v = view(self);
            let len = v.text().utf16_len;
            if from > len {
                index_panic("substringFromIndex:", from, len);
            }
            substring(self, &v, from, len - from)
        }

        #[unsafe(method_id(substringToIndex:))]
        fn substring_to_index(&self, to: NSUInteger) -> Retained<NSString> {
            let v = view(self);
            let len = v.text().utf16_len;
            if to > len {
                index_panic("substringToIndex:", to, len);
            }
            substring(self, &v, 0, to)
        }

        #[unsafe(method_id(substringWithRange:))]
        fn substring_with_range(&self, range: NSRange) -> Retained<NSString> {
            match native(self) {
                Some(v) => {
                    check_range("substringWithRange:", range, v.text().utf16_len);
                    substring(self, &v, range.location, range.length)
                }
                None => foreign_substring(self, "substringWithRange:", range),
            }
        }

        #[unsafe(method_id(stringByAppendingString:))]
        fn string_by_appending_string(&self, other: &NSString) -> Retained<NSString> {
            let (a, b) = (view(self), view(other));
            let (ta, tb) = (a.text(), b.text());
            if tb.utf16_len == 0 { substring(self, &a, 0, ta.utf16_len) } else { inline::concat(&ta, &tb) }
        }
    }

    unsafe impl NSObjectProtocol for NSStringImpl {}
);

/// What a primitive of an abstract `NSString` subclass does when the
/// subclass didn't implement it.
#[cold]
#[track_caller]
fn abstract_method(obj: &AnyObject, method: &str) -> ! {
    panic!(
        "-[NSString {method}]: only defined for abstract class. Define -[{} {method}]!",
        obj.class().name().to_string_lossy()
    )
}

/// NSString's loader.
///
/// NSString is put together by hand rather than by `define_class!`: every
/// method, from each file's helper class and the class methods that need
/// their receiver, is added before the class is registered. Once registered
/// the class is visible to every thread, and a method added after that
/// could be missing for another thread's first message.
pub(crate) fn load() {
    // SAFETY: the runtime hands over NSString's pending shell; methods are
    // added to it before it is registered.
    unsafe {
        let cls = objc2::ffi::objc_allocateClassPair(NSObject::class(), c"NSString".as_ptr(), 0);
        assert!(!cls.is_null(), "sidestep: NSString is defined once");
        let target: &AnyClass = &*cls;
        install::copy_methods(NSStringImpl::class(), target, false);
        placeholder::install(target);
        search::install(target);
        compare::install(target);
        case::install(target);
        lines::install(target);
        components::install(target);
        numeric::install(target);
        paths::install(target);
        objc2::ffi::objc_registerClassPair(cls);
    }
}
