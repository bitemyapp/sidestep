//! `NSResponder`: the chain events and actions travel up.
//!
//! A responder keeps an unretained link to the next one (a view's superview
//! or its window, a content view's window), which the owner sets and
//! clears. Event methods a responder doesn't override pass the event to the
//! next responder; actions go to the first responder that has them.
//!
//! A view with a view controller has the controller next, and the
//! controller whatever the view would have had (see `controllers`): the
//! view's `controller` link names the controller, and setting or clearing
//! the view's next responder sets or clears the controller's instead. Each
//! link is cleared before what it points at goes (a superview going away
//! clears its subviews', a window its content view's, a controller letting
//! go of its view the view's), so none dangles.

use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, MessageReceiver, NSObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSResponder, NSViewController};

/// A reference that doesn't retain, as AppKit's back pointers are.
type Unretained<T> = std::cell::Cell<Option<NonNull<T>>>;

#[derive(Default)]
pub(crate) struct ResponderIvars {
    next: Unretained<NSResponder>,
    /// The view controller standing between a view and its next responder,
    /// which holds the view (so outlives the link) and clears it when it
    /// lets go.
    controller: Unretained<NSViewController>,
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
            match self.ivars().controller.get() {
                Some(controller) if next.is_none_or(|n| !std::ptr::eq(n, controller.as_ptr().cast())) => {
                    // SAFETY: the controller holds this view, so it is alive
                    // while the link names it; the caller's responder
                    // outlives the link as it would the view's own.
                    unsafe { controller.as_ref().setNextResponder(next) };
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

fn ivars_of(responder: &NSResponder) -> &ResponderIvars {
    // SAFETY: every responder is an NSResponderImpl, whose ivars hold the
    // links.
    unsafe { &*(responder as *const NSResponder).cast::<NSResponderImpl>() }.ivars()
}

/// Clear `responder`'s link to `next`, if that is where it points: a view
/// leaving its window or superview (or either going away) keeps no link to
/// it. Only reads the links, so `next` may be going away already.
pub(crate) fn unlink_next(responder: &NSResponder, next: NonNull<NSResponder>) {
    let ivars = ivars_of(responder);
    // A view's controller holds the link in its place.
    if let Some(controller) = ivars.controller.get() {
        // SAFETY: the controller holds the view, so it is alive while the
        // link names it.
        unlink_next(unsafe { controller.as_ref() }, next);
    }
    if ivars.next.get() == Some(next) {
        ivars.next.set(None);
    }
}

/// `responder`'s next responder, without retaining it (it may be going
/// away).
pub(crate) fn next_of(responder: &NSResponder) -> Option<NonNull<NSResponder>> {
    ivars_of(responder).next.get()
}

/// Set `responder`'s next responder as it is, with no view controller
/// coming between.
///
/// # Safety
/// `next` must outlive the link, or clear it before it goes, as every
/// next responder must.
pub(crate) unsafe fn link_next(responder: &NSResponder, next: Option<NonNull<NSResponder>>) {
    ivars_of(responder).next.set(next);
}

/// The view controller standing between `view` and its next responder.
pub(crate) fn controller_of(view: &NSResponder) -> Option<NonNull<NSViewController>> {
    ivars_of(view).controller.get()
}

/// Have `controller` stand between `view` and its next responder, or no
/// controller.
///
/// # Safety
/// `controller` must hold `view` for as long as the link names it, and
/// clear the link when it lets go.
pub(crate) unsafe fn set_controller(view: &NSResponder, controller: Option<NonNull<NSViewController>>) {
    ivars_of(view).controller.set(controller);
}
