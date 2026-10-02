//! `NSPopUpButton`, `NSPopUpButtonCell` and `NSMenuItemCell`: a push
//! button showing one item of a menu, which pops up when clicked.
//!
//! The cell holds the menu and the selected item; the button forwards to
//! it by message, as AppKit's does, so subclasses of either see what they
//! override. As on macOS (`conformance/tests/popup_button.rs`):
//!
//! - In pop-up mode the button shows the selected item's title. The first
//!   item added is selected; removing the selected item selects the first;
//!   an unknown title or index -1 selects nothing. The selected item is on
//!   and every other off, unless `altersStateOfSelectedItem` is NO.
//!   `setTitle:` selects the item with that title, or adds one.
//! - In pull-down mode nothing is selected by itself, the title is the
//!   first item's (`setTitle:` renames it), and selecting changes neither
//!   the title nor any state.
//! - The cell watches its menu: items added without an action (by the
//!   button or straight to the menu) get the cell's `_popUpItemAction:`,
//!   which selects the item and sends the button's action from the
//!   button. Choosing any item of the menu, whatever its action, moves the
//!   selection to it first. Selecting in code sends nothing. The button's
//!   `autoenablesItems` is its menu's.
//!
//! A click pops the menu up over the button, through the menu tracking
//! loop, after `NSPopUpButtonWillPopUpNotification` (and the cell's): a
//! pop-up puts the selected item's title over the button's, the menu at
//! least as wide as the button; a pull-down opens below the button,
//! without its first item. Space and `performClick:` pop it up too.
//!
//! Sizes are macOS's: as tall as a push button of the control size, and as
//! wide as the widest item's title (the first item's for a pull-down) plus
//! 48 points (40, 32 and 56 for the small, mini and large sizes), the title
//! 12 points in (10, 8, 14). It draws as the theme's push bezel with a
//! chevron at its end, two (up and down) for a pop-up.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSActionCell, NSBezelStyle, NSButton, NSButtonCell, NSCell, NSControl, NSEvent, NSImage, NSMenu, NSMenuItem,
    NSMenuItemCell, NSPopUpArrowPosition, NSPopUpButton, NSPopUpButtonCell, NSResponder, NSView,
};
use objc2_foundation::{NSArray, NSNumber, NSPoint, NSRect, NSRectEdge, NSSize, NSString};

use crate::controls::cell::{self, Flags, Styled};
use crate::controls::control;
use crate::menu::{item_ivars, menu_ivars};
use crate::menu_tracking::{self, Opening};
use crate::theme::{self, metrics, parts};

sidestep_runtime::static_class!(pub NSMENUITEMCELL, NSMENUITEMCELL_META = "NSMenuItemCell", || {
    let _ = NSMenuItemCellImpl::class();
});

sidestep_runtime::static_class!(pub NSPOPUPBUTTONCELL, NSPOPUPBUTTONCELL_META = "NSPopUpButtonCell", || {
    let _ = NSPopUpButtonCellImpl::class();
});

sidestep_runtime::static_class!(pub NSPOPUPBUTTON, NSPOPUPBUTTON_META = "NSPopUpButton", || {
    control::register_cell_class(NSPopUpButtonImpl::class(), NSPopUpButtonCell::class());
});

sidestep_foundation::constant_string!(NSPopUpButtonWillPopUpNotification = "NSPopUpButtonWillPopUpNotification");
sidestep_foundation::constant_string!(
    NSPopUpButtonCellWillPopUpNotification = "NSPopUpButtonCellWillPopUpNotification"
);

/// Width beyond the title, by control size (regular, small, mini, large).
const EXTRA_WIDTH: [f64; 4] = [48.0, 40.0, 32.0, 56.0];
/// Where the title starts.
const TITLE_X: [f64; 4] = [12.0, 10.0, 8.0, 14.0];
/// The room the chevrons take at the button's end.
const ARROW_WIDTH: f64 = 26.0;

// NSMenuItemCell

pub(crate) struct ItemCellIvars {
    item: RefCell<Option<Retained<NSMenuItem>>>,
    needs_sizing: Cell<bool>,
    needs_display: Cell<bool>,
}

define_class!(
    // The cell AppKit once drew menu items with; kept for its API and as
    // the pop-up cell's superclass.
    #[unsafe(super(NSButtonCell, NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSMenuItemCell"]
    #[ivars = ItemCellIvars]
    pub(crate) struct NSMenuItemCellImpl;

    impl NSMenuItemCellImpl {
        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, title: &NSString) -> Retained<Self> {
            let this = this.set_ivars(ItemCellIvars {
                item: RefCell::new(None),
                needs_sizing: Cell::new(true),
                needs_display: Cell::new(false),
            });
            // SAFETY: NSButtonCell's initializer.
            unsafe { msg_send![super(this), initTextCell: title] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method_id(menuItem))]
        fn menu_item(&self) -> Option<Retained<NSMenuItem>> {
            self.ivars().item.borrow().clone()
        }

        #[unsafe(method(setMenuItem:))]
        fn set_menu_item(&self, item: Option<&NSMenuItem>) {
            let old = self.ivars().item.replace(item.map(|i| i.retain()));
            drop(old);
        }

        #[unsafe(method(needsSizing))]
        fn needs_sizing(&self) -> bool {
            self.ivars().needs_sizing.get()
        }

        #[unsafe(method(setNeedsSizing:))]
        fn set_needs_sizing(&self, flag: bool) {
            self.ivars().needs_sizing.set(flag);
        }

        #[unsafe(method(calcSize))]
        fn calc_size(&self) {
            self.ivars().needs_sizing.set(false);
        }

        #[unsafe(method(needsDisplay))]
        fn needs_display(&self) -> bool {
            self.ivars().needs_display.get()
        }

        #[unsafe(method(setNeedsDisplay:))]
        fn set_needs_display(&self, flag: bool) {
            self.ivars().needs_display.set(flag);
        }

        #[unsafe(method(stateImageWidth))]
        fn state_image_width(&self) -> f64 {
            0.0
        }

        #[unsafe(method(imageWidth))]
        fn image_width(&self) -> f64 {
            0.0
        }

        #[unsafe(method(titleWidth))]
        fn title_width(&self) -> f64 {
            let title = self.ivars().item.borrow().as_ref().map(|i| i.title());
            title.map_or(0.0, |t| text_size(cell::imp(as_cell(self)), &t).width)
        }

        #[unsafe(method(keyEquivalentWidth))]
        fn key_equivalent_width(&self) -> f64 {
            0.0
        }

        #[unsafe(method(stateImageRectForBounds:))]
        fn state_image_rect_for_bounds(&self, _bounds: NSRect) -> NSRect {
            NSRect::ZERO
        }

        #[unsafe(method(keyEquivalentRectForBounds:))]
        fn key_equivalent_rect_for_bounds(&self, _bounds: NSRect) -> NSRect {
            NSRect::ZERO
        }

        #[unsafe(method(drawSeparatorItemWithFrame:inView:))]
        fn draw_separator_item(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(drawStateImageWithFrame:inView:))]
        fn draw_state_image(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(drawImageWithFrame:inView:))]
        fn draw_image_with_frame(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(drawTitleWithFrame:inView:))]
        fn draw_title_with_frame(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(drawKeyEquivalentWithFrame:inView:))]
        fn draw_key_equivalent(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(drawBorderAndBackgroundWithFrame:inView:))]
        fn draw_border_and_background(&self, _frame: NSRect, _view: &NSView) {}
    }

    unsafe impl NSObjectProtocol for NSMenuItemCellImpl {}
);

fn as_cell<T>(this: &T) -> &NSCell {
    // SAFETY: the item and pop-up cells are cells.
    unsafe { &*(this as *const T).cast::<NSCell>() }
}

// NSPopUpButtonCell

pub(crate) struct PopUpIvars {
    menu: RefCell<Retained<NSMenu>>,
    selected: RefCell<Option<Retained<NSMenuItem>>>,
    pulls_down: Cell<bool>,
    edge: Cell<NSRectEdge>,
    uses_item: Cell<bool>,
    alters_state: Cell<bool>,
    arrow: Cell<NSPopUpArrowPosition>,
    /// The widest title's width, and the menu generation it was measured
    /// at.
    widest: Cell<Option<(u64, f64)>>,
}

define_class!(
    #[unsafe(super(NSMenuItemCell, NSButtonCell, NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSPopUpButtonCell"]
    #[ivars = PopUpIvars]
    pub(crate) struct NSPopUpButtonCellImpl;

    impl NSPopUpButtonCellImpl {
        /// A cell with a title has an item of that title.
        #[unsafe(method_id(initTextCell:pullsDown:))]
        fn init_text_cell_pulls_down(this: Allocated<Self>, title: &NSString, pulls_down: bool) -> Retained<Self> {
            let mtm = MainThreadMarker::new().expect("sidestep: AppKit's controls belong to the main thread");
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::new());
            let this = this.set_ivars(PopUpIvars {
                menu: RefCell::new(menu),
                selected: RefCell::new(None),
                pulls_down: Cell::new(pulls_down),
                edge: Cell::new(NSRectEdge::MinY),
                uses_item: Cell::new(true),
                alters_state: Cell::new(true),
                arrow: Cell::new(NSPopUpArrowPosition::ArrowAtBottom),
                widest: Cell::new(None),
            });
            // SAFETY: NSMenuItemCell's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initTextCell: &*NSString::new()] };
            // SAFETY: a pop-up cell is a button cell.
            let button: &NSButtonCell = unsafe { &*(Retained::as_ptr(&this).cast::<NSButtonCell>()) };
            button.setBezelStyle(NSBezelStyle::Push);
            let menu = this.ivars().menu.borrow().clone();
            menu_ivars(&menu).set_owner(Some(&*this));
            if title.length() > 0 {
                // SAFETY: addItemWithTitle: takes a title.
                let _: () = unsafe { msg_send![&*this, addItemWithTitle: title] };
            }
            this
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, title: &NSString) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: title, pullsDown: false] }
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, image: Option<&NSImage>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            let this: Retained<Self> = unsafe { msg_send![this, initTextCell: &*NSString::new(), pullsDown: false] };
            // SAFETY: setImage: takes an image or nil.
            let _: () = unsafe { msg_send![&*this, setImage: image] };
            this
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new(), pullsDown: false] }
        }

        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            Some(self.ivars().menu.borrow().clone())
        }

        /// The first item of a new menu is selected (in pop-up mode), and
        /// its items without an action act through the button.
        #[unsafe(method(setMenu:))]
        fn set_menu(&self, menu: Option<&NSMenu>) {
            let mtm = MainThreadMarker::from(self);
            let menu = menu.map_or_else(|| NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::new()), |m| m.retain());
            let old = self.ivars().menu.replace(menu.clone());
            menu_ivars(&old).set_owner::<AnyObject>(None);
            menu_ivars(&menu).set_owner(Some(self));
            for i in 0..menu_ivars(&menu).len() {
                if let Some(item) = menu_ivars(&menu).item(i) {
                    adopt(self, &item);
                }
            }
            let _ = self.ivars().selected.take();
            if !self.ivars().pulls_down.get() {
                let first = menu_ivars(&menu).item(0);
                select(self, first.as_deref());
            }
            changed(self);
            drop(old);
        }

        #[unsafe(method(pullsDown))]
        fn pulls_down(&self) -> bool {
            self.ivars().pulls_down.get()
        }

        #[unsafe(method(setPullsDown:))]
        fn set_pulls_down(&self, flag: bool) {
            if self.ivars().pulls_down.replace(flag) != flag {
                changed(self);
            }
        }

        #[unsafe(method(autoenablesItems))]
        fn autoenables_items(&self) -> bool {
            self.ivars().menu.borrow().autoenablesItems()
        }

        #[unsafe(method(setAutoenablesItems:))]
        fn set_autoenables_items(&self, flag: bool) {
            self.ivars().menu.borrow().setAutoenablesItems(flag);
        }

        #[unsafe(method(preferredEdge))]
        fn preferred_edge(&self) -> NSRectEdge {
            self.ivars().edge.get()
        }

        #[unsafe(method(setPreferredEdge:))]
        fn set_preferred_edge(&self, edge: NSRectEdge) {
            self.ivars().edge.set(edge);
        }

        #[unsafe(method(usesItemFromMenu))]
        fn uses_item_from_menu(&self) -> bool {
            self.ivars().uses_item.get()
        }

        #[unsafe(method(setUsesItemFromMenu:))]
        fn set_uses_item_from_menu(&self, flag: bool) {
            self.ivars().uses_item.set(flag);
            changed(self);
        }

        #[unsafe(method(altersStateOfSelectedItem))]
        fn alters_state_of_selected_item(&self) -> bool {
            self.ivars().alters_state.get()
        }

        #[unsafe(method(setAltersStateOfSelectedItem:))]
        fn set_alters_state_of_selected_item(&self, flag: bool) {
            self.ivars().alters_state.set(flag);
        }

        #[unsafe(method(arrowPosition))]
        fn arrow_position(&self) -> NSPopUpArrowPosition {
            self.ivars().arrow.get()
        }

        #[unsafe(method(setArrowPosition:))]
        fn set_arrow_position(&self, position: NSPopUpArrowPosition) {
            self.ivars().arrow.set(position);
            cell::redraw(cell::imp(as_cell(self)));
        }

        /// A title already there moves to the end.
        #[unsafe(method(addItemWithTitle:))]
        fn add_item_with_title(&self, title: &NSString) {
            let count = self.menu_now().numberOfItems();
            insert_titled(self, title, count, true);
        }

        /// As `addItemWithTitle:` for each, with the titles looked up in
        /// a table made once rather than by a search per title.
        #[unsafe(method(addItemsWithTitles:))]
        fn add_items_with_titles(&self, titles: &NSArray<NSString>) {
            let menu = self.menu_now();
            // The items with each title, in the menu's order.
            let mut by_title: HashMap<String, VecDeque<Retained<NSMenuItem>>> = HashMap::new();
            for i in 0..menu_ivars(&menu).len() {
                if let Some(item) = menu_ivars(&menu).item(i) {
                    by_title.entry(item.title().to_string()).or_default().push_back(item);
                }
            }
            let mtm = MainThreadMarker::from(self);
            for title in titles.iter() {
                let same = by_title.entry(title.to_string()).or_default();
                if let Some(old) = same.pop_front() {
                    menu.removeItem(&old);
                }
                // SAFETY: the designated initializer.
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &title, None, &NSString::new())
                };
                same.push_back(item.clone());
                menu.addItem(&item);
            }
        }

        #[unsafe(method(insertItemWithTitle:atIndex:))]
        fn insert_item_with_title(&self, title: &NSString, index: isize) {
            insert_titled(self, title, index, false);
        }

        #[unsafe(method(removeItemWithTitle:))]
        fn remove_item_with_title(&self, title: &NSString) {
            let menu = self.menu_now();
            let index = menu.indexOfItemWithTitle(title);
            if index >= 0 {
                menu.removeItemAtIndex(index);
            }
        }

        #[unsafe(method(removeItemAtIndex:))]
        fn remove_item_at_index(&self, index: isize) {
            self.menu_now().removeItemAtIndex(index);
        }

        #[unsafe(method(removeAllItems))]
        fn remove_all_items(&self) {
            self.menu_now().removeAllItems();
        }

        #[unsafe(method_id(itemArray))]
        fn item_array(&self) -> Retained<NSArray<NSMenuItem>> {
            self.menu_now().itemArray()
        }

        #[unsafe(method(numberOfItems))]
        fn number_of_items(&self) -> isize {
            self.menu_now().numberOfItems()
        }

        #[unsafe(method(indexOfItem:))]
        fn index_of_item(&self, item: &NSMenuItem) -> isize {
            self.menu_now().indexOfItem(item)
        }

        #[unsafe(method(indexOfItemWithTitle:))]
        fn index_of_item_with_title(&self, title: &NSString) -> isize {
            self.menu_now().indexOfItemWithTitle(title)
        }

        #[unsafe(method(indexOfItemWithTag:))]
        fn index_of_item_with_tag(&self, tag: isize) -> isize {
            self.menu_now().indexOfItemWithTag(tag)
        }

        #[unsafe(method(indexOfItemWithRepresentedObject:))]
        fn index_of_item_with_represented_object(&self, object: Option<&AnyObject>) -> isize {
            // SAFETY: the menu's method takes an object or nil.
            unsafe { self.menu_now().indexOfItemWithRepresentedObject(object) }
        }

        #[unsafe(method(indexOfItemWithTarget:andAction:))]
        fn index_of_item_with_target_and_action(&self, target: Option<&AnyObject>, action: Option<Sel>) -> isize {
            // SAFETY: the menu's method takes a target and an action.
            unsafe { self.menu_now().indexOfItemWithTarget_andAction(target, action) }
        }

        #[unsafe(method_id(itemAtIndex:))]
        fn item_at_index(&self, index: isize) -> Option<Retained<NSMenuItem>> {
            let menu = self.menu_now();
            usize::try_from(index).ok().and_then(|i| menu_ivars(&menu).item(i))
        }

        #[unsafe(method_id(itemWithTitle:))]
        fn item_with_title(&self, title: &NSString) -> Option<Retained<NSMenuItem>> {
            self.menu_now().itemWithTitle(title)
        }

        #[unsafe(method_id(lastItem))]
        fn last_item(&self) -> Option<Retained<NSMenuItem>> {
            let menu = self.menu_now();
            let ivars = menu_ivars(&menu);
            ivars.len().checked_sub(1).and_then(|i| ivars.item(i))
        }

        #[unsafe(method(selectItem:))]
        fn select_item(&self, item: Option<&NSMenuItem>) {
            let item = item.filter(|i| self.menu_now().indexOfItem(i) >= 0);
            select(self, item);
        }

        /// -1 (or any index out of range) selects nothing.
        #[unsafe(method(selectItemAtIndex:))]
        fn select_item_at_index(&self, index: isize) {
            select_index(self, index);
        }

        #[unsafe(method(selectItemWithTitle:))]
        fn select_item_with_title(&self, title: &NSString) {
            let item = self.menu_now().itemWithTitle(title);
            select(self, item.as_deref());
        }

        /// Selects nothing new when no item has the tag.
        #[unsafe(method(selectItemWithTag:))]
        fn select_item_with_tag(&self, tag: isize) -> bool {
            let item = self.menu_now().itemWithTag(tag);
            let found = item.is_some();
            if found {
                select(self, item.as_deref());
            }
            found
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            shown_title(self).unwrap_or_default()
        }

        /// A pop-up selects the item with the title, or adds one; a
        /// pull-down renames its first item.
        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: Option<&NSString>) {
            let title = title.map_or_else(NSString::new, |t| t.retain());
            let menu = self.menu_now();
            if self.ivars().pulls_down.get() {
                match menu_ivars(&menu).item(0) {
                    Some(first) => first.setTitle(&title),
                    None => insert_titled(self, &title, 0, false),
                }
                changed(self);
                return;
            }
            match menu.itemWithTitle(&title) {
                Some(item) => select(self, Some(&item)),
                None => {
                    let count = menu.numberOfItems();
                    insert_titled(self, &title, count, false);
                    let item = menu.itemWithTitle(&title);
                    select(self, item.as_deref());
                }
            }
        }

        #[unsafe(method_id(selectedItem))]
        fn selected_item(&self) -> Option<Retained<NSMenuItem>> {
            self.ivars().selected.borrow().clone()
        }

        #[unsafe(method(indexOfSelectedItem))]
        fn index_of_selected_item(&self) -> isize {
            selected_index(self)
        }

        #[unsafe(method(selectedTag))]
        fn selected_tag(&self) -> isize {
            self.ivars().selected.borrow().as_ref().map_or(-1, |i| i.tag())
        }

        /// The item shown: the selected one (the first, for a pull-down).
        #[unsafe(method_id(menuItem))]
        fn menu_item(&self) -> Option<Retained<NSMenuItem>> {
            shown_item(self)
        }

        #[unsafe(method(synchronizeTitleAndSelectedItem))]
        fn synchronize_title_and_selected_item(&self) {
            changed(self);
        }

        #[unsafe(method_id(itemTitleAtIndex:))]
        fn item_title_at_index(&self, index: isize) -> Retained<NSString> {
            let item = usize::try_from(index).ok().and_then(|i| menu_ivars(&self.menu_now()).item(i));
            item.map_or_else(NSString::new, |i| i.title())
        }

        #[unsafe(method_id(itemTitles))]
        fn item_titles(&self) -> Retained<NSArray<NSString>> {
            let menu = self.menu_now();
            let ivars = menu_ivars(&menu);
            let titles: Vec<Retained<NSString>> = (0..ivars.len()).filter_map(|i| ivars.item(i)).map(|i| i.title()).collect();
            NSArray::from_retained_slice(&titles)
        }

        #[unsafe(method_id(titleOfSelectedItem))]
        fn title_of_selected_item(&self) -> Option<Retained<NSString>> {
            self.ivars().selected.borrow().as_ref().map(|i| i.title())
        }

        // The value is the selected item's index.

        #[unsafe(method_id(objectValue))]
        fn object_value(&self) -> Option<Retained<AnyObject>> {
            Some(Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_isize(selected_index(self))))))
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, value: Option<&AnyObject>) {
            let index = value.and_then(crate::font::number).map_or(-1, |n| n as isize);
            select_index(self, index);
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> isize {
            selected_index(self)
        }

        #[unsafe(method(setIntegerValue:))]
        fn set_integer_value(&self, value: isize) {
            select_index(self, value);
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            selected_index(self) as i32
        }

        #[unsafe(method(setIntValue:))]
        fn set_int_value(&self, value: i32) {
            select_index(self, value as isize);
        }

        /// An item of the button's chosen: select it and send the button's
        /// action from the button.
        #[unsafe(method(_popUpItemAction:))]
        fn pop_up_item_action(&self, sender: Option<&AnyObject>) {
            if let Some(item) = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()) {
                select(self, Some(item));
            }
            let c = as_cell(self);
            if let Some(view) = cell::imp(c).view() {
                crate::controls::track::send_cell_action(c, &view);
            }
        }

        // Popping up.

        #[unsafe(method(attachPopUpWithFrame:inView:))]
        fn attach_pop_up_with_frame(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(dismissPopUp))]
        fn dismiss_pop_up(&self) {
            self.menu_now().cancelTracking();
        }

        #[unsafe(method(performClickWithFrame:inView:))]
        fn perform_click_with_frame(&self, frame: NSRect, view: &NSView) {
            pop_up(self, frame, view, false);
        }

        #[unsafe(method(trackMouse:inRect:ofView:untilMouseUp:))]
        fn track_mouse(&self, _event: &NSEvent, frame: NSRect, view: &NSView, _until_up: bool) -> bool {
            pop_up(self, frame, view, true);
            true
        }

        #[unsafe(method(performClick:))]
        fn perform_click(&self, _sender: Option<&AnyObject>) {
            if let Some(view) = cell::imp(as_cell(self)).view() {
                pop_up(self, view.bounds(), &view, false);
            }
        }

        // Size and drawing.

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            let i = cell::imp(as_cell(self)).control_size_index();
            let title = if self.ivars().pulls_down.get() {
                shown_title(self).map_or(0.0, |t| text_size(cell::imp(as_cell(self)), &t).width)
            } else {
                widest(self)
            };
            NSSize::new(title + EXTRA_WIDTH[i], metrics::PUSH_HEIGHT[i])
        }

        #[unsafe(method(titleRectForBounds:))]
        fn title_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            title_rect(self, bounds)
        }

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, view: &NSView) {
            draw(self, frame, view);
        }
    }

    unsafe impl NSObjectProtocol for NSPopUpButtonCellImpl {}
);

impl NSPopUpButtonCellImpl {
    fn menu_now(&self) -> Retained<NSMenu> {
        self.ivars().menu.borrow().clone()
    }
}

fn imp(cell: &AnyObject) -> Option<&NSPopUpButtonCellImpl> {
    // SAFETY: NSPopUpButtonCellImpl is the class NSPopUpButtonCell names.
    unsafe { crate::controls::impl_of::<NSPopUpButtonCell, NSPopUpButtonCellImpl>(cell) }
}

/// The cell's size or look changed: tell its button.
fn changed(cell: &NSPopUpButtonCellImpl) {
    cell::changed(cell::imp(as_cell(cell)));
}

fn selected_index(cell: &NSPopUpButtonCellImpl) -> isize {
    let selected = cell.ivars().selected.borrow().clone();
    selected.map_or(-1, |i| cell.menu_now().indexOfItem(&i))
}

/// The item the button shows: the selected one, the first for a
/// pull-down.
fn shown_item(cell: &NSPopUpButtonCellImpl) -> Option<Retained<NSMenuItem>> {
    if cell.ivars().pulls_down.get() {
        menu_ivars(&cell.menu_now()).item(0)
    } else {
        cell.ivars().selected.borrow().clone()
    }
}

fn shown_title(cell: &NSPopUpButtonCellImpl) -> Option<Retained<NSString>> {
    shown_item(cell).map(|i| i.title())
}

/// Select `item` (or none): in pop-up mode it alone is on, unless the
/// cell leaves states alone.
fn select(cell: &NSPopUpButtonCellImpl, item: Option<&NSMenuItem>) {
    let ivars = cell.ivars();
    let old = ivars.selected.replace(item.map(|i| i.retain()));
    if !ivars.pulls_down.get() && ivars.alters_state.get() {
        if let Some(old) = &old
            && item.is_none_or(|i| !std::ptr::eq(&**old, i))
        {
            old.setState(0);
        }
        if let Some(item) = item {
            item.setState(1);
        }
    }
    // SAFETY: setMenuItem: takes an item or nil.
    let _: () = unsafe { msg_send![cell, setMenuItem: item] };
    changed(cell);
    drop(old);
}

/// Select the item at `index`, or none out of range.
fn select_index(cell: &NSPopUpButtonCellImpl, index: isize) {
    let item = usize::try_from(index).ok().and_then(|i| menu_ivars(&cell.menu_now()).item(i));
    select(cell, item.as_deref());
}

/// Give an item without an action the cell's, and the cell as target.
fn adopt(cell: &NSPopUpButtonCellImpl, item: &NSMenuItem) {
    if item_ivars(item).action().is_none() {
        // SAFETY: the item keeps its target weakly; the action is the
        // cell's.
        unsafe {
            item.setAction(Some(sel!(_popUpItemAction:)));
            item.setTarget(Some(cell));
        }
    }
}

/// Add an item titled `title` at `index`, after taking away one with the
/// same title when `moving` (as `addItemWithTitle:` does).
fn insert_titled(cell: &NSPopUpButtonCellImpl, title: &NSString, index: isize, moving: bool) {
    let menu = cell.menu_now();
    let mut index = index;
    if moving {
        let there = menu.indexOfItemWithTitle(title);
        if there >= 0 {
            menu.removeItemAtIndex(there);
            index -= 1;
        }
    }
    let mtm = MainThreadMarker::from(cell);
    // SAFETY: the designated initializer.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), title, None, &NSString::new())
    };
    menu.insertItem_atIndex(&item, index.clamp(0, menu.numberOfItems()));
}

// What the cell's menu tells it (see `menu::MenuIvars::set_owner`).

/// An item joined the menu.
pub(crate) fn item_added(owner: &AnyObject, item: &NSMenuItem) {
    let Some(cell) = imp(owner) else { return };
    adopt(cell, item);
    let first = menu_ivars(&cell.menu_now()).len() == 1;
    if first && !cell.ivars().pulls_down.get() && cell.ivars().selected.borrow().is_none() {
        select(cell, Some(item));
    }
}

/// An item left the menu: if it was the selected one, a pop-up selects its
/// first item.
pub(crate) fn item_removed(owner: &AnyObject, item: &NSMenuItem) {
    let Some(cell) = imp(owner) else { return };
    let was = cell.ivars().selected.borrow().as_ref().is_some_and(|s| std::ptr::eq(&**s, item));
    if !was {
        return;
    }
    let _ = cell.ivars().selected.take();
    let first = if cell.ivars().pulls_down.get() { None } else { menu_ivars(&cell.menu_now()).item(0) };
    select(cell, first.as_deref());
}

/// An item of the menu is about to send its action: it becomes the
/// selected item.
pub(crate) fn item_chosen(owner: &AnyObject, item: &NSMenuItem) {
    if let Some(cell) = imp(owner) {
        select(cell, Some(item));
    }
}

/// The menu changed in a way that may change the button's size or look.
pub(crate) fn menu_changed(owner: &AnyObject) {
    if let Some(cell) = imp(owner) {
        changed(cell);
    }
}

// Sizes and drawing.

/// `text`'s size in the cell's font.
fn text_size(base: &cell::NSCellImpl, text: &NSString) -> NSSize {
    let font = cell::font_of(base);
    let attrs = cell::attrs(base, &font, [0.0; 4]);
    let size = Styled::plain(text.to_string(), attrs).size(None);
    NSSize::new(size.width.ceil(), size.height.ceil())
}

/// The widest item title, measured once per change to the menu.
fn widest(cell: &NSPopUpButtonCellImpl) -> f64 {
    let menu = cell.menu_now();
    let generation = menu_ivars(&menu).generation();
    if let Some((at, width)) = cell.ivars().widest.get()
        && at == generation
    {
        return width;
    }
    let base = cell::imp(as_cell(cell));
    let ivars = menu_ivars(&menu);
    let width = (0..ivars.len())
        .filter_map(|i| ivars.item(i))
        .filter(|i| !item_ivars(i).is_separator())
        .map(|i| text_size(base, &i.title()).width)
        .fold(0.0, f64::max);
    cell.ivars().widest.set(Some((generation, width)));
    width
}

fn title_rect(cell: &NSPopUpButtonCellImpl, bounds: NSRect) -> NSRect {
    let Some(title) = shown_title(cell).filter(|t| t.length() > 0) else { return NSRect::ZERO };
    let base = cell::imp(as_cell(cell));
    let i = base.control_size_index();
    let size = text_size(base, &title);
    let y = bounds.origin.y + ((bounds.size.height - size.height) / 2.0 + 0.5).floor();
    NSRect::new(NSPoint::new(bounds.origin.x + TITLE_X[i], y), size)
}

fn draw(cell: &NSPopUpButtonCellImpl, frame: NSRect, view: &NSView) {
    if cell.isTransparent() || !theme::paint::recording() {
        return;
    }
    let base = cell::imp(as_cell(cell));
    let p = theme::palette();
    let s = parts::State {
        disabled: !base.has(Flags::ENABLED),
        pressed: base.has(Flags::HIGHLIGHTED),
        ..parts::State::default()
    };
    let axis = parts::Axis { flipped: view.isFlipped() };
    let i = base.control_size_index();
    let height = metrics::PUSH_HEIGHT[i].min(frame.size.height);
    let bezel = NSRect::new(
        NSPoint::new(frame.origin.x, frame.origin.y + ((frame.size.height - height) / 2.0).floor()),
        NSSize::new(frame.size.width, height),
    );
    let ink = parts::button_text(p, parts::Emphasis::Normal, s);
    if cell.ivars().pulls_down.get() || cell.ivars().arrow.get() == NSPopUpArrowPosition::NoArrow {
        if cell.ivars().arrow.get() == NSPopUpArrowPosition::NoArrow {
            parts::button_bezel(p, bezel, theme::paint::radii(parts::RADIUS), parts::Emphasis::Normal, s);
        } else {
            parts::pop_up(p, bezel, axis, ARROW_WIDTH, s);
        }
    } else {
        parts::button_bezel(p, bezel, theme::paint::radii(parts::RADIUS), parts::Emphasis::Normal, s);
        let arrows = NSRect::new(
            NSPoint::new(bezel.origin.x + bezel.size.width - ARROW_WIDTH, bezel.origin.y),
            NSSize::new(ARROW_WIDTH, bezel.size.height),
        );
        let up = NSRect::new(arrows.origin, NSSize::new(ARROW_WIDTH, bezel.size.height / 2.0 + 2.0));
        let down = NSRect::new(
            NSPoint::new(arrows.origin.x, arrows.origin.y + bezel.size.height / 2.0 - 2.0),
            NSSize::new(ARROW_WIDTH, bezel.size.height / 2.0 + 2.0),
        );
        let (first, second) = if axis.flipped { (up, down) } else { (down, up) };
        parts::chevron(first, axis, 7.0, true, ink);
        parts::chevron(second, axis, 7.0, false, ink);
    }
    let Some(title) = shown_title(cell).filter(|t| t.length() > 0) else { return };
    let r = title_rect(cell, frame);
    let room = frame.origin.x + frame.size.width - ARROW_WIDTH - r.origin.x;
    let font = cell::font_of(base);
    let mut attrs = cell::attrs(base, &font, ink);
    // The title starts where the menu's does, whatever the cell's
    // alignment, and is cut short before the chevrons.
    attrs.paragraph.alignment = crate::text::layout::Align::Natural;
    attrs.paragraph.line_break = crate::text::layout::LineBreak::TruncateTail;
    let r = NSRect::new(r.origin, NSSize::new(room.max(0.0), r.size.height));
    Styled::plain(title.to_string(), attrs).draw(r);
}

/// Pop the menu up over the button (`frame` in `view`), and follow it until
/// it closes.
fn pop_up(cell: &NSPopUpButtonCellImpl, frame: NSRect, view: &NSView, pressed: bool) {
    let base = cell::imp(as_cell(cell));
    if !base.has(Flags::ENABLED) {
        return;
    }
    let Some(window) = view.window() else { return };
    crate::notifications::post(crate::menu::note!(NSPopUpButtonWillPopUpNotification), view);
    crate::notifications::post(crate::menu::note!(NSPopUpButtonCellWillPopUpNotification), cell);
    let menu = cell.menu_now();
    let pulls_down = cell.ivars().pulls_down.get();
    let i = base.control_size_index();
    let mut opening = Opening {
        parent: window,
        anchor: NSRect::ZERO,
        layout: menu_tracking::at_point(),
        positioning: None,
        min_width: frame.size.width,
        title_at: None,
        pressed,
        view: None,
        event: None,
    };
    // The first item of a pull-down is its title, not a choice.
    let first = if pulls_down { menu_ivars(&menu).item(0) } else { None };
    if pulls_down {
        opening.anchor = view.convertRect_toView(frame, None);
        opening.layout = menu_tracking::below();
        if let Some(first) = &first {
            crate::menu::hide_quietly(first, true);
        }
    } else {
        // The selected item's title over the button's, the menu over the
        // whole button: the tracking loop places it once the menu is up to
        // date (its delegate may fill it in).
        let item = cell.ivars().selected.borrow().clone().or_else(|| menu_ivars(&menu).item(0));
        let middle = NSPoint::new(frame.origin.x, frame.origin.y + frame.size.height / 2.0);
        opening.anchor = menu_tracking::point(view.convertPoint_toView(middle, None));
        opening.positioning = item;
        opening.title_at = Some(TITLE_X[i]);
    }
    menu_tracking::track(&menu, opening);
    if let Some(first) = &first {
        crate::menu::hide_quietly(first, false);
    }
}

// NSPopUpButton

define_class!(
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSPopUpButton"]
    pub(crate) struct NSPopUpButtonImpl;

    impl NSPopUpButtonImpl {
        #[unsafe(method_id(initWithFrame:pullsDown:))]
        fn init_with_frame_pulls_down(this: Allocated<Self>, frame: NSRect, pulls_down: bool) -> Retained<Self> {
            let this = this.set_ivars(());
            // SAFETY: NSControl's designated initializer, which makes the
            // cell from +cellClass.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            this.with_cell(|c| c.setPullsDown(pulls_down));
            this
        }

        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initWithFrame: frame, pullsDown: false] }
        }

        #[unsafe(method_id(popUpButtonWithMenu:target:action:))]
        fn pop_up_button_with_menu(menu: &NSMenu, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSPopUpButton> {
            let b = made(false, menu);
            // SAFETY: the button keeps the target weakly.
            unsafe {
                b.setTarget(target);
                b.setAction(action);
            }
            b
        }

        #[unsafe(method_id(pullDownButtonWithTitle:menu:))]
        fn pull_down_button_with_title(title: &NSString, menu: &NSMenu) -> Retained<NSPopUpButton> {
            let b = made(true, menu);
            b.setTitle(title);
            b.sizeToFit();
            b
        }

        #[unsafe(method_id(pullDownButtonWithImage:menu:))]
        fn pull_down_button_with_image(image: &NSImage, menu: &NSMenu) -> Retained<NSPopUpButton> {
            let b = made(true, menu);
            // SAFETY: setImage: takes an image.
            let _: () = unsafe { msg_send![&*b, setImage: image] };
            b
        }

        #[unsafe(method_id(pullDownButtonWithTitle:image:menu:))]
        fn pull_down_button_with_title_image(title: &NSString, image: &NSImage, menu: &NSMenu) -> Retained<NSPopUpButton> {
            let b = made(true, menu);
            b.setTitle(title);
            // SAFETY: setImage: takes an image.
            let _: () = unsafe { msg_send![&*b, setImage: image] };
            b.sizeToFit();
            b
        }

        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            self.popup_cell().and_then(|c| c.menu())
        }

        #[unsafe(method(setMenu:))]
        fn set_menu(&self, menu: Option<&NSMenu>) {
            self.with_cell(|c| c.setMenu(menu));
        }

        #[unsafe(method(pullsDown))]
        fn pulls_down(&self) -> bool {
            self.popup_cell().is_some_and(|c| c.pullsDown())
        }

        #[unsafe(method(setPullsDown:))]
        fn set_pulls_down(&self, flag: bool) {
            self.with_cell(|c| c.setPullsDown(flag));
        }

        #[unsafe(method(autoenablesItems))]
        fn autoenables_items(&self) -> bool {
            self.popup_cell().is_some_and(|c| c.autoenablesItems())
        }

        #[unsafe(method(setAutoenablesItems:))]
        fn set_autoenables_items(&self, flag: bool) {
            self.with_cell(|c| c.setAutoenablesItems(flag));
        }

        #[unsafe(method(preferredEdge))]
        fn preferred_edge(&self) -> NSRectEdge {
            self.popup_cell().map_or(NSRectEdge::MinY, |c| c.preferredEdge())
        }

        #[unsafe(method(setPreferredEdge:))]
        fn set_preferred_edge(&self, edge: NSRectEdge) {
            self.with_cell(|c| c.setPreferredEdge(edge));
        }

        #[unsafe(method(usesItemFromMenu))]
        fn uses_item_from_menu(&self) -> bool {
            self.popup_cell().is_some_and(|c| c.usesItemFromMenu())
        }

        #[unsafe(method(setUsesItemFromMenu:))]
        fn set_uses_item_from_menu(&self, flag: bool) {
            self.with_cell(|c| c.setUsesItemFromMenu(flag));
        }

        #[unsafe(method(altersStateOfSelectedItem))]
        fn alters_state_of_selected_item(&self) -> bool {
            self.popup_cell().is_some_and(|c| c.altersStateOfSelectedItem())
        }

        #[unsafe(method(setAltersStateOfSelectedItem:))]
        fn set_alters_state_of_selected_item(&self, flag: bool) {
            self.with_cell(|c| c.setAltersStateOfSelectedItem(flag));
        }

        #[unsafe(method(addItemWithTitle:))]
        fn add_item_with_title(&self, title: &NSString) {
            self.with_cell(|c| c.addItemWithTitle(title));
        }

        #[unsafe(method(addItemsWithTitles:))]
        fn add_items_with_titles(&self, titles: &NSArray<NSString>) {
            self.with_cell(|c| c.addItemsWithTitles(titles));
        }

        #[unsafe(method(insertItemWithTitle:atIndex:))]
        fn insert_item_with_title(&self, title: &NSString, index: isize) {
            self.with_cell(|c| c.insertItemWithTitle_atIndex(title, index));
        }

        #[unsafe(method(removeItemWithTitle:))]
        fn remove_item_with_title(&self, title: &NSString) {
            self.with_cell(|c| c.removeItemWithTitle(title));
        }

        #[unsafe(method(removeItemAtIndex:))]
        fn remove_item_at_index(&self, index: isize) {
            self.with_cell(|c| c.removeItemAtIndex(index));
        }

        #[unsafe(method(removeAllItems))]
        fn remove_all_items(&self) {
            self.with_cell(|c| c.removeAllItems());
        }

        #[unsafe(method_id(itemArray))]
        fn item_array(&self) -> Retained<NSArray<NSMenuItem>> {
            self.popup_cell().map_or_else(NSArray::new, |c| c.itemArray())
        }

        #[unsafe(method(numberOfItems))]
        fn number_of_items(&self) -> isize {
            self.popup_cell().map_or(0, |c| c.numberOfItems())
        }

        #[unsafe(method(indexOfItem:))]
        fn index_of_item(&self, item: &NSMenuItem) -> isize {
            self.popup_cell().map_or(-1, |c| c.indexOfItem(item))
        }

        #[unsafe(method(indexOfItemWithTitle:))]
        fn index_of_item_with_title(&self, title: &NSString) -> isize {
            self.popup_cell().map_or(-1, |c| c.indexOfItemWithTitle(title))
        }

        #[unsafe(method(indexOfItemWithTag:))]
        fn index_of_item_with_tag(&self, tag: isize) -> isize {
            self.popup_cell().map_or(-1, |c| c.indexOfItemWithTag(tag))
        }

        #[unsafe(method(indexOfItemWithRepresentedObject:))]
        fn index_of_item_with_represented_object(&self, object: Option<&AnyObject>) -> isize {
            // SAFETY: the cell's method takes an object or nil.
            self.popup_cell().map_or(-1, |c| unsafe { c.indexOfItemWithRepresentedObject(object) })
        }

        #[unsafe(method(indexOfItemWithTarget:andAction:))]
        fn index_of_item_with_target_and_action(&self, target: Option<&AnyObject>, action: Option<Sel>) -> isize {
            // SAFETY: the cell's method takes a target and an action.
            self.popup_cell().map_or(-1, |c| unsafe { c.indexOfItemWithTarget_andAction(target, action) })
        }

        #[unsafe(method_id(itemAtIndex:))]
        fn item_at_index(&self, index: isize) -> Option<Retained<NSMenuItem>> {
            self.popup_cell().and_then(|c| c.itemAtIndex(index))
        }

        #[unsafe(method_id(itemWithTitle:))]
        fn item_with_title(&self, title: &NSString) -> Option<Retained<NSMenuItem>> {
            self.popup_cell().and_then(|c| c.itemWithTitle(title))
        }

        #[unsafe(method_id(lastItem))]
        fn last_item(&self) -> Option<Retained<NSMenuItem>> {
            self.popup_cell().and_then(|c| c.lastItem())
        }

        #[unsafe(method(selectItem:))]
        fn select_item(&self, item: Option<&NSMenuItem>) {
            self.with_cell(|c| c.selectItem(item));
        }

        #[unsafe(method(selectItemAtIndex:))]
        fn select_item_at_index(&self, index: isize) {
            self.with_cell(|c| c.selectItemAtIndex(index));
        }

        #[unsafe(method(selectItemWithTitle:))]
        fn select_item_with_title(&self, title: &NSString) {
            self.with_cell(|c| c.selectItemWithTitle(title));
        }

        #[unsafe(method(selectItemWithTag:))]
        fn select_item_with_tag(&self, tag: isize) -> bool {
            self.popup_cell().is_some_and(|c| c.selectItemWithTag(tag))
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            self.with_cell(|c| c.setTitle(Some(title)));
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.popup_cell().map_or_else(NSString::new, |c| c.title())
        }

        #[unsafe(method_id(selectedItem))]
        fn selected_item(&self) -> Option<Retained<NSMenuItem>> {
            self.popup_cell().and_then(|c| c.selectedItem())
        }

        #[unsafe(method(indexOfSelectedItem))]
        fn index_of_selected_item(&self) -> isize {
            self.popup_cell().map_or(-1, |c| c.indexOfSelectedItem())
        }

        #[unsafe(method(selectedTag))]
        fn selected_tag(&self) -> isize {
            self.popup_cell().and_then(|c| c.selectedItem()).map_or(-1, |i| i.tag())
        }

        #[unsafe(method(synchronizeTitleAndSelectedItem))]
        fn synchronize_title_and_selected_item(&self) {
            self.with_cell(|c| c.synchronizeTitleAndSelectedItem());
        }

        #[unsafe(method_id(itemTitleAtIndex:))]
        fn item_title_at_index(&self, index: isize) -> Retained<NSString> {
            self.popup_cell().map_or_else(NSString::new, |c| c.itemTitleAtIndex(index))
        }

        #[unsafe(method_id(itemTitles))]
        fn item_titles(&self) -> Retained<NSArray<NSString>> {
            self.popup_cell().map_or_else(NSArray::new, |c| c.itemTitles())
        }

        #[unsafe(method_id(titleOfSelectedItem))]
        fn title_of_selected_item(&self) -> Option<Retained<NSString>> {
            self.popup_cell().and_then(|c| c.titleOfSelectedItem())
        }
    }
);

impl NSPopUpButtonImpl {
    fn popup_cell(&self) -> Option<Retained<NSPopUpButtonCell>> {
        // SAFETY: NSPopUpButtonImpl is a control.
        let control = unsafe { &*(self as *const Self).cast::<NSControl>() };
        let cell = control.cell()?;
        imp(&cell)?;
        // SAFETY: checked to be a pop-up cell just above.
        Some(unsafe { Retained::cast_unchecked(cell) })
    }

    fn with_cell(&self, f: impl FnOnce(&NSPopUpButtonCell)) {
        if let Some(c) = self.popup_cell() {
            f(&c);
        }
    }
}

/// A pop-up button (or pull-down) with `menu`.
fn made(pulls_down: bool, menu: &NSMenu) -> Retained<NSPopUpButton> {
    let mtm = MainThreadMarker::from(menu);
    let b = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, pulls_down);
    b.setMenu(Some(menu));
    b.sizeToFit();
    b
}
