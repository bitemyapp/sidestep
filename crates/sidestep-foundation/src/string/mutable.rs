//! `NSMutableString`: a growable WTF-8 buffer.
//!
//! `replaceCharactersInRange:withString:` is the primitive: every other
//! mutation is written in terms of it, and sends it as a message when a
//! subclass overrides it, so the subclass sees each change (the attributed
//! string backing store relies on this).
//!
//! The buffer lives in a `RefCell`. Foundation's contract makes mutable
//! strings single-threaded; the cell turns a violation into a panic rather
//! than undefined behaviour. No borrow is held across a message send, and
//! an argument that may be the receiver itself (`[s appendString:s]`) is
//! copied before the buffer is borrowed mutably.

use std::borrow::Cow;
use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send, sel};
use objc2_foundation::{NSMutableString, NSRange, NSString, NSStringCompareOptions, NSUInteger, NSZone};

use super::encoding::Decoded;
use super::index::{IndexRef, LocalIndex, Text, indexable};
use super::view::{self, StrView};
use super::wtf8::{self, ASCII, HAS_SURROGATE, Pos};
use super::{create, search};

/// A mutable string's contents.
#[derive(Default)]
pub(crate) struct MutBuf {
    bytes: Vec<u8>,
    utf16_len: usize,
    flags: u8,
    index: LocalIndex,
}

impl MutBuf {
    pub(crate) fn new(d: Decoded, capacity: usize) -> Self {
        let mut bytes = Vec::with_capacity(capacity.max(d.bytes.len()));
        bytes.extend_from_slice(&d.bytes);
        MutBuf { bytes, utf16_len: d.utf16_len, flags: d.flags, index: LocalIndex::default() }
    }

    fn empty(capacity: usize) -> Self {
        MutBuf { bytes: Vec::with_capacity(capacity), utf16_len: 0, flags: ASCII, index: LocalIndex::default() }
    }

    #[inline]
    pub(crate) fn text(&self) -> Text<'_> {
        let index = if indexable(self.bytes.len()) { IndexRef::Local(&self.index) } else { IndexRef::None };
        Text { bytes: &self.bytes, utf16_len: self.utf16_len, flags: self.flags, index }
    }

    pub(crate) fn len(&self) -> usize {
        self.utf16_len
    }

    /// Replace the UTF-16 range `loc..loc + len` (in bounds) with `insert`.
    pub(crate) fn replace(&mut self, loc: usize, len: usize, insert: &Text) {
        let (start, end) = self.text().range(loc, len);
        self.splice(start, end, insert);
        self.utf16_len = self.utf16_len - len + insert.utf16_len;
    }

    fn splice(&mut self, start: Pos, end: Pos, insert: &Text) {
        let split = start.low || end.low;
        wtf8::splice(&mut self.bytes, start, end, insert.bytes);
        // ASCII stays exact only while nothing but ASCII goes in; a string
        // that loses its last non-ASCII character keeps the slower path,
        // which is still correct.
        let ascii = if self.bytes.is_empty() { ASCII } else { self.flags & insert.flags & ASCII };
        let mut surrogate = (self.flags | insert.flags) & HAS_SURROGATE;
        if split || surrogate != 0 {
            // Splitting a pair can create a lone surrogate and joining can
            // remove one; look again.
            surrogate = if wtf8::has_surrogate(&self.bytes) { HAS_SURROGATE } else { 0 };
        }
        self.flags = ascii | surrogate;
        // Joins may reach back into the character before the edit.
        self.index.edited(start.byte.saturating_sub(4));
    }

    /// Take `bytes`, canonical WTF-8, as the whole text.
    fn set_bytes(&mut self, bytes: Vec<u8>) {
        self.utf16_len = wtf8::utf16_len(&bytes);
        self.flags = wtf8::flags_of(&bytes, true);
        self.bytes = bytes;
        self.index.edited(0);
    }

    fn set(&mut self, text: &Text) {
        self.bytes.clear();
        self.bytes.extend_from_slice(text.bytes);
        self.utf16_len = text.utf16_len;
        self.flags = text.flags;
        self.index.edited(0);
    }
}

sidestep_runtime::static_class!(pub(crate) NSMUTABLESTRING, NSMUTABLESTRING_META = "NSMutableString", || {
    let _ = NSMutableStringImpl::class();
});

pub(crate) struct MutableIvars {
    buf: RefCell<MutBuf>,
}

define_class!(
    #[unsafe(super(NSString, objc2::runtime::NSObject))]
    #[name = "NSMutableString"]
    #[ivars = MutableIvars]
    pub(crate) struct NSMutableStringImpl;

    impl NSMutableStringImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            finish(this, MutBuf::empty(0))
        }

        #[unsafe(method_id(initWithCapacity:))]
        fn init_with_capacity(this: Allocated<Self>, capacity: NSUInteger) -> Retained<Self> {
            finish(this, MutBuf::empty(capacity.min(1 << 20)))
        }

        #[unsafe(method_id(initWithString:))]
        fn init_with_string(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let buf = {
                let v = view::view(string);
                let t = v.text();
                MutBuf::new(Decoded { bytes: Cow::Borrowed(t.bytes), utf16_len: t.utf16_len, flags: t.flags }, 0)
            };
            finish(this, buf)
        }

        #[unsafe(method_id(initWithBytes:length:encoding:))]
        fn init_with_bytes(
            this: Allocated<Self>,
            bytes: *const c_void,
            length: NSUInteger,
            encoding: i32,
        ) -> Option<Retained<Self>> {
            // SAFETY: the caller passes `length` readable bytes.
            let decoded = unsafe { create::bytes(bytes, length, encoding as u32) };
            decoded.map(|d| finish(this, MutBuf::new(d, 0)))
        }

        #[unsafe(method_id(initWithBytesNoCopy:length:encoding:freeWhenDone:))]
        fn init_with_bytes_no_copy(
            this: Allocated<Self>,
            bytes: NonNull<c_void>,
            length: NSUInteger,
            encoding: NSUInteger,
            free_when_done: bool,
        ) -> Option<Retained<Self>> {
            // SAFETY: as above; the buffer is the caller's to hand over.
            let decoded = unsafe { create::bytes(bytes.as_ptr(), length, encoding as u32) };
            let result = decoded.map(|d| finish(this, MutBuf::new(d, 0)));
            // As for NSString: a failed initializer leaves the buffer to the
            // caller.
            if free_when_done && result.is_some() {
                // SAFETY: the caller gave up the malloc'ed buffer, which the
                // new string copied.
                unsafe { libc::free(bytes.as_ptr()) };
            }
            result
        }

        #[unsafe(method_id(initWithUTF8String:))]
        fn init_with_utf8_string(this: Allocated<Self>, s: NonNull<c_char>) -> Option<Retained<Self>> {
            // SAFETY: the caller passes a NUL-terminated string.
            let decoded = unsafe { create::c_string(s.as_ptr(), super::encoding::UTF8) };
            decoded.map(|d| finish(this, MutBuf::new(d, 0)))
        }

        #[unsafe(method_id(initWithCString:encoding:))]
        fn init_with_c_string(
            this: Allocated<Self>,
            s: NonNull<c_char>,
            encoding: NSUInteger,
        ) -> Option<Retained<Self>> {
            // SAFETY: as above.
            let decoded = unsafe { create::c_string(s.as_ptr(), encoding as u32) };
            decoded.map(|d| finish(this, MutBuf::new(d, 0)))
        }

        #[unsafe(method_id(initWithCharacters:length:))]
        fn init_with_characters(this: Allocated<Self>, chars: NonNull<u16>, length: NSUInteger) -> Retained<Self> {
            // SAFETY: the caller passes `length` units.
            let decoded = unsafe { create::units(chars.as_ptr(), length) };
            finish(this, MutBuf::new(decoded, 0))
        }

        #[unsafe(method_id(initWithCharactersNoCopy:length:freeWhenDone:))]
        fn init_with_characters_no_copy(
            this: Allocated<Self>,
            chars: NonNull<u16>,
            length: NSUInteger,
            free_when_done: bool,
        ) -> Retained<Self> {
            // SAFETY: as above.
            let decoded = unsafe { create::units(chars.as_ptr(), length) };
            if free_when_done {
                // SAFETY: the caller gave up the malloc'ed buffer.
                unsafe { libc::free(chars.as_ptr().cast()) };
            }
            finish(this, MutBuf::new(decoded, 0))
        }

        #[unsafe(method(replaceCharactersInRange:withString:))]
        fn replace_characters(&self, range: NSRange, string: &NSString) {
            replace_buffer(self, range, string, "replaceCharactersInRange:withString:");
        }

        #[unsafe(method(insertString:atIndex:))]
        fn insert_string(&self, string: &NSString, index: NSUInteger) {
            let len = length(self);
            if index > len {
                super::index_panic("insertString:atIndex:", index, len);
            }
            replace(self, NSRange::new(index, 0), string);
        }

        #[unsafe(method(deleteCharactersInRange:))]
        fn delete_characters(&self, range: NSRange) {
            let len = length(self);
            super::check_range("deleteCharactersInRange:", range, len);
            replace(self, range, &super::empty());
        }

        #[unsafe(method(appendString:))]
        fn append_string(&self, string: &NSString) {
            let len = length(self);
            replace(self, NSRange::new(len, 0), string);
        }

        #[unsafe(method(setString:))]
        fn set_string(&self, string: &NSString) {
            if is_native(self) {
                let copy = owned(string);
                self.ivars().buf.borrow_mut().set(&copy.text());
            } else {
                let len = length(self);
                replace(self, NSRange::new(0, len), string);
            }
        }

        #[unsafe(method(replaceOccurrencesOfString:withString:options:range:))]
        fn replace_occurrences(
            &self,
            target: &NSString,
            replacement: &NSString,
            options: NSStringCompareOptions,
            range: NSRange,
        ) -> NSUInteger {
            let len = length(self);
            super::check_range("replaceOccurrencesOfString:withString:options:range:", range, len);
            if is_native(self)
                && let Some(cell) = buffer(self)
            {
                // Build the new text in one pass and put it in place once.
                let done = {
                    let (hv, nv, rv) = (view::view(self), view::view(target), view::view(replacement));
                    search::replaced(&hv.text(), &nv.text(), &rv.text(), options.0, range)
                };
                return match done {
                    Some((bytes, count)) => {
                        cell.borrow_mut().set_bytes(bytes);
                        count
                    }
                    None => 0,
                };
            }
            // A subclass sees each edit through its primitive, made from the
            // end so the ranges before it stay valid.
            let edits: Vec<(NSRange, Retained<NSString>)> = {
                let (hv, nv, rv) = (view::view(self), view::view(target), view::view(replacement));
                let h = hv.text();
                let mut edits = Vec::new();
                search::each_replacement(&h, &nv.text(), &rv.text(), options.0, range, |s, e, with| {
                    let (loc, len) = search::pos_to_utf16(&h, s, e);
                    let with = super::inline::new(with, wtf8::utf16_len(with), wtf8::flags_of(with, true));
                    edits.push((NSRange::new(loc, len), with));
                });
                edits
            };
            for (r, with) in edits.iter().rev() {
                replace(self, *r, with);
            }
            edits.len()
        }

        #[unsafe(method(UTF8String))]
        fn utf8_string(&self) -> *const c_char {
            // A pointer into the buffer would dangle after the next edit, so
            // hand out an autoreleased immutable copy's bytes instead, as
            // objc2's `to_str` expects.
            let copy = immutable_copy(self);
            // SAFETY: a string answers -UTF8String.
            let ptr: *const c_char = unsafe { msg_send![&*copy, UTF8String] };
            let _ = Retained::autorelease_ptr(copy);
            ptr
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSString> {
            immutable_copy(self)
        }
    }

    unsafe impl NSObjectProtocol for NSMutableStringImpl {}
);

fn immutable_copy(this: &NSMutableStringImpl) -> Retained<NSString> {
    let v = view::view(this);
    let t = v.text();
    super::inline::new(t.bytes, t.utf16_len, t.flags)
}

/// The length of a mutable string, through its primitive when a subclass
/// keeps its own storage.
fn length(this: &NSMutableStringImpl) -> usize {
    match buffer(this) {
        Some(cell) => cell.borrow().len(),
        // SAFETY: every string answers -length.
        None => unsafe { msg_send![this, length] },
    }
}

fn finish(this: Allocated<NSMutableStringImpl>, buf: MutBuf) -> Retained<NSMutableStringImpl> {
    let this = this.set_ivars(MutableIvars { buf: RefCell::new(buf) });
    // SAFETY: NSObject's initializer; NSString has none of its own.
    unsafe { msg_send![super(this), init] }
}

/// Text copied out of a string, so no borrow of it is held, with an index
/// of its own.
pub(crate) struct Owned {
    bytes: Vec<u8>,
    utf16_len: usize,
    flags: u8,
    index: LocalIndex,
}

impl Owned {
    pub(crate) fn text(&self) -> Text<'_> {
        let index = if indexable(self.bytes.len()) { IndexRef::Local(&self.index) } else { IndexRef::None };
        Text { bytes: &self.bytes, utf16_len: self.utf16_len, flags: self.flags, index }
    }
}

/// Run `f` on a string's text: in place when the string is one of
/// Sidestep's immutable ones (which no edit can touch), copied otherwise,
/// so `f` may edit any mutable string, including this one.
pub(crate) fn with_text<R>(s: &AnyObject, f: impl FnOnce(&Text) -> R) -> R {
    if let Some(v) = view::native(s).filter(StrView::is_immutable) {
        return f(&v.text());
    }
    f(&owned(s).text())
}

pub(crate) fn owned(s: &AnyObject) -> Owned {
    let v = view::view(s);
    let t = v.text();
    Owned { bytes: t.bytes.to_vec(), utf16_len: t.utf16_len, flags: t.flags, index: LocalIndex::default() }
}

/// Apply the primitive: directly when the receiver's class keeps
/// NSMutableString's own implementation, otherwise by sending it, so
/// subclasses see every edit.
pub(crate) fn replace(this: &NSMutableStringImpl, range: NSRange, string: &NSString) {
    if is_native(this) {
        replace_buffer(this, range, string, "replaceCharactersInRange:withString:");
    } else {
        // SAFETY: the primitive every NSMutableString answers.
        let _: () = unsafe { msg_send![this, replaceCharactersInRange: range, withString: string] };
    }
}

/// The primitive on a string with NSMutableString's storage, editing the
/// buffer directly.
pub(crate) fn replace_buffer(this: &AnyObject, range: NSRange, string: &NSString, method: &str) {
    // An immutable string can be read in place; anything else could be this
    // very string, so copy it before borrowing the buffer mutably.
    let arg = view::native(string).filter(StrView::is_immutable);
    let copy;
    let insert = match &arg {
        Some(v) => v.text(),
        None => {
            copy = owned(string);
            copy.text()
        }
    };
    let mut buf = buffer(this).expect("sidestep: a string with NSMutableString's storage").borrow_mut();
    super::check_range(method, range, buf.len());
    buf.replace(range.location, range.length, &insert);
}

/// Edit a native mutable string's buffer directly: the attributed string
/// backing store, which owns one, uses this.
pub(crate) fn edit(obj: &AnyObject, range: NSRange, insert: &Text) {
    let cell = buffer(obj).expect("sidestep: a native mutable string");
    cell.borrow_mut().replace(range.location, range.length, insert);
}

/// Whether the receiver's `replaceCharactersInRange:withString:` is
/// NSMutableString's own.
fn is_native(this: &NSMutableStringImpl) -> bool {
    let class = class_of(this);
    std::ptr::eq(class, &NSMUTABLESTRING) || {
        // SAFETY: a class pointer taken from a live object.
        let class: &AnyClass = unsafe { &*class.cast() };
        super::install::keeps(class, NSMutableString::class(), &[sel!(replaceCharactersInRange:withString:)])
    }
}

fn class_of(obj: &AnyObject) -> *const sidestep_runtime::Class {
    // SAFETY: every object starts with its class pointer.
    unsafe { *(obj as *const AnyObject).cast::<*const sidestep_runtime::Class>() }
}

/// The buffer of a string whose storage is NSMutableString's: the class
/// itself, the attributed string backing store, and subclasses that keep
/// its primitives.
#[inline]
pub(crate) fn buffer(obj: &AnyObject) -> Option<&RefCell<MutBuf>> {
    let class = class_of(obj);
    let native = std::ptr::eq(class, &NSMUTABLESTRING)
        || std::ptr::eq(class, &crate::attributed::backing::ATTRIBUTED_TEXT)
        || keeps_storage(class);
    if !native {
        return None;
    }
    // SAFETY: an instance of NSMutableString or a subclass, so it has
    // NSMutableString's instance variables.
    let this = unsafe { &*(obj as *const AnyObject).cast::<NSMutableStringImpl>() };
    Some(&this.ivars().buf)
}

/// Whether an instance of `class` is a mutable string reading its text
/// from NSMutableString's buffer.
#[cold]
fn keeps_storage(class: *const sidestep_runtime::Class) -> bool {
    // SAFETY: a class pointer taken from a live object.
    let class: &AnyClass = unsafe { &*class.cast() };
    super::install::keeps(class, NSMutableString::class(), &[sel!(length), sel!(characterAtIndex:)])
}

/// A new NSMutableString with `text`.
pub(crate) fn new_mutable(text: &Text) -> Retained<NSMutableString> {
    // Loading the shell first makes NSMutableStringImpl's class the
    // registered NSMutableString (otherwise, this being the first use, a
    // second class of that name would be made).
    crate::load_shell(&NSMUTABLESTRING);
    let buf =
        MutBuf::new(Decoded { bytes: Cow::Borrowed(text.bytes), utf16_len: text.utf16_len, flags: text.flags }, 0);
    let this = finish(NSMutableStringImpl::alloc(), buf);
    // SAFETY: NSMutableStringImpl is the class registered as NSMutableString.
    unsafe { Retained::cast_unchecked(this) }
}
