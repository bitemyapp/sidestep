//! `NSTextViewportLayoutController`: laying out what a view shows.
//!
//! `layoutViewport` goes as measured on macOS (`conformance/tests/textkit2.rs`):
//! the delegate's `textViewportLayoutControllerWillLayout:`, then its
//! `viewportBoundsForTextViewportLayoutController:` (the bounds kept from
//! before, without a delegate), then the fragments whose frames meet the
//! bounds are laid out, top to bottom, each handed to
//! `textViewportLayoutController:configureRenderingSurfaceForTextLayoutFragment:`,
//! and last `textViewportLayoutControllerDidLayout:`. The viewport range is
//! the fragments' ranges together. Only those fragments are laid out: the
//! rest of the document keeps its estimates, so a long document costs what
//! shows of it. Bounds of no height lay nothing out (no range); an empty
//! document's viewport holds its extra line fragment (an empty range).
//! `adjustViewportByVerticalOffset:` moves the bounds without laying
//! anything out; `relocateViewportToTextLocation:` moves them to where the
//! fragment holding the location is estimated to be and makes the range
//! the empty range there, laying nothing out either. The fragments a
//! layout configured are kept (the text view draws them, whether or not
//! a subclass of it calls `super` in the delegate methods).

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSTextLayoutFragment, NSTextLayoutFragmentEnumerationOptions, NSTextLayoutManager, NSTextRange,
    NSTextViewportLayoutController,
};
use objc2_foundation::NSRect;

use super::location::{self, offset_of};

sidestep_runtime::static_class!(pub(crate) NSTEXTVIEWPORTLAYOUTCONTROLLER, NSTEXTVIEWPORTLAYOUTCONTROLLER_META = "NSTextViewportLayoutController", || {
    let _ = NSTextViewportLayoutControllerImpl::class();
});

pub(crate) struct Ivars {
    manager: RefCell<Weak<AnyObject>>,
    delegate: RefCell<Weak<AnyObject>>,
    bounds: Cell<NSRect>,
    range: RefCell<Option<Retained<NSTextRange>>>,
    /// The fragments the last layout configured, top to bottom.
    configured: RefCell<Vec<Retained<NSTextLayoutFragment>>>,
    busy: Cell<bool>,
}

impl Ivars {
    fn new(manager: Option<&AnyObject>) -> Ivars {
        Ivars {
            manager: RefCell::new(manager.map_or_else(Weak::default, Weak::new)),
            delegate: RefCell::new(Weak::default()),
            bounds: Cell::new(NSRect::ZERO),
            range: RefCell::new(None),
            configured: RefCell::new(Vec::new()),
            busy: Cell::new(false),
        }
    }
}

/// A layout in progress: another `layoutViewport` from a delegate method
/// does nothing until it ends, and it ends when dropped (a delegate that
/// unwinds ends it too).
struct Busy<'a>(&'a Cell<bool>);

impl<'a> Busy<'a> {
    fn begin(flag: &'a Cell<bool>) -> Option<Busy<'a>> {
        (!flag.replace(true)).then_some(Busy(flag))
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTextViewportLayoutController"]
    #[ivars = Ivars]
    pub(crate) struct NSTextViewportLayoutControllerImpl;

    impl NSTextViewportLayoutControllerImpl {
        #[unsafe(method_id(initWithTextLayoutManager:))]
        fn init_with_text_layout_manager(this: Allocated<Self>, manager: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(Ivars::new(manager));
            // SAFETY: NSObject's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithTextLayoutManager: None::<&AnyObject>] }
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            *self.ivars().delegate.borrow_mut() = delegate.map_or_else(Weak::default, Weak::new);
        }

        #[unsafe(method_id(textLayoutManager))]
        fn text_layout_manager(&self) -> Option<Retained<AnyObject>> {
            self.ivars().manager.borrow().load()
        }

        #[unsafe(method(viewportBounds))]
        fn viewport_bounds(&self) -> NSRect {
            self.ivars().bounds.get()
        }

        #[unsafe(method_id(viewportRange))]
        fn viewport_range(&self) -> Option<Retained<NSTextRange>> {
            self.ivars().range.borrow().clone()
        }

        #[unsafe(method(layoutViewport))]
        fn layout_viewport(&self) {
            self.lay_out();
        }

        #[unsafe(method(relocateViewportToTextLocation:))]
        fn relocate_viewport(&self, at: &AnyObject) -> f64 {
            let Some(m) = self.manager() else { return 0.0 };
            let Some(o) = offset_of(at) else { return 0.0 };
            let top = m.estimated_top(o.min(m.content_len()));
            let mut b = self.ivars().bounds.get();
            b.origin.y = top;
            self.ivars().bounds.set(b);
            *self.ivars().range.borrow_mut() = Some(location::range(o, o));
            top
        }

        #[unsafe(method(adjustViewportByVerticalOffset:))]
        fn adjust_viewport(&self, dy: f64) {
            let mut b = self.ivars().bounds.get();
            b.origin.y += dy;
            self.ivars().bounds.set(b);
        }
    }

    unsafe impl NSObjectProtocol for NSTextViewportLayoutControllerImpl {}
);

impl NSTextViewportLayoutControllerImpl {
    fn as_controller(&self) -> &NSTextViewportLayoutController {
        // SAFETY: NSTextViewportLayoutController is this class.
        unsafe { &*(self as *const Self).cast::<NSTextViewportLayoutController>() }
    }

    fn manager(&self) -> Option<Retained<super::layout_manager::NSTextLayoutManagerImpl>> {
        let m = self.ivars().manager.borrow().load()?;
        super::layout_manager::manager_of(&m)
    }

    fn lay_out(&self) {
        let Some(_busy) = Busy::begin(&self.ivars().busy) else { return };
        let delegate = self.ivars().delegate.borrow().load();
        let me = self.as_controller();
        let responds = |sel: Sel| delegate.as_ref().is_some_and(|d| crate::textkit::responds(d, sel));
        if let Some(d) = &delegate
            && responds(sel!(textViewportLayoutControllerWillLayout:))
        {
            // SAFETY: the delegate method takes the controller.
            let _: () = unsafe { msg_send![&**d, textViewportLayoutControllerWillLayout: me] };
        }
        let bounds = match &delegate {
            Some(d) if responds(sel!(viewportBoundsForTextViewportLayoutController:)) => {
                // SAFETY: the delegate method takes the controller and
                // returns a rect.
                let b: NSRect = unsafe { msg_send![&**d, viewportBoundsForTextViewportLayoutController: me] };
                b
            }
            _ => self.ivars().bounds.get(),
        };
        self.ivars().bounds.set(bounds);
        let (y0, y1) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
        let mut shown: Vec<Retained<NSTextLayoutFragment>> = Vec::new();
        let mut range: Option<(usize, usize)> = None;
        let manager = self.manager().filter(|_| bounds.size.height > 0.0);
        if let Some(extra) = manager.as_ref().and_then(|m| m.empty_document_fragment()) {
            // SAFETY: layoutFragmentFrame takes nothing.
            let frame: NSRect = unsafe { msg_send![&*extra, layoutFragmentFrame] };
            if frame.origin.y < y1 && frame.origin.y + frame.size.height >= y0 {
                shown.push(extra);
                range = Some((0, 0));
            }
        } else if let Some(m) = manager {
            m.ensure_y(y0, y1);
            let from = m.offset_at_top(y0);
            m.enumerate(Some(from), NSTextLayoutFragmentEnumerationOptions::EnsuresLayout, |f| {
                // SAFETY: layoutFragmentFrame takes nothing; a subclass may
                // override it.
                let frame: NSRect = unsafe { msg_send![f, layoutFragmentFrame] };
                let (top, bottom) = (frame.origin.y, frame.origin.y + frame.size.height);
                if top >= y1 && !(frame.size.height == 0.0 && top == y0) {
                    return false;
                }
                if bottom > y0 || (top >= y0 && top < y1) {
                    shown.push(f.retain());
                    if let Some((a, b)) =
                        super::fragment::ivars(f).and_then(|iv| iv.element()).and_then(|e| super::element::span(&e))
                    {
                        range = Some(range.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
                    }
                }
                true
            });
        }
        *self.ivars().range.borrow_mut() = range.map(|(a, b)| location::range(a, b));
        *self.ivars().configured.borrow_mut() = shown.clone();
        if let Some(d) = &delegate
            && responds(sel!(textViewportLayoutController:configureRenderingSurfaceForTextLayoutFragment:))
        {
            for f in &shown {
                let (d, f): (&AnyObject, &NSTextLayoutFragment) = (d, f);
                // SAFETY: the delegate method takes the controller and a
                // fragment.
                let _: () = unsafe {
                    msg_send![d, textViewportLayoutController: me, configureRenderingSurfaceForTextLayoutFragment: f]
                };
            }
        }
        if let Some(d) = &delegate
            && responds(sel!(textViewportLayoutControllerDidLayout:))
        {
            // SAFETY: the delegate method takes the controller.
            let _: () = unsafe { msg_send![&**d, textViewportLayoutControllerDidLayout: me] };
        }
    }
}

/// The fragments the last layout of `controller` (one of Sidestep's)
/// configured, top to bottom.
pub(crate) fn configured(controller: &AnyObject) -> Vec<Retained<NSTextLayoutFragment>> {
    let ours = <NSTextViewportLayoutController as ClassType>::class();
    if !crate::textkit::is_kind(controller.class(), ours) {
        return Vec::new();
    }
    // SAFETY: an instance of the class or a subclass.
    let c = unsafe { &*(controller as *const AnyObject).cast::<NSTextViewportLayoutControllerImpl>() };
    c.ivars().configured.borrow().clone()
}

/// A new controller for `manager`.
pub(crate) fn new_controller(manager: &NSTextLayoutManager) -> Retained<NSTextViewportLayoutController> {
    crate::load_shell::<NSTextViewportLayoutController>();
    let this = NSTextViewportLayoutControllerImpl::alloc().set_ivars(Ivars::new(Some(manager as &AnyObject)));
    // SAFETY: NSObject's initializer.
    let this: Retained<NSTextViewportLayoutControllerImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSTextViewportLayoutController is this class.
    unsafe { Retained::cast_unchecked(this) }
}
