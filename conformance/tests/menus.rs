//! Menus without showing them: `NSMenu` and `NSMenuItem` defaults, links
//! and ownership, their notifications, validation by `-[NSMenu update]`,
//! key equivalents through `-[NSMenu performKeyEquivalent:]` and actions
//! through the application. Key events are made with `NSEvent`'s
//! constructor, so only cases a real keyboard and a synthesized event
//! agree on are checked.
//!
//! AppKit belongs to the main thread, so this file has its own `main`. The
//! application runs with the accessory activation policy and is never
//! activated.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{NSCopying, NSNotification, NSNotificationCenter, NSNumber, NSPoint, NSString};

use sidestep as _;

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

fn app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app
}

fn title_of(sender: Option<&AnyObject>) -> String {
    match sender {
        Some(s) if is_kind(s, NSMenuItem::class()) => {
            // SAFETY: checked to be a menu item.
            let item = unsafe { &*(s as *const AnyObject).cast::<NSMenuItem>() };
            item.title().to_string()
        }
        Some(_) => "?".into(),
        None => "nil".into(),
    }
}

fn is_kind(object: &AnyObject, class: &objc2::runtime::AnyClass) -> bool {
    // SAFETY: isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![object, isKindOfClass: class] }
}

fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

// Targets: each logs the actions it gets, as "<name> <action> <sender>".

pub struct TargetIvars {
    name: &'static str,
    /// What `validateMenuItem:` or `validateUserInterfaceItem:` answer.
    answer: Cell<bool>,
}

define_class!(
    /// Has two actions and no validator.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMenuTarget"]
    #[ivars = TargetIvars]
    struct Target;

    impl Target {
        #[unsafe(method(act:))]
        fn act(&self, sender: Option<&AnyObject>) {
            log(format!("{} act: {}", self.ivars().name, title_of(sender)));
        }

        #[unsafe(method(other:))]
        fn other(&self, sender: Option<&AnyObject>) {
            log(format!("{} other: {}", self.ivars().name, title_of(sender)));
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

define_class!(
    /// Has `act:` and `validateMenuItem:`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMenuValidator"]
    #[ivars = TargetIvars]
    struct MenuValidator;

    impl MenuValidator {
        #[unsafe(method(act:))]
        fn act(&self, sender: Option<&AnyObject>) {
            log(format!("{} act: {}", self.ivars().name, title_of(sender)));
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            log(format!("{} validateMenuItem: {}", self.ivars().name, item.title()));
            self.ivars().answer.get()
        }
    }

    unsafe impl NSObjectProtocol for MenuValidator {}
);

define_class!(
    /// Has `act:` and `validateUserInterfaceItem:`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMenuUiValidator"]
    #[ivars = TargetIvars]
    struct UiValidator;

    impl UiValidator {
        #[unsafe(method(act:))]
        fn act(&self, sender: Option<&AnyObject>) {
            log(format!("{} act: {}", self.ivars().name, title_of(sender)));
        }

        #[unsafe(method(validateUserInterfaceItem:))]
        fn validate_user_interface_item(&self, item: &AnyObject) -> bool {
            log(format!("{} validateUserInterfaceItem: {}", self.ivars().name, title_of(Some(item))));
            self.ivars().answer.get()
        }
    }

    unsafe impl NSObjectProtocol for UiValidator {}
);

define_class!(
    /// Has `act:` and both validators.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMenuBothValidator"]
    #[ivars = TargetIvars]
    struct BothValidator;

    impl BothValidator {
        #[unsafe(method(act:))]
        fn act(&self, sender: Option<&AnyObject>) {
            log(format!("{} act: {}", self.ivars().name, title_of(sender)));
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            log(format!("{} validateMenuItem: {}", self.ivars().name, item.title()));
            self.ivars().answer.get()
        }

        #[unsafe(method(validateUserInterfaceItem:))]
        fn validate_user_interface_item(&self, item: &AnyObject) -> bool {
            log(format!("{} validateUserInterfaceItem: {}", self.ivars().name, title_of(Some(item))));
            self.ivars().answer.get()
        }
    }

    unsafe impl NSObjectProtocol for BothValidator {}
);

fn ivars(name: &'static str, answer: bool) -> TargetIvars {
    TargetIvars { name, answer: Cell::new(answer) }
}

fn target(mtm: MainThreadMarker, name: &'static str) -> Retained<Target> {
    let this = Target::alloc(mtm).set_ivars(ivars(name, true));
    unsafe { msg_send![super(this), init] }
}

fn menu_validator(mtm: MainThreadMarker, name: &'static str, answer: bool) -> Retained<MenuValidator> {
    let this = MenuValidator::alloc(mtm).set_ivars(ivars(name, answer));
    unsafe { msg_send![super(this), init] }
}

fn ui_validator(mtm: MainThreadMarker, name: &'static str, answer: bool) -> Retained<UiValidator> {
    let this = UiValidator::alloc(mtm).set_ivars(ivars(name, answer));
    unsafe { msg_send![super(this), init] }
}

fn both_validator(mtm: MainThreadMarker, name: &'static str, answer: bool) -> Retained<BothValidator> {
    let this = BothValidator::alloc(mtm).set_ivars(ivars(name, answer));
    unsafe { msg_send![super(this), init] }
}

define_class!(
    /// A menu that logs `update` and `performKeyEquivalent:` under its title,
    /// then does what NSMenu does.
    #[unsafe(super(NSMenu, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceLoggingMenu"]
    struct LoggingMenu;

    impl LoggingMenu {
        #[unsafe(method(update))]
        fn update(&self) {
            log(format!("{} update", self.title()));
            unsafe { msg_send![super(self), update] }
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            log(format!("{} performKeyEquivalent:", self.title()));
            unsafe { msg_send![super(self), performKeyEquivalent: event] }
        }
    }
);

fn logging_menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
    let this = LoggingMenu::alloc(mtm).set_ivars(());
    let menu: Retained<LoggingMenu> = unsafe { msg_send![super(this), initWithTitle: &*s(title)] };
    Retained::into_super(menu)
}

pub struct DelegateIvars;

define_class!(
    /// A menu delegate that logs what it's told.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMenuDelegate"]
    struct Delegate;

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
    }

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSMenuDelegate for Delegate {}
);

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
    unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &s(title), action, &s(key)) }
}

/// A key-down event: `chars` typed, `ignoring` its characters ignoring
/// modifiers.
fn key(chars: &str, ignoring: &str, flags: NSEventModifierFlags, code: u16) -> Retained<NSEvent> {
    key_event(NSEventType::KeyDown, chars, ignoring, flags, code)
}

fn key_event(
    kind: NSEventType,
    chars: &str,
    ignoring: &str,
    flags: NSEventModifierFlags,
    code: u16,
) -> Retained<NSEvent> {
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        kind,
        NSPoint::new(0.0, 0.0),
        flags,
        0.0,
        0,
        None,
        &s(chars),
        &s(ignoring),
        false,
        code,
    )
    .expect("a key event")
}

const CMD: NSEventModifierFlags = NSEventModifierFlags::Command;
const NONE: NSEventModifierFlags = NSEventModifierFlags(0);

/// Observe `name` on the default center, logging it with the object's
/// title and the user info's item index.
fn observe(name: &NSString) -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
    let block = RcBlock::new(|note: NonNull<NSNotification>| {
        // SAFETY: the center passes a live notification.
        let note = unsafe { note.as_ref() };
        let object = note.object();
        let title = object
            .as_deref()
            .filter(|o| is_kind(o, NSMenu::class()))
            .map(|o| unsafe { &*(o as *const AnyObject).cast::<NSMenu>() }.title().to_string())
            .unwrap_or_else(|| "?".into());
        let index = note
            .userInfo()
            .and_then(|info| info.objectForKey(&*s("NSMenuItemIndex")))
            .and_then(|n| n.downcast::<NSNumber>().ok())
            .map(|n| format!(" {}", n.integerValue()))
            .unwrap_or_default();
        let item = note
            .userInfo()
            .and_then(|info| info.objectForKey(&*s("MenuItem")))
            .map(|i| format!(" item {}", title_of(Some(&i))))
            .unwrap_or_default();
        log(format!("{} {title}{index}{item}", note.name()));
    });
    unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
    }
}

fn stop_observing(tokens: &[Retained<ProtocolObject<dyn NSObjectProtocol>>]) {
    for t in tokens {
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(t.as_ref()) };
    }
}

// Defaults and links.

fn menu_defaults(mtm: MainThreadMarker) {
    let m = NSMenu::new(mtm);
    assert_eq!(m.title().to_string(), "");
    assert!(m.autoenablesItems());
    assert!(m.showsStateColumn());
    assert_eq!(m.minimumWidth(), 0.0);
    assert_eq!(m.numberOfItems(), 0);
    assert_eq!(m.itemArray().count(), 0);
    assert!(unsafe { m.supermenu() }.is_none());
    assert!(m.delegate().is_none());
    assert!(m.highlightedItem().is_none());
    let titled = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("File"));
    assert_eq!(titled.title().to_string(), "File");
    titled.setTitle(&s("Edit"));
    assert_eq!(titled.title().to_string(), "Edit");
    m.setAutoenablesItems(false);
    assert!(!m.autoenablesItems());
    m.setShowsStateColumn(false);
    assert!(!m.showsStateColumn());
    m.setMinimumWidth(120.0);
    assert_eq!(m.minimumWidth(), 120.0);
    // A font of its own until set.
    assert!(m.font().is_some());
    assert!(m.allowsContextMenuPlugIns() && m.automaticallyInsertsWritingToolsItems());
    assert_eq!(m.presentationStyle(), NSMenuPresentationStyle::Regular);
    assert_eq!(m.selectionMode(), NSMenuSelectionMode::Automatic);
    assert_eq!(m.userInterfaceLayoutDirection(), NSUserInterfaceLayoutDirection::LeftToRight);
    // Only the main menu has a bar.
    assert_eq!(m.menuBarHeight(), 0.0);
    assert!(NSMenu::menuBarVisible(mtm));
    // The menu font, until one is set.
    let menu_font = NSFont::menuFontOfSize(0.0);
    assert_eq!(m.font().unwrap().pointSize(), menu_font.pointSize());
    let big = NSFont::systemFontOfSize(20.0);
    unsafe { m.setFont(Some(&big)) };
    assert_eq!(m.font().unwrap().pointSize(), 20.0);
}

fn item_defaults(mtm: MainThreadMarker) {
    let i = NSMenuItem::new(mtm);
    assert_eq!(i.title().to_string(), "NSMenuItem");
    assert!(i.isEnabled());
    assert_eq!(i.state(), 0);
    assert_eq!(i.keyEquivalent().to_string(), "");
    // Command, even with no key.
    assert_eq!(i.keyEquivalentModifierMask(), CMD);
    assert_eq!(i.tag(), 0);
    assert!(i.action().is_none());
    assert!(i.target().is_none());
    assert!(!i.hasSubmenu());
    assert!(i.submenu().is_none());
    assert!(!i.isSeparatorItem());
    assert!(!i.isHidden());
    assert!(!i.isHiddenOrHasHiddenAncestor());
    assert!(!i.isAlternate());
    assert!(!i.isHighlighted());
    assert_eq!(i.indentationLevel(), 0);
    assert!(i.representedObject().is_none());
    assert!(unsafe { i.menu() }.is_none());
    assert!(unsafe { i.parentItem() }.is_none());
    assert!(!i.allowsKeyEquivalentWhenHidden());
    assert!(i.toolTip().is_none());
    assert!(i.image().is_none());
    assert!(i.view().is_none());
    assert!(i.attributedTitle().is_none());
    // A check mark and a dash for the on and mixed states.
    assert!(i.onStateImage().is_some() && i.offStateImage().is_none() && i.mixedStateImage().is_some());
    assert_eq!(i.userKeyEquivalent().to_string(), "");
    assert!(!i.isSectionHeader());
    assert!(i.subtitle().is_none());
    assert!(i.allowsAutomaticKeyEquivalentLocalization() && i.allowsAutomaticKeyEquivalentMirroring());
    assert!(NSMenuItem::usesUserKeyEquivalents(mtm));
    let h = NSMenuItem::sectionHeaderWithTitle(&s("Head"), mtm);
    assert_eq!(h.title().to_string(), "Head");
    assert!(h.isSectionHeader() && h.isEnabled() && !h.isSeparatorItem());
    assert!(h.action().is_none());

    let q = item(mtm, "Quit", Some(sel!(terminate:)), "q");
    assert_eq!(q.title().to_string(), "Quit");
    assert_eq!(q.action(), Some(sel!(terminate:)));
    assert_eq!(q.keyEquivalent().to_string(), "q");
    assert_eq!(q.keyEquivalentModifierMask(), CMD);

    // An uppercase key doesn't add Shift to the mask.
    q.setKeyEquivalent(&s("Q"));
    assert_eq!(q.keyEquivalent().to_string(), "Q");
    assert_eq!(q.keyEquivalentModifierMask(), CMD);
    q.setKeyEquivalentModifierMask(NSEventModifierFlags::Option | NSEventModifierFlags::Shift);
    assert_eq!(q.keyEquivalentModifierMask(), NSEventModifierFlags::Option | NSEventModifierFlags::Shift);

    i.setTag(7);
    i.setState(1);
    i.setIndentationLevel(3);
    i.setHidden(true);
    i.setAlternate(true);
    i.setToolTip(Some(&s("tip")));
    i.setAllowsKeyEquivalentWhenHidden(true);
    assert_eq!((i.tag(), i.state(), i.indentationLevel()), (7, 1, 3));
    assert!(i.isHidden() && i.isHiddenOrHasHiddenAncestor() && i.isAlternate());
    assert_eq!(i.toolTip().unwrap().to_string(), "tip");
    assert!(i.allowsKeyEquivalentWhenHidden());
    // Indentation stops at 15.
    i.setIndentationLevel(40);
    assert_eq!(i.indentationLevel(), 15);
}

fn separators(mtm: MainThreadMarker) {
    let sep = NSMenuItem::separatorItem(mtm);
    assert!(sep.isSeparatorItem());
    assert_eq!(sep.title().to_string(), "");
    assert!(!sep.isEnabled());
    assert!(sep.action().is_none());
    assert!(!NSMenuItem::new(mtm).isSeparatorItem());
}

fn adding_and_removing(mtm: MainThreadMarker) {
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("M"));
    let a = item(mtm, "A", Some(sel!(act:)), "a");
    let b = item(mtm, "B", None, "");
    let t = target(mtm, "t");
    m.addItem(&a);
    assert!(unsafe { a.menu() }.is_some_and(|x| std::ptr::eq(&*x, &*m)));
    m.insertItem_atIndex(&b, 0);
    assert_eq!(m.numberOfItems(), 2);
    assert_eq!(m.indexOfItem(&b), 0);
    assert_eq!(m.indexOfItem(&a), 1);
    let c = unsafe { m.addItemWithTitle_action_keyEquivalent(&s("C"), Some(sel!(other:)), &s("c")) };
    assert_eq!(c.title().to_string(), "C");
    assert_eq!(c.keyEquivalentModifierMask(), CMD);
    assert_eq!(m.indexOfItem(&c), 2);
    let d = unsafe { m.insertItemWithTitle_action_keyEquivalent_atIndex(&s("D"), None, &s(""), 1) };
    assert_eq!(m.indexOfItem(&d), 1);
    let titles: Vec<String> = m.itemArray().iter().map(|i| i.title().to_string()).collect();
    assert_eq!(titles, ["B", "D", "A", "C"]);

    b.setTag(42);
    unsafe { a.setTarget(Some(&t)) };
    let object = NSString::from_str("represented");
    unsafe { d.setRepresentedObject(Some(&object)) };
    assert_eq!(m.indexOfItemWithTitle(&s("A")), 2);
    assert_eq!(m.indexOfItemWithTitle(&s("nope")), -1);
    assert_eq!(m.indexOfItemWithTag(42), 0);
    assert_eq!(m.indexOfItemWithTag(99), -1);
    assert_eq!(unsafe { m.indexOfItemWithRepresentedObject(Some(&object)) }, 1);
    assert_eq!(unsafe { m.indexOfItemWithRepresentedObject(Some(&s("other"))) }, -1);
    assert_eq!(unsafe { m.indexOfItemWithTarget_andAction(Some(&t), Some(sel!(act:))) }, 2);
    assert_eq!(unsafe { m.indexOfItemWithTarget_andAction(Some(&t), Some(sel!(other:))) }, -1);
    // nil finds the first item without one.
    assert_eq!(m.indexOfItemWithSubmenu(None), 0);
    assert_eq!(unsafe { m.indexOfItemWithRepresentedObject(None) }, 0);
    // The target must be the item's; no action finds any of its items.
    assert_eq!(unsafe { m.indexOfItemWithTarget_andAction(None, Some(sel!(act:))) }, -1);
    assert_eq!(unsafe { m.indexOfItemWithTarget_andAction(Some(&t), None) }, 2);
    assert!(m.itemWithTitle(&s("C")).is_some_and(|x| std::ptr::eq(&*x, &*c)));
    assert!(m.itemWithTitle(&s("nope")).is_none());
    assert!(m.itemWithTag(42).is_some_and(|x| std::ptr::eq(&*x, &*b)));
    assert!(m.itemWithTag(99).is_none());
    assert!(m.itemAtIndex(3).is_some_and(|x| std::ptr::eq(&*x, &*c)));
    let stranger = NSMenuItem::new(mtm);
    assert_eq!(m.indexOfItem(&stranger), -1);

    m.removeItem(&a);
    assert!(unsafe { a.menu() }.is_none());
    assert_eq!(m.numberOfItems(), 3);
    m.removeItemAtIndex(0);
    assert!(unsafe { b.menu() }.is_none());
    let titles: Vec<String> = m.itemArray().iter().map(|i| i.title().to_string()).collect();
    assert_eq!(titles, ["D", "C"]);
    m.removeAllItems();
    assert_eq!(m.numberOfItems(), 0);
    assert!(unsafe { c.menu() }.is_none());

    // An item goes to one menu at a time.
    let other = NSMenu::new(mtm);
    m.addItem(&a);
    let reason = raises(|| other.addItem(&a));
    assert_eq!(reason, "Item to be inserted into menu already is in another menu");
    m.removeItem(&a);
    other.addItem(&a);
    assert!(unsafe { a.menu() }.is_some_and(|x| std::ptr::eq(&*x, &*other)));

    // setItemArray: replaces every item.
    let x = item(mtm, "X", None, "");
    let y = item(mtm, "Y", None, "");
    m.setItemArray(&objc2_foundation::NSArray::from_retained_slice(&[x.clone(), y.clone()]));
    let titles: Vec<String> = m.itemArray().iter().map(|i| i.title().to_string()).collect();
    assert_eq!(titles, ["X", "Y"]);
    assert!(unsafe { y.menu() }.is_some_and(|z| std::ptr::eq(&*z, &*m)));
}

fn submenus(mtm: MainThreadMarker) {
    let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Main"));
    let file = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("File"));
    let host = item(mtm, "File", None, "");
    host.setSubmenu(Some(&file));
    assert!(host.hasSubmenu());
    assert_eq!(host.action(), Some(sel!(submenuAction:)));
    // Only once the host is in a menu.
    assert!(unsafe { file.supermenu() }.is_none());
    main.addItem(&host);
    assert!(unsafe { file.supermenu() }.is_some_and(|x| std::ptr::eq(&*x, &*main)));
    assert_eq!(main.indexOfItemWithSubmenu(Some(&file)), 0);
    let open = item(mtm, "Open", Some(sel!(act:)), "o");
    file.addItem(&open);
    assert!(unsafe { open.parentItem() }.is_some_and(|p| std::ptr::eq(&*p, &*host)));
    assert!(unsafe { host.parentItem() }.is_none());
    host.setHidden(true);
    assert!(!open.isHidden());
    assert!(open.isHiddenOrHasHiddenAncestor());
    host.setHidden(false);
    main.removeItem(&host);
    assert!(unsafe { file.supermenu() }.is_none());
    assert!(host.submenu().is_some_and(|x| std::ptr::eq(&*x, &*file)));

    // setSubmenu:forItem: is the item's setSubmenu:.
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Edit"));
    let edit_host = item(mtm, "Edit", None, "");
    main.addItem(&edit_host);
    main.setSubmenu_forItem(Some(&edit), &edit_host);
    assert!(edit_host.submenu().is_some_and(|x| std::ptr::eq(&*x, &*edit)));
    assert!(unsafe { edit.supermenu() }.is_some_and(|x| std::ptr::eq(&*x, &*main)));
    assert_eq!(edit_host.action(), Some(sel!(submenuAction:)));
    // Taking the submenu away takes the action too.
    edit_host.setSubmenu(None);
    assert!(!edit_host.hasSubmenu());
    assert!(unsafe { edit.supermenu() }.is_none());
    assert_eq!(edit_host.action(), None);
    let own = item(mtm, "own", Some(sel!(act:)), "");
    own.setSubmenu(Some(&NSMenu::new(mtm)));
    // An action of its own stays, with a submenu and after.
    assert_eq!(own.action(), Some(sel!(act:)));
    own.setSubmenu(None);
    assert_eq!(own.action(), Some(sel!(act:)));
    let own2 = item(mtm, "own2", None, "");
    own2.setSubmenu(Some(&NSMenu::new(mtm)));
    unsafe { own2.setAction(Some(sel!(act:))) };
    own2.setSubmenu(None);
    assert_eq!(own2.action(), Some(sel!(act:)));
    let twice = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Twice"));
    let h1 = item(mtm, "h1", None, "");
    h1.setSubmenu(Some(&twice));
    let h2 = item(mtm, "h2", None, "");
    h2.setSubmenu(Some(&twice));
    // A menu may be two items' submenu.
    assert!(h1.submenu().is_some_and(|x| std::ptr::eq(&*x, &*twice)));
    assert!(h2.submenu().is_some_and(|x| std::ptr::eq(&*x, &*twice)));
}

fn copies(mtm: MainThreadMarker) {
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("M"));
    let a = item(mtm, "A", Some(sel!(act:)), "a");
    a.setTag(3);
    a.setKeyEquivalentModifierMask(CMD | NSEventModifierFlags::Shift);
    m.addItem(&a);
    let copy = a.copy();
    assert!(unsafe { copy.menu() }.is_none());
    assert_eq!(copy.title().to_string(), "A");
    assert_eq!(copy.tag(), 3);
    assert_eq!(copy.action(), Some(sel!(act:)));
    assert_eq!(copy.keyEquivalent().to_string(), "a");
    assert_eq!(copy.keyEquivalentModifierMask(), CMD | NSEventModifierFlags::Shift);
    let mc = m.copy();
    assert_eq!(mc.title().to_string(), "M");
    assert_eq!(mc.numberOfItems(), 1);
    let first = mc.itemAtIndex(0).expect("an item");
    assert!(!std::ptr::eq(&*first, &*a));
    assert!(unsafe { first.menu() }.is_some_and(|x| std::ptr::eq(&*x, &*mc)));
}

fn notifications(mtm: MainThreadMarker) {
    let names = unsafe {
        [
            NSMenuDidAddItemNotification,
            NSMenuDidRemoveItemNotification,
            NSMenuDidChangeItemNotification,
            NSMenuWillSendActionNotification,
            NSMenuDidSendActionNotification,
            NSMenuDidBeginTrackingNotification,
            NSMenuDidEndTrackingNotification,
        ]
    };
    let values: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    assert_eq!(
        values,
        [
            "NSMenuDidAddItemNotification",
            "NSMenuDidRemoveItemNotification",
            "NSMenuDidChangeItemNotification",
            "NSMenuWillSendActionNotification",
            "NSMenuDidSendActionNotification",
            "NSMenuDidBeginTrackingNotification",
            "NSMenuDidEndTrackingNotification",
        ]
    );
    let tokens: Vec<_> = names.iter().map(|n| observe(n)).collect();
    take_log();
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Notes"));
    let a = item(mtm, "A", None, "");
    let b = item(mtm, "B", None, "");
    m.addItem(&a);
    m.insertItem_atIndex(&b, 0);
    a.setTitle(&s("A2"));
    a.setEnabled(false);
    a.setState(1);
    a.setKeyEquivalent(&s("k"));
    m.removeItem(&b);
    // Each names the menu and the item's index.
    assert_eq!(
        take_log(),
        [
            "NSMenuDidAddItemNotification Notes 0",
            "NSMenuDidAddItemNotification Notes 0",
            "NSMenuDidChangeItemNotification Notes 1",
            "NSMenuDidChangeItemNotification Notes 1",
            "NSMenuDidChangeItemNotification Notes 1",
            "NSMenuDidChangeItemNotification Notes 1",
            "NSMenuDidRemoveItemNotification Notes 0",
        ]
    );
    stop_observing(&tokens);
}

// Validation.

fn validation(mtm: MainThreadMarker) {
    let app = app(mtm);
    let delegate_before = app.delegate();
    let chain = target(mtm, "chain");
    app.setDelegate(Some(unsafe { &*(Retained::as_ptr(&chain) as *const ProtocolObject<dyn NSApplicationDelegate>) }));

    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("V"));
    let no_action = item(mtm, "no action", None, "");
    let sep = NSMenuItem::separatorItem(mtm);
    let sub_host = item(mtm, "host", None, "");
    sub_host.setSubmenu(Some(&NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Sub"))));
    let unresolved = item(mtm, "unresolved", Some(sel!(nobodyHasThis:)), "");
    let via_chain = item(mtm, "via chain", Some(sel!(act:)), "");
    let yes = menu_validator(mtm, "yes", true);
    let no = menu_validator(mtm, "no", false);
    let ui_no = ui_validator(mtm, "ui-no", false);
    let both_no = both_validator(mtm, "both-no", false);
    let explicit_missing = item(mtm, "explicit missing", Some(sel!(other:)), "");
    unsafe { explicit_missing.setTarget(Some(&no)) };
    let validated_yes = item(mtm, "validated yes", Some(sel!(act:)), "");
    unsafe { validated_yes.setTarget(Some(&yes)) };
    let validated_no = item(mtm, "validated no", Some(sel!(act:)), "");
    unsafe { validated_no.setTarget(Some(&no)) };
    let ui_validated = item(mtm, "ui validated", Some(sel!(act:)), "");
    unsafe { ui_validated.setTarget(Some(&ui_no)) };
    let both_validated = item(mtm, "both validated", Some(sel!(act:)), "");
    unsafe { both_validated.setTarget(Some(&both_no)) };
    let delegate = Delegate::alloc(mtm).set_ivars(());
    let delegate: Retained<Delegate> = unsafe { msg_send![super(delegate), init] };
    m.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    for i in [
        &no_action,
        &sep,
        &sub_host,
        &unresolved,
        &via_chain,
        &explicit_missing,
        &validated_yes,
        &validated_no,
        &ui_validated,
        &both_validated,
    ] {
        m.addItem(i);
        i.setEnabled(true);
    }
    take_log();
    m.update();
    let enabled = |i: &NSMenuItem| i.isEnabled();
    assert!(!enabled(&no_action));
    assert!(!enabled(&sep));
    assert!(enabled(&sub_host));
    assert!(!enabled(&unresolved));
    assert!(enabled(&via_chain));
    assert!(!enabled(&explicit_missing));
    assert!(enabled(&validated_yes));
    assert!(!enabled(&validated_no));
    assert!(!enabled(&ui_validated));
    assert!(!enabled(&both_validated));
    assert_eq!(
        take_log(),
        [
            "yes validateMenuItem: validated yes",
            "no validateMenuItem: validated no",
            "ui-no validateUserInterfaceItem: ui validated",
            "both-no validateMenuItem: both validated",
        ]
    );
    // A host keeps what it had.
    sub_host.setEnabled(false);
    m.update();
    assert!(!sub_host.isEnabled());
    take_log();

    // Without autoenabling, update leaves items alone.
    m.setAutoenablesItems(false);
    no_action.setEnabled(true);
    validated_yes.setEnabled(false);
    m.update();
    assert!(no_action.isEnabled());
    assert!(!validated_yes.isEnabled());
    assert_eq!(take_log(), Vec::<String>::new());

    // update doesn't descend into submenus.
    let top = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Top"));
    let inner = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Inner"));
    let inner_item = item(mtm, "inner", None, "");
    inner.addItem(&inner_item);
    let host = item(mtm, "Inner", None, "");
    host.setSubmenu(Some(&inner));
    top.addItem(&host);
    inner_item.setEnabled(true);
    top.update();
    assert!(inner_item.isEnabled());

    app.setDelegate(delegate_before.as_deref());
}

fn validators_exist(mtm: MainThreadMarker) {
    let app = app(mtm);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            objc2_foundation::NSRect::new(NSPoint::new(0.0, 0.0), objc2_foundation::NSSize::new(100.0, 100.0)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let view = NSView::new(mtm);
    let vmi = sel!(validateMenuItem:);
    let vui = sel!(validateUserInterfaceItem:);
    assert!(responds(&app, vmi) && responds(&app, vui));
    assert!(responds(&window, vmi) && responds(&window, vui));
    assert!(!responds(&view, vmi) && !responds(&view, vui));
    assert!(!responds(&NSMenu::new(mtm), vmi));
    // What they answer for an action they know nothing about.
    let i = item(mtm, "x", Some(sel!(act:)), "");
    let app_answer: bool = unsafe { msg_send![&app, validateMenuItem: &*i] };
    let window_answer: bool = unsafe { msg_send![&window, validateMenuItem: &*i] };
    assert!(app_answer && window_answer);
    window_validation(mtm, &window);
    application_validation(mtm, &app);
    let v = NSView::new(mtm);
    assert!(v.menu().is_none());
    let menu = NSMenu::new(mtm);
    unsafe { v.setMenu(Some(&menu)) };
    let right = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType::RightMouseDown,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        1,
        1.0,
    )
    .expect("a mouse event");
    assert!(v.menuForEvent(&right).is_some_and(|m| std::ptr::eq(&*m, &*menu)));
    // For any event type.
    let k = key("a", "a", NONE, 0);
    assert!(v.menuForEvent(&k).is_some_and(|m| std::ptr::eq(&*m, &*menu)));
    assert!(NSView::defaultMenu(mtm).is_none());
    assert!(responds(&v, sel!(willOpenMenu:withEvent:)) && responds(&v, sel!(didCloseMenu:withEvent:)));
    // Subviews don't share their superview's menu.
    let sub = NSView::new(mtm);
    v.addSubview(&sub);
    assert!(sub.menu().is_none() && sub.menuForEvent(&right).is_none());
    // Windows and the application keep one; they have no menuForEvent:.
    assert!(window.menu().is_none());
    unsafe { window.setMenu(Some(&menu)) };
    assert!(window.menu().is_some_and(|m| std::ptr::eq(&*m, &*menu)));
    unsafe { window.setMenu(None) };
    // The application's menu is its main menu.
    let main_before = app.mainMenu();
    app.setMainMenu(None);
    assert!(app.menu().is_none());
    let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Main"));
    app.setMainMenu(Some(&main));
    assert!(app.menu().is_some_and(|m| std::ptr::eq(&*m, &*main)));
    let other = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Other"));
    unsafe { app.setMenu(Some(&other)) };
    assert!(app.mainMenu().is_some_and(|m| std::ptr::eq(&*m, &*other)));
    unsafe { app.setMenu(None) };
    assert!(app.mainMenu().is_none());
    app.setMainMenu(main_before.as_deref());
    assert!(!responds(&window, sel!(menuForEvent:)) && !responds(&app, sel!(menuForEvent:)));
    // A plain responder's are abstract.
    let r = NSResponder::new(mtm);
    assert!(responds(&r, sel!(menu)) && responds(&r, sel!(setMenu:)));
    let reason = raises(|| {
        let _ = r.menu();
    });
    assert_eq!(
        reason,
        "Abstract method -[NSResponder menu:] called from class NSResponder.  Subclasses must override."
    );
    drop(window);
}

/// What a window's validators answer, by its style: an item with each
/// action, asked both ways.
fn window_validation(mtm: MainThreadMarker, titled: &NSWindow) {
    let window = |style: NSWindowStyleMask| {
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                objc2_foundation::NSRect::new(NSPoint::new(0.0, 0.0), objc2_foundation::NSSize::new(100.0, 100.0)),
                style,
                NSBackingStoreType::Buffered,
                true,
            )
        };
        unsafe { w.setReleasedWhenClosed(false) };
        w
    };
    let answer = |w: &NSWindow, action: Sel| {
        let i = item(mtm, "x", Some(action), "");
        let by_menu: bool = unsafe { msg_send![w, validateMenuItem: &*i] };
        let by_ui: bool = unsafe { msg_send![w, validateUserInterfaceItem: &*i] };
        assert_eq!(by_menu, by_ui, "{action:?}");
        by_menu
    };
    let t = NSWindowStyleMask::Titled;
    let all = t | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable | NSWindowStyleMask::Resizable;
    let full = window(all);
    // Closing, miniaturizing and zooming follow the style.
    let cases: &[(Sel, &NSWindow, bool)] = &[
        (sel!(performClose:), titled, false),
        (sel!(performClose:), &full, true),
        (sel!(performMiniaturize:), titled, false),
        (sel!(miniaturize:), titled, false),
        (sel!(performMiniaturize:), &full, true),
        (sel!(miniaturize:), &full, true),
        (sel!(performZoom:), titled, false),
        (sel!(zoom:), titled, false),
        (sel!(performZoom:), &full, true),
        (sel!(zoom:), &full, true),
        // No toolbar, no tabs.
        (sel!(toggleToolbarShown:), &full, false),
        (sel!(runToolbarCustomizationPalette:), &full, false),
        (sel!(selectNextTab:), &full, false),
        (sel!(selectPreviousTab:), &full, false),
        (sel!(toggleTabBar:), titled, false),
        (sel!(mergeAllWindows:), &full, false),
        // Anything else.
        (sel!(close), titled, true),
        (sel!(orderFront:), titled, true),
        (sel!(act:), titled, true),
    ];
    for &(action, w, want) in cases {
        assert_eq!(answer(w, action), want, "{action:?} for {:?}", w.styleMask());
    }
    // Each flag on its own: close wants a title too, zoom as well,
    // miniaturizing doesn't.
    let closable = window(t | NSWindowStyleMask::Closable);
    assert!(answer(&closable, sel!(performClose:)) && !answer(&closable, sel!(performZoom:)));
    assert!(!answer(&window(NSWindowStyleMask::Closable), sel!(performClose:)));
    assert!(answer(&window(t | NSWindowStyleMask::Resizable), sel!(zoom:)));
    assert!(!answer(&window(NSWindowStyleMask::Resizable), sel!(zoom:)));
    assert!(answer(&window(NSWindowStyleMask::Miniaturizable), sel!(miniaturize:)));
    // Full screen: for a primary full-screen window, and the item says
    // which way it goes.
    let i = item(mtm, "x", Some(sel!(toggleFullScreen:)), "");
    let enabled: bool = unsafe { msg_send![titled, validateMenuItem: &*i] };
    assert!(!enabled);
    assert_eq!(i.title().to_string(), "Enter Full Screen");
    let primary = window(all);
    primary.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    let i = item(mtm, "x", Some(sel!(toggleFullScreen:)), "");
    let enabled: bool = unsafe { msg_send![&*primary, validateMenuItem: &*i] };
    assert!(enabled);
    assert_eq!(i.title().to_string(), "Enter Full Screen");
    primary.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenNone);
    assert!(!answer(&primary, sel!(toggleFullScreen:)));
}

/// What the application's validators answer (it is never activated here,
/// and runs as an accessory).
fn application_validation(mtm: MainThreadMarker, app: &NSApplication) {
    let answer = |action: Sel| {
        let i = item(mtm, "x", Some(action), "");
        let by_menu: bool = unsafe { msg_send![app, validateMenuItem: &*i] };
        let by_ui: bool = unsafe { msg_send![app, validateUserInterfaceItem: &*i] };
        assert_eq!(by_menu, by_ui, "{action:?}");
        by_menu
    };
    // Showing all applications again depends on whether another one is
    // hidden: macOS enables it then. Sidestep sees no other applications.
    let unhide_all = answer(sel!(unhideAllApplications:));
    if !cfg!(target_vendor = "apple") {
        assert!(!unhide_all);
    }
    assert!(!answer(sel!(hide:)));
    for action in [sel!(terminate:), sel!(unhide:), sel!(orderFrontStandardAboutPanel:)] {
        assert!(answer(action), "{action:?}");
    }
    // Hiding the others depends on which other applications are running
    // and visible: yes on a desktop, no on CI's headless macOS runner.
    // Sidestep sees no other applications and always answers yes.
    let hide_others = answer(sel!(hideOtherApplications:));
    if !cfg!(target_vendor = "apple") {
        assert!(hide_others);
    }
    // Arranging and miniaturizing all windows wants one on screen.
    if !app.windows().iter().any(|w| w.isVisible()) {
        assert!(!answer(sel!(arrangeInFront:)));
        assert!(!answer(sel!(miniaturizeAll:)));
    }
}

// Key equivalents.

/// A menu of items with key equivalents, each sending `act:` to `t`.
fn key_menu(mtm: MainThreadMarker, t: &Target) -> Retained<NSMenu> {
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Keys"));
    let add = |title: &str, key: &str, mask: NSEventModifierFlags| {
        let i = item(mtm, title, Some(sel!(act:)), key);
        i.setKeyEquivalentModifierMask(mask);
        unsafe { i.setTarget(Some(t)) };
        m.addItem(&i);
    };
    add("quit", "q", CMD);
    add("upper", "W", CMD);
    add("option", "o", CMD | NSEventModifierFlags::Option);
    add("control", "k", NSEventModifierFlags::Control);
    add("return", "\r", NONE);
    add("escape", "\u{1b}", NONE);
    add("f5", "\u{F708}", NONE);
    add("delete", "\u{7f}", CMD);
    add("empty", "", CMD);
    m
}

fn key_equivalents(mtm: MainThreadMarker) {
    let _app = app(mtm);
    let t = target(mtm, "t");
    let m = key_menu(mtm, &t);
    let shift = NSEventModifierFlags::Shift;
    let option = NSEventModifierFlags::Option;
    let control = NSEventModifierFlags::Control;
    let caps = NSEventModifierFlags::CapsLock;
    let function = NSEventModifierFlags::Function;
    let numpad = NSEventModifierFlags::NumericPad;
    let cases: &[(&str, &str, NSEventModifierFlags, u16, Option<&str>)] = &[
        ("q", "q", CMD, 12, Some("quit")),
        // Caps Lock, Function and Numeric Pad don't count.
        ("q", "q", CMD | caps, 12, Some("quit")),
        ("q", "q", CMD | function | numpad, 12, Some("quit")),
        // Command, Option and Control must be exactly the mask's.
        ("q", "q", CMD | option, 12, None),
        ("q", "q", CMD | control, 12, None),
        ("q", "q", NONE, 12, None),
        ("ø", "o", CMD | option, 31, Some("option")),
        ("o", "o", CMD, 31, None),
        ("\u{b}", "k", control, 40, Some("control")),
        ("k", "k", CMD, 40, None),
        // An uppercase key equivalent is a shifted one.
        ("W", "W", CMD | shift, 13, Some("upper")),
        ("\r", "\r", NONE, 36, Some("return")),
        ("\u{1b}", "\u{1b}", NONE, 53, Some("escape")),
        ("\u{F708}", "\u{F708}", function, 96, Some("f5")),
        ("\u{7f}", "\u{7f}", CMD, 51, Some("delete")),
        ("x", "x", CMD, 7, None),
    ];
    for &(chars, ignoring, flags, code, want) in cases {
        let got = m.performKeyEquivalent(&key(chars, ignoring, flags, code));
        let log = take_log();
        match want {
            Some(title) => {
                assert!(got, "{chars:?} {flags:?} should match {title}");
                assert_eq!(log, [format!("t act: {title}")], "{chars:?} {flags:?}");
            }
            None => {
                assert!(!got, "{chars:?} {flags:?} matched {log:?}");
                assert!(log.is_empty(), "{log:?}");
            }
        }
    }
    // Key-ups never match.
    let up = key_event(NSEventType::KeyUp, "q", "q", CMD, 12);
    assert!(!m.performKeyEquivalent(&up));
    assert!(take_log().is_empty());
}

fn traversal_order(mtm: MainThreadMarker) {
    let _app = app(mtm);
    let t = target(mtm, "t");
    let main = logging_menu(mtm, "Main");
    for (title, key) in [("File", "o"), ("Edit", "c")] {
        let sub = logging_menu(mtm, title);
        let i = item(mtm, &format!("{title} item"), Some(sel!(act:)), key);
        unsafe { i.setTarget(Some(&t)) };
        sub.addItem(&i);
        let host = item(mtm, title, None, "");
        host.setSubmenu(Some(&sub));
        main.addItem(&host);
    }
    take_log();
    assert!(main.performKeyEquivalent(&key("c", "c", CMD, 8)));
    // Each menu brings itself up to date, then asks its submenus in turn,
    // depth first; the first match wins.
    assert_eq!(
        take_log(),
        [
            "Main performKeyEquivalent:",
            "Main update",
            "File performKeyEquivalent:",
            "File update",
            "Edit performKeyEquivalent:",
            "Edit update",
            "t act: Edit item"
        ]
    );
    assert!(!main.performKeyEquivalent(&key("z", "z", CMD, 6)));
    assert_eq!(
        take_log(),
        [
            "Main performKeyEquivalent:",
            "Main update",
            "File performKeyEquivalent:",
            "File update",
            "Edit performKeyEquivalent:",
            "Edit update"
        ]
    );
    // A key-up asks nobody.
    assert!(!main.performKeyEquivalent(&key_event(NSEventType::KeyUp, "c", "c", CMD, 8)));
    assert_eq!(take_log(), ["Main performKeyEquivalent:"]);
}

/// The actions logged since the last call, without validation.
fn actions() -> Vec<String> {
    take_log().into_iter().filter(|l| !l.contains(" validate")).collect()
}

fn disabled_and_hidden(mtm: MainThreadMarker) {
    let _app = app(mtm);
    let t = target(mtm, "t");
    let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Main"));
    let sub = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Sub"));
    let host = item(mtm, "Sub", None, "");
    host.setSubmenu(Some(&sub));
    main.addItem(&host);
    let inner = item(mtm, "inner", Some(sel!(act:)), "i");
    unsafe { inner.setTarget(Some(&t)) };
    sub.addItem(&inner);
    let hidden = item(mtm, "hidden", Some(sel!(act:)), "h");
    unsafe { hidden.setTarget(Some(&t)) };
    hidden.setHidden(true);
    main.addItem(&hidden);
    // Validation disables it: the key is still taken, nothing is sent.
    let v = menu_validator(mtm, "v", false);
    let disabled = item(mtm, "disabled", Some(sel!(act:)), "d");
    unsafe { disabled.setTarget(Some(&v)) };
    main.addItem(&disabled);
    actions();

    assert!(main.performKeyEquivalent(&key("d", "d", CMD, 2)));
    assert!(!disabled.isEnabled());
    assert_eq!(actions(), Vec::<String>::new());

    // Hidden items still match.
    assert!(main.performKeyEquivalent(&key("h", "h", CMD, 4)));
    assert_eq!(actions(), ["t act: hidden"]);
    host.setHidden(true);
    assert!(main.performKeyEquivalent(&key("i", "i", CMD, 34)));
    assert_eq!(actions(), ["t act: inner"]);
    host.setHidden(false);

    // A disabled host: the key is taken, nothing is sent.
    host.setEnabled(false);
    assert!(main.performKeyEquivalent(&key("i", "i", CMD, 34)));
    assert_eq!(actions(), Vec::<String>::new());
    host.setEnabled(true);
    assert!(main.performKeyEquivalent(&key("i", "i", CMD, 34)));
    assert_eq!(actions(), ["t act: inner"]);
}

fn actions_through_the_application(mtm: MainThreadMarker) {
    let app = app(mtm);
    let delegate_before = app.delegate();
    let chain = target(mtm, "delegate");
    app.setDelegate(Some(unsafe { &*(Retained::as_ptr(&chain) as *const ProtocolObject<dyn NSApplicationDelegate>) }));
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("M"));
    let a = item(mtm, "to delegate", Some(sel!(act:)), "a");
    m.addItem(&a);
    take_log();
    // No target: the application's delegate has it.
    assert!(m.performKeyEquivalent(&key("a", "a", CMD, 0)));
    assert_eq!(take_log(), ["delegate act: to delegate"]);
    // performActionForItemAtIndex: sends with the item as sender.
    m.performActionForItemAtIndex(0);
    assert_eq!(take_log(), ["delegate act: to delegate"]);
    // Not when the item is disabled.
    m.setAutoenablesItems(false);
    a.setEnabled(false);
    m.performActionForItemAtIndex(0);
    assert_eq!(take_log(), Vec::<String>::new());
    a.setEnabled(true);
    // An index past the items does nothing.
    NSMenu::new(mtm).performActionForItemAtIndex(3);
    m.performActionForItemAtIndex(1);
    assert_eq!(take_log(), Vec::<String>::new());
    let target_of_act = unsafe { app.targetForAction_to_from(sel!(act:), None, None) };
    assert!(target_of_act.is_some_and(|t| std::ptr::eq(&*t, (&*chain as &NSObject) as &AnyObject)));
    assert!(unsafe { app.targetForAction_to_from(sel!(nobodyHasThis:), None, None) }.is_none());
    assert!(!unsafe { app.sendAction_to_from(sel!(nobodyHasThis:), None, None) });
    app.setDelegate(delegate_before.as_deref());
}

fn action_notifications(mtm: MainThreadMarker) {
    let _app = app(mtm);
    let t = target(mtm, "t");
    let names = unsafe { [NSMenuWillSendActionNotification, NSMenuDidSendActionNotification] };
    let tokens: Vec<_> = names.iter().map(|n| observe(n)).collect();
    let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Main"));
    let sub = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Sub"));
    let host = item(mtm, "Sub", None, "");
    host.setSubmenu(Some(&sub));
    main.addItem(&host);
    let inner = item(mtm, "inner", Some(sel!(act:)), "i");
    unsafe { inner.setTarget(Some(&t)) };
    sub.addItem(&inner);
    take_log();
    // The item's menu posts around the action, naming the item.
    let around = [
        "NSMenuWillSendActionNotification Sub item inner",
        "t act: inner",
        "NSMenuDidSendActionNotification Sub item inner",
    ];
    assert!(main.performKeyEquivalent(&key("i", "i", CMD, 34)));
    assert_eq!(take_log(), around);
    sub.performActionForItemAtIndex(0);
    assert_eq!(take_log(), around);
    stop_observing(&tokens);

    // performActionForItemAtIndex: doesn't validate first.
    let v = menu_validator(mtm, "v", false);
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("P"));
    let x = item(mtm, "x", Some(sel!(act:)), "");
    unsafe { x.setTarget(Some(&v)) };
    m.addItem(&x);
    x.setEnabled(true);
    take_log();
    m.performActionForItemAtIndex(0);
    assert_eq!(take_log(), ["v act: x"]);
    assert!(x.isEnabled());
}

/// A change to make, and whether the menu posts it.
type Change<'a> = (&'a str, Box<dyn Fn() + 'a>, bool);

/// Which changes to an item its menu posts.
fn change_notifications(mtm: MainThreadMarker) {
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("C"));
    let x = item(mtm, "x", Some(sel!(act:)), "");
    m.addItem(&NSMenuItem::new(mtm));
    m.addItem(&x);
    let tokens = [observe(unsafe { NSMenuDidChangeItemNotification })];
    let posted = "NSMenuDidChangeItemNotification C 1";
    let image: Retained<NSImage> = unsafe { msg_send![NSImage::class(), new] };
    // A plain item second in a menu of its own, for the state images.
    let states = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("C"));
    let y = item(mtm, "y", Some(sel!(act:)), "");
    states.addItem(&NSMenuItem::new(mtm));
    states.addItem(&y);
    let cases: Vec<Change> = vec![
        ("the same title", Box::new(|| x.setTitle(&s("x"))), false),
        ("a new title", Box::new(|| x.setTitle(&s("y"))), true),
        ("hidden", Box::new(|| x.setHidden(true)), true),
        ("hidden again", Box::new(|| x.setHidden(true)), false),
        ("enabled as it was", Box::new(|| x.setEnabled(true)), false),
        ("disabled", Box::new(|| x.setEnabled(false)), true),
        ("the same state", Box::new(|| x.setState(0)), false),
        ("a new state", Box::new(|| x.setState(1)), true),
        ("a key", Box::new(|| x.setKeyEquivalent(&s("k"))), true),
        ("a mask", Box::new(|| x.setKeyEquivalentModifierMask(NSEventModifierFlags::Option)), true),
        ("a tooltip", Box::new(|| x.setToolTip(Some(&s("t")))), true),
        ("indentation", Box::new(|| x.setIndentationLevel(1)), true),
        ("alternate", Box::new(|| x.setAlternate(true)), true),
        ("a submenu", Box::new(|| x.setSubmenu(Some(&NSMenu::new(mtm)))), true),
        ("an action", Box::new(|| unsafe { x.setAction(Some(sel!(other:))) }), false),
        ("a target", Box::new(|| unsafe { x.setTarget(Some(&m)) }), false),
        ("a tag", Box::new(|| x.setTag(5)), false),
        ("a represented object", Box::new(|| unsafe { x.setRepresentedObject(Some(&s("r"))) }), false),
        ("the menu's title", Box::new(|| m.setTitle(&s("C"))), false),
        ("itemChanged:", Box::new(|| m.itemChanged(&x)), true),
        ("an image", Box::new(|| x.setImage(Some(&image))), true),
        // Of an item that is off, only the off image posts: the one its
        // state shows.
        ("an on image while off", Box::new(|| unsafe { y.setOnStateImage(Some(&image)) }), false),
        ("an off image while off", Box::new(|| y.setOffStateImage(Some(&image))), true),
        ("a mixed image while off", Box::new(|| unsafe { y.setMixedStateImage(Some(&image)) }), false),
    ];
    take_log();
    for (what, change, posts) in cases {
        change();
        let want: Vec<String> = if posts { vec![posted.into()] } else { vec![] };
        assert_eq!(take_log(), want, "{what}");
    }
    stop_observing(&tokens);
    // Taking every item out posts no removals; setting the items anew
    // posts the additions only.
    let names = unsafe { [NSMenuDidAddItemNotification, NSMenuDidRemoveItemNotification] };
    let tokens: Vec<_> = names.iter().map(|n| observe(n)).collect();
    m.removeAllItems();
    assert_eq!(take_log(), Vec::<String>::new());
    m.addItem(&item(mtm, "a", None, ""));
    take_log();
    m.setItemArray(&objc2_foundation::NSArray::from_retained_slice(&[
        item(mtm, "p", None, ""),
        item(mtm, "q", None, ""),
    ]));
    assert_eq!(take_log(), ["NSMenuDidAddItemNotification C 0", "NSMenuDidAddItemNotification C 1"]);
    stop_observing(&tokens);
}

// Tracking (opt-in: it shows windows and menus).

define_class!(
    /// A menu delegate that logs what it's told while its menu tracks.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTrackingDelegate"]
    struct TrackingDelegate;

    impl TrackingDelegate {
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
            let title = item.map_or_else(|| "nil".to_string(), |i| i.title().to_string());
            log(format!("menu:willHighlightItem: {title}"));
        }
    }

    unsafe impl NSObjectProtocol for TrackingDelegate {}

    unsafe impl NSMenuDelegate for TrackingDelegate {}
);

define_class!(
    /// A view that logs the menus it hears of. AppKit passes the current
    /// event, which may be nil, to both.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceMenuView"]
    struct MenuView;

    impl MenuView {
        #[unsafe(method(willOpenMenu:withEvent:))]
        fn will_open_menu(&self, menu: &NSMenu, _event: Option<&NSEvent>) {
            log(format!("willOpenMenu:withEvent: {}", menu.title()));
        }

        #[unsafe(method(didCloseMenu:withEvent:))]
        fn did_close_menu(&self, menu: &NSMenu, _event: Option<&NSEvent>) {
            log(format!("didCloseMenu:withEvent: {}", menu.title()));
        }
    }
);

/// Run `f` once after `seconds`, in any common mode (menus' loops too).
fn soon(seconds: f64, f: impl Fn() + 'static) -> Retained<objc2_foundation::NSTimer> {
    let block = RcBlock::new(move |_: NonNull<objc2_foundation::NSTimer>| f());
    let timer = unsafe { objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(seconds, false, &block) };
    unsafe {
        objc2_foundation::NSRunLoop::currentRunLoop().addTimer_forMode(&timer, objc2_foundation::NSRunLoopCommonModes)
    };
    timer
}

/// Post a key press to the front of the queue, as typed in window `number`.
fn post_key(chars: &str, flags: NSEventModifierFlags, code: u16, number: isize) {
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        flags,
        0.0,
        number,
        None,
        &s(chars),
        &s(chars),
        false,
        code,
    )
    .expect("a key event");
    let mtm = MainThreadMarker::new().expect("the main thread");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, true);
}

/// The log without the begin-tracking notification, whose place before or
/// after `menuNeedsUpdate:` AppKit doesn't keep the same; it comes before
/// `menuWillOpen:`.
fn without_begin(log: Vec<String>) -> Vec<String> {
    let begin = log.iter().position(|l| l.starts_with("NSMenuDidBeginTrackingNotification"));
    let will_open = log.iter().position(|l| l.starts_with("menuWillOpen:"));
    assert!(begin.is_some_and(|b| will_open.is_some_and(|w| b < w)), "{log:?}");
    log.into_iter().filter(|l| !l.starts_with("NSMenuDidBeginTrackingNotification")).collect()
}

/// What a menu tells its delegate, its view and observers as it opens,
/// tracks and closes: chosen with Return, cancelled, and with a submenu
/// opened from the keyboard.
fn tracking(mtm: MainThreadMarker) {
    let _app = app(mtm);
    let frame = objc2_foundation::NSRect::new(NSPoint::new(100.0, 100.0), objc2_foundation::NSSize::new(300.0, 200.0));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let view = MenuView::alloc(mtm).set_ivars(());
    let bounds = objc2_foundation::NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
    let view: Retained<MenuView> = unsafe { msg_send![super(view), initWithFrame: bounds] };
    window.setContentView(Some(&view));
    window.orderFront(None);
    let number = window.windowNumber();
    let v = menu_validator(mtm, "v", true);
    let m = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("M"));
    let d = TrackingDelegate::alloc(mtm).set_ivars(());
    let d: Retained<TrackingDelegate> = unsafe { msg_send![super(d), init] };
    m.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    let one = item(mtm, "one", Some(sel!(act:)), "");
    let two = item(mtm, "two", Some(sel!(act:)), "");
    for i in [&one, &two] {
        unsafe { i.setTarget(Some(&v)) };
        m.addItem(i);
    }
    let names = unsafe {
        [
            NSMenuDidBeginTrackingNotification,
            NSMenuDidEndTrackingNotification,
            NSMenuWillSendActionNotification,
            NSMenuDidSendActionNotification,
        ]
    };
    let tokens: Vec<_> = names.iter().map(|n| observe(n)).collect();
    let at = NSPoint::new(20.0, 150.0);
    take_log();

    // Return chooses the item the menu opened on.
    let _t = soon(0.3, move || post_key("\r", NONE, 36, number));
    assert!(m.popUpMenuPositioningItem_atLocation_inView(Some(&one), at, Some(&view)));
    assert_eq!(
        without_begin(take_log()),
        [
            "menuNeedsUpdate: M",
            "menuWillOpen: M",
            "willOpenMenu:withEvent: M",
            "v validateMenuItem: one",
            "v validateMenuItem: two",
            "menu:willHighlightItem: one",
            "menu:willHighlightItem: nil",
            "menuDidClose: M",
            "didCloseMenu:withEvent: M",
            "NSMenuDidEndTrackingNotification M",
            "NSMenuWillSendActionNotification M item one",
            "v act: one",
            "NSMenuDidSendActionNotification M item one",
        ]
    );

    // cancelTracking: nothing chosen, nothing highlighted to take away.
    let m2 = m.clone();
    let _t = soon(0.3, move || m2.cancelTracking());
    assert!(!m.popUpMenuPositioningItem_atLocation_inView(None, at, Some(&view)));
    assert_eq!(
        without_begin(take_log()),
        [
            "menuNeedsUpdate: M",
            "menuWillOpen: M",
            "willOpenMenu:withEvent: M",
            "v validateMenuItem: one",
            "v validateMenuItem: two",
            "menuDidClose: M",
            "didCloseMenu:withEvent: M",
            "NSMenuDidEndTrackingNotification M",
        ]
    );

    // Right opens a submenu on its first item; Return chooses it. Every
    // menu loses its highlight, then the menus close (AppKit doesn't keep
    // the submenu's place among the outer menu's calls the same, but it
    // closes before tracking ends).
    let sub = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("S"));
    sub.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    let s1 = item(mtm, "s1", Some(sel!(act:)), "");
    unsafe { s1.setTarget(Some(&v)) };
    sub.addItem(&s1);
    let host = item(mtm, "host", None, "");
    host.setSubmenu(Some(&sub));
    m.addItem(&host);
    let arrows = NSEventModifierFlags::Function | NSEventModifierFlags::NumericPad;
    let _t = soon(0.3, move || post_key("\u{F703}", arrows, 124, number));
    let _t2 = soon(0.8, move || post_key("\r", NONE, 36, number));
    assert!(m.popUpMenuPositioningItem_atLocation_inView(Some(&host), at, Some(&view)));
    let log = without_begin(take_log());
    let closed = log.iter().position(|l| l == "menuDidClose: S");
    let unlit = log.iter().rposition(|l| l == "menu:willHighlightItem: nil");
    let ended = log.iter().position(|l| l.starts_with("NSMenuDidEndTrackingNotification"));
    assert!(closed.is_some_and(|c| unlit.is_some_and(|u| u < c) && ended.is_some_and(|e| c < e)), "{log:?}");
    assert_eq!(
        log.into_iter().filter(|l| l != "menuDidClose: S").collect::<Vec<_>>(),
        [
            "menuNeedsUpdate: M",
            "menuWillOpen: M",
            "willOpenMenu:withEvent: M",
            "v validateMenuItem: one",
            "v validateMenuItem: two",
            "menu:willHighlightItem: host",
            "menuNeedsUpdate: S",
            "menuWillOpen: S",
            "v validateMenuItem: s1",
            "menu:willHighlightItem: s1",
            "menu:willHighlightItem: nil",
            "menu:willHighlightItem: nil",
            "menuDidClose: M",
            "didCloseMenu:withEvent: M",
            "NSMenuDidEndTrackingNotification M",
            "NSMenuWillSendActionNotification S item s1",
            "v act: s1",
            "NSMenuDidSendActionNotification S item s1",
        ]
    );
    stop_observing(&tokens);
    window.orderOut(None);
}

/// The reason of the exception `f` raises (Sidestep panics where Apple
/// raises).
fn raises(f: impl FnOnce()) -> String {
    use std::panic::AssertUnwindSafe;
    #[cfg(target_vendor = "apple")]
    {
        match objc2::exception::catch(AssertUnwindSafe(f)) {
            Ok(()) => panic!("expected an exception"),
            Err(Some(e)) => {
                let reason: Retained<NSString> = unsafe { msg_send![&*e, reason] };
                reason.to_string()
            }
            Err(None) => panic!("nil exception"),
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let e = std::panic::catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
        match e.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => e.downcast::<&str>().map(|s| s.to_string()).unwrap_or_default(),
        }
    }
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let windows = std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_some();
    #[cfg(not(target_vendor = "apple"))]
    if windows && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        // SAFETY: nothing else runs yet to read the environment.
        unsafe { std::env::set_var("SIDESTEP_BACKEND", "null") };
    }
    let mut tests: Vec<Test> = vec![
        ("menu_defaults", menu_defaults),
        ("item_defaults", item_defaults),
        ("separators", separators),
        ("adding_and_removing", adding_and_removing),
        ("submenus", submenus),
        ("copies", copies),
        ("notifications", notifications),
        ("validation", validation),
        ("validators_exist", validators_exist),
        ("key_equivalents", key_equivalents),
        ("traversal_order", traversal_order),
        ("disabled_and_hidden", disabled_and_hidden),
        ("actions_through_the_application", actions_through_the_application),
        ("action_notifications", action_notifications),
        ("change_notifications", change_notifications),
    ];
    if windows {
        tests.push(("tracking", tracking));
    } else {
        println!("menus: tracking skipped (SIDESTEP_CONFORMANCE_WINDOWS=1 runs it, which shows windows)");
    }
    let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
    for (name, test) in tests {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
