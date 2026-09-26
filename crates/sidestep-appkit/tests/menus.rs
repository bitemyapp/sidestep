//! Menus on Linux, through the null render thread: the main menu's key
//! equivalents as typed on a keyboard, and pop-up and context menus
//! tracked with the pointer and the keys, playing the compositor's part
//! (`sidestep_appkit::testing`). What Apple's AppKit does without showing
//! a menu is pinned by `conformance/tests/menus.rs`.
//!
//! A menu's loop runs inside the call that opens it, so input for an open
//! menu is injected either before the call (keys, which go to the key
//! window) or from a timer that fires inside the loop (the pointer over
//! the menu's window, which only exists once the menu shows).
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
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSEvent, NSEventModifierFlags, NSMenu, NSMenuDelegate, NSMenuItem,
        NSPopUpButton, NSResponder, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{
        NSNotification, NSNotificationCenter, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString,
        NSTimer,
    };
    use sidestep_appkit::testing::{self, Seen};

    thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

    fn log(line: String) {
        LOG.with(|l| l.borrow_mut().push(line));
    }

    fn take_log() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    fn s(text: &str) -> Retained<NSString> {
        NSString::from_str(text)
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    const COMMAND: usize = NSEventModifierFlags::Command.0;
    const CONTROL: usize = NSEventModifierFlags::Control.0;
    const SHIFT: usize = NSEventModifierFlags::Shift.0;
    const FUNCTION: usize = NSEventModifierFlags::Function.0;

    define_class!(
        /// Logs the actions menus send it, and the keys that reach it.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxMenusProbe"]
        pub(crate) struct Probe;

        impl Probe {
            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }

            #[unsafe(method(keyDown:))]
            fn key_down(&self, event: &NSEvent) {
                let chars = event.characters().map(|c| c.to_string()).unwrap_or_default();
                log(format!("keyDown: {chars}"));
            }

            #[unsafe(method(act:))]
            fn act(&self, sender: &NSMenuItem) {
                log(format!("act: {}", sender.title()));
            }

            #[unsafe(method(picked:))]
            fn picked(&self, sender: &NSPopUpButton) {
                log(format!("picked: {} at {}", sender.title(), sender.indexOfSelectedItem()));
            }

            /// Everything it's asked about is enabled (validation still
            /// asks, as the benchmark wants).
            #[unsafe(method(validateMenuItem:))]
            fn validate_menu_item(&self, _item: &NSMenuItem) -> bool {
                true
            }
        }
    );

    fn probe(mtm: MainThreadMarker) -> Retained<Probe> {
        unsafe { msg_send![super(Probe::alloc(mtm).set_ivars(())), initWithFrame: rect(0.0, 0.0, 300.0, 200.0)] }
    }

    define_class!(
        /// Logs what a menu tells its delegate.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxMenusDelegate"]
        pub(crate) struct Delegate;

        impl Delegate {
            #[unsafe(method(menuNeedsUpdate:))]
            fn menu_needs_update(&self, menu: &NSMenu) {
                log(format!("menuNeedsUpdate: {}", menu.title()));
            }

            #[unsafe(method(menuWillOpen:))]
            fn menu_will_open(&self, menu: &NSMenu) {
                log(format!("menuWillOpen: {}", menu.title()));
            }

            #[unsafe(method(menuDidClose:))]
            fn menu_did_close(&self, menu: &NSMenu) {
                log(format!("menuDidClose: {}", menu.title()));
            }

            #[unsafe(method(menu:willHighlightItem:))]
            fn will_highlight(&self, _menu: &NSMenu, item: Option<&NSMenuItem>) {
                log(format!("willHighlightItem: {}", item.map(|i| i.title().to_string()).unwrap_or_default()));
            }
        }

        unsafe impl NSObjectProtocol for Delegate {}
        unsafe impl NSMenuDelegate for Delegate {}
    );

    fn delegate(mtm: MainThreadMarker) -> Retained<Delegate> {
        unsafe { msg_send![super(Delegate::alloc(mtm).set_ivars(())), init] }
    }

    define_class!(
        /// Fills its menu in: three items, the second disabled.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxMenusFiller"]
        pub(crate) struct Filler;

        impl Filler {
            #[unsafe(method(numberOfItemsInMenu:))]
            fn number_of_items(&self, _menu: &NSMenu) -> isize {
                log("numberOfItemsInMenu:".into());
                3
            }

            #[unsafe(method(menu:updateItem:atIndex:shouldCancel:))]
            fn update_item(&self, _menu: &NSMenu, item: &NSMenuItem, index: isize, _cancel: bool) -> bool {
                item.setTitle(&s(&format!("filled {index}")));
                log(format!("updateItem: {index}"));
                index < 1
            }
        }

        unsafe impl NSObjectProtocol for Filler {}
        unsafe impl NSMenuDelegate for Filler {}
    );

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
        w.makeFirstResponder(Some(content));
        testing::settle();
        let id = testing::showing_id(&w);
        (w, id)
    }

    fn close(w: &NSWindow) {
        w.orderOut(None);
        testing::settle();
        testing::take_render_log();
        take_log();
    }

    /// A menu of `titles` sending `act:` to `target` ("-" for a separator,
    /// a leading "!" for an item nobody can perform, so disabled).
    fn menu_of(mtm: MainThreadMarker, title: &str, titles: &[&str], target: &AnyObject) -> Retained<NSMenu> {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s(title));
        for t in titles {
            if *t == "-" {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
                continue;
            }
            let (t, action) = match t.strip_prefix('!') {
                Some(rest) => (rest, sel!(nobodyHasThis:)),
                None => (*t, sel!(act:)),
            };
            let item = unsafe { menu.addItemWithTitle_action_keyEquivalent(&s(t), Some(action), &s("")) };
            unsafe { item.setTarget(Some(target)) };
        }
        menu
    }

    /// The menu windows on screen, outermost first, with their ids.
    fn menu_windows(mtm: MainThreadMarker) -> Vec<(Retained<NSWindow>, u32)> {
        let windows = NSApplication::sharedApplication(mtm).windows();
        windows
            .iter()
            .filter(|w| w.isVisible() && w.class().name().to_str() == Ok("_SidestepMenuWindow"))
            .map(|w| {
                let id = testing::showing_id(&w);
                (w, id)
            })
            .collect()
    }

    /// Where the middle of row `row` of a menu of `rows` rows, none a
    /// separator, is in its window (from the top left): the rows share the
    /// menu's height less 5 points above and below.
    fn row_center(menu: &NSMenu, rows: usize, row: usize) -> (f64, f64) {
        let size = menu.size();
        let height = (size.height - 10.0) / rows as f64;
        (size.width / 2.0, 5.0 + height * (row as f64 + 0.5))
    }

    /// Run `f` after `ms` milliseconds, in whatever loop runs then (a
    /// menu's included).
    fn after(ms: u64, f: impl Fn() + 'static) -> Retained<NSTimer> {
        let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(ms as f64 / 1000.0, false, &block) };
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        timer
    }

    /// Run `f` once, in whatever loop runs (a menu's included), as soon as
    /// `ready` holds: looked at every 5 ms, so a test waits for a state
    /// rather than for a time.
    fn when(ready: impl Fn() -> bool + 'static, f: impl Fn() + 'static) -> Retained<NSTimer> {
        let done = std::cell::Cell::new(false);
        let block = RcBlock::new(move |timer: NonNull<NSTimer>| {
            if !done.get() && ready() {
                done.set(true);
                unsafe { timer.as_ref() }.invalidate();
                f();
            }
        });
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.005, true, &block) };
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        timer
    }

    /// Whether `menu` shows `title` highlighted.
    fn lit(menu: &NSMenu, title: &str) -> bool {
        menu.highlightedItem().is_some_and(|h| h.title().to_string() == title)
    }

    /// Log the menu notifications posted.
    fn observe_menus() -> Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>> {
        let names = unsafe {
            [
                objc2_app_kit::NSMenuDidBeginTrackingNotification,
                objc2_app_kit::NSMenuDidEndTrackingNotification,
                objc2_app_kit::NSMenuWillSendActionNotification,
                objc2_app_kit::NSMenuDidSendActionNotification,
            ]
        };
        names
            .iter()
            .map(|name| {
                let block = RcBlock::new(|note: NonNull<NSNotification>| {
                    let note = unsafe { note.as_ref() };
                    log(note.name().to_string());
                });
                unsafe {
                    NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        None,
                        &block,
                    )
                }
            })
            .collect()
    }

    fn stop_observing(tokens: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>) {
        for t in tokens {
            unsafe { NSNotificationCenter::defaultCenter().removeObserver(t.as_ref()) };
        }
    }

    fn key(window: u32, code: u16, chars: &str, unmodified: &str, modifiers: usize) {
        testing::inject_key(window, code, chars, unmodified, true, modifiers);
        testing::inject_key(window, code, chars, unmodified, false, modifiers);
    }

    const DOWN: &str = "\u{F701}";
    const RIGHT: &str = "\u{F703}";

    /// Keys typed on the keyboard find the main menu's items: Command
    /// (Super) and Control shortcuts, shifted ones either way, function
    /// keys; other keys go to the first responder.
    fn key_equivalents_from_the_keyboard(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let view = probe(mtm);
        let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Main"));
        let file = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("File"));
        let add = |menu: &NSMenu, title: &str, key: &str, mask: usize| {
            let item = unsafe { menu.addItemWithTitle_action_keyEquivalent(&s(title), Some(sel!(act:)), &s(key)) };
            item.setKeyEquivalentModifierMask(NSEventModifierFlags(mask));
            unsafe { item.setTarget(Some(&view)) };
        };
        add(&file, "Quit", "q", COMMAND);
        add(&file, "Redo", "z", COMMAND | SHIFT);
        add(&file, "Bang", "1", CONTROL | SHIFT);
        add(&file, "Kill", "k", CONTROL);
        add(&file, "Refresh", "\u{F708}", 0);
        let host = NSMenuItem::new(mtm);
        host.setSubmenu(Some(&file));
        main.addItem(&host);
        app.setMainMenu(Some(&main));
        let (w, id) = shown(mtm, &view);
        take_log();
        // XKB keycodes: Q 24, Z 52, 1 10, K 45, A 38, F5 71.
        key(id, 24, "q", "q", COMMAND);
        key(id, 52, "Z", "Z", COMMAND | SHIFT);
        key(id, 10, "!", "!", CONTROL | SHIFT);
        key(id, 45, "\u{b}", "k", CONTROL);
        key(id, 71, "\u{F708}", "\u{F708}", FUNCTION);
        key(id, 38, "a", "a", 0);
        testing::settle();
        assert_eq!(take_log(), ["act: Quit", "act: Redo", "act: Bang", "act: Kill", "act: Refresh", "keyDown: a"]);
        app.setMainMenu(None);
        close(&w);
    }

    /// A right click pops the view's menu up; the pointer highlights, the
    /// release chooses; the calls come in AppKit's order (as
    /// `conformance/tests/menus.rs` pins it on macOS).
    fn context_menus_choose_with_the_pointer(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let menu = menu_of(mtm, "Context", &["One", "Two", "Three"], &view);
        let d = delegate(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        unsafe { view.setMenu(Some(&menu)) };
        let (w, id) = shown(mtm, &view);
        let tokens = observe_menus();
        take_log();
        testing::take_render_log();
        let m = menu.clone();
        let _timer = after(10, move || {
            let (_, popup) = menu_windows(mtm).pop().expect("the menu shows");
            let (x, y) = row_center(&m, 3, 1);
            testing::inject_motion(popup, x, y, 0);
            testing::inject_button(popup, x, y, 1, false, 1, 0);
        });
        testing::inject_button(id, 40.0, 50.0, 1, true, 1, 0);
        testing::settle();
        let log = testing::take_render_log();
        assert!(
            log.iter().any(|s| matches!(s, Seen::Created { popup_of: Some(parent), .. } if *parent == id)),
            "{log:?}"
        );
        assert_eq!(
            take_log(),
            [
                "menuNeedsUpdate: Context",
                "NSMenuDidBeginTrackingNotification",
                "menuWillOpen: Context",
                "willHighlightItem: Two",
                "willHighlightItem: ",
                "menuDidClose: Context",
                "NSMenuDidEndTrackingNotification",
                "NSMenuWillSendActionNotification",
                "act: Two",
                "NSMenuDidSendActionNotification",
            ]
        );
        assert!(menu_windows(mtm).is_empty());
        stop_observing(tokens);
        close(&w);
    }

    /// Up, Down, Return: disabled items and separators are passed over.
    fn keys_move_and_choose(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let menu = menu_of(mtm, "Keys", &["Apple", "!Banana", "-", "Cherry"], &view);
        let (w, id) = shown(mtm, &view);
        take_log();
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 36, "\r", "\r", 0);
        let chosen = menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view));
        assert!(chosen);
        assert_eq!(take_log(), ["act: Cherry"]);
        // Wrapping round from the last.
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 65, " ", " ", 0);
        assert!(menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        assert_eq!(take_log(), ["act: Apple"]);
        // A letter finds the next item it starts.
        key(id, 54, "c", "c", 0);
        key(id, 36, "\r", "\r", 0);
        assert!(menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        assert_eq!(take_log(), ["act: Cherry"]);
        close(&w);
    }

    /// Escape, cancelTracking and the compositor taking the popup away all
    /// close the menu with nothing chosen; the delegate hears it closed.
    fn menus_close_without_a_choice(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let menu = menu_of(mtm, "Closing", &["Apple", "Banana"], &view);
        let d = delegate(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        let (w, id) = shown(mtm, &view);
        take_log();
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 9, "\u{1b}", "\u{1b}", 0);
        assert!(!menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        let log = take_log();
        assert_eq!(log.last().map(String::as_str), Some("menuDidClose: Closing"), "{log:?}");
        assert!(!log.iter().any(|l| l.starts_with("act:")));

        let m = menu.clone();
        let _timer = after(10, move || m.cancelTracking());
        assert!(!menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        assert_eq!(take_log().last().map(String::as_str), Some("menuDidClose: Closing"));

        // The compositor dismissing the popup orders its window out.
        let _timer = after(10, move || {
            let (window, _) = menu_windows(mtm).pop().expect("the menu shows");
            window.orderOut(None);
        });
        assert!(!menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        assert_eq!(take_log().last().map(String::as_str), Some("menuDidClose: Closing"));

        // A press outside the menu.
        let _timer = after(10, move || testing::inject_button(id, 250.0, 20.0, 0, true, 1, 0));
        assert!(!menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        testing::inject_button(id, 250.0, 20.0, 0, false, 1, 0);
        testing::settle();
        assert!(menu_windows(mtm).is_empty());
        close(&w);
    }

    /// The release of the click that opened the menu, right away, leaves it
    /// open; a click on an item then chooses it.
    fn click_then_click(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let menu = menu_of(mtm, "Clicks", &["Apple", "Banana"], &view);
        unsafe { view.setMenu(Some(&menu)) };
        let (w, id) = shown(mtm, &view);
        take_log();
        let m = menu.clone();
        let _up = after(10, move || testing::inject_button(id, 40.0, 50.0, 1, false, 1, 0));
        let _click = after(60, move || {
            let (_, popup) = menu_windows(mtm).pop().expect("the menu stays open");
            let (x, y) = row_center(&m, 2, 0);
            testing::inject_button(popup, x, y, 0, true, 1, 0);
            testing::inject_button(popup, x, y, 0, false, 1, 0);
        });
        testing::inject_button(id, 40.0, 50.0, 1, true, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["act: Apple"]);
        close(&w);
    }

    /// Resting on an item with a submenu opens it; the pointer goes on into
    /// it and chooses there. Right opens one from the keyboard.
    fn submenus(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let menu = menu_of(mtm, "Top", &["Apple", "Banana"], &view);
        let sub = menu_of(mtm, "Sub", &["Cherry", "Damson"], &view);
        let d = delegate(mtm);
        sub.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        let host = NSMenuItem::new(mtm);
        host.setTitle(&s("More"));
        host.setSubmenu(Some(&sub));
        menu.addItem(&host);
        let (w, id) = shown(mtm, &view);
        take_log();
        testing::take_render_log();
        let m = menu.clone();
        let _hover = after(10, move || {
            let (_, popup) = menu_windows(mtm).pop().expect("the menu shows");
            let (x, y) = row_center(&m, 3, 2);
            testing::inject_motion(popup, x, y, 0);
        });
        let (sub2, log_ids) = (sub.clone(), std::rc::Rc::new(RefCell::new(Vec::new())));
        let ids = log_ids.clone();
        let _choose = after(400, move || {
            let open = menu_windows(mtm);
            ids.borrow_mut().extend(open.iter().map(|(_, id)| *id));
            let (_, popup) = open.last().cloned().expect("a menu shows");
            let (x, y) = row_center(&sub2, 2, 1);
            testing::inject_motion(popup, x, y, 0);
            testing::inject_button(popup, x, y, 0, false, 1, 0);
        });
        let chosen = menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view));
        assert!(chosen);
        let ids = log_ids.borrow().clone();
        assert_eq!(ids.len(), 2, "the menu and its submenu show");
        let log = testing::take_render_log();
        assert!(
            log.iter().any(|s| matches!(s, Seen::Created { popup_of: Some(p), .. } if *p == ids[0])),
            "the submenu is a popup of the menu: {log:?}"
        );
        let log = take_log();
        assert!(log.contains(&"menuWillOpen: Sub".to_string()), "{log:?}");
        assert_eq!(&log[log.len() - 2..], ["menuDidClose: Sub", "act: Damson"]);

        // From the keyboard: Down to More, Right opens it on its first item.
        for _ in 0..3 {
            key(id, 116, DOWN, DOWN, FUNCTION);
        }
        key(id, 114, RIGHT, RIGHT, FUNCTION);
        key(id, 36, "\r", "\r", 0);
        assert!(menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        assert!(take_log().ends_with(&["act: Cherry".to_string()]));
        close(&w);
    }

    /// A delegate that says how many items there are fills them in, until
    /// it says to stop.
    fn delegates_fill_menus_in(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let menu = menu_of(mtm, "Filled", &["old 0", "old 1", "old 2", "old 3", "old 4"], &view);
        let filler: Retained<Filler> = unsafe { msg_send![super(Filler::alloc(mtm).set_ivars(())), init] };
        menu.setDelegate(Some(ProtocolObject::from_ref(&*filler)));
        let (w, id) = shown(mtm, &view);
        take_log();
        key(id, 9, "\u{1b}", "\u{1b}", 0);
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view));
        assert_eq!(take_log(), ["numberOfItemsInMenu:", "updateItem: 0", "updateItem: 1"]);
        let titles: Vec<String> = menu.itemArray().iter().map(|i| i.title().to_string()).collect();
        assert_eq!(titles, ["filled 0", "filled 1", "old 2"]);
        close(&w);
    }

    /// A menu changing while it tracks keeps its highlight on the same
    /// item, never takes a row that went away for another, and shows
    /// again at its new size.
    fn menus_change_while_they_track(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let (w, id) = shown(mtm, &view);
        let at = NSPoint::new(20.0, 150.0);
        take_log();

        // An item put in above the highlighted one: Return still chooses
        // the one highlighted.
        let menu = menu_of(mtm, "Growing", &["One", "Two", "Three", "Four"], &view);
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 116, DOWN, DOWN, FUNCTION);
        let (m, m2, target) = (menu.clone(), menu.clone(), view.clone());
        let _t = when(
            move || lit(&m2, "Two"),
            move || {
                let zero = unsafe {
                    m.insertItemWithTitle_action_keyEquivalent_atIndex(&s("Zero"), Some(sel!(act:)), &s(""), 0)
                };
                unsafe { zero.setTarget(Some(&target)) };
                key(id, 36, "\r", "\r", 0);
            },
        );
        testing::take_render_log();
        assert!(menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(&view)));
        assert_eq!(take_log(), ["act: Two"]);
        // It grew, so it showed again, taller.
        testing::settle();
        let heights: Vec<u32> = testing::take_render_log()
            .iter()
            .filter_map(|s| match s {
                Seen::Created { height, popup_of: Some(p), .. } if *p == id => Some(*height),
                _ => None,
            })
            .collect();
        assert!(heights.len() == 2 && heights[1] > heights[0], "{heights:?}");

        // The highlighted item and the one above it go: nothing is
        // highlighted, and Down starts from the top.
        let menu = menu_of(mtm, "Shrinking", &["One", "Two", "Three", "Four"], &view);
        for _ in 0..4 {
            key(id, 116, DOWN, DOWN, FUNCTION);
        }
        let (m, m2) = (menu.clone(), menu.clone());
        let _t = when(
            move || lit(&m2, "Four"),
            move || {
                m.removeItemAtIndex(3);
                m.removeItemAtIndex(2);
                assert!(m.highlightedItem().is_none());
                key(id, 116, DOWN, DOWN, FUNCTION);
                key(id, 36, "\r", "\r", 0);
            },
        );
        assert!(menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(&view)));
        assert_eq!(take_log(), ["act: One"]);

        // A submenu waiting to open whose host goes away doesn't open (and
        // one that opened first closes when its host goes).
        let menu = menu_of(mtm, "Hosting", &["Apple", "Banana"], &view);
        let sub = menu_of(mtm, "Sub", &["Cherry"], &view);
        let host = NSMenuItem::new(mtm);
        host.setTitle(&s("More"));
        host.setSubmenu(Some(&sub));
        menu.addItem(&host);
        let m = menu.clone();
        let _hover = after(10, move || {
            let (_, popup) = menu_windows(mtm).pop().expect("the menu shows");
            let (x, y) = row_center(&m, 3, 2);
            testing::inject_motion(popup, x, y, 0);
        });
        // Once it's highlighted (the submenu waits 150 ms to open, but a
        // slow machine may have opened it: it closes then).
        let (m, m2) = (menu.clone(), menu.clone());
        let _gone = when(move || lit(&m2, "More"), move || m.removeItemAtIndex(2));
        let m = menu.clone();
        let _later = when(
            move || m.numberOfItems() == 2,
            move || {
                let _check = after(300, move || {
                    log(format!("menus open: {}", menu_windows(mtm).len()));
                    key(id, 9, "\u{1b}", "\u{1b}", 0);
                });
            },
        );
        assert!(!menu.popUpMenuPositioningItem_atLocation_inView(None, at, Some(&view)));
        assert_eq!(take_log(), ["menus open: 1"]);
        close(&w);
    }

    /// The window a menu belongs to leaving the screen closes the menu
    /// first (a popup can't outlive its parent), and tracking ends.
    fn windows_leaving_close_their_menus(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let menu = menu_of(mtm, "Leaving", &["Apple", "Banana"], &view);
        let d = delegate(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*d)));
        let (w, id) = shown(mtm, &view);
        take_log();
        testing::take_render_log();
        let w2 = w.clone();
        let menu_id = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = menu_id.clone();
        let _t = after(20, move || {
            seen.set(menu_windows(mtm).pop().expect("the menu shows").1);
            w2.orderOut(None);
        });
        assert!(!menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        testing::settle();
        let closed: Vec<u32> = testing::take_render_log()
            .iter()
            .filter_map(|s| match s {
                Seen::Closed { window } => Some(*window),
                _ => None,
            })
            .collect();
        assert_eq!(closed, [menu_id.get(), id]);
        assert_eq!(take_log().last().map(String::as_str), Some("menuDidClose: Leaving"));
        assert!(menu_windows(mtm).is_empty());
        close(&w);
    }

    /// A pop-up button in a plain view, its action going to `probe`.
    fn pop_up_button(
        mtm: MainThreadMarker,
        probe: &Probe,
        pulls_down: bool,
        titles: &[&str],
    ) -> Retained<NSPopUpButton> {
        let b = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            rect(20.0, 150.0, 120.0, 24.0),
            pulls_down,
        );
        for t in titles {
            b.addItemWithTitle(&s(t));
        }
        unsafe {
            b.setTarget(Some(probe));
            b.setAction(Some(sel!(picked:)));
        }
        probe.addSubview(&b);
        b
    }

    /// A click pops the menu up; choosing an item selects it and sends the
    /// button's action. A pull-down shows its items but the first, and its
    /// title stays.
    fn pop_up_buttons(mtm: MainThreadMarker) {
        let view = probe(mtm);
        let b = pop_up_button(mtm, &view, false, &["One", "Two", "Three"]);
        let (w, id) = shown(mtm, &view);
        let note = unsafe { objc2_app_kit::NSPopUpButtonWillPopUpNotification };
        let block = RcBlock::new(|n: NonNull<NSNotification>| log(unsafe { n.as_ref() }.name().to_string()));
        let token = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(note),
                None,
                None,
                &block,
            )
        };
        take_log();
        testing::take_render_log();
        let menu = b.menu().unwrap();
        let _choose = after(10, move || {
            let (_, popup) = menu_windows(mtm).pop().expect("the menu shows");
            let (x, y) = row_center(&menu, 3, 2);
            testing::inject_motion(popup, x, y, 0);
            testing::inject_button(popup, x, y, 0, true, 1, 0);
            testing::inject_button(popup, x, y, 0, false, 1, 0);
        });
        // The button is 26 to 50 points from the top of the 200-point view.
        testing::inject_button(id, 60.0, 38.0, 0, true, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["NSPopUpButtonWillPopUpNotification", "picked: Three at 2"]);
        assert_eq!(b.title().to_string(), "Three");
        let log = testing::take_render_log();
        assert!(log.iter().any(|s| matches!(s, Seen::Created { popup_of: Some(p), .. } if *p == id)), "{log:?}");
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(token.as_ref()) };

        // From the keyboard: the selected item is highlighted to start.
        key(id, 116, DOWN, DOWN, FUNCTION);
        key(id, 36, "\r", "\r", 0);
        unsafe { b.performClick(None) };
        assert_eq!(take_log(), ["picked: One at 0"]);

        // A pull-down.
        b.removeFromSuperview();
        let p = pop_up_button(mtm, &view, true, &["Title", "Apple", "Banana"]);
        let menu = p.menu().unwrap();
        let _choose = after(10, move || {
            let (_, popup) = menu_windows(mtm).pop().expect("the menu shows");
            // Two rows: the first item, the title, isn't one.
            let (x, y) = row_center(&menu, 2, 1);
            testing::inject_motion(popup, x, y, 0);
            testing::inject_button(popup, x, y, 0, false, 1, 0);
        });
        testing::inject_button(id, 60.0, 38.0, 0, true, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["picked: Title at 2"]);
        assert_eq!(p.indexOfSelectedItem(), 2);
        assert!(!p.itemAtIndex(0).unwrap().isHidden());
        close(&w);
    }

    /// A main menu of three menus sending `act:` to `target`.
    fn main_menu(mtm: MainThreadMarker, target: &AnyObject) -> Retained<NSMenu> {
        let main = NSMenu::new(mtm);
        for (title, items) in [("App", &["About", "Quit"][..]), ("File", &["New", "Open"]), ("Edit", &["Cut", "Copy"])]
        {
            let sub = menu_of(mtm, title, items, target);
            let host = NSMenuItem::new(mtm);
            host.setSubmenu(Some(&sub));
            main.addItem(&host);
        }
        main
    }

    /// Windows show the main menu as a bar above their content, which keeps
    /// its size; the frame takes the bar in. Panels don't show it, and
    /// hiding it takes it away.
    fn the_main_menu_is_a_bar(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let view = probe(mtm);
        let main = main_menu(mtm, &view);
        app.setMainMenu(Some(&main));
        let (w, _) = shown(mtm, &view);
        testing::settle();
        let bar = main.menuBarHeight();
        assert!(bar >= 24.0, "{bar}");
        assert_eq!(NSMenu::new(mtm).menuBarHeight(), 0.0);
        assert_eq!(w.contentLayoutRect().size, NSSize::new(300.0, 200.0));
        assert_eq!(w.frame().size, NSSize::new(300.0, 200.0 + bar));
        assert_eq!(w.frameRectForContentRect(rect(0.0, 0.0, 300.0, 200.0)).size.height, 200.0 + bar);
        assert_eq!(view.frame().size, NSSize::new(300.0, 200.0));
        // A panel shows none.
        let panel = objc2_app_kit::NSPanel::initWithContentRect_styleMask_backing_defer(
            objc2_app_kit::NSPanel::alloc(mtm),
            rect(0.0, 0.0, 100.0, 80.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::UtilityWindow,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.orderFront(None);
        testing::settle();
        assert_eq!(panel.frame().size.height, 80.0);
        panel.orderOut(None);
        // Hidden, the bar goes; shown, it comes back.
        NSMenu::setMenuBarVisible(false, mtm);
        testing::settle();
        assert_eq!(w.frame().size.height, 200.0);
        assert_eq!(main.menuBarHeight(), 0.0);
        NSMenu::setMenuBarVisible(true, mtm);
        testing::settle();
        assert_eq!(w.frame().size.height, 200.0 + bar);
        app.setMainMenu(None);
        testing::settle();
        assert_eq!(w.frame().size.height, 200.0);
        close(&w);
    }

    /// A click on a title opens its menu; F10 opens the first from the
    /// keyboard, and Right goes on to the next.
    fn the_bar_opens_its_menus(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let view = probe(mtm);
        let main = main_menu(mtm, &view);
        app.setMainMenu(Some(&main));
        let (w, id) = shown(mtm, &view);
        testing::settle();
        let bar = main.menuBarHeight();
        take_log();
        testing::take_render_log();
        // Above the content, on the first title (which starts 6 points in).
        let _choose = after(10, move || {
            let open = menu_windows(mtm);
            log(format!("menus open: {}", open.len()));
            let (_, popup) = open.last().cloned().expect("a menu shows");
            key(popup, 116, DOWN, DOWN, FUNCTION);
            key(popup, 36, "\r", "\r", 0);
        });
        testing::inject_button(id, 12.0, -bar / 2.0, 0, true, 1, 0);
        testing::settle();
        assert_eq!(take_log(), ["menus open: 1", "act: About"]);
        let log = testing::take_render_log();
        assert!(log.iter().any(|s| matches!(s, Seen::Created { popup_of: Some(p), .. } if *p == id)), "{log:?}");
        testing::inject_button(id, 12.0, -bar / 2.0, 0, false, 1, 0);
        testing::settle();
        // F10, Right to File, whose first item is highlighted; Return.
        key(id, 76, "\u{F70D}", "\u{F70D}", FUNCTION);
        key(id, 114, RIGHT, RIGHT, FUNCTION);
        key(id, 36, "\r", "\r", 0);
        testing::settle();
        assert_eq!(take_log(), ["act: New"]);
        app.setMainMenu(None);
        close(&w);
    }

    type Test = (&'static str, fn(MainThreadMarker));

    /// Median microseconds per call of `f` over seven runs of `iters`.
    fn median(iters: u32, mut f: impl FnMut()) -> f64 {
        let mut runs: Vec<f64> = (0..7)
            .map(|_| {
                let start = std::time::Instant::now();
                for _ in 0..iters {
                    f();
                }
                start.elapsed().as_secs_f64() * 1e6 / f64::from(iters)
            })
            .collect();
        runs.sort_by(f64::total_cmp);
        runs[3]
    }

    /// What menus cost on the main thread (not a test; run it by name, in
    /// release mode:
    /// `scripts/linux-cargo test --release -p sidestep-appkit --test menus -- bench`):
    ///
    /// - a key no item has, offered to a main menu of 10 menus of 20 items
    ///   that all have key equivalents and a target whose
    ///   `validateMenuItem:` enables them, as every Command and Control key
    ///   press is (it updates, asking the validator of each of the 200
    ///   items, and scans them);
    /// - `size` of a 20-item menu laid out again with titles never laid out
    ///   before (the text engine's cache misses for them; the key labels,
    ///   the same each time, hit it), and with nothing new (every line from
    ///   the cache);
    /// - a context menu of 20 items popped up and closed by an Escape
    ///   already queued, through the null render thread: preparing and
    ///   validating the menu, its window, the popup request and the
    ///   tracking loop (the loop takes the Escape before a display pass
    ///   runs, so this leaves drawing out).
    ///
    /// Drawing an open menu and the bar isn't timed here.
    fn bench_menus(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        let view = probe(mtm);
        let masks = [COMMAND, COMMAND | SHIFT, CONTROL, COMMAND | CONTROL, CONTROL | SHIFT];
        let main = NSMenu::new(mtm);
        for m in 0..10 {
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s(&format!("Menu {m}")));
            for i in 0..20u8 {
                let key = char::from(b'a' + i).to_string();
                let title = format!("Item {i} of menu {m}");
                let item =
                    unsafe { menu.addItemWithTitle_action_keyEquivalent(&s(&title), Some(sel!(act:)), &s(&key)) };
                item.setKeyEquivalentModifierMask(NSEventModifierFlags(masks[m % masks.len()]));
                unsafe { item.setTarget(Some(&view)) };
            }
            let host = NSMenuItem::new(mtm);
            host.setSubmenu(Some(&menu));
            main.addItem(&host);
        }
        app.setMainMenu(Some(&main));
        let (w, id) = shown(mtm, &view);
        testing::settle();
        let miss = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            objc2_app_kit::NSEventType::KeyDown,
            NSPoint::ZERO,
            NSEventModifierFlags(COMMAND | NSEventModifierFlags::Option.0),
            0.0,
            w.windowNumber(),
            None,
            &s("9"),
            &s("9"),
            false,
            18,
        )
        .expect("a key event");
        assert!(!main.performKeyEquivalent(&miss));
        let scan = median(2000, || assert!(!main.performKeyEquivalent(&miss)));
        println!("key equivalent, 200 items, no match {scan:>9.2} µs");

        let menu = main.itemAtIndex(3).and_then(|i| i.submenu()).expect("a menu");
        let mut fresh = 0u64;
        let cold = median(200, || {
            for item in menu.itemArray().iter() {
                fresh += 1;
                item.setTitle(&s(&format!("Item number {fresh}")));
            }
            std::hint::black_box(menu.size());
        });
        println!("layout of a 20-item menu, new titles {cold:>8.2} µs (setting the titles included)");
        let warm = median(2000, || {
            std::hint::black_box(menu.size());
        });
        println!("layout of a 20-item menu, cached text {warm:>7.2} µs");

        let open = median(50, || {
            key(id, 9, "\u{1b}", "\u{1b}", 0);
            assert!(!menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(20.0, 150.0), Some(&view)));
        });
        println!("context menu opened and closed      {open:>9.2} µs (no drawing)");
        take_log();
        app.setMainMenu(None);
        w.orderOut(None);
    }

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::use_null_backend();
        let tests: &[Test] = &[
            ("key_equivalents_from_the_keyboard", key_equivalents_from_the_keyboard),
            ("context_menus_choose_with_the_pointer", context_menus_choose_with_the_pointer),
            ("keys_move_and_choose", keys_move_and_choose),
            ("menus_close_without_a_choice", menus_close_without_a_choice),
            ("click_then_click", click_then_click),
            ("submenus", submenus),
            ("delegates_fill_menus_in", delegates_fill_menus_in),
            ("menus_change_while_they_track", menus_change_while_they_track),
            ("windows_leaving_close_their_menus", windows_leaving_close_their_menus),
            ("pop_up_buttons", pop_up_buttons),
            ("the_main_menu_is_a_bar", the_main_menu_is_a_bar),
            ("the_bar_opens_its_menus", the_bar_opens_its_menus),
        ];
        let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
        if only.as_deref() == Some("bench") {
            objc2::rc::autoreleasepool(|_| bench_menus(mtm));
            return;
        }
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
