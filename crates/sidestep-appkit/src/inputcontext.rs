//! Input methods on the main thread: `NSTextInputContext`, and what an
//! input method's text does to the first responder.
//!
//! A view that implements `NSTextInputClient` has an input context. While
//! the key window's first responder is such a view, the render thread keeps
//! the window's Wayland text input enabled and knows where the caret is (the
//! client's `firstRectForCharacterRange:actualRange:` for its selection),
//! so the input method can put its candidates beside it (see
//! `backend::textinput`). Text being composed arrives as marked text
//! (`setMarkedText:selectedRange:replacementRange:`) and text committed as
//! `insertText:replacementRange:`, which replaces the marked text. Keys
//! typed without an input method reach clients the same way, through
//! `interpretKeyEvents:`.
//!
//! Clients are recognized by their methods rather than by protocol
//! conformance, which the runtime only knows for protocols it has.

use std::cell::Cell;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSEvent, NSEventType, NSTextInputContext, NSView};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize, NSString};

use crate::app;
use crate::protocol::{Rect, ToRender};
use crate::views::{self, NSViewImpl};
use crate::window::{self, NSWindowImpl};

/// `NSNotFound`: `NSIntegerMax`.
pub(crate) const NOT_FOUND: usize = isize::MAX as usize;

pub(crate) struct ContextIvars {
    /// Not retained: the client owns its context.
    client: Option<Weak<AnyObject>>,
    accepts_glyph_info: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTextInputContext"]
    #[ivars = ContextIvars]
    pub(crate) struct NSTextInputContextImpl;

    impl NSTextInputContextImpl {
        #[unsafe(method_id(initWithClient:))]
        fn init_with_client(this: Allocated<Self>, client: &AnyObject) -> Retained<Self> {
            let this = this.set_ivars(ContextIvars { client: Some(Weak::new(client)), accepts_glyph_info: Cell::new(false) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ContextIvars { client: None, accepts_glyph_info: Cell::new(false) });
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(currentInputContext))]
        fn current_input_context() -> Option<Retained<NSTextInputContext>> {
            current()
        }

        #[unsafe(method_id(client))]
        fn client(&self) -> Option<Retained<AnyObject>> {
            self.client_object()
        }

        #[unsafe(method(acceptsGlyphInfo))]
        fn accepts_glyph_info(&self) -> bool {
            self.ivars().accepts_glyph_info.get()
        }

        #[unsafe(method(setAcceptsGlyphInfo:))]
        fn set_accepts_glyph_info(&self, flag: bool) {
            self.ivars().accepts_glyph_info.set(flag);
        }

        #[unsafe(method(activate))]
        fn activate(&self) {}

        #[unsafe(method(deactivate))]
        fn deactivate(&self) {}

        /// Keys typed without an input method: the key bindings turn them
        /// into text and commands for the client. False for keys that do
        /// nothing.
        #[unsafe(method(handleEvent:))]
        fn handle_event(&self, event: &NSEvent) -> bool {
            self.handle(event)
        }

        #[unsafe(method(discardMarkedText))]
        fn discard_marked_text(&self) {
            if let Some(window) = self.client_window() {
                app::send_if_running(ToRender::ResetTextInput { window: window::imp(&window).id() });
            }
        }

        #[unsafe(method(invalidateCharacterCoordinates))]
        fn invalidate_character_coordinates(&self) {
            self.caret_moved();
        }

        #[unsafe(method(textInputClientDidUpdateSelection))]
        fn text_input_client_did_update_selection(&self) {
            self.caret_moved();
        }

        #[unsafe(method(textInputClientDidScroll))]
        fn text_input_client_did_scroll(&self) {
            self.caret_moved();
        }

        #[unsafe(method(textInputClientWillStartScrollingOrZooming))]
        fn text_input_client_will_start_scrolling_or_zooming(&self) {}

        #[unsafe(method(textInputClientDidEndScrollingOrZooming))]
        fn text_input_client_did_end_scrolling_or_zooming(&self) {
            self.caret_moved();
        }
    }

    unsafe impl NSObjectProtocol for NSTextInputContextImpl {}
);

fn current() -> Option<Retained<NSTextInputContext>> {
    let window = app::key_window()?;
    let responder = window.firstResponder()?;
    let view = responder.downcast::<NSView>().ok()?;
    for_view(views::imp(&view))
}

impl NSTextInputContextImpl {
    fn handle(&self, event: &NSEvent) -> bool {
        let Some(client) = self.client_object() else { return false };
        if event.r#type() != NSEventType::KeyDown {
            return false;
        }
        crate::keybindings::interpret_for(&client, event, crate::keybindings::Via::InputContext)
    }

    fn client_object(&self) -> Option<Retained<AnyObject>> {
        self.ivars().client.as_ref().and_then(Weak::load)
    }

    /// The window of the client, if the client is a view in one.
    fn client_window(&self) -> Option<Retained<objc2_app_kit::NSWindow>> {
        let client = self.client_object()?;
        let view = client.downcast::<NSView>().ok()?;
        view.window()
    }

    fn caret_moved(&self) {
        if let Some(window) = self.client_window() {
            update(window::imp(&window));
        }
    }
}

/// Whether `object` takes text from input methods.
pub(crate) fn is_client(object: &AnyObject) -> bool {
    let responds = |selector| -> bool {
        // SAFETY: respondsToSelector: takes a selector and returns BOOL.
        unsafe { msg_send![object, respondsToSelector: selector] }
    };
    responds(sel!(setMarkedText:selectedRange:replacementRange:))
        && responds(sel!(insertText:replacementRange:))
        && responds(sel!(hasMarkedText))
}

/// A view's input context: one for each client view, made when first asked
/// for.
pub(crate) fn for_view(view: &NSViewImpl) -> Option<Retained<NSTextInputContext>> {
    let object: &AnyObject = views::as_view(view);
    if !is_client(object) {
        return None;
    }
    let slot = views::input_context(view);
    if let Some(context) = slot.borrow().as_ref() {
        return Some(context.clone());
    }
    let mtm = MainThreadMarker::from(view);
    crate::load_shell::<objc2_app_kit::NSTextInputContext>();
    let this = NSTextInputContextImpl::alloc(mtm)
        .set_ivars(ContextIvars { client: Some(Weak::new(object)), accepts_glyph_info: Cell::new(false) });
    // SAFETY: NSObject's designated initializer.
    let context: Retained<NSTextInputContextImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSTextInputContextImpl is the class NSTextInputContext names.
    let context: Retained<NSTextInputContext> = unsafe { Retained::cast_unchecked(context) };
    slot.replace(Some(context.clone()));
    Some(context)
}

/// Tell the render thread whether the window's first responder takes text
/// from input methods, and where its caret is, if either changed.
pub(crate) fn update(window: &NSWindowImpl) {
    let client = if window.is_key() { window.first_responder_object().filter(|r| is_client(r)) } else { None };
    let state = match &client {
        Some(client) => (true, caret(window, client)),
        None => (false, None),
    };
    if window.text_input_state().replace(state) != state {
        app::send_if_running(ToRender::TextInput { window: window.id(), wanted: state.0, caret: state.1 });
    }
}

/// Where the client's caret (or selection) is, in points from the top left
/// of the window's content.
fn caret(window: &NSWindowImpl, client: &AnyObject) -> Option<Rect> {
    // SAFETY: NSTextInputClient's selectedRange returns an NSRange.
    let selected: NSRange = unsafe { msg_send![client, selectedRange] };
    let mut actual = NSRange::new(NOT_FOUND, 0);
    // SAFETY: firstRectForCharacterRange:actualRange: takes a range and a
    // pointer to a range it may fill, and returns a rectangle on screen.
    let on_screen: NSRect =
        unsafe { msg_send![client, firstRectForCharacterRange: selected, actualRange: &mut actual as *mut NSRange] };
    if on_screen.size == NSSize::ZERO && on_screen.origin == NSPoint::ZERO {
        return None;
    }
    let origin = window.content_origin();
    let height = window.content_height();
    let x = on_screen.origin.x - origin.x;
    let top = height - (on_screen.origin.y - origin.y + on_screen.size.height);
    Some(Rect::new(x as f32, top as f32, (x + on_screen.size.width) as f32, (top + on_screen.size.height) as f32))
}

/// Apply what an input method sent: insert committed text, then show the
/// text being composed as marked text (or clear it).
pub(crate) fn apply(window: &NSWindowImpl, commit: Option<String>, (text, begin, end): (String, i32, i32)) {
    let Some(client) = window.first_responder_object().filter(|r| is_client(r)) else { return };
    let none = NSRange::new(NOT_FOUND, 0);
    if let Some(commit) = commit.filter(|c| !c.is_empty()) {
        let string = NSString::from_str(&commit);
        // SAFETY: insertText:replacementRange: takes a string and a range.
        let _: () = unsafe { msg_send![&*client, insertText: &*string, replacementRange: none] };
    }
    if !text.is_empty() {
        let (start, stop) = if begin < 0 || end < 0 {
            let len = utf16_len(&text, text.len());
            (len, len)
        } else {
            (utf16_len(&text, begin as usize), utf16_len(&text, end as usize))
        };
        let (start, stop) = (start.min(stop), start.max(stop));
        let string = NSString::from_str(&text);
        // SAFETY: setMarkedText:selectedRange:replacementRange: takes a
        // string and two ranges.
        let _: () = unsafe {
            msg_send![&*client, setMarkedText: &*string, selectedRange: NSRange::new(start, stop - start), replacementRange: none]
        };
    } else {
        mark(&client, "");
    }
    update(window);
}

/// Show `text` as the client's marked text with the caret after it, or
/// clear the marked text for an empty one.
pub(crate) fn mark(client: &AnyObject, text: &str) {
    let none = NSRange::new(NOT_FOUND, 0);
    if text.is_empty() {
        // SAFETY: hasMarkedText returns BOOL.
        let marked: bool = unsafe { msg_send![client, hasMarkedText] };
        if !marked {
            return;
        }
    }
    let string = NSString::from_str(text);
    let caret = NSRange::new(utf16_len(text, text.len()), 0);
    // SAFETY: setMarkedText:selectedRange:replacementRange: takes a string
    // and two ranges.
    let _: () = unsafe { msg_send![client, setMarkedText: &*string, selectedRange: caret, replacementRange: none] };
}

/// UTF-16 units in the first `bytes` bytes of `s` (cut back to a character
/// boundary).
fn utf16_len(s: &str, bytes: usize) -> usize {
    let mut end = bytes.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::utf16_len;

    #[test]
    fn byte_offsets_become_utf16_offsets() {
        assert_eq!(utf16_len("abc", 2), 2);
        // "é" is two bytes and one unit; "𝄞" four bytes and two units.
        assert_eq!(utf16_len("é𝄞x", 2), 1);
        assert_eq!(utf16_len("é𝄞x", 6), 3);
        assert_eq!(utf16_len("é𝄞x", 7), 4);
        // Inside a character: cut back.
        assert_eq!(utf16_len("é𝄞x", 3), 1);
        assert_eq!(utf16_len("é", 99), 1);
    }
}
