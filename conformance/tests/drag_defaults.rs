//! Drag and drop destinations before any drag: which dragging methods
//! views and windows have, what NSView's defaults answer, and
//! `registerForDraggedTypes:`. A plain object stands in for the dragging
//! info, logging what it's asked. (Drags themselves are checked on Linux,
//! in sidestep-appkit's `dnd_state` test.)
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSPasteboard, NSPasteboardTypeFileURL,
    NSPasteboardTypePNG, NSPasteboardTypeString, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

thread_local!(static ASKED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) });

fn asked(what: &'static str) {
    ASKED.with(|a| a.borrow_mut().push(what));
}

fn take_asked() -> Vec<&'static str> {
    ASKED.with(|a| std::mem::take(&mut *a.borrow_mut()))
}

define_class!(
    // Answers the dragging info's questions, and notes each.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceDraggingInfo"]
    struct StandIn;

    unsafe impl NSObjectProtocol for StandIn {}

    impl StandIn {
        #[unsafe(method(draggingSourceOperationMask))]
        fn mask(&self) -> NSDragOperation {
            asked("draggingSourceOperationMask");
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingLocation))]
        fn location(&self) -> NSPoint {
            asked("draggingLocation");
            NSPoint::new(1.0, 1.0)
        }

        #[unsafe(method_id(draggingPasteboard))]
        fn pasteboard(&self) -> Retained<NSPasteboard> {
            asked("draggingPasteboard");
            NSPasteboard::pasteboardWithUniqueName()
        }

        #[unsafe(method(draggingSequenceNumber))]
        fn sequence(&self) -> isize {
            asked("draggingSequenceNumber");
            1
        }
    }
);

fn stand_in(mtm: MainThreadMarker) -> Retained<StandIn> {
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(StandIn::alloc(mtm).set_ivars(())), init] }
}

fn as_info(object: &StandIn) -> &ProtocolObject<dyn NSDraggingInfo> {
    // SAFETY: the stand-in answers the methods the defaults could ask.
    unsafe { &*(object as *const StandIn).cast::<ProtocolObject<dyn NSDraggingInfo>>() }
}

fn view(mtm: MainThreadMarker) -> Retained<NSView> {
    NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(10.0, 10.0)))
}

fn responds(object: &AnyObject, sel: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: sel] }
}

fn strings(array: &NSArray<NSString>) -> Vec<String> {
    array.iter().map(|s| s.to_string()).collect()
}

fn views_and_windows_are_destinations(mtm: MainThreadMarker) {
    let v = view(mtm);
    for sel in [
        sel!(draggingEntered:),
        sel!(draggingUpdated:),
        sel!(draggingExited:),
        sel!(prepareForDragOperation:),
        sel!(performDragOperation:),
        sel!(concludeDragOperation:),
        sel!(registerForDraggedTypes:),
        sel!(unregisterDraggedTypes),
        sel!(registeredDraggedTypes),
    ] {
        assert!(responds(&v, sel), "NSView {sel}");
    }
    for sel in [sel!(draggingEnded:), sel!(wantsPeriodicDraggingUpdates)] {
        assert!(!responds(&v, sel), "NSView {sel}");
    }
    // SAFETY: a titled window, not shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 100.0)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: the window isn't shown; the test keeps its reference.
    unsafe { w.setReleasedWhenClosed(false) };
    // A window has every destination method, passing them to its delegate.
    for sel in [
        sel!(draggingEntered:),
        sel!(draggingUpdated:),
        sel!(draggingExited:),
        sel!(prepareForDragOperation:),
        sel!(performDragOperation:),
        sel!(concludeDragOperation:),
        sel!(draggingEnded:),
        sel!(wantsPeriodicDraggingUpdates),
        sel!(updateDraggingItemsForDrag:),
        sel!(registerForDraggedTypes:),
        sel!(unregisterDraggedTypes),
        sel!(registeredDraggedTypes),
    ] {
        assert!(responds(&w, sel), "NSWindow {sel}");
    }
    // SAFETY: the constant lives as long as the program.
    let string = unsafe { NSPasteboardTypeString };
    w.registerForDraggedTypes(&NSArray::from_slice(&[string, string]));
    // SAFETY: registeredDraggedTypes takes nothing and returns an array of
    // types.
    let registered: Retained<NSArray<NSString>> = unsafe { msg_send![&*w, registeredDraggedTypes] };
    assert_eq!(strings(&registered), ["public.utf8-plain-text"]);
    w.unregisterDraggedTypes();
    // SAFETY: as above.
    let registered: Retained<NSArray<NSString>> = unsafe { msg_send![&*w, registeredDraggedTypes] };
    assert!(registered.is_empty());
}

define_class!(
    // A window delegate that takes periodic updates, and notes the end of a
    // drag.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceDragWindowDelegate"]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    impl WindowDelegate {
        #[unsafe(method(wantsPeriodicDraggingUpdates))]
        fn wants_periodic_dragging_updates(&self) -> bool {
            asked("wantsPeriodicDraggingUpdates");
            true
        }

        #[unsafe(method(draggingEnded:))]
        fn dragging_ended(&self, _sender: &AnyObject) {
            asked("draggingEnded");
        }

        #[unsafe(method(updateDraggingItemsForDrag:))]
        fn update_dragging_items_for_drag(&self, _sender: Option<&AnyObject>) {
            asked("updateDraggingItemsForDrag");
        }
    }
);

fn wants_periodic(object: &AnyObject) -> bool {
    // SAFETY: wantsPeriodicDraggingUpdates takes nothing and returns BOOL.
    unsafe { msg_send![object, wantsPeriodicDraggingUpdates] }
}

fn windows_pass_the_rest_to_their_delegate(mtm: MainThreadMarker) {
    // SAFETY: a titled window, not shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 100.0)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: the window isn't shown; the test keeps its reference.
    unsafe { w.setReleasedWhenClosed(false) };
    let sender = stand_in(mtm);
    take_asked();
    // Without a delegate that wants them, no periodic updates.
    assert!(!wants_periodic(&w));
    let plain = NSObject::new();
    // SAFETY: setDelegate: takes an object or nil, which the window doesn't
    // retain; the test keeps it.
    let _: () = unsafe { msg_send![&*w, setDelegate: &*plain] };
    assert!(!wants_periodic(&w));
    // SAFETY: the window's draggingEnded: and updateDraggingItemsForDrag:
    // take the dragging info.
    let _: () = unsafe { msg_send![&*w, draggingEnded: &*sender] };
    let _: () = unsafe { msg_send![&*w, updateDraggingItemsForDrag: &*sender] };
    // SAFETY: NSObject's designated initializer.
    let delegate: Retained<WindowDelegate> =
        unsafe { msg_send![super(WindowDelegate::alloc(mtm).set_ivars(())), init] };
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*w, setDelegate: &*delegate] };
    assert!(wants_periodic(&w));
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*w, draggingEnded: &*sender] };
    let _: () = unsafe { msg_send![&*w, updateDraggingItemsForDrag: &*sender] };
    assert_eq!(take_asked(), ["wantsPeriodicDraggingUpdates", "draggingEnded", "updateDraggingItemsForDrag"]);
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&*w, setDelegate: std::ptr::null::<AnyObject>()] };
}

fn view_defaults(mtm: MainThreadMarker) {
    let v = view(mtm);
    let sender = stand_in(mtm);
    take_asked();
    assert_eq!(v.draggingEntered(as_info(&sender)), NSDragOperation::None);
    assert!(v.prepareForDragOperation(as_info(&sender)));
    assert!(!v.performDragOperation(as_info(&sender)));
    v.concludeDragOperation(Some(as_info(&sender)));
    v.draggingExited(Some(as_info(&sender)));
    v.draggingExited(None);
    v.concludeDragOperation(None);
    // The defaults ask the dragging info nothing.
    assert_eq!(take_asked(), Vec::<&str>::new());
}

fn registered_types_are_an_ordered_set(mtm: MainThreadMarker) {
    let v = view(mtm);
    assert!(v.registeredDraggedTypes().is_empty());
    // SAFETY: the constants live as long as the program.
    let (file_url, string, png) = unsafe { (NSPasteboardTypeFileURL, NSPasteboardTypeString, NSPasteboardTypePNG) };
    v.registerForDraggedTypes(&NSArray::from_slice(&[file_url, string, file_url]));
    assert_eq!(strings(&v.registeredDraggedTypes()), ["public.file-url", "public.utf8-plain-text"]);
    v.registerForDraggedTypes(&NSArray::from_slice(&[png, string]));
    assert_eq!(strings(&v.registeredDraggedTypes()), ["public.file-url", "public.utf8-plain-text", "public.png"]);
    v.unregisterDraggedTypes();
    assert!(v.registeredDraggedTypes().is_empty());
    // Views keep their own.
    let other = view(mtm);
    v.registerForDraggedTypes(&NSArray::from_slice(&[png]));
    assert!(other.registeredDraggedTypes().is_empty());
}

define_class!(
    // A destination that overrides only draggingEntered:, and calls super
    // for the rest, as apps do.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceDropView"]
    struct DropView;

    unsafe impl NSObjectProtocol for DropView {}

    unsafe impl NSDraggingDestination for DropView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            asked("entered");
            // SAFETY: NSView's default takes the dragging info.
            let _: NSDragOperation = unsafe { msg_send![super(self), draggingEntered: sender] };
            NSDragOperation::Copy
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            asked("perform");
            // SAFETY: as above.
            unsafe { msg_send![super(self), performDragOperation: sender] }
        }
    }
);

fn subclasses_reach_the_defaults(mtm: MainThreadMarker) {
    // SAFETY: NSView's designated initializer.
    let v: Retained<DropView> = unsafe {
        msg_send![super(DropView::alloc(mtm).set_ivars(())), initWithFrame: NSRect::new(NSPoint::ZERO, NSSize::new(4.0, 4.0))]
    };
    let sender = stand_in(mtm);
    take_asked();
    assert_eq!(v.draggingEntered(as_info(&sender)), NSDragOperation::Copy);
    assert!(!v.performDragOperation(as_info(&sender)));
    assert!(v.prepareForDragOperation(as_info(&sender)));
    assert_eq!(take_asked(), ["entered", "perform"]);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("views_and_windows_are_destinations", views_and_windows_are_destinations),
        ("view_defaults", view_defaults),
        ("registered_types_are_an_ordered_set", registered_types_are_an_ordered_set),
        ("subclasses_reach_the_defaults", subclasses_reach_the_defaults),
        ("windows_pass_the_rest_to_their_delegate", windows_pass_the_rest_to_their_delegate),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
