//! `NSTextElement` and `NSTextParagraph`: the pieces of a text a content
//! manager hands its layout managers, each over a range of the document.
//!
//! An element knows its content manager (weakly) and its range. A content
//! storage's elements follow its text as it is edited: their ranges move
//! with edits before them (see `content`), so an element the storage made
//! stays the same object, with the same layout fragment, while its
//! paragraph is untouched, as on macOS. A paragraph's text is its own
//! attributed string when it was made with one (a delegate's substitute),
//! else the storage's text over its range, read when first asked for.
//!
//! Measured on macOS (`conformance/tests/textkit2.rs`): a paragraph's range
//! takes in its separator; `paragraphContentRange` leaves the separator out
//! and `paragraphSeparatorRange` is the separator alone (empty at the end
//! of a paragraph without one), both nil for a paragraph with no range; the
//! separator's length comes from the paragraph's own text (a substitute's
//! trailing "\n" makes the last unit of its range the separator).

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_app_kit::{NSTextContentManager, NSTextElement, NSTextParagraph, NSTextRange};
use objc2_foundation::{NSArray, NSAttributedString, NSString};

use super::SharedWeak;
use super::location::{self, span_of};

sidestep_runtime::static_class!(pub(crate) NSTEXTELEMENT, NSTEXTELEMENT_META = "NSTextElement", || {
    let _ = NSTextElementImpl::class();
});

sidestep_runtime::static_class!(pub(crate) NSTEXTPARAGRAPH, NSTEXTPARAGRAPH_META = "NSTextParagraph", || {
    let _ = NSTextParagraphImpl::class();
});

/// Where a content storage's element is: its range as of an edit
/// generation of the storage (`stamp`), which the storage brings up to
/// date as it is asked, the separator's length, and whether its text is
/// the storage's own (not a delegate's substitute).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Live {
    pub start: usize,
    pub len: usize,
    pub stamp: u64,
    pub sep: u8,
    pub default: bool,
}

pub(crate) struct ElementIvars {
    /// Its content manager: a content storage's elements share the
    /// storage's weak reference to itself.
    manager: RefCell<Option<SharedWeak>>,
    /// The range set, or the last one made for a live element.
    range: RefCell<Option<Retained<NSTextRange>>>,
    pub(crate) live: Cell<Option<Live>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextElement"]
    #[ivars = ElementIvars]
    pub(crate) struct NSTextElementImpl;

    impl NSTextElementImpl {
        #[unsafe(method_id(initWithTextContentManager:))]
        fn init_with_text_content_manager(this: Allocated<Self>, manager: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(ElementIvars {
                manager: RefCell::new(manager.map(SharedWeak::new)),
                range: RefCell::new(None),
                live: Cell::new(None),
            });
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithTextContentManager: None::<&AnyObject>] }
        }

        #[unsafe(method_id(textContentManager))]
        fn text_content_manager(&self) -> Option<Retained<AnyObject>> {
            self.ivars().manager()
        }

        #[unsafe(method(setTextContentManager:))]
        fn set_text_content_manager(&self, manager: Option<&AnyObject>) {
            let same = self.ivars().manager().zip(manager).is_some_and(|(a, b)| std::ptr::eq(&*a, b));
            if !same {
                *self.ivars().manager.borrow_mut() = manager.map(SharedWeak::new);
            }
        }

        #[unsafe(method_id(elementRange))]
        fn element_range(&self) -> Option<Retained<NSTextRange>> {
            current_range(self)
        }

        #[unsafe(method(setElementRange:))]
        fn set_element_range(&self, range: Option<&NSTextRange>) {
            let iv = self.ivars();
            iv.live.set(None);
            *iv.range.borrow_mut() = range.map(objc2::Message::retain);
        }

        #[unsafe(method_id(childElements))]
        fn child_elements(&self) -> Retained<NSArray<NSTextElement>> {
            NSArray::new()
        }

        #[unsafe(method_id(parentElement))]
        fn parent_element(&self) -> Option<Retained<NSTextElement>> {
            None
        }

        #[unsafe(method(isRepresentedElement))]
        fn is_represented_element(&self) -> bool {
            true
        }
    }

    unsafe impl NSObjectProtocol for NSTextElementImpl {}
);

/// An element's ivars, if it is one of Sidestep's (or a subclass's).
pub(crate) fn ivars(e: &AnyObject) -> Option<&ElementIvars> {
    let ours = <NSTextElement as ClassType>::class();
    // SAFETY: an instance of the class or a subclass.
    crate::textkit::is_kind(e.class(), ours)
        .then(|| unsafe { &*(e as *const AnyObject).cast::<NSTextElementImpl>() }.ivars())
}

impl ElementIvars {
    fn manager(&self) -> Option<Retained<AnyObject>> {
        self.manager.borrow().as_ref().and_then(SharedWeak::load)
    }
}

/// The element's range now: a live one brought up to date by its storage.
fn current_range(e: &NSTextElementImpl) -> Option<Retained<NSTextRange>> {
    let iv = e.ivars();
    if iv.live.get().is_some() {
        let manager = iv.manager();
        if let Some(m) = manager {
            super::content::refresh(&m, iv);
        }
        let live = iv.live.get()?;
        let span = (live.start, live.start + live.len);
        let cached = iv.range.borrow().clone();
        if let Some(r) = cached
            && span_of(&r) == Some(span)
        {
            return Some(r);
        }
        let r = location::range(span.0, span.1);
        *iv.range.borrow_mut() = Some(r.clone());
        return Some(r);
    }
    iv.range.borrow().clone()
}

/// The element's range in countable offsets, without making a range object
/// where it needn't; `None` for no range, or locations of another kind.
pub(crate) fn span(e: &AnyObject) -> Option<(usize, usize)> {
    if let Some(iv) = ivars(e) {
        if iv.live.get().is_some() {
            if let Some(m) = iv.manager() {
                super::content::refresh(&m, iv);
            }
            return iv.live.get().map(|l| (l.start, l.start + l.len));
        }
        if let Some(r) = iv.range.borrow().as_ref() {
            return span_of(r);
        }
    }
    // SAFETY: elementRange takes nothing and returns a range or nil.
    let r: Option<Retained<NSTextRange>> = unsafe { msg_send![e, elementRange] };
    r.and_then(|r| span_of(&r))
}

/// Make `e` an element of the content storage `manager` refers to, over
/// `start..start + len`, as of edit generation `stamp`.
pub(crate) fn set_live(e: &AnyObject, manager: &SharedWeak, live: Live) {
    if let Some(iv) = ivars(e) {
        let mut m = iv.manager.borrow_mut();
        if !m.as_ref().is_some_and(|m| m.same(manager)) {
            *m = Some(manager.clone());
        }
        drop(m);
        iv.live.set(Some(live));
    } else if let Some(manager) = manager.load() {
        // Another kind of element: told by message.
        let r = location::range(live.start, live.start + live.len);
        // SAFETY: the element's own setters.
        unsafe {
            let _: () = msg_send![e, setTextContentManager: &*manager];
            let _: () = msg_send![e, setElementRange: &*r];
        }
    }
}

/// An element's text: its own attributed string (a paragraph's), else the
/// one its content manager (`content`, or the element's own) has for it.
pub(crate) fn text_of(e: &AnyObject, content: Option<&AnyObject>) -> Option<Retained<NSAttributedString>> {
    if crate::textkit::responds(e, objc2::sel!(attributedString)) {
        // SAFETY: attributedString takes nothing.
        let t: Option<Retained<NSAttributedString>> = unsafe { msg_send![e, attributedString] };
        return t;
    }
    let own: Option<Retained<AnyObject>> = match content {
        Some(_) => None,
        // SAFETY: textContentManager takes nothing.
        None => unsafe { msg_send![e, textContentManager] },
    };
    let m = content.or(own.as_deref())?;
    if !crate::textkit::responds(m, objc2::sel!(attributedStringForTextElement:)) {
        return None;
    }
    // SAFETY: the content manager's method takes an element.
    unsafe { msg_send![m, attributedStringForTextElement: e] }
}

pub(crate) struct ParagraphIvars {
    /// The text, given or read from the storage when first asked for.
    text: RefCell<Option<Retained<NSAttributedString>>>,
}

define_class!(
    #[unsafe(super(NSTextElement, NSObject))]
    #[name = "NSTextParagraph"]
    #[ivars = ParagraphIvars]
    pub(crate) struct NSTextParagraphImpl;

    impl NSTextParagraphImpl {
        #[unsafe(method_id(initWithAttributedString:))]
        fn init_with_attributed_string(this: Allocated<Self>, text: Option<&NSAttributedString>) -> Retained<Self> {
            let text = text.map(|t| {
                // SAFETY: -copy of an attributed string is an attributed
                // string.
                let copy: Retained<NSAttributedString> = unsafe { msg_send![t, copy] };
                copy
            });
            let this = this.set_ivars(ParagraphIvars { text: RefCell::new(text) });
            // SAFETY: NSTextElement's designated initializer.
            unsafe { msg_send![super(this), initWithTextContentManager: None::<&AnyObject>] }
        }

        #[unsafe(method_id(initWithTextContentManager:))]
        fn init_with_text_content_manager(this: Allocated<Self>, manager: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(ParagraphIvars { text: RefCell::new(None) });
            // SAFETY: NSTextElement's designated initializer.
            unsafe { msg_send![super(this), initWithTextContentManager: manager] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the initializer above.
            unsafe { msg_send![this, initWithTextContentManager: None::<&AnyObject>] }
        }

        #[unsafe(method_id(attributedString))]
        fn attributed_string(&self) -> Option<Retained<NSAttributedString>> {
            self.text()
        }

        #[unsafe(method_id(paragraphContentRange))]
        fn paragraph_content_range(&self) -> Option<Retained<NSTextRange>> {
            self.parts().map(|(a, b, sep)| location::range(a, b - sep))
        }

        #[unsafe(method_id(paragraphSeparatorRange))]
        fn paragraph_separator_range(&self) -> Option<Retained<NSTextRange>> {
            self.parts().map(|(_, b, sep)| location::range(b - sep, b))
        }
    }

    unsafe impl NSObjectProtocol for NSTextParagraphImpl {}
);

impl NSTextParagraphImpl {
    fn as_object(&self) -> &AnyObject {
        // SAFETY: an object.
        unsafe { &*(self as *const Self).cast::<AnyObject>() }
    }

    fn as_element_ivars(&self) -> &ElementIvars {
        ivars(self.as_object()).expect("a paragraph is an element")
    }

    /// The paragraph's text: given, or a storage's read now.
    fn text(&self) -> Option<Retained<NSAttributedString>> {
        let known = self.ivars().text.borrow().clone();
        if known.is_some() {
            return known;
        }
        let element = self.as_element_ivars();
        let live = element.live.get()?;
        let manager = element.manager()?;
        let text = super::content::text_of(&manager, element)?;
        if live.default {
            *self.ivars().text.borrow_mut() = Some(text.clone());
        }
        Some(text)
    }

    /// Its range's ends and its separator's length.
    fn parts(&self) -> Option<(usize, usize, usize)> {
        let (a, b) = span(self.as_object())?;
        Some((a, b, self.separator().min(b - a)))
    }

    /// UTF-16 units of separator ending the paragraph's text.
    fn separator(&self) -> usize {
        if let Some(live) = self.as_element_ivars().live.get()
            && live.default
        {
            return usize::from(live.sep);
        }
        // SAFETY: attributedString takes nothing (a subclass may override
        // it) and returns an attributed string or nil.
        let text: Option<Retained<NSAttributedString>> = unsafe { msg_send![self, attributedString] };
        text.map_or(0, |t| separator_len(&t.string()))
    }
}

/// UTF-16 units of paragraph separator ending `s`.
pub(crate) fn separator_len(s: &NSString) -> usize {
    let n = s.length();
    if n == 0 {
        return 0;
    }
    usize::from(separator_units(s.characterAtIndex(n - 1), (n >= 2).then(|| s.characterAtIndex(n - 2))))
}

/// UTF-16 units of paragraph separator ending in `last` (after `before`):
/// 2 for "\r\n", 1 for "\n", "\r" or U+2029, else 0.
pub(crate) fn separator_units(last: u16, before: Option<u16>) -> u8 {
    match last {
        0x0A if before == Some(0x0D) => 2,
        0x0A | 0x0D | 0x2029 => 1,
        _ => 0,
    }
}

/// A paragraph over `text`, made by Sidestep.
pub(crate) fn paragraph_with(text: Option<&NSAttributedString>) -> Retained<NSTextParagraph> {
    crate::load_shell::<NSTextParagraph>();
    // SAFETY: the designated initializer.
    unsafe { msg_send![NSTextParagraph::alloc(), initWithAttributedString: text] }
}

/// An empty paragraph for the storage's text (read when asked for).
pub(crate) fn storage_paragraph() -> Retained<NSTextParagraph> {
    crate::load_shell::<NSTextParagraph>();
    // SAFETY: the initializer for a content manager's paragraph.
    unsafe { msg_send![NSTextParagraph::alloc(), initWithTextContentManager: None::<&NSTextContentManager>] }
}

/// The attributed string a paragraph was given, without reading a
/// storage's (for layout, which reads the storage itself).
pub(crate) fn given_text(p: &AnyObject) -> Option<Retained<NSAttributedString>> {
    let ours = <NSTextParagraph as ClassType>::class();
    if !crate::textkit::is_kind(p.class(), ours) {
        return None;
    }
    // SAFETY: an instance of the class or a subclass.
    let p = unsafe { &*(p as *const AnyObject).cast::<NSTextParagraphImpl>() };
    p.ivars().text.borrow().clone()
}
