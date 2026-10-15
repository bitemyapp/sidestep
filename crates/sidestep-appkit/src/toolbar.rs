//! Toolbar item models and their selectable client-area strip.
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSButton, NSImage, NSMenuItem, NSToolbar, NSToolbarItem, NSView};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
sidestep_runtime::static_class!(pub(crate) TOOLBAR, TOOLBAR_META = "NSToolbar", || { let _ = Toolbar::class(); });
sidestep_runtime::static_class!(pub(crate) ITEM, ITEM_META = "NSToolbarItem", || { let _ = Item::class(); });
sidestep_foundation::constant_string!(NSToolbarSpaceItemIdentifier = "NSToolbarSpaceItemIdentifier");
sidestep_foundation::constant_string!(NSToolbarFlexibleSpaceItemIdentifier = "NSToolbarFlexibleSpaceItemIdentifier");
sidestep_foundation::constant_string!(NSToolbarSeparatorItemIdentifier = "NSToolbarSeparatorItemIdentifier");
sidestep_foundation::constant_string!(
    NSToolbarCustomizeToolbarItemIdentifier = "NSToolbarCustomizeToolbarItemIdentifier"
);
sidestep_foundation::constant_string!(NSToolbarPrintItemIdentifier = "NSToolbarPrintItemIdentifier");
sidestep_foundation::constant_string!(NSToolbarShowColorsItemIdentifier = "NSToolbarShowColorsItemIdentifier");
sidestep_foundation::constant_string!(NSToolbarShowFontsItemIdentifier = "NSToolbarShowFontsItemIdentifier");
sidestep_foundation::constant_string!(NSToolbarCloudSharingItemIdentifier = "NSToolbarCloudSharingItemIdentifier");
sidestep_foundation::constant_string!(NSToolbarToggleSidebarItemIdentifier = "NSToolbarToggleSidebarItemIdentifier");
sidestep_foundation::constant_string!(
    NSToolbarToggleInspectorItemIdentifier = "NSToolbarToggleInspectorItemIdentifier"
);
sidestep_foundation::constant_string!(
    NSToolbarSidebarTrackingSeparatorItemIdentifier = "NSToolbarSidebarTrackingSeparatorItemIdentifier"
);
sidestep_foundation::constant_string!(
    NSToolbarInspectorTrackingSeparatorItemIdentifier = "NSToolbarInspectorTrackingSeparatorItemIdentifier"
);
sidestep_foundation::constant_string!(NSToolbarWritingToolsItemIdentifier = "NSToolbarWritingToolsItemIdentifier");
sidestep_foundation::constant_string!(NSToolbarWillAddItemNotification = "NSToolbarWillAddItemNotification");
sidestep_foundation::constant_string!(NSToolbarDidRemoveItemNotification = "NSToolbarDidRemoveItemNotification");
sidestep_foundation::constant_string!(NSToolbarItemKey = "NSToolbarItemKey");
sidestep_foundation::constant_string!(NSToolbarNewIndexKey = "NSToolbarNewIndexKey");
struct ItemState {
    id: Retained<NSString>,
    label: RefCell<Retained<NSString>>,
    palette_label: RefCell<Retained<NSString>>,
    tip: RefCell<Option<Retained<NSString>>>,
    view: RefCell<Option<Retained<NSView>>>,
    image: RefCell<Option<Retained<NSImage>>>,
    menu: RefCell<Option<Retained<NSMenuItem>>>,
    target: RefCell<Option<Weak<AnyObject>>>,
    action: Cell<Option<Sel>>,
    enabled: Cell<bool>,
    bordered: Cell<bool>,
    priority: Cell<isize>,
    tag: Cell<isize>,
    min: Cell<NSSize>,
    max: Cell<NSSize>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSToolbarItem"]
    #[ivars = ItemState]
    struct Item;
    impl Item {
        #[unsafe(method_id(initWithItemIdentifier:))]
        fn init(this: Allocated<Self>, id: &NSString) -> Retained<Self> {
            let this=this.set_ivars(ItemState { id: id.retain(), label: RefCell::new(NSString::new()), palette_label: RefCell::new(NSString::new()), tip: RefCell::new(None), view: RefCell::new(None), image: RefCell::new(None), menu: RefCell::new(None), target: RefCell::new(None), action: Cell::new(None), enabled: Cell::new(true), bordered: Cell::new(false), priority: Cell::new(0), tag: Cell::new(0), min: Cell::new(NSSize::ZERO), max: Cell::new(NSSize::new(f64::MAX,f64::MAX)) }); unsafe { msg_send![super(this), init] }
        }
        #[unsafe(method_id(itemIdentifier))]
        fn id(&self) -> Retained<NSString> { self.ivars().id.clone() }
        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> { self.ivars().target.borrow().as_ref().and_then(Weak::load) }
        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) { self.ivars().target.replace(target.map(Weak::new)); }
        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> { self.ivars().action.get() }
        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) { self.ivars().action.set(action); }
        #[unsafe(method(validate))]
        fn validate(&self) { validate(self); }
        #[unsafe(method_id(label))]
        fn label(&self) -> Retained<NSString> { self.ivars().label.borrow().clone() }
        #[unsafe(method(setLabel:))]
        fn set_label(&self, value: &NSString) { self.ivars().label.replace(value.retain()); }
        #[unsafe(method_id(paletteLabel))]
        fn palette_label(&self) -> Retained<NSString> { self.ivars().palette_label.borrow().clone() }
        #[unsafe(method(setPaletteLabel:))]
        fn set_palette_label(&self, value: &NSString) { self.ivars().palette_label.replace(value.retain()); }
        #[unsafe(method_id(toolTip))]
        fn tip(&self) -> Option<Retained<NSString>> { self.ivars().tip.borrow().clone() }
        #[unsafe(method(setToolTip:))]
        fn set_tip(&self, value: Option<&NSString>) { self.ivars().tip.replace(value.map(Message::retain)); }
        #[unsafe(method_id(view))]
        fn view(&self) -> Option<Retained<NSView>> { self.ivars().view.borrow().clone() }
        #[unsafe(method(setView:))]
        fn set_view(&self, value: Option<&NSView>) { self.ivars().view.replace(value.map(Message::retain)); }
        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<NSImage>> { self.ivars().image.borrow().clone() }
        #[unsafe(method(setImage:))]
        fn set_image(&self, value: Option<&NSImage>) { self.ivars().image.replace(value.map(Message::retain)); }
        #[unsafe(method_id(menuFormRepresentation))]
        fn menu(&self) -> Option<Retained<NSMenuItem>> { self.ivars().menu.borrow().clone() }
        #[unsafe(method(setMenuFormRepresentation:))]
        fn set_menu(&self, value: Option<&NSMenuItem>) { self.ivars().menu.replace(value.map(Message::retain)); }
        #[unsafe(method(isEnabled))]
        fn enabled(&self) -> bool { self.ivars().enabled.get() }
        #[unsafe(method(setEnabled:))]
        fn set_enabled(&self, value: bool) { self.ivars().enabled.set(value); }
        #[unsafe(method(isBordered))]
        fn bordered(&self) -> bool { self.ivars().bordered.get() }
        #[unsafe(method(setBordered:))]
        fn set_bordered(&self, value: bool) { self.ivars().bordered.set(value); }
        #[unsafe(method(visibilityPriority))]
        fn priority(&self) -> isize { self.ivars().priority.get() }
        #[unsafe(method(setVisibilityPriority:))]
        fn set_priority(&self, value: isize) { self.ivars().priority.set(value); }
        #[unsafe(method(tag))]
        fn tag(&self) -> isize { self.ivars().tag.get() }
        #[unsafe(method(setTag:))]
        fn set_tag(&self, value: isize) { self.ivars().tag.set(value); }
        #[unsafe(method(minSize))]
        fn min(&self) -> NSSize { self.ivars().min.get() }
        #[unsafe(method(setMinSize:))]
        fn set_min(&self, value: NSSize) { self.ivars().min.set(value); }
        #[unsafe(method(maxSize))]
        fn max(&self) -> NSSize { self.ivars().max.get() }
        #[unsafe(method(setMaxSize:))]
        fn set_max(&self, value: NSSize) { self.ivars().max.set(value); }
    }
    unsafe impl NSObjectProtocol for Item {}
);
fn validate(item: &Item) {
    let target = item.ivars().target.borrow().as_ref().and_then(Weak::load);
    if let Some(target) = target {
        let responds: bool = unsafe { msg_send![&*target, respondsToSelector: sel!(validateToolbarItem:)] };
        if responds {
            let enabled: bool = unsafe { msg_send![&*target, validateToolbarItem: item] };
            item.ivars().enabled.set(enabled);
        }
    }
}
struct State {
    id: Retained<NSString>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
    items: RefCell<Vec<Retained<NSToolbarItem>>>,
    visible: Cell<bool>,
    customize: Cell<bool>,
    autosave: Cell<bool>,
    display: Cell<usize>,
    size: Cell<usize>,
    centered: RefCell<Option<Retained<NSString>>>,
    selected: RefCell<Option<Retained<NSString>>>,
    bar: RefCell<Option<Retained<NSView>>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSToolbar"]
    #[ivars = State]
    struct Toolbar;
    impl Toolbar {
        #[unsafe(method_id(initWithIdentifier:))]
        fn init(this: Allocated<Self>, id: &NSString) -> Retained<Self> { let this=this.set_ivars(State { id: id.retain(), delegate: RefCell::new(None), items: RefCell::new(Vec::new()), visible: Cell::new(true), customize: Cell::new(false), autosave: Cell::new(false), display: Cell::new(0), size: Cell::new(0), centered: RefCell::new(None), selected: RefCell::new(None), bar: RefCell::new(None) }); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(identifier))]
        fn id(&self) -> Retained<NSString> { self.ivars().id.clone() }
        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> { self.ivars().delegate.borrow().as_ref().and_then(Weak::load) }
        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) { self.ivars().delegate.replace(delegate.map(Weak::new)); }
        #[unsafe(method_id(items))]
        fn items(&self) -> Retained<NSArray<NSToolbarItem>> { NSArray::from_retained_slice(&self.ivars().items.borrow()) }
        #[unsafe(method_id(visibleItems))]
        fn visible_items(&self) -> Retained<NSArray<NSToolbarItem>> { NSArray::from_retained_slice(&self.ivars().items.borrow()) }
        #[unsafe(method(insertItemWithItemIdentifier:atIndex:))]
        fn insert(&self, id: &NSString, index: isize) { if let Some(item)=make_item(self,id) { let len=self.ivars().items.borrow().len(); self.ivars().items.borrow_mut().insert((index.max(0) as usize).min(len),item); rebuild(self); } }
        #[unsafe(method(removeItemAtIndex:))]
        fn remove(&self, index: isize) { if index>=0 && (index as usize)<self.ivars().items.borrow().len() { self.ivars().items.borrow_mut().remove(index as usize); rebuild(self); } }
        #[unsafe(method(validateVisibleItems))]
        fn validate(&self) { for item in self.ivars().items.borrow().clone() { item.validate(); } }
        #[unsafe(method(isVisible))]
        fn visible(&self) -> bool { self.ivars().visible.get() }
        #[unsafe(method(setVisible:))]
        fn set_visible(&self, value: bool) {
            self.ivars().visible.set(value);
            let bar = self.ivars().bar.borrow().clone();
            if let Some(bar) = bar {
                bar.setHidden(!value);
                if let Some(window) = bar.window() { mount(&window); }
            }
        }
        #[unsafe(method(allowsUserCustomization))]
        fn customize(&self) -> bool { self.ivars().customize.get() }
        #[unsafe(method(setAllowsUserCustomization:))]
        fn set_customize(&self, value: bool) { self.ivars().customize.set(value); }
        #[unsafe(method(autosavesConfiguration))]
        fn autosave(&self) -> bool { self.ivars().autosave.get() }
        #[unsafe(method(setAutosavesConfiguration:))]
        fn set_autosave(&self, value: bool) { self.ivars().autosave.set(value); }
        #[unsafe(method(displayMode))]
        fn display(&self) -> usize { self.ivars().display.get() }
        #[unsafe(method(setDisplayMode:))]
        fn set_display(&self, value: usize) { self.ivars().display.set(value); }
        #[unsafe(method(sizeMode))]
        fn size(&self) -> usize { self.ivars().size.get() }
        #[unsafe(method(setSizeMode:))]
        fn set_size(&self, value: usize) { self.ivars().size.set(value); }
        #[unsafe(method_id(centeredItemIdentifier))]
        fn centered(&self) -> Option<Retained<NSString>> { self.ivars().centered.borrow().clone() }
        #[unsafe(method(setCenteredItemIdentifier:))]
        fn set_centered(&self, value: Option<&NSString>) { self.ivars().centered.replace(value.map(Message::retain)); }
        #[unsafe(method_id(selectedItemIdentifier))]
        fn selected(&self) -> Option<Retained<NSString>> { self.ivars().selected.borrow().clone() }
        #[unsafe(method(setSelectedItemIdentifier:))]
        fn set_selected(&self, value: Option<&NSString>) { self.ivars().selected.replace(value.map(Message::retain)); }
    }
    unsafe impl NSObjectProtocol for Toolbar {}
);
fn imp(toolbar: &NSToolbar) -> &Toolbar {
    unsafe { &*(toolbar as *const NSToolbar).cast() }
}
fn make_item(toolbar: &Toolbar, id: &NSString) -> Option<Retained<NSToolbarItem>> {
    if id.to_string().starts_with("NSToolbar") {
        return Some(NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(toolbar.mtm()), id));
    }
    let delegate = toolbar.ivars().delegate.borrow().as_ref().and_then(Weak::load)?;
    let responds: bool = unsafe {
        msg_send![&*delegate, respondsToSelector: sel!(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)]
    };
    if responds {
        unsafe { msg_send![&*delegate, toolbar: toolbar, itemForItemIdentifier: id, willBeInsertedIntoToolbar: true] }
    } else {
        None
    }
}
pub(crate) fn view(toolbar: &NSToolbar) -> Retained<NSView> {
    let this = imp(toolbar);
    if let Some(bar) = this.ivars().bar.borrow().clone() {
        return bar;
    }
    let bar = NSView::new(toolbar.mtm());
    this.ivars().bar.replace(Some(bar.clone()));
    let delegate = this.ivars().delegate.borrow().as_ref().and_then(Weak::load);
    if let Some(delegate) = delegate {
        let responds: bool = unsafe { msg_send![&*delegate, respondsToSelector: sel!(toolbarDefaultItemIdentifiers:)] };
        if responds {
            let ids: Retained<NSArray<NSString>> =
                unsafe { msg_send![&*delegate, toolbarDefaultItemIdentifiers: toolbar] };
            for id in ids {
                if let Some(item) = make_item(this, &id) {
                    this.ivars().items.borrow_mut().push(item);
                }
            }
        }
    }
    rebuild(this);
    bar
}
fn rebuild(toolbar: &Toolbar) {
    let Some(bar) = toolbar.ivars().bar.borrow().clone() else { return };
    for view in bar.subviews() {
        view.removeFromSuperview();
    }
    let mut x = 8.0;
    for item in toolbar.ivars().items.borrow().clone() {
        let id = item.itemIdentifier().to_string();
        if id == "NSToolbarSpaceItemIdentifier" || id == "NSToolbarFlexibleSpaceItemIdentifier" {
            x += 20.0;
            continue;
        }
        let view = item.view().unwrap_or_else(|| {
            let button = NSButton::new(toolbar.mtm());
            button.setTitle(&item.label());
            button.setImage(item.image().as_deref());
            unsafe {
                button.setTarget(item.target().as_deref());
                button.setAction(item.action());
            }
            Retained::into_super(Retained::into_super(button))
        });
        let size = view.fittingSize();
        let width = size.width.max(view.frame().size.width).max(32.0);
        view.setFrame(NSRect::new(NSPoint::new(x, 4.0), NSSize::new(width, 32.0)));
        bar.addSubview(&view);
        x += width + 8.0;
    }
    bar.setFrameSize(NSSize::new(x, 40.0));
    bar.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin);
    bar.setHidden(!toolbar.ivars().visible.get());
}

pub(crate) fn height(window: &objc2_app_kit::NSWindow) -> f64 {
    let toolbar = window.toolbar().filter(|t| t.isVisible()).map_or(0.0, |_| 40.0);
    toolbar
        + window
            .titlebarAccessoryViewControllers()
            .iter()
            .filter(|a| !a.view().isHidden())
            .map(|a| a.view().frame().size.height)
            .sum::<f64>()
}

pub(crate) fn mount(window: &objc2_app_kit::NSWindow) {
    let Some(content) = window.contentView() else { return };
    let mut y = content.bounds().size.height;
    if let Some(toolbar) = window.toolbar() {
        let bar = view(&toolbar);
        if toolbar.isVisible() {
            y -= 40.0;
        }
        bar.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(content.bounds().size.width, 40.0)));
        content.addSubview(&bar);
    }
    for accessory in window.titlebarAccessoryViewControllers() {
        let view = accessory.view();
        let height = view.fittingSize().height.max(view.frame().size.height).max(0.0);
        y -= height;
        view.setTranslatesAutoresizingMaskIntoConstraints(true);
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        view.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(content.bounds().size.width, height)));
        content.addSubview(&view);
    }
    content.setNeedsLayout(true);
}
