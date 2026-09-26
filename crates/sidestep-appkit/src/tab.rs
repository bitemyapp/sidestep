//! `NSTabView` and `NSTabViewItem`: pages of views, one shown at a time,
//! chosen by a row of tabs.
//!
//! The tab view keeps its items and the selected one; the selected item's
//! view is its subview, framed to `contentRect`, and the others' views are
//! out of the tree. Selecting asks the delegate (`shouldSelect`, then
//! `willSelect`, the swap, `didSelect`), as AppKit does, and adding the
//! first item selects it. The content rectangle keeps AppKit's insets for
//! each tab view type, so layouts made against macOS fit the same.
//!
//! The tabs are drawn as a row of labels centered on the tab side (text in
//! the tab view's font, fills with NSColor), and a click on one selects it.
//! Each tab is its label's width and 13 points either side, 24 points
//! deep, ending 4 points short of the content on the top or left and
//! starting 3 points past it on the bottom or right, as AppKit hit-tests
//! them.
//!
//! An item points back to its tab view weakly, as in AppKit: an item that
//! outlives its tab view has none, and one added to another tab view
//! leaves the first.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSControlSize, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSResponder, NSStringDrawing, NSTabPosition, NSTabState, NSTabView, NSTabViewBorderType, NSTabViewDelegate,
    NSTabViewItem, NSTabViewType, NSView,
};
use objc2_foundation::{NSArray, NSDictionary, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::views;

sidestep_runtime::static_class!(pub NSTABVIEW, NSTABVIEW_META = "NSTabView", || {
    let _ = NSTabViewImpl::class();
});

sidestep_runtime::static_class!(pub NSTABVIEWITEM, NSTABVIEWITEM_META = "NSTabViewItem", || {
    let _ = NSTabViewItemImpl::class();
});

/// `NSNotFound`.
const NOT_FOUND: isize = isize::MAX;

/// How far the content sits in from each side (left, top, right, bottom,
/// in the flipped tab view), as AppKit places it: a bezel, a line, or
/// nothing, and more on the side with the tabs.
fn insets(kind: NSTabViewType) -> [f64; 4] {
    match kind {
        NSTabViewType::TopTabsBezelBorder => [10.0, 33.0, 10.0, 13.0],
        NSTabViewType::LeftTabsBezelBorder => [32.0, 7.0, 10.0, 13.0],
        NSTabViewType::BottomTabsBezelBorder => [10.0, 7.0, 10.0, 31.0],
        NSTabViewType::RightTabsBezelBorder => [10.0, 7.0, 31.0, 13.0],
        NSTabViewType::NoTabsBezelBorder => [10.0, 7.0, 10.0, 13.0],
        NSTabViewType::NoTabsLineBorder => [1.0; 4],
        _ => [0.0; 4],
    }
}

/// The position and border a type stands for.
fn parts(kind: NSTabViewType) -> (NSTabPosition, NSTabViewBorderType) {
    let position = match kind {
        NSTabViewType::TopTabsBezelBorder => NSTabPosition::Top,
        NSTabViewType::LeftTabsBezelBorder => NSTabPosition::Left,
        NSTabViewType::BottomTabsBezelBorder => NSTabPosition::Bottom,
        NSTabViewType::RightTabsBezelBorder => NSTabPosition::Right,
        _ => NSTabPosition::None,
    };
    let border = match kind {
        NSTabViewType::NoTabsLineBorder => NSTabViewBorderType::Line,
        NSTabViewType::NoTabsNoBorder => NSTabViewBorderType::None,
        _ => NSTabViewBorderType::Bezel,
    };
    (position, border)
}

/// The type for a position and border.
fn kind_of(position: NSTabPosition, border: NSTabViewBorderType) -> NSTabViewType {
    match (position, border) {
        (NSTabPosition::Top, _) => NSTabViewType::TopTabsBezelBorder,
        (NSTabPosition::Left, _) => NSTabViewType::LeftTabsBezelBorder,
        (NSTabPosition::Bottom, _) => NSTabViewType::BottomTabsBezelBorder,
        (NSTabPosition::Right, _) => NSTabViewType::RightTabsBezelBorder,
        (_, NSTabViewBorderType::Line) => NSTabViewType::NoTabsLineBorder,
        (_, NSTabViewBorderType::None) => NSTabViewType::NoTabsNoBorder,
        _ => NSTabViewType::NoTabsBezelBorder,
    }
}

/// How far a tab reaches out from its label, and how deep the row is.
const TAB_PADDING: f64 = 13.0;
const TAB_HEIGHT: f64 = 24.0;
/// How far from the content the tabs start: on the top or left they end
/// this near it; on the bottom or right they start this far past it.
const TAB_GAP_BEFORE: f64 = 4.0;
const TAB_GAP_AFTER: f64 = 3.0;
/// The bezel's other edges, out from the content.
const BEZEL: f64 = 3.0;

// NSTabViewItem

pub(crate) struct ItemIvars {
    identifier: RefCell<Option<Retained<AnyObject>>>,
    label: RefCell<Retained<NSString>>,
    color: RefCell<Option<Retained<NSColor>>>,
    image: RefCell<Option<Retained<AnyObject>>>,
    /// Made when first asked for.
    view: RefCell<Option<Retained<NSView>>>,
    view_controller: RefCell<Option<Retained<AnyObject>>>,
    tab_view: RefCell<Option<Weak<NSTabView>>>,
    first_responder: RefCell<Option<Weak<NSView>>>,
    tool_tip: RefCell<Option<Retained<NSString>>>,
}

impl Default for ItemIvars {
    fn default() -> Self {
        ItemIvars {
            identifier: RefCell::new(None),
            label: RefCell::new(NSString::new()),
            color: RefCell::new(None),
            image: RefCell::new(None),
            view: RefCell::new(None),
            view_controller: RefCell::new(None),
            tab_view: RefCell::new(None),
            first_responder: RefCell::new(None),
            tool_tip: RefCell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTabViewItem"]
    #[ivars = ItemIvars]
    pub(crate) struct NSTabViewItemImpl;

    impl NSTabViewItemImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ItemIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithIdentifier:))]
        fn init_with_identifier(this: Allocated<Self>, identifier: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(ItemIvars::default());
            // SAFETY: NSObject's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            this.ivars().identifier.replace(identifier.map(|i| i.retain()));
            this
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Option<Retained<AnyObject>> {
            self.ivars().identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: Option<&AnyObject>) {
            let old = self.ivars().identifier.replace(identifier.map(|i| i.retain()));
            drop(old);
        }

        #[unsafe(method_id(label))]
        fn label(&self) -> Retained<NSString> {
            self.ivars().label.borrow().clone()
        }

        #[unsafe(method(setLabel:))]
        fn set_label(&self, label: &NSString) {
            let old = self.ivars().label.replace(objc2_foundation::NSCopying::copy(label));
            drop(old);
            self.redraw_tabs();
        }

        #[unsafe(method_id(color))]
        fn color(&self) -> Retained<NSColor> {
            self.ivars().color.borrow().clone().unwrap_or_else(|| NSColor::colorWithWhite_alpha(0.93, 1.0))
        }

        #[unsafe(method(setColor:))]
        fn set_color(&self, color: &NSColor) {
            self.ivars().color.replace(Some(color.retain()));
        }

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<AnyObject>> {
            self.ivars().image.borrow().clone()
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, image: Option<&AnyObject>) {
            let old = self.ivars().image.replace(image.map(|i| i.retain()));
            drop(old);
        }

        #[unsafe(method_id(view))]
        fn view(&self) -> Option<Retained<NSView>> {
            Some(self.content_view())
        }

        #[unsafe(method(setView:))]
        fn set_view(&self, view: Option<&NSView>) {
            let old = self.ivars().view.replace(view.map(|v| v.retain()));
            if let Some(tab) = self.tab_view() {
                let tab = tab_imp(&tab);
                if tab.selected().is_some_and(|s| std::ptr::eq(&*s, self.as_item())) {
                    if let Some(old) = &old {
                        old.removeFromSuperview();
                    }
                    tab.show(self.as_item());
                }
            }
            drop(old);
        }

        #[unsafe(method_id(viewController))]
        fn view_controller(&self) -> Option<Retained<AnyObject>> {
            self.ivars().view_controller.borrow().clone()
        }

        #[unsafe(method(setViewController:))]
        fn set_view_controller(&self, controller: Option<&AnyObject>) {
            let old = self.ivars().view_controller.replace(controller.map(|c| c.retain()));
            drop(old);
        }

        #[unsafe(method(tabState))]
        fn tab_state(&self) -> NSTabState {
            let selected = self
                .tab_view()
                .is_some_and(|tab| tab_imp(&tab).selected().is_some_and(|s| std::ptr::eq(&*s, self.as_item())));
            if selected { NSTabState::SelectedTab } else { NSTabState::BackgroundTab }
        }

        #[unsafe(method_id(tabView))]
        fn tab_view_method(&self) -> Option<Retained<NSTabView>> {
            self.tab_view()
        }

        #[unsafe(method_id(initialFirstResponder))]
        fn initial_first_responder(&self) -> Option<Retained<NSView>> {
            self.ivars().first_responder.borrow().as_ref().and_then(Weak::load)
        }

        #[unsafe(method(setInitialFirstResponder:))]
        fn set_initial_first_responder(&self, view: Option<&NSView>) {
            self.ivars().first_responder.replace(view.map(Weak::new));
        }

        #[unsafe(method_id(toolTip))]
        fn tool_tip(&self) -> Option<Retained<NSString>> {
            self.ivars().tool_tip.borrow().clone()
        }

        #[unsafe(method(setToolTip:))]
        fn set_tool_tip(&self, tip: Option<&NSString>) {
            let old = self.ivars().tool_tip.replace(tip.map(objc2_foundation::NSCopying::copy));
            drop(old);
        }

        #[unsafe(method(drawLabel:inRect:))]
        fn draw_label_in_rect(&self, _truncate: bool, rect: NSRect) {
            let attributes = label_attributes(&self.font());
            let label = self.ivars().label.borrow().clone();
            // SAFETY: the dictionary maps attribute names to their values.
            unsafe { label.drawAtPoint_withAttributes(rect.origin, Some(&attributes)) };
        }

        #[unsafe(method(sizeOfLabel:))]
        fn size_of_label(&self, _minimum: bool) -> NSSize {
            let attributes = label_attributes(&self.font());
            let label = self.ivars().label.borrow().clone();
            // SAFETY: as in drawLabel:inRect:.
            unsafe { label.sizeWithAttributes(Some(&attributes)) }
        }
    }

    unsafe impl NSObjectProtocol for NSTabViewItemImpl {}
);

fn item_imp(item: &NSTabViewItem) -> &NSTabViewItemImpl {
    // SAFETY: NSTabViewItem is NSTabViewItemImpl's class; subclasses
    // share its layout.
    unsafe { &*(item as *const NSTabViewItem).cast::<NSTabViewItemImpl>() }
}

fn label_attributes(font: &NSFont) -> Retained<NSDictionary<NSString, AnyObject>> {
    let color = NSColor::colorWithWhite_alpha(0.1, 1.0);
    // SAFETY: the attribute names are constants AppKit exports.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [font, &color];
    NSDictionary::from_slices(&keys, &values)
}

impl NSTabViewItemImpl {
    fn as_item(&self) -> &NSTabViewItem {
        // SAFETY: as in `item_imp`.
        unsafe { &*(self as *const Self).cast::<NSTabViewItem>() }
    }

    fn tab_view(&self) -> Option<Retained<NSTabView>> {
        self.ivars().tab_view.borrow().as_ref().and_then(Weak::load)
    }

    fn link(&self, tab: Option<&NSTabView>) {
        let old = self.ivars().tab_view.replace(tab.map(Weak::new));
        drop(old);
    }

    /// The item's view, made empty when it has none yet.
    fn content_view(&self) -> Retained<NSView> {
        if let Some(view) = self.ivars().view.borrow().clone() {
            return view;
        }
        let mtm = MainThreadMarker::new().expect("a tab view item's view on the main thread");
        let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        self.ivars().view.replace(Some(view.clone()));
        view
    }

    fn font(&self) -> Retained<NSFont> {
        match self.tab_view() {
            Some(tab) => tab.font(),
            None => NSFont::systemFontOfSize(NSFont::systemFontSize()),
        }
    }

    fn redraw_tabs(&self) {
        if let Some(tab) = self.tab_view() {
            tab.setNeedsDisplay(true);
        }
    }
}

// NSTabView

pub(crate) struct TabIvars {
    items: RefCell<Vec<Retained<NSTabViewItem>>>,
    selected: RefCell<Option<Retained<NSTabViewItem>>>,
    kind: Cell<NSTabViewType>,
    font: RefCell<Option<Retained<NSFont>>>,
    draws_background: Cell<bool>,
    truncates: Cell<bool>,
    control_size: Cell<NSControlSize>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
}

impl Default for TabIvars {
    fn default() -> Self {
        TabIvars {
            items: RefCell::default(),
            selected: RefCell::new(None),
            kind: Cell::new(NSTabViewType::TopTabsBezelBorder),
            font: RefCell::new(None),
            draws_background: Cell::new(true),
            truncates: Cell::new(true),
            control_size: Cell::new(NSControlSize::Regular),
            delegate: RefCell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTabView"]
    #[ivars = TabIvars]
    pub(crate) struct NSTabViewImpl;

    impl NSTabViewImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(TabIvars::default());
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(selectTabViewItem:))]
        fn select_tab_view_item(&self, item: Option<&NSTabViewItem>) {
            if let Some(item) = item {
                self.select(item);
            }
        }

        #[unsafe(method(selectTabViewItemAtIndex:))]
        fn select_tab_view_item_at_index(&self, index: isize) {
            if let Some(item) = self.item_at(index) {
                self.select(&item);
            }
        }

        #[unsafe(method(selectTabViewItemWithIdentifier:))]
        fn select_tab_view_item_with_identifier(&self, identifier: &AnyObject) {
            let index = self.index_of_identifier(identifier);
            if let Some(item) = self.item_at(index) {
                self.select(&item);
            }
        }

        #[unsafe(method(takeSelectedTabViewItemFromSender:))]
        fn take_selected_tab_view_item_from_sender(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let index: isize = if sender.class().responds_to(sel!(indexOfSelectedItem)) {
                // SAFETY: indexOfSelectedItem takes nothing and returns an
                // integer.
                unsafe { msg_send![sender, indexOfSelectedItem] }
            } else if sender.class().responds_to(sel!(selectedSegment)) {
                // SAFETY: as above.
                unsafe { msg_send![sender, selectedSegment] }
            } else {
                return;
            };
            if let Some(item) = self.item_at(index) {
                self.select(&item);
            }
        }

        #[unsafe(method(selectFirstTabViewItem:))]
        fn select_first_tab_view_item(&self, _sender: Option<&AnyObject>) {
            if let Some(item) = self.item_at(0) {
                self.select(&item);
            }
        }

        #[unsafe(method(selectLastTabViewItem:))]
        fn select_last_tab_view_item(&self, _sender: Option<&AnyObject>) {
            let last = self.ivars().items.borrow().len() as isize - 1;
            if let Some(item) = self.item_at(last) {
                self.select(&item);
            }
        }

        #[unsafe(method(selectNextTabViewItem:))]
        fn select_next_tab_view_item(&self, _sender: Option<&AnyObject>) {
            if let Some(i) = self.selected_index()
                && let Some(item) = self.item_at(i as isize + 1)
            {
                self.select(&item);
            }
        }

        #[unsafe(method(selectPreviousTabViewItem:))]
        fn select_previous_tab_view_item(&self, _sender: Option<&AnyObject>) {
            if let Some(i) = self.selected_index()
                && let Some(item) = self.item_at(i as isize - 1)
            {
                self.select(&item);
            }
        }

        #[unsafe(method_id(selectedTabViewItem))]
        fn selected_tab_view_item(&self) -> Option<Retained<NSTabViewItem>> {
            self.selected()
        }

        #[unsafe(method_id(font))]
        fn font(&self) -> Retained<NSFont> {
            let font = self.ivars().font.borrow().clone();
            font.unwrap_or_else(|| NSFont::systemFontOfSize(NSFont::systemFontSize()))
        }

        #[unsafe(method(setFont:))]
        fn set_font(&self, font: &NSFont) {
            self.ivars().font.replace(Some(font.retain()));
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(tabViewType))]
        fn tab_view_type(&self) -> NSTabViewType {
            self.ivars().kind.get()
        }

        #[unsafe(method(setTabViewType:))]
        fn set_tab_view_type(&self, kind: NSTabViewType) {
            self.ivars().kind.set(kind);
            self.relayout();
        }

        #[unsafe(method(tabPosition))]
        fn tab_position(&self) -> NSTabPosition {
            parts(self.ivars().kind.get()).0
        }

        #[unsafe(method(setTabPosition:))]
        fn set_tab_position(&self, position: NSTabPosition) {
            let border = parts(self.ivars().kind.get()).1;
            self.ivars().kind.set(kind_of(position, border));
            self.relayout();
        }

        #[unsafe(method(tabViewBorderType))]
        fn tab_view_border_type(&self) -> NSTabViewBorderType {
            parts(self.ivars().kind.get()).1
        }

        #[unsafe(method(setTabViewBorderType:))]
        fn set_tab_view_border_type(&self, border: NSTabViewBorderType) {
            let position = parts(self.ivars().kind.get()).0;
            self.ivars().kind.set(kind_of(position, border));
            self.relayout();
        }

        #[unsafe(method_id(tabViewItems))]
        fn tab_view_items(&self) -> Retained<NSArray<NSTabViewItem>> {
            NSArray::from_retained_slice(&self.ivars().items.borrow())
        }

        #[unsafe(method(setTabViewItems:))]
        fn set_tab_view_items(&self, items: &NSArray<NSTabViewItem>) {
            let old: Vec<Retained<NSTabViewItem>> = self.ivars().items.borrow().clone();
            for item in old.iter().rev() {
                self.remove(item, false);
            }
            for item in items.iter() {
                self.insert(&item, isize::MAX);
            }
        }

        #[unsafe(method(allowsTruncatedLabels))]
        fn allows_truncated_labels(&self) -> bool {
            self.ivars().truncates.get()
        }

        #[unsafe(method(setAllowsTruncatedLabels:))]
        fn set_allows_truncated_labels(&self, flag: bool) {
            self.ivars().truncates.set(flag);
        }

        #[unsafe(method(minimumSize))]
        fn minimum_size(&self) -> NSSize {
            let [l, t, r, b] = insets(self.ivars().kind.get());
            NSSize::new(l + r, t + b)
        }

        #[unsafe(method(drawsBackground))]
        fn draws_background(&self) -> bool {
            self.ivars().draws_background.get()
        }

        #[unsafe(method(setDrawsBackground:))]
        fn set_draws_background(&self, flag: bool) {
            self.ivars().draws_background.set(flag);
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(controlSize))]
        fn control_size(&self) -> NSControlSize {
            self.ivars().control_size.get()
        }

        #[unsafe(method(setControlSize:))]
        fn set_control_size(&self, size: NSControlSize) {
            self.ivars().control_size.set(size);
        }

        #[unsafe(method(addTabViewItem:))]
        fn add_tab_view_item(&self, item: &NSTabViewItem) {
            self.insert(item, isize::MAX);
        }

        #[unsafe(method(insertTabViewItem:atIndex:))]
        fn insert_tab_view_item(&self, item: &NSTabViewItem, index: isize) {
            self.insert(item, index);
        }

        #[unsafe(method(removeTabViewItem:))]
        fn remove_tab_view_item(&self, item: &NSTabViewItem) {
            self.remove(item, true);
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<ProtocolObject<dyn NSTabViewDelegate>>> {
            let d = self.ivars().delegate.borrow().as_ref().and_then(Weak::load);
            // SAFETY: only delegates are stored.
            d.map(|d| unsafe { Retained::cast_unchecked(d) })
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&ProtocolObject<dyn NSTabViewDelegate>>) {
            self.ivars().delegate.replace(delegate.map(|d| Weak::new(d.as_ref())));
        }

        #[unsafe(method_id(tabViewItemAtPoint:))]
        fn tab_view_item_at_point(&self, point: NSPoint) -> Option<Retained<NSTabViewItem>> {
            let items = self.ivars().items.borrow().clone();
            self.tab_rects().into_iter().zip(items).find(|(r, _)| contains(*r, point)).map(|(_, item)| item)
        }

        #[unsafe(method(contentRect))]
        fn content_rect(&self) -> NSRect {
            let b = self.as_view().bounds();
            let [l, t, r, bottom] = insets(self.ivars().kind.get());
            NSRect::new(
                NSPoint::new(b.origin.x + l, b.origin.y + t),
                NSSize::new((b.size.width - l - r).max(0.0), (b.size.height - t - bottom).max(0.0)),
            )
        }

        #[unsafe(method(numberOfTabViewItems))]
        fn number_of_tab_view_items(&self) -> isize {
            self.ivars().items.borrow().len() as isize
        }

        #[unsafe(method(indexOfTabViewItem:))]
        fn index_of_tab_view_item(&self, item: &NSTabViewItem) -> isize {
            let items = self.ivars().items.borrow();
            items.iter().position(|i| std::ptr::eq(&**i, item)).map_or(NOT_FOUND, |i| i as isize)
        }

        #[unsafe(method_id(tabViewItemAtIndex:))]
        fn tab_view_item_at_index(&self, index: isize) -> Retained<NSTabViewItem> {
            self.item_at(index).unwrap_or_else(|| {
                panic!("sidestep: -[NSTabView tabViewItemAtIndex:]: index {index} beyond bounds")
            })
        }

        #[unsafe(method(indexOfTabViewItemWithIdentifier:))]
        fn index_of_tab_view_item_with_identifier(&self, identifier: &AnyObject) -> isize {
            self.index_of_identifier(identifier)
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews_with_old_size(&self, old: NSSize) {
            // SAFETY: NSView's resizeSubviewsWithOldSize: takes the size.
            let _: () = unsafe { msg_send![super(self), resizeSubviewsWithOldSize: old] };
            self.relayout();
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.as_view().convertPoint_fromView(event.locationInWindow(), None);
            match self.as_tab_view().tabViewItemAtPoint(p) {
                Some(item) => self.select(&item),
                // SAFETY: NSResponder's mouseDown: takes the event.
                None => unsafe { msg_send![super(self), mouseDown: event] },
            }
        }
    }

    unsafe impl NSObjectProtocol for NSTabViewImpl {}
);

fn tab_imp(tab: &NSTabView) -> &NSTabViewImpl {
    // SAFETY: NSTabView is NSTabViewImpl's class; subclasses share its
    // layout.
    unsafe { &*(tab as *const NSTabView).cast::<NSTabViewImpl>() }
}

fn contains(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x && p.y >= r.origin.y && p.x < r.origin.x + r.size.width && p.y < r.origin.y + r.size.height
}

impl NSTabViewImpl {
    fn as_view(&self) -> &NSView {
        // SAFETY: an NSTabView is an NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn as_tab_view(&self) -> &NSTabView {
        // SAFETY: NSTabView is this class.
        unsafe { &*(self as *const Self).cast::<NSTabView>() }
    }

    fn selected(&self) -> Option<Retained<NSTabViewItem>> {
        self.ivars().selected.borrow().clone()
    }

    fn selected_index(&self) -> Option<usize> {
        let selected = self.selected()?;
        self.ivars().items.borrow().iter().position(|i| std::ptr::eq(&**i, &*selected))
    }

    fn item_at(&self, index: isize) -> Option<Retained<NSTabViewItem>> {
        let items = self.ivars().items.borrow();
        usize::try_from(index).ok().and_then(|i| items.get(i).cloned())
    }

    fn index_of_identifier(&self, identifier: &AnyObject) -> isize {
        let items = self.ivars().items.borrow().clone();
        items
            .iter()
            .position(|i| {
                let id = item_imp(i).ivars().identifier.borrow().clone();
                // SAFETY: isEqual: takes an object and returns BOOL.
                id.is_some_and(|id| unsafe { msg_send![&*id, isEqual: identifier] })
            })
            .map_or(NOT_FOUND, |i| i as isize)
    }

    fn delegate_for(&self, selector: Sel) -> Option<Retained<AnyObject>> {
        let d = self.ivars().delegate.borrow().as_ref().and_then(Weak::load)?;
        d.class().responds_to(selector).then_some(d)
    }

    fn insert(&self, item: &NSTabViewItem, index: isize) {
        // An item is in one tab view at a time: it leaves the one it is in.
        if let Some(old) = item_imp(item).tab_view() {
            tab_imp(&old).remove(item, true);
        }
        {
            let mut items = self.ivars().items.borrow_mut();
            let at = usize::try_from(index).unwrap_or(0).min(items.len());
            items.insert(at, item.retain());
        }
        item_imp(item).link(Some(self.as_tab_view()));
        if self.selected().is_none() {
            self.select(item);
        }
        self.count_changed();
        self.as_view().setNeedsDisplay(true);
    }

    /// Take `item` out; the selected one hands over to its neighbour
    /// before it, or after it if it was first (when `reselect`).
    fn remove(&self, item: &NSTabViewItem, reselect: bool) {
        let Some(at) = self.ivars().items.borrow().iter().position(|i| std::ptr::eq(&**i, item)) else { return };
        let was_selected = self.selected().is_some_and(|s| std::ptr::eq(&*s, item));
        if was_selected {
            let count = self.ivars().items.borrow().len();
            let next = if at > 0 {
                Some(at - 1)
            } else if count > 1 {
                Some(1)
            } else {
                None
            };
            let next = next.and_then(|i| self.item_at(i as isize)).filter(|_| reselect);
            match next {
                Some(next) => self.select(&next),
                None => {
                    item_imp(item).content_view().removeFromSuperview();
                    self.ivars().selected.replace(None);
                }
            }
            // A refused selection leaves the removed item's view behind.
            if self.selected().is_some_and(|s| std::ptr::eq(&*s, item)) {
                item_imp(item).content_view().removeFromSuperview();
                self.ivars().selected.replace(None);
            }
        }
        // Found again: the delegate may have changed the items while it was
        // told of the selection.
        let removed = {
            let mut items = self.ivars().items.borrow_mut();
            items.iter().position(|i| std::ptr::eq(&**i, item)).map(|i| items.remove(i))
        };
        if removed.is_some() {
            item_imp(item).link(None);
            self.count_changed();
            self.as_view().setNeedsDisplay(true);
        }
        drop(removed);
    }

    fn count_changed(&self) {
        if let Some(d) = self.delegate_for(sel!(tabViewDidChangeNumberOfTabViewItems:)) {
            // SAFETY: the delegate method takes the tab view.
            let _: () = unsafe { msg_send![&*d, tabViewDidChangeNumberOfTabViewItems: self.as_tab_view()] };
        }
    }

    /// Select `item`, asking the delegate first and telling it before and
    /// after.
    fn select(&self, item: &NSTabViewItem) {
        if self.selected().is_some_and(|s| std::ptr::eq(&*s, item)) {
            return;
        }
        let tab = self.as_tab_view();
        if let Some(d) = self.delegate_for(sel!(tabView:shouldSelectTabViewItem:)) {
            // SAFETY: the delegate method takes the tab view and an item and
            // returns BOOL.
            let ok: bool = unsafe { msg_send![&*d, tabView: tab, shouldSelectTabViewItem: Some(item)] };
            if !ok {
                return;
            }
        }
        if let Some(d) = self.delegate_for(sel!(tabView:willSelectTabViewItem:)) {
            // SAFETY: the delegate method takes the tab view and an item.
            let _: () = unsafe { msg_send![&*d, tabView: tab, willSelectTabViewItem: Some(item)] };
        }
        let old = self.ivars().selected.replace(Some(item.retain()));
        if let Some(old) = &old {
            item_imp(old).content_view().removeFromSuperview();
        }
        self.show(item);
        self.as_view().setNeedsDisplay(true);
        if let Some(d) = self.delegate_for(sel!(tabView:didSelectTabViewItem:)) {
            // SAFETY: as for willSelect.
            let _: () = unsafe { msg_send![&*d, tabView: tab, didSelectTabViewItem: Some(item)] };
        }
        drop(old);
    }

    /// Put the selected item's view in the content rectangle.
    fn show(&self, item: &NSTabViewItem) {
        let view = item_imp(item).content_view();
        view.setFrame(self.as_tab_view().contentRect());
        let own =
            views::superview_of(views::imp(&view)).is_some_and(|s| std::ptr::eq(views::as_view(s), self.as_view()));
        if !own {
            self.as_view().addSubview(&view);
        }
    }

    fn relayout(&self) {
        if let Some(item) = self.selected() {
            let view = item_imp(&item).content_view();
            view.setFrame(self.as_tab_view().contentRect());
        }
        self.as_view().setNeedsDisplay(true);
    }

    /// Each item's tab, along the side the type puts them, centered.
    fn tab_rects(&self) -> Vec<NSRect> {
        let kind = self.ivars().kind.get();
        let (position, _) = parts(kind);
        if position == NSTabPosition::None {
            return Vec::new();
        }
        let items = self.ivars().items.borrow().clone();
        let lengths: Vec<f64> = items.iter().map(|i| i.sizeOfLabel(false).width.ceil() + 2.0 * TAB_PADDING).collect();
        let total: f64 = lengths.iter().sum();
        let b = self.as_view().bounds();
        let c = self.as_tab_view().contentRect();
        let horizontal = matches!(position, NSTabPosition::Top | NSTabPosition::Bottom);
        let mut at = if horizontal {
            b.origin.x + ((b.size.width - total) / 2.0).round()
        } else {
            b.origin.y + ((b.size.height - total) / 2.0).round()
        };
        let before = |edge: f64| edge - TAB_GAP_BEFORE - TAB_HEIGHT;
        let after = |edge: f64| edge + TAB_GAP_AFTER;
        lengths
            .iter()
            .map(|&len| {
                let r = match position {
                    NSTabPosition::Top => {
                        NSRect::new(NSPoint::new(at, before(c.origin.y)), NSSize::new(len, TAB_HEIGHT))
                    }
                    NSTabPosition::Bottom => {
                        NSRect::new(NSPoint::new(at, after(c.origin.y + c.size.height)), NSSize::new(len, TAB_HEIGHT))
                    }
                    NSTabPosition::Left => {
                        NSRect::new(NSPoint::new(before(c.origin.x), at), NSSize::new(TAB_HEIGHT, len))
                    }
                    _ => NSRect::new(NSPoint::new(after(c.origin.x + c.size.width), at), NSSize::new(TAB_HEIGHT, len)),
                };
                at += len;
                r
            })
            .collect()
    }

    /// The bezel or line, the content background, and the tabs.
    fn draw(&self) {
        let kind = self.ivars().kind.get();
        let (_, border) = parts(kind);
        let c = self.as_tab_view().contentRect();
        let fill = |r: NSRect, white: f64| {
            NSColor::colorWithWhite_alpha(white, 1.0).setFill();
            NSBezierPath::fillRect(r);
        };
        if border != NSTabViewBorderType::None {
            // The bezel (or line) around the content, out to the middle of
            // the tabs on their side.
            let (position, _) = parts(kind);
            let out = if border == NSTabViewBorderType::Line { 1.0 } else { BEZEL };
            // On the tab side, out to the middle of the tabs.
            let tabs = |gap_before: bool| {
                if gap_before { TAB_GAP_BEFORE + TAB_HEIGHT / 2.0 } else { TAB_GAP_AFTER + TAB_HEIGHT / 2.0 }
            };
            let side = |p| {
                if position != p { out } else { tabs(matches!(p, NSTabPosition::Top | NSTabPosition::Left)) }
            };
            let (l, t, r, b) = (
                side(NSTabPosition::Left),
                side(NSTabPosition::Top),
                side(NSTabPosition::Right),
                side(NSTabPosition::Bottom),
            );
            let outer = NSRect::new(
                NSPoint::new(c.origin.x - l, c.origin.y - t),
                NSSize::new(c.size.width + l + r, c.size.height + t + b),
            );
            fill(outer, 0.75);
            let inner = NSRect::new(
                NSPoint::new(outer.origin.x + 1.0, outer.origin.y + 1.0),
                NSSize::new(outer.size.width - 2.0, outer.size.height - 2.0),
            );
            fill(inner, 0.93);
        }
        if self.ivars().draws_background.get() || border != NSTabViewBorderType::None {
            fill(c, 0.93);
        }
        let items = self.ivars().items.borrow().clone();
        let selected = self.selected();
        for (item, r) in items.iter().zip(self.tab_rects()) {
            let on = selected.as_ref().is_some_and(|s| std::ptr::eq(&**s, &**item));
            fill(r, 0.7);
            let inner = NSRect::new(
                NSPoint::new(r.origin.x + 0.5, r.origin.y + 0.5),
                NSSize::new(r.size.width - 1.0, r.size.height - 1.0),
            );
            fill(inner, if on { 1.0 } else { 0.97 });
            let size = item.sizeOfLabel(false);
            let label = NSRect::new(
                NSPoint::new(
                    r.origin.x + ((r.size.width - size.width) / 2.0).round(),
                    r.origin.y + ((r.size.height - size.height) / 2.0).round(),
                ),
                size,
            );
            item.drawLabel_inRect(false, label);
        }
    }
}
