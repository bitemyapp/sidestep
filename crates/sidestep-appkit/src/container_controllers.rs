//! Container controllers keep the child-controller and view hierarchies in sync.
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSResponder, NSSplitView, NSSplitViewController, NSSplitViewItem, NSTabView, NSTabViewItem, NSTabViewType,
    NSViewController,
};
use objc2_foundation::{NSArray, NSBundle, NSString};
use std::cell::{Cell, RefCell};

sidestep_runtime::static_class!(pub(crate) SPLIT, SPLIT_META = "NSSplitViewController", || { let _ = Split::class(); });
sidestep_runtime::static_class!(pub(crate) ITEM, ITEM_META = "NSSplitViewItem", || { let _ = Item::class(); });
sidestep_runtime::static_class!(pub(crate) TABS, TABS_META = "NSTabViewController", || { let _ = Tabs::class(); });
sidestep_runtime::static_class!(pub(crate) ACCESSORY, ACCESSORY_META = "NSTitlebarAccessoryViewController", || { let _ = Accessory::class(); });
/// A split view controller's dimension left to it: -FLT_MAX, as on macOS.
#[unsafe(no_mangle)]
pub static NSSplitViewControllerAutomaticDimension: f64 = -(f32::MAX as f64);
/// A split view item's dimension not given, as its maximum thickness is
/// until set.
#[unsafe(no_mangle)]
pub static NSSplitViewItemUnspecifiedDimension: f64 = UNSPECIFIED;
const UNSPECIFIED: f64 = -1.0;

struct ItemState {
    controller: Retained<NSViewController>,
    owner: RefCell<Option<Weak<NSSplitViewController>>>,
    collapsed: Cell<bool>,
    can_collapse: Cell<bool>,
    min: Cell<f64>,
    max: Cell<f64>,
    priority: Cell<f32>,
    behavior: isize,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSSplitViewItem"]
    #[ivars = ItemState]
    struct Item;
    impl Item {
        #[unsafe(method_id(splitViewItemWithViewController:))]
        fn item(controller: &NSViewController) -> Retained<Self> { split_item(controller, 0) }
        #[unsafe(method_id(sidebarWithViewController:))]
        fn sidebar(controller: &NSViewController) -> Retained<Self> { split_item(controller, 1) }
        #[unsafe(method_id(contentListWithViewController:))]
        fn list(controller: &NSViewController) -> Retained<Self> { split_item(controller, 2) }
        #[unsafe(method_id(inspectorWithViewController:))]
        fn inspector(controller: &NSViewController) -> Retained<Self> { split_item(controller, 3) }
        #[unsafe(method_id(viewController))]
        fn controller(&self) -> Retained<NSViewController> { self.ivars().controller.clone() }
        #[unsafe(method(behavior))]
        fn behavior(&self) -> isize { self.ivars().behavior }
        #[unsafe(method(isCollapsed))]
        fn collapsed(&self) -> bool { self.ivars().collapsed.get() }
        #[unsafe(method(setCollapsed:))]
        fn set_collapsed(&self, value: bool) { self.ivars().collapsed.set(value); refresh_item(self); }
        #[unsafe(method(canCollapse))]
        fn can_collapse(&self) -> bool { self.ivars().can_collapse.get() }
        #[unsafe(method(setCanCollapse:))]
        fn set_can_collapse(&self, value: bool) { self.ivars().can_collapse.set(value); }
        #[unsafe(method(minimumThickness))]
        fn min(&self) -> f64 { self.ivars().min.get() }
        #[unsafe(method(setMinimumThickness:))]
        fn set_min(&self, value: f64) { self.ivars().min.set(value); }
        #[unsafe(method(maximumThickness))]
        fn max(&self) -> f64 { self.ivars().max.get() }
        #[unsafe(method(setMaximumThickness:))]
        fn set_max(&self, value: f64) { self.ivars().max.set(value); }
        #[unsafe(method(holdingPriority))]
        fn priority(&self) -> f32 { self.ivars().priority.get() }
        #[unsafe(method(setHoldingPriority:))]
        fn set_priority(&self, value: f32) { self.ivars().priority.set(value); refresh_item(self); }
    }
    unsafe impl NSObjectProtocol for Item {}
);
fn split_item(controller: &NSViewController, behavior: isize) -> Retained<Item> {
    let this = Item::alloc().set_ivars(ItemState {
        controller: controller.retain(),
        owner: RefCell::new(None),
        collapsed: Cell::new(false),
        can_collapse: Cell::new(behavior == 1 || behavior == 3),
        min: Cell::new(0.0),
        max: Cell::new(UNSPECIFIED),
        priority: Cell::new(250.0),
        behavior,
    });
    unsafe { msg_send![super(this), init] }
}
fn item_imp(item: &NSSplitViewItem) -> &Item {
    unsafe { &*(item as *const NSSplitViewItem).cast::<Item>() }
}
fn refresh_item(item: &Item) {
    let owner = item.ivars().owner.borrow().as_ref().and_then(Weak::load);
    if let Some(owner) = owner
        && owner.isViewLoaded()
    {
        sync_split(unsafe { &*(&*owner as *const NSSplitViewController).cast::<Split>() });
    }
}
#[derive(Default)]
struct SplitState {
    items: RefCell<Vec<Retained<NSSplitViewItem>>>,
    split: RefCell<Option<Retained<NSSplitView>>>,
}
define_class!(
    #[unsafe(super(NSViewController, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSplitViewController"]
    #[ivars = SplitState]
    struct Split;
    impl Split {
        #[unsafe(method_id(initWithNibName:bundle:))]
        fn init(this: Allocated<Self>, name: Option<&NSString>, bundle: Option<&NSBundle>) -> Retained<Self> { let this = this.set_ivars(SplitState::default()); unsafe { msg_send![super(this), initWithNibName: name, bundle: bundle] } }
        #[unsafe(method(loadView))]
        fn load(&self) {
            let view = self.ivars().split.borrow().clone().unwrap_or_else(|| NSSplitView::new(self.mtm()));
            self.ivars().split.replace(Some(view.clone())); self.setView(&view); sync_split(self);
        }
        #[unsafe(method_id(splitView))]
        fn split(&self) -> Retained<NSSplitView> { self.loadViewIfNeeded(); self.ivars().split.borrow().clone().unwrap() }
        #[unsafe(method(setSplitView:))]
        fn set_split(&self, view: &NSSplitView) { self.ivars().split.replace(Some(view.retain())); self.setView(view); sync_split(self); }
        #[unsafe(method_id(splitViewItems))]
        fn items(&self) -> Retained<NSArray<NSSplitViewItem>> { NSArray::from_retained_slice(&self.ivars().items.borrow()) }
        #[unsafe(method(setSplitViewItems:))]
        fn set_items(&self, items: &NSArray<NSSplitViewItem>) {
            let old = self.ivars().items.take(); for item in old { item.viewController(self.mtm()).removeFromParentViewController(); item_imp(&item).ivars().owner.take(); }
            for item in items { let index = self.ivars().items.borrow().len(); insert_split(self, &item, index); }
            if self.isViewLoaded() { sync_split(self); }
        }
        #[unsafe(method(addSplitViewItem:))]
        fn add(&self, item: &NSSplitViewItem) { let index = self.ivars().items.borrow().len(); insert_split(self, item, index); }
        #[unsafe(method(insertSplitViewItem:atIndex:))]
        fn insert(&self, item: &NSSplitViewItem, index: isize) { insert_split(self, item, index.max(0) as usize); }
        #[unsafe(method(removeSplitViewItem:))]
        fn remove(&self, item: &NSSplitViewItem) {
            self.ivars().items.borrow_mut().retain(|i| !std::ptr::eq(&**i, item));
            item.viewController(self.mtm()).removeFromParentViewController(); item_imp(item).ivars().owner.take();
            if self.isViewLoaded() { sync_split(self); }
        }
        #[unsafe(method_id(splitViewItemForViewController:))]
        fn for_controller(&self, vc: &NSViewController) -> Option<Retained<NSSplitViewItem>> { self.ivars().items.borrow().iter().find(|i| std::ptr::eq(&*i.viewController(self.mtm()), vc)).cloned() }
    }
    unsafe impl NSObjectProtocol for Split {}
);
fn insert_split(controller: &Split, item: &NSSplitViewItem, index: usize) {
    let old_owner = item_imp(item).ivars().owner.borrow().as_ref().and_then(Weak::load);
    if let Some(owner) = old_owner {
        owner.removeSplitViewItem(item);
    }
    let count = controller.ivars().items.borrow().len();
    controller.ivars().items.borrow_mut().insert(index.min(count), item.retain());
    controller.addChildViewController(&item.viewController(controller.mtm()));
    let external: &NSSplitViewController = unsafe { &*(controller as *const Split).cast() };
    item_imp(item).ivars().owner.replace(Some(Weak::new(external)));
    if controller.isViewLoaded() {
        sync_split(controller);
    }
}
fn sync_split(controller: &Split) {
    let Some(split) = controller.ivars().split.borrow().clone() else { return };
    for view in split.arrangedSubviews() {
        split.removeArrangedSubview(&view);
        view.removeFromSuperview();
    }
    for item in controller.ivars().items.borrow().clone() {
        if item.isCollapsed() {
            continue;
        }
        let view = item.viewController(controller.mtm()).view();
        split.addArrangedSubview(&view);
        let index = split.arrangedSubviews().count() as isize - 1;
        split.setHoldingPriority_forSubviewAtIndex(item.holdingPriority(), index);
    }
    split.adjustSubviews();
}
#[derive(Default)]
struct TabState {
    tab: RefCell<Option<Retained<NSTabView>>>,
    style: Cell<isize>,
    transition: Cell<usize>,
    propagate: Cell<bool>,
}
define_class!(
    #[unsafe(super(NSViewController, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTabViewController"]
    #[ivars = TabState]
    struct Tabs;
    impl Tabs {
        #[unsafe(method_id(initWithNibName:bundle:))]
        fn init(this: Allocated<Self>, name: Option<&NSString>, bundle: Option<&NSBundle>) -> Retained<Self> { let this = this.set_ivars(TabState::default()); unsafe { msg_send![super(this), initWithNibName: name, bundle: bundle] } }
        #[unsafe(method(loadView))]
        fn load(&self) { self.setView(&tab(self)); }
        #[unsafe(method_id(tabView))]
        fn tab(&self) -> Retained<NSTabView> { tab(self) }
        #[unsafe(method(setTabView:))]
        fn set_tab(&self, view: &NSTabView) { self.ivars().tab.replace(Some(view.retain())); self.setView(view); }
        #[unsafe(method(tabStyle))]
        fn style(&self) -> isize { self.ivars().style.get() }
        #[unsafe(method(setTabStyle:))]
        fn set_style(&self, value: isize) {
            self.ivars().style.set(value);
            // Toolbar tabs use the same selectable strip in Sidestep's client area.
            tab(self).setTabViewType(match value { 1 => NSTabViewType::BottomTabsBezelBorder, -1 => NSTabViewType::NoTabsNoBorder, _ => NSTabViewType::TopTabsBezelBorder });
        }
        #[unsafe(method(transitionOptions))]
        fn transition(&self) -> usize { self.ivars().transition.get() }
        #[unsafe(method(setTransitionOptions:))]
        fn set_transition(&self, value: usize) { self.ivars().transition.set(value); }
        #[unsafe(method(canPropagateSelectedChildViewControllerTitle))]
        fn propagate(&self) -> bool { self.ivars().propagate.get() }
        #[unsafe(method(setCanPropagateSelectedChildViewControllerTitle:))]
        fn set_propagate(&self, value: bool) { self.ivars().propagate.set(value); }
        #[unsafe(method_id(tabViewItems))]
        fn items(&self) -> Retained<NSArray<NSTabViewItem>> { tab(self).tabViewItems() }
        #[unsafe(method(setTabViewItems:))]
        fn set_items(&self, items: &NSArray<NSTabViewItem>) {
            for item in tab(self).tabViewItems() { remove_tab(self, &item); }
            for item in items { insert_tab(self, &item, tab(self).numberOfTabViewItems()); }
        }
        #[unsafe(method(addTabViewItem:))]
        fn add(&self, item: &NSTabViewItem) { insert_tab(self, item, tab(self).numberOfTabViewItems()); }
        #[unsafe(method(insertTabViewItem:atIndex:))]
        fn insert(&self, item: &NSTabViewItem, index: isize) { insert_tab(self, item, index); }
        #[unsafe(method(removeTabViewItem:))]
        fn remove(&self, item: &NSTabViewItem) { remove_tab(self, item); }
        #[unsafe(method(selectedTabViewItemIndex))]
        fn selected(&self) -> isize { let tab = tab(self); tab.selectedTabViewItem().map_or(-1, |i| tab.indexOfTabViewItem(&i)) }
        #[unsafe(method(setSelectedTabViewItemIndex:))]
        fn set_selected(&self, index: isize) {
            // Observable, as AppKit's (Upleft remembers the pane by it).
            sidestep_foundation::observing::will_change(self, "selectedTabViewItemIndex");
            tab(self).selectTabViewItemAtIndex(index);
            sidestep_foundation::observing::did_change(self, "selectedTabViewItemIndex");
        }
        #[unsafe(method_id(tabViewItemForViewController:))]
        fn for_controller(&self, vc: &NSViewController) -> Option<Retained<NSTabViewItem>> { tab(self).tabViewItems().iter().find(|item| item.viewController(self.mtm()).is_some_and(|c| std::ptr::eq(&*c, vc))) }
    }
    unsafe impl NSObjectProtocol for Tabs {}
);
fn tab(controller: &Tabs) -> Retained<NSTabView> {
    if let Some(tab) = controller.ivars().tab.borrow().clone() {
        return tab;
    }
    let tab = NSTabView::new(controller.mtm());
    controller.ivars().tab.replace(Some(tab.clone()));
    tab
}
fn insert_tab(controller: &Tabs, item: &NSTabViewItem, index: isize) {
    if let Some(vc) = item.viewController(controller.mtm()) {
        controller.addChildViewController(&vc);
    }
    tab(controller).insertTabViewItem_atIndex(item, index);
}
fn remove_tab(controller: &Tabs, item: &NSTabViewItem) {
    tab(controller).removeTabViewItem(item);
    if let Some(vc) = item.viewController(controller.mtm()) {
        vc.removeFromParentViewController();
    }
}
#[derive(Default)]
struct AccessoryState {
    attribute: Cell<isize>,
    height: Cell<f64>,
    hidden: Cell<bool>,
}
define_class!(
    #[unsafe(super(NSViewController, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTitlebarAccessoryViewController"]
    #[ivars = AccessoryState]
    struct Accessory;
    impl Accessory {
        #[unsafe(method_id(initWithNibName:bundle:))]
        fn init(this: Allocated<Self>, name: Option<&NSString>, bundle: Option<&NSBundle>) -> Retained<Self> { let this = this.set_ivars(AccessoryState::default()); unsafe { msg_send![super(this), initWithNibName: name, bundle: bundle] } }
        #[unsafe(method(layoutAttribute))]
        fn attribute(&self) -> isize { self.ivars().attribute.get() }
        #[unsafe(method(setLayoutAttribute:))]
        fn set_attribute(&self, value: isize) { self.ivars().attribute.set(value); }
        #[unsafe(method(fullScreenMinHeight))]
        fn height(&self) -> f64 { self.ivars().height.get() }
        #[unsafe(method(setFullScreenMinHeight:))]
        fn set_height(&self, value: f64) { self.ivars().height.set(value); }
        #[unsafe(method(isHidden))]
        fn hidden(&self) -> bool { self.ivars().hidden.get() }
        #[unsafe(method(setHidden:))]
        fn set_hidden(&self, value: bool) { self.ivars().hidden.set(value); self.view().setHidden(value); }
    }
    unsafe impl NSObjectProtocol for Accessory {}
);
