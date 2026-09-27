//! Text editing: TextKit 1 (`NSTextStorage`, `NSLayoutManager`,
//! `NSTextContainer`), `NSTextView`, the field editor controls edit in,
//! and the undo manager glue. TextKit 2 is in `textkit2`, over these.
//!
//! - `storage`: the text of a text storage as a tree of paragraphs, each
//!   its UTF-8 text and attribute runs over UTF-16 units; pure Rust.
//! - `attrs`: attribute dictionaries interned per storage.
//! - `text_storage`: `NSTextStorage` over them, with AppKit's editing and
//!   `processEditing` sequence, on any thread.
//! - `string_proxy`: the live string `-[NSTextStorage string]` returns.
//! - `blocks`: `NSTextBlock`, `NSTextTableBlock`, `NSTextTable`, and how
//!   paragraphs in them are placed.
//! - `temporary`: a layout manager's temporary attributes.

pub(crate) mod attrs;
pub(crate) mod blocks;
pub(crate) mod commands;
pub(crate) mod container;
pub(crate) mod drop;
pub(crate) mod edit;
pub(crate) mod field_editor;
#[doc(hidden)]
pub use field_editor::testing;
pub(crate) mod input_client;
pub(crate) mod layout_cache;
pub(crate) mod layout_manager;
pub(crate) mod notify;
pub(crate) mod selection;
pub(crate) mod storage;
pub(crate) mod string_proxy;
pub(crate) mod temporary;
pub(crate) mod text_storage;
pub(crate) mod text_view;
pub(crate) mod undo_text;

use objc2::runtime::{AnyClass, AnyObject, Sel};

/// Whether `obj` answers `sel`.
pub(crate) fn responds(obj: &AnyObject, sel: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector.
    unsafe { objc2::msg_send![obj, respondsToSelector: sel] }
}

/// Whether `class` has `base`'s own implementation of `sel` (nothing
/// between them overrides it).
pub(crate) fn same_method(class: &AnyClass, base: &AnyClass, sel: Sel) -> bool {
    std::ptr::eq(class, base)
        || match (class.instance_method(sel), base.instance_method(sel)) {
            (Some(a), Some(b)) => std::ptr::fn_addr_eq(a.implementation(), b.implementation()),
            _ => false,
        }
}

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
