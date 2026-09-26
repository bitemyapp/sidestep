//! `NSCursor`: the pointer's appearance over the program's windows.
//!
//! The standard cursors are shapes the compositor draws (wp_cursor_shape_v1)
//! or images from the cursor theme; cursors made from images aren't
//! supported yet. Setting a cursor shows it over the content of every
//! window of the program, as a cursor set on macOS stays until another is
//! set; over the decorations Sidestep draws, the resize cursors win.
//! `push` and `pop` keep a stack, and hiding hides the pointer over the
//! content, until it moves if asked.
//!
//! Cursors belong to the main thread in practice; the current cursor and
//! the stack are the main thread's, and setting one elsewhere has no
//! effect on screen.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, Message, define_class, msg_send};
use objc2_app_kit::NSCursor;

use crate::protocol::{Cursor, ToRender};

pub(crate) struct CursorIvars {
    shape: Cursor,
}

thread_local! {
    /// The cursor set last, if any has been, and the pushed ones below it.
    static CURRENT: RefCell<Option<Retained<NSCursor>>> = const { RefCell::new(None) };
    static STACK: RefCell<Vec<Retained<NSCursor>>> = const { RefCell::new(Vec::new()) };
    /// Balances `hide` and `unhide`.
    static HIDDEN: Cell<isize> = const { Cell::new(0) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSCursor"]
    #[ivars = CursorIvars]
    pub(crate) struct NSCursorImpl;

    impl NSCursorImpl {
        #[unsafe(method_id(arrowCursor))]
        fn arrow_cursor() -> Retained<NSCursor> {
            standard(Cursor::Default)
        }

        #[unsafe(method_id(IBeamCursor))]
        fn i_beam_cursor() -> Retained<NSCursor> {
            standard(Cursor::Text)
        }

        #[unsafe(method_id(IBeamCursorForVerticalLayout))]
        fn i_beam_cursor_for_vertical_layout() -> Retained<NSCursor> {
            standard(Cursor::VerticalText)
        }

        #[unsafe(method_id(crosshairCursor))]
        fn crosshair_cursor() -> Retained<NSCursor> {
            standard(Cursor::Crosshair)
        }

        #[unsafe(method_id(pointingHandCursor))]
        fn pointing_hand_cursor() -> Retained<NSCursor> {
            standard(Cursor::Pointer)
        }

        #[unsafe(method_id(openHandCursor))]
        fn open_hand_cursor() -> Retained<NSCursor> {
            standard(Cursor::Grab)
        }

        #[unsafe(method_id(closedHandCursor))]
        fn closed_hand_cursor() -> Retained<NSCursor> {
            standard(Cursor::Grabbing)
        }

        #[unsafe(method_id(operationNotAllowedCursor))]
        fn operation_not_allowed_cursor() -> Retained<NSCursor> {
            standard(Cursor::NotAllowed)
        }

        #[unsafe(method_id(dragLinkCursor))]
        fn drag_link_cursor() -> Retained<NSCursor> {
            standard(Cursor::Alias)
        }

        #[unsafe(method_id(dragCopyCursor))]
        fn drag_copy_cursor() -> Retained<NSCursor> {
            standard(Cursor::Copy)
        }

        #[unsafe(method_id(contextualMenuCursor))]
        fn contextual_menu_cursor() -> Retained<NSCursor> {
            standard(Cursor::ContextMenu)
        }

        #[unsafe(method_id(disappearingItemCursor))]
        fn disappearing_item_cursor() -> Retained<NSCursor> {
            standard(Cursor::Default)
        }

        #[unsafe(method_id(resizeLeftCursor))]
        fn resize_left_cursor() -> Retained<NSCursor> {
            standard(Cursor::WResize)
        }

        #[unsafe(method_id(resizeRightCursor))]
        fn resize_right_cursor() -> Retained<NSCursor> {
            standard(Cursor::EResize)
        }

        #[unsafe(method_id(resizeLeftRightCursor))]
        fn resize_left_right_cursor() -> Retained<NSCursor> {
            standard(Cursor::EwResize)
        }

        #[unsafe(method_id(resizeUpCursor))]
        fn resize_up_cursor() -> Retained<NSCursor> {
            standard(Cursor::NResize)
        }

        #[unsafe(method_id(resizeDownCursor))]
        fn resize_down_cursor() -> Retained<NSCursor> {
            standard(Cursor::SResize)
        }

        #[unsafe(method_id(resizeUpDownCursor))]
        fn resize_up_down_cursor() -> Retained<NSCursor> {
            standard(Cursor::NsResize)
        }

        #[unsafe(method_id(currentCursor))]
        fn current_cursor() -> Retained<NSCursor> {
            CURRENT.with(|c| c.borrow().clone()).unwrap_or_else(|| standard(Cursor::Default))
        }

        #[unsafe(method(set))]
        fn set(&self) {
            show(as_cursor(self));
        }

        #[unsafe(method(push))]
        fn push(&self) {
            let current = CURRENT.with(|c| c.borrow().clone()).unwrap_or_else(|| standard(Cursor::Default));
            STACK.with(|s| s.borrow_mut().push(current));
            show(as_cursor(self));
        }

        #[unsafe(method(pop))]
        fn pop(&self) {
            pop();
        }

        #[unsafe(method(pop))]
        fn pop_class() {
            pop();
        }

        #[unsafe(method(hide))]
        fn hide() {
            let n = HIDDEN.with(|h| h.replace(h.get() + 1)) + 1;
            if n == 1 {
                hidden(true, false);
            }
        }

        #[unsafe(method(unhide))]
        fn unhide() {
            let n = HIDDEN.with(|h| h.replace(h.get() - 1)) - 1;
            if n == 0 {
                hidden(false, false);
            }
        }

        #[unsafe(method(setHiddenUntilMouseMoves:))]
        fn set_hidden_until_mouse_moves(flag: bool) {
            if HIDDEN.with(Cell::get) <= 0 {
                hidden(flag, flag);
            }
        }
    }

    unsafe impl NSObjectProtocol for NSCursorImpl {}
);

fn as_cursor(cursor: &NSCursorImpl) -> &NSCursor {
    // SAFETY: NSCursorImpl is the class NSCursor names.
    unsafe { &*(cursor as *const NSCursorImpl).cast::<NSCursor>() }
}

fn shape_of(cursor: &NSCursor) -> Cursor {
    // SAFETY: every NSCursor is an NSCursorImpl.
    unsafe { &*(cursor as *const NSCursor).cast::<NSCursorImpl>() }.ivars().shape
}

/// The standard cursor with this shape: one object each, as AppKit has.
fn standard(shape: Cursor) -> Retained<NSCursor> {
    // Pointers to cursors that are never freed: the registry keeps a
    // reference to each.
    static REGISTRY: OnceLock<Mutex<HashMap<Cursor, usize>>> = OnceLock::new();
    let mut registry = REGISTRY.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let ptr = *registry.entry(shape).or_insert_with(|| {
        crate::load_shell::<objc2_app_kit::NSCursor>();
        let this = NSCursorImpl::alloc().set_ivars(CursorIvars { shape });
        // SAFETY: NSObject's designated initializer.
        let cursor: Retained<NSCursorImpl> = unsafe { msg_send![super(this), init] };
        Retained::into_raw(cursor) as usize
    });
    // SAFETY: the registry holds a reference to every cursor in it, so this
    // one is alive, and NSCursorImpl is the class NSCursor names.
    unsafe { Retained::retain(ptr as *mut NSCursor) }.expect("registered cursors aren't null")
}

/// Make `cursor` current and show it over every window's content.
fn show(cursor: &NSCursor) {
    let shape = shape_of(cursor);
    let previous = CURRENT.with(|c| c.replace(Some(cursor.retain())));
    drop(previous);
    if MainThreadMarker::new().is_some() {
        crate::app::set_cursor_everywhere(shape);
    }
}

fn pop() {
    if let Some(below) = STACK.with(|s| s.borrow_mut().pop()) {
        show(&below);
    }
}

fn hidden(hidden: bool, until_moved: bool) {
    if MainThreadMarker::new().is_some() {
        crate::app::send_if_running(ToRender::HideCursor { hidden, until_moved });
    }
}
