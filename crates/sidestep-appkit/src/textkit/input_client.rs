//! `NSTextInputClient` on `NSTextView`: what the input context
//! (`inputcontext`) sends a text view for typed text, text an input method
//! composes (marked text), commands, and the questions it asks about
//! where text is on screen.
//!
//! Text being composed lives in the text storage, in the typing
//! attributes, drawn underlined; the view remembers its range. Each update
//! from the input method (`setMarkedText:…`, `insertText:replacementRange:`)
//! is one transaction. Composing doesn't register undo on its way: the
//! text it replaced is kept aside, and committing registers one action
//! that puts that back, so undoing a composition undoes it whole.
//!
//! `doCommandBySelector:` offers the command to the delegate's
//! `textView:doCommandBySelector:` first (a field editor's delegate, the
//! control, asks its own delegate's `control:textView:doCommandBySelector:`
//! there), then performs it, or passes it up the responder chain.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, MessageReceiver, NSObject, Sel};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSText, NSTextView};
use objc2_foundation::{NSArray, NSAttributedString, NSPoint, NSRange, NSRect, NSSize, NSString};

use super::edit::Kind;
use super::notify;
use super::text_view::{NOT_FOUND, NSTextViewImpl, offset};

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepTextViewInputClient"]
    struct InputClient;

    impl InputClient {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, text: &AnyObject, range: NSRange) {
            insert(tv(self), text, range);
        }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &AnyObject) {
            insert(tv(self), text, NSRange::new(NOT_FOUND, 0));
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            do_command(tv(self), selector);
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text(&self, text: &AnyObject, selected: NSRange, replacement: NSRange) {
            mark(tv(self), text, selected, replacement);
        }

        /// The marked text stays as it is, committed: one undo action, as
        /// an inserted composition is.
        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            unmark(tv(self));
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            tv(self).selection()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            tv(self).marked_range().unwrap_or(NSRange::new(NOT_FOUND, 0))
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            tv(self).marked_range().is_some()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring(&self, range: NSRange, actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
            substring(tv(self), range, actual)
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSString>> {
            // SAFETY: the keys are constant strings AppKit exports.
            let keys = unsafe {
                [
                    objc2_app_kit::NSUnderlineStyleAttributeName,
                    objc2_app_kit::NSUnderlineColorAttributeName,
                    objc2_app_kit::NSForegroundColorAttributeName,
                    objc2_app_kit::NSBackgroundColorAttributeName,
                    objc2_app_kit::NSMarkedClauseSegmentAttributeName,
                ]
            };
            NSArray::from_slice(&keys)
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range(&self, range: NSRange, actual: *mut NSRange) -> NSRect {
            first_rect(tv(self), range, actual)
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, p: NSPoint) -> usize {
            index_for_screen_point(tv(self), p)
        }

        #[unsafe(method_id(attributedString))]
        fn attributed_string(&self) -> Retained<NSAttributedString> {
            match tv(self).storage() {
                Some(s) => Retained::into_super(Retained::into_super(s)),
                None => NSAttributedString::new(),
            }
        }

        #[unsafe(method(fractionOfDistanceThroughGlyphForPoint:))]
        fn fraction_through_glyph(&self, _p: NSPoint) -> f64 {
            0.0
        }

        #[unsafe(method(drawsVerticallyForCharacterAtIndex:))]
        fn draws_vertically(&self, _index: usize) -> bool {
            false
        }
    }
);

sidestep_runtime::category!("NSTextView"(SidestepInputClient), |category| {
    // SAFETY: the helper's methods treat their receiver as a text view.
    unsafe { category.add_methods_of(InputClient::class()) };
});

fn tv(this: &InputClient) -> &NSTextViewImpl {
    // SAFETY: the methods are installed on NSTextView, so the receiver is
    // one.
    unsafe { &*(this as *const InputClient).cast::<NSTextViewImpl>() }
}

fn substring(v: &NSTextViewImpl, range: NSRange, actual: *mut NSRange) -> Option<Retained<NSAttributedString>> {
    let storage = v.storage()?;
    let len = storage.length();
    let loc = range.location.min(len);
    let r = NSRange::new(loc, range.length.min(len - loc));
    if !actual.is_null() {
        // SAFETY: the caller passes a valid pointer or null.
        unsafe { *actual = r };
    }
    Some(storage.attributedSubstringFromRange(r))
}

/// The string of an `NSString` or `NSAttributedString`.
fn string_of(text: &AnyObject) -> Retained<NSString> {
    if let Some(a) = text.downcast_ref::<NSAttributedString>() {
        return a.string();
    }
    match text.downcast_ref::<NSString>() {
        Some(s) => s.retain(),
        // SAFETY: anything input methods send answers description.
        None => unsafe { msg_send![text, description] },
    }
}

/// Committed text: replaces the marked text, or `range`, or the
/// selection.
fn insert(v: &NSTextViewImpl, text: &AnyObject, range: NSRange) {
    let string = string_of(text);
    let marked = v.marked_range();
    v.ivars().marked.set(None);
    let target = match marked {
        Some(m) => m,
        None if range.location != NOT_FOUND => range,
        None => v.selection(),
    };
    super::field_editor::before_insert(v);
    match (marked, composition(v)) {
        (Some(_), Some(original)) => {
            // One undo for the whole composition.
            super::edit::user_replace_quietly(v, target, &string);
            super::edit::register_composition(v, target.location, string.length(), original);
        }
        _ => {
            let before = v.selection();
            let elsewhere = marked.is_none() && range.location != NOT_FOUND && range != before;
            if v.edit_replace_ns(target, &string, Kind::Typing) && elsewhere {
                // Text put in elsewhere leaves the selection where it was,
                // moved with the text after it.
                let delta = string.length() as isize - target.length as isize;
                let end = target.location + target.length;
                let sel = if before.location >= end {
                    NSRange::new((before.location as isize + delta) as usize, before.length)
                } else if before.location + before.length > target.location {
                    NSRange::new(target.location + string.length(), 0)
                } else {
                    before
                };
                v.set_selection_internal(sel, true);
            }
        }
    }
    set_composition(v, None);
    v.as_view().setNeedsDisplay(true);
}

/// Text being composed: shown in place of the marked text (or `replacement`,
/// or the selection), with `selected` (within it) selected.
fn mark(v: &NSTextViewImpl, text: &AnyObject, selected: NSRange, replacement: NSRange) {
    let string = string_of(text);
    let marked = v.marked_range();
    let target = match marked {
        Some(m) => m,
        None if replacement.location != NOT_FOUND => replacement,
        None => v.selection(),
    };
    if marked.is_none() {
        // Composing begins: keep what it replaces, for undo.
        let original = v.storage().map(|s| s.attributedSubstringFromRange(target));
        set_composition(v, original);
    }
    if !super::edit::user_replace_quietly(v, target, &string) {
        return;
    }
    let len = string.length();
    v.ivars().marked.set((len > 0).then(|| NSRange::new(target.location, len)));
    let loc = (target.location + selected.location).min(target.location + len);
    let sel = NSRange::new(loc, selected.length.min(target.location + len - loc));
    v.set_selection_internal(sel, true);
    v.as_view().setNeedsDisplay(true);
    if len == 0 {
        set_composition(v, None);
    }
}

thread_local! {
    /// What a composition replaced, by view, until it commits.
    static COMPOSING: std::cell::RefCell<Vec<(usize, Retained<NSAttributedString>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn composition(v: &NSTextViewImpl) -> Option<Retained<NSAttributedString>> {
    let key = v as *const NSTextViewImpl as usize;
    COMPOSING.with(|c| c.borrow().iter().find(|e| e.0 == key).map(|e| e.1.clone()))
}

/// The view's composition is over without being committed.
pub(crate) fn forget_composition(v: &NSTextViewImpl) {
    set_composition(v, None);
}

fn set_composition(v: &NSTextViewImpl, original: Option<Retained<NSAttributedString>>) {
    let key = v as *const NSTextViewImpl as usize;
    COMPOSING.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|e| e.0 != key);
        if let Some(o) = original {
            c.push((key, o));
        }
    });
}

/// `unmarkText`: the marked text stays as it is, committed.
pub(crate) fn unmark(v: &NSTextViewImpl) {
    let marked = v.marked_range();
    v.ivars().marked.set(None);
    if let (Some(m), Some(original)) = (marked, composition(v)) {
        super::edit::register_composition(v, m.location, m.length, original);
    }
    set_composition(v, None);
    v.as_view().setNeedsDisplay(true);
}

/// Offer a command to the delegate, then perform it, or pass it up.
fn do_command(v: &NSTextViewImpl, selector: Sel) {
    let view: &NSTextView = v.as_text_view();
    if let Some(d) = v.delegate_object()
        && notify::responds(&d, sel!(textView:doCommandBySelector:))
    {
        // SAFETY: the delegate method takes the view and a selector and
        // returns BOOL.
        let handled: bool = unsafe { msg_send![&*d, textView: view, doCommandBySelector: selector] };
        if handled {
            return;
        }
    }
    if notify::responds(view, selector) {
        // SAFETY: commands are action methods: they take the sender (none,
        // as AppKit sends them) and return nothing.
        unsafe { MessageReceiver::send_message::<_, ()>(view, selector, (None::<&AnyObject>,)) };
        return;
    }
    // Not ours: the responder chain's.
    // SAFETY: NSResponder's doCommandBySelector:, from the text view's
    // superclass, with the view as receiver.
    unsafe {
        MessageReceiver::send_super_message::<_, ()>(view, NSText::class(), sel!(doCommandBySelector:), (selector,))
    };
}

/// The screen rect of the first line piece of `range` (the caret's for an
/// empty one), and how much of the range it covers.
fn first_rect(v: &NSTextViewImpl, range: NSRange, actual: *mut NSRange) -> NSRect {
    let Some(lm) = v.geo() else { return NSRect::ZERO };
    let len = v.text_length();
    let loc = range.location.min(len);
    let r = NSRange::new(loc, range.length.min(len - loc));
    let (rect, covered) = if r.length == 0 {
        // The insertion point's place, no width, as AppKit gives it.
        let caret = lm.caret_rect(r.location, false);
        (NSRect::new(caret.origin, NSSize::new(0.0, caret.size.height)), r)
    } else {
        // The range's first line only: nothing after it is laid out or
        // measured (this runs on each selection change).
        let (rects, first) = lm.first_line_rects(r.location..r.location + r.length);
        let rect = rects.into_iter().reduce(|a, b| {
            let x0 = a.origin.x.min(b.origin.x);
            let x1 = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
            NSRect::new(NSPoint::new(x0, a.origin.y), NSSize::new(x1 - x0, a.size.height))
        });
        (rect.unwrap_or_else(|| lm.caret_rect(r.location, false)), NSRange::new(first.start, first.len()))
    };
    if !actual.is_null() {
        // SAFETY: the caller passes a valid pointer or null.
        unsafe { *actual = covered };
    }
    let view = v.as_view();
    let in_view = offset(rect, v.origin());
    let in_window = view.convertRect_toView(in_view, None);
    match view.window() {
        Some(w) => w.convertRectToScreen(in_window),
        None => in_window,
    }
}

fn index_for_screen_point(v: &NSTextViewImpl, p: NSPoint) -> usize {
    let view = v.as_view();
    let Some(w) = view.window() else { return NOT_FOUND };
    let in_window = w.convertPointFromScreen(p);
    let in_view = view.convertPoint_fromView(in_window, None);
    let o = v.origin();
    let Some(lm) = v.geo() else { return NOT_FOUND };
    lm.insertion_index(NSPoint::new(in_view.x - o.x, in_view.y - o.y)).0
}
