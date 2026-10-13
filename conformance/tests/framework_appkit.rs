//! Container ownership, text-layer pixels, glass content, PDF output, the
//! application icon, table drag masks and the Services question.
mod common;
use common::rect;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::*;
use objc2_foundation::*;
use objc2_quartz_core::*;
use sidestep as _;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SidestepFrameworkPropertyObserver"]
    #[ivars = std::cell::RefCell<Vec<Retained<NSDictionary<NSString, AnyObject>>>>]
    struct PropertyObserver;
    impl PropertyObserver {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn changed(&self, _key: &NSString, _object: &AnyObject, change: &NSDictionary<NSString, AnyObject>, context: *mut std::ffi::c_void) {
            assert_eq!(context as usize, 42);
            self.ivars().borrow_mut().push(objc2::Message::retain(change));
        }
    }
    unsafe impl NSObjectProtocol for PropertyObserver {}
);

fn window_observation(mtm: MainThreadMarker) {
    let window = window(mtm);
    window.setTitle(&NSString::from_str("Before"));
    let observer = PropertyObserver::alloc(mtm).set_ivars(Default::default());
    let observer: Retained<PropertyObserver> = unsafe { msg_send![super(observer), init] };
    let key = NSString::from_str("title");
    unsafe {
        let _: () = msg_send![&*window, addObserver: &*observer, forKeyPath: &*key, options: 15usize, context: 42usize as *mut std::ffi::c_void];
    }
    window.setTitle(&NSString::from_str("After"));
    let changes = observer.ivars().borrow();
    assert_eq!(changes.len(), 3);
    assert_eq!(
        changes[0].objectForKey(&NSString::from_str("new")).unwrap().downcast_ref::<NSString>().unwrap().to_string(),
        "Before"
    );
    assert!(changes[1].objectForKey(&NSString::from_str("notificationIsPrior")).is_some());
    assert_eq!(
        changes[2].objectForKey(&NSString::from_str("old")).unwrap().downcast_ref::<NSString>().unwrap().to_string(),
        "Before"
    );
    assert_eq!(
        changes[2].objectForKey(&NSString::from_str("new")).unwrap().downcast_ref::<NSString>().unwrap().to_string(),
        "After"
    );
    drop(changes);
    unsafe {
        let _: () = msg_send![&*window, removeObserver: &*observer, forKeyPath: &*key, context: 42usize as *mut std::ffi::c_void];
    }
    window.setTitle(&NSString::from_str("Unobserved"));
    assert_eq!(observer.ivars().borrow().len(), 3);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SidestepFrameworkToolbarDelegate"]
    struct ToolbarDelegate;
    impl ToolbarDelegate {
        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn defaults(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            NSArray::from_retained_slice(&[NSString::from_str("test.item")])
        }
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn item(&self, _toolbar: &NSToolbar, id: &NSString, _inserted: bool) -> Retained<NSToolbarItem> {
            let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), id);
            item.setLabel(&NSString::from_str("Test"));
            item.setView(Some(&NSView::initWithFrame(NSView::alloc(self.mtm()), rect(0.0, 0.0, 70.0, 24.0))));
            item
        }
    }
    unsafe impl NSObjectProtocol for ToolbarDelegate {}
);

fn window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    window
}

fn toolbar_and_safe_area(mtm: MainThreadMarker) {
    let delegate: Retained<ToolbarDelegate> = unsafe { msg_send![ToolbarDelegate::alloc(mtm), init] };
    let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &NSString::from_str("test.toolbar"));
    unsafe {
        let _: () = msg_send![&*toolbar, setDelegate: &*delegate];
    }
    let window = window(mtm);
    window.setToolbar(Some(&toolbar));
    assert_eq!(toolbar.items().count(), 1);
    assert_eq!(toolbar.items().objectAtIndex(0).label().to_string(), "Test");
    toolbar.insertItemWithItemIdentifier_atIndex(&NSString::from_str("test.second"), 1);
    assert_eq!(toolbar.items().count(), 2);
    toolbar.removeItemAtIndex(0);
    assert_eq!(toolbar.items().count(), 1);
    window.setToolbar(None);

    let root = window.contentView().unwrap();
    root.setAdditionalSafeAreaInsets(NSEdgeInsets { top: 12.0, left: 8.0, bottom: 4.0, right: 6.0 });
    let guide = root.safeAreaLayoutGuide();
    root.layoutSubtreeIfNeeded();
    assert_eq!(root.safeAreaInsets().top, 12.0);
    assert_eq!(guide.frame().size, NSSize::new(root.bounds().size.width - 14.0, root.bounds().size.height - 16.0));
    root.setFrameSize(NSSize::new(500.0, 350.0));
    root.layoutSubtreeIfNeeded();
    assert_eq!(guide.frame().size.width, root.safeAreaRect().size.width);
}

fn gestures(mtm: MainThreadMarker) {
    let window = window(mtm);
    let root = window.contentView().unwrap();
    let pan = unsafe { NSPanGestureRecognizer::initWithTarget_action(NSPanGestureRecognizer::alloc(mtm), None, None) };
    root.addGestureRecognizer(&pan);
    assert!(pan.view().is_some());
    let other = NSView::new(mtm);
    other.addGestureRecognizer(&pan);
    assert_eq!(root.gestureRecognizers().count(), 0);
    root.addGestureRecognizer(&pan);
    let event = |kind, point, time| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, point, NSEventModifierFlags::empty(), time, window.windowNumber(), None, 0, 1, 1.0).unwrap()
    };
    window.sendEvent(&event(NSEventType::LeftMouseDown, NSPoint::new(10.0, 20.0), 1.0));
    window.sendEvent(&event(NSEventType::LeftMouseDragged, NSPoint::new(40.0, 35.0), 1.1));
    window.sendEvent(&event(NSEventType::LeftMouseDragged, NSPoint::new(60.0, 45.0), 1.2));
    assert_eq!(pan.translationInView(None), NSPoint::new(50.0, 25.0));
    assert_eq!(pan.state(), NSGestureRecognizerState::Changed);
    window.sendEvent(&event(NSEventType::LeftMouseUp, NSPoint::new(60.0, 45.0), 1.3));
    root.removeGestureRecognizer(&pan);
    assert!(pan.view().is_none());
}

fn sharing(_mtm: MainThreadMarker) {
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    let calls = count.clone();
    let block = block2::RcBlock::new(move || calls.set(calls.get() + 1));
    let service: Retained<NSSharingService> = unsafe {
        msg_send![NSSharingService::alloc(), initWithTitle: &*NSString::from_str("Test service"), image: &*NSImage::new(), alternateImage: None::<&NSImage>, handler: &*block]
    };
    let items = NSArray::<AnyObject>::from_retained_slice(&[NSString::from_str("document").into()]);
    assert_eq!(service.title().to_string(), "Test service");
    unsafe {
        assert!(service.canPerformWithItems(Some(&items)));
        service.performWithItems(&items);
    }
    assert_eq!(count.get(), 1);
}

fn containers(mtm: MainThreadMarker) {
    let split = NSSplitViewController::new(mtm);
    let child = NSViewController::new(mtm);
    child.setView(&NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0)));
    let item = NSSplitViewItem::splitViewItemWithViewController(&child);
    split.addSplitViewItem(&item);
    assert_eq!(split.childViewControllers().count(), 1);
    assert!(child.parentViewController().is_some());
    split.view().setFrame(rect(0.0, 0.0, 300.0, 120.0));
    split.splitView().adjustSubviews();
    assert_eq!(split.splitViewItems().count(), 1);
    assert_eq!(split.splitView().arrangedSubviews().count(), 1);
    assert_eq!(child.view().frame().size, NSSize::new(300.0, 120.0));
    split.removeSplitViewItem(&item);
    assert!(child.parentViewController().is_none());
    assert_eq!(split.splitView().arrangedSubviews().count(), 0);

    let tabs = NSTabViewController::new(mtm);
    let a = NSViewController::new(mtm);
    let b = NSViewController::new(mtm);
    a.setTitle(Some(&NSString::from_str("First")));
    b.setTitle(Some(&NSString::from_str("Second")));
    let ia = NSTabViewItem::tabViewItemWithViewController(&a);
    let ib = NSTabViewItem::tabViewItemWithViewController(&b);
    tabs.addTabViewItem(&ia);
    tabs.addTabViewItem(&ib);
    tabs.view().setFrame(rect(0.0, 0.0, 300.0, 200.0));
    tabs.setSelectedTabViewItemIndex(1);
    assert_eq!(tabs.tabViewItems().count(), 2);
    assert_eq!(tabs.selectedTabViewItemIndex(), 1);
    assert!(unsafe { b.view().superview() }.is_some());
    assert!(unsafe { a.view().superview() }.is_none());
    assert_eq!(tabs.childViewControllers().count(), 2);
    tabs.removeTabViewItem(&ib);
    assert!(b.parentViewController().is_none());
}
fn glass(mtm: MainThreadMarker) {
    let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), rect(0.0, 0.0, 100.0, 60.0));
    let content = NSView::new(mtm);
    glass.setContentView(Some(&content));
    assert!(unsafe { content.superview() }.is_some());
    glass.setFrameSize(NSSize::new(200.0, 90.0));
    assert_eq!(content.frame().size, NSSize::new(200.0, 90.0));
    glass.setStyle(NSGlassEffectViewStyle::Clear);
    glass.setTintColor(Some(&NSColor::redColor()));
    glass.setCornerRadius(8.0);
    assert_eq!(glass.style(), NSGlassEffectViewStyle::Clear);
    assert_eq!(glass.cornerRadius(), 8.0);
    glass.setContentView(None);
    assert!(unsafe { content.superview() }.is_none());
    let container = NSGlassEffectContainerView::new(mtm);
    container.setContentView(Some(&glass));
    assert!(unsafe { glass.superview() }.is_some());
    container.setContentView(None);
    assert!(unsafe { glass.superview() }.is_none());
}
fn text_layer(_mtm: MainThreadMarker) {
    let text = CATextLayer::new();
    text.setBounds(rect(0.0, 0.0, 160.0, 60.0));
    text.setFontSize(24.0);
    text.setContentsScale(1.0);
    unsafe { text.setString(Some(&NSString::from_str("Sidestep"))) };
    text.setForegroundColor(Some(&NSColor::redColor().CGColor()));
    text.displayIfNeeded();
    let bitmap = common::bitmap(160, 60);
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap).unwrap();
    let cg = context.CGContext();
    text.renderInContext(&cg);
    let data = unsafe {
        std::slice::from_raw_parts(bitmap.bitmapData(), bitmap.bytesPerRow() as usize * bitmap.pixelsHigh() as usize)
    };
    let colored = data.as_chunks::<4>().0.iter().filter(|p| p[0] > 0 && p[3] > 0).count();
    assert!(colored > 20, "no rendered text ({colored} pixels)");
    unsafe { text.setString(Some(&NSString::from_str("changed"))) };
    assert!(text.needsDisplay());
}
fn printing(mtm: MainThreadMarker) {
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 200.0, 800.0));
    let original = NSPrintInfo::sharedPrintInfo();
    let info: Retained<NSPrintInfo> = unsafe { objc2::msg_send![&*original, copy] };
    info.setPaperSize(NSSize::new(300.0, 400.0));
    info.setLeftMargin(20.0);
    info.setRightMargin(20.0);
    info.setTopMargin(20.0);
    info.setBottomMargin(20.0);
    info.setJobDisposition(unsafe { NSPrintSaveJob });
    let path = std::env::temp_dir().join(format!("sidestep-print-test-{}.pdf", std::process::id()));
    let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
    let dict: Retained<NSMutableDictionary<NSString, AnyObject>> = unsafe { objc2::msg_send![&*info, dictionary] };
    unsafe {
        let _: () = objc2::msg_send![&*dict, setObject: &*url, forKey: NSPrintJobSavingURL];
    }
    let operation = NSPrintOperation::printOperationWithView_printInfo(&view, &info);
    operation.setShowsPrintPanel(false);
    operation.setShowsProgressPanel(false);
    assert!(operation.runOperation());
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.len() > 100);
    assert_eq!(view.frame().size, NSSize::new(200.0, 800.0));
    std::fs::remove_file(path).unwrap();
}

/// What tables allow drags from them to do, the application's icon, and
/// the Services menu's question to a text view: it answers with itself for
/// plain text it can send or take, and the chain after it answers the rest.
fn services_icons_and_drag_masks(mtm: MainThreadMarker) {
    let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    table.setDraggingSourceOperationMask_forLocal(NSDragOperation::Move, true);
    table.setDraggingSourceOperationMask_forLocal(NSDragOperation::Copy, false);
    let mask = |local: bool| -> NSDragOperation {
        // SAFETY: the method takes a BOOL and answers the mask.
        unsafe { msg_send![&*table, draggingSourceOperationMaskForLocal: local] }
    };
    assert_eq!((mask(true), mask(false)), (NSDragOperation::Move, NSDragOperation::Copy));

    let app = NSApplication::sharedApplication(mtm);
    let original = app.applicationIconImage().expect("an icon");
    let replacement = NSImage::initWithSize(NSImage::alloc(), NSSize::new(16.0, 16.0));
    // SAFETY: the setter takes an image or nil.
    unsafe { app.setApplicationIconImage(Some(&replacement)) };
    assert!(app.applicationIconImage().is_some_and(|i| std::ptr::eq(&*i, &*replacement)));
    // SAFETY: as above.
    unsafe { app.setApplicationIconImage(None) };
    assert!(app.applicationIconImage().is_some());
    drop(original);

    let text = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    text.setString(&NSString::from_str("some text"));
    let plain = NSString::from_str("public.utf8-plain-text");
    let png = NSString::from_str("public.png");
    let ask = |send: Option<&NSString>, take: Option<&NSString>| -> Option<Retained<AnyObject>> {
        // SAFETY: the method takes two pasteboard types and answers an object.
        unsafe { msg_send![&*text, validRequestorForSendType: send, returnType: take] }
    };
    let is_text = |answer: Option<Retained<AnyObject>>| {
        answer.is_some_and(|a| std::ptr::eq(&*a, (&*text as *const NSTextView).cast()))
    };
    text.setSelectedRange(NSRange::new(0, 4));
    assert!(is_text(ask(Some(&plain), None)), "the selection goes out as text");
    assert!(is_text(ask(None, Some(&plain))), "text comes back into an editable view");
    assert!(ask(Some(&png), None).is_none(), "nothing after it takes images");
    text.setSelectedRange(NSRange::new(0, 0));
    assert!(ask(Some(&plain), None).is_none(), "no selection, nothing to send");
    text.setEditable(false);
    assert!(ask(None, Some(&plain)).is_none(), "nothing comes back into a view that can't be edited");
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    for (name, test) in [
        ("container controllers", containers as fn(MainThreadMarker)),
        ("glass content", glass),
        ("text layer pixels", text_layer),
        ("PDF printing", printing),
        ("toolbar and safe area", toolbar_and_safe_area),
        ("pan gestures", gestures),
        ("custom sharing service", sharing),
        ("window property observation", window_observation),
        ("services, icons and drag masks", services_icons_and_drag_masks),
    ] {
        autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
