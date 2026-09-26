//! Text editing: TextKit 1 (`NSTextStorage`, `NSLayoutManager`,
//! `NSTextContainer`), `NSTextView`, the field editor controls edit in,
//! and the undo manager glue.
//!
//! - `storage`: the text of a text storage as a tree of paragraphs, each
//!   its UTF-8 text and attribute runs over UTF-16 units; pure Rust.
//! - `attrs`: attribute dictionaries interned per storage.
//! - `text_storage`: `NSTextStorage` over them, with AppKit's editing and
//!   `processEditing` sequence, on any thread.
//! - `string_proxy`: the live string `-[NSTextStorage string]` returns.

pub(crate) mod attrs;
pub(crate) mod layout_manager;
pub(crate) mod storage;
pub(crate) mod string_proxy;
pub(crate) mod text_storage;
pub(crate) mod undo_text;

use objc2::runtime::AnyClass;

/// Whether `class` is `of` or a subclass of it.
pub(crate) fn is_kind(class: &AnyClass, of: &AnyClass) -> bool {
    let mut c = Some(class);
    while let Some(k) = c {
        if std::ptr::eq(k, of) {
            return true;
        }
        c = k.superclass();
    }
    false
}
