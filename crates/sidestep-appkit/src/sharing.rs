//! A sharing picker backed by local clipboard copy. Optional desktop sharing
//! services can be supplied by the picker delegate through the public API.
use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSSharingService, NSView};
use objc2_foundation::{NSArray, NSRect, NSString, NSURL};
use std::cell::RefCell;
sidestep_runtime::static_class!(pub(crate) PICKER, PICKER_META = "NSSharingServicePicker", || { let _ = Picker::class(); });
sidestep_runtime::static_class!(pub(crate) SERVICE, SERVICE_META = "NSSharingService", || { let _ = Service::class(); });
struct ServiceState {
    title: Retained<NSString>,
    image: Option<Retained<AnyObject>>,
    alternate: Option<Retained<AnyObject>>,
    handler: RcBlock<dyn Fn()>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSSharingService"]
    #[ivars = ServiceState]
    struct Service;
    impl Service {
        #[unsafe(method_id(initWithTitle:image:alternateImage:handler:))]
        fn init(this: Allocated<Self>, title: &NSString, image: Option<&AnyObject>, alternate: Option<&AnyObject>, handler: &DynBlock<dyn Fn()>) -> Retained<Self> { let this=this.set_ivars(ServiceState { title: title.retain(), image: image.map(Message::retain), alternate: alternate.map(Message::retain), handler: handler.copy(), delegate: RefCell::new(None) }); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> { self.ivars().title.clone() }
        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<AnyObject>> { self.ivars().image.clone() }
        #[unsafe(method_id(alternateImage))]
        fn alternate(&self) -> Option<Retained<AnyObject>> { self.ivars().alternate.clone() }
        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> { self.ivars().delegate.borrow().as_ref().and_then(Weak::load) }
        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) { self.ivars().delegate.replace(delegate.map(Weak::new)); }
        #[unsafe(method(canPerformWithItems:))]
        fn can(&self, _items: &NSArray) -> bool { true }
        #[unsafe(method(performWithItems:))]
        fn perform(&self, items: &NSArray) {
            let delegate=self.ivars().delegate.borrow().as_ref().and_then(Weak::load);
            if let Some(d)=&delegate && responds(d,sel!(sharingService:willShareItems:)) { let _: ()=unsafe { msg_send![&**d, sharingService: self, willShareItems: items] }; }
            self.ivars().handler.call(());
            if let Some(d)=&delegate && responds(d,sel!(sharingService:didShareItems:)) { let _: ()=unsafe { msg_send![&**d, sharingService: self, didShareItems: items] }; }
        }
        #[unsafe(method_id(sharingServicesForItems:))]
        fn services(items: &NSArray) -> Retained<NSArray<NSSharingService>> { services(items) }
        #[unsafe(method_id(sharingServiceNamed:))]
        fn named(_name: &NSString) -> Option<Retained<Self>> { None }
    }
    unsafe impl NSObjectProtocol for Service {}
);
fn responds(object: &AnyObject, selector: objc2::runtime::Sel) -> bool {
    unsafe { msg_send![object, respondsToSelector: selector] }
}
fn services(items: &NSArray) -> Retained<NSArray<NSSharingService>> {
    let strings: Vec<String> = items
        .iter()
        .filter_map(|o| {
            o.downcast_ref::<NSString>()
                .map(ToString::to_string)
                .or_else(|| o.downcast_ref::<NSURL>().and_then(|u| u.absoluteString()).map(|s| s.to_string()))
        })
        .collect();
    if strings.is_empty() {
        return NSArray::new();
    }
    let text = strings.join("\n");
    let handler = RcBlock::new(move || {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        pasteboard.setString_forType(&NSString::from_str(&text), unsafe { NSPasteboardTypeString });
    });
    let service: Retained<NSSharingService> = unsafe {
        msg_send![NSSharingService::alloc(), initWithTitle: &*NSString::from_str("Copy"), image: None::<&AnyObject>, alternateImage: None::<&AnyObject>, handler: &*handler]
    };
    NSArray::from_retained_slice(&[service])
}
struct State {
    items: Retained<NSArray>,
    delegate: RefCell<Option<Weak<AnyObject>>>,
    services: RefCell<Vec<Retained<NSSharingService>>>,
    menu: RefCell<Option<Retained<NSMenu>>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSSharingServicePicker"]
    #[ivars = State]
    struct Picker;
    impl Picker {
        #[unsafe(method_id(initWithItems:))]
        fn init(this: Allocated<Self>, items: &NSArray) -> Retained<Self> { let items=unsafe { msg_send![items, copy] }; let this=this.set_ivars(State { items, delegate: RefCell::new(None), services: RefCell::new(Vec::new()), menu: RefCell::new(None) }); unsafe { msg_send![super(this), init] } }
        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> { self.ivars().delegate.borrow().as_ref().and_then(Weak::load) }
        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) { self.ivars().delegate.replace(delegate.map(Weak::new)); }
        #[unsafe(method(showRelativeToRect:ofView:preferredEdge:))]
        fn show(&self, rect: NSRect, view: &NSView, _edge: usize) { show(self,rect,view); }
        #[unsafe(method(close))]
        fn close(&self) { let menu=self.ivars().menu.borrow().clone(); if let Some(menu)=menu { menu.cancelTracking(); } }
        #[unsafe(method(chooseSharingService:))]
        fn choose(&self, sender: &NSMenuItem) { choose(self,sender.tag() as usize); }
    }
    unsafe impl NSObjectProtocol for Picker {}
);
fn show(picker: &Picker, rect: NSRect, view: &NSView) {
    let mut services = services(&picker.ivars().items);
    let delegate = picker.ivars().delegate.borrow().as_ref().and_then(Weak::load);
    if let Some(d) = &delegate
        && responds(d, sel!(sharingServicePicker:sharingServicesForItems:proposedSharingServices:))
    {
        services = unsafe {
            msg_send![&**d, sharingServicePicker: picker, sharingServicesForItems: &*picker.ivars().items, proposedSharingServices: &*services]
        };
    }
    picker.ivars().services.replace(services.to_vec());
    let menu = NSMenu::initWithTitle(NSMenu::alloc(view.mtm()), &NSString::from_str("Share"));
    for (index, service) in services.iter().enumerate() {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(view.mtm()),
                &service.title(),
                Some(sel!(chooseSharingService:)),
                &NSString::new(),
            )
        };
        item.setTag(index as isize);
        unsafe { item.setTarget(Some(picker)) };
        menu.addItem(&item);
    }
    picker.ivars().menu.replace(Some(menu.clone()));
    menu.popUpMenuPositioningItem_atLocation_inView(None, rect.origin, Some(view));
    picker.ivars().menu.take();
}
fn choose(picker: &Picker, index: usize) {
    let Some(service) = picker.ivars().services.borrow().get(index).cloned() else { return };
    let delegate = picker.ivars().delegate.borrow().as_ref().and_then(Weak::load);
    if let Some(d) = &delegate {
        if responds(d, sel!(sharingServicePicker:delegateForSharingService:)) {
            let target: Option<Retained<AnyObject>> =
                unsafe { msg_send![&**d, sharingServicePicker: picker, delegateForSharingService: &*service] };
            let _: () = unsafe { msg_send![&*service, setDelegate: target.as_deref()] };
        }
        if responds(d, sel!(sharingServicePicker:didChooseSharingService:)) {
            let _: () = unsafe { msg_send![&**d, sharingServicePicker: picker, didChooseSharingService: &*service] };
        }
    }
    unsafe { service.performWithItems(&picker.ivars().items) };
}
