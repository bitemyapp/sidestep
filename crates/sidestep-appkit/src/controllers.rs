//! `NSViewController` and `NSWindowController`, without nibs (Linux has
//! none to load).
//!
//! As on macOS (`conformance/tests/appkit_events.rs`): a view controller
//! loads its view the first time it is asked (`loadView`, then
//! `viewDidLoad`); without a nib, `loadView` makes a plain view. A
//! controller sits in the responder chain between its view and the view's
//! superview: its view's next responder is the controller, and whatever
//! sets the view's next responder later (adding it to a superview) sets the
//! controller's instead (the view's `controller` link, in `responder`).
//! When the controller lets go of its view, or goes, the view has the
//! controller's next responder again. A controller given a view another
//! controller has takes the other's place (macOS chains the new one to the
//! old one until the old one goes; Sidestep takes the old one out of the
//! chain at once, so nothing is left pointing at it). Child controllers
//! keep their parent weakly.
//!
//! A window controller holding a window is the window's next responder and
//! its `windowController`, and keeps it: the window isn't released when
//! closed. (A window merely told its controller with `setWindowController:`
//! doesn't chain to it.) It forwards its content view controller and frame
//! autosave name to the window. A window made with
//! `windowWithContentViewController:` has the view controller's view's size
//! and title, the four standard buttons, and belongs to its caller (not
//! released when closed).

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSResponder, NSView, NSViewController, NSWindow, NSWindowController, NSWindowStyleMask,
};
use objc2_foundation::{NSBundle, NSPoint, NSRect, NSSize, NSString};

use crate::responder;

pub(crate) struct ViewControllerIvars {
    view: RefCell<Option<Retained<NSView>>>,
    nib_name: Option<Retained<NSString>>,
    nib_bundle: Option<Retained<NSBundle>>,
    title: RefCell<Option<Retained<NSString>>>,
    represented: RefCell<Option<Retained<AnyObject>>>,
    children: RefCell<Vec<Retained<NSViewController>>>,
    parent: RefCell<Option<Weak<NSViewController>>>,
    preferred_size: Cell<NSSize>,
}

define_class!(
    #[unsafe(super(NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSViewController"]
    #[ivars = ViewControllerIvars]
    pub(crate) struct NSViewControllerImpl;

    impl NSViewControllerImpl {
        #[unsafe(method_id(initWithNibName:bundle:))]
        fn init_with_nib_name(
            this: Allocated<Self>,
            nib: Option<&NSString>,
            bundle: Option<&NSBundle>,
        ) -> Retained<Self> {
            let this = this.set_ivars(ViewControllerIvars {
                view: RefCell::new(None),
                nib_name: nib.map(|n| n.retain()),
                nib_bundle: bundle.map(|b| b.retain()),
                title: RefCell::new(None),
                represented: RefCell::new(None),
                children: RefCell::new(Vec::new()),
                parent: RefCell::new(None),
                preferred_size: Cell::new(NSSize::ZERO),
            });
            // SAFETY: NSResponder's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer, with no nib.
            unsafe { msg_send![this, initWithNibName: None::<&NSString>, bundle: None::<&NSBundle>] }
        }

        #[unsafe(method_id(nibName))]
        fn nib_name(&self) -> Option<Retained<NSString>> {
            self.ivars().nib_name.clone()
        }

        #[unsafe(method_id(nibBundle))]
        fn nib_bundle(&self) -> Option<Retained<NSBundle>> {
            self.ivars().nib_bundle.clone()
        }

        /// Loads the view the first time.
        #[unsafe(method_id(view))]
        fn view(&self) -> Retained<NSView> {
            if self.ivars().view.borrow().is_none() {
                load(self);
            }
            self.ivars().view.borrow().clone().expect("loadView left the view controller without a view")
        }

        #[unsafe(method(setView:))]
        fn set_view(&self, view: &NSView) {
            set_view(self, view);
        }

        #[unsafe(method_id(viewIfLoaded))]
        fn view_if_loaded(&self) -> Option<Retained<NSView>> {
            self.ivars().view.borrow().clone()
        }

        #[unsafe(method(isViewLoaded))]
        fn is_view_loaded(&self) -> bool {
            self.ivars().view.borrow().is_some()
        }

        /// Without a nib: a plain view.
        #[unsafe(method(loadView))]
        fn load_view(&self) {
            let view = NSView::initWithFrame(NSView::alloc(self.mtm()), NSRect::ZERO);
            set_view(self, &view);
        }

        #[unsafe(method(loadViewIfNeeded))]
        fn load_view_if_needed(&self) {
            if self.ivars().view.borrow().is_none() {
                load(self);
            }
        }

        #[unsafe(method(viewDidLoad))]
        fn view_did_load(&self) {}

        #[unsafe(method(viewWillAppear))]
        fn view_will_appear(&self) {}

        #[unsafe(method(viewDidAppear))]
        fn view_did_appear(&self) {}

        #[unsafe(method(viewWillDisappear))]
        fn view_will_disappear(&self) {}

        #[unsafe(method(viewDidDisappear))]
        fn view_did_disappear(&self) {}

        #[unsafe(method(viewWillLayout))]
        fn view_will_layout(&self) {}

        #[unsafe(method(viewDidLayout))]
        fn view_did_layout(&self) {}

        #[unsafe(method(updateViewConstraints))]
        fn update_view_constraints(&self) {}

        #[unsafe(method_id(title))]
        fn title(&self) -> Option<Retained<NSString>> {
            self.ivars().title.borrow().clone()
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: Option<&NSString>) {
            let old = self.ivars().title.replace(title.map(objc2_foundation::NSCopying::copy));
            drop(old);
        }

        #[unsafe(method_id(representedObject))]
        fn represented_object(&self) -> Option<Retained<AnyObject>> {
            self.ivars().represented.borrow().clone()
        }

        #[unsafe(method(setRepresentedObject:))]
        fn set_represented_object(&self, object: Option<&AnyObject>) {
            let old = self.ivars().represented.replace(object.map(|o| o.retain()));
            drop(old);
        }

        #[unsafe(method(preferredContentSize))]
        fn preferred_content_size(&self) -> NSSize {
            self.ivars().preferred_size.get()
        }

        #[unsafe(method(setPreferredContentSize:))]
        fn set_preferred_content_size(&self, size: NSSize) {
            self.ivars().preferred_size.set(size);
        }

        #[unsafe(method(commitEditing))]
        fn commit_editing(&self) -> bool {
            true
        }

        #[unsafe(method(discardEditing))]
        fn discard_editing(&self) {}

        #[unsafe(method_id(childViewControllers))]
        fn child_view_controllers(&self) -> Retained<AnyObject> {
            crate::app::array_of(&self.ivars().children.borrow())
        }

        #[unsafe(method(addChildViewController:))]
        fn add_child_view_controller(&self, child: &NSViewController) {
            let at = self.ivars().children.borrow().len();
            insert_child(self, child, at);
        }

        #[unsafe(method(insertChildViewController:atIndex:))]
        fn insert_child_view_controller(&self, child: &NSViewController, index: isize) {
            insert_child(self, child, index.max(0) as usize);
        }

        #[unsafe(method(removeChildViewControllerAtIndex:))]
        fn remove_child_view_controller_at_index(&self, index: isize) {
            let child = self.ivars().children.borrow().get(index.max(0) as usize).cloned();
            if let Some(child) = child {
                child.removeFromParentViewController();
            }
        }

        #[unsafe(method(removeFromParentViewController))]
        fn remove_from_parent_view_controller(&self) {
            let Some(parent) = self.ivars().parent.take().and_then(|p| p.load()) else { return };
            let parent = imp(&parent);
            let gone = {
                let mut children = parent.ivars().children.borrow_mut();
                let at = children.iter().position(|c| std::ptr::eq(imp(c), self));
                at.map(|i| children.remove(i))
            };
            // Released outside the borrow.
            drop(gone);
        }

        #[unsafe(method_id(parentViewController))]
        fn parent_view_controller(&self) -> Option<Retained<NSViewController>> {
            self.ivars().parent.borrow().as_ref().and_then(Weak::load)
        }
    }

    unsafe impl NSObjectProtocol for NSViewControllerImpl {}
);

impl Drop for NSViewControllerImpl {
    fn drop(&mut self) {
        // The view may outlive its controller: nothing is left pointing at
        // the controller.
        let view = self.ivars().view.borrow().clone();
        if let Some(view) = view {
            let_go(self, &view);
        }
    }
}

fn imp(controller: &NSViewController) -> &NSViewControllerImpl {
    // SAFETY: every NSViewController is an NSViewControllerImpl.
    unsafe { &*(controller as *const NSViewController).cast::<NSViewControllerImpl>() }
}

fn as_controller(controller: &NSViewControllerImpl) -> &NSViewController {
    // SAFETY: NSViewControllerImpl is the class NSViewController names.
    unsafe { &*(controller as *const NSViewControllerImpl).cast::<NSViewController>() }
}

/// `loadView`, then `viewDidLoad`, by message, as subclasses override them.
fn load(controller: &NSViewControllerImpl) {
    let this = as_controller(controller);
    this.loadView();
    this.viewDidLoad();
}

fn insert_child(parent: &NSViewControllerImpl, child: &NSViewController, at: usize) {
    child.removeFromParentViewController();
    let mut children = parent.ivars().children.borrow_mut();
    let at = at.min(children.len());
    children.insert(at, child.retain());
    drop(children);
    let old = imp(child).ivars().parent.replace(Some(Weak::new(as_controller(parent))));
    drop(old);
}

/// Make `view` the controller's, between it and whatever it is next to.
fn set_view(controller: &NSViewControllerImpl, view: &NSView) {
    let this = as_controller(controller);
    let this_ptr = NonNull::from(this);
    let old = controller.ivars().view.replace(Some(view.retain()));
    if let Some(old) = old.as_deref().filter(|o| !std::ptr::eq(*o, view)) {
        let_go(controller, old);
    }
    if responder::controller_of(view) == Some(this_ptr) {
        // Already the view's.
        return;
    }
    // What comes after the view comes after the controller: taken from a
    // controller the view has (which leaves the chain), or the view's own.
    let next = match responder::controller_of(view) {
        Some(other) => {
            // SAFETY: a controller named by a view holds the view, so it is
            // alive.
            let other: &NSResponder = unsafe { other.as_ref() };
            let next = responder::next_of(other);
            // SAFETY: clearing the link.
            unsafe { responder::link_next(other, None) };
            next
        }
        None => responder::next_of(view),
    };
    let next = next.filter(|n| n.as_ptr() != this_ptr.as_ptr().cast());
    // SAFETY: the controller's next responder is what the view's was (its
    // superview or window), which clears the link before it goes as it
    // would the view's (every change to the view's link goes to the
    // controller); the controller holds the view, and clears the view's
    // links to it when it lets go (`let_go`).
    unsafe {
        responder::link_next(this, next);
        responder::set_controller(view, Some(this_ptr));
        responder::link_next(view, Some(this_ptr.cast()));
    }
    drop(old);
}

/// The controller lets go of `view` (for another, or going away): if it
/// stands for the view, the view has the controller's next responder
/// again.
fn let_go(controller: &NSViewControllerImpl, view: &NSView) {
    let this = as_controller(controller);
    let this_ptr = NonNull::from(this);
    if responder::controller_of(view) != Some(this_ptr) {
        return;
    }
    let next = responder::next_of(this);
    // SAFETY: the controller's next responder is the view's superview or
    // window, which clears the view's link before it goes; the rest clears
    // links.
    unsafe {
        responder::set_controller(view, None);
        responder::link_next(view, next);
        responder::link_next(this, None);
    }
}

// NSWindowController.

pub(crate) struct WindowControllerIvars {
    window: RefCell<Option<Retained<NSWindow>>>,
    cascade: Cell<bool>,
    autosave_name: RefCell<Retained<NSString>>,
    document: RefCell<Option<Weak<AnyObject>>>,
    should_close_document: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSWindowController"]
    #[ivars = WindowControllerIvars]
    pub(crate) struct NSWindowControllerImpl;

    impl NSWindowControllerImpl {
        #[unsafe(method_id(initWithWindow:))]
        fn init_with_window(this: Allocated<Self>, window: Option<&NSWindow>) -> Retained<Self> {
            let this = this.set_ivars(WindowControllerIvars {
                window: RefCell::new(None),
                cascade: Cell::new(true),
                autosave_name: RefCell::new(NSString::new()),
                document: RefCell::new(None),
                should_close_document: Cell::new(false),
            });
            // SAFETY: NSResponder's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            set_window(&this, window);
            this
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer, with no window.
            unsafe { msg_send![this, initWithWindow: None::<&NSWindow>] }
        }

        #[unsafe(method_id(window))]
        fn window(&self) -> Option<Retained<NSWindow>> {
            self.ivars().window.borrow().clone()
        }

        #[unsafe(method(setWindow:))]
        fn set_window(&self, window: Option<&NSWindow>) {
            set_window(self, window);
        }

        /// Without nibs, the window is whatever the controller was given.
        #[unsafe(method(isWindowLoaded))]
        fn is_window_loaded(&self) -> bool {
            true
        }

        #[unsafe(method(loadWindow))]
        fn load_window(&self) {}

        #[unsafe(method(windowWillLoad))]
        fn window_will_load(&self) {}

        #[unsafe(method(windowDidLoad))]
        fn window_did_load(&self) {}

        #[unsafe(method(showWindow:))]
        fn show_window(&self, _sender: Option<&AnyObject>) {
            let window = self.ivars().window.borrow().clone();
            if let Some(window) = window {
                window.makeKeyAndOrderFront(None);
            }
        }

        #[unsafe(method(close))]
        fn close(&self) {
            let window = self.ivars().window.borrow().clone();
            if let Some(window) = window {
                window.close();
            }
        }

        #[unsafe(method(shouldCascadeWindows))]
        fn should_cascade_windows(&self) -> bool {
            self.ivars().cascade.get()
        }

        #[unsafe(method(setShouldCascadeWindows:))]
        fn set_should_cascade_windows(&self, flag: bool) {
            self.ivars().cascade.set(flag);
        }

        #[unsafe(method_id(windowFrameAutosaveName))]
        fn window_frame_autosave_name(&self) -> Retained<NSString> {
            self.ivars().autosave_name.borrow().clone()
        }

        #[unsafe(method(setWindowFrameAutosaveName:))]
        fn set_window_frame_autosave_name(&self, name: &NSString) {
            let old = self.ivars().autosave_name.replace(objc2_foundation::NSCopying::copy(name));
            drop(old);
            let window = self.ivars().window.borrow().clone();
            if let Some(window) = window {
                window.setFrameAutosaveName(name);
            }
        }

        #[unsafe(method_id(contentViewController))]
        fn content_view_controller(&self) -> Option<Retained<NSViewController>> {
            self.ivars().window.borrow().as_ref().and_then(|w| w.contentViewController())
        }

        #[unsafe(method(setContentViewController:))]
        fn set_content_view_controller(&self, controller: Option<&NSViewController>) {
            let window = self.ivars().window.borrow().clone();
            if let Some(window) = window {
                window.setContentViewController(controller);
            }
        }

        #[unsafe(method_id(document))]
        fn document(&self) -> Option<Retained<AnyObject>> {
            self.ivars().document.borrow().as_ref().and_then(Weak::load)
        }

        #[unsafe(method(setDocument:))]
        fn set_document(&self, document: Option<&AnyObject>) {
            let old = self.ivars().document.replace(document.map(Weak::new));
            drop(old);
        }

        #[unsafe(method(shouldCloseDocument))]
        fn should_close_document(&self) -> bool {
            self.ivars().should_close_document.get()
        }

        #[unsafe(method(setShouldCloseDocument:))]
        fn set_should_close_document(&self, flag: bool) {
            self.ivars().should_close_document.set(flag);
        }

        #[unsafe(method(setDocumentEdited:))]
        fn set_document_edited(&self, flag: bool) {
            let window = self.ivars().window.borrow().clone();
            if let Some(window) = window {
                window.setDocumentEdited(flag);
            }
        }

        #[unsafe(method(synchronizeWindowTitleWithDocumentName))]
        fn synchronize_window_title_with_document_name(&self) {}

        #[unsafe(method_id(windowTitleForDocumentDisplayName:))]
        fn window_title_for_document_display_name(&self, name: &NSString) -> Retained<NSString> {
            name.retain()
        }

        #[unsafe(method_id(owner))]
        fn owner(&self) -> Option<Retained<AnyObject>> {
            let this: &AnyObject = self;
            Some(this.retain())
        }

        #[unsafe(method_id(windowNibName))]
        fn window_nib_name(&self) -> Option<Retained<NSString>> {
            None
        }

        #[unsafe(method_id(windowNibPath))]
        fn window_nib_path(&self) -> Option<Retained<NSString>> {
            None
        }
    }

    unsafe impl NSObjectProtocol for NSWindowControllerImpl {}
);

impl Drop for NSWindowControllerImpl {
    fn drop(&mut self) {
        // The window may outlive its controller: it no longer points at it.
        let window = self.ivars().window.borrow_mut().take();
        if let Some(window) = window {
            responder::unlink_next(&window, NonNull::from(window_controller(self)).cast());
        }
    }
}

fn window_controller(controller: &NSWindowControllerImpl) -> &NSWindowController {
    // SAFETY: NSWindowControllerImpl is the class NSWindowController names.
    unsafe { &*(controller as *const NSWindowControllerImpl).cast::<NSWindowController>() }
}

/// Take `window`, and be its controller and next responder; let go of the
/// window held before.
fn set_window(controller: &NSWindowControllerImpl, window: Option<&NSWindow>) {
    let this = window_controller(controller);
    let this_ptr = NonNull::from(this).cast::<NSResponder>();
    let old = controller.ivars().window.replace(window.map(|w| w.retain()));
    if let Some(old) = old.as_deref().filter(|o| window.is_none_or(|w| !std::ptr::eq(*o, w))) {
        responder::unlink_next(old, this_ptr);
        if old.windowController().is_some_and(|c| std::ptr::eq(&*c, this)) {
            old.setWindowController(None);
        }
    }
    if let Some(window) = window {
        // SAFETY: the controller keeps the window, so closing it mustn't
        // give up a reference.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setWindowController(Some(this));
        // SAFETY: the controller holds the window, and clears the link
        // when it lets go of it (above, or when it goes).
        unsafe { responder::link_next(window, Some(this_ptr)) };
    }
    drop(old);
}

/// `+[NSWindow windowWithContentViewController:]`.
pub(crate) fn window_with_content_view_controller(controller: &NSViewController) -> Retained<NSWindow> {
    let mtm = controller.mtm();
    let size = controller.view().frame().size;
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    crate::load_shell::<NSWindow>();
    // SAFETY: NSWindow's designated initializer.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::ZERO, size),
            style,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: the caller gets the window's only reference, which closing it
    // mustn't give up.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setContentViewController(Some(controller));
    if let Some(title) = controller.title() {
        window.setTitle(&title);
    }
    window
}
