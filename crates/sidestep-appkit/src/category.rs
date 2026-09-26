//! What menus add to the responder classes, by category: `menu` and
//! `setMenu:` (abstract on `NSResponder`, as on macOS; kept by views and
//! windows, and the main menu on the application), views' context menus
//! (`menuForEvent:`, `+defaultMenu`, the default `rightMouseDown:` popping
//! one up, and the `willOpenMenu:withEvent:` and `didCloseMenu:withEvent:`
//! hooks), and the validators menus ask the application and windows
//! (`validateMenuItem:`, `validateUserInterfaceItem:`). Views have no
//! validators, as on macOS.
//!
//! The validators answer as AppKit's do (`conformance/tests/menus.rs`): a
//! window can close from the menu only when titled and closable, zoom only
//! when titled and resizable, and miniaturize when miniaturizable; it goes
//! full screen when its collection behaviour says it is a primary
//! full-screen window (the item then reads Enter Full Screen, or Exit Full
//! Screen once it is); it has no toolbar or tabs to show. The application
//! arranges and miniaturizes windows only when one is on screen, never
//! unhides other applications (Wayland shows none), and hides only as a
//! regular application not hidden yet. Everything else is enabled.
//!
//! A responder's menu is an associated object, so no class grows an ivar
//! for it.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{ClassType, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSMenu, NSMenuItem, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::NSString;

/// The key of a responder's menu.
static MENU_KEY: u8 = 0;

fn object<T>(this: &T) -> &AnyObject {
    // SAFETY: every receiver is an object.
    unsafe { &*(this as *const T).cast::<AnyObject>() }
}

fn stored_menu(object: &AnyObject) -> Option<Retained<NSMenu>> {
    // SAFETY: the key is this module's static; the value, if any, is the
    // menu `store_menu` kept, which the association keeps alive.
    unsafe {
        let value = objc2::ffi::objc_getAssociatedObject(object, (&raw const MENU_KEY).cast::<c_void>());
        Retained::retain(value.cast::<NSMenu>().cast_mut())
    }
}

fn store_menu(object: &AnyObject, menu: Option<&NSMenu>) {
    let value = menu.map_or(std::ptr::null_mut(), |m| (m as *const NSMenu).cast::<AnyObject>().cast_mut());
    // SAFETY: the key is this module's static, and the association retains
    // the menu (or removes the old one, for nil).
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            (object as *const AnyObject).cast_mut(),
            (&raw const MENU_KEY).cast::<c_void>(),
            value,
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
    }
}

fn abstract_method(this: &AnyObject, method: &str) -> ! {
    panic!(
        "Abstract method -[NSResponder {method}] called from class {}.  Subclasses must override.",
        this.class().name().to_string_lossy()
    )
}

define_class!(
    // NSResponder's menu methods, abstract as on macOS (its message names
    // `menu:`). `self` is a responder there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepResponderMenus"]
    struct ResponderMenus;

    impl ResponderMenus {
        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            abstract_method(object(self), "menu:")
        }

        #[unsafe(method(setMenu:))]
        fn set_menu(&self, _menu: Option<&NSMenu>) {
            abstract_method(object(self), "setMenu:")
        }
    }
);

define_class!(
    // A menu kept: NSWindow's `menu`. `self` is a window there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepKeptMenu"]
    struct KeptMenu;

    impl KeptMenu {
        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            stored_menu(object(self))
        }

        #[unsafe(method(setMenu:))]
        fn set_menu(&self, menu: Option<&NSMenu>) {
            store_menu(object(self), menu);
        }
    }
);

define_class!(
    // NSApplication's `menu`, which is its main menu. `self` is the
    // application there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepApplicationMenu"]
    struct ApplicationMenu;

    impl ApplicationMenu {
        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            // SAFETY: mainMenu takes nothing and returns a menu or nil, by
            // message as subclasses may override it.
            unsafe { msg_send![object(self), mainMenu] }
        }

        #[unsafe(method(setMenu:))]
        fn set_menu(&self, menu: Option<&NSMenu>) {
            // SAFETY: setMainMenu: takes a menu or nil.
            unsafe { msg_send![object(self), setMainMenu: menu] }
        }
    }
);

/// The action an item sends, as a validator is asked about it.
fn action_of(item: &AnyObject) -> Option<Sel> {
    // SAFETY: an item a validator is asked about answers action with a
    // selector or NULL (menu items and controls do).
    unsafe { msg_send![item, action] }
}

/// A window's answer for `action` (`item` is a menu item, else anything a
/// validator may be asked about).
fn window_validates(window: &NSWindow, action: Option<Sel>, item: Option<&NSMenuItem>) -> bool {
    let Some(action) = action else { return true };
    let style = window.styleMask();
    let titled = style.contains(NSWindowStyleMask::Titled);
    let name = action.name();
    match name.to_bytes() {
        b"performClose:" => titled && style.contains(NSWindowStyleMask::Closable),
        b"performMiniaturize:" | b"miniaturize:" => style.contains(NSWindowStyleMask::Miniaturizable),
        b"performZoom:" | b"zoom:" => titled && style.contains(NSWindowStyleMask::Resizable),
        b"toggleFullScreen:" => {
            let full = style.contains(NSWindowStyleMask::FullScreen);
            if let Some(item) = item {
                let title = if full { "Exit Full Screen" } else { "Enter Full Screen" };
                item.setTitle(&NSString::from_str(title));
            }
            full || window.collectionBehavior().contains(NSWindowCollectionBehavior::FullScreenPrimary)
        }
        b"toggleToolbarShown:"
        | b"runToolbarCustomizationPalette:"
        | b"selectNextTab:"
        | b"selectPreviousTab:"
        | b"toggleTabBar:"
        | b"toggleTabOverview:"
        | b"mergeAllWindows:"
        | b"moveTabToNewWindow:" => false,
        _ => true,
    }
}

/// The application's answer for `action`.
fn application_validates(app: &NSApplication, action: Option<Sel>) -> bool {
    let Some(action) = action else { return true };
    match action.name().to_bytes() {
        b"arrangeInFront:" | b"miniaturizeAll:" => app.windows().iter().any(|w| w.isVisible()),
        b"unhideAllApplications:" | b"toggleTouchBarCustomizationPalette:" => false,
        b"hide:" => app.activationPolicy() == NSApplicationActivationPolicy::Regular && !app.isHidden(),
        _ => true,
    }
}

define_class!(
    // A window's validators (see the module's description). `self` is a
    // window there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepWindowValidators"]
    struct WindowValidators;

    impl WindowValidators {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            // SAFETY: the category adds this to NSWindow.
            let window = unsafe { &*(self as *const Self).cast::<NSWindow>() };
            window_validates(window, item.action(), Some(item))
        }

        #[unsafe(method(validateUserInterfaceItem:))]
        fn validate_user_interface_item(&self, item: &AnyObject) -> bool {
            // SAFETY: the category adds this to NSWindow.
            let window = unsafe { &*(self as *const Self).cast::<NSWindow>() };
            let menu_item = crate::controls::kind_of(item, NSMenuItem::class()).then(|| {
                // SAFETY: checked to be a menu item.
                unsafe { &*(item as *const AnyObject).cast::<NSMenuItem>() }
            });
            window_validates(window, action_of(item), menu_item)
        }
    }
);

define_class!(
    // The application's validators (see the module's description). `self`
    // is the application there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepApplicationValidators"]
    struct ApplicationValidators;

    impl ApplicationValidators {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            // SAFETY: the category adds this to NSApplication.
            let app = unsafe { &*(self as *const Self).cast::<NSApplication>() };
            application_validates(app, item.action())
        }

        #[unsafe(method(validateUserInterfaceItem:))]
        fn validate_user_interface_item(&self, item: &AnyObject) -> bool {
            // SAFETY: the category adds this to NSApplication.
            let app = unsafe { &*(self as *const Self).cast::<NSApplication>() };
            application_validates(app, action_of(item))
        }
    }
);

define_class!(
    // NSView's context menus. `self` is a view there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepViewMenus"]
    struct ViewMenus;

    impl ViewMenus {
        /// The menu set, else the class's `+defaultMenu`.
        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            let this = object(self);
            // SAFETY: +defaultMenu takes nothing and returns a menu or nil.
            stored_menu(this).or_else(|| unsafe { msg_send![this.class(), defaultMenu] })
        }

        #[unsafe(method(setMenu:))]
        fn set_menu(&self, menu: Option<&NSMenu>) {
            store_menu(object(self), menu);
        }

        #[unsafe(method_id(defaultMenu))]
        fn default_menu() -> Option<Retained<NSMenu>> {
            None
        }

        /// The view's `menu`, whatever the event.
        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, _event: &NSEvent) -> Option<Retained<NSMenu>> {
            // SAFETY: menu takes nothing and returns a menu or nil, by
            // message as subclasses override it.
            unsafe { msg_send![object(self), menu] }
        }

        /// AppKit passes the current event, which may be nil.
        #[unsafe(method(willOpenMenu:withEvent:))]
        fn will_open_menu(&self, _menu: &NSMenu, _event: Option<&NSEvent>) {}

        #[unsafe(method(didCloseMenu:withEvent:))]
        fn did_close_menu(&self, _menu: &NSMenu, _event: Option<&NSEvent>) {}

        /// Pops up the view's menu for the event, if it has one; else the
        /// event goes up the responder chain.
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            // SAFETY: the category adds this to NSView, so `self` is a view.
            let view = unsafe { &*(self as *const Self).cast::<NSView>() };
            match view.menuForEvent(event) {
                Some(menu) => NSMenu::popUpContextMenu_withEvent_forView(&menu, event, view),
                None => {
                    // SAFETY: nextResponder returns a responder or nil.
                    if let Some(next) = unsafe { view.nextResponder() } {
                        next.rightMouseDown(event);
                    }
                }
            }
        }
    }
);

// A plain responder's `menu` is abstract.
sidestep_runtime::category!("NSResponder"(SidestepMenus), |category| {
    // SAFETY: the helper's methods treat their receiver as any object.
    unsafe { category.add_methods_of(ResponderMenus::class()) };
});

sidestep_runtime::category!("NSView"(SidestepMenus), |category| {
    // SAFETY: the helper's methods treat their receiver as a view.
    unsafe { category.add_methods_of(ViewMenus::class()) };
});

sidestep_runtime::category!("NSWindow"(SidestepMenus), |category| {
    // SAFETY: the helpers' methods treat their receiver as any object, and
    // the validators as a window.
    unsafe {
        category.add_methods_of(KeptMenu::class());
        category.add_methods_of(WindowValidators::class());
    }
});

sidestep_runtime::category!("NSApplication"(SidestepMenus), |category| {
    // SAFETY: the helpers' methods treat their receiver as any object, and
    // the validators as the application.
    unsafe {
        category.add_methods_of(ApplicationMenu::class());
        category.add_methods_of(ApplicationValidators::class());
    }
});
