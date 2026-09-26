//! Menus, pop-up buttons and alerts, written only against objc2-app-kit:
//! on macOS it runs on AppKit, on Linux on Sidestep. `SCENARIO` chooses
//! what it shows once it has launched:
//!
//! - `context` (default): the window's context menu, opened as a
//!   right-click would open it: key equivalents, check marks, a disabled
//!   item, separators and submenus.
//! - `submenu`: the same menu with its Font submenu opened from the
//!   keyboard (Down, Down, Right).
//! - `popup`: the menu popped up with its third item over a point
//!   (`popUpMenuPositioningItem:atLocation:inView:`).
//! - `buttons`: a pop-up button and a pull-down button;
//!   `popup-button` and `pulldown` open their menus.
//! - `menubar`: the main menu's Edit menu opened from the window's menu
//!   bar (Linux shows the main menu in each window).
//! - `alert`: an alert with three buttons and a text field as its
//!   accessory view, run modally; `alert-sheet`: two buttons, as a sheet.
//!
//! Right-clicking the window opens the context menu too; choosing an item
//! prints its title. MENUS_QUIT_AFTER: seconds until the program
//! terminates itself.

use std::cell::OnceCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSBezierPath,
    NSColor, NSEvent, NSEventModifierFlags, NSEventType, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSMenu, NSMenuItem, NSPopUpButton, NSResponder, NSStringDrawing, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn after(seconds: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(seconds, false, &block) }
}

define_class!(
    /// The window's content: a pale ground with a hint.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MenusBoard"]
    struct Board;

    impl Board {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.93, 0.93, 0.92, 1.0).setFill();
            NSBezierPath::fillRect(self.bounds());
            let font = NSFont::systemFontOfSize(14.0);
            let ink = NSColor::colorWithSRGBRed_green_blue_alpha(0.3, 0.3, 0.32, 1.0);
            let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
            let values: [&AnyObject; 2] = [&font, &ink];
            let attributes = NSDictionary::from_slices(&keys, &values);
            let label = NSString::from_str("Right-click for a menu");
            unsafe { label.drawAtPoint_withAttributes(NSPoint::new(20.0, 20.0), Some(&attributes)) };
        }
    }
);

define_class!(
    /// Hears what the menus choose.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MenusTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(chose:))]
        fn chose(&self, sender: &NSMenuItem) {
            println!("chose {}", sender.title());
        }

        #[unsafe(method(picked:))]
        fn picked(&self, sender: &NSPopUpButton) {
            println!("picked {} ({})", sender.title(), sender.indexOfSelectedItem());
        }

        #[unsafe(method(toggle:))]
        fn toggle(&self, sender: &NSMenuItem) {
            sender.setState(if sender.state() == 0 { 1 } else { 0 });
            println!("toggled {} to {}", sender.title(), sender.state());
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

fn item(menu: &NSMenu, title: &str, action: Option<Sel>, key: &str, target: &Target) -> Retained<NSMenuItem> {
    let item = unsafe {
        menu.addItemWithTitle_action_keyEquivalent(&NSString::from_str(title), action, &NSString::from_str(key))
    };
    if action.is_some() {
        unsafe { item.setTarget(Some(target)) };
    }
    item
}

/// A menu a text editor might show.
fn editor_menu(mtm: MainThreadMarker, target: &Target) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Editor"));
    item(&menu, "Cut", Some(sel!(chose:)), "x", target);
    item(&menu, "Copy", Some(sel!(chose:)), "c", target);
    // Nobody has paste:, so it shows disabled.
    item(&menu, "Paste", Some(sel!(paste:)), "v", target);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let font = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Font"));
    let bold = item(&font, "Bold", Some(sel!(toggle:)), "b", target);
    bold.setState(1);
    item(&font, "Italic", Some(sel!(toggle:)), "i", target);
    item(&font, "Underline", Some(sel!(toggle:)), "u", target);
    font.addItem(&NSMenuItem::separatorItem(mtm));
    let bigger = item(&font, "Bigger", Some(sel!(chose:)), "+", target);
    bigger.setKeyEquivalentModifierMask(NSEventModifierFlags::Control);
    let smaller = item(&font, "Smaller", Some(sel!(chose:)), "-", target);
    smaller.setKeyEquivalentModifierMask(NSEventModifierFlags::Control);
    let font_host = item(&menu, "Font", None, "", target);
    menu.setSubmenu_forItem(Some(&font), &font_host);
    let spelling = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Spelling"));
    item(&spelling, "Check Document Now", Some(sel!(chose:)), ";", target);
    item(&spelling, "Check Spelling While Typing", Some(sel!(toggle:)), "", target);
    let spelling_host = item(&menu, "Spelling and Grammar", None, "", target);
    menu.setSubmenu_forItem(Some(&spelling), &spelling_host);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let wrap = item(&menu, "Wrap Lines", Some(sel!(toggle:)), "", target);
    wrap.setState(1);
    let invisibles = item(&menu, "Show Invisibles", Some(sel!(toggle:)), "I", target);
    invisibles.setKeyEquivalentModifierMask(NSEventModifierFlags::Control);
    let find = item(&menu, "Find Next", Some(sel!(chose:)), "\u{F704}", target);
    find.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
    menu
}

/// A key press, posted for the menu's tracking loop to take.
fn post_key(mtm: MainThreadMarker, window: &NSWindow, chars: &str, code: u16) {
    let key = NSString::from_str(chars);
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::ZERO,
        NSEventModifierFlags::empty(),
        0.0,
        window.windowNumber(),
        None,
        &key,
        &key,
        false,
        code,
    )
    .expect("a key event");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
}

/// A main menu: the application's, File, Edit and View.
fn main_menu(mtm: MainThreadMarker, target: &Target) -> Retained<NSMenu> {
    let main = NSMenu::new(mtm);
    let submenu = |title: &str, items: &[(&str, &str, Option<Sel>)]| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
        for &(name, key, action) in items {
            if name == "-" {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            } else {
                item(&menu, name, action, key, target);
            }
        }
        let host = NSMenuItem::new(mtm);
        host.setSubmenu(Some(&menu));
        main.addItem(&host);
        menu
    };
    let app = submenu(
        "",
        &[("About Menus", "", Some(sel!(chose:))), ("-", "", None), ("Quit Menus", "q", Some(sel!(terminate:)))],
    );
    let _ = app;
    submenu(
        "File",
        &[
            ("New", "n", Some(sel!(chose:))),
            ("Open…", "o", Some(sel!(chose:))),
            ("-", "", None),
            ("Close", "w", Some(sel!(chose:))),
        ],
    );
    submenu(
        "Edit",
        &[
            ("Undo", "z", Some(sel!(chose:))),
            ("Redo", "Z", Some(sel!(chose:))),
            ("-", "", None),
            ("Cut", "x", Some(sel!(chose:))),
            ("Copy", "c", Some(sel!(chose:))),
            ("Paste", "v", Some(sel!(chose:))),
            ("Select All", "a", Some(sel!(chose:))),
        ],
    );
    submenu(
        "View",
        &[
            ("Show Sidebar", "s", Some(sel!(toggle:))),
            ("Zoom In", "+", Some(sel!(chose:))),
            ("Zoom Out", "-", Some(sel!(chose:))),
        ],
    );
    main
}

#[derive(Default)]
struct DelegateIvars {
    kept: OnceCell<Vec<Retained<AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MenusDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _note: &NSNotification) {
            self.launched();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }
    }
);

impl Delegate {
    fn launched(&self) {
        let mtm = self.mtm();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 640.0, 420.0),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Menus"));
        let board: Retained<Board> = unsafe { msg_send![super(Board::alloc(mtm).set_ivars(())), init] };
        let target: Retained<Target> = unsafe { msg_send![super(Target::alloc(mtm).set_ivars(())), init] };
        let menu = editor_menu(mtm, &target);
        unsafe { board.setMenu(Some(&menu)) };
        NSApplication::sharedApplication(mtm).setMainMenu(Some(&main_menu(mtm, &target)));
        // A pop-up button and a pull-down, top left.
        let sizes =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(20.0, 370.0, 160.0, 24.0), false);
        for title in ["Small", "Medium", "Large", "Extra Large"] {
            sizes.addItemWithTitle(&NSString::from_str(title));
        }
        sizes.selectItemAtIndex(1);
        let actions =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), rect(200.0, 370.0, 120.0, 24.0), true);
        for title in ["Actions", "Duplicate", "Rename…", "Move to Trash"] {
            actions.addItemWithTitle(&NSString::from_str(title));
        }
        for b in [&sizes, &actions] {
            unsafe {
                b.setTarget(Some(&target));
                b.setAction(Some(sel!(picked:)));
            }
            board.addSubview(b);
        }
        window.setContentView(Some(&board));
        window.makeKeyAndOrderFront(None);

        let scenario = std::env::var("SCENARIO").unwrap_or_else(|_| "context".into());
        let mut kept: Vec<Retained<AnyObject>> =
            vec![Retained::into_super(Retained::into_super(target)), Retained::into_super(Retained::into_super(menu))];
        let (w, b) = (window.clone(), board.clone());
        let (sizes, actions) = (sizes.clone(), actions.clone());
        let open = after(0.5, move || {
            let mtm = MainThreadMarker::new().unwrap();
            let menu = b.menu().expect("the board has a menu");
            match scenario.as_str() {
                "buttons" => {}
                "alert" => {
                    let alert = NSAlert::new(mtm);
                    alert.setMessageText(&NSString::from_str("Do you want to save the changes made to “Notes”?"));
                    alert.setInformativeText(&NSString::from_str("Your changes will be lost if you don’t save them."));
                    alert.addButtonWithTitle(&NSString::from_str("Save"));
                    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
                    alert.addButtonWithTitle(&NSString::from_str("Don’t Save"));
                    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(0.0, 0.0, 240.0, 22.0));
                    field.setPlaceholderString(Some(&NSString::from_str("File name")));
                    alert.setAccessoryView(Some(&field));
                    alert.setShowsSuppressionButton(true);
                    println!("alert returned {}", alert.runModal());
                }
                "alert-sheet" => {
                    let alert = NSAlert::new(mtm);
                    alert.setMessageText(&NSString::from_str("Delete “Notes”?"));
                    alert.setInformativeText(&NSString::from_str("You can’t undo this action."));
                    alert.addButtonWithTitle(&NSString::from_str("Delete"));
                    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
                    let handler = RcBlock::new(|code: isize| println!("sheet ended with {code}"));
                    alert.beginSheetModalForWindow_completionHandler(&w, Some(&handler));
                }
                "menubar" => {
                    // A click on the bar's third title, Edit (Linux draws the
                    // bar above the content; macOS ignores the click).
                    let content = b.frame().size.height;
                    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                        NSEventType::LeftMouseDown,
                        NSPoint::new(180.0, content + 10.0),
                        NSEventModifierFlags::empty(),
                        0.0,
                        w.windowNumber(),
                        None,
                        0,
                        1,
                        1.0,
                    )
                    .expect("a mouse event");
                    NSApplication::sharedApplication(mtm).sendEvent(&event);
                }
                "popup-button" => unsafe { sizes.performClick(None) },
                "pulldown" => unsafe { actions.performClick(None) },
                "popup" => {
                    let third = menu.itemAtIndex(2);
                    let chosen = menu.popUpMenuPositioningItem_atLocation_inView(
                        third.as_deref(),
                        NSPoint::new(200.0, 300.0),
                        Some(&b),
                    );
                    println!("popUpMenuPositioningItem returned {chosen}");
                }
                other => {
                    if other == "submenu" {
                        // Taken by the menu's loop once it runs.
                        post_key(mtm, &w, "\u{F701}", 125);
                        post_key(mtm, &w, "\u{F701}", 125);
                        post_key(mtm, &w, "\u{F701}", 125);
                        post_key(mtm, &w, "\u{F703}", 124);
                    }
                    let at = NSPoint::new(120.0, 330.0);
                    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                        NSEventType::RightMouseDown,
                        at,
                        NSEventModifierFlags::empty(),
                        0.0,
                        w.windowNumber(),
                        None,
                        0,
                        1,
                        1.0,
                    )
                    .expect("a mouse event");
                    NSMenu::popUpContextMenu_withEvent_forView(&menu, &event, &b);
                }
            }
        });
        kept.push(Retained::into_super(Retained::into_super(open)));
        kept.push(Retained::into_super(Retained::into_super(Retained::into_super(window))));
        if let Some(secs) = std::env::var("MENUS_QUIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()) {
            let quit =
                after(secs, || NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None));
            unsafe {
                objc2_foundation::NSRunLoop::currentRunLoop()
                    .addTimer_forMode(&quit, objc2_foundation::NSRunLoopCommonModes)
            };
            kept.push(Retained::into_super(Retained::into_super(quit)));
        }
        let _ = self.ivars().kept.set(kept);
    }
}

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate: Retained<Delegate> =
        unsafe { msg_send![super(Delegate::alloc(mtm).set_ivars(DelegateIvars::default())), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
