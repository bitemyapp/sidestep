//! Appearance changes in shown windows, through the null render thread:
//! a title drawn in a dynamic system color (`labelColor`) shows in the new
//! appearance's color when its window, or a view above it, changes
//! appearance, and again when it changes back; menu bars are drawn in their
//! windows' appearance; and when the desktop switches between light and
//! dark, programs hear as macOS tells them (`NSGlobalDomain`'s
//! `AppleInterfaceStyle`, `AppleInterfaceThemeChangedNotification`, KVO on
//! the application's `effectiveAppearance`). Pixels are the null render
//! thread's (`testing::capture_pixels`), at scale 1, row 0 the top of the
//! window's content; menu bars' colors are the text it was asked to paint.
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
    use std::cell::Cell;
    use std::ffi::c_void;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceName, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSApplication, NSBackingStoreType, NSButton, NSColor, NSForegroundColorAttributeName, NSMenu, NSMenuItem,
        NSRectFill, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{
        NSDistributedNotificationCenter, NSMutableAttributedString, NSNotification, NSPoint, NSRange, NSRect, NSSize,
        NSString, NSUserDefaults,
    };
    use objc2_quartz_core::CALayer;
    use sidestep_appkit::testing;

    type Test = (&'static str, fn(MainThreadMarker));

    thread_local! {
        /// What the listener heard: KVO changes, and theme notifications.
        static HEARD: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
    }

    define_class!(
        /// Counts KVO changes and `AppleInterfaceThemeChangedNotification`s.
        #[unsafe(super(NSObject))]
        #[name = "LinuxAppearanceListener"]
        struct Listener;

        impl Listener {
            #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
            fn observe(
                &self,
                _key: Option<&NSString>,
                _object: Option<&AnyObject>,
                _change: Option<&AnyObject>,
                _context: *mut c_void,
            ) {
                HEARD.with(|h| h.set((h.get().0 + 1, h.get().1)));
            }

            #[unsafe(method(themeChanged:))]
            fn theme_changed(&self, _notification: &NSNotification) {
                HEARD.with(|h| h.set((h.get().0, h.get().1 + 1)));
            }
        }
    );

    /// Which views have layers: none, the content (and so every view in
    /// it), or only the button.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Layers {
        None,
        Content,
        Button,
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn named(name: &NSAppearanceName) -> Retained<NSAppearance> {
        NSAppearance::appearanceNamed(name).expect("a named appearance")
    }

    fn aqua() -> Retained<NSAppearance> {
        // SAFETY: AppKit's appearance name.
        named(unsafe { NSAppearanceNameAqua })
    }

    fn dark_aqua() -> Retained<NSAppearance> {
        // SAFETY: AppKit's appearance name.
        named(unsafe { NSAppearanceNameDarkAqua })
    }

    define_class!(
        /// A view that fills itself with the window background color, in
        /// its own appearance.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxAppearanceBackdrop"]
        struct Backdrop;

        impl Backdrop {
            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                NSColor::windowBackgroundColor().set();
                NSRectFill(self.bounds());
            }
        }
    );

    impl Backdrop {
        fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
            // SAFETY: NSView's initializer.
            unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
        }
    }

    /// A borderless button whose attributed title is drawn in
    /// `labelColor`, set once, as toolbars set theirs.
    fn label_button(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSButton> {
        let button = NSButton::initWithFrame(NSButton::alloc(mtm), frame);
        button.setBordered(false);
        let text = NSString::from_str("Document");
        let title = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &text);
        // SAFETY: the key is AppKit's, and its value an NSColor.
        unsafe {
            title.addAttribute_value_range(
                NSForegroundColorAttributeName,
                &NSColor::labelColor(),
                NSRange::new(0, text.length()),
            )
        };
        button.setAttributedTitle(&title);
        button
    }

    /// A shown window whose content holds
    /// `container`, a backdrop, which holds a label button at
    /// (20, 20, 120, 30) in the content.
    fn shown(mtm: MainThreadMarker, layered: Layers) -> (Retained<NSWindow>, Retained<NSView>) {
        // SAFETY: a plain window.
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 240.0, 80.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Rust owns the window, so closing it mustn't release it.
        unsafe { w.setReleasedWhenClosed(false) };
        let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 240.0, 80.0));
        content.setWantsLayer(layered == Layers::Content);
        let container = Backdrop::new(mtm, rect(10.0, 10.0, 220.0, 60.0));
        let button = label_button(mtm, rect(10.0, 10.0, 120.0, 30.0));
        if layered == Layers::Button {
            // Its own layer, with a sublayer under what it draws, as
            // toolbar buttons show hover feedback.
            button.setWantsLayer(true);
            if let Some(layer) = button.layer() {
                layer.insertSublayer_atIndex(&CALayer::new(), 0);
            }
        }
        container.addSubview(&button);
        content.addSubview(&container);
        w.setContentView(Some(&content));
        w.makeKeyAndOrderFront(None);
        testing::settle_first_frames();
        show(&w);
        (w, Retained::into_super(container))
    }

    fn show(w: &NSWindow) {
        testing::commit_layers();
        testing::settle();
        testing::display_now(w);
        testing::settle();
    }

    /// Whether the title shows lighter than what's behind it: of the
    /// button's pixels, the one furthest in lightness from its corner's
    /// (the background), and which way.
    #[track_caller]
    fn title_is_light(w: &NSWindow) -> bool {
        let (width, _, pixels) = testing::window_pixels(w).expect("captured pixels");
        let lightness = |x: u32, y: u32| {
            let p = pixels[(y * width + x) as usize];
            i32::from(p[0]) + i32::from(p[1]) + i32::from(p[2])
        };
        // The button spans x 20 to 140 and, from the top, y 30 to 60.
        let background = lightness(21, 31);
        let furthest = (30..60)
            .flat_map(|y| (20..140).map(move |x| lightness(x, y) - background))
            .max_by_key(|d| d.abs())
            .expect("pixels");
        assert!(furthest.abs() > 240, "the title shows (furthest {furthest} from the background)");
        furthest > 0
    }

    /// Whether the menu bar's titles painted since the last call are light,
    /// all of them (and some were painted: the bar was drawn again).
    #[track_caller]
    fn bar_is_light() -> bool {
        let runs: Vec<_> = testing::take_painted_text().into_iter().filter(|r| r.bar).collect();
        assert!(!runs.is_empty(), "the bar was drawn again");
        let light = runs[0].color[0] > 0.5;
        assert!(runs.iter().all(|r| (r.color[0] > 0.5) == light), "titles disagree");
        light
    }

    /// Give the application a main menu with a File menu, so titled windows
    /// show a bar, or take it away.
    fn main_menu(mtm: MainThreadMarker, on: bool) {
        let app = NSApplication::sharedApplication(mtm);
        if !on {
            app.setMainMenu(None);
            return;
        }
        let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Main"));
        let file = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("File"));
        // SAFETY: a plain item, with no action.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("File"),
                None,
                &NSString::new(),
            )
        };
        item.setSubmenu(Some(&file));
        main.addItem(&item);
        app.setMainMenu(Some(&main));
    }

    /// `AppleInterfaceStyle`, as programs read it.
    fn interface_style() -> Option<String> {
        NSUserDefaults::standardUserDefaults()
            .stringForKey(&NSString::from_str("AppleInterfaceStyle"))
            .map(|s| s.to_string())
    }

    /// The window's, then a view's appearance goes dark and back.
    fn switches(mtm: MainThreadMarker, layered: Layers) {
        let (w, container) = shown(mtm, layered);
        assert!(!title_is_light(&w), "labelColor is dark in Aqua");
        w.setAppearance(Some(&dark_aqua()));
        show(&w);
        assert!(title_is_light(&w), "labelColor is light in a Dark Aqua window");
        w.setAppearance(Some(&aqua()));
        show(&w);
        assert!(!title_is_light(&w), "and dark again in an Aqua window");
        container.setAppearance(Some(&dark_aqua()));
        show(&w);
        assert!(title_is_light(&w), "labelColor is light under a Dark Aqua view");
        container.setAppearance(None);
        show(&w);
        assert!(!title_is_light(&w), "and dark again once the view follows its window");
        w.close();
        testing::settle();
    }

    fn label_colors_follow_appearance_changes(mtm: MainThreadMarker) {
        switches(mtm, Layers::None);
    }

    fn label_colors_follow_appearance_changes_in_layers(mtm: MainThreadMarker) {
        switches(mtm, Layers::Content);
    }

    fn label_colors_follow_appearance_changes_in_their_own_layers(mtm: MainThreadMarker) {
        switches(mtm, Layers::Button);
    }

    fn menu_bars_follow_their_windows(mtm: MainThreadMarker) {
        main_menu(mtm, true);
        testing::note_painted_text(true);
        let (w, _) = shown(mtm, Layers::Content);
        assert!(!bar_is_light(), "dark titles on an Aqua window's bar");
        w.setAppearance(Some(&dark_aqua()));
        show(&w);
        assert!(bar_is_light(), "light ones on a Dark Aqua window's");
        w.setAppearance(None);
        show(&w);
        assert!(!bar_is_light(), "and dark again once the window follows the application");
        testing::note_painted_text(false);
        w.close();
        testing::settle();
        main_menu(mtm, false);
    }

    fn desktop_changes_reach_programs(mtm: MainThreadMarker) {
        let app = NSApplication::sharedApplication(mtm);
        app.setAppearance(None);
        main_menu(mtm, true);
        testing::note_painted_text(true);
        let (w, _) = shown(mtm, Layers::Content);
        assert!(!bar_is_light());
        let listener: Retained<Listener> = unsafe { msg_send![Listener::alloc(), init] };
        let key = NSString::from_str("effectiveAppearance");
        let name = NSString::from_str("AppleInterfaceThemeChangedNotification");
        let center = NSDistributedNotificationCenter::defaultCenter();
        // SAFETY: the listener answers both, and is removed below.
        unsafe {
            let _: () = msg_send![&*app, addObserver: &*listener, forKeyPath: &*key, options: 0usize, context: std::ptr::null_mut::<c_void>()];
            center.addObserver_selector_name_object(&listener, sel!(themeChanged:), Some(&name), None);
        }
        HEARD.with(|h| h.set((0, 0)));
        assert_eq!(interface_style(), None, "no interface style while light");

        testing::set_desktop_dark(true);
        show(&w);
        assert_eq!(HEARD.with(Cell::get), (1, 1), "effectiveAppearance changed, and the notification came");
        assert_eq!(interface_style().as_deref(), Some("Dark"));
        assert_eq!(app.effectiveAppearance().name().to_string(), dark_aqua().name().to_string());
        assert!(title_is_light(&w), "views follow");
        assert!(bar_is_light(), "and so does the bar");

        testing::set_desktop_dark(false);
        show(&w);
        assert_eq!(HEARD.with(Cell::get), (2, 2));
        assert_eq!(interface_style(), None);
        assert!(!title_is_light(&w));
        assert!(!bar_is_light());

        // With an appearance of its own, the application keeps it, and still
        // hears of the desktop's.
        app.setAppearance(Some(&aqua()));
        testing::set_desktop_dark(true);
        show(&w);
        assert_eq!(HEARD.with(Cell::get), (2, 3), "its effectiveAppearance stays");
        assert_eq!(interface_style().as_deref(), Some("Dark"));
        assert!(!title_is_light(&w), "windows keep its Aqua");
        testing::set_desktop_dark(false);
        assert_eq!(HEARD.with(Cell::get), (2, 4));

        // Setting its own appearance is observable too, when that changes
        // its effective one.
        app.setAppearance(Some(&dark_aqua()));
        app.setAppearance(Some(&dark_aqua()));
        app.setAppearance(Some(&aqua()));
        assert_eq!(HEARD.with(Cell::get), (4, 4));

        // SAFETY: as added above.
        unsafe {
            let _: () = msg_send![&*app, removeObserver: &*listener, forKeyPath: &*key];
            center.removeObserver(&listener);
        }
        testing::note_painted_text(false);
        w.close();
        testing::settle();
        main_menu(mtm, false);
    }

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::use_null_backend();
        testing::capture_pixels(true);
        NSApplication::sharedApplication(mtm).setAppearance(Some(&aqua()));
        let tests: &[Test] = &[
            ("label_colors_follow_appearance_changes", label_colors_follow_appearance_changes),
            ("label_colors_follow_appearance_changes_in_layers", label_colors_follow_appearance_changes_in_layers),
            (
                "label_colors_follow_appearance_changes_in_their_own_layers",
                label_colors_follow_appearance_changes_in_their_own_layers,
            ),
            ("menu_bars_follow_their_windows", menu_bars_follow_their_windows),
            ("desktop_changes_reach_programs", desktop_changes_reach_programs),
        ];
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test(mtm));
            println!("test {name} ... ok");
        }
    }
}
