//! The clipboard, drag and drop and screens on a real compositor, with
//! other programs: `wl-copy` and `wl-paste` on the other side of the
//! clipboard, a drag source of the test's own (another Wayland client,
//! dragging two files from its window onto ours with a virtual pointer),
//! `swaymsg` adding an output, moving the window to it and changing its
//! scale, and `wtype` giving the window the keyboard (and so the input
//! serial the clipboard needs). Linux only, and only under a compositor;
//! run it under the headless sway:
//!
//! ```sh
//! scripts/linux-run scripts/headless-wayland cargo test -p sidestep-appkit --test wayland_system
//! ```
//!
//! Without a Wayland display it says so and passes.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    linux::run();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::Cell;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObject, ProtocolObject};
    use objc2::{AnyThread, ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSApplicationDelegate, NSBackingStoreType, NSDragOperation, NSDraggingDestination,
        NSDraggingInfo, NSDraggingItem, NSDraggingItemEnumerationOptions, NSEventMask, NSPasteboard, NSPasteboardItem,
        NSPasteboardTypeFileURL, NSPasteboardTypeHTML, NSPasteboardTypeString, NSPasteboardWriting, NSScreen, NSView,
        NSWindow, NSWindowDelegate, NSWindowStyleMask,
    };
    use objc2_foundation::{
        NSArray, NSDictionary, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    };
    // Links Sidestep's frameworks, which nothing here names.
    use sidestep_appkit as _;

    thread_local! {
        static SCREEN_CHANGES: Cell<u32> = const { Cell::new(0) };
        static WINDOW_SCREEN_CHANGES: Cell<u32> = const { Cell::new(0) };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "WaylandSystemAppDelegate"]
        struct AppDelegate;

        unsafe impl NSObjectProtocol for AppDelegate {}

        unsafe impl NSApplicationDelegate for AppDelegate {
            #[unsafe(method(applicationDidChangeScreenParameters:))]
            fn screens_changed(&self, _note: &NSNotification) {
                SCREEN_CHANGES.with(|c| c.set(c.get() + 1));
            }
        }
    );

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "WaylandSystemWindowDelegate"]
        struct WindowDelegate;

        unsafe impl NSObjectProtocol for WindowDelegate {}

        unsafe impl NSWindowDelegate for WindowDelegate {
            #[unsafe(method(windowDidChangeScreen:))]
            fn screen_changed(&self, _note: &NSNotification) {
                WINDOW_SCREEN_CHANGES.with(|c| c.set(c.get() + 1));
            }
        }
    );

    thread_local!(static PROVIDED: Cell<u32> = const { Cell::new(0) });

    define_class!(
        // Provides text when it's asked for.
        #[unsafe(super(NSObject))]
        #[name = "WaylandSystemOwner"]
        struct Owner;

        unsafe impl NSObjectProtocol for Owner {}

        impl Owner {
            #[unsafe(method(pasteboard:provideDataForType:))]
            fn provide(&self, board: &NSPasteboard, kind: &NSString) {
                PROVIDED.with(|p| p.set(p.get() + 1));
                board.setString_forType(&NSString::from_str("promised text"), kind);
            }
        }
    );

    /// Handle what the render thread sends until `done`, for five seconds
    /// at most.
    fn pump_until(app: &NSApplication, what: &str, done: impl FnMut() -> bool) {
        pump_within(app, what, Duration::from_secs(5), done);
    }

    fn pump_within(app: &NSApplication, what: &str, time: Duration, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + time;
        let mode = NSString::from_str("kCFRunLoopDefaultMode");
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            // With no date, it handles what has arrived and returns.
            if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, None, &mode, true)
            {
                app.sendEvent(&event);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pump_for(app: &NSApplication, time: Duration) {
        let until = Instant::now() + time;
        pump_until(app, "time to pass", || Instant::now() >= until);
    }

    fn sh(script: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(script).stderr(Stdio::inherit()).output().expect("sh runs");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{script}: {}: {text}", out.status);
        text
    }

    /// Run `script` on another thread while the main thread keeps handling
    /// the render thread's messages (it may be asked for promised data),
    /// and return its output.
    fn sh_pumping(app: &NSApplication, script: &str) -> String {
        let (tx, rx) = mpsc::channel();
        let script = script.to_owned();
        std::thread::spawn(move || {
            let _ = tx.send(sh(&script));
        });
        let mut out = None;
        pump_until(app, "a command", || {
            out = out.take().or_else(|| rx.try_recv().ok());
            out.is_some()
        });
        out.expect("the command's output")
    }

    /// A sway command (in single quotes to the shell).
    fn swaymsg(command: &str) {
        sh(&format!("swaymsg -s \"$(ls \"$XDG_RUNTIME_DIR\"/sway-ipc.*.sock | head -n1)\" -- '{command}' >/dev/null"));
    }

    fn texts(types: Option<Retained<NSArray<NSString>>>) -> Vec<String> {
        types.map(|t| t.iter().map(|s| s.to_string()).collect()).unwrap_or_default()
    }

    fn screens(app: &NSApplication, window: &NSWindow, mtm: MainThreadMarker) {
        // A second output, before the program has asked about screens at
        // all: the delegate hears the screens changed.
        swaymsg("create_output");
        pump_until(app, "a second screen", || SCREEN_CHANGES.with(Cell::get) > 0);
        let both = NSScreen::screens(mtm);
        assert_eq!(both.count(), 2);
        let only = both.objectAtIndex(0);
        assert_eq!(only.frame().origin, NSPoint::ZERO);
        let on = |screen: &NSScreen| window.screen().is_some_and(|s| std::ptr::eq(&*s, screen));
        // The window stays on the first (sway's new output may pass over it
        // on the way to its place).
        pump_until(app, "the window to be on the first screen", || on(&only));
        assert!(std::ptr::eq(&*NSScreen::screens(mtm).objectAtIndex(0), &*only), "the screens stay the same objects");
        let second = both.objectAtIndex(1);
        assert_eq!(second.frame().origin.x, only.frame().size.width, "beside the first: {:?}", second.frame());
        // The window moves to it.
        let changes = WINDOW_SCREEN_CHANGES.with(Cell::get);
        swaymsg("[title=\"Wayland system\"] move container to output HEADLESS-2");
        pump_until(app, "the window's screen to change", || {
            WINDOW_SCREEN_CHANGES.with(Cell::get) > changes && on(&second)
        });
        assert!(std::ptr::eq(&*window.screen().expect("a screen"), &*second));
        // Its scale changes.
        let before = SCREEN_CHANGES.with(Cell::get);
        swaymsg("output HEADLESS-2 scale 2");
        pump_until(app, "the scale to change", || SCREEN_CHANGES.with(Cell::get) > before);
        assert_eq!(second.backingScaleFactor(), 2.0);
        // The window hears of its own scale separately.
        pump_until(app, "the window's scale to change", || window.backingScaleFactor() == 2.0);
        println!("  screens: {:?}, then {:?} at {}", only.frame(), second.frame(), second.backingScaleFactor());
        // The second output goes: its screen does, and the window is back on
        // the first.
        let before = (SCREEN_CHANGES.with(Cell::get), WINDOW_SCREEN_CHANGES.with(Cell::get));
        swaymsg("output HEADLESS-2 unplug");
        pump_until(app, "the second screen to go", || SCREEN_CHANGES.with(Cell::get) > before.0);
        let left = NSScreen::screens(mtm);
        assert_eq!(left.count(), 1);
        assert!(std::ptr::eq(&*left.objectAtIndex(0), &*only));
        pump_until(app, "the window to come back", || WINDOW_SCREEN_CHANGES.with(Cell::get) > before.1 && on(&only));
    }

    fn foreign_clipboard(app: &NSApplication) {
        let board = NSPasteboard::generalPasteboard();
        let copy = |script: &str| {
            let count = board.changeCount();
            sh(script);
            pump_until(app, script, || board.changeCount() != count);
        };
        // SAFETY: the constants live as long as the program.
        let (string, file_url) = unsafe { (NSPasteboardTypeString, NSPasteboardTypeFileURL) };
        copy("wl-copy 'from wl-copy'");
        assert_eq!(board.stringForType(string).map(|s| s.to_string()).as_deref(), Some("from wl-copy"));
        // The same again (as the compositor offers it on a change of focus)
        // is no change: what the program holds of it still reads.
        assert!(texts(board.types()).contains(&"public.utf8-plain-text".to_owned()));
        let count = board.changeCount();
        sh("wl-copy 'from wl-copy'");
        pump_for(app, Duration::from_millis(300));
        assert_eq!(board.changeCount(), count);
        assert_eq!(board.stringForType(string).map(|s| s.to_string()).as_deref(), Some("from wl-copy"));
        // Another program's text was read ahead: a paste doesn't wait.
        let start = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(board.stringForType(string));
        }
        println!("  pasting another program's text: {:.2} µs", start.elapsed().as_secs_f64() * 1e3);
        copy("printf 'file:///tmp/a%%20b\\r\\nfile:///tmp/c\\r\\n' | wl-copy --type text/uri-list");
        let items = board.pasteboardItems().expect("items");
        assert_eq!(items.count(), 2);
        let urls: Vec<String> =
            items.iter().map(|i| i.stringForType(file_url).map(|s| s.to_string()).unwrap_or_default()).collect();
        assert_eq!(urls, ["file:///tmp/a%20b", "file:///tmp/c"]);
        let png = NSString::from_str("public.png");
        let found = board.availableTypeFromArray(&NSArray::from_slice(&[&*png, file_url]));
        assert_eq!(found.map(|t| t.to_string()).as_deref(), Some("public.file-url"));
        let paths = board.propertyListForType(&NSString::from_str("NSFilenamesPboardType")).expect("paths");
        let paths = paths.downcast::<NSArray>().expect("an array");
        let paths: Vec<String> = paths.iter().map(|p| p.downcast::<NSString>().unwrap().to_string()).collect();
        assert_eq!(paths, ["/tmp/a b", "/tmp/c"]);
        copy("printf 'x' | wl-copy --type application/x-sidestep-uti.com.example.thing");
        assert!(texts(board.types()).contains(&"com.example.thing".to_owned()), "{:?}", texts(board.types()));
        let thing = NSString::from_str("com.example.thing");
        // Types other than text are read when asked for: a round trip.
        let start = Instant::now();
        assert_eq!(board.stringForType(&thing).map(|s| s.to_string()).as_deref(), Some("x"));
        println!("  reading another type from wl-copy: {:.2} ms", start.elapsed().as_secs_f64() * 1e3);
        // Something large is read whole.
        copy("head -c 6000000 /dev/zero | tr '\\0' x | wl-copy --type application/x-sidestep-uti.com.example.big");
        let big = NSString::from_str("com.example.big");
        let start = Instant::now();
        let read = board.stringForType(&big).map(|s| s.length());
        println!("  reading 6 MB from wl-copy: {:.1} ms", start.elapsed().as_secs_f64() * 1e3);
        assert_eq!(read, Some(6_000_000));
        println!("  read from wl-copy: text, two files, a type of our own, 6 MB");
    }

    fn our_clipboard(app: &NSApplication) {
        // Setting the selection takes an input event newer than the current
        // selection's, as when a copy follows a key press.
        sh("wtype -k Escape");
        pump_for(app, Duration::from_millis(300));
        let board = NSPasteboard::generalPasteboard();
        // SAFETY: the constants live as long as the program.
        let (string, html, file_url) =
            unsafe { (NSPasteboardTypeString, NSPasteboardTypeHTML, NSPasteboardTypeFileURL) };
        // Count the selections other programs see.
        sh(
            "rm -f /tmp/selections; wl-paste --watch sh -c 'echo >>/tmp/selections' >/dev/null 2>&1 & echo $! >/tmp/watch.pid",
        );
        let selections = || sh("cat /tmp/selections 2>/dev/null | wc -l").trim().parse::<usize>().unwrap_or(0);
        pump_until(app, "the watcher to start", || selections() > 0);
        let before = selections();
        let start = Instant::now();
        board.clearContents();
        let (a, b) = (NSPasteboardItem::new(), NSPasteboardItem::new());
        a.setString_forType(&NSString::from_str("ours"), string);
        a.setString_forType(&NSString::from_str("<b>ours</b>"), html);
        a.setString_forType(&NSString::from_str("file:///tmp/ours%201"), file_url);
        a.setString_forType(&NSString::from_str("not really a PNG"), &NSString::from_str("public.png"));
        b.setString_forType(&NSString::from_str("file:///tmp/ours2"), file_url);
        let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
            NSArray::from_retained_slice(&[ProtocolObject::from_retained(a), ProtocolObject::from_retained(b)]);
        assert!(board.writeObjects(&objects));
        println!("  copying text, HTML, PNG and two files: {:.1} µs", start.elapsed().as_secs_f64() * 1e6);
        // Offered at the end of the turn: one selection for all of it.
        pump_for(app, Duration::from_millis(300));
        assert_eq!(selections(), before + 1, "one selection per turn");
        let listed = sh_pumping(app, "wl-paste --list-types");
        for mime in
            ["text/plain;charset=utf-8", "text/html", "image/png", "text/uri-list", "x-special/gnome-copied-files"]
        {
            assert!(listed.lines().any(|l| l == mime), "{mime} in {listed:?}");
        }
        assert_eq!(sh_pumping(app, "wl-paste --no-newline --type text/plain"), "ours");
        assert_eq!(sh_pumping(app, "wl-paste --no-newline --type text/html"), "<b>ours</b>");
        assert_eq!(sh_pumping(app, "wl-paste --no-newline --type image/png"), "not really a PNG");
        assert_eq!(
            sh_pumping(app, "wl-paste --no-newline --type text/uri-list"),
            "file:///tmp/ours%201\r\nfile:///tmp/ours2\r\n"
        );
        // A promise is kept when another program reads it, on the main
        // thread.
        // SAFETY: the owner answers pasteboard:provideDataForType:.
        let owner: Retained<Owner> = unsafe { msg_send![super(Owner::alloc().set_ivars(())), init] };
        let owner_object: &AnyObject = owner.as_ref();
        // SAFETY: as above.
        unsafe { board.declareTypes_owner(&NSArray::from_slice(&[string]), Some(owner_object)) };
        // The watcher reads each new selection's text, as clipboard managers
        // do: the owner is asked, on the main thread, once.
        pump_until(app, "the watcher to read the promise", || PROVIDED.with(Cell::get) == 1);
        // Keeping the promise isn't a new selection: the one offered stays,
        // and reads again, without asking the owner again.
        pump_for(app, Duration::from_millis(300));
        assert_eq!(selections(), before + 2, "a kept promise is no new selection");
        sh("kill $(cat /tmp/watch.pid)");
        assert_eq!(sh_pumping(app, "wl-paste --no-newline --type text/plain"), "promised text");
        assert_eq!(PROVIDED.with(Cell::get), 1);
        // Read back here, it's read locally.
        assert_eq!(board.stringForType(string).map(|s| s.to_string()).as_deref(), Some("promised text"));
        println!("  wl-paste read: one selection, text, HTML, PNG, a URL list, promised text (twice)");
    }

    thread_local!(static DRAG_LOG: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) });

    fn drag_log(line: String) {
        DRAG_LOG.with(|l| l.borrow_mut().push(line));
    }

    define_class!(
        // Takes dragged files and text, and reads them when they're dropped.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "WaylandSystemDropView"]
        struct DropView;

        unsafe impl NSObjectProtocol for DropView {}

        unsafe impl NSDraggingDestination for DropView {
            #[unsafe(method(draggingEntered:))]
            fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                // SAFETY: the constant lives as long as the program.
                let file_url = unsafe { NSPasteboardTypeFileURL };
                let board = sender.draggingPasteboard();
                let found = board.availableTypeFromArray(&NSArray::from_slice(&[file_url]));
                drag_log(format!(
                    "entered {:?} {}",
                    found.map(|t| t.to_string()),
                    sender.draggingSourceOperationMask().contains(NSDragOperation::Copy)
                ));
                NSDragOperation::Copy
            }

            #[unsafe(method(draggingUpdated:))]
            fn dragging_updated(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
                drag_log("updated".into());
                NSDragOperation::Copy
            }

            #[unsafe(method(performDragOperation:))]
            fn perform(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
                // SAFETY: the constant lives as long as the program.
                let file_url = unsafe { NSPasteboardTypeFileURL };
                let items = sender.draggingPasteboard().pasteboardItems().map(|i| i.to_vec()).unwrap_or_default();
                let urls: Vec<String> =
                    items.iter().filter_map(|i| i.stringForType(file_url)).map(|s| s.to_string()).collect();
                let at = sender.draggingLocation();
                drag_log(format!("perform {urls:?} at {},{}", at.x, at.y));
                // The same items, as dragging items, and a walk that stops.
                let classes = NSArray::from_slice(&[NSPasteboardItem::class()]);
                let search = NSDictionary::new();
                let each = RcBlock::new(|item: NonNull<NSDraggingItem>, index: isize, _stop: NonNull<Bool>| {
                    // SAFETY: the dragging info hands the block a live item.
                    let object = unsafe { item.as_ref() }.item();
                    let url = object.downcast::<NSPasteboardItem>().ok().and_then(|i| i.stringForType(file_url));
                    drag_log(format!("item {index} {:?}", url.map(|u| u.to_string())));
                });
                let first = RcBlock::new(|_item: NonNull<NSDraggingItem>, index: isize, stop: NonNull<Bool>| {
                    drag_log(format!("stopped at {index}"));
                    // SAFETY: the stop flag is the caller's, for this call.
                    unsafe { stop.write(Bool::YES) };
                });
                for block in [&each, &first] {
                    // SAFETY: the classes read from pasteboards, and the
                    // blocks take what the dragging info hands them.
                    unsafe {
                        sender.enumerateDraggingItemsWithOptions_forView_classes_searchOptions_usingBlock(
                            NSDraggingItemEnumerationOptions::empty(),
                            None,
                            &classes,
                            &search,
                            block,
                        )
                    };
                }
                !urls.is_empty()
            }

            #[unsafe(method(concludeDragOperation:))]
            fn conclude(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                drag_log("conclude".into());
            }
        }
    );

    /// Drag files from a window of another client onto ours: the drag
    /// source runs on a thread of its own, with a virtual pointer.
    fn drag_and_drop(app: &NSApplication, window: &NSWindow, mtm: MainThreadMarker) {
        // SAFETY: NSView's designated initializer.
        let view: Retained<DropView> = unsafe {
            msg_send![super(DropView::alloc(mtm).set_ivars(())), initWithFrame: NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 200.0))]
        };
        // SAFETY: the constants live as long as the program.
        let (file_url, string) = unsafe { (NSPasteboardTypeFileURL, NSPasteboardTypeString) };
        view.registerForDraggedTypes(&NSArray::from_slice(&[file_url, string]));
        window.setContentView(Some(&view));
        let drag = || {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(source::drag());
            });
            let mut outcome = None;
            // The source gives up on each step after five seconds.
            pump_within(app, "the drag", Duration::from_secs(30), || {
                outcome = outcome.take().or_else(|| rx.try_recv().ok());
                outcome.is_some()
            });
            outcome.expect("the drag's outcome")
        };
        let outcome = drag();
        let mut log = DRAG_LOG.with(|l| std::mem::take(&mut *l.borrow_mut()));
        // Updates as the drag moved, and while it waited before the drop.
        let updates = log.iter().filter(|l| *l == "updated").count();
        log.retain(|l| l != "updated");
        println!("  drag: {log:?}, {updates} updates, source: {outcome:?}");
        assert!(updates >= 3, "periodic updates while the drag waits: {updates}");
        assert!(log.first().is_some_and(|l| l == r#"entered Some("public.file-url") true"#), "{log:?}");
        let perform = log.iter().find(|l| l.starts_with("perform")).expect("a drop");
        assert!(perform.starts_with(r#"perform ["file:///tmp/dragged%201", "file:///tmp/dragged2"] at "#), "{log:?}");
        let enumerated: Vec<&str> =
            log.iter().filter(|l| l.starts_with("item") || l.starts_with("stopped")).map(String::as_str).collect();
        assert_eq!(
            enumerated,
            [r#"item 0 Some("file:///tmp/dragged%201")"#, r#"item 1 Some("file:///tmp/dragged2")"#, "stopped at 0"]
        );
        assert_eq!(log.last().map(String::as_str), Some("conclude"));
        assert_eq!(outcome, source::Outcome::Finished);
        // Once the drop is finished, the drag's data is gone: its text reads
        // as nothing, not as empty text; its URLs came with it, and stay.
        let board = NSPasteboard::pasteboardWithName(&NSString::from_str("Apple CFPasteboard drag"));
        assert_eq!(board.stringForType(string).map(|s| s.to_string()), None);
        assert!(board.stringForType(file_url).is_some());
        // Another drag, from a new source, goes the same way.
        let outcome = drag();
        let log = DRAG_LOG.with(|l| std::mem::take(&mut *l.borrow_mut()));
        assert!(log.first().is_some_and(|l| l.starts_with("entered")), "{log:?}");
        assert!(log.iter().any(|l| l.starts_with(r#"perform ["file:///tmp/dragged%201""#)), "{log:?}");
        assert_eq!(log.last().map(String::as_str), Some("conclude"));
        assert_eq!(outcome, source::Outcome::Finished);
    }

    /// A drag source: another client, with a window of its own, which
    /// drags two files from its window to the left half of the output (our
    /// window, as sway tiles them) with a virtual pointer.
    mod source {
        use std::io::Write;
        use std::time::{Duration, Instant};

        use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
        use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
        use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
        use smithay_client_toolkit::data_device_manager::data_source::{DataSourceHandler, DragSource};
        use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
        use smithay_client_toolkit::output::{OutputHandler, OutputState};
        use smithay_client_toolkit::reexports::client::globals::{GlobalList, registry_queue_init};
        use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
        use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
        use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
        use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
        use smithay_client_toolkit::reexports::client::protocol::wl_pointer::{ButtonState, WlPointer};
        use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
        use smithay_client_toolkit::reexports::client::protocol::wl_shm::Format;
        use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
        use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
        use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
        use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;
        use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
        use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
        use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
        use smithay_client_toolkit::shell::WaylandSurface;
        use smithay_client_toolkit::shell::xdg::XdgShell;
        use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
        use smithay_client_toolkit::shm::slot::SlotPool;
        use smithay_client_toolkit::shm::{Shm, ShmHandler};
        use smithay_client_toolkit::{
            delegate_compositor, delegate_data_device, delegate_output, delegate_pointer, delegate_registry,
            delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
        };

        #[derive(Debug, PartialEq, Eq)]
        pub(super) enum Outcome {
            Finished,
            Cancelled,
            TimedOut(&'static str),
        }

        struct Source {
            registry: RegistryState,
            seats: SeatState,
            outputs: OutputState,
            shm: Shm,
            pool: SlotPool,
            window: Window,
            configured: bool,
            seat: Option<WlSeat>,
            pointer: Option<WlPointer>,
            press: Option<u32>,
            device: Option<DataDevice>,
            done: Option<Outcome>,
        }

        const BTN_LEFT: u32 = 0x110;
        const URIS: &str = "file:///tmp/dragged%201\r\nfile:///tmp/dragged2\r\n";

        pub(super) fn drag() -> Outcome {
            let conn = Connection::connect_to_env().expect("a compositor");
            let (globals, mut queue): (GlobalList, _) = registry_queue_init(&conn).expect("its registry");
            let qh = queue.handle();
            let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
            let xdg = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
            let shm = Shm::bind(&globals, &qh).expect("wl_shm");
            let manager = DataDeviceManagerState::bind(&globals, &qh).expect("wl_data_device_manager");
            let pointers: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).expect("virtual pointers");
            let surface = compositor.create_surface(&qh);
            let window = xdg.create_window(surface, WindowDecorations::ServerDefault, &qh);
            window.set_title("Drag source");
            window.commit();
            let mut s = Source {
                registry: RegistryState::new(&globals),
                seats: SeatState::new(&globals, &qh),
                outputs: OutputState::new(&globals, &qh),
                pool: SlotPool::new(1 << 22, &shm).expect("a pool"),
                shm,
                window,
                configured: false,
                seat: None,
                pointer: None,
                press: None,
                device: None,
                done: None,
            };
            let wait = |s: &mut Source, queue: &mut _, what: &'static str, until: &dyn Fn(&Source) -> bool| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !until(s) {
                    if Instant::now() > deadline {
                        return Err(Outcome::TimedOut(what));
                    }
                    let queue: &mut smithay_client_toolkit::reexports::client::EventQueue<Source> = queue;
                    queue.roundtrip(s).expect("a round trip");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            };
            if let Err(e) = wait(&mut s, &mut queue, "the source's window", &|s| s.configured) {
                return e;
            }
            let Some(seat) = s.seat.clone().or_else(|| s.seats.seats().next()) else {
                return Outcome::TimedOut("a seat");
            };
            s.device = Some(manager.get_data_device(&qh, &seat));
            let pointer = pointers.create_virtual_pointer(Some(&seat), &qh, ());
            if let Err(e) = wait(&mut s, &mut queue, "a pointer", &|s| s.pointer.is_some()) {
                return e;
            }
            let time =
                || (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()) as u32;
            let move_to = |x: u32, y: u32| {
                pointer.motion_absolute(time(), x, y, 1280, 800);
                pointer.frame();
            };
            // Press on the source's window, on the right half.
            move_to(960, 400);
            pointer.button(time(), BTN_LEFT, ButtonState::Pressed);
            pointer.frame();
            if let Err(e) = wait(&mut s, &mut queue, "the press", &|s| s.press.is_some()) {
                return e;
            }
            let source: DragSource = manager.create_drag_and_drop_source(
                &qh,
                ["text/uri-list", "text/plain;charset=utf-8"],
                DndAction::Copy | DndAction::Move,
            );
            source.start_drag(s.device.as_ref().expect("a data device"), s.window.wl_surface(), None, s.press.unwrap());
            // Over to our window, a step at a time, giving it time to answer.
            for x in [900, 700, 500, 320] {
                move_to(x, 400);
                queue.roundtrip(&mut s).expect("a round trip");
                std::thread::sleep(Duration::from_millis(100));
            }
            // Waiting before the drop, as a user does.
            std::thread::sleep(Duration::from_millis(300));
            pointer.button(time(), BTN_LEFT, ButtonState::Released);
            pointer.frame();
            if let Err(e) = wait(&mut s, &mut queue, "the drop to finish", &|s| s.done.is_some()) {
                return e;
            }
            drop(source);
            s.done.take().expect("an outcome")
        }

        impl DataSourceHandler for Source {
            fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}

            fn send_request(
                &mut self,
                _: &Connection,
                _: &QueueHandle<Self>,
                _: &WlDataSource,
                mime: String,
                mut fd: WritePipe,
            ) {
                let data = if mime == "text/uri-list" { URIS } else { "dragged text" };
                let _ = fd.write_all(data.as_bytes());
            }

            fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {
                self.done.get_or_insert(Outcome::Cancelled);
            }

            fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

            fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {
                self.done.get_or_insert(Outcome::Finished);
            }

            fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
        }

        impl DataDeviceHandler for Source {
            fn enter(
                &mut self,
                _: &Connection,
                _: &QueueHandle<Self>,
                _: &WlDataDevice,
                _: f64,
                _: f64,
                _: &WlSurface,
            ) {
            }
            fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
            fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
            fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
            fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
        }

        impl DataOfferHandler for Source {
            fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
            fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
        }

        impl PointerHandler for Source {
            fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlPointer, events: &[PointerEvent]) {
                for event in events {
                    if let PointerEventKind::Press { serial, .. } = event.kind
                        && event.surface == *self.window.wl_surface()
                    {
                        self.press = Some(serial);
                    }
                }
            }
        }

        impl SeatHandler for Source {
            fn seat_state(&mut self) -> &mut SeatState {
                &mut self.seats
            }

            fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
                self.seat.get_or_insert(seat);
            }

            fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat, capability: Capability) {
                if capability == Capability::Pointer && self.pointer.is_none() {
                    self.pointer = self.seats.get_pointer(qh, &seat).ok();
                }
            }

            fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
            fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
        }

        impl WindowHandler for Source {
            fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {}

            fn configure(
                &mut self,
                _: &Connection,
                _: &QueueHandle<Self>,
                window: &Window,
                configure: WindowConfigure,
                _: u32,
            ) {
                let w = configure.new_size.0.map_or(320, |w| w.get());
                let h = configure.new_size.1.map_or(240, |h| h.get());
                let (buffer, canvas) =
                    self.pool.create_buffer(w as i32, h as i32, w as i32 * 4, Format::Xrgb8888).expect("a buffer");
                canvas.fill(0x80);
                buffer.attach_to(window.wl_surface()).expect("attached");
                window.wl_surface().damage_buffer(0, 0, w as i32, h as i32);
                window.commit();
                self.configured = true;
            }
        }

        impl CompositorHandler for Source {
            fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: i32) {}
            fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: Transform) {}
            fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
            fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
            fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &WlOutput) {}
        }

        impl OutputHandler for Source {
            fn output_state(&mut self) -> &mut OutputState {
                &mut self.outputs
            }
            fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
            fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
            fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
        }

        impl ShmHandler for Source {
            fn shm_state(&mut self) -> &mut Shm {
                &mut self.shm
            }
        }

        impl ProvidesRegistryState for Source {
            fn registry(&mut self) -> &mut RegistryState {
                &mut self.registry
            }
            registry_handlers![OutputState, SeatState];
        }

        impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for Source {
            fn event(
                _: &mut Self,
                _: &ZwlrVirtualPointerManagerV1,
                _: <ZwlrVirtualPointerManagerV1 as smithay_client_toolkit::reexports::client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        impl Dispatch<ZwlrVirtualPointerV1, ()> for Source {
            fn event(
                _: &mut Self,
                _: &ZwlrVirtualPointerV1,
                _: <ZwlrVirtualPointerV1 as smithay_client_toolkit::reexports::client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        delegate_compositor!(Source);
        delegate_output!(Source);
        delegate_shm!(Source);
        delegate_seat!(Source);
        delegate_pointer!(Source);
        delegate_xdg_shell!(Source);
        delegate_xdg_window!(Source);
        delegate_data_device!(Source);
        delegate_registry!(Source);
    }

    pub(crate) fn run() {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            println!("skipped: no Wayland display (run it under scripts/headless-wayland)");
            return;
        }
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: NSObject's designated initializer.
        let app_delegate: Retained<AppDelegate> =
            unsafe { msg_send![super(AppDelegate::alloc(mtm).set_ivars(())), init] };
        app.setDelegate(Some(ProtocolObject::from_ref(&*app_delegate)));
        // SAFETY: a titled window. Resizable, so sway tiles it: on the left
        // half of the output once the drag source's window comes.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 200.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the test keeps its reference.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Wayland system"));
        // SAFETY: NSObject's designated initializer.
        let window_delegate: Retained<WindowDelegate> =
            unsafe { msg_send![super(WindowDelegate::alloc(mtm).set_ivars(())), init] };
        window.setDelegate(Some(ProtocolObject::from_ref(&*window_delegate)));
        window.makeKeyAndOrderFront(None);
        pump_for(&app, Duration::from_millis(500));
        let has_wtype = Command::new("sh").arg("-c").arg("command -v wtype").output().is_ok_and(|o| o.status.success());
        let clipboard = |check: fn(&NSApplication)| {
            if has_wtype {
                check(&app);
            } else {
                println!("  (no wtype: the window can't get the keyboard, which the clipboard needs)");
            }
        };
        // The screens last: moving the window to another output leaves it
        // without the keyboard.
        let tests: [(&str, &dyn Fn()); 4] = [
            ("foreign_clipboard", &|| clipboard(foreign_clipboard)),
            ("our_clipboard", &|| clipboard(our_clipboard)),
            ("drag_and_drop", &|| drag_and_drop(&app, &window, mtm)),
            ("screens", &|| screens(&app, &window, mtm)),
        ];
        if has_wtype {
            // A key gives the window the keyboard: the compositor tells the
            // program of the selection, and gives it an input serial to set
            // its own with.
            sh("wtype -k Escape");
            pump_for(&app, Duration::from_millis(300));
        }
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test());
            println!("test {name} ... ok");
        }
    }
}
