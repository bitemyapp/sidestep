//! Input and window behaviour on Linux, through the null render thread:
//! each test shows windows that no compositor displays and plays the
//! compositor's part itself (`sidestep_appkit::testing`), so what the
//! program sees, and when, is exact. What Apple's AppKit does is pinned by
//! `conformance/`; these cover what only a real display could show there:
//! input arriving from the render thread, focus and activation, pointer
//! tracking and cursors.
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

    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSApplication, NSApplicationDelegate, NSBackingStoreType, NSEvent, NSEventMask, NSEventType, NSPanel,
        NSResponder, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    };
    use objc2_foundation::{NSNotification, NSNumber, NSPoint, NSRect, NSSize};
    use sidestep_appkit::testing::{self, Seen};

    thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

    fn log(line: String) {
        LOG.with(|l| l.borrow_mut().push(line));
    }

    fn take_log() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    pub(crate) struct ProbeIvars {
        name: &'static str,
        /// What a mouse down does: nothing more, or follow the drag in a
        /// loop of its own until the button comes up.
        tracks: bool,
    }

    define_class!(
        /// A view that writes down what it gets.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsProbe"]
        #[ivars = ProbeIvars]
        pub(crate) struct Probe;

        impl Probe {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                true
            }

            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, event: &NSEvent) {
                log(format!("{} mouseDown: {}", self.ivars().name, at(self, event)));
                if !self.ivars().tracks {
                    return;
                }
                let window = self.window().expect("in a window");
                let mask = NSEventMask::LeftMouseDragged | NSEventMask::LeftMouseUp;
                while let Some(e) = window.nextEventMatchingMask(mask) {
                    let what = if e.r#type() == NSEventType::LeftMouseUp { "up" } else { "drag" };
                    log(format!("{} tracked {what} {}", self.ivars().name, at(self, &e)));
                    if e.r#type() == NSEventType::LeftMouseUp {
                        break;
                    }
                }
            }

            #[unsafe(method(mouseUp:))]
            fn mouse_up(&self, event: &NSEvent) {
                log(format!("{} mouseUp: {}", self.ivars().name, at(self, event)));
            }

            #[unsafe(method(keyDown:))]
            fn key_down(&self, event: &NSEvent) {
                let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
                log(format!("{} keyDown: {chars}", self.ivars().name));
                // And up the chain, to the window.
                unsafe { msg_send![super(self), keyDown: event] }
            }
        }

        unsafe impl NSObjectProtocol for Probe {}
    );

    impl Probe {
        fn new(mtm: MainThreadMarker, name: &'static str, frame: NSRect, tracks: bool) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(ProbeIvars { name, tracks });
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }
    }

    fn at(view: &NSView, event: &NSEvent) -> String {
        let p = view.convertPoint_fromView(event.locationInWindow(), None);
        format!("{:.0},{:.0}", p.x, p.y)
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    /// A titled window on "screen", settled: configured and key.
    fn shown(mtm: MainThreadMarker, content: &NSView) -> (Retained<NSWindow>, u32) {
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable;
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 300.0, 200.0),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { w.setReleasedWhenClosed(false) };
        w.setContentView(Some(content));
        w.makeKeyAndOrderFront(None);
        testing::settle();
        let id = testing::showing_id(&w);
        (w, id)
    }

    fn windows_show_and_take_the_keyboard(mtm: MainThreadMarker) {
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let (w, id) = shown(mtm, &view);
        assert!(w.isVisible());
        assert!(w.isKeyWindow() && w.isMainWindow());
        assert!(NSApplication::sharedApplication(mtm).isActive());
        let log = testing::take_render_log();
        let created = Seen::Created { window: id, width: 300, height: 200, popup_of: None, sheet_of: None };
        assert!(log.contains(&created), "{log:?}");
        w.orderOut(None);
        testing::settle();
        assert!(!w.isVisible() && !w.isKeyWindow());
        assert!(testing::take_render_log().contains(&Seen::Closed { window: id }));
        take_log();
    }

    fn input_reaches_views_in_order(mtm: MainThreadMarker) {
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let (w, id) = shown(mtm, &view);
        testing::inject_enter(id, 10.0, 10.0);
        testing::inject_button(id, 10.0, 20.0, 0, true, 1, 0);
        testing::inject_button(id, 10.0, 20.0, 0, false, 1, 0);
        testing::inject_key(id, 38, "a", "a", true, 0);
        testing::settle();
        assert_eq!(take_log(), ["content mouseDown: 10,20", "content mouseUp: 10,20", "content keyDown: a"]);
        w.orderOut(None);
        testing::settle();
    }

    /// A loop nested in a handler carries on with the input after the event
    /// being handled, even input that arrived with it.
    fn a_nested_loop_gets_the_rest_of_a_batch(mtm: MainThreadMarker) {
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), true);
        let (w, id) = shown(mtm, &view);
        testing::inject_button(id, 5.0, 5.0, 0, true, 1, 0);
        testing::inject_motion(id, 6.0, 7.0, 0);
        testing::inject_motion(id, 8.0, 9.0, 0);
        testing::inject_button(id, 8.0, 9.0, 0, false, 1, 0);
        testing::inject_key(id, 38, "a", "a", true, 0);
        testing::settle();
        // The two moves are coalesced into the later one; the key waits for
        // the loop to end.
        assert_eq!(
            take_log(),
            ["content mouseDown: 5,5", "content tracked drag 8,9", "content tracked up 8,9", "content keyDown: a"]
        );
        w.orderOut(None);
        testing::settle();
    }

    define_class!(
        /// A view whose click handler waits by running the run loop itself,
        /// while a key arrives.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsSpinner"]
        pub(crate) struct Spinner;

        impl Spinner {
            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, _: &NSEvent) {
                log("spinner mouseDown: begins".into());
                let id = testing::showing_id(&self.window().unwrap());
                testing::inject_key(id, 38, "a", "a", true, 0);
                for _ in 0..3 {
                    let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.02);
                    let mode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
                    objc2_foundation::NSRunLoop::currentRunLoop().runMode_beforeDate(mode, &until);
                }
                log("spinner mouseDown: ends".into());
            }

            #[unsafe(method(keyDown:))]
            fn key_down(&self, event: &NSEvent) {
                let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
                log(format!("spinner keyDown: {chars}"));
            }
        }
    );

    /// A handler running the run loop itself isn't entered again: input
    /// waits for AppKit's loop.
    fn handlers_spinning_the_run_loop_get_no_input(mtm: MainThreadMarker) {
        let view: Retained<Spinner> =
            unsafe { msg_send![super(Spinner::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 300.0, 200.0)] };
        let (w, id) = shown(mtm, &view);
        w.makeFirstResponder(Some(&view));
        take_log();
        testing::inject_button(id, 5.0, 5.0, 0, true, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["spinner mouseDown: begins", "spinner mouseDown: ends", "spinner keyDown: a"]);
        w.orderOut(None);
        testing::settle();
    }

    /// Input that arrives while a timer inside a modal loop runs the run
    /// loop itself waits for the timer, and then the modal loop sends it at
    /// once, without waiting for more.
    fn input_during_a_timers_own_run_reaches_the_modal_loop(mtm: MainThreadMarker) {
        let view = Probe::new(mtm, "modal", rect(0.0, 0.0, 200.0, 100.0), false);
        let modal = titled_window(mtm, &view, 200.0, 100.0);
        modal.makeFirstResponder(Some(&view));
        let start = std::time::Instant::now();
        let spin = block2::RcBlock::new(|_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            let id = testing::showing_id(&app.modalWindow().unwrap());
            testing::inject_key(id, 38, "a", "a", true, 0);
            let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
            let mode = unsafe { objc2_foundation::NSDefaultRunLoopMode };
            objc2_foundation::NSRunLoop::currentRunLoop().runMode_beforeDate(mode, &until);
            log("timer done".into());
        });
        let stop = block2::RcBlock::new(|_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
            log("stopping".into());
            NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).stopModal();
        });
        let common = unsafe { objc2_foundation::NSRunLoopCommonModes };
        let run_loop = objc2_foundation::NSRunLoop::currentRunLoop();
        let spinner = unsafe { objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(0.05, false, &spin) };
        let stopper = unsafe { objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(1.5, false, &stop) };
        unsafe {
            run_loop.addTimer_forMode(&spinner, common);
            run_loop.addTimer_forMode(&stopper, common);
        }
        take_log();
        NSApplication::sharedApplication(mtm).runModalForWindow(&modal);
        assert_eq!(take_log(), ["timer done", "modal keyDown: a", "stopping"]);
        assert!(start.elapsed().as_secs_f64() >= 1.4);
        modal.orderOut(None);
        testing::settle();
    }

    define_class!(
        /// A view that copies.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsCopier"]
        pub(crate) struct Copier;

        impl Copier {
            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }

            #[unsafe(method(copy:))]
            fn copy(&self, _sender: Option<&objc2::runtime::AnyObject>) {
                log("copier copy:".into());
            }
        }
    );

    /// With a panel key over a document window, actions nobody in the
    /// panel handles go on to the main window's responders.
    fn actions_reach_the_main_window_under_a_key_panel(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let copier: Retained<Copier> =
            unsafe { msg_send![super(Copier::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 300.0, 200.0)] };
        let (w, _) = shown(mtm, &copier);
        w.makeFirstResponder(Some(&copier));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect(0.0, 0.0, 100.0, 100.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::UtilityWindow,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.makeKeyAndOrderFront(None);
        testing::settle();
        assert!(app.keyWindow().is_some_and(|k| std::ptr::eq(&*k, &**panel as &NSWindow)));
        assert!(app.mainWindow().is_some_and(|m| std::ptr::eq(&*m, &*w)));
        take_log();
        let copy = objc2::sel!(copy:);
        let target = unsafe { app.targetForAction(copy) };
        assert!(target.is_some_and(|t| std::ptr::eq(&*t, &**copier as &objc2::runtime::AnyObject)));
        assert!(unsafe { app.sendAction_to_from(copy, None, None) });
        assert_eq!(take_log(), ["copier copy:"]);
        panel.orderOut(None);
        w.orderOut(None);
        testing::settle();
    }

    define_class!(
        /// An application and window delegate that writes down what it
        /// hears.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsDelegate"]
        pub(crate) struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl NSWindowDelegate for Delegate {
            #[unsafe(method(windowDidBecomeKey:))]
            fn did_become_key(&self, _: &NSNotification) {
                log("windowDidBecomeKey:".into());
            }

            #[unsafe(method(windowDidResignKey:))]
            fn did_resign_key(&self, _: &NSNotification) {
                log("windowDidResignKey:".into());
            }

            #[unsafe(method(windowDidResize:))]
            fn did_resize(&self, n: &NSNotification) {
                let w: Retained<NSWindow> = n.object().unwrap().downcast().unwrap();
                log(format!("windowDidResize: {}", w.contentLayoutRect().size.width));
            }

            #[unsafe(method(windowWillStartLiveResize:))]
            fn will_start_live_resize(&self, n: &NSNotification) {
                let w: Retained<NSWindow> = n.object().unwrap().downcast().unwrap();
                log(format!("windowWillStartLiveResize: {}", w.inLiveResize()));
            }

            #[unsafe(method(windowDidEndLiveResize:))]
            fn did_end_live_resize(&self, n: &NSNotification) {
                let w: Retained<NSWindow> = n.object().unwrap().downcast().unwrap();
                log(format!("windowDidEndLiveResize: {}", w.inLiveResize()));
            }

            #[unsafe(method(windowDidChangeBackingProperties:))]
            fn did_change_backing(&self, n: &NSNotification) {
                let info = n.userInfo().expect("user info");
                let key = unsafe { objc2_app_kit::NSBackingPropertyOldScaleFactorKey };
                let old: Retained<NSNumber> = info.objectForKey(key).unwrap().downcast().unwrap();
                log(format!("windowDidChangeBackingProperties: from {}", old.doubleValue()));
            }

            #[unsafe(method(windowWillClose:))]
            fn will_close(&self, _: &NSNotification) {
                log("windowWillClose:".into());
            }
        }

        impl Delegate {
            #[unsafe(method(defaultsChanged:))]
            fn defaults_changed(&self, _: &NSNotification) {
                log("defaults changed".into());
            }
        }

        unsafe impl NSApplicationDelegate for Delegate {
            #[unsafe(method(applicationWillBecomeActive:))]
            fn will_become_active(&self, _: &NSNotification) {
                log("applicationWillBecomeActive:".into());
            }

            #[unsafe(method(applicationDidBecomeActive:))]
            fn did_become_active(&self, _: &NSNotification) {
                log("applicationDidBecomeActive:".into());
            }

            #[unsafe(method(applicationWillResignActive:))]
            fn will_resign_active(&self, _: &NSNotification) {
                log("applicationWillResignActive:".into());
            }

            #[unsafe(method(applicationDidResignActive:))]
            fn did_resign_active(&self, _: &NSNotification) {
                log("applicationDidResignActive:".into());
            }

            #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
            fn should_terminate_after_last_window_closed(&self, _: &NSApplication) -> bool {
                log("applicationShouldTerminateAfterLastWindowClosed:".into());
                false
            }
        }
    );

    impl Delegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            unsafe { msg_send![super(Self::alloc(mtm).set_ivars(())), init] }
        }
    }

    fn focus_and_activation_notify(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let delegate = Delegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let (w, id) = shown(mtm, &view);
        w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        take_log();
        testing::inject_focus(id, false);
        testing::settle();
        // Losing the keyboard resigns key at once, and the application's
        // activity a moment later, unless a window of ours gets it back.
        assert_eq!(take_log(), ["windowDidResignKey:"]);
        testing::run_for(80);
        assert_eq!(take_log(), ["applicationWillResignActive:", "applicationDidResignActive:"]);
        testing::inject_focus(id, true);
        testing::settle();
        assert_eq!(take_log(), ["windowDidBecomeKey:", "applicationWillBecomeActive:", "applicationDidBecomeActive:"]);
        // Focus leaving and coming back in one batch of input changes
        // nothing.
        testing::inject_focus(id, false);
        testing::inject_focus(id, true);
        testing::settle();
        testing::run_for(80);
        assert_eq!(take_log(), Vec::<String>::new());
        // In two, the window resigns and becomes key again, but the
        // application stays active.
        testing::inject_focus(id, false);
        testing::settle();
        testing::inject_focus(id, true);
        testing::settle();
        testing::run_for(80);
        assert_eq!(take_log(), ["windowDidResignKey:", "windowDidBecomeKey:"]);
        w.setDelegate(None);
        app.setDelegate(None);
        w.orderOut(None);
        testing::settle();
        take_log();
    }

    fn live_resizes_and_scale_changes_notify(mtm: MainThreadMarker) {
        let delegate = Delegate::new(mtm);
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let (w, id) = shown(mtm, &view);
        w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        testing::inject_configure(id, 320, 200, 1.0, true, false);
        testing::inject_configure(id, 340, 200, 1.0, true, false);
        testing::inject_configure(id, 340, 200, 1.0, false, false);
        testing::settle();
        assert_eq!(
            take_log(),
            [
                "windowWillStartLiveResize: true",
                "windowDidResize: 320",
                "windowDidResize: 340",
                "windowDidEndLiveResize: false",
            ]
        );
        testing::inject_configure(id, 340, 200, 2.0, false, false);
        testing::settle();
        assert_eq!(take_log(), ["windowDidChangeBackingProperties: from 1"]);
        assert_eq!(w.backingScaleFactor(), 2.0);
        w.setDelegate(None);
        w.orderOut(None);
        testing::settle();
    }

    /// A window's frame is autosaved once a live resize ends, not at each
    /// step, and a frame just read or already saved isn't written again.
    fn live_resizes_autosave_once(mtm: MainThreadMarker) {
        let delegate = Delegate::new(mtm);
        let name = objc2_foundation::NSString::from_str("LinuxEventsAutosave");
        NSWindow::removeFrameUsingName(&name, mtm);
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let (w, id) = shown(mtm, &view);
        let center = objc2_foundation::NSNotificationCenter::defaultCenter();
        let changed = unsafe { objc2_foundation::NSUserDefaultsDidChangeNotification };
        unsafe {
            center.addObserver_selector_name_object(&delegate, objc2::sel!(defaultsChanged:), Some(changed), None)
        };
        assert!(w.setFrameAutosaveName(&name));
        testing::inject_configure(id, 320, 200, 1.0, true, false);
        testing::inject_configure(id, 330, 200, 1.0, true, false);
        testing::inject_configure(id, 340, 200, 1.0, true, false);
        testing::settle();
        assert_eq!(take_log(), Vec::<String>::new());
        testing::inject_configure(id, 340, 200, 1.0, false, false);
        testing::settle();
        assert_eq!(take_log(), ["defaults changed"]);
        // Moved to where it is, nothing is written.
        w.setFrameOrigin(w.frame().origin);
        // Another window given the name after this one lets go reads the
        // frame, and doesn't write it back.
        w.setFrameAutosaveName(&objc2_foundation::NSString::from_str(""));
        let other =
            titled_window(mtm, &NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0)), 10.0, 10.0);
        assert!(other.setFrameAutosaveName(&name));
        assert_eq!(other.contentLayoutRect().size.width, 340.0);
        assert_eq!(take_log(), Vec::<String>::new());
        unsafe { center.removeObserver(&delegate) };
        other.setFrameAutosaveName(&objc2_foundation::NSString::from_str(""));
        NSWindow::removeFrameUsingName(&name, mtm);
        w.orderOut(None);
        testing::settle();
    }

    define_class!(
        /// Closes its window when clicked.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsCloser"]
        pub(crate) struct Closer;

        impl Closer {
            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, _: &NSEvent) {
                self.window().unwrap().close();
                log("closed".into());
            }
        }
    );

    fn the_last_window_closing_is_asked_about_afterwards(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let delegate = Delegate::new(mtm);
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let view: Retained<Closer> =
            unsafe { msg_send![super(Closer::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 300.0, 200.0)] };
        let (w, id) = shown(mtm, &view);
        w.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        take_log();
        testing::inject_button(id, 5.0, 5.0, 0, true, 1, 0);
        testing::settle();
        testing::run_for(80);
        // Asked once the click is handled, not inside close. (Without a
        // key window the application is no longer active, on Wayland.)
        assert_eq!(
            take_log(),
            [
                "windowWillClose:",
                "windowDidResignKey:",
                "closed",
                "applicationShouldTerminateAfterLastWindowClosed:",
                "applicationWillResignActive:",
                "applicationDidResignActive:",
            ]
        );
        w.setDelegate(None);
        app.setDelegate(None);
    }

    define_class!(
        /// A view that takes the click that activates its window.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsFirstMouse"]
        pub(crate) struct FirstMouse;

        impl FirstMouse {
            #[unsafe(method(acceptsFirstMouse:))]
            fn accepts_first_mouse(&self, _: Option<&NSEvent>) -> bool {
                true
            }

            #[unsafe(method(mouseDown:))]
            fn mouse_down(&self, _: &NSEvent) {
                log("first mouseDown:".into());
            }

            #[unsafe(method(mouseUp:))]
            fn mouse_up(&self, _: &NSEvent) {
                log("first mouseUp:".into());
            }
        }
    );

    fn activating_clicks_reach_views_that_accept_them(mtm: MainThreadMarker) {
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let (w, id) = shown(mtm, &view);
        // The click that activated the window only activated it.
        testing::inject_activating_press(id, 5.0, 5.0);
        testing::inject_button(id, 5.0, 5.0, 0, false, 1, 0);
        testing::settle();
        assert_eq!(take_log(), Vec::<String>::new());
        testing::inject_button(id, 6.0, 6.0, 0, true, 1, 0);
        testing::inject_button(id, 6.0, 6.0, 0, false, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["content mouseDown: 6,6", "content mouseUp: 6,6"]);
        // A view that accepts first mouse gets it.
        let first: Retained<FirstMouse> = unsafe {
            msg_send![super(FirstMouse::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 300.0, 200.0)]
        };
        w.setContentView(Some(&first));
        testing::inject_activating_press(id, 5.0, 5.0);
        testing::inject_button(id, 5.0, 5.0, 0, false, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["first mouseDown:", "first mouseUp:"]);
        w.orderOut(None);
        testing::settle();
    }

    fn windows_move_by_their_background(mtm: MainThreadMarker) {
        // A plain view isn't opaque: pressing it moves the window.
        let plain = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        let (w, id) = shown(mtm, &plain);
        w.setMovableByWindowBackground(true);
        testing::take_render_log();
        testing::inject_button(id, 5.0, 5.0, 0, true, 1, 0);
        testing::inject_button(id, 5.0, 5.0, 0, false, 1, 0);
        testing::settle();
        assert!(testing::take_render_log().contains(&Seen::Request { window: id, request: "Move".into() }));
        // A view that handles its clicks (the probe says it's flipped, not
        // opaque, so make it refuse): only right clicks and views that
        // won't let the window move reach views.
        w.setMovableByWindowBackground(false);
        testing::inject_button(id, 5.0, 5.0, 0, true, 1, 0);
        testing::inject_button(id, 5.0, 5.0, 0, false, 1, 0);
        testing::settle();
        assert!(!testing::take_render_log().iter().any(|s| matches!(s, Seen::Request { .. })));
        w.orderOut(None);
        testing::settle();
    }

    fn tab_through_input(mtm: MainThreadMarker) {
        let content = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let a = Probe::new(mtm, "a", rect(10.0, 10.0, 50.0, 20.0), false);
        let b = Probe::new(mtm, "b", rect(10.0, 50.0, 50.0, 20.0), false);
        content.addSubview(&a);
        content.addSubview(&b);
        let (w, id) = shown(mtm, &content);
        unsafe { a.setNextKeyView(Some(&b)) };
        w.makeFirstResponder(Some(&a));
        take_log();
        // Tab (evdev 15), then Shift-Tab.
        testing::inject_key(id, 23, "\t", "\t", true, 0);
        testing::settle();
        let first = w.firstResponder().unwrap();
        assert!(std::ptr::eq(&*first, &**b as &NSResponder));
        let shift = objc2_app_kit::NSEventModifierFlags::Shift.0;
        testing::inject_key(id, 23, "\u{19}", "\u{19}", true, shift);
        testing::settle();
        let first = w.firstResponder().unwrap();
        assert!(std::ptr::eq(&*first, &**a as &NSResponder));
        assert_eq!(
            take_log(),
            ["a keyDown: \t", "content keyDown: \t", "b keyDown: \u{19}", "content keyDown: \u{19}"]
        );
        w.orderOut(None);
        testing::settle();
    }

    fn titled_window(mtm: MainThreadMarker, content: &NSView, width: f64, height: f64) -> Retained<NSWindow> {
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, width, height),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { w.setReleasedWhenClosed(false) };
        w.setContentView(Some(content));
        w
    }

    fn sheets_take_keys_and_block_their_parent(mtm: MainThreadMarker) {
        let parent_view = Probe::new(mtm, "parent", rect(0.0, 0.0, 300.0, 200.0), false);
        let (parent, parent_id) = shown(mtm, &parent_view);
        let sheet_view = Probe::new(mtm, "sheet", rect(0.0, 0.0, 200.0, 100.0), false);
        let sheet = titled_window(mtm, &sheet_view, 200.0, 100.0);
        parent.makeFirstResponder(Some(&parent_view));
        sheet.makeFirstResponder(Some(&sheet_view));
        testing::take_render_log();
        parent.beginSheet_completionHandler(&sheet, None);
        testing::settle();
        let sheet_id = testing::showing_id(&sheet);
        let created =
            Seen::Created { window: sheet_id, width: 200, height: 100, popup_of: None, sheet_of: Some(parent_id) };
        assert!(testing::take_render_log().contains(&created));
        // The sheet is key in its parent's place; the parent stays main.
        assert!(sheet.isKeyWindow() && !parent.isKeyWindow());
        assert!(parent.isMainWindow() && !sheet.isMainWindow());
        // Keys typed on the parent's toplevel go to the sheet; clicks on the
        // parent go nowhere, clicks on the sheet reach it.
        testing::inject_key(parent_id, 38, "a", "a", true, 0);
        testing::inject_button(parent_id, 5.0, 5.0, 0, true, 1, 0);
        testing::inject_button(parent_id, 5.0, 5.0, 0, false, 1, 0);
        testing::inject_button(sheet_id, 7.0, 8.0, 0, true, 1, 0);
        testing::inject_button(sheet_id, 7.0, 8.0, 0, false, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["sheet keyDown: a", "sheet mouseDown: 7,8", "sheet mouseUp: 7,8"]);
        // Ended, the parent is key again and takes input.
        parent.endSheet(&sheet);
        testing::settle();
        assert!(parent.isKeyWindow() && !sheet.isVisible());
        testing::inject_key(parent_id, 38, "b", "b", true, 0);
        testing::settle();
        assert_eq!(take_log(), ["parent keyDown: b"]);
        parent.orderOut(None);
        testing::settle();
    }

    fn modal_loops_take_only_their_input(mtm: MainThreadMarker) {
        let other_view = Probe::new(mtm, "other", rect(0.0, 0.0, 300.0, 200.0), false);
        let (other, other_id) = shown(mtm, &other_view);
        let panel_view = Probe::new(mtm, "panel", rect(0.0, 0.0, 100.0, 100.0), false);
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect(0.0, 0.0, 100.0, 100.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::UtilityWindow,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setContentView(Some(&panel_view));
        panel.setWorksWhenModal(true);
        panel.orderFront(None);
        let modal_view = Probe::new(mtm, "modal", rect(0.0, 0.0, 200.0, 100.0), false);
        let modal = titled_window(mtm, &modal_view, 200.0, 100.0);
        testing::settle();
        let panel_id = testing::showing_id(&panel);
        take_log();
        // Clicks for each window arrive once the modal loop runs; a timer in
        // it ends it.
        let block = block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
            let modal_id = testing::showing_id(
                &NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).modalWindow().unwrap(),
            );
            for id in [other_id, modal_id, panel_id] {
                testing::inject_button(id, 5.0, 5.0, 0, true, 1, 0);
                testing::inject_button(id, 5.0, 5.0, 0, false, 1, 0);
            }
            // The panel, which works when modal, may have the keyboard.
            testing::take_render_log();
            testing::inject_focus(panel_id, true);
        });
        let stop = block2::RcBlock::new(|_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
            NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).stopModalWithCode(9);
        });
        let common = unsafe { objc2_foundation::NSRunLoopCommonModes };
        let run_loop = objc2_foundation::NSRunLoop::currentRunLoop();
        let clicks = unsafe { objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(0.02, false, &block) };
        let stopper = unsafe { objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(0.2, false, &stop) };
        unsafe {
            run_loop.addTimer_forMode(&clicks, common);
            run_loop.addTimer_forMode(&stopper, common);
        }
        let response = NSApplication::sharedApplication(mtm).runModalForWindow(&modal);
        assert_eq!(response, 9);
        assert_eq!(
            take_log(),
            ["modal mouseDown: 5,5", "modal mouseUp: 5,5", "panel mouseDown: 5,5", "panel mouseUp: 5,5"]
        );
        let modal_id = testing::showing_id(&modal);
        let log = testing::take_render_log();
        assert!(!log.iter().any(|s| matches!(s, Seen::Request { window, .. } if *window == modal_id)), "{log:?}");
        assert!(panel.isKeyWindow());
        for w in [&*modal, &*panel as &NSWindow, &*other] {
            w.orderOut(None);
        }
        testing::settle();
    }

    /// Set `NSInitialToolTipDelay`, in milliseconds.
    fn set_tool_tip_delay(ms: isize) {
        let class = objc2::runtime::AnyClass::get(c"NSUserDefaults").unwrap();
        unsafe {
            let defaults: Retained<objc2::runtime::AnyObject> = msg_send![class, standardUserDefaults];
            let key = objc2_foundation::NSString::from_str("NSInitialToolTipDelay");
            let _: () = msg_send![&*defaults, setInteger: ms, forKey: &*key];
        }
    }

    fn tool_tips_show_after_a_rest(mtm: MainThreadMarker) {
        set_tool_tip_delay(40);
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 200.0));
        let tipped = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 100.0, 100.0, 100.0));
        tipped.setToolTip(Some(&objc2_foundation::NSString::from_str("A tip")));
        content.addSubview(&tipped);
        let (w, id) = shown(mtm, &content);
        testing::take_render_log();
        let popups = |log: &[Seen]| -> Vec<u32> {
            log.iter()
                .filter_map(|s| match s {
                    Seen::Created { window, popup_of: Some(parent), .. } if *parent == id => Some(*window),
                    _ => None,
                })
                .collect()
        };
        // Resting over the view (its top left, 10 points in) brings the tip
        // up below the pointer.
        testing::inject_enter(id, 10.0, 10.0);
        testing::settle();
        assert!(popups(&testing::take_render_log()).is_empty());
        testing::run_for(120);
        testing::settle();
        let shown = popups(&testing::take_render_log());
        assert_eq!(shown.len(), 1);
        // Leaving takes it down.
        testing::inject_motion(id, 200.0, 150.0, 0);
        testing::settle();
        assert!(testing::take_render_log().contains(&Seen::Closed { window: shown[0] }));
        // A click takes it down too, and it doesn't come back until the
        // pointer moves on and rests.
        testing::inject_motion(id, 10.0, 10.0, 0);
        testing::run_for(120);
        testing::settle();
        let again = popups(&testing::take_render_log());
        assert_eq!(again.len(), 1);
        testing::inject_button(id, 10.0, 10.0, 0, true, 1, 0);
        testing::inject_button(id, 10.0, 10.0, 0, false, 1, 0);
        testing::settle();
        testing::run_for(120);
        testing::settle();
        let log = testing::take_render_log();
        assert!(log.contains(&Seen::Closed { window: again[0] }));
        assert!(popups(&log).is_empty());
        // Moving inside the view starts the wait over. (Well after the last
        // tooltip hid: right after, the wait is a tenth.)
        testing::inject_motion(id, 200.0, 150.0, 0);
        testing::settle();
        testing::run_for(600);
        set_tool_tip_delay(300);
        testing::inject_motion(id, 10.0, 10.0, 0);
        testing::run_for(200);
        testing::inject_motion(id, 12.0, 12.0, 0);
        testing::run_for(200);
        testing::settle();
        assert!(popups(&testing::take_render_log()).is_empty());
        testing::run_for(400);
        testing::settle();
        assert_eq!(popups(&testing::take_render_log()).len(), 1);
        w.orderOut(None);
        testing::settle();
        set_tool_tip_delay(0);
    }

    define_class!(
        /// Stands in for a menu: performs Command-Q.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsMenu"]
        pub(crate) struct Menu;

        impl Menu {
            #[unsafe(method(performKeyEquivalent:))]
            fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
                let key = event.charactersIgnoringModifiers().map(|c| c.to_string()).unwrap_or_default();
                log(format!("menu performKeyEquivalent: {key}"));
                key == "q"
            }
        }
    );

    fn the_main_menu_has_key_equivalents_after_the_window(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let menu: Retained<Menu> = unsafe { msg_send![super(Menu::alloc(mtm).set_ivars(())), init] };
        let _: () = unsafe { msg_send![&*app, setMainMenu: &*menu] };
        let view = Probe::new(mtm, "content", rect(0.0, 0.0, 300.0, 200.0), false);
        let (w, id) = shown(mtm, &view);
        w.makeFirstResponder(Some(&view));
        take_log();
        let command = objc2_app_kit::NSEventModifierFlags::Command.0;
        testing::inject_key(id, 32, "q", "q", true, command);
        testing::inject_key(id, 33, "w", "w", true, command);
        testing::settle();
        // The menu performs Q; W goes on to the first responder.
        let log = take_log();
        assert_eq!(log[0], "menu performKeyEquivalent: q");
        assert!(log.contains(&"menu performKeyEquivalent: w".to_string()));
        assert!(log.contains(&"content keyDown: w".to_string()) && !log.contains(&"content keyDown: q".to_string()));
        let _: () = unsafe { msg_send![&*app, setMainMenu: None::<&objc2::runtime::AnyObject>] };
        w.orderOut(None);
        testing::settle();
    }

    type Test = (&'static str, fn(MainThreadMarker));

    thread_local!(static TEXT_MATRICES: RefCell<Vec<(&'static str, [f64; 6])>> = const { RefCell::new(Vec::new()) });

    define_class!(
        /// A view that writes down the text matrix its drawing starts
        /// with, then leaves another behind.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxEventsTextMatrixProbe"]
        #[ivars = &'static str]
        pub(crate) struct TextMatrixProbe;

        impl TextMatrixProbe {
            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                use objc2_core_graphics::CGContext;
                let cg = objc2_app_kit::NSGraphicsContext::currentContext().expect("a context").CGContext();
                let m = CGContext::text_matrix(Some(&cg));
                TEXT_MATRICES.with(|t| t.borrow_mut().push((*self.ivars(), [m.a, m.b, m.c, m.d, m.tx, m.ty])));
                let scaled = objc2_core_foundation::CGAffineTransform { a: 3.0, b: 0.0, c: 0.0, d: 3.0, tx: 1.0, ty: 1.0 };
                CGContext::set_text_matrix(Some(&cg), scaled);
                CGContext::set_text_position(Some(&cg), 7.0, 8.0);
            }
        }

        unsafe impl NSObjectProtocol for TextMatrixProbe {}
    );

    /// Each view's drawing in a window starts with the identity text
    /// matrix, whatever the views drawn before it left (as each has a
    /// layer of its own on macOS).
    fn each_view_starts_with_the_identity_text_matrix(mtm: MainThreadMarker) {
        let probe = |name: &'static str, frame: NSRect| -> Retained<TextMatrixProbe> {
            let this = TextMatrixProbe::alloc(mtm).set_ivars(name);
            unsafe { msg_send![super(this), initWithFrame: frame] }
        };
        let content = probe("content", rect(0.0, 0.0, 300.0, 200.0));
        let a = probe("a", rect(10.0, 10.0, 50.0, 50.0));
        let b = probe("b", rect(70.0, 10.0, 50.0, 50.0));
        let inner = probe("inner", rect(5.0, 5.0, 20.0, 20.0));
        a.addSubview(&inner);
        content.addSubview(&a);
        content.addSubview(&b);
        let (w, _) = shown(mtm, &content);
        TEXT_MATRICES.with(|t| t.borrow_mut().clear());
        content.setNeedsDisplay(true);
        testing::display_now(&w);
        let seen = TEXT_MATRICES.with(|t| std::mem::take(&mut *t.borrow_mut()));
        let names: Vec<&str> = seen.iter().map(|s| s.0).collect();
        for name in ["content", "a", "inner", "b"] {
            assert!(names.contains(&name), "{name} drew: {names:?}");
        }
        for (name, m) in seen {
            assert_eq!(m, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], "{name}");
        }
        w.close();
        testing::settle();
    }

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::use_null_backend();
        let tests: &[Test] = &[
            ("windows_show_and_take_the_keyboard", windows_show_and_take_the_keyboard),
            ("input_reaches_views_in_order", input_reaches_views_in_order),
            ("a_nested_loop_gets_the_rest_of_a_batch", a_nested_loop_gets_the_rest_of_a_batch),
            ("handlers_spinning_the_run_loop_get_no_input", handlers_spinning_the_run_loop_get_no_input),
            (
                "input_during_a_timers_own_run_reaches_the_modal_loop",
                input_during_a_timers_own_run_reaches_the_modal_loop,
            ),
            ("actions_reach_the_main_window_under_a_key_panel", actions_reach_the_main_window_under_a_key_panel),
            ("focus_and_activation_notify", focus_and_activation_notify),
            ("live_resizes_and_scale_changes_notify", live_resizes_and_scale_changes_notify),
            ("live_resizes_autosave_once", live_resizes_autosave_once),
            ("the_last_window_closing_is_asked_about_afterwards", the_last_window_closing_is_asked_about_afterwards),
            ("activating_clicks_reach_views_that_accept_them", activating_clicks_reach_views_that_accept_them),
            ("windows_move_by_their_background", windows_move_by_their_background),
            ("tab_through_input", tab_through_input),
            ("sheets_take_keys_and_block_their_parent", sheets_take_keys_and_block_their_parent),
            ("modal_loops_take_only_their_input", modal_loops_take_only_their_input),
            ("tool_tips_show_after_a_rest", tool_tips_show_after_a_rest),
            ("the_main_menu_has_key_equivalents_after_the_window", the_main_menu_has_key_equivalents_after_the_window),
            ("each_view_starts_with_the_identity_text_matrix", each_view_starts_with_the_identity_text_matrix),
        ];
        let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
        for (name, test) in tests {
            if only.as_deref().is_some_and(|o| !name.contains(o)) {
                continue;
            }
            objc2::rc::autoreleasepool(|_| test(mtm));
            testing::take_render_log();
            println!("test {name} ... ok");
        }
    }
}
