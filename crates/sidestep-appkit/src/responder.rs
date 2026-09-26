//! `NSResponder`: the chain events and actions travel up.
//!
//! A responder keeps an unretained link to the next one (a view's superview
//! or its window, a content view's window), which the owner sets and
//! clears. Event methods a responder doesn't override pass the event to the
//! next responder; actions go to the first responder that has them.

use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, MessageReceiver, NSObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSResponder};

/// A reference that doesn't retain, as AppKit's back pointers are.
type Unretained<T> = std::cell::Cell<Option<NonNull<T>>>;

#[derive(Default)]
pub(crate) struct ResponderIvars {
    next: Unretained<NSResponder>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSResponder"]
    #[ivars = ResponderIvars]
    pub(crate) struct NSResponderImpl;

    impl NSResponderImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ResponderIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(nextResponder))]
        fn next_responder(&self) -> Option<Retained<NSResponder>> {
            // SAFETY: the next responder outlives the link (it is the
            // superview or window, which own this responder).
            self.ivars().next.get().map(|p| unsafe { p.as_ref() }.retain())
        }

        #[unsafe(method(setNextResponder:))]
        fn set_next_responder(&self, next: Option<&NSResponder>) {
            // A view with a view controller has the controller next; what
            // comes after the view comes after the controller.
            match crate::controllers::controller_of(as_responder(self)) {
                Some(controller) if next.is_none_or(|n| !std::ptr::eq(n, &*controller as &NSResponder)) => {
                    // SAFETY: the controller keeps its view, which the
                    // caller's responder outlives as it would the view.
                    unsafe { controller.setNextResponder(next) };
                }
                _ => self.ivars().next.set(next.map(NonNull::from)),
            }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            forward(self, |next| next.mouseDown(event));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            forward(self, |next| next.mouseUp(event));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            forward(self, |next| next.mouseDragged(event));
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            forward(self, |next| next.mouseMoved(event));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            forward(self, |next| next.rightMouseDown(event));
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            forward(self, |next| next.rightMouseUp(event));
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            forward(self, |next| next.otherMouseDown(event));
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            forward(self, |next| next.otherMouseUp(event));
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            forward(self, |next| next.scrollWheel(event));
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            match self.ivars().next.get() {
                // SAFETY: as in `nextResponder`.
                Some(next) => unsafe { next.as_ref() }.keyDown(event),
                // The end of the chain: nobody took the key.
                // SAFETY: noResponderFor: takes a selector.
                None => unsafe { msg_send![self, noResponderFor: objc2::sel!(keyDown:)] },
            }
        }

        /// macOS beeps when a key reaches no responder; Sidestep does
        /// nothing.
        #[unsafe(method(noResponderFor:))]
        fn no_responder_for(&self, _selector: Sel) {}

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            forward(self, |next| next.keyUp(event));
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, _event: &NSEvent) -> bool {
            false
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            forward(self, |next| next.flagsChanged(event));
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            forward(self, |next| next.rightMouseDragged(event));
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            forward(self, |next| next.otherMouseDragged(event));
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            forward(self, |next| next.mouseEntered(event));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            forward(self, |next| next.mouseExited(event));
        }

        #[unsafe(method(cursorUpdate:))]
        fn cursor_update(&self, event: &NSEvent) {
            forward(self, |next| next.cursorUpdate(event));
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify_with_event(&self, event: &NSEvent) {
            forward(self, |next| next.magnifyWithEvent(event));
        }

        #[unsafe(method(rotateWithEvent:))]
        fn rotate_with_event(&self, event: &NSEvent) {
            forward(self, |next| next.rotateWithEvent(event));
        }

        #[unsafe(method(swipeWithEvent:))]
        fn swipe_with_event(&self, event: &NSEvent) {
            forward(self, |next| next.swipeWithEvent(event));
        }

        #[unsafe(method(smartMagnifyWithEvent:))]
        fn smart_magnify_with_event(&self, event: &NSEvent) {
            forward(self, |next| next.smartMagnifyWithEvent(event));
        }

        #[unsafe(method(interpretKeyEvents:))]
        fn interpret_key_events(&self, events: &AnyObject) {
            crate::keybindings::interpret_all(as_responder(self), events);
        }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &AnyObject) {
            // SAFETY: insertText: takes the text.
            forward(self, |next| unsafe { msg_send![next, insertText: text] });
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            do_command(self, selector);
        }

        /// Perform `action` here, or ask up the chain.
        #[unsafe(method(tryToPerform:with:))]
        fn try_to_perform(&self, action: Sel, object: Option<&AnyObject>) -> bool {
            crate::app::perform(self, action, object)
                // SAFETY: tryToPerform:with: takes a selector and an object.
                || self.ivars().next.get().is_some_and(|n| unsafe { n.as_ref().tryToPerform_with(action, object) })
        }
    }

    unsafe impl NSObjectProtocol for NSResponderImpl {}
);

fn as_responder(this: &NSResponderImpl) -> &NSResponder {
    // SAFETY: NSResponder is NSResponderImpl's class.
    unsafe { &*(this as *const NSResponderImpl).cast::<NSResponder>() }
}

/// Perform an editing command if this responder has it, else pass it up
/// the chain.
fn do_command(this: &NSResponderImpl, selector: Sel) {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![this, respondsToSelector: selector] };
    if responds {
        // SAFETY: editing commands are action methods: they take the sender
        // (none, as AppKit sends them) and return nothing.
        unsafe { MessageReceiver::send_message::<_, ()>(this, selector, (None::<&AnyObject>,)) }
    } else {
        // SAFETY: doCommandBySelector: takes a selector.
        forward(this, |next| unsafe { msg_send![next, doCommandBySelector: selector] });
    }
}

/// Pass an event a responder doesn't handle up the chain.
fn forward(this: &NSResponderImpl, send: impl FnOnce(&NSResponder)) {
    if let Some(next) = this.ivars().next.get() {
        // SAFETY: as in `nextResponder`.
        send(unsafe { next.as_ref() });
    }
}

/// Clear `responder`'s link to `next`, if that is where it points: a view
/// leaving its window (or the window going away) keeps no link to it.
pub(crate) fn unlink_next(responder: &NSResponder, next: NonNull<NSResponder>) {
    // A view's controller holds the link in its place.
    if let Some(controller) = crate::controllers::controller_of(responder) {
        unlink_next(&controller, next);
    }
    // SAFETY: every responder is an NSResponderImpl, whose ivars hold the
    // link.
    let responder = unsafe { &*(responder as *const NSResponder).cast::<NSResponderImpl>() };
    if responder.ivars().next.get() == Some(next) {
        responder.ivars().next.set(None);
    }
}

/// Set `responder`'s next responder as it is, with no view controller
/// coming between.
///
/// # Safety
/// `next` must outlive the link, or clear it before it goes, as every
/// next responder must.
pub(crate) unsafe fn link_next(responder: &NSResponder, next: Option<&NSResponder>) {
    // SAFETY: every responder is an NSResponderImpl, whose ivars hold the
    // link.
    let responder = unsafe { &*(responder as *const NSResponder).cast::<NSResponderImpl>() };
    responder.ivars().next.set(next.map(NonNull::from));
}
