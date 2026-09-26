//! Drag and drop into our windows: the destination side of AppKit's
//! dragging protocol over Wayland's (wl_data_device, followed on the render
//! thread by `backend::dnd`).
//!
//! Views and windows register the pasteboard types they take with
//! `registerForDraggedTypes:` (an ordered set, kept as an associated
//! object). When a drag comes over a window, the destination is the view
//! under the pointer, or the nearest of its superviews, that registered a
//! type the drag offers, else the window if it did; when it changes, the
//! old one gets `draggingExited:` and the new one `draggingEntered:`, and
//! otherwise it gets `draggingUpdated:` as the pointer moves or the
//! source's operations change. The render thread sends one move at a time
//! and waits for the answer, so a busy main thread sees the latest
//! position, not a queue of old ones. The answer is the operation the
//! destination returned, masked with the source's, as Wayland actions,
//! and the MIME type of the first registered type the drag offers.
//!
//! On a drop over a destination that took the drag, it gets
//! `prepareForDragOperation:`, then (if that said YES)
//! `performDragOperation:`, then (if that did too) `concludeDragOperation:`,
//! and `draggingEnded:` if it has one; the drop is finished only if
//! `performDragOperation:` said YES, which tells the source the data was
//! taken. A drop with no destination, or one whose last answer was no
//! operation, sends `draggingExited:` and is refused.
//!
//! The destination reads the drag from `draggingPasteboard`, the drag
//! pasteboard (`NSPasteboardNameDrag`), which holds the offer: its types
//! are known when the drag enters, and its data is read through the
//! render thread only when asked for (see `pasteboard`).
//!
//! Operations map as Wayland's allow: the source's copy is Copy, move is
//! Move and Generic, ask is Generic; the destination's Copy or Link is
//! copy, Move or Generic is move. Wayland can't show a dragged image or
//! slide one back, so those methods do nothing.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, MessageReceiver, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSDragOperation, NSDraggingFormation, NSPasteboard, NSSpringLoadingHighlight, NSView, NSWindow};
use objc2_foundation::{NSArray, NSPoint, NSString};

use crate::clipboard::{self, Source};
use crate::pasteboard_types::{self as types, FILE_URL, FILENAMES, OLD_URL, URL};
use crate::protocol::ToRender;
use crate::window;

/// Wayland's drag and drop actions (wl_data_device_manager.dnd_action).
pub const DND_COPY: u32 = 1;
pub const DND_MOVE: u32 = 2;
pub const DND_ASK: u32 = 4;

/// The operations a source offering Wayland `actions` allows.
pub(crate) fn from_wayland(actions: u32) -> NSDragOperation {
    let mut op = NSDragOperation::None;
    if actions & DND_COPY != 0 {
        op |= NSDragOperation::Copy;
    }
    if actions & DND_MOVE != 0 {
        op |= NSDragOperation::Move | NSDragOperation::Generic;
    }
    if actions & DND_ASK != 0 {
        op |= NSDragOperation::Generic;
    }
    op
}

/// A destination's operation as Wayland actions, and the one it prefers.
pub(crate) fn to_wayland(op: NSDragOperation) -> (u32, u32) {
    let copy = op.intersects(NSDragOperation::Copy | NSDragOperation::Link);
    let moves = op.intersects(NSDragOperation::Move | NSDragOperation::Generic);
    let actions = if copy { DND_COPY } else { 0 } | if moves { DND_MOVE } else { 0 };
    let preferred = if copy {
        DND_COPY
    } else if moves {
        DND_MOVE
    } else {
        0
    };
    (actions, preferred)
}

/// What the main thread tells the render thread about a drag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The MIME type taken (none: refused) and the Wayland actions.
    Status { mime: Option<String>, actions: u32, preferred: u32 },
    /// The drop is over: `performed` if the destination took the data.
    Finish { performed: bool },
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static SEQUENCE: Cell<isize> = const { Cell::new(0) };
    /// Replies kept for the testing hooks rather than sent.
    static CAPTURED: RefCell<Option<Vec<Reply>>> = const { RefCell::new(None) };
}

/// A drag over one of our windows.
struct Session {
    window: Retained<NSWindow>,
    /// Pasteboard types the drag offers (URLs as both kinds until read),
    /// and its MIME types.
    kinds: Vec<String>,
    mimes: Vec<String>,
    /// The view or window under the drag that takes it.
    dest: Option<Retained<AnyObject>>,
    /// What the destination last answered.
    last: NSDragOperation,
    info: Retained<DraggingInfo>,
}

fn reply(reply: Reply) {
    let kept = CAPTURED.with(|c| c.borrow_mut().as_mut().map(|replies| replies.push(reply.clone())).is_some());
    if kept {
        return;
    }
    crate::app::send_if_running(match reply {
        Reply::Status { mime, actions, preferred } => ToRender::DndStatus { mime, actions, preferred },
        Reply::Finish { performed } => ToRender::DndFinish { performed },
    });
}

/// A drag entered `window` at `x`, `y` (points from the content's top
/// left), offering `mimes`, with the source allowing Wayland `actions`.
pub(crate) fn enter(window: &NSWindow, x: f64, y: f64, mimes: Vec<String>, actions: u32) {
    // A drag that never said it left ends first.
    leave();
    clipboard::shared_for(Source::Drag).drag_offered(mimes.clone());
    let seq = SEQUENCE.with(|s| {
        s.set(s.get() + 1);
        s.get()
    });
    let mtm = MainThreadMarker::from(window);
    let info = DraggingInfo::new(mtm, window, seq, from_wayland(actions));
    info.ivars().location.set(location(window, x, y));
    let mut kinds: Vec<String> = mimes.iter().filter_map(|m| types::type_for_mime(m)).map(|k| k.into_owned()).collect();
    if types::url_mime(&mimes).is_some() {
        kinds.extend([FILE_URL.to_owned(), URL.to_owned()]);
    }
    let session = Session { window: window.retain(), kinds, mimes, dest: None, last: NSDragOperation::None, info };
    SESSION.with(|s| *s.borrow_mut() = Some(session));
    update();
}

/// The drag moved to `x`, `y` over the window it entered.
pub(crate) fn motion(x: f64, y: f64) {
    let Some((window, info)) = SESSION.with(|s| s.borrow().as_ref().map(|s| (s.window.clone(), s.info.clone()))) else {
        return;
    };
    info.ivars().location.set(location(&window, x, y));
    update();
}

/// The source's allowed actions changed (a modifier key, say).
pub(crate) fn actions(source: u32) {
    let Some(info) = SESSION.with(|s| s.borrow().as_ref().map(|s| s.info.clone())) else { return };
    info.ivars().mask.set(from_wayland(source));
    update();
}

/// The drag left without a drop.
pub(crate) fn leave() {
    let Some(session) = SESSION.with(|s| s.borrow_mut().take()) else { return };
    if let Some(dest) = &session.dest {
        // SAFETY: draggingExited: takes the dragging info (or nil).
        let _: () = unsafe { msg_send![&**dest, draggingExited: &*session.info] };
    }
}

/// The drag was dropped where it last was.
pub(crate) fn dropped() {
    let Some((dest, last, info)) =
        SESSION.with(|s| s.borrow().as_ref().map(|s| (s.dest.clone(), s.last, s.info.clone())))
    else {
        reply(Reply::Finish { performed: false });
        return;
    };
    let op = last & info.ivars().mask.get();
    let performed = match dest {
        Some(dest) if op != NSDragOperation::None => {
            // SAFETY: the dragging destination methods take the dragging
            // info; the first two return BOOL.
            let prepared: bool = unsafe { msg_send![&*dest, prepareForDragOperation: &*info] };
            let performed = prepared && unsafe { msg_send![&*dest, performDragOperation: &*info] };
            if performed {
                // SAFETY: as above.
                let _: () = unsafe { msg_send![&*dest, concludeDragOperation: &*info] };
            }
            if responds(&dest, sel!(draggingEnded:)) {
                // SAFETY: as above.
                let _: () = unsafe { msg_send![&*dest, draggingEnded: &*info] };
            }
            performed
        }
        Some(dest) => {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![&*dest, draggingExited: &*info] };
            false
        }
        None => false,
    };
    let ended = SESSION.with(|s| s.borrow_mut().take());
    // Released outside the borrow.
    drop(ended);
    reply(Reply::Finish { performed });
}

/// Find the destination, tell it (and the old one) what happened, and
/// answer the render thread.
fn update() {
    let Some((window, kinds, old, info)) = SESSION
        .with(|s| s.borrow().as_ref().map(|s| (s.window.clone(), s.kinds.clone(), s.dest.clone(), s.info.clone())))
    else {
        return;
    };
    let dest = destination(&window, info.ivars().location.get(), &kinds);
    let same = match (&old, &dest) {
        (Some(a), Some(b)) => std::ptr::eq(&**a, &**b),
        (None, None) => true,
        _ => false,
    };
    // Recorded first: the destination's methods may look at it (NSView's
    // draggingUpdated: answers what draggingEntered: did).
    SESSION.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.dest = dest.clone();
        }
    });
    let op = match &dest {
        Some(d) if !same => {
            if let Some(old) = &old {
                // SAFETY: draggingExited: takes the dragging info.
                let _: () = unsafe { msg_send![&**old, draggingExited: &*info] };
            }
            // SAFETY: draggingEntered: takes the dragging info and returns
            // an operation.
            unsafe { msg_send![&**d, draggingEntered: &*info] }
        }
        // SAFETY: as for draggingEntered:.
        Some(d) => unsafe { msg_send![&**d, draggingUpdated: &*info] },
        None => {
            if let Some(old) = &old {
                // SAFETY: as above.
                let _: () = unsafe { msg_send![&**old, draggingExited: &*info] };
            }
            NSDragOperation::None
        }
    };
    let mime = SESSION.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut()?;
        s.last = op;
        let dest = s.dest.as_ref()?;
        accepted_mime(dest, &s.kinds, &s.mimes)
    });
    let (actions, preferred) = to_wayland(op & info.ivars().mask.get());
    let mime = mime.filter(|_| actions != 0);
    reply(Reply::Status { mime, actions, preferred });
}

/// The deepest view under `location` (window coordinates) that registered
/// a type among `kinds`, or one of its superviews, else the window if it
/// did.
fn destination(window: &NSWindow, location: NSPoint, kinds: &[String]) -> Option<Retained<AnyObject>> {
    let content = window::imp(window).content();
    let mut view = content.and_then(|c| c.hitTest(location));
    while let Some(v) = view {
        if takes(&v, kinds) {
            return Some(Retained::into_super(Retained::into_super(Retained::into_super(v))));
        }
        // SAFETY: a view's superview is alive while the view is in it.
        view = unsafe { v.superview() };
    }
    let window: &AnyObject = window.as_ref();
    takes(window, kinds).then(|| window.retain())
}

/// Whether `object` registered a type among `kinds`.
fn takes(object: &AnyObject, kinds: &[String]) -> bool {
    registered(object).is_some_and(|types| types.iter().any(|t| offered(&types::from_ns(&t), kinds)))
}

fn offered(kind: &str, kinds: &[String]) -> bool {
    let has = |k: &str| kinds.iter().any(|o| o == k);
    match kind {
        FILENAMES => has(FILE_URL),
        OLD_URL => has(URL),
        k => has(k),
    }
}

/// The MIME type to accept for `dest`: its first registered type the drag
/// offers.
fn accepted_mime(dest: &AnyObject, kinds: &[String], mimes: &[String]) -> Option<String> {
    let registered = registered(dest)?;
    let kind = registered.iter().map(|t| types::from_ns(&t)).find(|t| offered(t, kinds))?;
    if matches!(kind.as_str(), FILE_URL | URL | FILENAMES | OLD_URL) {
        return types::url_mime(mimes).map(str::to_owned);
    }
    mimes.iter().find(|m| types::type_for_mime(m).is_some_and(|k| k == kind)).cloned()
}

fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// Where `x`, `y` (points from the content's top left) is in the window.
fn location(window: &NSWindow, x: f64, y: f64) -> NSPoint {
    NSPoint::new(x, window::imp(window).content_height() - y)
}

// Registered types, as an associated object of the view or window.

/// The address that keys the association.
static TYPES_KEY: u8 = 0;

fn registered(object: &AnyObject) -> Option<Retained<NSArray<NSString>>> {
    // SAFETY: the key is this module's static; the value, if any, is the
    // NSArray `register` stored, which the association keeps alive.
    unsafe {
        let value = objc2::ffi::objc_getAssociatedObject(object, (&raw const TYPES_KEY).cast::<c_void>());
        Retained::retain(value.cast::<NSArray<NSString>>().cast_mut())
    }
}

fn set_registered(object: &AnyObject, types: Option<&NSArray<NSString>>) {
    let value = types.map_or(std::ptr::null_mut(), |t| (t as *const NSArray<NSString>).cast::<AnyObject>().cast_mut());
    // SAFETY: the key is this module's static, and the association retains
    // the array (or removes the old one, for nil).
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            (object as *const AnyObject).cast_mut(),
            (&raw const TYPES_KEY).cast::<c_void>(),
            value,
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
    }
}

/// `registerForDraggedTypes:`: the types join the ones already registered,
/// each once, in order.
fn register(object: &AnyObject, new: &NSArray<NSString>) {
    let mut all: Vec<Retained<NSString>> = registered(object).map(|a| a.to_vec()).unwrap_or_default();
    for kind in new.iter() {
        if !all.iter().any(|k| k.isEqualToString(&kind)) {
            all.push(kind);
        }
    }
    set_registered(object, Some(&NSArray::from_retained_slice(&all)));
}

// The methods views and windows get (see `category`).

define_class!(
    // Holds NSView's drag destination methods, which `install` copies onto
    // NSView. `self` is an NSView there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepViewDragging"]
    struct ViewDragging;

    impl ViewDragging {
        #[unsafe(method(registerForDraggedTypes:))]
        fn register_for_dragged_types(&self, kinds: &NSArray<NSString>) {
            register(object(self), kinds);
        }

        #[unsafe(method(unregisterDraggedTypes))]
        fn unregister_dragged_types(&self) {
            set_registered(object(self), None);
        }

        #[unsafe(method_id(registeredDraggedTypes))]
        fn registered_dragged_types(&self) -> Retained<NSArray<NSString>> {
            registered(object(self)).unwrap_or_default()
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _sender: &AnyObject) -> NSDragOperation {
            NSDragOperation::None
        }

        /// What `draggingEntered:` (or the last `draggingUpdated:`) answered.
        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, _sender: &AnyObject) -> NSDragOperation {
            SESSION.with(|s| s.borrow().as_ref().map_or(NSDragOperation::None, |s| s.last))
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag_operation(&self, _sender: &AnyObject) -> bool {
            true
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, _sender: &AnyObject) -> bool {
            false
        }

        #[unsafe(method(concludeDragOperation:))]
        fn conclude_drag_operation(&self, _sender: Option<&AnyObject>) {}
    }
);

define_class!(
    // Holds NSWindow's drag destination methods, which `install` copies
    // onto NSWindow. `self` is an NSWindow there. A window that takes a
    // drag passes the destination methods on to its delegate.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepWindowDragging"]
    struct WindowDragging;

    impl WindowDragging {
        #[unsafe(method(registerForDraggedTypes:))]
        fn register_for_dragged_types(&self, kinds: &NSArray<NSString>) {
            register(object(self), kinds);
        }

        #[unsafe(method(unregisterDraggedTypes))]
        fn unregister_dragged_types(&self) {
            set_registered(object(self), None);
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &AnyObject) -> NSDragOperation {
            delegated(self, sel!(draggingEntered:), sender).unwrap_or(NSDragOperation::None)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, sender: &AnyObject) -> NSDragOperation {
            delegated(self, sel!(draggingUpdated:), sender)
                .unwrap_or_else(|| SESSION.with(|s| s.borrow().as_ref().map_or(NSDragOperation::None, |s| s.last)))
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, sender: Option<&AnyObject>) {
            if let Some(delegate) = delegate_with(self, sel!(draggingExited:)) {
                // SAFETY: draggingExited: takes the dragging info.
                let _: () = unsafe { msg_send![&*delegate, draggingExited: sender] };
            }
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag_operation(&self, sender: &AnyObject) -> bool {
            match delegate_with(self, sel!(prepareForDragOperation:)) {
                // SAFETY: prepareForDragOperation: takes the info, returns BOOL.
                Some(delegate) => unsafe { msg_send![&*delegate, prepareForDragOperation: sender] },
                None => true,
            }
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &AnyObject) -> bool {
            match delegate_with(self, sel!(performDragOperation:)) {
                // SAFETY: performDragOperation: takes the info, returns BOOL.
                Some(delegate) => unsafe { msg_send![&*delegate, performDragOperation: sender] },
                None => false,
            }
        }

        #[unsafe(method(concludeDragOperation:))]
        fn conclude_drag_operation(&self, sender: Option<&AnyObject>) {
            if let Some(delegate) = delegate_with(self, sel!(concludeDragOperation:)) {
                // SAFETY: concludeDragOperation: takes the info.
                let _: () = unsafe { msg_send![&*delegate, concludeDragOperation: sender] };
            }
        }
    }
);

/// A category method's receiver, which is an instance of the class the
/// method was copied onto, not of the helper.
fn object<T>(this: &T) -> &AnyObject {
    // SAFETY: every receiver is an object.
    unsafe { &*(this as *const T).cast::<AnyObject>() }
}

/// The window's delegate, if it has `selector`. `this` is an NSWindow.
fn delegate_with(this: &WindowDragging, selector: Sel) -> Option<Retained<AnyObject>> {
    // SAFETY: the method was copied onto NSWindow, so `this` is a window,
    // whose delegate is an object or nil.
    let window = unsafe { &*(this as *const WindowDragging).cast::<NSWindow>() };
    let delegate = window::imp(window).delegate_object()?;
    responds(&delegate, selector).then_some(delegate)
}

fn delegated(this: &WindowDragging, selector: Sel, sender: &AnyObject) -> Option<NSDragOperation> {
    let delegate = delegate_with(this, selector)?;
    // SAFETY: draggingEntered: and draggingUpdated: take the info and
    // return an operation.
    Some(unsafe { MessageReceiver::send_message::<_, NSDragOperation>(&*delegate, selector, (sender,)) })
}

/// Give NSView its drag destination methods; from NSView's loader.
pub(crate) fn install_view_methods() {
    crate::category::install(
        ViewDragging::class(),
        <NSView as ClassType>::class(),
        &[
            sel!(registerForDraggedTypes:),
            sel!(unregisterDraggedTypes),
            sel!(registeredDraggedTypes),
            sel!(draggingEntered:),
            sel!(draggingUpdated:),
            sel!(draggingExited:),
            sel!(prepareForDragOperation:),
            sel!(performDragOperation:),
            sel!(concludeDragOperation:),
        ],
    );
}

/// Give NSWindow its drag destination methods; from NSWindow's loader.
pub(crate) fn install_window_methods() {
    crate::category::install(
        WindowDragging::class(),
        <NSWindow as ClassType>::class(),
        &[
            sel!(registerForDraggedTypes:),
            sel!(unregisterDraggedTypes),
            sel!(draggingEntered:),
            sel!(draggingUpdated:),
            sel!(draggingExited:),
            sel!(prepareForDragOperation:),
            sel!(performDragOperation:),
            sel!(concludeDragOperation:),
        ],
    );
}

// The dragging info destinations are handed.

pub(crate) struct InfoIvars {
    window: Weak<NSWindow>,
    sequence: isize,
    /// Where the drag is, in the window's coordinates.
    location: Cell<NSPoint>,
    /// What the source allows.
    mask: Cell<NSDragOperation>,
    formation: Cell<NSDraggingFormation>,
    animates: Cell<bool>,
    valid_items: Cell<Option<isize>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepDraggingInfo"]
    #[ivars = InfoIvars]
    pub(crate) struct DraggingInfo;

    impl DraggingInfo {
        #[unsafe(method_id(draggingDestinationWindow))]
        fn dragging_destination_window(&self) -> Option<Retained<NSWindow>> {
            self.ivars().window.load()
        }

        #[unsafe(method(draggingSourceOperationMask))]
        fn dragging_source_operation_mask(&self) -> NSDragOperation {
            self.ivars().mask.get()
        }

        #[unsafe(method(draggingLocation))]
        fn dragging_location(&self) -> NSPoint {
            self.ivars().location.get()
        }

        #[unsafe(method(draggedImageLocation))]
        fn dragged_image_location(&self) -> NSPoint {
            self.ivars().location.get()
        }

        // Wayland shows the source's image itself.
        #[unsafe(method_id(draggedImage))]
        fn dragged_image(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method_id(draggingPasteboard))]
        fn dragging_pasteboard(&self) -> Retained<NSPasteboard> {
            crate::pasteboard::named(crate::pasteboard::DRAG)
        }

        // Drags from other programs have no source object.
        #[unsafe(method_id(draggingSource))]
        fn dragging_source(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(draggingSequenceNumber))]
        fn dragging_sequence_number(&self) -> isize {
            self.ivars().sequence
        }

        #[unsafe(method(slideDraggedImageTo:))]
        fn slide_dragged_image_to(&self, _point: NSPoint) {}

        // Promised files come from the source's own protocols, not Wayland.
        #[unsafe(method_id(namesOfPromisedFilesDroppedAtDestination:))]
        fn names_of_promised_files_dropped_at_destination(&self, _url: &AnyObject) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(draggingFormation))]
        fn dragging_formation(&self) -> NSDraggingFormation {
            self.ivars().formation.get()
        }

        #[unsafe(method(setDraggingFormation:))]
        fn set_dragging_formation(&self, formation: NSDraggingFormation) {
            self.ivars().formation.set(formation);
        }

        #[unsafe(method(animatesToDestination))]
        fn animates_to_destination(&self) -> bool {
            self.ivars().animates.get()
        }

        #[unsafe(method(setAnimatesToDestination:))]
        fn set_animates_to_destination(&self, flag: bool) {
            self.ivars().animates.set(flag);
        }

        /// The drag's items, unless the destination said otherwise.
        #[unsafe(method(numberOfValidItemsForDrop))]
        fn number_of_valid_items_for_drop(&self) -> isize {
            self.ivars().valid_items.get().unwrap_or_else(|| {
                let items = crate::pasteboard::named(crate::pasteboard::DRAG).pasteboardItems();
                items.map_or(0, |i| i.count() as isize)
            })
        }

        #[unsafe(method(setNumberOfValidItemsForDrop:))]
        fn set_number_of_valid_items_for_drop(&self, count: isize) {
            self.ivars().valid_items.set(Some(count));
        }

        // Dragging items (with their images) need NSDraggingItem; the
        // block is never called.
        #[unsafe(method(enumerateDraggingItemsWithOptions:forView:classes:searchOptions:usingBlock:))]
        fn enumerate_dragging_items(
            &self,
            _options: usize,
            _view: Option<&NSView>,
            _classes: &AnyObject,
            _search: &AnyObject,
            _block: &block2::DynBlock<dyn Fn(std::ptr::NonNull<AnyObject>, isize, std::ptr::NonNull<objc2::runtime::Bool>)>,
        ) {
        }

        #[unsafe(method(springLoadingHighlight))]
        fn spring_loading_highlight(&self) -> NSSpringLoadingHighlight {
            NSSpringLoadingHighlight::None
        }

        #[unsafe(method(resetSpringLoading))]
        fn reset_spring_loading(&self) {}
    }

    unsafe impl NSObjectProtocol for DraggingInfo {}
);

impl DraggingInfo {
    fn new(mtm: MainThreadMarker, window: &NSWindow, sequence: isize, mask: NSDragOperation) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(InfoIvars {
            window: Weak::new(window),
            sequence,
            location: Cell::new(NSPoint::ZERO),
            mask: Cell::new(mask),
            formation: Cell::new(NSDraggingFormation::Default),
            animates: Cell::new(false),
            valid_items: Cell::new(None),
        });
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Hooks for tests of the drag state without a compositor: they drive the
/// same functions the render thread's messages do, and collect the replies
/// that would go back to it.
#[doc(hidden)]
pub mod testing {
    use objc2_app_kit::NSWindow;

    pub use super::{DND_ASK, DND_COPY, DND_MOVE, Reply};

    /// Keep replies for `take_replies` instead of sending them.
    pub fn capture_replies() {
        super::CAPTURED.with(|c| *c.borrow_mut() = Some(Vec::new()));
    }

    pub fn take_replies() -> Vec<Reply> {
        super::CAPTURED.with(|c| c.borrow_mut().as_mut().map(std::mem::take).unwrap_or_default())
    }

    /// A drag enters `window` at `x`, `y` (points from the content's top
    /// left) offering `mimes`, the source allowing Wayland `actions`.
    pub fn enter(window: &NSWindow, x: f64, y: f64, mimes: &[&str], actions: u32) {
        super::enter(window, x, y, mimes.iter().map(|m| m.to_string()).collect(), actions);
    }

    pub fn motion(x: f64, y: f64) {
        super::motion(x, y);
    }

    pub fn source_actions(actions: u32) {
        super::actions(actions);
    }

    pub fn leave() {
        super::leave();
    }

    pub fn drop() {
        super::dropped();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_map_both_ways() {
        assert_eq!(from_wayland(DND_COPY), NSDragOperation::Copy);
        assert_eq!(from_wayland(DND_MOVE), NSDragOperation::Move | NSDragOperation::Generic);
        assert_eq!(from_wayland(DND_ASK), NSDragOperation::Generic);
        assert_eq!(from_wayland(0), NSDragOperation::None);
        assert_eq!(to_wayland(NSDragOperation::Copy), (DND_COPY, DND_COPY));
        assert_eq!(to_wayland(NSDragOperation::Link), (DND_COPY, DND_COPY));
        assert_eq!(to_wayland(NSDragOperation::Move), (DND_MOVE, DND_MOVE));
        assert_eq!(to_wayland(NSDragOperation::Generic), (DND_MOVE, DND_MOVE));
        assert_eq!(to_wayland(NSDragOperation::None), (0, 0));
        assert_eq!(to_wayland(NSDragOperation::Private), (0, 0));
        assert_eq!(to_wayland(NSDragOperation::Every), (DND_COPY | DND_MOVE, DND_COPY));
    }
}
