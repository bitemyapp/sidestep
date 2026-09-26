//! Undo in the responder chain: `-[NSResponder undoManager]` asks the next
//! responder, and a window answers with its delegate's
//! `windowWillReturnUndoManager:`, or else with an undo manager of its
//! own, made when first asked for. `undo:` and `redo:` on a window send
//! `undo` and `redo` to it.
//!
//! These are link-time categories, so they need nothing from the classes'
//! own files; a window keeps its undo manager as an associated object.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{ClassType, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{NSResponder, NSWindow};
use objc2_foundation::NSUndoManager;

/// The key a window's own undo manager is associated under.
static WINDOW_UNDO_KEY: u8 = 0;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepResponderUndo"]
    struct ResponderUndo;

    impl ResponderUndo {
        #[unsafe(method_id(undoManager))]
        fn undo_manager(&self) -> Option<Retained<NSUndoManager>> {
            // SAFETY: installed on NSResponder, so the receiver is one.
            let this = unsafe { &*(self as *const Self).cast::<NSResponder>() };
            // SAFETY: nextResponder takes nothing; every responder answers
            // undoManager (this category).
            unsafe { this.nextResponder() }.and_then(|next| unsafe { msg_send![&*next, undoManager] })
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepWindowUndo"]
    struct WindowUndo;

    impl WindowUndo {
        #[unsafe(method_id(undoManager))]
        fn undo_manager(&self) -> Option<Retained<NSUndoManager>> {
            // SAFETY: installed on NSWindow, so the receiver is one.
            let window = unsafe { &*(self as *const Self).cast::<NSWindow>() };
            window_undo_manager(window)
        }

        #[unsafe(method(undo:))]
        fn undo(&self, _sender: Option<&AnyObject>) {
            // SAFETY: undoManager takes nothing and returns a manager or nil.
            let um: Option<Retained<NSUndoManager>> = unsafe { msg_send![self, undoManager] };
            if let Some(um) = um.filter(|um| um.canUndo()) {
                um.undo();
            }
        }

        #[unsafe(method(redo:))]
        fn redo(&self, _sender: Option<&AnyObject>) {
            // SAFETY: as in `undo:`.
            let um: Option<Retained<NSUndoManager>> = unsafe { msg_send![self, undoManager] };
            if let Some(um) = um.filter(|um| um.canRedo()) {
                um.redo();
            }
        }
    }
);

/// A window's undo manager: its delegate's, or its own.
fn window_undo_manager(window: &NSWindow) -> Option<Retained<NSUndoManager>> {
    let mtm = MainThreadMarker::from(window);
    if let Some(delegate) = window.delegate() {
        let sel = sel!(windowWillReturnUndoManager:);
        if responds(delegate.as_ref(), sel) {
            // SAFETY: the delegate method takes the window and returns an
            // undo manager or nil.
            return unsafe { msg_send![&*delegate, windowWillReturnUndoManager: window] };
        }
    }
    let key = (&WINDOW_UNDO_KEY as *const u8).cast::<std::ffi::c_void>();
    let obj = (window as *const NSWindow).cast::<AnyObject>();
    // SAFETY: the window is an object and the key a static address.
    let found = unsafe { objc2::ffi::objc_getAssociatedObject(obj, key) };
    if !found.is_null() {
        // SAFETY: only an undo manager is associated under this key, and
        // the window keeps it.
        return unsafe { Retained::retain(found.cast::<NSUndoManager>().cast_mut()) };
    }
    let um = NSUndoManager::new(mtm);
    // SAFETY: as above; the window retains the manager.
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            obj.cast_mut(),
            key,
            Retained::as_ptr(&um).cast_mut().cast(),
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
    }
    Some(um)
}

pub(crate) fn responds(obj: &AnyObject, sel: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector.
    unsafe { msg_send![obj, respondsToSelector: sel] }
}

sidestep_runtime::category!("NSResponder"(SidestepUndo), |category| {
    // SAFETY: the helper's method treats its receiver as a responder.
    unsafe { category.add_methods_of(ResponderUndo::class()) };
});

sidestep_runtime::category!("NSWindow"(SidestepUndo), |category| {
    // SAFETY: the helper's methods treat their receiver as a window.
    unsafe { category.add_methods_of(WindowUndo::class()) };
});
