//! Accessibility beyond views and cells: `NSAccessibilityElement` (the
//! custom elements programs make for parts of a view that aren't views,
//! and subclass), `NSAccessibilityCustomAction`, and posting accessibility
//! notifications. Like the properties views and cells keep
//! (`controls::a11y`), all of it is stored until an AccessKit adapter
//! reads it; nothing does yet.
//!
//! An element answers the same properties as views and cells, from the
//! same store, with its own defaults as macOS gives them: no role or
//! label, not enabled, an element, no parent or children. Its frame is in
//! screen coordinates; one set in its parent's space
//! (`setAccessibilityFrameInParentSpace:`) is turned into the screen
//! frame through the parent view, as AppKit does, and a subclass that
//! overrides `accessibilityFrameInParentSpace` instead is asked for it.
//!
//! A posted notification (`NSAccessibilityPostNotificationWithUserInfo`)
//! goes into a short queue, newest last, for the adapter to take.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::NSView;
use objc2_foundation::{NSDictionary, NSRect, NSString, NSZone};

use crate::controls::a11y::{self, Node};

sidestep_runtime::static_class!(pub NSACCESSIBILITYELEMENT, NSACCESSIBILITYELEMENT_META = "NSAccessibilityElement", || {
    let class = AccessibilityElementImpl::class();
    a11y::install(class);
    install_class_methods(class);
});

sidestep_runtime::static_class!(
    pub NSACCESSIBILITYCUSTOMACTION,
    NSACCESSIBILITYCUSTOMACTION_META = "NSAccessibilityCustomAction",
    || {
        let _ = CustomActionImpl::class();
    }
);

// NSAccessibilityElement

pub(crate) struct ElementIvars {
    a11y: Node,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSAccessibilityElement"]
    #[ivars = ElementIvars]
    pub(crate) struct AccessibilityElementImpl;

    impl AccessibilityElementImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ElementIvars { a11y: Node::default() });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(accessibilityFrame))]
        fn accessibility_frame(&self) -> NSRect {
            frame_of(self)
        }

        #[unsafe(method(setAccessibilityFrame:))]
        fn set_accessibility_frame(&self, frame: NSRect) {
            let this: &AnyObject = self;
            a11y::write(this, |p| &mut p.frame, Some(frame));
            a11y::write(this, |p| &mut p.frame_in_parent, None);
        }

        #[unsafe(method(accessibilityFrameInParentSpace))]
        fn accessibility_frame_in_parent_space(&self) -> NSRect {
            let this: &AnyObject = self;
            a11y::read(this, |p| p.frame_in_parent).unwrap_or(NSRect::ZERO)
        }

        /// The screen frame is worked out from it from now on.
        #[unsafe(method(setAccessibilityFrameInParentSpace:))]
        fn set_accessibility_frame_in_parent_space(&self, frame: NSRect) {
            let this: &AnyObject = self;
            a11y::write(this, |p| &mut p.frame_in_parent, Some(frame));
            a11y::write(this, |p| &mut p.frame, None);
        }

        /// Nothing to press; subclasses that can be pressed say YES.
        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            false
        }

        #[unsafe(method(isAccessibilityFocused))]
        fn is_accessibility_focused(&self) -> bool {
            false
        }

        /// Adds `child` to the children and makes the element its parent.
        #[unsafe(method(accessibilityAddChildElement:))]
        fn accessibility_add_child_element(&self, child: &AnyObject) {
            add_child(self, child);
        }
    }

    unsafe impl NSObjectProtocol for AccessibilityElementImpl {}
);

impl AccessibilityElementImpl {
    fn node(&self) -> &Node {
        &self.ivars().a11y
    }
}

/// `object`'s record's hold, if it is an accessibility element.
pub(crate) fn element_node(object: &AnyObject) -> Option<&Node> {
    as_element(object).map(AccessibilityElementImpl::node)
}

/// Whether `object` is an accessibility element (or a subclass's).
pub(crate) fn is_element(object: &AnyObject) -> bool {
    as_element(object).is_some()
}

fn as_element(object: &AnyObject) -> Option<&AccessibilityElementImpl> {
    // The shell is the class once it has loaded (which it has if the
    // object is one), and comparing with it loads nothing.
    // SAFETY: a class shell is a class object.
    let ours = unsafe { &*(&raw const NSACCESSIBILITYELEMENT).cast::<AnyClass>() };
    crate::textkit::is_kind(object.class(), ours).then(|| {
        // SAFETY: an instance of the class or a subclass.
        unsafe { &*(object as *const AnyObject).cast::<AccessibilityElementImpl>() }
    })
}

/// The screen frame: the one set; else the frame in the parent's space,
/// asked by message (subclasses override it), through a parent view
/// that's in a window, or as it is.
fn frame_of(element: &AccessibilityElementImpl) -> NSRect {
    let this: &AnyObject = element;
    if let Some(frame) = a11y::read(this, |p| p.frame) {
        return frame;
    }
    // SAFETY: the method takes nothing and returns a rect.
    let local: NSRect = unsafe { msg_send![this, accessibilityFrameInParentSpace] };
    // SAFETY: accessibilityParent returns an object or nil.
    let parent: Option<Retained<AnyObject>> = unsafe { msg_send![this, accessibilityParent] };
    match parent.as_deref().and_then(|p| p.downcast_ref::<NSView>()) {
        Some(view) => match view.window() {
            Some(window) => window.convertRectToScreen(view.convertRect_toView(local, None)),
            None => local,
        },
        None => local,
    }
}

fn add_child(element: &AccessibilityElementImpl, child: &AnyObject) {
    let this: &AnyObject = element;
    // SAFETY: accessibilityChildren returns an array or nil.
    let children: Option<Retained<objc2_foundation::NSArray<AnyObject>>> =
        unsafe { msg_send![this, accessibilityChildren] };
    let mut all: Vec<Retained<AnyObject>> = children.map(|c| c.to_vec()).unwrap_or_default();
    all.push(child.retain());
    let array = objc2_foundation::NSArray::from_retained_slice(&all);
    // SAFETY: the setters take an array, and an object.
    unsafe {
        let _: () = msg_send![this, setAccessibilityChildren: &*array];
        let _: () = msg_send![child, setAccessibilityParent: this];
    }
}

/// `+accessibilityElementWithRole:frame:label:parent:`, which makes an
/// instance of the class it is sent to (with `+new`, so a subclass's
/// initializer runs): added by hand when the class loads, as
/// `define_class!` class methods don't see their receiver.
unsafe extern "C-unwind" fn element_with_role(
    class: &AnyClass,
    _cmd: Sel,
    role: Option<&NSString>,
    frame: NSRect,
    label: Option<&NSString>,
    parent: Option<&AnyObject>,
) -> *mut AnyObject {
    // SAFETY: +new makes an instance; the setters take what they're given.
    let element: Retained<AnyObject> = unsafe {
        let made: Option<Retained<AnyObject>> = msg_send![class, new];
        let element = made.expect("sidestep: an accessibility element's initializer returned nil");
        let _: () = msg_send![&*element, setAccessibilityRole: role];
        let _: () = msg_send![&*element, setAccessibilityFrame: frame];
        let _: () = msg_send![&*element, setAccessibilityLabel: label];
        let _: () = msg_send![&*element, setAccessibilityParent: parent];
        element
    };
    Retained::autorelease_return(element)
}

/// Give the class [`element_with_role`].
fn install_class_methods(class: &AnyClass) {
    type Factory = unsafe extern "C-unwind" fn(
        &AnyClass,
        Sel,
        Option<&NSString>,
        NSRect,
        Option<&NSString>,
        Option<&AnyObject>,
    ) -> *mut AnyObject;
    let types = format!("@@:@{}@@", <NSRect as objc2::encode::Encode>::ENCODING);
    let types = std::ffi::CString::new(types).expect("encodings have no NUL");
    let sel = objc2::sel!(accessibilityElementWithRole:frame:label:parent:);
    // SAFETY: the implementation takes the receiver (the class), the
    // selector and the arguments the encoding gives, and returns an
    // object; it is added to the metaclass, so it is a class method.
    let added = unsafe {
        let imp = std::mem::transmute::<Factory, objc2::runtime::Imp>(element_with_role);
        let meta = (class.metaclass() as *const AnyClass).cast_mut();
        objc2::ffi::class_addMethod(meta, sel, imp, types.as_ptr())
    };
    assert!(added.as_bool(), "sidestep: the element factory was already defined");
}

// NSAccessibilityCustomAction

type Handler = DynBlock<dyn Fn() -> Bool>;

#[derive(Default)]
pub(crate) struct ActionIvars {
    name: RefCell<Option<Retained<NSString>>>,
    handler: RefCell<Option<RcBlock<dyn Fn() -> Bool>>>,
    /// Weak, as AppKit's.
    target: RefCell<Weak<AnyObject>>,
    selector: Cell<Option<Sel>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSAccessibilityCustomAction"]
    #[ivars = ActionIvars]
    pub(crate) struct CustomActionImpl;

    impl CustomActionImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ActionIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithName:handler:))]
        fn init_with_name_handler(this: Allocated<Self>, name: &NSString, handler: Option<&Handler>) -> Retained<Self> {
            let ivars = ActionIvars::default();
            ivars.name.replace(Some(name.copy_string()));
            ivars.handler.replace(handler.map(|h| h.copy()));
            let this = this.set_ivars(ivars);
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithName:target:selector:))]
        fn init_with_name_target_selector(
            this: Allocated<Self>,
            name: &NSString,
            target: &AnyObject,
            selector: Sel,
        ) -> Retained<Self> {
            let ivars = ActionIvars::default();
            ivars.name.replace(Some(name.copy_string()));
            ivars.target.replace(Weak::new(target));
            ivars.selector.set(Some(selector));
            let this = this.set_ivars(ivars);
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(name))]
        fn name(&self) -> Option<Retained<NSString>> {
            self.ivars().name.borrow().clone()
        }

        #[unsafe(method(setName:))]
        fn set_name(&self, name: Option<&NSString>) {
            let old = self.ivars().name.replace(name.map(|n| n.copy_string()));
            drop(old);
        }

        #[unsafe(method(handler))]
        fn handler(&self) -> *mut Handler {
            self.ivars().handler.borrow().as_ref().map_or(std::ptr::null_mut(), RcBlock::as_ptr)
        }

        #[unsafe(method(setHandler:))]
        fn set_handler(&self, handler: Option<&Handler>) {
            let old = self.ivars().handler.replace(handler.map(|h| h.copy()));
            drop(old);
        }

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            self.ivars().target.borrow().load()
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            let old = self.ivars().target.replace(target.map_or_else(Weak::default, Weak::new));
            drop(old);
        }

        #[unsafe(method(selector))]
        fn selector(&self) -> Option<Sel> {
            self.ivars().selector.get()
        }

        #[unsafe(method(setSelector:))]
        fn set_selector(&self, selector: Option<Sel>) {
            self.ivars().selector.set(selector);
        }

        /// `<NSAccessibilityCustomAction: 0x…> Name`, as macOS writes it.
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let name = self.ivars().name.borrow().as_ref().map(|n| n.to_string()).unwrap_or_default();
            NSString::from_str(&format!("<NSAccessibilityCustomAction: {:p}> {name}", self))
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for CustomActionImpl {}
);

// Notifications.

/// A notification posted for assistive technology.
pub(crate) struct Posted {
    pub element: Weak<AnyObject>,
    pub name: Retained<NSString>,
    pub user_info: Option<Retained<NSDictionary<NSString, AnyObject>>>,
}

/// How many posted notifications wait for an adapter; older ones go.
const KEPT: usize = 64;

thread_local! {
    static POSTED: RefCell<VecDeque<Posted>> = const { RefCell::new(VecDeque::new()) };
}

/// The notifications posted since the last call, oldest first.
pub(crate) fn take_posted() -> Vec<Posted> {
    POSTED.with_borrow_mut(|q| q.drain(..).collect())
}

/// A notification posted off the main thread, on its way to the main
/// thread's queue, where the accessibility adapter reads.
struct Carried {
    element: Retained<AnyObject>,
    name: Retained<NSString>,
    user_info: Option<Retained<NSDictionary<NSString, AnyObject>>>,
}

// SAFETY: it only moves to the main thread, which queues it; reference
// counts are atomic, so the objects may be retained here and released
// there.
unsafe impl Send for Carried {}

fn post(element: &AnyObject, name: &NSString, user_info: Option<&NSDictionary<NSString, AnyObject>>) {
    if objc2::MainThreadMarker::new().is_none() {
        let carried =
            Carried { element: element.retain(), name: name.copy_string(), user_info: user_info.map(|i| i.retain()) };
        use sidestep_foundation::runloop::{self, Mode};
        runloop::main().perform(&[Mode::COMMON], move || {
            let carried = carried;
            post(&carried.element, &carried.name, carried.user_info.as_deref());
        });
        return;
    }
    let posted =
        Posted { element: Weak::new(element), name: name.copy_string(), user_info: user_info.map(|i| i.retain()) };
    let gone = POSTED.with_borrow_mut(|q| {
        q.push_back(posted);
        (q.len() > KEPT).then(|| q.pop_front()).flatten()
    });
    // Released outside the borrow.
    drop(gone);
}

/// # Safety
///
/// `element` and `notification` are objects; `user_info` is a dictionary
/// or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSAccessibilityPostNotificationWithUserInfo(
    element: &AnyObject,
    notification: &NSString,
    user_info: Option<&NSDictionary<NSString, AnyObject>>,
) {
    post(element, notification, user_info);
}

/// # Safety
///
/// `element` and `notification` are objects.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn NSAccessibilityPostNotification(element: &AnyObject, notification: &NSString) {
    post(element, notification, None);
}

trait CopyString {
    fn copy_string(&self) -> Retained<NSString>;
}

impl CopyString for NSString {
    /// An immutable copy, as AppKit's `copy` properties keep.
    fn copy_string(&self) -> Retained<NSString> {
        // SAFETY: -copy of a string is a string.
        unsafe { msg_send![self, copy] }
    }
}
