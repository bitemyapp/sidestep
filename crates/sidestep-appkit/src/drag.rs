//! Drag and drop into our windows: the destination side of AppKit's
//! dragging protocol over Wayland's (wl_data_device, followed on the render
//! thread by `backend::dnd`).
//!
//! Views and windows register the pasteboard types they take with
//! `registerForDraggedTypes:` (an ordered set, kept as an associated
//! object). When a drag comes over a window, the destination is the view
//! under the pointer, or the nearest of its superviews, that registered a
//! type the drag's pasteboard has (as `availableTypeFromArray:` finds it: a
//! file drag has `public.url`, a link has no `public.file-url`), else the
//! window if it did; when it changes, the old one gets `draggingExited:`
//! and the new one `draggingEntered:`, and otherwise it gets
//! `draggingUpdated:` as the pointer moves or the source's operations
//! change, and every 50 ms while the drag waits, unless it answers NO to
//! `wantsPeriodicDraggingUpdates` (asked when it becomes the destination;
//! a window asks its delegate, and says NO for one that doesn't say). The
//! render thread sends one move at a time and waits for the answer, so a
//! busy main thread sees the latest position, not a queue of old ones. The
//! answer is the operation the destination returned, masked with the
//! source's, as Wayland actions, and the MIME type of the first registered
//! type the drag has. Every message about a drag gets exactly one answer,
//! naming the drag, a refusal when there's no such drag or window, as the
//! render thread counts on.
//!
//! On a drop over a destination that took the drag, it gets
//! `prepareForDragOperation:`, then (if that said YES)
//! `performDragOperation:`, then (if that did too) `concludeDragOperation:`,
//! and `draggingEnded:` if it has one; the drop is finished only if
//! `performDragOperation:` said YES, which tells the source the data was
//! taken. A drop with no destination, or one whose last answer was no
//! operation, sends `draggingExited:` and is refused.
//!
//! Destinations may run a nested event loop (a modal panel in
//! `performDragOperation:`, say), so each call into them can see other
//! drags come and go: a drop ends its drag before the destination hears of
//! it, and every step works on its own drag, found by the name the render
//! thread gave it, never on whichever drag is current when a call returns.
//! (The drag pasteboard, of which there is one, as on macOS, then holds
//! the newer drag.)
//!
//! The destination reads the drag from `draggingPasteboard`, the drag
//! pasteboard (`NSPasteboardNameDrag`), which holds the offer: its types
//! and URLs are known when the drag enters (the render thread reads the URL
//! list first), and the rest of its data is read through the render thread
//! only when asked for, from that drag's offer, until the drop is finished
//! (see `pasteboard`). `enumerateDraggingItems…` hands out an
//! `NSDraggingItem` for each of its items that can be read as one of the
//! classes asked for, as `readObjectsForClasses:options:` reads them.
//!
//! Operations map as Wayland's allow: the source's copy is Copy, move is
//! Move and Generic, ask is Generic; the destination's Copy or Link is
//! copy, Move or Generic is move. Wayland can't show a dragged image or
//! slide one back, so those methods do nothing.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use block2::DynBlock;
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, Bool, MessageReceiver, NSObject, NSObjectProtocol, Sel};
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSDragOperation, NSDraggingFormation, NSDraggingItem, NSDraggingItemEnumerationOptions, NSPasteboard,
    NSSpringLoadingHighlight, NSView, NSWindow,
};
use objc2_foundation::{NSArray, NSDictionary, NSPoint, NSRect, NSString};

use crate::clipboard::{self, Source};
use crate::pasteboard_item::responds;
use crate::pasteboard_types::{self as types, FILE_URL, FILENAMES, Found, OLD_URL, URL};
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
    /// The MIME type taken (none: refused), the Wayland actions, and
    /// whether the destination wants periodic updates.
    Status { mime: Option<String>, actions: u32, preferred: u32, periodic: bool },
    /// The drop is over: `performed` if the destination took the data.
    Finish { performed: bool },
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    static SEQUENCE: Cell<isize> = const { Cell::new(0) };
    /// Replies kept for the testing hooks rather than sent, with the drags
    /// they name.
    static CAPTURED: RefCell<Option<Vec<(u64, Reply)>>> = const { RefCell::new(None) };
}

/// A drag over one of our windows.
struct Session {
    /// The render thread's name for the drag.
    drag: u64,
    window: Retained<NSWindow>,
    /// Pasteboard types the drag's pasteboard has, in its order, and its
    /// MIME types.
    kinds: Vec<String>,
    mimes: Vec<String>,
    /// The view or window under the drag that takes it.
    dest: Option<Retained<AnyObject>>,
    /// What the destination last answered.
    last: NSDragOperation,
    /// Whether the destination wants periodic updates.
    periodic: bool,
    info: Retained<DraggingInfo>,
}

fn reply(drag: u64, reply: Reply) {
    let kept = CAPTURED.with(|c| c.borrow_mut().as_mut().map(|replies| replies.push((drag, reply.clone()))).is_some());
    if kept {
        return;
    }
    crate::app::send_if_running(match reply {
        Reply::Status { mime, actions, preferred, periodic } => {
            ToRender::DndStatus { drag, mime, actions, preferred, periodic }
        }
        Reply::Finish { performed } => ToRender::DndFinish { drag, performed },
    });
}

/// Run `f` on drag `drag`'s session, if that's the drag under way.
fn with_session<R>(drag: u64, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|s| s.borrow_mut().as_mut().filter(|s| s.drag == drag).map(f))
}

/// Drag `drag` entered `window` at `at` (points from the content's top
/// left), offering `mimes`, with the source allowing Wayland `actions`, and
/// its URL list (MIME type and data) if it has one; a window that's gone
/// refuses it.
pub(crate) fn enter(
    drag: u64,
    window: Option<&NSWindow>,
    at: (f64, f64),
    mimes: Vec<String>,
    actions: u32,
    urls: Option<(String, Arc<[u8]>)>,
) {
    // A drag that never said it left ends first.
    end_session(SESSION.with(|s| s.borrow_mut().take()));
    let Some(window) = window else {
        refuse(drag);
        return;
    };
    let urls_listed: Vec<String> = urls.as_ref().map(|(mime, data)| types::parse_urls(mime, data)).unwrap_or_default();
    clipboard::shared_for(Source::Drag).drag_offered(drag, mimes.clone(), urls);
    let seq = SEQUENCE.with(|s| {
        s.set(s.get() + 1);
        s.get()
    });
    let mtm = MainThreadMarker::from(window);
    let info = DraggingInfo::new(mtm, window, seq, from_wayland(actions));
    info.ivars().location.set(location(window, at.0, at.1));
    let session = Session {
        drag,
        window: window.retain(),
        kinds: kinds(&mimes, &urls_listed),
        mimes,
        dest: None,
        last: NSDragOperation::None,
        periodic: false,
        info,
    };
    SESSION.with(|s| *s.borrow_mut() = Some(session));
    update(drag);
}

/// The types the drag pasteboard has for a drag offering `mimes` and URLs
/// `urls`, in the pasteboard's order (see `pasteboard::foreign_items`).
fn kinds(mimes: &[String], urls: &[String]) -> Vec<String> {
    let url_kinds = urls.iter().map(|u| Cow::Borrowed(types::url_kind(u)));
    let mime_kinds = mimes.iter().filter_map(|m| types::type_for_mime(m));
    let mut kinds: Vec<String> = Vec::new();
    // The first URL's item has the offer's other types too.
    for kind in url_kinds.clone().take(1).chain(mime_kinds).chain(url_kinds.skip(1)) {
        if !kinds.iter().any(|k| *k == kind) {
            kinds.push(kind.into_owned());
        }
    }
    kinds
}

/// Drag `drag` moved to `x`, `y` over the window it entered.
pub(crate) fn motion(drag: u64, x: f64, y: f64) {
    let Some((window, info)) = with_session(drag, |s| (s.window.clone(), s.info.clone())) else {
        refuse(drag);
        return;
    };
    info.ivars().location.set(location(&window, x, y));
    update(drag);
}

/// The source's allowed actions changed (a modifier key, say).
pub(crate) fn actions(drag: u64, source: u32) {
    let Some(info) = with_session(drag, |s| s.info.clone()) else {
        refuse(drag);
        return;
    };
    info.ivars().mask.set(from_wayland(source));
    update(drag);
}

/// The drag is still where it was: update the destination, if it wants
/// periodic updates (the render thread sends these only then, but it may
/// have changed its mind); else its last answer stands.
pub(crate) fn tick(drag: u64) {
    match with_session(drag, |s| s.periodic) {
        None => refuse(drag),
        Some(true) => update(drag),
        Some(false) => reply_status(drag),
    }
}

/// Answer a message about a drag there's no destination for: the render
/// thread counts on an answer to each.
fn refuse(drag: u64) {
    reply(drag, Reply::Status { mime: None, actions: 0, preferred: 0, periodic: false });
}

/// Drag `drag` left without a drop.
pub(crate) fn leave(drag: u64) {
    end_session(SESSION.with(|s| s.borrow_mut().take_if(|s| s.drag == drag)));
}

/// A session is over without a drop: its destination hears the drag left.
fn end_session(session: Option<Session>) {
    let Some(session) = session else { return };
    if let Some(dest) = &session.dest {
        // SAFETY: draggingExited: takes the dragging info (or nil).
        let _: () = unsafe { msg_send![&**dest, draggingExited: &*session.info] };
    }
}

/// Drag `drag` was dropped where it last was.
pub(crate) fn dropped(drag: u64) {
    // The drop ends the drag: its session goes first, so a drag that comes
    // while the destination handles the drop (in a nested event loop) is
    // one of its own, and doesn't end this one.
    let Some(session) = SESSION.with(|s| s.borrow_mut().take_if(|s| s.drag == drag)) else {
        reply(drag, Reply::Finish { performed: false });
        return;
    };
    let info = &session.info;
    let op = session.last & info.ivars().mask.get();
    let performed = match &session.dest {
        Some(dest) if op != NSDragOperation::None => {
            // SAFETY: the dragging destination methods take the dragging
            // info; the first two return BOOL.
            let prepared: bool = unsafe { msg_send![&**dest, prepareForDragOperation: &**info] };
            let performed = prepared && unsafe { msg_send![&**dest, performDragOperation: &**info] };
            if performed {
                // SAFETY: as above.
                let _: () = unsafe { msg_send![&**dest, concludeDragOperation: &**info] };
            }
            if responds(dest, sel!(draggingEnded:)) {
                // SAFETY: as above.
                let _: () = unsafe { msg_send![&**dest, draggingEnded: &**info] };
            }
            performed
        }
        Some(dest) => {
            // SAFETY: as above.
            let _: () = unsafe { msg_send![&**dest, draggingExited: &**info] };
            false
        }
        None => false,
    };
    drop(session);
    reply(drag, Reply::Finish { performed });
}

/// Find drag `drag`'s destination, tell it (and the old one) what happened,
/// and answer the render thread.
fn update(drag: u64) {
    let Some((window, kinds, old, info)) =
        with_session(drag, |s| (s.window.clone(), s.kinds.clone(), s.dest.clone(), s.info.clone()))
    else {
        refuse(drag);
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
    with_session(drag, |s| s.dest = dest.clone());
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
    // A new destination is asked once whether it wants periodic updates;
    // with none, there's nothing to update.
    let periodic = (!same).then(|| dest.as_ref().is_some_and(|d| wants_periodic_updates(d)));
    // Into this drag's session only: in a nested loop, the destination may
    // have seen it end, or another begin.
    with_session(drag, |s| {
        s.last = op;
        if let Some(periodic) = periodic {
            s.periodic = periodic;
        }
    });
    reply_status(drag);
}

/// Whether `dest` wants `draggingUpdated:` while the drag waits: yes,
/// unless it answers `wantsPeriodicDraggingUpdates`.
fn wants_periodic_updates(dest: &AnyObject) -> bool {
    // SAFETY: wantsPeriodicDraggingUpdates takes nothing and returns BOOL.
    !responds(dest, sel!(wantsPeriodicDraggingUpdates)) || unsafe { msg_send![dest, wantsPeriodicDraggingUpdates] }
}

/// Tell the render thread drag `drag`'s destination's last answer, masked
/// with what the source allows, and the MIME type it takes: a refusal if
/// the drag is gone.
fn reply_status(drag: u64) {
    let answer = with_session(drag, |s| {
        let op = s.last & s.info.ivars().mask.get();
        let mime = s.dest.as_ref().and_then(|dest| accepted_mime(dest, &s.kinds, &s.mimes));
        (op, mime, s.periodic)
    });
    let Some((op, mime, periodic)) = answer else {
        refuse(drag);
        return;
    };
    let (actions, preferred) = to_wayland(op);
    let mime = mime.filter(|_| actions != 0);
    reply(drag, Reply::Status { mime, actions, preferred, periodic });
}

/// The deepest view under `location` (window coordinates) that registered
/// a type the drag has (`kinds`), or one of its superviews, else the window
/// if it did.
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

/// Whether `object` registered a type the drag has.
fn takes(object: &AnyObject, kinds: &[String]) -> bool {
    drop_types(object).is_some_and(|types| types.iter().any(|t| types::find(kinds, &types::from_ns(&t)).is_some()))
}

/// The types `object` takes: those it registered, or a text view's own
/// (see `textkit::drop`).
fn drop_types(object: &AnyObject) -> Option<Retained<NSArray<NSString>>> {
    registered(object).or_else(|| crate::textkit::drop::drop_types(object))
}

/// The MIME type to accept for `dest`: that of the first type it registered
/// that the drag has.
fn accepted_mime(dest: &AnyObject, kinds: &[String], mimes: &[String]) -> Option<String> {
    let registered = drop_types(dest)?;
    let kind = registered.iter().find_map(|t| {
        let wanted = types::from_ns(&t);
        Some(match types::find(kinds, &wanted)? {
            Found::Itself => wanted,
            Found::Kind(kind) => kind.to_owned(),
        })
    })?;
    if matches!(kind.as_str(), FILE_URL | URL | FILENAMES | OLD_URL) {
        return types::url_mime(mimes).map(str::to_owned);
    }
    mimes.iter().find(|m| types::type_for_mime(m).is_some_and(|k| k == kind)).cloned()
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

// The methods views and windows get, through categories.

define_class!(
    // Holds NSView's drag destination methods, which the `SidestepDragging`
    // category adds to NSView. `self` is an NSView there.
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
            last_answer()
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
    // Holds NSWindow's drag destination methods, which the
    // `SidestepDragging` category adds to NSWindow. `self` is an NSWindow
    // there. A window has every destination method, passing each on to its
    // delegate if it has it, as on macOS.
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

        #[unsafe(method_id(registeredDraggedTypes))]
        fn registered_dragged_types(&self) -> Retained<NSArray<NSString>> {
            registered(object(self)).unwrap_or_default()
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &AnyObject) -> NSDragOperation {
            delegated(self, sel!(draggingEntered:), sender).unwrap_or(NSDragOperation::None)
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, sender: &AnyObject) -> NSDragOperation {
            delegated(self, sel!(draggingUpdated:), sender).unwrap_or_else(last_answer)
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

        #[unsafe(method(draggingEnded:))]
        fn dragging_ended(&self, sender: &AnyObject) {
            if let Some(delegate) = delegate_with(self, sel!(draggingEnded:)) {
                // SAFETY: draggingEnded: takes the info.
                let _: () = unsafe { msg_send![&*delegate, draggingEnded: sender] };
            }
        }

        /// NO unless the delegate says otherwise, as on macOS.
        #[unsafe(method(wantsPeriodicDraggingUpdates))]
        fn wants_periodic_dragging_updates(&self) -> bool {
            match delegate_with(self, sel!(wantsPeriodicDraggingUpdates)) {
                // SAFETY: wantsPeriodicDraggingUpdates takes nothing and
                // returns BOOL.
                Some(delegate) => unsafe { msg_send![&*delegate, wantsPeriodicDraggingUpdates] },
                None => false,
            }
        }

        #[unsafe(method(updateDraggingItemsForDrag:))]
        fn update_dragging_items_for_drag(&self, sender: Option<&AnyObject>) {
            if let Some(delegate) = delegate_with(self, sel!(updateDraggingItemsForDrag:)) {
                // SAFETY: updateDraggingItemsForDrag: takes the info or nil.
                let _: () = unsafe { msg_send![&*delegate, updateDraggingItemsForDrag: sender] };
            }
        }
    }
);

/// What the destination of the drag under way last answered.
fn last_answer() -> NSDragOperation {
    SESSION.with(|s| s.borrow().as_ref().map_or(NSDragOperation::None, |s| s.last))
}

/// A category method's receiver, which is an instance of the class the
/// category adds the method to, not of the helper.
fn object<T>(this: &T) -> &AnyObject {
    // SAFETY: every receiver is an object.
    unsafe { &*(this as *const T).cast::<AnyObject>() }
}

/// The window's delegate, if it has `selector`. `this` is an NSWindow.
fn delegate_with(this: &WindowDragging, selector: Sel) -> Option<Retained<AnyObject>> {
    // SAFETY: the category adds the method to NSWindow, so `this` is a
    // window, whose delegate is an object or nil.
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

// NSView's drag destination methods.
sidestep_runtime::category!("NSView"(SidestepDragging), |category| {
    // SAFETY: the helper's methods treat their receiver as an NSView.
    unsafe { category.add_methods_of(ViewDragging::class()) };
});

// NSWindow's drag destination methods.
sidestep_runtime::category!("NSWindow"(SidestepDragging), |category| {
    // SAFETY: the helper's methods treat their receiver as an NSWindow.
    unsafe { category.add_methods_of(WindowDragging::class()) };
});

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

        /// The drag's items that can be read as one of `classes`, each as a
        /// dragging item holding the object read (see
        /// `readObjectsForClasses:options:`), with the index of its
        /// pasteboard item. Wayland shows the source's image itself, so
        /// the items have no frames or images.
        #[unsafe(method(enumerateDraggingItemsWithOptions:forView:classes:searchOptions:usingBlock:))]
        fn enumerate_dragging_items(
            &self,
            _options: NSDraggingItemEnumerationOptions,
            _view: Option<&NSView>,
            classes: &NSArray<AnyClass>,
            search: &NSDictionary<NSString, AnyObject>,
            block: &DynBlock<dyn Fn(NonNull<NSDraggingItem>, isize, NonNull<Bool>)>,
        ) {
            let board = crate::pasteboard::named(crate::pasteboard::DRAG);
            let search: &AnyObject = search.as_ref();
            for (index, object) in crate::pasteboard::objects_by_item(&board, classes, Some(search)) {
                let item = DraggingItem::holding(Some(object));
                let mut stop = Bool::NO;
                block.call((NonNull::from(&*item), index as isize, NonNull::from(&mut stop)));
                if stop.as_bool() {
                    break;
                }
            }
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

// Dragging items, which `enumerateDraggingItems…` hands out.

sidestep_runtime::static_class!(pub NSDRAGGINGITEM, NSDRAGGINGITEM_META = "NSDraggingItem", || {
    let _ = DraggingItem::class();
});

pub(crate) struct ItemIvars {
    /// What's dragged: a pasteboard writer for a drag source, the object
    /// read for a destination.
    item: Option<Retained<AnyObject>>,
    frame: Cell<NSRect>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSDraggingItem"]
    #[ivars = ItemIvars]
    pub(crate) struct DraggingItem;

    impl DraggingItem {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ItemIvars { item: None, frame: Cell::new(NSRect::ZERO) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithPasteboardWriter:))]
        fn init_with_pasteboard_writer(this: Allocated<Self>, writer: &AnyObject) -> Retained<Self> {
            let this = this.set_ivars(ItemIvars { item: Some(writer.retain()), frame: Cell::new(NSRect::ZERO) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(item))]
        fn item(&self) -> Option<Retained<AnyObject>> {
            self.ivars().item.clone()
        }

        #[unsafe(method(draggingFrame))]
        fn dragging_frame(&self) -> NSRect {
            self.ivars().frame.get()
        }

        #[unsafe(method(setDraggingFrame:))]
        fn set_dragging_frame(&self, frame: NSRect) {
            self.ivars().frame.set(frame);
        }

        // Images are the source's to show, which Wayland does itself.
        #[unsafe(method(setDraggingFrame:contents:))]
        fn set_dragging_frame_contents(&self, frame: NSRect, _contents: Option<&AnyObject>) {
            self.ivars().frame.set(frame);
        }

        #[unsafe(method_id(imageComponents))]
        fn image_components(&self) -> Option<Retained<AnyObject>> {
            None
        }
    }

    unsafe impl NSObjectProtocol for DraggingItem {}
);

impl DraggingItem {
    /// An item holding `object`, as a dragging item.
    fn holding(object: Option<Retained<AnyObject>>) -> Retained<NSDraggingItem> {
        crate::load_shell::<NSDraggingItem>();
        let this = Self::alloc().set_ivars(ItemIvars { item: object, frame: Cell::new(NSRect::ZERO) });
        // SAFETY: NSObject's designated initializer.
        let item: Retained<Self> = unsafe { msg_send![super(this), init] };
        // SAFETY: DraggingItem is the class NSDraggingItem names.
        unsafe { Retained::cast_unchecked(item) }
    }
}

/// Hooks for tests of the drag state without a compositor: they drive the
/// same functions the render thread's messages do, and collect the replies
/// that would go back to it. Each drag entered gets a new name, which the
/// other hooks use.
#[doc(hidden)]
pub mod testing {
    use std::cell::Cell;
    use std::sync::Arc;

    use objc2_app_kit::NSWindow;

    pub use super::{DND_ASK, DND_COPY, DND_MOVE, Reply};

    thread_local!(static DRAG: Cell<u64> = const { Cell::new(0) });

    /// Keep replies for `take_replies` instead of sending them.
    pub fn capture_replies() {
        super::CAPTURED.with(|c| *c.borrow_mut() = Some(Vec::new()));
    }

    /// The replies since the last call, with the drags they name.
    pub fn take_answers() -> Vec<(u64, Reply)> {
        super::CAPTURED.with(|c| c.borrow_mut().as_mut().map(std::mem::take).unwrap_or_default())
    }

    /// The replies since the last call, checked to be about the drag under
    /// way.
    pub fn take_replies() -> Vec<Reply> {
        let drag = current();
        take_answers()
            .into_iter()
            .map(|(about, reply)| {
                assert_eq!(about, drag, "{reply:?} is about another drag");
                reply
            })
            .collect()
    }

    /// The drag the other hooks are about: the one entered last.
    pub fn current() -> u64 {
        DRAG.with(Cell::get)
    }

    /// A drag enters `window` at `x`, `y` (points from the content's top
    /// left) offering `mimes`, the source allowing Wayland `actions`.
    pub fn enter(window: &NSWindow, x: f64, y: f64, mimes: &[&str], actions: u32) {
        enter_with_urls(window, x, y, mimes, actions, None);
    }

    /// As `enter`, for a drag offering the URL list `urls` (text/uri-list).
    pub fn enter_with_urls(window: &NSWindow, x: f64, y: f64, mimes: &[&str], actions: u32, urls: Option<&str>) {
        let drag = DRAG.with(|d| {
            d.set(d.get() + 1);
            d.get()
        });
        let urls = urls.map(|u| ("text/uri-list".to_owned(), Arc::from(u.as_bytes())));
        super::enter(drag, Some(window), (x, y), mimes.iter().map(|m| m.to_string()).collect(), actions, urls);
    }

    pub fn motion(x: f64, y: f64) {
        super::motion(current(), x, y);
    }

    /// A move of a drag that isn't the one under way.
    pub fn motion_of(drag: u64, x: f64, y: f64) {
        super::motion(drag, x, y);
    }

    pub fn source_actions(actions: u32) {
        super::actions(current(), actions);
    }

    /// A periodic update, the drag not having moved.
    pub fn tick() {
        super::tick(current());
    }

    pub fn leave() {
        super::leave(current());
    }

    pub fn drop() {
        super::dropped(current());
    }

    /// The drop of a drag that isn't the one under way (one entered earlier).
    pub fn drop_of(drag: u64) {
        super::dropped(drag);
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
