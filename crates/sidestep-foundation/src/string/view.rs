//! How `NSString`'s methods read any string: a view of its text.
//!
//! `NSString` implements each selector once, over [`Text`]. Where the text
//! comes from depends on the receiver:
//!
//! - `_SidestepString`: the header and bytes inside the object.
//! - `_SidestepConstantString`: the static body.
//! - `NSMutableString` and subclasses that keep its storage: a shared borrow
//!   of its buffer, held only while Rust code reads it, never across a
//!   message send.
//! - Any other subclass (an app's own string class): a snapshot taken
//!   through its primitives, `length` and `getCharacters:range:`.

use std::cell::Ref;

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSRange;

use super::index::{IndexRef, LocalIndex, Text, indexable};
use super::inline::{self, Inline};
use super::mutable::{self, MutBuf};
use super::wtf8;

/// A string's text, borrowed or snapshotted.
pub(crate) enum StrView<'a> {
    Inline(&'a Inline),
    Constant(&'static crate::const_string::ConstStr),
    Mutable(Ref<'a, MutBuf>),
    Foreign(Snapshot),
}

/// The text of a string Sidestep doesn't store itself, with an index of
/// its own so that walking it by UTF-16 index is as cheap as walking one of
/// Sidestep's.
pub(crate) struct Snapshot {
    bytes: Vec<u8>,
    utf16_len: usize,
    flags: u8,
    index: LocalIndex,
}

impl StrView<'_> {
    #[inline]
    pub(crate) fn text(&self) -> Text<'_> {
        match self {
            StrView::Inline(s) => s.text(),
            StrView::Constant(c) => c.text(),
            StrView::Mutable(m) => m.text(),
            StrView::Foreign(s) => {
                let index = if indexable(s.bytes.len()) { IndexRef::Local(&s.index) } else { IndexRef::None };
                Text { bytes: &s.bytes, utf16_len: s.utf16_len, flags: s.flags, index }
            }
        }
    }

    /// Whether the text lives in an object that can't change, so a string
    /// with the same contents may simply be that object.
    pub(crate) fn is_immutable(&self) -> bool {
        matches!(self, StrView::Inline(_) | StrView::Constant(_))
    }
}

/// The view of a string Sidestep stores, or `None` for any other class.
#[inline]
pub(crate) fn native(obj: &AnyObject) -> Option<StrView<'_>> {
    if inline::is_inline(obj) {
        // SAFETY: checked the class.
        return Some(StrView::Inline(unsafe { inline::header(obj) }));
    }
    if crate::const_string::is_constant(obj) {
        return Some(StrView::Constant(crate::const_string::body(obj)));
    }
    mutable::buffer(obj).map(|cell| StrView::Mutable(cell.borrow()))
}

/// The view of any string.
#[inline]
pub(crate) fn view(obj: &AnyObject) -> StrView<'_> {
    match native(obj) {
        Some(v) => v,
        None => StrView::Foreign(snapshot(obj)),
    }
}

/// Read a string of another class through its primitives.
#[cold]
pub(crate) fn snapshot(obj: &AnyObject) -> Snapshot {
    // SAFETY: every NSString answers -length and -getCharacters:range:.
    let len: usize = unsafe { msg_send![obj, length] };
    let mut units = vec![0u16; len];
    if len > 0 {
        let range = NSRange::new(0, len);
        let buffer = units.as_mut_ptr();
        // SAFETY: room for `len` units.
        let _: () = unsafe { msg_send![obj, getCharacters: buffer, range: range] };
    }
    let bytes = wtf8::from_utf16(units.iter().copied(), len);
    let flags = wtf8::flags_of(&bytes, true);
    Snapshot { bytes, utf16_len: len, flags, index: LocalIndex::default() }
}
