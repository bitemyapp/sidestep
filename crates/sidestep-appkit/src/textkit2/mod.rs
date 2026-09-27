//! TextKit 2: `NSTextLayoutManager` and its content manager, elements,
//! layout and line fragments, the viewport controller and selections, over
//! TextKit 1's pieces (the text storage's paragraph tree, and the paragraph
//! engine of `text::lines`). See docs/text.md, "TextKit 2".
//!
//! - `location`: countable locations and `NSTextRange`.
//! - `element`: `NSTextElement` and `NSTextParagraph`.
//! - `content`: `NSTextContentManager` and `NSTextContentStorage`, the
//!   storage's observer, keeping the paragraphs' elements.
//! - `seq`: the chunked sequences the content storage keeps its elements in
//!   and the layout manager its index.
//! - `layout`: an element's text laid out as TextKit 2 stacks it.
//! - `fragment`: `NSTextLayoutFragment` and `NSTextLineFragment`.
//! - `layout_manager`: `NSTextLayoutManager`: the index of fragments and
//!   estimates, layout on demand, enumeration, geometry for text views.
//! - `viewport`: `NSTextViewportLayoutController`.
//! - `selection`: `NSTextSelection`, `NSTextSelectionNavigation`.
//! - `draw`: the one door from a `CGContext` to the drawing state.
//! - `view`: what `NSTextView` does in TextKit 2 mode.

pub(crate) mod content;
pub(crate) mod draw;
pub(crate) mod element;
pub(crate) mod fragment;
pub(crate) mod layout;
pub(crate) mod layout_manager;
pub(crate) mod location;
pub(crate) mod selection;
pub(crate) mod seq;
pub(crate) mod view;
pub(crate) mod viewport;

use std::rc::Rc;

use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;

/// A weak reference many objects share: a layout manager's fragments hold
/// one to it, a content storage's elements one to it. Holding a clone
/// costs a reference count, where a weak reference of its own would cost
/// each fragment or element a weak location (a boxed slot and the
/// runtime's record of it, about 50 bytes) and the runtime's lock to make
/// and free it: with 200 000 of each, 22 MB more at peak and twice the time
/// to free them (docs/text.md).
#[derive(Clone)]
pub(crate) struct SharedWeak(Rc<Weak<AnyObject>>);

impl SharedWeak {
    pub fn new(obj: &AnyObject) -> SharedWeak {
        SharedWeak(Rc::new(Weak::new(obj)))
    }

    pub fn load(&self) -> Option<Retained<AnyObject>> {
        self.0.load()
    }

    pub fn same(&self, other: &SharedWeak) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
