//! Drag and drop destinations on Linux, driven without a compositor: the
//! testing hooks feed `drag` what the render thread's messages would (a
//! drag entering, moving, changing its actions, leaving, dropping) and
//! collect the answers it would send back. Checks which view or window
//! takes a drag, the messages each gets and in what order, the MIME type
//! and Wayland actions answered, and when a drop is finished.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::{Cell, RefCell};

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, ProtocolObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSBackingStoreType, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSPasteboardTypePNG,
        NSPasteboardTypeString, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    };
    use objc2_foundation::{NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize};
    use sidestep_appkit::drag::testing::{self, DND_ASK, DND_COPY, DND_MOVE, Reply};

    thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

    fn log(line: String) {
        LOG.with(|l| l.borrow_mut().push(line));
    }

    fn take_log() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    pub(crate) struct Behavior {
        name: &'static str,
        answer: Cell<NSDragOperation>,
        performs: Cell<bool>,
    }

    define_class!(
        // A destination that notes what it's sent. It doesn't override
        // draggingUpdated:, so NSView's default answers those.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "DndStateDestination"]
        #[ivars = Behavior]
        struct Destination;

        unsafe impl NSObjectProtocol for Destination {}

        unsafe impl NSDraggingDestination for Destination {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                let at = sender.draggingLocation();
                log(format!("{}.entered at {},{}", self.ivars().name, at.x, at.y));
                self.ivars().answer.get()
            }

            #[unsafe(method(draggingExited:))]
            fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                log(format!("{}.exited", self.ivars().name));
            }

            #[unsafe(method(prepareForDragOperation:))]
            fn prepare(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                log(format!("{}.prepare", self.ivars().name));
                true
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                log(format!("{}.perform", self.ivars().name));
                self.ivars().performs.get()
            }

            #[unsafe(method(concludeDragOperation:))]
            fn conclude(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                log(format!("{}.conclude", self.ivars().name));
            }

            #[unsafe(method(draggingEnded:))]
            fn ended(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) {
                log(format!("{}.ended", self.ivars().name));
            }
        }
    );

    define_class!(
        // A window delegate that takes drags over its window.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "DndStateWindowDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}
        unsafe impl NSWindowDelegate for Delegate {}

        unsafe impl NSDraggingDestination for Delegate {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                log("delegate.entered".into());
                NSDragOperation::Copy
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                log("delegate.perform".into());
                true
            }
        }
    );

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn destination(mtm: MainThreadMarker, name: &'static str, frame: NSRect) -> Retained<Destination> {
        let behavior = Behavior { name, answer: Cell::new(NSDragOperation::Copy), performs: Cell::new(true) };
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(Destination::alloc(mtm).set_ivars(behavior)), initWithFrame: frame] }
    }

    struct Scene {
        window: Retained<NSWindow>,
        /// Takes text, on the left; `inner` is a plain view inside it.
        a: Retained<Destination>,
        /// Takes PNG, on the right.
        _b: Retained<Destination>,
    }

    /// A 200 by 100 window: `a` on the left half with a plain view in its
    /// middle, `b` on the right half.
    fn scene(mtm: MainThreadMarker) -> Scene {
        // SAFETY: a titled window, never shown.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 200.0, 100.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                true,
            )
        };
        // SAFETY: the test keeps its reference.
        unsafe { window.setReleasedWhenClosed(false) };
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
        window.setContentView(Some(&content));
        let a = destination(mtm, "a", rect(0.0, 0.0, 100.0, 100.0));
        let inner = NSView::initWithFrame(NSView::alloc(mtm), rect(40.0, 40.0, 20.0, 20.0));
        a.addSubview(&inner);
        let b = destination(mtm, "b", rect(100.0, 0.0, 100.0, 100.0));
        content.addSubview(&a);
        content.addSubview(&b);
        // SAFETY: the constants live as long as the program.
        let (string, png) = unsafe { (NSPasteboardTypeString, NSPasteboardTypePNG) };
        a.registerForDraggedTypes(&NSArray::from_slice(&[string]));
        b.registerForDraggedTypes(&NSArray::from_slice(&[png]));
        Scene { window, a, _b: b }
    }

    const TEXT: &str = "text/plain;charset=utf-8";

    fn status(mime: Option<&str>, actions: u32, preferred: u32) -> Reply {
        Reply::Status { mime: mime.map(str::to_owned), actions, preferred }
    }

    fn destinations_follow_the_drag(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // Points from the content's top left: (50, 50) is inside `inner`,
        // which `a` holds.
        testing::enter(&s.window, 50.0, 50.0, &[TEXT, "text/html"], DND_COPY | DND_MOVE);
        assert_eq!(take_log(), ["a.entered at 50,50"]);
        assert_eq!(testing::take_replies(), [status(Some(TEXT), DND_COPY, DND_COPY)]);
        // Within `a`: NSView's draggingUpdated: answers what draggingEntered:
        // did.
        testing::motion(5.0, 10.0);
        assert!(take_log().is_empty());
        assert_eq!(testing::take_replies(), [status(Some(TEXT), DND_COPY, DND_COPY)]);
        // Over `b`, which takes only PNG: nobody takes the drag.
        testing::motion(150.0, 50.0);
        assert_eq!(take_log(), ["a.exited"]);
        assert_eq!(testing::take_replies(), [status(None, 0, 0)]);
        // Back over `a`, which is entered again.
        testing::motion(20.0, 30.0);
        assert_eq!(take_log(), ["a.entered at 20,70"]);
        testing::take_replies();
        testing::leave();
        assert_eq!(take_log(), ["a.exited"]);
        assert!(testing::take_replies().is_empty());
        // A PNG drag goes to `b` alone.
        testing::enter(&s.window, 150.0, 50.0, &["image/png"], DND_COPY);
        assert_eq!(take_log(), ["b.entered at 150,50"]);
        assert_eq!(testing::take_replies(), [status(Some("image/png"), DND_COPY, DND_COPY)]);
        testing::motion(50.0, 50.0);
        assert_eq!(take_log(), ["b.exited"]);
        testing::leave();
        take_log();
        testing::take_replies();
    }

    fn drops_run_in_order(mtm: MainThreadMarker) {
        let s = scene(mtm);
        testing::enter(&s.window, 50.0, 50.0, &[TEXT], DND_COPY);
        testing::drop();
        assert_eq!(take_log(), ["a.entered at 50,50", "a.prepare", "a.perform", "a.conclude", "a.ended"]);
        assert_eq!(testing::take_replies().last(), Some(&Reply::Finish { performed: true }));
        // A destination that doesn't perform isn't concluded, and the drop
        // isn't finished.
        s.a.ivars().performs.set(false);
        testing::enter(&s.window, 50.0, 50.0, &[TEXT], DND_COPY);
        testing::drop();
        assert_eq!(take_log(), ["a.entered at 50,50", "a.prepare", "a.perform", "a.ended"]);
        assert_eq!(testing::take_replies().last(), Some(&Reply::Finish { performed: false }));
        // One that answered no operation is exited instead.
        s.a.ivars().answer.set(NSDragOperation::None);
        testing::enter(&s.window, 50.0, 50.0, &[TEXT], DND_COPY);
        assert_eq!(testing::take_replies(), [status(None, 0, 0)]);
        testing::drop();
        assert_eq!(take_log(), ["a.entered at 50,50", "a.exited"]);
        assert_eq!(testing::take_replies(), [Reply::Finish { performed: false }]);
        // A drop over nobody is refused.
        testing::enter(&s.window, 150.0, 50.0, &[TEXT], DND_COPY);
        testing::drop();
        assert!(take_log().is_empty());
        assert_eq!(testing::take_replies().last(), Some(&Reply::Finish { performed: false }));
    }

    fn operations_are_masked_and_mapped(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // The source only moves; `a` only copies.
        testing::enter(&s.window, 50.0, 50.0, &[TEXT], DND_MOVE);
        assert_eq!(testing::take_replies(), [status(None, 0, 0)]);
        testing::drop();
        assert_eq!(take_log(), ["a.entered at 50,50", "a.exited"]);
        assert_eq!(testing::take_replies(), [Reply::Finish { performed: false }]);
        // Generic is a move, and a source that asks allows it.
        s.a.ivars().answer.set(NSDragOperation::Generic);
        testing::enter(&s.window, 50.0, 50.0, &[TEXT], DND_ASK);
        assert_eq!(testing::take_replies(), [status(Some(TEXT), DND_MOVE, DND_MOVE)]);
        // The source's actions change (a modifier key): answered again.
        testing::source_actions(DND_COPY);
        assert_eq!(testing::take_replies(), [status(None, 0, 0)]);
        testing::source_actions(DND_COPY | DND_MOVE);
        assert_eq!(testing::take_replies(), [status(Some(TEXT), DND_MOVE, DND_MOVE)]);
        testing::leave();
        // Every operation, offered both: copy is preferred.
        s.a.ivars().answer.set(NSDragOperation::Every);
        testing::enter(&s.window, 50.0, 50.0, &[TEXT], DND_COPY | DND_MOVE);
        assert_eq!(testing::take_replies(), [status(Some(TEXT), DND_COPY | DND_MOVE, DND_COPY)]);
        testing::leave();
        take_log();
    }

    define_class!(
        // Checks what the dragging info says, when a drag enters.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "DndStateInspector"]
        struct Inspector;

        unsafe impl NSObjectProtocol for Inspector {}

        unsafe impl NSDraggingDestination for Inspector {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                let types: Vec<String> = sender
                    .draggingPasteboard()
                    .types()
                    .map(|t| t.iter().map(|t| t.to_string()).collect())
                    .unwrap_or_default();
                let window = sender.draggingDestinationWindow().is_some();
                log(format!(
                    "seq {} mask {:?} types {types:?} window {window} source {}",
                    sender.draggingSequenceNumber(),
                    sender.draggingSourceOperationMask(),
                    sender.draggingSource().is_some()
                ));
                NSDragOperation::Copy
            }
        }
    );

    fn dragging_info(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // SAFETY: NSView's designated initializer.
        let inspector: Retained<Inspector> = unsafe {
            msg_send![super(Inspector::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 200.0, 100.0)]
        };
        s.window.setContentView(Some(&inspector));
        // SAFETY: the constant lives as long as the program.
        inspector.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]));
        testing::enter(&s.window, 10.0, 10.0, &[TEXT], DND_COPY | DND_MOVE);
        let first = take_log();
        testing::leave();
        testing::enter(&s.window, 10.0, 10.0, &[TEXT], DND_COPY);
        let second = take_log();
        testing::leave();
        testing::take_replies();
        let seq = |line: &str| line.split(' ').nth(1).and_then(|n| n.parse::<isize>().ok()).expect("a number");
        assert_eq!(seq(&second[0]), seq(&first[0]) + 1, "{first:?} {second:?}");
        let copy_move = NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic;
        assert!(first[0].contains(&format!("mask {copy_move:?}")), "{first:?}");
        assert!(first[0].contains(r#"types ["public.utf8-plain-text", "NSStringPboardType"]"#), "{first:?}");
        assert!(first[0].ends_with("window true source false"), "{first:?}");
        assert!(second[0].contains(&format!("mask {:?}", NSDragOperation::Copy)), "{second:?}");
    }

    fn windows_pass_drags_to_their_delegate(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // SAFETY: NSObject's designated initializer.
        let delegate: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc(mtm).set_ivars(())), init] };
        s.window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // SAFETY: the constant lives as long as the program.
        s.window.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypePNG }]));
        // Over `a`, which doesn't take PNG: the window does.
        testing::enter(&s.window, 50.0, 50.0, &["image/png"], DND_COPY);
        assert_eq!(take_log(), ["delegate.entered"]);
        assert_eq!(testing::take_replies(), [status(Some("image/png"), DND_COPY, DND_COPY)]);
        testing::drop();
        assert_eq!(take_log(), ["delegate.perform"]);
        assert_eq!(testing::take_replies(), [Reply::Finish { performed: true }]);
        // Unregistered, it takes nothing.
        s.window.unregisterDraggedTypes();
        testing::enter(&s.window, 50.0, 50.0, &["image/png"], DND_COPY);
        assert_eq!(testing::take_replies(), [status(None, 0, 0)]);
        testing::leave();
        assert!(take_log().is_empty());
    }

    type Test = (&'static str, fn(MainThreadMarker));

    pub(crate) fn run() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::capture_replies();
        let tests: &[Test] = &[
            ("destinations_follow_the_drag", destinations_follow_the_drag),
            ("drops_run_in_order", drops_run_in_order),
            ("operations_are_masked_and_mapped", operations_are_masked_and_mapped),
            ("dragging_info", dragging_info),
            ("windows_pass_drags_to_their_delegate", windows_pass_drags_to_their_delegate),
        ];
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test(mtm));
            println!("test {name} ... ok");
        }
    }
}
