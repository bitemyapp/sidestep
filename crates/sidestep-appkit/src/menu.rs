//! `NSMenu` and `NSMenuItem`: the model menus are drawn from, validation,
//! key equivalents and actions.
//!
//! A menu retains its items, and an item its submenu and represented
//! object. The links back don't retain: an item's `menu`, a submenu's
//! `supermenu` (set only while its host item is in a menu), an item's
//! target and a menu's delegate (the last two weakly). A menu clears its
//! items' links to it when it lets them go or goes away, so none dangles.
//!
//! Each menu keeps a generation, counted up whenever it or one of its items
//! changes in a way that shows; a menu's layout (`menu_view`) is kept until
//! the generation moves, and a menu that shows is laid out again at once
//! (`menu_tracking::menu_changed`). The bar (`menubar`) is drawn again when
//! the main menu or one of its menus' titles changes. Changes post
//! `NSMenuDidAddItemNotification`, `…RemoveItem…` and `…ChangeItem…` with
//! the item's index, as macOS does (only for changes that show, and only
//! when the value differs; `removeAllItems` posts no removals, and a state
//! image posts only when it is the one the item's state shows); nothing is
//! built when nobody observes.
//!
//! `-[NSMenu update]` validates, when the menu enables its items itself
//! (`autoenablesItems`, the default): an item without an action, or whose
//! action finds no target through the application
//! (`targetForAction:to:from:`), is disabled; else the target's
//! `validateMenuItem:` or `validateUserInterfaceItem:` decides, and a
//! target with neither enables it. Hosts of submenus keep what they have,
//! and submenus are validated when they are asked themselves.
//!
//! `-[NSMenu performKeyEquivalent:]` brings the menu up to date (`update`,
//! by message, so subclasses see it), then offers the key to its items in
//! order, depth first: a host passes it on to its submenu by message. The
//! first item whose key equivalent matches (see `keyequiv`) takes the key
//! and sends its action if it and every host above it are enabled; a
//! disabled one takes the key and does nothing. Actions go through
//! `-[NSApplication sendAction:to:from:]` with the item as sender, between
//! `NSMenuWillSendActionNotification` and `…DidSendAction…`.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSFont, NSImage, NSMenu, NSMenuItem, NSMenuPresentationStyle,
    NSMenuProperties, NSMenuSelectionMode, NSUserInterfaceLayoutDirection, NSView,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSNumber, NSPoint, NSSize, NSString, NSZone};
use sidestep_foundation::notification_center;

use crate::keyequiv::{self, Pressed, Shortcut};

sidestep_runtime::static_class!(pub NSMENU, NSMENU_META = "NSMenu", || {
    let _ = NSMenuImpl::class();
});

sidestep_runtime::static_class!(pub NSMENUITEM, NSMENUITEM_META = "NSMenuItem", || {
    let _ = NSMenuItemImpl::class();
});

// The notifications menus post; each name is its value on macOS
// (conformance/tests/menus.rs checks them).
sidestep_foundation::constant_string!(NSMenuWillSendActionNotification = "NSMenuWillSendActionNotification");
sidestep_foundation::constant_string!(NSMenuDidSendActionNotification = "NSMenuDidSendActionNotification");
sidestep_foundation::constant_string!(NSMenuDidAddItemNotification = "NSMenuDidAddItemNotification");
sidestep_foundation::constant_string!(NSMenuDidRemoveItemNotification = "NSMenuDidRemoveItemNotification");
sidestep_foundation::constant_string!(NSMenuDidChangeItemNotification = "NSMenuDidChangeItemNotification");
sidestep_foundation::constant_string!(NSMenuDidBeginTrackingNotification = "NSMenuDidBeginTrackingNotification");
sidestep_foundation::constant_string!(NSMenuDidEndTrackingNotification = "NSMenuDidEndTrackingNotification");

/// A notification name this module exports.
macro_rules! note {
    ($name:ident) => {{
        // SAFETY: the name is a constant string exported above, alive for
        // the whole program.
        unsafe { objc2_app_kit::$name }
    }};
}
#[allow(unused_imports)]
pub(crate) use note;

/// A reference that doesn't retain, as AppKit's back links are.
type Unretained<T> = Cell<Option<NonNull<T>>>;

thread_local! {
    /// `+[NSMenu menuBarVisible]`.
    static BAR_VISIBLE: Cell<bool> = const { Cell::new(true) };
    /// `+[NSMenuItem usesUserKeyEquivalents]`.
    static USER_KEYS: Cell<bool> = const { Cell::new(true) };
    /// Menus being asked `menuNeedsUpdate:`, when `propertiesToUpdate` may
    /// be asked.
    static UPDATING: Cell<usize> = const { Cell::new(0) };
}

// Items.

pub(crate) struct ItemIvars {
    title: RefCell<Retained<NSString>>,
    attributed_title: RefCell<Option<Retained<AnyObject>>>,
    subtitle: RefCell<Option<Retained<NSString>>>,
    key: RefCell<Retained<NSString>>,
    mask: Cell<NSEventModifierFlags>,
    /// The key equivalent and mask, ready to match.
    shortcut: Cell<Shortcut>,
    action: Cell<Option<Sel>>,
    target: RefCell<Weak<AnyObject>>,
    tag: Cell<isize>,
    state: Cell<isize>,
    enabled: Cell<bool>,
    hidden: Cell<bool>,
    alternate: Cell<bool>,
    indentation: Cell<isize>,
    separator: Cell<bool>,
    section_header: Cell<bool>,
    represented: RefCell<Option<Retained<AnyObject>>>,
    submenu: RefCell<Option<Retained<NSMenu>>>,
    /// The menu the item is in.
    menu: Unretained<NSMenu>,
    image: RefCell<Option<Retained<NSImage>>>,
    on_image: RefCell<Option<Retained<NSImage>>>,
    off_image: RefCell<Option<Retained<NSImage>>>,
    mixed_image: RefCell<Option<Retained<NSImage>>>,
    view: RefCell<Option<Retained<NSView>>>,
    tooltip: RefCell<Option<Retained<NSString>>>,
    badge: RefCell<Option<Retained<AnyObject>>>,
    identifier: RefCell<Option<Retained<NSString>>>,
    when_hidden: Cell<bool>,
    localizes_key: Cell<bool>,
    mirrors_key: Cell<bool>,
    /// Shown highlighted by a menu being tracked.
    highlighted: Cell<bool>,
}

impl ItemIvars {
    fn new(title: Retained<NSString>, action: Option<Sel>, key: Retained<NSString>) -> ItemIvars {
        let mask = NSEventModifierFlags::Command;
        ItemIvars {
            shortcut: Cell::new(Shortcut::new(&key, mask)),
            title: RefCell::new(title),
            attributed_title: RefCell::new(None),
            subtitle: RefCell::new(None),
            key: RefCell::new(key),
            mask: Cell::new(mask),
            action: Cell::new(action),
            target: RefCell::new(Weak::default()),
            tag: Cell::new(0),
            state: Cell::new(0),
            enabled: Cell::new(true),
            hidden: Cell::new(false),
            alternate: Cell::new(false),
            indentation: Cell::new(0),
            separator: Cell::new(false),
            section_header: Cell::new(false),
            represented: RefCell::new(None),
            submenu: RefCell::new(None),
            menu: Cell::new(None),
            image: RefCell::new(None),
            on_image: RefCell::new(None),
            off_image: RefCell::new(None),
            mixed_image: RefCell::new(None),
            view: RefCell::new(None),
            tooltip: RefCell::new(None),
            badge: RefCell::new(None),
            identifier: RefCell::new(None),
            when_hidden: Cell::new(false),
            localizes_key: Cell::new(true),
            mirrors_key: Cell::new(true),
            highlighted: Cell::new(false),
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSMenuItem"]
    #[ivars = ItemIvars]
    pub(crate) struct NSMenuItemImpl;

    impl NSMenuItemImpl {
        #[unsafe(method_id(initWithTitle:action:keyEquivalent:))]
        fn init_with_title(
            this: Allocated<Self>,
            title: &NSString,
            action: Option<Sel>,
            key: &NSString,
        ) -> Retained<Self> {
            let this = this.set_ivars(ItemIvars::new(title.copy(), action, key.copy()));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer, with the default title.
            unsafe {
                msg_send![this, initWithTitle: &*NSString::from_str("NSMenuItem"), action: None::<Sel>, keyEquivalent: &*NSString::new()]
            }
        }

        #[unsafe(method_id(separatorItem))]
        fn separator_item() -> Retained<NSMenuItem> {
            let item = new_item("", None, "");
            let ivars = item_ivars(&item);
            ivars.separator.set(true);
            ivars.enabled.set(false);
            item
        }

        #[unsafe(method_id(sectionHeaderWithTitle:))]
        fn section_header_with_title(title: &NSString) -> Retained<NSMenuItem> {
            let item = new_item("", None, "");
            let ivars = item_ivars(&item);
            ivars.title.replace(title.copy());
            ivars.section_header.set(true);
            item
        }

        /// Linux has no writing tools.
        #[unsafe(method_id(writingToolsItems))]
        fn writing_tools_items() -> Retained<NSArray<NSMenuItem>> {
            NSArray::new()
        }

        #[unsafe(method(usesUserKeyEquivalents))]
        fn uses_user_key_equivalents() -> bool {
            USER_KEYS.with(Cell::get)
        }

        #[unsafe(method(setUsesUserKeyEquivalents:))]
        fn set_uses_user_key_equivalents(flag: bool) {
            USER_KEYS.with(|u| u.set(flag));
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMenuItem> {
            let mine = self.ivars();
            // SAFETY: an item's class makes items with alloc and the
            // designated initializer.
            let copy: Retained<NSMenuItem> = unsafe {
                let allocated: Allocated<NSMenuItem> = msg_send![self.class(), alloc];
                msg_send![allocated, initWithTitle: &**mine.title.borrow(), action: mine.action.get(), keyEquivalent: &**mine.key.borrow()]
            };
            let theirs = item_ivars(&copy);
            theirs.attributed_title.replace(mine.attributed_title.borrow().clone());
            theirs.subtitle.replace(mine.subtitle.borrow().clone());
            theirs.mask.set(mine.mask.get());
            theirs.shortcut.set(mine.shortcut.get());
            theirs.target.replace(mine.target.borrow().clone());
            theirs.tag.set(mine.tag.get());
            theirs.state.set(mine.state.get());
            theirs.enabled.set(mine.enabled.get());
            theirs.hidden.set(mine.hidden.get());
            theirs.alternate.set(mine.alternate.get());
            theirs.indentation.set(mine.indentation.get());
            theirs.separator.set(mine.separator.get());
            theirs.section_header.set(mine.section_header.get());
            theirs.represented.replace(mine.represented.borrow().clone());
            // A submenu is copied with its host, as a menu can't be in two
            // places at once.
            let submenu = mine.submenu.borrow().as_ref().map(|s| s.copy());
            theirs.submenu.replace(submenu);
            theirs.image.replace(mine.image.borrow().clone());
            theirs.on_image.replace(mine.on_image.borrow().clone());
            theirs.off_image.replace(mine.off_image.borrow().clone());
            theirs.mixed_image.replace(mine.mixed_image.borrow().clone());
            theirs.tooltip.replace(mine.tooltip.borrow().clone());
            theirs.badge.replace(mine.badge.borrow().clone());
            theirs.identifier.replace(mine.identifier.borrow().clone());
            theirs.when_hidden.set(mine.when_hidden.get());
            theirs.localizes_key.set(mine.localizes_key.get());
            theirs.mirrors_key.set(mine.mirrors_key.get());
            copy
        }

        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            // SAFETY: the menu holds the item while the link names it, and
            // clears the link when it lets go.
            self.ivars().menu.get().map(|m| unsafe { m.as_ref() }.retain())
        }

        /// The link back to the menu the item is in, which the menu sets.
        #[unsafe(method(setMenu:))]
        fn set_menu(&self, menu: Option<&NSMenu>) {
            self.ivars().menu.set(menu.map(NonNull::from));
        }

        #[unsafe(method(hasSubmenu))]
        fn has_submenu(&self) -> bool {
            self.ivars().submenu.borrow().is_some()
        }

        #[unsafe(method_id(submenu))]
        fn submenu(&self) -> Option<Retained<NSMenu>> {
            self.ivars().submenu.borrow().clone()
        }

        /// A new submenu gives an item without an action `submenuAction:`,
        /// which taking it away takes back. The submenu's supermenu is the
        /// item's menu, while it has one.
        #[unsafe(method(setSubmenu:))]
        fn set_submenu(&self, submenu: Option<&NSMenu>) {
            let ivars = self.ivars();
            let same = match (&*ivars.submenu.borrow(), submenu) {
                (Some(a), Some(b)) => std::ptr::eq(&**a, b),
                (None, None) => true,
                _ => false,
            };
            if same {
                return;
            }
            let old = ivars.submenu.replace(submenu.map(|s| s.retain()));
            let menu = ivars.menu.get();
            if let Some(old) = &old
                && menu.is_some()
                && menu_ivars(old).supermenu.get() == menu
            {
                menu_ivars(old).supermenu.set(None);
            }
            match submenu {
                Some(new) => {
                    if ivars.action.get().is_none() {
                        ivars.action.set(Some(sel!(submenuAction:)));
                    }
                    if menu.is_some() {
                        menu_ivars(new).supermenu.set(menu);
                    }
                }
                None if ivars.action.get() == Some(sel!(submenuAction:)) => ivars.action.set(None),
                None => {}
            }
            changed(self);
            drop(old);
        }

        /// The item whose submenu holds this one.
        #[unsafe(method_id(parentItem))]
        fn parent_item(&self) -> Option<Retained<NSMenuItem>> {
            parent_of(self.ivars())
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.ivars().title.borrow().clone()
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            if self.ivars().title.borrow().isEqualToString(title) {
                return;
            }
            let old = self.ivars().title.replace(title.copy());
            changed(self);
            drop(old);
        }

        #[unsafe(method_id(attributedTitle))]
        fn attributed_title(&self) -> Option<Retained<AnyObject>> {
            self.ivars().attributed_title.borrow().clone()
        }

        /// Sets the title to the attributed title's string, too.
        #[unsafe(method(setAttributedTitle:))]
        fn set_attributed_title(&self, title: Option<&AnyObject>) {
            let copied = title.map(|t| {
                // SAFETY: an attributed string answers copy with one.
                let copy: Retained<AnyObject> = unsafe { msg_send![t, copy] };
                copy
            });
            let old = self.ivars().attributed_title.replace(copied);
            if let Some(t) = title {
                // SAFETY: an attributed string's string is an NSString.
                let string: Retained<NSString> = unsafe { msg_send![t, string] };
                drop(self.ivars().title.replace(string.copy()));
            }
            changed(self);
            drop(old);
        }

        #[unsafe(method_id(subtitle))]
        fn subtitle(&self) -> Option<Retained<NSString>> {
            self.ivars().subtitle.borrow().clone()
        }

        #[unsafe(method(setSubtitle:))]
        fn set_subtitle(&self, subtitle: Option<&NSString>) {
            let old = self.ivars().subtitle.replace(subtitle.map(|s| s.copy()));
            changed(self);
            drop(old);
        }

        #[unsafe(method(isSeparatorItem))]
        fn is_separator_item(&self) -> bool {
            self.ivars().separator.get()
        }

        #[unsafe(method(isSectionHeader))]
        fn is_section_header(&self) -> bool {
            self.ivars().section_header.get()
        }

        #[unsafe(method_id(keyEquivalent))]
        fn key_equivalent(&self) -> Retained<NSString> {
            self.ivars().key.borrow().clone()
        }

        #[unsafe(method(setKeyEquivalent:))]
        fn set_key_equivalent(&self, key: &NSString) {
            let ivars = self.ivars();
            if ivars.key.borrow().isEqualToString(key) {
                return;
            }
            let old = ivars.key.replace(key.copy());
            ivars.shortcut.set(Shortcut::new(key, ivars.mask.get()));
            changed(self);
            drop(old);
        }

        #[unsafe(method(keyEquivalentModifierMask))]
        fn key_equivalent_modifier_mask(&self) -> NSEventModifierFlags {
            self.ivars().mask.get()
        }

        #[unsafe(method(setKeyEquivalentModifierMask:))]
        fn set_key_equivalent_modifier_mask(&self, mask: NSEventModifierFlags) {
            let ivars = self.ivars();
            if ivars.mask.replace(mask) == mask {
                return;
            }
            ivars.shortcut.set(Shortcut::new(&ivars.key.borrow(), mask));
            changed(self);
        }

        /// The user's own shortcut for the item: Linux keeps none.
        #[unsafe(method_id(userKeyEquivalent))]
        fn user_key_equivalent(&self) -> Retained<NSString> {
            NSString::new()
        }

        #[unsafe(method(allowsKeyEquivalentWhenHidden))]
        fn allows_key_equivalent_when_hidden(&self) -> bool {
            self.ivars().when_hidden.get()
        }

        #[unsafe(method(setAllowsKeyEquivalentWhenHidden:))]
        fn set_allows_key_equivalent_when_hidden(&self, flag: bool) {
            self.ivars().when_hidden.set(flag);
        }

        #[unsafe(method(allowsAutomaticKeyEquivalentLocalization))]
        fn allows_automatic_key_equivalent_localization(&self) -> bool {
            self.ivars().localizes_key.get()
        }

        #[unsafe(method(setAllowsAutomaticKeyEquivalentLocalization:))]
        fn set_allows_automatic_key_equivalent_localization(&self, flag: bool) {
            self.ivars().localizes_key.set(flag);
        }

        #[unsafe(method(allowsAutomaticKeyEquivalentMirroring))]
        fn allows_automatic_key_equivalent_mirroring(&self) -> bool {
            self.ivars().mirrors_key.get()
        }

        #[unsafe(method(setAllowsAutomaticKeyEquivalentMirroring:))]
        fn set_allows_automatic_key_equivalent_mirroring(&self, flag: bool) {
            self.ivars().mirrors_key.set(flag);
        }

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<NSImage>> {
            self.ivars().image.borrow().clone()
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, image: Option<&NSImage>) {
            set_shown(self, &self.ivars().image, image);
        }

        #[unsafe(method(state))]
        fn state(&self) -> isize {
            self.ivars().state.get()
        }

        #[unsafe(method(setState:))]
        fn set_state(&self, state: isize) {
            if self.ivars().state.replace(state) != state {
                changed(self);
            }
        }

        /// A check mark, until one is set.
        #[unsafe(method_id(onStateImage))]
        fn on_state_image(&self) -> Option<Retained<NSImage>> {
            let set = self.ivars().on_image.borrow().clone();
            set.or_else(|| state_image("checkmark"))
        }

        #[unsafe(method(setOnStateImage:))]
        fn set_on_state_image(&self, image: Option<&NSImage>) {
            set_state_image(self, &self.ivars().on_image, image, 1);
        }

        #[unsafe(method_id(offStateImage))]
        fn off_state_image(&self) -> Option<Retained<NSImage>> {
            self.ivars().off_image.borrow().clone()
        }

        #[unsafe(method(setOffStateImage:))]
        fn set_off_state_image(&self, image: Option<&NSImage>) {
            set_state_image(self, &self.ivars().off_image, image, 0);
        }

        /// A dash, until one is set.
        #[unsafe(method_id(mixedStateImage))]
        fn mixed_state_image(&self) -> Option<Retained<NSImage>> {
            let set = self.ivars().mixed_image.borrow().clone();
            set.or_else(|| state_image("minus"))
        }

        #[unsafe(method(setMixedStateImage:))]
        fn set_mixed_state_image(&self, image: Option<&NSImage>) {
            set_state_image(self, &self.ivars().mixed_image, image, -1);
        }

        #[unsafe(method(isEnabled))]
        fn is_enabled(&self) -> bool {
            self.ivars().enabled.get()
        }

        #[unsafe(method(setEnabled:))]
        fn set_enabled(&self, enabled: bool) {
            if self.ivars().enabled.replace(enabled) != enabled {
                changed(self);
            }
        }

        #[unsafe(method(isAlternate))]
        fn is_alternate(&self) -> bool {
            self.ivars().alternate.get()
        }

        #[unsafe(method(setAlternate:))]
        fn set_alternate(&self, alternate: bool) {
            if self.ivars().alternate.replace(alternate) != alternate {
                changed(self);
            }
        }

        #[unsafe(method(indentationLevel))]
        fn indentation_level(&self) -> isize {
            self.ivars().indentation.get()
        }

        /// From 0 to 15.
        #[unsafe(method(setIndentationLevel:))]
        fn set_indentation_level(&self, level: isize) {
            let level = level.clamp(0, 15);
            if self.ivars().indentation.replace(level) != level {
                changed(self);
            }
        }

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            self.ivars().target.borrow().load()
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            let old = self.ivars().target.replace(target.map_or_else(Weak::default, Weak::new));
            drop(old);
        }

        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> {
            self.ivars().action.get()
        }

        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) {
            self.ivars().action.set(action);
        }

        #[unsafe(method(tag))]
        fn tag(&self) -> isize {
            self.ivars().tag.get()
        }

        #[unsafe(method(setTag:))]
        fn set_tag(&self, tag: isize) {
            self.ivars().tag.set(tag);
        }

        #[unsafe(method_id(representedObject))]
        fn represented_object(&self) -> Option<Retained<AnyObject>> {
            self.ivars().represented.borrow().clone()
        }

        #[unsafe(method(setRepresentedObject:))]
        fn set_represented_object(&self, object: Option<&AnyObject>) {
            let old = self.ivars().represented.replace(object.map(|o| o.retain()));
            drop(old);
        }

        #[unsafe(method_id(view))]
        fn view(&self) -> Option<Retained<NSView>> {
            self.ivars().view.borrow().clone()
        }

        #[unsafe(method(setView:))]
        fn set_view(&self, view: Option<&NSView>) {
            set_shown(self, &self.ivars().view, view);
        }

        #[unsafe(method(isHighlighted))]
        fn is_highlighted(&self) -> bool {
            self.ivars().highlighted.get()
        }

        #[unsafe(method(isHidden))]
        fn is_hidden(&self) -> bool {
            self.ivars().hidden.get()
        }

        #[unsafe(method(setHidden:))]
        fn set_hidden(&self, hidden: bool) {
            if self.ivars().hidden.replace(hidden) != hidden {
                changed(self);
            }
        }

        #[unsafe(method(isHiddenOrHasHiddenAncestor))]
        fn is_hidden_or_has_hidden_ancestor(&self) -> bool {
            hidden_up(self.ivars())
        }

        #[unsafe(method_id(toolTip))]
        fn tool_tip(&self) -> Option<Retained<NSString>> {
            self.ivars().tooltip.borrow().clone()
        }

        #[unsafe(method(setToolTip:))]
        fn set_tool_tip(&self, tip: Option<&NSString>) {
            let same = match (&*self.ivars().tooltip.borrow(), tip) {
                (Some(a), Some(b)) => a.isEqualToString(b),
                (None, None) => true,
                _ => false,
            };
            if !same {
                let old = self.ivars().tooltip.replace(tip.map(|t| t.copy()));
                changed(self);
                drop(old);
            }
        }

        #[unsafe(method_id(badge))]
        fn badge(&self) -> Option<Retained<AnyObject>> {
            self.ivars().badge.borrow().clone()
        }

        #[unsafe(method(setBadge:))]
        fn set_badge(&self, badge: Option<&AnyObject>) {
            set_shown(self, &self.ivars().badge, badge);
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Option<Retained<NSString>> {
            self.ivars().identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: Option<&NSString>) {
            let old = self.ivars().identifier.replace(identifier.map(|i| i.copy()));
            drop(old);
        }

        // The old mnemonics: an ampersand marked the letter to type.

        #[unsafe(method(setMnemonicLocation:))]
        fn set_mnemonic_location(&self, _location: usize) {}

        #[unsafe(method(mnemonicLocation))]
        fn mnemonic_location(&self) -> usize {
            objc2_foundation::NSNotFound as usize
        }

        #[unsafe(method_id(mnemonic))]
        fn mnemonic(&self) -> Option<Retained<NSString>> {
            None
        }

        #[unsafe(method(setTitleWithMnemonic:))]
        fn set_title_with_mnemonic(&self, title: &NSString) {
            let plain = title.to_string().replacen('&', "", 1);
            as_item(self).setTitle(&NSString::from_str(&plain));
        }
    }

    unsafe impl NSObjectProtocol for NSMenuItemImpl {}
);

/// A new item, made as `+[NSMenuItem alloc]` and the designated
/// initializer make one.
fn new_item(title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
    let mtm = MainThreadMarker::new().expect("sidestep: menus belong to the main thread");
    // SAFETY: the designated initializer, with a selector or none.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

/// A symbol image for a state column, made once per thread and shared.
fn state_image(name: &'static str) -> Option<Retained<NSImage>> {
    thread_local!(static IMAGES: RefCell<Vec<(&'static str, Retained<NSImage>)>> = const { RefCell::new(Vec::new()) });
    if let Some(found) = IMAGES.with(|i| i.borrow().iter().find(|(n, _)| *n == name).map(|(_, i)| i.clone())) {
        return Some(found);
    }
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    IMAGES.with(|i| i.borrow_mut().push((name, image.clone())));
    Some(image)
}

/// Keep `value` in `slot`, telling the menu if it's a different object.
fn set_shown<T: Message>(item: &NSMenuItemImpl, slot: &RefCell<Option<Retained<T>>>, value: Option<&T>) {
    let same = match (&*slot.borrow(), value) {
        (Some(a), Some(b)) => std::ptr::eq(&**a, b),
        (None, None) => true,
        _ => false,
    };
    if !same {
        let old = slot.replace(value.map(|v| v.retain()));
        changed(item);
        drop(old);
    }
}

/// Keep `value` in `slot`, the image for items in `state`: telling the menu
/// if it's a different object, and posting it only if the item is in that
/// state, as macOS does (the image doesn't show otherwise).
fn set_state_image(
    item: &NSMenuItemImpl,
    slot: &RefCell<Option<Retained<NSImage>>>,
    value: Option<&NSImage>,
    state: isize,
) {
    if item.ivars().state.get() == state {
        set_shown(item, slot, value);
        return;
    }
    let same = match (&*slot.borrow(), value) {
        (Some(a), Some(b)) => std::ptr::eq(&**a, b),
        (None, None) => true,
        _ => false,
    };
    if !same {
        let old = slot.replace(value.map(|v| v.retain()));
        if let Some(menu) = item.ivars().menu.get() {
            // SAFETY: the menu holds the item while the link names it.
            bump(unsafe { menu.as_ref() });
        }
        drop(old);
    }
}

/// An item changed in a way that shows: its menu's generation moves, and
/// the menu posts it.
fn changed(item: &NSMenuItemImpl) {
    let Some(menu) = item.ivars().menu.get() else { return };
    // SAFETY: the menu holds the item while the link names it.
    let menu = unsafe { menu.as_ref() };
    bump(menu);
    let index = index_of(menu, as_item(item));
    if let Some(index) = index {
        post_index(note!(NSMenuDidChangeItemNotification), menu, index);
    }
}

pub(crate) fn item_ivars(item: &NSMenuItem) -> &ItemIvars {
    // SAFETY: every NSMenuItem is an NSMenuItemImpl (a subclass's
    // instances start with its ivars).
    unsafe { &*(item as *const NSMenuItem).cast::<NSMenuItemImpl>() }.ivars()
}

fn as_item(item: &NSMenuItemImpl) -> &NSMenuItem {
    // SAFETY: NSMenuItemImpl is the class NSMenuItem names.
    unsafe { &*(item as *const NSMenuItemImpl).cast::<NSMenuItem>() }
}

impl ItemIvars {
    pub(crate) fn submenu(&self) -> Option<Retained<NSMenu>> {
        self.submenu.borrow().clone()
    }

    pub(crate) fn is_separator(&self) -> bool {
        self.separator.get()
    }

    pub(crate) fn is_hidden(&self) -> bool {
        self.hidden.get()
    }

    pub(crate) fn action(&self) -> Option<Sel> {
        self.action.get()
    }
}

/// Hide or show `item` without telling anyone but the layouts (a
/// pull-down's first item, while its menu shows).
pub(crate) fn hide_quietly(item: &NSMenuItem, hidden: bool) {
    let ivars = item_ivars(item);
    if ivars.hidden.replace(hidden) != hidden
        && let Some(menu) = ivars.menu.get()
    {
        // SAFETY: the menu holds the item while the link names it.
        let g = &menu_ivars(unsafe { menu.as_ref() }).generation;
        g.set(g.get() + 1);
    }
}

// Menus.

pub(crate) struct MenuIvars {
    title: RefCell<Retained<NSString>>,
    items: RefCell<Vec<Retained<NSMenuItem>>>,
    /// The menu holding the item whose submenu this is, while it holds it.
    supermenu: Unretained<NSMenu>,
    autoenables: Cell<bool>,
    delegate: RefCell<Weak<AnyObject>>,
    minimum_width: Cell<f64>,
    font: RefCell<Option<Retained<NSFont>>>,
    shows_state_column: Cell<bool>,
    direction: Cell<NSUserInterfaceLayoutDirection>,
    presentation: Cell<NSMenuPresentationStyle>,
    selection: Cell<NSMenuSelectionMode>,
    plug_ins: Cell<bool>,
    writing_tools: Cell<bool>,
    change_messages: Cell<bool>,
    identifier: RefCell<Option<Retained<NSString>>>,
    /// Counted up whenever the menu or an item changes in a way that shows.
    generation: Cell<u64>,
    /// The item a menu being tracked shows highlighted.
    highlighted: RefCell<Option<Retained<NSMenuItem>>>,
    /// The pop-up button cell whose menu this is, told of its changes
    /// (see `popup_button`); `owned` says whether there is one without a
    /// weak load.
    owner: RefCell<Weak<AnyObject>>,
    owned: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSMenu"]
    #[ivars = MenuIvars]
    pub(crate) struct NSMenuImpl;

    impl NSMenuImpl {
        #[unsafe(method_id(initWithTitle:))]
        fn init_with_title(this: Allocated<Self>, title: &NSString) -> Retained<Self> {
            let this = this.set_ivars(MenuIvars {
                title: RefCell::new(title.copy()),
                items: RefCell::new(Vec::new()),
                supermenu: Cell::new(None),
                autoenables: Cell::new(true),
                delegate: RefCell::new(Weak::default()),
                minimum_width: Cell::new(0.0),
                font: RefCell::new(None),
                shows_state_column: Cell::new(true),
                direction: Cell::new(NSUserInterfaceLayoutDirection::LeftToRight),
                presentation: Cell::new(NSMenuPresentationStyle::Regular),
                selection: Cell::new(NSMenuSelectionMode::Automatic),
                plug_ins: Cell::new(true),
                writing_tools: Cell::new(true),
                change_messages: Cell::new(true),
                identifier: RefCell::new(None),
                generation: Cell::new(0),
                highlighted: RefCell::new(None),
                owner: RefCell::new(Weak::default()),
                owned: Cell::new(false),
            });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer, with no title.
            unsafe { msg_send![this, initWithTitle: &*NSString::new()] }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSMenu> {
            let mine = self.ivars();
            // SAFETY: a menu's class makes menus with alloc and the
            // designated initializer.
            let copy: Retained<NSMenu> = unsafe {
                let allocated: Allocated<NSMenu> = msg_send![self.class(), alloc];
                msg_send![allocated, initWithTitle: &**mine.title.borrow()]
            };
            let theirs = menu_ivars(&copy);
            theirs.autoenables.set(mine.autoenables.get());
            theirs.delegate.replace(mine.delegate.borrow().clone());
            theirs.minimum_width.set(mine.minimum_width.get());
            theirs.font.replace(mine.font.borrow().clone());
            theirs.shows_state_column.set(mine.shows_state_column.get());
            theirs.direction.set(mine.direction.get());
            theirs.presentation.set(mine.presentation.get());
            theirs.selection.set(mine.selection.get());
            theirs.plug_ins.set(mine.plug_ins.get());
            theirs.writing_tools.set(mine.writing_tools.get());
            theirs.identifier.replace(mine.identifier.borrow().clone());
            let items = mine.items.borrow().clone();
            for item in items {
                copy.addItem(&item.copy());
            }
            copy
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.ivars().title.borrow().clone()
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            let old = self.ivars().title.replace(title.copy());
            bump(as_menu(self));
            crate::menubar::title_changed(as_menu(self));
            drop(old);
        }

        #[unsafe(method(popUpContextMenu:withEvent:forView:))]
        fn pop_up_context_menu(menu: &NSMenu, event: &NSEvent, view: &NSView) {
            crate::menu_tracking::pop_up_context_menu(menu, event, view, None);
        }

        #[unsafe(method(popUpContextMenu:withEvent:forView:withFont:))]
        fn pop_up_context_menu_with_font(menu: &NSMenu, event: &NSEvent, view: &NSView, font: Option<&NSFont>) {
            crate::menu_tracking::pop_up_context_menu(menu, event, view, font);
        }

        #[unsafe(method(popUpMenuPositioningItem:atLocation:inView:))]
        fn pop_up_menu_positioning_item(
            &self,
            item: Option<&NSMenuItem>,
            location: NSPoint,
            view: Option<&NSView>,
        ) -> bool {
            crate::menu_tracking::pop_up_positioning(as_menu(self), item, location, view)
        }

        #[unsafe(method(setMenuBarVisible:))]
        fn set_menu_bar_visible(visible: bool) {
            if BAR_VISIBLE.with(|b| b.replace(visible)) != visible {
                crate::menubar::visibility_changed();
            }
        }

        #[unsafe(method(menuBarVisible))]
        fn menu_bar_visible() -> bool {
            BAR_VISIBLE.with(Cell::get)
        }

        #[unsafe(method_id(supermenu))]
        fn supermenu(&self) -> Option<Retained<NSMenu>> {
            // SAFETY: the supermenu holds the host item holding this menu
            // while the link names it, and clears it when it lets go.
            self.ivars().supermenu.get().map(|m| unsafe { m.as_ref() }.retain())
        }

        #[unsafe(method(setSupermenu:))]
        fn set_supermenu(&self, supermenu: Option<&NSMenu>) {
            self.ivars().supermenu.set(supermenu.map(NonNull::from));
        }

        #[unsafe(method(insertItem:atIndex:))]
        fn insert_item(&self, item: &NSMenuItem, index: isize) {
            insert(self, item, index);
        }

        #[unsafe(method(addItem:))]
        fn add_item(&self, item: &NSMenuItem) {
            let count = self.ivars().items.borrow().len();
            // SAFETY: insertItem:atIndex: takes an item and an index, by
            // message as subclasses may override it.
            unsafe { msg_send![self, insertItem: item, atIndex: count as isize] }
        }

        #[unsafe(method_id(insertItemWithTitle:action:keyEquivalent:atIndex:))]
        fn insert_item_with_title(
            &self,
            title: &NSString,
            action: Option<Sel>,
            key: &NSString,
            index: isize,
        ) -> Retained<NSMenuItem> {
            insert_new(self, title, action, key, index)
        }

        #[unsafe(method_id(addItemWithTitle:action:keyEquivalent:))]
        fn add_item_with_title(&self, title: &NSString, action: Option<Sel>, key: &NSString) -> Retained<NSMenuItem> {
            let count = self.ivars().items.borrow().len() as isize;
            insert_new(self, title, action, key, count)
        }

        #[unsafe(method(removeItemAtIndex:))]
        fn remove_item_at_index(&self, index: isize) {
            remove(self, index, true);
        }

        #[unsafe(method(removeItem:))]
        fn remove_item(&self, item: &NSMenuItem) {
            if let Some(index) = index_of(as_menu(self), item) {
                // SAFETY: removeItemAtIndex: takes an index, by message as
                // subclasses may override it.
                unsafe { msg_send![self, removeItemAtIndex: index as isize] }
            }
        }

        #[unsafe(method(setSubmenu:forItem:))]
        fn set_submenu_for_item(&self, menu: Option<&NSMenu>, item: &NSMenuItem) {
            item.setSubmenu(menu);
        }

        /// Posts no removals, as macOS doesn't.
        #[unsafe(method(removeAllItems))]
        fn remove_all_items(&self) {
            while !self.ivars().items.borrow().is_empty() {
                let last = self.ivars().items.borrow().len() as isize - 1;
                remove(self, last, false);
            }
        }

        #[unsafe(method_id(itemArray))]
        fn item_array(&self) -> Retained<NSArray<NSMenuItem>> {
            NSArray::from_retained_slice(&self.ivars().items.borrow())
        }

        #[unsafe(method(setItemArray:))]
        fn set_item_array(&self, items: &NSArray<NSMenuItem>) {
            let menu = as_menu(self);
            menu.removeAllItems();
            for item in items.iter() {
                menu.addItem(&item);
            }
        }

        #[unsafe(method(numberOfItems))]
        fn number_of_items(&self) -> isize {
            self.ivars().items.borrow().len() as isize
        }

        #[unsafe(method_id(itemAtIndex:))]
        fn item_at_index(&self, index: isize) -> Option<Retained<NSMenuItem>> {
            Some(item_at(self, index, "itemAtIndex:"))
        }

        #[unsafe(method(indexOfItem:))]
        fn index_of_item(&self, item: &NSMenuItem) -> isize {
            index_of(as_menu(self), item).map_or(-1, |i| i as isize)
        }

        #[unsafe(method(indexOfItemWithTitle:))]
        fn index_of_item_with_title(&self, title: &NSString) -> isize {
            find(self, |i| i.title.borrow().isEqualToString(title))
        }

        #[unsafe(method(indexOfItemWithTag:))]
        fn index_of_item_with_tag(&self, tag: isize) -> isize {
            find(self, |i| i.tag.get() == tag)
        }

        /// The first item whose represented object is `object`, or has
        /// none when `object` is nil.
        #[unsafe(method(indexOfItemWithRepresentedObject:))]
        fn index_of_item_with_represented_object(&self, object: Option<&AnyObject>) -> isize {
            find(self, |i| match (&*i.represented.borrow(), object) {
                (Some(a), Some(b)) => is_equal(a, b),
                (None, None) => true,
                _ => false,
            })
        }

        /// The first item whose submenu is `submenu`, or has none when
        /// `submenu` is nil.
        #[unsafe(method(indexOfItemWithSubmenu:))]
        fn index_of_item_with_submenu(&self, submenu: Option<&NSMenu>) -> isize {
            find(self, |i| match (&*i.submenu.borrow(), submenu) {
                (Some(a), Some(b)) => std::ptr::eq(&**a, b),
                (None, None) => true,
                _ => false,
            })
        }

        /// The first item with this target and action; without an action,
        /// the first with this target.
        #[unsafe(method(indexOfItemWithTarget:andAction:))]
        fn index_of_item_with_target_and_action(&self, target: Option<&AnyObject>, action: Option<Sel>) -> isize {
            find(self, |i| {
                let theirs = i.target.borrow().load();
                let same_target = match (&theirs, target) {
                    (Some(a), Some(b)) => std::ptr::eq(&**a, b),
                    (None, None) => true,
                    _ => false,
                };
                same_target && (action.is_none() || i.action.get() == action)
            })
        }

        #[unsafe(method_id(itemWithTitle:))]
        fn item_with_title(&self, title: &NSString) -> Option<Retained<NSMenuItem>> {
            let items = self.ivars().items.borrow();
            items.iter().find(|i| item_ivars(i).title.borrow().isEqualToString(title)).cloned()
        }

        #[unsafe(method_id(itemWithTag:))]
        fn item_with_tag(&self, tag: isize) -> Option<Retained<NSMenuItem>> {
            let items = self.ivars().items.borrow();
            items.iter().find(|i| item_ivars(i).tag.get() == tag).cloned()
        }

        #[unsafe(method(autoenablesItems))]
        fn autoenables_items(&self) -> bool {
            self.ivars().autoenables.get()
        }

        #[unsafe(method(setAutoenablesItems:))]
        fn set_autoenables_items(&self, flag: bool) {
            self.ivars().autoenables.set(flag);
        }

        #[unsafe(method(update))]
        fn update(&self) {
            update(self);
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            perform_key_equivalent(self, event)
        }

        #[unsafe(method(itemChanged:))]
        fn item_changed(&self, item: &NSMenuItem) {
            bump(as_menu(self));
            if let Some(index) = index_of(as_menu(self), item) {
                post_index(note!(NSMenuDidChangeItemNotification), as_menu(self), index);
            }
        }

        /// Sends the item's action if the item is enabled; it isn't
        /// validated first. An index past the items does nothing, as on
        /// macOS.
        #[unsafe(method(performActionForItemAtIndex:))]
        fn perform_action_for_item_at_index(&self, index: isize) {
            let item = usize::try_from(index).ok().and_then(|i| self.ivars().item(i));
            if let Some(item) = item.filter(|i| i.isEnabled()) {
                send_action(&item);
            }
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            let old = self.ivars().delegate.replace(delegate.map_or_else(Weak::default, Weak::new));
            drop(old);
        }

        #[unsafe(method(menuBarHeight))]
        fn menu_bar_height(&self) -> f64 {
            crate::menubar::height_for(as_menu(self))
        }

        #[unsafe(method(cancelTracking))]
        fn cancel_tracking(&self) {
            crate::menu_tracking::cancel(as_menu(self));
        }

        #[unsafe(method(cancelTrackingWithoutAnimation))]
        fn cancel_tracking_without_animation(&self) {
            crate::menu_tracking::cancel(as_menu(self));
        }

        #[unsafe(method_id(highlightedItem))]
        fn highlighted_item(&self) -> Option<Retained<NSMenuItem>> {
            self.ivars().highlighted.borrow().clone()
        }

        #[unsafe(method(minimumWidth))]
        fn minimum_width(&self) -> f64 {
            self.ivars().minimum_width.get()
        }

        #[unsafe(method(setMinimumWidth:))]
        fn set_minimum_width(&self, width: f64) {
            self.ivars().minimum_width.set(width);
            bump(as_menu(self));
        }

        /// The size the menu would show at.
        #[unsafe(method(size))]
        fn size(&self) -> NSSize {
            crate::menu_view::size_of(as_menu(self))
        }

        #[unsafe(method_id(font))]
        fn font(&self) -> Retained<NSFont> {
            let set = self.ivars().font.borrow().clone();
            set.unwrap_or_else(|| NSFont::menuFontOfSize(0.0))
        }

        #[unsafe(method(setFont:))]
        fn set_font(&self, font: Option<&NSFont>) {
            let old = self.ivars().font.replace(font.map(|f| f.retain()));
            bump(as_menu(self));
            drop(old);
        }

        #[unsafe(method(allowsContextMenuPlugIns))]
        fn allows_context_menu_plug_ins(&self) -> bool {
            self.ivars().plug_ins.get()
        }

        #[unsafe(method(setAllowsContextMenuPlugIns:))]
        fn set_allows_context_menu_plug_ins(&self, flag: bool) {
            self.ivars().plug_ins.set(flag);
        }

        #[unsafe(method(automaticallyInsertsWritingToolsItems))]
        fn automatically_inserts_writing_tools_items(&self) -> bool {
            self.ivars().writing_tools.get()
        }

        #[unsafe(method(setAutomaticallyInsertsWritingToolsItems:))]
        fn set_automatically_inserts_writing_tools_items(&self, flag: bool) {
            self.ivars().writing_tools.set(flag);
        }

        #[unsafe(method(showsStateColumn))]
        fn shows_state_column(&self) -> bool {
            self.ivars().shows_state_column.get()
        }

        #[unsafe(method(setShowsStateColumn:))]
        fn set_shows_state_column(&self, flag: bool) {
            self.ivars().shows_state_column.set(flag);
            bump(as_menu(self));
        }

        #[unsafe(method(userInterfaceLayoutDirection))]
        fn user_interface_layout_direction(&self) -> NSUserInterfaceLayoutDirection {
            self.ivars().direction.get()
        }

        #[unsafe(method(setUserInterfaceLayoutDirection:))]
        fn set_user_interface_layout_direction(&self, direction: NSUserInterfaceLayoutDirection) {
            self.ivars().direction.set(direction);
            bump(as_menu(self));
        }

        #[unsafe(method(presentationStyle))]
        fn presentation_style(&self) -> NSMenuPresentationStyle {
            self.ivars().presentation.get()
        }

        #[unsafe(method(setPresentationStyle:))]
        fn set_presentation_style(&self, style: NSMenuPresentationStyle) {
            self.ivars().presentation.set(style);
        }

        #[unsafe(method(selectionMode))]
        fn selection_mode(&self) -> NSMenuSelectionMode {
            self.ivars().selection.get()
        }

        #[unsafe(method(setSelectionMode:))]
        fn set_selection_mode(&self, mode: NSMenuSelectionMode) {
            self.ivars().selection.set(mode);
        }

        /// The items that are on.
        #[unsafe(method_id(selectedItems))]
        fn selected_items(&self) -> Retained<NSArray<NSMenuItem>> {
            let on: Vec<_> =
                self.ivars().items.borrow().iter().filter(|i| item_ivars(i).state.get() == 1).cloned().collect();
            NSArray::from_retained_slice(&on)
        }

        #[unsafe(method(setSelectedItems:))]
        fn set_selected_items(&self, selected: &NSArray<NSMenuItem>) {
            let items = self.ivars().items.borrow().clone();
            for item in items {
                let on = selected.iter().any(|s| std::ptr::eq(&*s, &*item));
                item.setState(if on { 1 } else { 0 });
            }
        }

        /// What opens a submenu; nothing to do when sent.
        #[unsafe(method(submenuAction:))]
        fn submenu_action(&self, _sender: Option<&AnyObject>) {}

        /// Only while the delegate is asked `menuNeedsUpdate:`: every
        /// property.
        #[unsafe(method(propertiesToUpdate))]
        fn properties_to_update(&self) -> NSMenuProperties {
            assert!(
                UPDATING.with(Cell::get) > 0,
                "-[NSMenu propertiesToUpdate] may only be called from -menuNeedsUpdate:"
            );
            NSMenuProperties::all()
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Option<Retained<NSString>> {
            self.ivars().identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: Option<&NSString>) {
            let old = self.ivars().identifier.replace(identifier.map(|i| i.copy()));
            drop(old);
        }

        // What old versions of AppKit had, answered as a menu that is
        // never torn off or attached.

        #[unsafe(method(setMenuRepresentation:))]
        fn set_menu_representation(&self, _rep: Option<&AnyObject>) {}

        #[unsafe(method_id(menuRepresentation))]
        fn menu_representation(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setContextMenuRepresentation:))]
        fn set_context_menu_representation(&self, _rep: Option<&AnyObject>) {}

        #[unsafe(method_id(contextMenuRepresentation))]
        fn context_menu_representation(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setTearOffMenuRepresentation:))]
        fn set_tear_off_menu_representation(&self, _rep: Option<&AnyObject>) {}

        #[unsafe(method_id(tearOffMenuRepresentation))]
        fn tear_off_menu_representation(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(menuZone))]
        fn menu_zone() -> *mut NSZone {
            std::ptr::null_mut()
        }

        #[unsafe(method(setMenuZone:))]
        fn set_menu_zone(_zone: *mut NSZone) {}

        #[unsafe(method_id(attachedMenu))]
        fn attached_menu(&self) -> Option<Retained<NSMenu>> {
            None
        }

        #[unsafe(method(isAttached))]
        fn is_attached(&self) -> bool {
            false
        }

        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {}

        #[unsafe(method(locationForSubmenu:))]
        fn location_for_submenu(&self, _submenu: Option<&NSMenu>) -> NSPoint {
            NSPoint::ZERO
        }

        #[unsafe(method(menuChangedMessagesEnabled))]
        fn menu_changed_messages_enabled(&self) -> bool {
            self.ivars().change_messages.get()
        }

        #[unsafe(method(setMenuChangedMessagesEnabled:))]
        fn set_menu_changed_messages_enabled(&self, flag: bool) {
            self.ivars().change_messages.set(flag);
        }

        #[unsafe(method(helpRequested:))]
        fn help_requested(&self, _event: &NSEvent) {}

        #[unsafe(method(isTornOff))]
        fn is_torn_off(&self) -> bool {
            false
        }
    }

    unsafe impl NSObjectProtocol for NSMenuImpl {}
);

impl Drop for NSMenuImpl {
    fn drop(&mut self) {
        // The items may outlive the menu, and their submenus: nothing is
        // left pointing at it.
        let me = Some(NonNull::from(as_menu(self)));
        for item in self.ivars().items.borrow().iter() {
            let ivars = item_ivars(item);
            ivars.menu.set(None);
            if let Some(sub) = &*ivars.submenu.borrow()
                && menu_ivars(sub).supermenu.get() == me
            {
                menu_ivars(sub).supermenu.set(None);
            }
        }
    }
}

/// The item at `index`; out of range, a panic naming `method`.
fn item_at(menu: &NSMenuImpl, index: isize, method: &str) -> Retained<NSMenuItem> {
    let items = menu.ivars().items.borrow();
    match usize::try_from(index).ok().and_then(|i| items.get(i)) {
        Some(item) => item.clone(),
        None => panic!("*** -[NSMenu {method}]: index ({index}) beyond bounds ({})", items.len()),
    }
}

/// `insertItemWithTitle:action:keyEquivalent:atIndex:`: a new item, put in
/// by message as subclasses may override `insertItem:atIndex:`.
fn insert_new(
    menu: &NSMenuImpl,
    title: &NSString,
    action: Option<Sel>,
    key: &NSString,
    index: isize,
) -> Retained<NSMenuItem> {
    let mtm = MainThreadMarker::from(menu);
    // SAFETY: the designated initializer.
    let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), title, action, key) };
    as_menu(menu).insertItem_atIndex(&item, index);
    item
}

/// Whether the item or an item above it is hidden.
fn hidden_up(item: &ItemIvars) -> bool {
    if item.hidden.get() {
        return true;
    }
    let mut host = parent_of(item);
    while let Some(h) = host {
        if item_ivars(&h).hidden.get() {
            return true;
        }
        host = parent_of(item_ivars(&h));
    }
    false
}

/// The item whose submenu holds the item with these ivars.
fn parent_of(item: &ItemIvars) -> Option<Retained<NSMenuItem>> {
    // SAFETY: the menu holds the item while the link names it.
    let menu = unsafe { item.menu.get()?.as_ref() };
    host_of(menu)
}

pub(crate) fn menu_ivars(menu: &NSMenu) -> &MenuIvars {
    // SAFETY: every NSMenu is an NSMenuImpl (a subclass's instances start
    // with its ivars).
    unsafe { &*(menu as *const NSMenu).cast::<NSMenuImpl>() }.ivars()
}

fn as_menu(menu: &NSMenuImpl) -> &NSMenu {
    // SAFETY: NSMenuImpl is the class NSMenu names.
    unsafe { &*(menu as *const NSMenuImpl).cast::<NSMenu>() }
}

impl MenuIvars {
    /// Counted up whenever the menu or an item changes in a way that shows.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// The `i`th item, if there are that many.
    pub(crate) fn item(&self, i: usize) -> Option<Retained<NSMenuItem>> {
        self.items.borrow().get(i).cloned()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.borrow().len()
    }

    pub(crate) fn minimum_width(&self) -> f64 {
        self.minimum_width.get()
    }

    pub(crate) fn shows_state_column(&self) -> bool {
        self.shows_state_column.get()
    }

    pub(crate) fn font(&self) -> Option<Retained<NSFont>> {
        self.font.borrow().clone()
    }

    pub(crate) fn delegate(&self) -> Option<Retained<AnyObject>> {
        self.delegate.borrow().load()
    }

    /// Have `owner` (a pop-up button cell) told of the menu's changes, or
    /// no one.
    pub(crate) fn set_owner<T: Message>(&self, owner: Option<&T>) {
        let owner: Option<&AnyObject> = owner.map(|o| {
            // SAFETY: every Message type is an object.
            unsafe { &*(o as *const T).cast::<AnyObject>() }
        });
        self.owned.set(owner.is_some());
        let old = self.owner.replace(owner.map_or_else(Weak::default, Weak::new));
        drop(old);
    }

    /// The pop-up button cell whose menu this is.
    fn owner(&self) -> Option<Retained<AnyObject>> {
        if self.owned.get() { self.owner.borrow().load() } else { None }
    }

    /// The item a menu being tracked shows highlighted.
    pub(crate) fn highlighted_item(&self) -> Option<Retained<NSMenuItem>> {
        self.highlighted.borrow().clone()
    }

    /// Show `item` highlighted (or none), as a menu being tracked does.
    pub(crate) fn set_highlighted(&self, item: Option<&NSMenuItem>) {
        let old = self.highlighted.replace(item.map(|i| i.retain()));
        if let Some(old) = &old {
            item_ivars(old).highlighted.set(false);
        }
        if let Some(item) = item {
            item_ivars(item).highlighted.set(true);
        }
        drop(old);
    }
}

/// The menu changed in a way that shows.
pub(crate) fn bump(menu: &NSMenu) {
    let ivars = menu_ivars(menu);
    ivars.generation.set(ivars.generation.get() + 1);
    crate::menubar::menu_changed(menu);
    crate::menu_tracking::menu_changed(menu);
    if let Some(owner) = ivars.owner() {
        crate::popup_button::menu_changed(&owner);
    }
}

/// Where `item` is in `menu`.
pub(crate) fn index_of(menu: &NSMenu, item: &NSMenuItem) -> Option<usize> {
    menu_ivars(menu).items.borrow().iter().position(|i| std::ptr::eq(&**i, item))
}

fn find(menu: &NSMenuImpl, mut test: impl FnMut(&ItemIvars) -> bool) -> isize {
    menu.ivars().items.borrow().iter().position(|i| test(item_ivars(i))).map_or(-1, |i| i as isize)
}

fn is_equal(a: &AnyObject, b: &AnyObject) -> bool {
    // SAFETY: isEqual: takes an object and returns BOOL.
    std::ptr::eq(a, b) || unsafe { msg_send![a, isEqual: b] }
}

fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

fn insert(menu: &NSMenuImpl, item: &NSMenuItem, index: isize) {
    let ivars = item_ivars(item);
    assert!(ivars.menu.get().is_none(), "Item to be inserted into menu already is in another menu");
    let len = menu.ivars().items.borrow().len();
    let at = match usize::try_from(index) {
        Ok(i) if i <= len => i,
        _ => panic!("*** -[NSMenu insertItem:atIndex:]: index ({index}) beyond bounds ({len})"),
    };
    menu.ivars().items.borrow_mut().insert(at, item.retain());
    let me = NonNull::from(as_menu(menu));
    ivars.menu.set(Some(me));
    if let Some(sub) = &*ivars.submenu.borrow() {
        menu_ivars(sub).supermenu.set(Some(me));
    }
    if let Some(owner) = menu.ivars().owner() {
        crate::popup_button::item_added(&owner, item);
    }
    bump(as_menu(menu));
    post_index(note!(NSMenuDidAddItemNotification), as_menu(menu), at);
}

/// Take out the item at `index`, posting `NSMenuDidRemoveItemNotification`
/// if `post`.
fn remove(menu: &NSMenuImpl, index: isize, post: bool) {
    let len = menu.ivars().items.borrow().len();
    let at = match usize::try_from(index) {
        Ok(i) if i < len => i,
        _ => panic!("*** -[NSMenu removeItemAtIndex:]: index ({index}) beyond bounds ({len})"),
    };
    let item = menu.ivars().items.borrow_mut().remove(at);
    let ivars = item_ivars(&item);
    let me = Some(NonNull::from(as_menu(menu)));
    ivars.menu.set(None);
    if let Some(sub) = &*ivars.submenu.borrow()
        && menu_ivars(sub).supermenu.get() == me
    {
        menu_ivars(sub).supermenu.set(None);
    }
    let was_highlighted = menu.ivars().highlighted.borrow().as_ref().is_some_and(|h| std::ptr::eq(&**h, &*item));
    if was_highlighted {
        menu.ivars().set_highlighted(None);
    }
    if let Some(owner) = menu.ivars().owner() {
        crate::popup_button::item_removed(&owner, &item);
    }
    bump(as_menu(menu));
    if post {
        post_index(note!(NSMenuDidRemoveItemNotification), as_menu(menu), at);
    }
    // Released last: releasing may run arbitrary code.
    drop(item);
}

/// Post `name` from `menu` with the item's index, if anyone listens.
fn post_index(name: &NSString, menu: &NSMenu, index: usize) {
    if !notification_center::has_observers(name) {
        return;
    }
    let number = NSNumber::new_isize(index as isize);
    let info = NSDictionary::from_slices(&[&*NSString::from_str("NSMenuItemIndex")], &[&*number as &AnyObject]);
    notification_center::post(name, Some(menu), Some(&info));
}

/// Post `name` from the item's menu with the item, if anyone listens.
fn post_item(name: &NSString, item: &NSMenuItem) {
    if !notification_center::has_observers(name) {
        return;
    }
    // SAFETY: menu returns a menu or nil.
    let menu = unsafe { item.menu() };
    let info = NSDictionary::from_slices(&[&*NSString::from_str("MenuItem")], &[item as &AnyObject]);
    notification_center::post(name, menu.as_deref().map(|m| m as &AnyObject), Some(&info));
}

/// The application, if the program made it; menus don't make one.
fn application() -> Option<Retained<NSApplication>> {
    crate::app::existing()
}

/// `-[NSMenu update]`: validate each item, when the menu enables its items
/// itself.
fn update(menu: &NSMenuImpl) {
    if !menu.ivars().autoenables.get() {
        return;
    }
    let app = application();
    // By index: a validator may change the menu.
    let mut i = 0;
    while let Some(item) = menu.ivars().item(i) {
        i += 1;
        if item_ivars(&item).submenu.borrow().is_some() {
            continue;
        }
        let enabled = validate(&item, app.as_deref());
        item.setEnabled(enabled);
    }
}

/// Whether `item` should be enabled: its action has a target, which
/// agrees.
pub(crate) fn validate(item: &NSMenuItem, app: Option<&NSApplication>) -> bool {
    let Some(action) = item_ivars(item).action.get() else { return false };
    let Some(app) = app else { return false };
    let target = item.target();
    // SAFETY: targetForAction:to:from: takes a selector, a target or nil
    // and the sender.
    let Some(target) = (unsafe { app.targetForAction_to_from(action, target.as_deref(), Some(item)) }) else {
        return false;
    };
    if responds(&target, sel!(validateMenuItem:)) {
        // SAFETY: validateMenuItem: takes the item and returns BOOL.
        unsafe { msg_send![&*target, validateMenuItem: item] }
    } else if responds(&target, sel!(validateUserInterfaceItem:)) {
        // SAFETY: validateUserInterfaceItem: takes the item and returns
        // BOOL.
        unsafe { msg_send![&*target, validateUserInterfaceItem: item] }
    } else {
        true
    }
}

/// `-[NSMenu performKeyEquivalent:]`.
fn perform_key_equivalent(menu: &NSMenuImpl, event: &NSEvent) -> bool {
    let Some(pressed) = Pressed::of(event) else { return false };
    // SAFETY: update takes nothing, by message as subclasses override it.
    let _: () = unsafe { msg_send![menu, update] };
    let mut i = 0;
    while let Some(item) = menu.ivars().item(i) {
        i += 1;
        let ivars = item_ivars(&item);
        let submenu = ivars.submenu.borrow().clone();
        if let Some(submenu) = submenu {
            if submenu.performKeyEquivalent(event) {
                return true;
            }
            continue;
        }
        let many = || {
            let theirs = event.charactersIgnoringModifiers();
            theirs.is_some_and(|t| ivars.key.borrow().isEqualToString(&t))
        };
        if keyequiv::matches(&ivars.shortcut.get(), &pressed, many) {
            if item.isEnabled() && hosts_enabled(as_menu(menu)) {
                send_action(&item);
            }
            return true;
        }
    }
    // F10 no item takes opens the main menu's bar from the keyboard.
    keyequiv::is_f10(&pressed) && is_main(as_menu(menu)) && crate::menubar::open_with_keyboard()
}

/// Whether `menu` is the application's main menu.
fn is_main(menu: &NSMenu) -> bool {
    application().and_then(|app| app.mainMenu()).is_some_and(|m| std::ptr::eq(&*m, menu))
}

/// The item in `menu`'s supermenu whose submenu `menu` is.
pub(crate) fn host_of(menu: &NSMenu) -> Option<Retained<NSMenuItem>> {
    // SAFETY: supermenu returns a menu or nil.
    let sup = unsafe { menu.supermenu() }?;
    let items = menu_ivars(&sup).items.borrow();
    items.iter().find(|i| item_ivars(i).submenu.borrow().as_deref().is_some_and(|s| std::ptr::eq(s, menu))).cloned()
}

/// Whether every item above `menu` (its host, its host's host…) is
/// enabled.
fn hosts_enabled(menu: &NSMenu) -> bool {
    let mut menu = menu.retain();
    while let Some(host) = host_of(&menu) {
        if !host.isEnabled() {
            return false;
        }
        // SAFETY: the host is in a menu (host_of found it there).
        let Some(up) = (unsafe { host.menu() }) else { break };
        menu = up;
    }
    true
}

/// Send the item's action through the application, from the item, between
/// the menu's will-send and did-send notifications.
pub(crate) fn send_action(item: &NSMenuItem) {
    let Some(action) = item.action() else { return };
    let mtm = MainThreadMarker::from(item);
    let app = NSApplication::sharedApplication(mtm);
    // A pop-up button's item becomes its selected item, whatever it does.
    // SAFETY: menu returns a menu or nil.
    if let Some(owner) = unsafe { item.menu() }.and_then(|m| menu_ivars(&m).owner()) {
        crate::popup_button::item_chosen(&owner, item);
    }
    post_item(note!(NSMenuWillSendActionNotification), item);
    let target = item.target();
    // SAFETY: sendAction:to:from: takes a selector, a target or nil and
    // the sender.
    unsafe { app.sendAction_to_from(action, target.as_deref(), Some(item)) };
    post_item(note!(NSMenuDidSendActionNotification), item);
}

/// Whether windows show the main menu as a bar (`+menuBarVisible`).
pub(crate) fn bar_visible() -> bool {
    BAR_VISIBLE.with(Cell::get)
}

/// Ask `menu`'s delegate `menuNeedsUpdate:`, during which the menu's
/// `propertiesToUpdate` may be asked.
pub(crate) fn needs_update(menu: &NSMenu, delegate: &AnyObject) {
    UPDATING.with(|u| u.set(u.get() + 1));
    // SAFETY: menuNeedsUpdate: takes the menu.
    let _: () = unsafe { msg_send![delegate, menuNeedsUpdate: menu] };
    UPDATING.with(|u| u.set(u.get() - 1));
}
