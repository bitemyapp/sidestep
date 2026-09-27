//! What only a window on screen shows, through the null render thread
//! (see `sidestep_appkit::testing`): a live resize tells every view in the
//! window, parents before their subviews, after the window's notification
//! as it starts and before it as it ends; titled windows on screen go in
//! the Window menu, and leave it when they're ordered out; an overriding
//! `orderWindow:relativeTo:` that calls super shows the window; posted
//! accessibility notifications wait in the store; and what views that
//! don't clip let their subviews draw outside them is redrawn when it
//! changes.
//!
//! With `SIDESTEP_FUNNEL_BENCH=1` it also times the funnels' check (see
//! `funnel`) against the calls it guards, and with `SIDESTEP_DRAW_BENCH=1`
//! the display pass over views that don't clip, for a release build to
//! report.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

#[cfg(target_vendor = "apple")]
fn main() {}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    linux::main();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::RefCell;
    use std::time::Instant;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSMenu, NSPanel, NSResponder, NSView, NSWindow, NSWindowOrderingMode,
        NSWindowStyleMask,
    };
    use objc2_foundation::{NSDictionary, NSNotification, NSNotificationCenter, NSPoint, NSRect, NSSize, NSString};
    use sidestep_appkit::testing::{self, Seen};

    thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

    fn log(line: String) {
        LOG.with(|l| l.borrow_mut().push(line));
    }

    fn take_log() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    define_class!(
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxSweepView"]
        #[ivars = &'static str]
        struct Probe;

        impl Probe {
            #[unsafe(method(viewWillStartLiveResize))]
            fn will_start(&self) {
                let live: bool = unsafe { msg_send![self, inLiveResize] };
                log(format!("{} will {live}", self.ivars()));
            }

            #[unsafe(method(viewDidEndLiveResize))]
            fn did_end(&self) {
                log(format!("{} did", self.ivars()));
            }
        }
    );

    impl Probe {
        fn new(mtm: MainThreadMarker, name: &'static str) -> Retained<Probe> {
            unsafe { msg_send![super(Probe::alloc(mtm).set_ivars(name)), initWithFrame: rect(0.0, 0.0, 50.0, 50.0)] }
        }
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxSweepObserver"]
        struct Observer;

        unsafe impl NSObjectProtocol for Observer {}

        impl Observer {
            #[unsafe(method(noted:))]
            fn noted(&self, n: &NSNotification) {
                log(n.name().to_string());
            }
        }
    );

    define_class!(
        /// Logs its ordering, then orders as AppKit's does.
        #[unsafe(super(NSWindow, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxSweepWindow"]
        struct OrderingWindow;

        impl OrderingWindow {
            #[unsafe(method(orderWindow:relativeTo:))]
            fn order(&self, place: NSWindowOrderingMode, other: isize) {
                log(format!("order {}", place.0));
                let _: () = unsafe { msg_send![super(self), orderWindow: place, relativeTo: other] };
            }
        }
    );

    define_class!(
        /// Logs what it's asked to constrain, then constrains as AppKit's
        /// does.
        #[unsafe(super(NSWindow, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxSweepConstrained"]
        struct ConstrainedWindow;

        impl ConstrainedWindow {
            #[unsafe(method(constrainFrameRect:toScreen:))]
            fn constrain(&self, frame: NSRect, screen: Option<&objc2_app_kit::NSScreen>) -> NSRect {
                log(format!("constrain {}", frame.origin.y));
                unsafe { msg_send![super(self), constrainFrameRect: frame, toScreen: screen] }
            }
        }
    );

    fn window(mtm: MainThreadMarker, content: &NSView) -> Retained<NSWindow> {
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 300.0, 200.0),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { w.setReleasedWhenClosed(false) };
        w.setContentView(Some(content));
        w
    }

    fn live_resize_reaches_every_view(mtm: MainThreadMarker) {
        let (content, child, grandchild, sibling) = (
            Probe::new(mtm, "content"),
            Probe::new(mtm, "child"),
            Probe::new(mtm, "grandchild"),
            Probe::new(mtm, "sibling"),
        );
        content.addSubview(&child);
        child.addSubview(&grandchild);
        content.addSubview(&sibling);
        let w = window(mtm, &content);
        let observer: Retained<Observer> = unsafe { msg_send![Observer::alloc(mtm), init] };
        let center = NSNotificationCenter::defaultCenter();
        for name in ["NSWindowWillStartLiveResizeNotification", "NSWindowDidEndLiveResizeNotification"] {
            unsafe {
                center.addObserver_selector_name_object(
                    &observer,
                    sel!(noted:),
                    Some(&NSString::from_str(name)),
                    Some(&w),
                )
            };
        }
        w.makeKeyAndOrderFront(None);
        testing::settle();
        let id = testing::showing_id(&w);
        take_log();
        testing::inject_configure(id, 320, 200, 1.0, true, false);
        testing::settle();
        assert_eq!(
            take_log(),
            [
                "NSWindowWillStartLiveResizeNotification",
                "content will true",
                "child will true",
                "grandchild will true",
                "sibling will true",
            ]
        );
        testing::inject_configure(id, 340, 200, 1.0, false, false);
        testing::settle();
        assert_eq!(
            take_log(),
            ["content did", "child did", "grandchild did", "sibling did", "NSWindowDidEndLiveResizeNotification"]
        );
        unsafe { center.removeObserver(&observer) };
        w.orderOut(None);
        testing::settle();
    }

    fn styled(mtm: MainThreadMarker, style: NSWindowStyleMask, title: &str) -> Retained<NSWindow> {
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 200.0, 100.0),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { w.setReleasedWhenClosed(false) };
        w.setTitle(&NSString::from_str(title));
        w
    }

    /// As `contract_sweep`'s `windows_menu_on_screen` measures on macOS:
    /// titled windows that aren't panels list themselves as they're
    /// ordered in, unless excluded or untitled; retitling one on screen
    /// renames it; ordering it out takes it out.
    fn shown_windows_are_in_the_window_menu(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Window"));
        let _: () = unsafe { msg_send![&*app, setWindowsMenu: &*menu] };
        let items = || -> Vec<String> {
            (0..menu.numberOfItems())
                .filter_map(|i| menu.itemAtIndex(i))
                .filter(|i| !i.isSeparatorItem())
                .map(|i| i.title().to_string())
                .collect()
        };
        let titled = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
        let w = styled(mtm, titled, "Listed");
        assert!(items().is_empty(), "not until it is on screen");
        w.makeKeyAndOrderFront(None);
        testing::settle();
        assert_eq!(items(), ["Listed"]);
        let borderless = styled(mtm, NSWindowStyleMask::Borderless, "Borderless");
        borderless.orderFront(None);
        borderless.setTitle(&NSString::from_str("Borderless 2"));
        let panel: Retained<NSPanel> = unsafe {
            msg_send![NSPanel::alloc(mtm), initWithContentRect: rect(0.0, 0.0, 200.0, 100.0), styleMask: titled, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let panel = panel.into_super();
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(&NSString::from_str("Panel"));
        panel.orderFront(None);
        let untitled = styled(mtm, titled, "");
        untitled.orderFront(None);
        let excluded = styled(mtm, titled, "Excluded");
        excluded.setExcludedFromWindowsMenu(true);
        excluded.orderFront(None);
        testing::settle();
        assert_eq!(items(), ["Listed"], "borderless, panel, untitled and excluded windows aren't listed");
        untitled.setTitle(&NSString::from_str("Late"));
        assert_eq!(items(), ["Late", "Listed"], "a title set on screen lists it");
        untitled.setTitle(&NSString::from_str(""));
        assert_eq!(items(), ["Listed"], "an empty one takes it out");
        untitled.setTitle(&NSString::from_str("later"));
        w.setTitle(&NSString::from_str("Renamed"));
        assert_eq!(items(), ["later", "Renamed"]);
        w.orderOut(None);
        testing::settle();
        assert_eq!(items(), ["later"], "ordered out, it leaves");
        w.setTitle(&NSString::from_str("Hidden"));
        assert_eq!(items(), ["later"]);
        w.orderFront(None);
        testing::settle();
        assert_eq!(items(), ["Hidden", "later"]);
        w.setExcludedFromWindowsMenu(true);
        assert_eq!(items(), ["later"]);
        w.setExcludedFromWindowsMenu(false);
        assert_eq!(items(), ["Hidden", "later"]);
        excluded.setExcludedFromWindowsMenu(false);
        assert_eq!(items(), ["Excluded", "Hidden", "later"]);
        for w in [&w, &borderless, &panel, &untitled, &excluded] {
            w.close();
        }
        testing::settle();
        assert!(items().is_empty());
    }

    fn overriding_order_window_still_shows(mtm: MainThreadMarker) {
        let w: Retained<OrderingWindow> = unsafe {
            msg_send![OrderingWindow::alloc(mtm), initWithContentRect: rect(0.0, 0.0, 100.0, 80.0), styleMask: NSWindowStyleMask::Titled, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let w: Retained<NSWindow> = unsafe { Retained::cast_unchecked(w) };
        unsafe { w.setReleasedWhenClosed(false) };
        testing::take_render_log();
        w.orderFront(None);
        testing::settle();
        assert_eq!(take_log(), ["order 1"]);
        assert!(w.isVisible());
        assert!(testing::take_render_log().iter().any(|s| matches!(s, Seen::Created { .. })));
        w.orderOut(None);
        testing::settle();
        assert_eq!(take_log(), ["order 0"]);
        assert!(!w.isVisible());
    }

    /// As `funnels`' `window_constraints_on_screen` counts on macOS:
    /// ordering in asks once, on screen or not; moving asks twice; a frame
    /// or a content size once, the content keeping its top left corner.
    fn window_constraints(mtm: MainThreadMarker) {
        let w: Retained<ConstrainedWindow> = unsafe {
            msg_send![ConstrainedWindow::alloc(mtm), initWithContentRect: rect(100.0, 100.0, 400.0, 300.0), styleMask: NSWindowStyleMask::Titled, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let w: Retained<NSWindow> = w.into_super();
        unsafe { w.setReleasedWhenClosed(false) };
        let count = |what: &str, n: usize| {
            let log = take_log();
            assert_eq!(log.iter().filter(|l| l.starts_with("constrain")).count(), n, "{what}: {log:?}");
        };
        take_log();
        w.setFrameOrigin(NSPoint::new(10.0, 300.0));
        count("moved off screen", 0);
        w.orderFront(None);
        testing::settle();
        count("ordered in", 1);
        w.orderFront(None);
        count("ordered front again", 1);
        w.setFrame_display(rect(100.0, 100.0, 400.0, 300.0), false);
        count("setFrame:display:", 1);
        w.setContentSize(NSSize::new(200.0, 100.0));
        count("setContentSize:", 1);
        let top_left = |f: NSRect| (f.origin.x, f.origin.y + f.size.height);
        assert_eq!(top_left(w.frame()), (100.0, 400.0), "the top left corner stays");
        testing::settle();
        assert_eq!(w.frame(), rect(100.0, 300.0, 200.0, 100.0), "once the compositor has resized it");
        // The compositor's own resizes keep it too.
        testing::inject_configure(testing::showing_id(&w), 260, 150, 1.0, false, false);
        testing::settle();
        assert_eq!(w.frame(), rect(100.0, 250.0, 260.0, 150.0));
        w.setFrameOrigin(NSPoint::new(10.0, 200.0));
        count("setFrameOrigin:", 2);
        w.setFrameTopLeftPoint(NSPoint::new(10.0, 500.0));
        count("setFrameTopLeftPoint:", 2);
        w.orderOut(None);
        testing::settle();
        take_log();
    }

    fn accessibility_notifications_are_stored(mtm: MainThreadMarker) {
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
        testing::take_accessibility_notifications();
        let key = unsafe { objc2_app_kit::NSAccessibilityAnnouncementKey };
        let info = NSDictionary::from_retained_objects(
            &[key],
            &[unsafe { Retained::cast_unchecked::<AnyObject>(NSString::from_str("Saved")) }],
        );
        unsafe {
            objc2_app_kit::NSAccessibilityPostNotificationWithUserInfo(
                &view,
                objc2_app_kit::NSAccessibilityAnnouncementRequestedNotification,
                Some(&info),
            )
        };
        let posted = testing::take_accessibility_notifications();
        assert_eq!(posted.len(), 1);
        let (element, name, info) = &posted[0];
        assert!(element.as_deref().is_some_and(|e| std::ptr::eq(e, &**view as &AnyObject)));
        assert_eq!(name, "AXAnnouncementRequested");
        let said = info.as_ref().and_then(|i| i.objectForKey(key)).and_then(|v| v.downcast::<NSString>().ok());
        assert_eq!(said.map(|s| s.to_string()).as_deref(), Some("Saved"));
        // Posted from another thread, it waits in the main thread's store.
        let element = std::thread::spawn(|| {
            let element = objc2_foundation::NSObject::new();
            unsafe {
                objc2_app_kit::NSAccessibilityPostNotificationWithUserInfo(
                    &element,
                    objc2_app_kit::NSAccessibilityAnnouncementRequestedNotification,
                    None,
                )
            };
            // Kept alive for the main thread to find.
            objc2::rc::Retained::into_raw(element) as usize
        })
        .join()
        .unwrap();
        assert!(testing::take_accessibility_notifications().is_empty(), "not until the main loop runs");
        testing::settle();
        let posted = testing::take_accessibility_notifications();
        assert_eq!(posted.len(), 1);
        assert!(posted[0].0.as_deref().is_some_and(|e| std::ptr::eq(e, element as *const AnyObject)));
        drop(unsafe { Retained::from_raw(element as *mut objc2_foundation::NSObject) });
    }

    define_class!(
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxSweepOverriding"]
        struct Overriding;

        impl Overriding {
            #[unsafe(method(setNeedsDisplayInRect:))]
            fn needs_display(&self, r: NSRect) {
                let _: () = unsafe { msg_send![super(self), setNeedsDisplayInRect: r] };
            }
        }
    );

    /// The cost of `setNeedsDisplay:` for a view whose class doesn't
    /// override `setNeedsDisplayInRect:` (the check, then Rust) and for one
    /// that does (the check, then a message), in a window on screen.
    fn funnel_timing(mtm: MainThreadMarker) {
        if std::env::var_os("SIDESTEP_FUNNEL_BENCH").is_none() {
            return;
        }
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        let plain = NSView::initWithFrame(NSView::alloc(mtm), rect(10.0, 10.0, 20.0, 20.0));
        let overriding: Retained<Overriding> =
            unsafe { msg_send![Overriding::alloc(mtm), initWithFrame: rect(40.0, 10.0, 20.0, 20.0)] };
        content.addSubview(&plain);
        content.addSubview(&overriding);
        let w = window(mtm, &content);
        w.orderFront(None);
        testing::settle();
        const N: u32 = 1_000_000;
        let time = |view: &NSView| {
            let start = Instant::now();
            for _ in 0..N {
                view.setNeedsDisplay(true);
            }
            start.elapsed().as_nanos() as f64 / f64::from(N)
        };
        let (a, b) = (time(&plain), time(&overriding));
        let rect_only = {
            let start = Instant::now();
            for _ in 0..N {
                plain.setNeedsDisplayInRect(plain.bounds());
            }
            start.elapsed().as_nanos() as f64 / f64::from(N)
        };
        println!(
            "funnel: setNeedsDisplay: {a:.1} ns plain, {b:.1} ns overridden; setNeedsDisplayInRect: {rect_only:.1} ns"
        );
        w.orderOut(None);
        testing::settle();
    }

    define_class!(
        /// Fills its bounds, as a message's parts would draw.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxSweepFilled"]
        struct Filled;

        impl Filled {
            #[unsafe(method(drawRect:))]
            fn draw(&self, _dirty: NSRect) {
                objc2_app_kit::NSColor::systemBlueColor().setFill();
                objc2_app_kit::NSRectFill(self.bounds());
            }

            #[unsafe(method(isFlipped))]
            fn flipped(&self) -> bool {
                true
            }
        }
    );

    fn filled(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
        let v: Retained<Filled> = unsafe { msg_send![Filled::alloc(mtm), initWithFrame: frame] };
        v.into_super()
    }

    /// The display pass's cost for a transcript of views that don't clip
    /// (a view per message, three parts each, in a scroll view), against
    /// the same views all clipping: a small change, and scrolling by
    /// less than a screen (new tiles). Median microseconds per pass.
    fn draw_pass_timing(mtm: MainThreadMarker) {
        if std::env::var_os("SIDESTEP_DRAW_BENCH").is_none() {
            return;
        }
        const MESSAGES: usize = 2000;
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 800.0, 600.0));
        let scroll = objc2_app_kit::NSScrollView::initWithFrame(
            objc2_app_kit::NSScrollView::alloc(mtm),
            rect(0.0, 0.0, 800.0, 600.0),
        );
        let document = filled(mtm, rect(0.0, 0.0, 780.0, 60.0 * MESSAGES as f64));
        let mut all = vec![document.clone()];
        for i in 0..MESSAGES {
            let message = filled(mtm, rect(10.0, 60.0 * i as f64, 760.0, 56.0));
            for j in 0..3 {
                let part = filled(mtm, rect(8.0 + 250.0 * j as f64, 8.0, 240.0, 40.0));
                message.addSubview(&part);
                all.push(part);
            }
            document.addSubview(&message);
            all.push(message);
        }
        scroll.setDocumentView(Some(&document));
        content.addSubview(&scroll);
        let w = window(mtm, &content);
        w.orderFront(None);
        testing::settle();
        let clip = scroll.contentView();
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let pass = |change: &dyn Fn(usize)| {
            let mut times = Vec::new();
            for i in 0..200 {
                change(i);
                let start = Instant::now();
                testing::display_now(&w);
                times.push(start.elapsed().as_secs_f64() * 1e6);
                testing::settle();
            }
            median(times)
        };
        for clipping in [false, true] {
            for v in &all {
                let _: () = unsafe { msg_send![&**v, setClipsToBounds: clipping] };
            }
            clip.scrollToPoint(NSPoint::new(0.0, 0.0));
            testing::settle();
            let small = pass(&|i| all[1 + (i % 20) * 4].setNeedsDisplay(true));
            let scrolled = pass(&|i| {
                let y = 60_000.0 * (i as f64 / 200.0) + 37.0 * (i % 7) as f64;
                clip.scrollToPoint(NSPoint::new(0.0, y));
                scroll.reflectScrolledClipView(&clip);
            });
            let whole = pass(&|_| content.setNeedsDisplay(true));
            // A message on screen grows and shrinks, as a streamed one
            // does.
            clip.scrollToPoint(NSPoint::new(0.0, 0.0));
            testing::settle();
            let grown = pass(&|i| all[24].setFrameSize(NSSize::new(760.0, 56.0 - (i % 2) as f64)));
            println!(
                "draw pass ({}): small change {small:.0} us, scrolling {scrolled:.0} us, whole window {whole:.0} us, \
                 a message resized {grown:.0} us",
                if clipping { "all views clip" } else { "views don't clip" }
            );
        }
        w.orderOut(None);
        testing::settle();
    }

    /// A view that doesn't clip shows what its subviews draw outside it:
    /// hiding it, taking it out, moving it or making it clip redraws
    /// that too, and damage there draws them.
    fn overflow_is_redrawn(mtm: MainThreadMarker) {
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        let parent = NSView::initWithFrame(NSView::alloc(mtm), rect(50.0, 50.0, 40.0, 40.0));
        let child = filled(mtm, rect(30.0, 30.0, 40.0, 40.0));
        let label = objc2_app_kit::NSTextField::labelWithString(&NSString::from_str("Out"), mtm);
        label.setFrame(rect(0.0, 0.0, 40.0, 20.0));
        child.addSubview(&label);
        parent.addSubview(&child);
        content.addSubview(&parent);
        let w = window(mtm, &content);
        w.orderFront(None);
        testing::settle();
        // The parent is at x 50 to 90, the child at 80 to 120; from the
        // content's top (200 high), y 110 to 150 and 80 to 120.
        let covers = |what: &str, want: [f32; 4]| {
            let damage = testing::root_damage(&w);
            let hull = damage.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |a, r| {
                [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])]
            });
            assert!(
                hull[0] <= want[0] && hull[1] <= want[1] && hull[2] >= want[2] && hull[3] >= want[3],
                "{what}: {damage:?} doesn't cover {want:?}"
            );
            testing::settle();
        };
        let reach = [50.0, 80.0, 120.0, 150.0];
        parent.setHidden(true);
        covers("hidden", reach);
        parent.setHidden(false);
        covers("shown", reach);
        parent.removeFromSuperview();
        covers("taken out", reach);
        content.addSubview(&parent);
        covers("put back", reach);
        parent.setFrameOrigin(NSPoint::new(150.0, 50.0));
        covers("moved from", reach);
        parent.setFrameOrigin(NSPoint::new(50.0, 50.0));
        testing::settle();
        let _: () = unsafe { msg_send![&*parent, setClipsToBounds: true] };
        covers("made to clip", reach);
        let _: () = unsafe { msg_send![&*parent, setClipsToBounds: false] };
        covers("made not to", reach);
        // Damage only where the child sticks out draws the child's label.
        testing::note_painted_text(true);
        content.setNeedsDisplayInRect(rect(95.0, 100.0, 20.0, 15.0));
        testing::settle();
        assert!(!testing::take_painted_text().is_empty(), "the label outside its grandparent is drawn");
        content.setNeedsDisplayInRect(rect(200.0, 150.0, 20.0, 20.0));
        testing::settle();
        assert!(testing::take_painted_text().is_empty());
        // Moved further out, and its parent resized, it is drawn where it
        // went; the parent's reach follows.
        child.setFrameOrigin(NSPoint::new(90.0, 30.0));
        parent.setFrameSize(NSSize::new(45.0, 40.0));
        testing::settle();
        testing::take_painted_text();
        content.setNeedsDisplayInRect(rect(150.0, 100.0, 20.0, 15.0));
        testing::settle();
        assert!(!testing::take_painted_text().is_empty(), "drawn where it went");
        child.removeFromSuperview();
        testing::settle();
        testing::take_painted_text();
        content.setNeedsDisplayInRect(rect(150.0, 100.0, 20.0, 15.0));
        testing::settle();
        assert!(testing::take_painted_text().is_empty(), "gone");
        testing::note_painted_text(false);
        w.orderOut(None);
        testing::settle();
    }

    type Test = (&'static str, fn(MainThreadMarker));

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::use_null_backend();
        let _ = NSApplication::sharedApplication(mtm);
        let tests: &[Test] = &[
            ("live_resize_reaches_every_view", live_resize_reaches_every_view),
            ("shown_windows_are_in_the_window_menu", shown_windows_are_in_the_window_menu),
            ("overriding_order_window_still_shows", overriding_order_window_still_shows),
            ("window_constraints", window_constraints),
            ("accessibility_notifications_are_stored", accessibility_notifications_are_stored),
            ("overflow_is_redrawn", overflow_is_redrawn),
            ("funnel_timing", funnel_timing),
            ("draw_pass_timing", draw_pass_timing),
        ];
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test(mtm));
            testing::take_render_log();
            println!("test {name} ... ok");
        }
    }
}
