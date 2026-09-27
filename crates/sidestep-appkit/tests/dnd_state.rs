//! Drag and drop destinations on Linux, driven without a compositor: the
//! testing hooks feed `drag` what the render thread's messages would (a
//! drag entering, moving, changing its actions, leaving, dropping) and
//! collect the answers it would send back. Checks which view or window
//! takes a drag (by its URLs, too), the messages each gets and in what
//! order, the MIME type and Wayland actions answered, whether periodic
//! updates are wanted, when a drop is finished, and that a drag coming
//! while a drop is handled (a nested event loop) leaves the drop alone.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::{Cell, RefCell};
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, ProtocolObject};
    use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSBackingStoreType, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSDraggingItem,
        NSDraggingItemEnumerationOptions, NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardTypeString, NSView,
        NSWindow, NSWindowDelegate, NSWindowStyleMask,
    };
    use objc2_foundation::{NSArray, NSDictionary, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
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
        // A window delegate that takes drags over its window, and periodic
        // updates.
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

            #[unsafe(method(draggingEnded:))]
            fn ended(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) {
                log("delegate.ended".into());
            }

            #[unsafe(method(wantsPeriodicDraggingUpdates))]
            fn wants_periodic_dragging_updates(&self) -> bool {
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

    /// A view's answer: views want periodic updates.
    fn status(mime: Option<&str>, actions: u32, preferred: u32) -> Reply {
        Reply::Status { mime: mime.map(str::to_owned), actions, preferred, periodic: true }
    }

    /// No destination, or no drag.
    fn refused() -> Reply {
        Reply::Status { mime: None, actions: 0, preferred: 0, periodic: false }
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
        assert_eq!(testing::take_replies(), [refused()]);
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
                // The drag's items, as dragging items holding pasteboard items.
                let classes = NSArray::from_slice(&[NSPasteboardItem::class()]);
                let each = RcBlock::new(|item: NonNull<NSDraggingItem>, index: isize, _stop: NonNull<Bool>| {
                    // SAFETY: the dragging info hands the block a live item.
                    let object = unsafe { item.as_ref() }.item();
                    let types: Vec<String> = object
                        .downcast::<NSPasteboardItem>()
                        .map(|i| i.types().iter().map(|t| t.to_string()).collect())
                        .unwrap_or_default();
                    log(format!("item {index} {types:?}"));
                });
                // SAFETY: pasteboard items read from pasteboards, and the block
                // takes what the dragging info hands it.
                unsafe {
                    sender.enumerateDraggingItemsWithOptions_forView_classes_searchOptions_usingBlock(
                        NSDraggingItemEnumerationOptions::empty(),
                        None,
                        &classes,
                        &NSDictionary::new(),
                        &each,
                    )
                };
                log(format!("valid items {}", sender.numberOfValidItemsForDrop()));
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
        testing::take_answers();
        let seq = |line: &str| line.split(' ').nth(1).and_then(|n| n.parse::<isize>().ok()).expect("a number");
        assert_eq!(seq(&second[0]), seq(&first[0]) + 1, "{first:?} {second:?}");
        let copy_move = NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic;
        assert!(first[0].contains(&format!("mask {copy_move:?}")), "{first:?}");
        assert!(first[0].contains(r#"types ["public.utf8-plain-text", "NSStringPboardType"]"#), "{first:?}");
        assert!(first[0].ends_with("window true source false"), "{first:?}");
        assert!(second[0].contains(&format!("mask {:?}", NSDragOperation::Copy)), "{second:?}");
        assert_eq!(first[1..], [r#"item 0 ["public.utf8-plain-text"]"#, "valid items 1"], "{first:?}");
    }

    fn windows_pass_drags_to_their_delegate(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // SAFETY: NSObject's designated initializer.
        let delegate: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc(mtm).set_ivars(())), init] };
        s.window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // SAFETY: the constant lives as long as the program.
        s.window.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypePNG }]));
        // Over `a`, which doesn't take PNG: the window does, and its
        // delegate wants periodic updates.
        testing::enter(&s.window, 50.0, 50.0, &["image/png"], DND_COPY);
        assert_eq!(take_log(), ["delegate.entered"]);
        assert_eq!(testing::take_replies(), [status(Some("image/png"), DND_COPY, DND_COPY)]);
        testing::drop();
        assert_eq!(take_log(), ["delegate.perform", "delegate.ended"]);
        assert_eq!(testing::take_replies(), [Reply::Finish { performed: true }]);
        // Without a delegate, the window takes the drag, and wants no
        // periodic updates.
        s.window.setDelegate(None);
        testing::enter(&s.window, 50.0, 50.0, &["image/png"], DND_COPY);
        let none = Reply::Status { mime: None, actions: 0, preferred: 0, periodic: false };
        assert_eq!(testing::take_replies(), [none]);
        testing::leave();
        // Unregistered, it takes nothing.
        s.window.unregisterDraggedTypes();
        testing::enter(&s.window, 50.0, 50.0, &["image/png"], DND_COPY);
        assert_eq!(testing::take_replies(), [refused()]);
        testing::leave();
        assert!(take_log().is_empty());
        let _ = delegate;
    }

    define_class!(
        // Notes periodic updates; `quiet` ones say they don't want them.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "DndStateTicker"]
        struct Ticker;

        unsafe impl NSObjectProtocol for Ticker {}

        unsafe impl NSDraggingDestination for Ticker {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                NSDragOperation::Copy
            }

            #[unsafe(method(draggingUpdated:))]
            fn dragging_updated(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                log("ticker.updated".into());
                NSDragOperation::Copy
            }
        }
    );

    define_class!(
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "DndStateQuiet"]
        struct Quiet;

        unsafe impl NSObjectProtocol for Quiet {}

        unsafe impl NSDraggingDestination for Quiet {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                NSDragOperation::Copy
            }

            #[unsafe(method(draggingUpdated:))]
            fn dragging_updated(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                log("quiet.updated".into());
                NSDragOperation::Copy
            }

            #[unsafe(method(wantsPeriodicDraggingUpdates))]
            fn wants_periodic_dragging_updates(&self) -> bool {
                false
            }
        }
    );

    fn periodic_updates(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // SAFETY: the constant lives as long as the program.
        let string = unsafe { NSPasteboardTypeString };
        // SAFETY: NSView's designated initializer.
        let ticker: Retained<Ticker> =
            unsafe { msg_send![super(Ticker::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 200.0, 100.0)] };
        ticker.registerForDraggedTypes(&NSArray::from_slice(&[string]));
        s.window.setContentView(Some(&ticker));
        testing::enter(&s.window, 10.0, 10.0, &[TEXT], DND_COPY);
        testing::take_replies();
        // The drag waits: the destination is updated, and answers again.
        testing::tick();
        assert_eq!(take_log(), ["ticker.updated"]);
        assert_eq!(testing::take_replies(), [status(Some(TEXT), DND_COPY, DND_COPY)]);
        testing::leave();
        // One that doesn't want periodic updates isn't sent them, but its last
        // answer still goes back.
        // SAFETY: as above.
        let quiet: Retained<Quiet> =
            unsafe { msg_send![super(Quiet::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 200.0, 100.0)] };
        quiet.registerForDraggedTypes(&NSArray::from_slice(&[string]));
        s.window.setContentView(Some(&quiet));
        testing::enter(&s.window, 10.0, 10.0, &[TEXT], DND_COPY);
        let quiet_status =
            Reply::Status { mime: Some(TEXT.into()), actions: DND_COPY, preferred: DND_COPY, periodic: false };
        assert_eq!(testing::take_replies(), std::slice::from_ref(&quiet_status));
        testing::tick();
        assert!(take_log().is_empty());
        assert_eq!(testing::take_replies(), [quiet_status]);
        // Moving still updates it.
        testing::motion(20.0, 20.0);
        assert_eq!(take_log(), ["quiet.updated"]);
        testing::take_replies();
        testing::leave();
        // Messages about a drag that has gone are answered with a refusal.
        testing::tick();
        testing::motion(1.0, 1.0);
        testing::source_actions(DND_COPY);
        assert_eq!(testing::take_replies(), [refused(), refused(), refused()]);
        take_log();
    }

    define_class!(
        // Takes the types it's made with, and notes what it's sent.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "DndStateUrlView"]
        #[ivars = &'static str]
        struct UrlView;

        unsafe impl NSObjectProtocol for UrlView {}

        unsafe impl NSDraggingDestination for UrlView {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                log(format!("{}.entered", self.ivars()));
                NSDragOperation::Copy
            }
        }
    );

    fn url_drags_go_by_what_the_urls_are(mtm: MainThreadMarker) {
        let s = scene(mtm);
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
        s.window.setContentView(Some(&content));
        // Three views side by side: files, file names (their old type), and
        // any URL.
        for (i, (name, kind)) in
            [("files", "public.file-url"), ("names", "NSFilenamesPboardType"), ("urls", "public.url")]
                .into_iter()
                .enumerate()
        {
            // SAFETY: NSView's designated initializer.
            let view: Retained<UrlView> = unsafe {
                msg_send![super(UrlView::alloc(mtm).set_ivars(name)), initWithFrame: rect(i as f64 * 60.0, 0.0, 60.0, 100.0)]
            };
            view.registerForDraggedTypes(&NSArray::from_retained_slice(&[NSString::from_str(kind)]));
            content.addSubview(&view);
        }
        let over = |x: f64, urls: &str| {
            testing::enter_with_urls(&s.window, x, 50.0, &["text/uri-list"], DND_COPY, Some(urls));
            let log = take_log();
            let replies = testing::take_replies();
            testing::leave();
            take_log();
            (log, replies)
        };
        let taken = [status(Some("text/uri-list"), DND_COPY, DND_COPY)];
        // A link isn't a file: only the view that takes any URL takes it.
        let link = "https://example.com/\r\n";
        assert_eq!(over(30.0, link), (vec![], vec![refused()]));
        assert_eq!(over(90.0, link), (vec![], vec![refused()]));
        assert_eq!(over(150.0, link), (vec!["urls.entered".to_owned()], taken.to_vec()));
        // Files are URLs too: all three take them.
        let files = "file:///tmp/a\r\nfile:///tmp/b\r\n";
        assert_eq!(over(30.0, files), (vec!["files.entered".to_owned()], taken.to_vec()));
        assert_eq!(over(90.0, files), (vec!["names.entered".to_owned()], taken.to_vec()));
        assert_eq!(over(150.0, files), (vec!["urls.entered".to_owned()], taken.to_vec()));
        // The drag pasteboard has the URLs, an item each, without asking the
        // source (there is none here).
        testing::enter_with_urls(&s.window, 30.0, 50.0, &["text/uri-list"], DND_COPY, Some(files));
        let board = objc2_app_kit::NSPasteboard::pasteboardWithName(&NSString::from_str("Apple CFPasteboard drag"));
        let items = board.pasteboardItems().expect("items");
        let file_url = NSString::from_str("public.file-url");
        let urls: Vec<String> =
            items.iter().filter_map(|i| i.stringForType(&file_url)).map(|u| u.to_string()).collect();
        assert_eq!(urls, ["file:///tmp/a", "file:///tmp/b"]);
        testing::leave();
        testing::take_replies();
        take_log();
    }

    thread_local!(static NESTED_WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) });

    define_class!(
        // A destination whose performDragOperation: runs a nested event
        // loop in which another drag comes over the window and moves.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "DndStateNester"]
        struct Nester;

        unsafe impl NSObjectProtocol for Nester {}

        unsafe impl NSDraggingDestination for Nester {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                log(format!("entered {}", testing::current()));
                NSDragOperation::Copy
            }

            #[unsafe(method(draggingExited:))]
            fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                log(format!("exited {}", testing::current()));
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                let seq = sender.draggingSequenceNumber();
                log(format!("perform {}", testing::current()));
                let window = NESTED_WINDOW.with(|w| w.borrow().clone()).expect("the window");
                testing::enter(&window, 10.0, 10.0, &[TEXT], DND_COPY);
                testing::motion(20.0, 20.0);
                // The dragging info is still this drop's.
                assert_eq!(sender.draggingSequenceNumber(), seq);
                true
            }

            #[unsafe(method(concludeDragOperation:))]
            fn conclude(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                log("conclude".into());
            }
        }
    );

    fn drops_outlive_drags_in_nested_loops(mtm: MainThreadMarker) {
        let s = scene(mtm);
        // SAFETY: NSView's designated initializer.
        let nester: Retained<Nester> =
            unsafe { msg_send![super(Nester::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 200.0, 100.0)] };
        // SAFETY: the constant lives as long as the program.
        nester.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeString }]));
        s.window.setContentView(Some(&nester));
        NESTED_WINDOW.with(|w| *w.borrow_mut() = Some(s.window.clone()));
        testing::enter(&s.window, 10.0, 10.0, &[TEXT], DND_COPY);
        let first = testing::current();
        testing::take_answers();
        testing::drop_of(first);
        let second = testing::current();
        assert_ne!(first, second);
        // The drop isn't exited when the second drag comes; the second
        // drag is entered and moves; then the drop concludes.
        assert_eq!(
            take_log(),
            [format!("entered {first}"), format!("perform {first}"), format!("entered {second}"), "conclude".into()]
        );
        let answers = testing::take_answers();
        assert_eq!(
            answers,
            [
                (second, status(Some(TEXT), DND_COPY, DND_COPY)),
                (second, status(Some(TEXT), DND_COPY, DND_COPY)),
                (first, Reply::Finish { performed: true }),
            ]
        );
        // The second drag is still under way.
        testing::motion(30.0, 30.0);
        assert_eq!(testing::take_replies(), [status(Some(TEXT), DND_COPY, DND_COPY)]);
        // Messages about the first, which is over, are refused.
        testing::motion_of(first, 1.0, 1.0);
        testing::drop_of(first);
        assert_eq!(testing::take_answers(), [(first, refused()), (first, Reply::Finish { performed: false })]);
        testing::leave();
        assert_eq!(take_log(), [format!("exited {second}")]);
        NESTED_WINDOW.with(|w| w.borrow_mut().take());
    }

    type Test = (&'static str, fn(MainThreadMarker));

    /// A new editable text view takes dropped text without registering
    /// anything (its `registeredDraggedTypes` is empty, as on macOS);
    /// made not editable, it doesn't; editable again, it does.
    fn text_views_take_text(mtm: MainThreadMarker) {
        let s = scene(mtm);
        let tv = objc2_app_kit::NSTextView::initWithFrame(
            objc2_app_kit::NSTextView::alloc(mtm),
            rect(0.0, 0.0, 200.0, 100.0),
        );
        s.window.contentView().unwrap().addSubview(&tv);
        assert_eq!(tv.registeredDraggedTypes().count(), 0);
        let takes = || {
            testing::enter(&s.window, 150.0, 50.0, &[TEXT], DND_COPY);
            let replies = testing::take_replies();
            testing::leave();
            testing::take_replies();
            take_log();
            replies == [status(Some(TEXT), DND_COPY, DND_COPY)]
        };
        assert!(takes(), "a new text view takes text");
        tv.setEditable(false);
        assert!(!takes(), "not while it isn't editable");
        tv.setEditable(true);
        assert!(takes());
        tv.removeFromSuperview();
    }

    pub(crate) fn run() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::capture_replies();
        let tests: &[Test] = &[
            ("destinations_follow_the_drag", destinations_follow_the_drag),
            ("drops_run_in_order", drops_run_in_order),
            ("operations_are_masked_and_mapped", operations_are_masked_and_mapped),
            ("dragging_info", dragging_info),
            ("windows_pass_drags_to_their_delegate", windows_pass_drags_to_their_delegate),
            ("periodic_updates", periodic_updates),
            ("url_drags_go_by_what_the_urls_are", url_drags_go_by_what_the_urls_are),
            ("drops_outlive_drags_in_nested_loops", drops_outlive_drags_in_nested_loops),
            ("text_views_take_text", text_views_take_text),
        ];
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test(mtm));
            println!("test {name} ... ok");
        }
    }
}
