//! What programs reach at link time and at launch, and the small gaps
//! around them, as macOS has them:
//!
//! - data symbols: `NSApp` (nil until the application's initializer runs,
//!   then the application), the booleans and null of CoreFoundation, and
//!   constants such as the accessibility announcement's;
//! - classes programs make at launch: the font manager and its trait and
//!   weight conversions, the haptic feedback performer, accessibility
//!   elements and custom actions;
//! - the application's Window menu and its list of windows, views'
//!   `clipsToBounds` (off, but for clip views) and what it does to
//!   drawing, `inLiveResize`, and views' accessibility children, custom
//!   actions and parent;
//! - `-[NSWindow center]`, the running application's bundle identifier,
//!   `quickLookWithEvent:`, and a text view's drag types;
//! - Foundation: collections read from property-list files, a file URL's
//!   resource values, and `+[NSThread callStackSymbols]`;
//! - C functions: zones, page sizes, `NSGetSizeAndAlignment`, the extra
//!   reference count, window depths and typed file pasteboard types.
//!
//! No window is shown. AppKit belongs to the main thread, so this file has
//! its own `main`, which checks `NSApp` before anything makes the
//! application.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char};
use std::ptr::NonNull;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSPoint, NSRect, NSSize, NSString, NSThread, NSURL};

use sidestep as _;

unsafe extern "C" {
    /// AppKit's data symbol, as programs that don't message
    /// `+sharedApplication` read it.
    static NSApp: *mut AnyObject;
}

fn app_symbol() -> *mut AnyObject {
    // SAFETY: a pointer-sized data symbol.
    unsafe { std::ptr::read_volatile(&raw const NSApp) }
}

thread_local! {
    /// `NSApp` as the application's subclass saw it once its super
    /// initializer returned.
    static SEEN_IN_INIT: Cell<usize> = const { Cell::new(1) };
    static DRAG_UPDATES: Cell<usize> = const { Cell::new(0) };
    /// The Window menu messages the application got.
    static WINDOWS_ITEMS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn windows_items() -> Vec<String> {
    WINDOWS_ITEMS.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

define_class!(
    #[unsafe(super(NSApplication, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SweepApplication"]
    struct SweepApplication;

    impl SweepApplication {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            assert!(app_symbol().is_null(), "NSApp is set by the application's initializer");
            let this: Option<Retained<Self>> = unsafe { msg_send![super(this.set_ivars(())), init] };
            SEEN_IN_INIT.with(|c| c.set(app_symbol() as usize));
            this
        }

        #[unsafe(method(addWindowsItem:title:filename:))]
        fn add_windows_item(&self, window: &NSWindow, title: &NSString, filename: bool) {
            WINDOWS_ITEMS.with(|l| l.borrow_mut().push(format!("add {title}")));
            let _: () = unsafe { msg_send![super(self), addWindowsItem: window, title: title, filename: filename] };
        }

        #[unsafe(method(changeWindowsItem:title:filename:))]
        fn change_windows_item(&self, window: &NSWindow, title: &NSString, filename: bool) {
            WINDOWS_ITEMS.with(|l| l.borrow_mut().push(format!("change {title}")));
            let _: () = unsafe { msg_send![super(self), changeWindowsItem: window, title: title, filename: filename] };
        }

        #[unsafe(method(removeWindowsItem:))]
        fn remove_windows_item(&self, window: &NSWindow) {
            WINDOWS_ITEMS.with(|l| l.borrow_mut().push("remove".to_owned()));
            let _: () = unsafe { msg_send![super(self), removeWindowsItem: window] };
        }
    }
);

define_class!(
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SweepTextView"]
    struct SweepTextView;

    impl SweepTextView {
        #[unsafe(method(updateDragTypeRegistration))]
        fn update(&self) {
            DRAG_UPDATES.with(|c| c.set(c.get() + 1));
            let _: () = unsafe { msg_send![super(self), updateDragTypeRegistration] };
        }
    }
);

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SweepOverdrawingView"]
    struct OverdrawingView;

    impl OverdrawingView {
        /// Fills twice its size, around itself.
        #[unsafe(method(drawRect:))]
        fn draw(&self, _dirty: NSRect) {
            NSColor::redColor().setFill();
            NSRectFill(rect(-10.0, -10.0, 40.0, 40.0));
        }
    }
);

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn class(name: &CStr) -> &'static AnyClass {
    AnyClass::get(name).unwrap_or_else(|| panic!("no class {name:?}"))
}

fn window(mtm: MainThreadMarker, style: NSWindowStyleMask, size: NSSize) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(10.0, 10.0), size),
            style,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

fn same<A: ?Sized, B: ?Sized>(a: &A, b: &B) -> bool {
    std::ptr::eq((a as *const A).cast::<u8>(), (b as *const B).cast::<u8>())
}

/// `kCFBooleanTrue` and `kCFBooleanFalse` are the numbers
/// `+numberWithBool:` hands out: booleans of C type `char`, of a class of
/// their own below NSNumber; `kCFNull` is `+[NSNull null]`.
fn booleans(_mtm: MainThreadMarker) {
    let yes = objc2_core_foundation::CFBoolean::new(true);
    let no = objc2_core_foundation::CFBoolean::new(false);
    let yes: &AnyObject = unsafe { &*(yes as *const objc2_core_foundation::CFBoolean).cast::<AnyObject>() };
    let no: &AnyObject = unsafe { &*(no as *const objc2_core_foundation::CFBoolean).cast::<AnyObject>() };
    assert!(same(&*NSNumber::new_bool(true), yes));
    assert!(same(&*NSNumber::new_bool(false), no));
    let made: Retained<NSNumber> = unsafe { msg_send![NSNumber::alloc(), initWithBool: true] };
    assert!(same(&*made, yes));
    assert!(yes.class().superclass().is_some_and(|c| c == NSNumber::class()));
    let char_one = NSNumber::new_i8(1);
    assert!(!same(yes.class(), char_one.class()));
    let kind: *const c_char = unsafe { msg_send![yes, objCType] };
    assert_eq!(unsafe { CStr::from_ptr(kind) }, c"c");
    let description: Retained<NSString> = unsafe { msg_send![yes, description] };
    assert_eq!(description.to_string(), "1");
    let description: Retained<NSString> = unsafe { msg_send![no, description] };
    assert_eq!(description.to_string(), "0");
    let one = NSNumber::new_i32(1);
    let equal: bool = unsafe { msg_send![yes, isEqual: &*one] };
    assert!(equal);
    let equal: bool = unsafe { msg_send![&*one, isEqual: yes] };
    assert!(equal);
    let (h1, h2): (usize, usize) = unsafe { (msg_send![yes, hash], msg_send![&*one, hash]) };
    assert_eq!(h1, h2);
    let copied: Retained<AnyObject> = unsafe { msg_send![yes, copy] };
    assert!(same(&*copied, yes));
    let value: i64 = unsafe { msg_send![yes, longLongValue] };
    assert_eq!(value, 1);
    use objc2_core_foundation::{CFBoolean, CFNumber, CFType, ConcreteType};
    let type_of =
        |o: &AnyObject| objc2_core_foundation::CFGetTypeID(Some(unsafe { &*(o as *const AnyObject).cast::<CFType>() }));
    assert_eq!(type_of(yes), CFBoolean::type_id());
    assert_eq!(type_of(&char_one), CFNumber::type_id());
    assert!(objc2_core_foundation::CFBoolean::new(true).as_bool());
    assert!(!objc2_core_foundation::CFBoolean::new(false).as_bool());
    // Only they are booleans to JSON and property lists: a char 1 is a
    // number.
    let list: Retained<NSArray<AnyObject>> = NSArray::from_retained_slice(&[
        Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_i8(1)))),
        Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_bool(true)))),
    ]);
    let json = unsafe {
        objc2_foundation::NSJSONSerialization::dataWithJSONObject_options_error(
            &list,
            objc2_foundation::NSJSONWritingOptions::empty(),
        )
    }
    .expect("JSON");
    assert_eq!(String::from_utf8_lossy(&json.to_vec()), "[1,true]");
    let null = unsafe { objc2_core_foundation::kCFNull }.expect("kCFNull");
    let ns_null: Retained<AnyObject> = unsafe { msg_send![class(c"NSNull"), null] };
    assert!(same(null, &*ns_null));
}

/// Constants programs name, with macOS's values.
fn constants(_mtm: MainThreadMarker) {
    unsafe {
        assert_eq!(NSAccessibilityAnnouncementRequestedNotification.to_string(), "AXAnnouncementRequested");
        assert_eq!(NSAccessibilityAnnouncementKey.to_string(), "AXAnnouncementKey");
        assert_eq!(NSRTFTextDocumentType.to_string(), "NSRTF");
        assert_eq!(NSRTFDTextDocumentType.to_string(), "NSRTFD");
        assert_eq!(NSHTMLTextDocumentType.to_string(), "NSHTML");
        assert_eq!(NSDocumentTypeDocumentAttribute.to_string(), "DocumentType");
        assert_eq!(NSDocumentTypeDocumentOption.to_string(), "DocumentType");
        assert!(NSAppKitVersionNumber >= 2600.0, "{}", NSAppKitVersionNumber);
        assert!(objc2_foundation::NSFoundationVersionNumber > 4000.0);
        assert_eq!(objc2_foundation::NSZeroRect, NSRect::ZERO);
        assert_eq!(objc2_foundation::NSZeroPoint, NSPoint::ZERO);
        assert_eq!(objc2_foundation::NSZeroSize, NSSize::ZERO);
        #[allow(deprecated)]
        let by_word = NSUnderlineByWordMask;
        assert_eq!(by_word, 0x8000);
        assert_eq!(NSWhite, 1.0);
        assert_eq!(NSBlack, 0.0);
    }
    // Posting an announcement is a plain C call; nothing comes back.
    let element = NSObject::new();
    let info = NSDictionary::from_retained_objects(
        &[unsafe { NSAccessibilityAnnouncementKey }],
        &[unsafe { Retained::cast_unchecked::<AnyObject>(s("Saved")) }],
    );
    unsafe {
        NSAccessibilityPostNotificationWithUserInfo(
            &element,
            NSAccessibilityAnnouncementRequestedNotification,
            Some(&info),
        )
    };
}

fn font_manager(_mtm: MainThreadMarker) {
    let manager_class = class(c"NSFontManager");
    let fm: Retained<AnyObject> = unsafe { msg_send![manager_class, sharedFontManager] };
    let again: Retained<AnyObject> = unsafe { msg_send![manager_class, sharedFontManager] };
    assert!(same(&*fm, &*again));
    let made: Retained<AnyObject> = unsafe { msg_send![manager_class, new] };
    assert!(same(&*fm, &*made), "alloc and init give the shared manager");
    let traits = |f: &NSFont| -> usize { unsafe { msg_send![&*fm, traitsOfFont: f] } };
    let weight = |f: &NSFont| -> isize { unsafe { msg_send![&*fm, weightOfFont: f] } };
    let convert =
        |f: &NSFont, t: usize| -> Retained<NSFont> { unsafe { msg_send![&*fm, convertFont: f, toHaveTrait: t] } };
    let remove =
        |f: &NSFont, t: usize| -> Retained<NSFont> { unsafe { msg_send![&*fm, convertFont: f, toNotHaveTrait: t] } };
    let (italic, bold, unbold, unitalic) = (1usize, 2usize, 4usize, 0x0100_0000usize);
    let regular = NSFont::systemFontOfSize(13.0);
    assert_eq!(traits(&regular) & 3, 0);
    assert_eq!(weight(&regular), 5);
    let b = convert(&regular, bold);
    assert_eq!(traits(&b) & 3, bold);
    assert_eq!(weight(&b), 9);
    assert_eq!(b.pointSize(), 13.0);
    assert!(same(&*convert(&b, bold), &*b), "a trait the font has gives the font back");
    let i = convert(&regular, italic);
    assert_eq!(traits(&i) & 3, italic);
    let bi = convert(&b, italic);
    assert_eq!(traits(&bi) & 3, italic | bold);
    assert_eq!(traits(&convert(&bi, unbold)) & 3, italic);
    assert_eq!(traits(&remove(&bi, bold)) & 3, italic);
    assert_eq!(traits(&convert(&bi, unitalic)) & 3, bold);
    assert!(same(&*convert(&regular, unbold), &*regular));
    assert!(same(&*convert(&regular, unitalic), &*regular));
    let bold_system = NSFont::boldSystemFontOfSize(13.0);
    assert_eq!(traits(&bold_system) & 3, bold);
    assert_eq!(traits(&convert(&bold_system, unbold)) & 3, 0);
    let up: Retained<NSFont> = unsafe { msg_send![&*fm, convertWeight: true, ofFont: &*regular] };
    assert!(weight(&up) > 5, "a heavier weight: {}", weight(&up));
    let sized: Retained<NSFont> = unsafe { msg_send![&*fm, convertFont: &*regular, toSize: 20.0f64] };
    assert_eq!(sized.pointSize(), 20.0);
    let mono = NSFont::monospacedSystemFontOfSize_weight(12.0, 0.0);
    assert_eq!(traits(&mono) & 0x400, 0x400, "fixed pitch");
    let families: Retained<NSArray<NSString>> = unsafe { msg_send![&*fm, availableFontFamilies] };
    assert!(families.count() > 0);
    let action: Option<Sel> = unsafe { msg_send![&*fm, action] };
    assert_eq!(action, Some(sel!(changeFont:)));
    let target: Option<Retained<AnyObject>> = unsafe { msg_send![&*fm, target] };
    assert!(target.is_none());
}

fn haptics(_mtm: MainThreadMarker) {
    let manager = class(c"NSHapticFeedbackManager");
    let performer: Retained<AnyObject> = unsafe { msg_send![manager, defaultPerformer] };
    let again: Retained<AnyObject> = unsafe { msg_send![manager, defaultPerformer] };
    assert!(same(&*performer, &*again));
    let _: () = unsafe { msg_send![&*performer, performFeedbackPattern: 0isize, performanceTime: 0usize] };
}

fn accessibility_element(mtm: MainThreadMarker) {
    let element_class = class(c"NSAccessibilityElement");
    let e: Retained<AnyObject> = unsafe { msg_send![element_class, new] };
    unsafe {
        let role: Option<Retained<NSString>> = msg_send![&*e, accessibilityRole];
        let label: Option<Retained<NSString>> = msg_send![&*e, accessibilityLabel];
        let parent: Option<Retained<AnyObject>> = msg_send![&*e, accessibilityParent];
        let children: Option<Retained<AnyObject>> = msg_send![&*e, accessibilityChildren];
        let enabled: bool = msg_send![&*e, isAccessibilityEnabled];
        let is_element: bool = msg_send![&*e, isAccessibilityElement];
        let frame: NSRect = msg_send![&*e, accessibilityFrame];
        assert!(role.is_none() && label.is_none() && parent.is_none() && children.is_none());
        assert!(!enabled && is_element);
        assert_eq!(frame, NSRect::ZERO);
        let view = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
        let _: () = msg_send![&*e, setAccessibilityParent: &*view];
        let _: () = msg_send![&*e, setAccessibilityRole: &*s("AXButton")];
        let _: () = msg_send![&*e, setAccessibilityLabel: &*s("Go")];
        let _: () = msg_send![&*e, setAccessibilityHelp: &*s("Goes")];
        let _: () = msg_send![&*e, setAccessibilityEnabled: true];
        let _: () = msg_send![&*e, setAccessibilityFrame: rect(1.0, 2.0, 3.0, 4.0)];
        let parent: Option<Retained<AnyObject>> = msg_send![&*e, accessibilityParent];
        assert!(parent.is_some_and(|p| same(&*p, &*view)));
        let role: Option<Retained<NSString>> = msg_send![&*e, accessibilityRole];
        assert_eq!(role.map(|r| r.to_string()).as_deref(), Some("AXButton"));
        let help: Option<Retained<NSString>> = msg_send![&*e, accessibilityHelp];
        assert_eq!(help.map(|r| r.to_string()).as_deref(), Some("Goes"));
        let enabled: bool = msg_send![&*e, isAccessibilityEnabled];
        assert!(enabled);
        let frame: NSRect = msg_send![&*e, accessibilityFrame];
        assert_eq!(frame, rect(1.0, 2.0, 3.0, 4.0));
        // A frame in the parent's space: the parent isn't in a window, so
        // it is the frame too.
        let _: () = msg_send![&*e, setAccessibilityFrameInParentSpace: rect(10.0, 20.0, 30.0, 40.0)];
        let local: NSRect = msg_send![&*e, accessibilityFrameInParentSpace];
        let frame: NSRect = msg_send![&*e, accessibilityFrame];
        assert_eq!((local, frame), (rect(10.0, 20.0, 30.0, 40.0), rect(10.0, 20.0, 30.0, 40.0)));
        let child: Retained<AnyObject> = msg_send![element_class, accessibilityElementWithRole: &*s("AXGroup"), frame: rect(5.0, 6.0, 7.0, 8.0), label: &*s("L"), parent: &*e];
        let role: Option<Retained<NSString>> = msg_send![&*child, accessibilityRole];
        let label: Option<Retained<NSString>> = msg_send![&*child, accessibilityLabel];
        let frame: NSRect = msg_send![&*child, accessibilityFrame];
        let parent: Option<Retained<AnyObject>> = msg_send![&*child, accessibilityParent];
        assert_eq!(role.map(|r| r.to_string()).as_deref(), Some("AXGroup"));
        assert_eq!(label.map(|r| r.to_string()).as_deref(), Some("L"));
        assert_eq!(frame, rect(5.0, 6.0, 7.0, 8.0));
        assert!(parent.is_some_and(|p| same(&*p, &*e)));
        let list = NSArray::from_retained_slice(std::slice::from_ref(&child));
        let _: () = msg_send![&*e, setAccessibilityChildren: &*list];
        let children: Option<Retained<NSArray>> = msg_send![&*e, accessibilityChildren];
        assert!(children.is_some_and(|c| same(&*c, &*list)), "the array itself is kept");
        let responds: bool = msg_send![&*e, respondsToSelector: sel!(accessibilityPerformPress)];
        assert!(responds);
    }
}

fn custom_action(_mtm: MainThreadMarker) {
    let action_class = class(c"NSAccessibilityCustomAction");
    let handler = block2::RcBlock::new(|| Bool::YES);
    unsafe {
        let a: Allocated<AnyObject> = msg_send![action_class, alloc];
        let action: Retained<AnyObject> = msg_send![a, initWithName: &*s("Act"), handler: &*handler];
        let name: Retained<NSString> = msg_send![&*action, name];
        assert_eq!(name.to_string(), "Act");
        let stored: *mut AnyObject = msg_send![&*action, handler];
        assert!(!stored.is_null());
        let target: Option<Retained<AnyObject>> = msg_send![&*action, target];
        let selector: Option<Sel> = msg_send![&*action, selector];
        assert!(target.is_none() && selector.is_none());
        let object = NSObject::new();
        let a: Allocated<AnyObject> = msg_send![action_class, alloc];
        let action: Retained<AnyObject> =
            msg_send![a, initWithName: &*s("Two"), target: &*object, selector: sel!(description)];
        let stored: *mut AnyObject = msg_send![&*action, handler];
        let target: Option<Retained<AnyObject>> = msg_send![&*action, target];
        let selector: Option<Sel> = msg_send![&*action, selector];
        assert!(stored.is_null());
        assert!(target.is_some_and(|t| same(&*t, &*object)));
        assert_eq!(selector, Some(sel!(description)));
        let plain: Retained<AnyObject> = msg_send![action_class, new];
        let name: Option<Retained<NSString>> = msg_send![&*plain, name];
        assert!(name.is_none());
        let _: () = msg_send![&*plain, setName: &*s("N")];
        let name: Option<Retained<NSString>> = msg_send![&*plain, name];
        assert_eq!(name.map(|n| n.to_string()).as_deref(), Some("N"));
    }
}

/// The Window menu: nil until set; setting nil keeps it. Its window list
/// goes after a separator (added with the first window), sorted by title,
/// each item ordering its window front.
fn windows_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let menu_of = || -> Option<Retained<AnyObject>> { unsafe { msg_send![&*app, windowsMenu] } };
    assert!(menu_of().is_none());
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Window"));
    unsafe { menu.addItemWithTitle_action_keyEquivalent(&s("Minimize"), Some(sel!(performMiniaturize:)), &s("m")) };
    let _: () = unsafe { msg_send![&*app, setWindowsMenu: &*menu] };
    assert!(menu_of().is_some_and(|m| same(&*m, &*menu)));
    let _: () = unsafe { msg_send![&*app, setWindowsMenu: None::<&AnyObject>] };
    assert!(menu_of().is_some_and(|m| same(&*m, &*menu)));
    let titled = NSWindowStyleMask::Titled;
    let size = NSSize::new(100.0, 100.0);
    let (w1, w2) = (window(mtm, titled, size), window(mtm, titled, size));
    let items = || -> Vec<String> { menu_items(&menu) };
    windows_items();
    // The application's methods list any window they are given, on screen
    // or not, whatever its style.
    unsafe {
        let _: () = msg_send![&*app, addWindowsItem: &*w1, title: &*s("Beta"), filename: false];
        assert_eq!(items(), ["Minimize", "---", "Beta"]);
        let item = menu.itemAtIndex(2).unwrap();
        assert_eq!(item.action(), Some(sel!(makeKeyAndOrderFront:)));
        assert!(item.target().is_some_and(|t| same(&*t, &*w1)));
        let _: () = msg_send![&*app, addWindowsItem: &*w2, title: &*s("Alpha"), filename: false];
        assert_eq!(items(), ["Minimize", "---", "Alpha", "Beta"]);
        let _: () = msg_send![&*app, addWindowsItem: &*w1, title: &*s("Again"), filename: false];
        assert_eq!(items(), ["Minimize", "---", "Alpha", "Beta"], "a window is listed once");
        let borderless = window(mtm, NSWindowStyleMask::Borderless, size);
        let _: () = msg_send![&*app, addWindowsItem: &*borderless, title: &*s("Delta"), filename: false];
        let w3 = window(mtm, titled, size);
        let _: () = msg_send![&*app, addWindowsItem: &*w3, title: &*s("charlie"), filename: false];
        assert_eq!(items(), ["Minimize", "---", "Alpha", "Beta", "charlie", "Delta"], "sorted, ignoring case");
        let untitled = window(mtm, titled, size);
        let _: () = msg_send![&*app, addWindowsItem: &*untitled, title: &*s(""), filename: false];
        assert_eq!(items().len(), 6, "a window without a title isn't listed");
        // Changing adds a window that isn't listed; an empty title takes
        // it out, by removeWindowsItem:.
        let _: () = msg_send![&*app, changeWindowsItem: &*untitled, title: &*s("Echo"), filename: false];
        assert_eq!(items(), ["Minimize", "---", "Alpha", "Beta", "charlie", "Delta", "Echo"]);
        windows_items();
        let _: () = msg_send![&*app, changeWindowsItem: &*untitled, title: &*s(""), filename: false];
        assert_eq!(windows_items(), ["change ", "remove"]);
        assert_eq!(items(), ["Minimize", "---", "Alpha", "Beta", "charlie", "Delta"]);
        // A represented file's window is listed by the file's name.
        let _: () =
            msg_send![&*app, addWindowsItem: &*untitled, title: &*s("notes.txt  \u{2014}  /tmp/a"), filename: true];
        assert_eq!(items(), ["Minimize", "---", "Alpha", "Beta", "charlie", "Delta", "notes.txt"]);
        for w in [&borderless, &w3, &untitled] {
            let _: () = msg_send![&*app, removeWindowsItem: &**w];
        }
        let _: () = msg_send![&*app, changeWindowsItem: &*w1, title: &*s("Gamma"), filename: false];
        assert_eq!(items(), ["Minimize", "---", "Alpha", "Gamma"]);
        let _: () = msg_send![&*app, removeWindowsItem: &*w1];
        assert_eq!(items(), ["Minimize", "---", "Alpha"]);
        windows_items();
        // A window off screen: excluding it says so once; including it
        // again, retitling it and ordering it out (it says so anyway)
        // don't list it.
        w2.setExcludedFromWindowsMenu(true);
        w2.setExcludedFromWindowsMenu(true);
        assert_eq!(windows_items(), ["remove"]);
        assert_eq!(items(), ["Minimize", "---"], "excluding a window takes it out");
        w2.setExcludedFromWindowsMenu(false);
        w2.setTitle(&s("Foxtrot"));
        assert!(windows_items().is_empty());
        w2.orderOut(None);
        assert_eq!(windows_items(), ["remove"]);
        w2.orderOut(None);
        assert_eq!(windows_items(), ["remove"]);
        assert_eq!(items(), ["Minimize", "---"]);
        borderless.close();
        assert_eq!(windows_items().first().map(String::as_str), Some("remove"), "closing orders out");
        w3.close();
        untitled.close();
    }
    w1.close();
    w2.close();
    windows_items();
    if !on_screen_tests() {
        println!("contract_sweep: windows_menu on screen skipped (SIDESTEP_CONFORMANCE_WINDOWS=1 runs it)");
        return;
    }
    windows_menu_on_screen(mtm, &menu);
}

/// Opt-in on macOS, where it shows windows (never activating the
/// application); on Linux, `sidestep-appkit`'s `linux_sweep` shows them
/// through the null render thread.
fn on_screen_tests() -> bool {
    cfg!(target_vendor = "apple") && std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_some()
}

fn menu_items(menu: &NSMenu) -> Vec<String> {
    (0..menu.numberOfItems())
        .map(|i| {
            let item = menu.itemAtIndex(i).unwrap();
            if item.isSeparatorItem() { "---".into() } else { item.title().to_string() }
        })
        .collect()
}

/// Which windows list themselves as they come on screen: titled ones that
/// aren't panels, as they are ordered in, and not while excluded; a
/// window on screen is renamed with its title, and leaves the menu when
/// it's ordered out.
fn windows_menu_on_screen(mtm: MainThreadMarker, menu: &NSMenu) {
    let spin = || {
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&until);
    };
    let items = || -> Vec<String> { menu_items(menu).into_iter().skip(2).collect() };
    let size = NSSize::new(200.0, 100.0);
    let titled = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    let shown = |w: &NSWindow| {
        w.orderFront(None);
        spin();
    };
    let listed = window(mtm, titled, size);
    listed.setTitle(&s("Listed"));
    windows_items();
    shown(&listed);
    assert_eq!(windows_items(), ["add Listed"]);
    assert_eq!(items(), ["Listed"]);
    shown(&listed);
    assert_eq!(windows_items(), ["add Listed"], "ordered front again, it says so again");
    let borderless = window(mtm, NSWindowStyleMask::Borderless, size);
    borderless.setTitle(&s("Borderless"));
    shown(&borderless);
    borderless.setTitle(&s("Borderless 2"));
    let panel_class = class(c"NSPanel");
    let panel: Retained<NSWindow> = unsafe {
        let a: Allocated<NSWindow> = msg_send![panel_class, alloc];
        msg_send![a, initWithContentRect: rect(10.0, 10.0, 200.0, 100.0), styleMask: titled, backing: NSBackingStoreType::Buffered, defer: true]
    };
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(&s("Panel"));
    shown(&panel);
    assert!(windows_items().is_empty(), "borderless windows and panels aren't listed");
    assert_eq!(items(), ["Listed"]);
    let untitled = window(mtm, titled, size);
    shown(&untitled);
    assert_eq!(windows_items(), ["add "]);
    assert_eq!(items(), ["Listed"], "nor windows without a title");
    untitled.setTitle(&s("Late"));
    assert_eq!(windows_items(), ["change Late"]);
    assert_eq!(items(), ["Late", "Listed"]);
    untitled.setTitle(&s("Later"));
    untitled.setTitle(&s("Later"));
    assert_eq!(windows_items(), ["change Later"], "the same title again says nothing");
    assert_eq!(items(), ["Later", "Listed"]);
    listed.orderOut(None);
    spin();
    assert_eq!(windows_items(), ["remove"]);
    assert_eq!(items(), ["Later"]);
    listed.setTitle(&s("Renamed"));
    assert!(windows_items().is_empty(), "retitling a window off screen says nothing");
    shown(&listed);
    assert_eq!(items(), ["Later", "Renamed"]);
    listed.setExcludedFromWindowsMenu(true);
    assert_eq!(items(), ["Later"]);
    windows_items();
    listed.setExcludedFromWindowsMenu(false);
    assert_eq!(windows_items(), ["add Renamed"]);
    assert_eq!(items(), ["Later", "Renamed"]);
    // A style changed on screen doesn't add or remove it.
    listed.setStyleMask(NSWindowStyleMask::Borderless);
    borderless.setStyleMask(titled);
    spin();
    assert_eq!(items(), ["Later", "Renamed"]);
    for w in [&listed, &borderless, &panel, &untitled] {
        w.close();
    }
    spin();
    assert!(items().is_empty());
    windows_items();
}

/// Views don't clip to their bounds (macOS 14 and later), but clip views,
/// scrollers and table row views do; a view that doesn't draws outside
/// itself.
fn clips_to_bounds(mtm: MainThreadMarker) {
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    let clips = |o: &AnyObject| -> bool { unsafe { msg_send![o, clipsToBounds] } };
    assert!(!clips(&v));
    let _: () = unsafe { msg_send![&*v, setClipsToBounds: true] };
    assert!(clips(&v));
    for (name, expected) in [
        (c"NSClipView", true),
        (c"NSScroller", true),
        (c"NSTableRowView", true),
        (c"NSScrollView", false),
        (c"NSTableView", false),
        (c"NSTableCellView", false),
        (c"NSTableHeaderView", false),
        (c"NSTextView", false),
        (c"NSTextField", false),
        (c"NSButton", false),
        (c"NSBox", false),
        (c"NSStackView", false),
        (c"NSSplitView", false),
        (c"NSTabView", false),
        (c"NSImageView", false),
        (c"NSVisualEffectView", false),
        (c"NSPopUpButton", false),
        (c"NSSlider", false),
        (c"NSSegmentedControl", false),
    ] {
        let a: Allocated<AnyObject> = unsafe { msg_send![class(name), alloc] };
        let o: Retained<AnyObject> = unsafe { msg_send![a, initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] };
        assert_eq!(clips(&o), expected, "{name:?}");
    }
    for clip in [false, true] {
        let parent = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 40.0, 40.0));
        let child: Retained<OverdrawingView> =
            unsafe { msg_send![OverdrawingView::alloc(mtm), initWithFrame: rect(10.0, 10.0, 20.0, 20.0)] };
        let _: () = unsafe { msg_send![&*child, setClipsToBounds: clip] };
        parent.addSubview(&child);
        let rep = parent.bitmapImageRepForCachingDisplayInRect(parent.bounds()).expect("a bitmap");
        parent.cacheDisplayInRect_toBitmapImageRep(parent.bounds(), &rep);
        let alpha = |x: isize, y: isize| {
            let (w, h) = (rep.pixelsWide(), rep.pixelsHigh());
            rep.colorAtX_y(x * w / 40, y * h / 40).map_or(0.0, |c| c.alphaComponent())
        };
        assert_eq!(alpha(15, 15), 1.0, "inside, clip {clip}");
        assert_eq!(alpha(5, 5), if clip { 0.0 } else { 1.0 }, "outside, clip {clip}");
        assert_eq!(alpha(35, 35), if clip { 0.0 } else { 1.0 }, "outside, clip {clip}");
        assert_eq!(alpha(2, 2), if clip { 0.0 } else { 1.0 }, "outside, clip {clip}");
    }
    let live: bool = unsafe { msg_send![&*v, inLiveResize] };
    assert!(!live);
}

/// A view's accessibility children, until set, are its subviews that are
/// elements (none for plain views); what is set is kept as it is, and so
/// are custom actions and the parent.
fn view_accessibility(mtm: MainThreadMarker) {
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    let sub = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 5.0, 5.0));
    v.addSubview(&sub);
    unsafe {
        let children: Option<Retained<NSArray>> = msg_send![&*v, accessibilityChildren];
        assert_eq!(children.map(|c| c.count()), Some(0));
        let actions: Option<Retained<AnyObject>> = msg_send![&*v, accessibilityCustomActions];
        assert!(actions.is_none());
        let parent: Option<Retained<AnyObject>> = msg_send![&*sub, accessibilityParent];
        assert!(parent.is_none(), "a view outside a window has no parent");
        let element: Retained<AnyObject> = msg_send![class(c"NSAccessibilityElement"), new];
        let list = NSArray::from_retained_slice(std::slice::from_ref(&element));
        let _: () = msg_send![&*v, setAccessibilityChildren: &*list];
        let children: Option<Retained<NSArray>> = msg_send![&*v, accessibilityChildren];
        assert!(children.is_some_and(|c| same(&*c, &*list)));
        let _: () = msg_send![&*v, setAccessibilityChildren: None::<&AnyObject>];
        let children: Option<Retained<AnyObject>> = msg_send![&*v, accessibilityChildren];
        assert!(children.is_none());
        let action: Retained<AnyObject> = msg_send![class(c"NSAccessibilityCustomAction"), new];
        let actions = NSArray::from_retained_slice(&[action]);
        let _: () = msg_send![&*v, setAccessibilityCustomActions: &*actions];
        let stored: Option<Retained<NSArray>> = msg_send![&*v, accessibilityCustomActions];
        assert!(stored.is_some_and(|c| same(&*c, &*actions)));
        let other = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        let _: () = msg_send![&*sub, setAccessibilityParent: &*other];
        let parent: Option<Retained<AnyObject>> = msg_send![&*sub, accessibilityParent];
        assert!(parent.is_some_and(|p| same(&*p, &*other)));
    }
}

/// `center` puts a window halfway across its screen's visible frame and a
/// quarter of the way down, in whole points; a window taller than it has
/// its top at the top. Without a screen (Linux without a compositor), the
/// frame stays as it is.
fn window_center(mtm: MainThreadMarker) {
    let screen = NSScreen::mainScreen(mtm);
    for (w, h) in [(401.0, 301.0), (200.0, 100.0), (300.0, 5000.0)] {
        let win = window(mtm, NSWindowStyleMask::Titled, NSSize::new(w, h));
        let before = win.frame();
        win.center();
        let after = win.frame();
        assert_eq!(after.size, before.size);
        match &screen {
            Some(screen) => {
                let area = screen.visibleFrame();
                let x = (area.origin.x + (area.size.width - after.size.width) / 2.0).floor();
                let y = if after.size.height <= area.size.height {
                    (area.origin.y + (area.size.height - after.size.height) * 0.75).floor()
                } else {
                    area.origin.y + area.size.height - after.size.height
                };
                assert_eq!(after.origin, NSPoint::new(x, y), "{w}x{h} in {area:?}");
            }
            None => assert_eq!(after, before),
        }
    }
}

fn running_application(_mtm: MainThreadMarker) {
    let current = NSRunningApplication::currentApplication();
    let bundle = objc2_foundation::NSBundle::mainBundle();
    assert_eq!(current.bundleIdentifier().map(|i| i.to_string()), bundle.bundleIdentifier().map(|i| i.to_string()));
}

/// `quickLookWithEvent:` is a responder method; a text view registers its
/// drag types while editable, through `updateDragTypeRegistration`, which
/// `setEditable:` asks for when the flag changes (and `setSelectable:`
/// when it turns editing off); a new one has none registered.
fn responder_hooks(mtm: MainThreadMarker) {
    let responds = |c: &CStr, s: Sel| -> bool { unsafe { msg_send![class(c), instancesRespondToSelector: s] } };
    assert!(responds(c"NSResponder", sel!(quickLookWithEvent:)));
    assert!(responds(c"NSView", sel!(quickLookWithEvent:)));
    assert!(responds(c"NSTextView", sel!(updateDragTypeRegistration)));
    assert!(responds(c"NSView", sel!(viewWillStartLiveResize)));
    assert!(responds(c"NSView", sel!(viewDidEndLiveResize)));
    assert!(responds(c"NSWindow", sel!(constrainFrameRect:toScreen:)));
    assert!(!responds(c"NSTextView", sel!(pasteAndMatchStyle:)), "AppKit's text view has no such command");
    let w = window(mtm, NSWindowStyleMask::Titled, NSSize::new(300.0, 300.0));
    let tv: Retained<SweepTextView> =
        unsafe { msg_send![SweepTextView::alloc(mtm), initWithFrame: rect(0.0, 0.0, 100.0, 100.0)] };
    let tv: Retained<NSTextView> = unsafe { Retained::cast_unchecked(tv) };
    w.contentView().unwrap().addSubview(&tv);
    let registered = || -> usize {
        let types: Retained<NSArray> = unsafe { msg_send![&*tv, registeredDraggedTypes] };
        types.count()
    };
    let updates = || DRAG_UPDATES.with(|c| c.replace(0));
    assert_eq!(updates(), 0, "a new text view, into a window, isn't told");
    assert_eq!(registered(), 0);
    tv.setEditable(true);
    assert_eq!(updates(), 0, "already editable");
    tv.setEditable(false);
    assert_eq!(updates(), 1);
    assert_eq!(registered(), 0);
    tv.setEditable(false);
    assert_eq!(updates(), 0);
    tv.setEditable(true);
    assert_eq!(updates(), 1);
    assert!(registered() > 0);
    tv.setSelectable(false);
    assert_eq!(updates(), 1, "no longer editable either");
    assert!(!tv.isEditable());
    assert_eq!(registered(), 0);
    let acceptable: Retained<NSArray> = unsafe { msg_send![&*tv, acceptableDragTypes] };
    assert!(acceptable.count() > 0);
}

/// Dictionaries and arrays read from property-list files: nil for a file
/// that isn't there or holds the other kind.
fn property_list_files(_mtm: MainThreadMarker) {
    let dir = std::env::temp_dir().join(format!("sidestep-sweep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dict_path = dir.join("d.plist");
    std::fs::write(
        &dict_path,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>a</key><integer>1</integer><key>b</key><array><string>x</string></array></dict></plist>\n",
    )
    .unwrap();
    let array_path = dir.join("a.plist");
    std::fs::write(&array_path, "(one, two, three)").unwrap();
    let url = NSURL::fileURLWithPath(&s(dict_path.to_str().unwrap()));
    let array_url = NSURL::fileURLWithPath(&s(array_path.to_str().unwrap()));
    let missing = NSURL::fileURLWithPath(&s(dir.join("none.plist").to_str().unwrap()));
    unsafe {
        let a: Allocated<AnyObject> = msg_send![NSDictionary::<AnyObject, AnyObject>::class(), alloc];
        let d: Option<Retained<AnyObject>> = msg_send![a, initWithContentsOfURL: &*url];
        let d = d.expect("the dictionary").downcast::<NSDictionary>().expect("a dictionary");
        let d: Retained<NSDictionary<NSString, AnyObject>> = Retained::cast_unchecked(d);
        assert_eq!(d.count(), 2);
        let one = d.objectForKey(&s("a")).and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.integerValue());
        assert_eq!(one, Some(1));
        let b = d.objectForKey(&s("b")).and_then(|v| v.downcast::<NSArray>().ok()).map(|a| a.count());
        assert_eq!(b, Some(1));
        let a: Allocated<AnyObject> = msg_send![NSDictionary::<AnyObject, AnyObject>::class(), alloc];
        let none: Option<Retained<AnyObject>> = msg_send![a, initWithContentsOfURL: &*missing];
        assert!(none.is_none());
        let a: Allocated<AnyObject> = msg_send![NSDictionary::<AnyObject, AnyObject>::class(), alloc];
        let none: Option<Retained<AnyObject>> = msg_send![a, initWithContentsOfURL: &*array_url];
        assert!(none.is_none(), "an array isn't a dictionary");
        let d: Option<Retained<NSDictionary>> =
            msg_send![NSDictionary::<AnyObject, AnyObject>::class(), dictionaryWithContentsOfURL: &*url];
        assert_eq!(d.map(|d| d.count()), Some(2));
        let d: Option<Retained<NSDictionary>> =
            msg_send![class(c"NSMutableDictionary"), dictionaryWithContentsOfFile: &*s(dict_path.to_str().unwrap())];
        let d = d.expect("a mutable dictionary");
        let mutable: bool = msg_send![&*d, isKindOfClass: class(c"NSMutableDictionary")];
        assert!(mutable);
        let a: Allocated<AnyObject> = msg_send![NSArray::<AnyObject>::class(), alloc];
        let list: Option<Retained<AnyObject>> = msg_send![a, initWithContentsOfURL: &*array_url];
        assert_eq!(list.and_then(|l| l.downcast::<NSArray>().ok()).map(|l| l.count()), Some(3));
        let list: Option<Retained<NSArray>> = msg_send![NSArray::<AnyObject>::class(), arrayWithContentsOfURL: &*url];
        assert!(list.is_none(), "a dictionary isn't an array");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A file URL's resource values: the keys it has values for (unknown ones
/// left out); nil and Cocoa's 260 for a missing file; none, and no error,
/// for a URL that isn't a file's.
fn resource_values(_mtm: MainThreadMarker) {
    let dir = std::env::temp_dir().join(format!("sidestep-values-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("file.txt");
    std::fs::write(&path, "twelve bytes").unwrap();
    let url = NSURL::fileURLWithPath(&s(path.to_str().unwrap()));
    let keys = NSArray::from_retained_slice(&[
        s("NSURLIsDirectoryKey"),
        s("NSURLNameKey"),
        s("NSURLFileSizeKey"),
        s("NSURLIsRegularFileKey"),
        s("NSURLContentModificationDateKey"),
        s("NoSuchKey"),
    ]);
    unsafe {
        let mut error: *mut NSError = std::ptr::null_mut();
        let values: Option<Retained<NSDictionary<NSString, AnyObject>>> =
            msg_send![&*url, resourceValuesForKeys: &*keys, error: &mut error];
        let values = values.expect("values");
        assert_eq!(values.count(), 5);
        let get = |k: &str| values.objectForKey(&s(k));
        assert_eq!(
            get("NSURLNameKey").and_then(|v| v.downcast::<NSString>().ok()).map(|n| n.to_string()).as_deref(),
            Some("file.txt")
        );
        assert_eq!(
            get("NSURLFileSizeKey").and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.integerValue()),
            Some(12)
        );
        assert_eq!(
            get("NSURLIsDirectoryKey").and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.boolValue()),
            Some(false)
        );
        assert_eq!(
            get("NSURLIsRegularFileKey").and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.boolValue()),
            Some(true)
        );
        assert!(get("NSURLContentModificationDateKey").is_some());
        let mut value: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![&*url, getResourceValue: &mut value, forKey: &*s("NSURLIsDirectoryKey"), error: std::ptr::null_mut::<*mut NSError>()];
        assert!(ok && !value.is_null());
        let missing = NSURL::fileURLWithPath(&s(dir.join("none").to_str().unwrap()));
        let mut error: *mut NSError = std::ptr::null_mut();
        let values: Option<Retained<AnyObject>> =
            msg_send![&*missing, resourceValuesForKeys: &*keys, error: &mut error];
        assert!(values.is_none());
        assert!(!error.is_null());
        assert_eq!((*error).code(), 260);
        let web = NSURL::URLWithString(&s("https://example.com/")).unwrap();
        let mut error: *mut NSError = std::ptr::null_mut();
        let values: Option<Retained<NSDictionary>> = msg_send![&*web, resourceValuesForKeys: &*keys, error: &mut error];
        assert_eq!(values.map(|v| v.count()), Some(0));
        assert!(error.is_null());
        let empty: Retained<NSArray<NSString>> = NSArray::new();
        let values: Option<Retained<NSDictionary>> =
            msg_send![&*url, resourceValuesForKeys: &*empty, error: std::ptr::null_mut::<*mut NSError>()];
        assert_eq!(values.map(|v| v.count()), Some(0));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A line per frame, from the caller out: the index padded to four
/// columns, the image to thirty-six, the address in sixteen hex digits,
/// then a symbol and an offset.
fn call_stack(_mtm: MainThreadMarker) {
    let symbols: Retained<NSArray<NSString>> = unsafe { msg_send![NSThread::class(), callStackSymbols] };
    let addresses: Retained<NSArray<NSNumber>> = unsafe { msg_send![NSThread::class(), callStackReturnAddresses] };
    assert!(symbols.count() > 2, "{}", symbols.count());
    assert!(addresses.count() > 2);
    for (i, line) in symbols.iter().enumerate() {
        let line = line.to_string();
        let index = format!("{i:<4}");
        assert!(line.starts_with(&index), "{line:?}");
        let rest = &line[4..];
        assert!(rest.len() > 36 && rest.as_bytes()[35] == b' ', "{line:?}");
        let tail = &rest[36..];
        assert!(tail.starts_with("0x") && tail[2..18].bytes().all(|b| b.is_ascii_hexdigit()), "{line:?}");
        let (_, offset) = tail[19..].rsplit_once(" + ").expect("an offset");
        assert!(offset.parse::<u64>().is_ok(), "{line:?}");
    }
}

/// Zones are one default zone; pages are the machine's.
fn zones_and_pages(_mtm: MainThreadMarker) {
    use objc2_foundation::*;
    let zone = NSDefaultMallocZone();
    assert_eq!(unsafe { NSZoneName(zone.as_ptr()) }.to_string(), "DefaultMallocZone");
    assert_eq!(unsafe { NSZoneName(std::ptr::null_mut()) }.to_string(), "DefaultMallocZone");
    let page = NSPageSize();
    assert!(page.is_power_of_two() && page >= 4096);
    assert_eq!(1 << NSLogPageSize(), page);
    assert_eq!(NSRoundUpToMultipleOfPageSize(1), page);
    assert_eq!(NSRoundDownToMultipleOfPageSize(page + 1), page);
    let memory = unsafe { NSZoneMalloc(zone.as_ptr(), 64) };
    unsafe { NSZoneFree(zone.as_ptr(), memory) };
    let size_of = |t: &CStr| {
        let (mut size, mut align) = (0usize, 0usize);
        let rest =
            unsafe { NSGetSizeAndAlignment(NonNull::new(t.as_ptr().cast_mut()).unwrap(), &mut size, &mut align) };
        let rest = unsafe { CStr::from_ptr(rest.as_ptr()) }.to_str().unwrap().to_owned();
        (size, align, rest)
    };
    assert_eq!(size_of(c"i"), (4, 4, String::new()));
    assert_eq!(size_of(c"{CGRect={CGPoint=dd}{CGSize=dd}}"), (32, 8, String::new()));
    assert_eq!(size_of(c"[4s]"), (8, 2, String::new()));
    assert_eq!(size_of(c"(?=ic)"), (4, 4, String::new()));
    assert_eq!(size_of(c"id"), (4, 4, "d".to_owned()));
    let o = NSObject::new();
    unsafe {
        assert_eq!(NSExtraRefCount(&o), 0);
        NSIncrementExtraRefCount(&o);
        assert_eq!(NSExtraRefCount(&o), 1);
        assert!(!NSDecrementExtraRefCountWasZero(&o));
        assert!(NSDecrementExtraRefCountWasZero(&o));
        assert_eq!(NSExtraRefCount(&o), 0);
    }
}

/// Window depths, typed file pasteboard types and the window list.
mod raw {
    use objc2_foundation::NSString;

    unsafe extern "C-unwind" {
        pub(crate) fn NSCreateFilenamePboardType(file_type: &NSString) -> *mut NSString;
        pub(crate) fn NSCreateFileContentsPboardType(file_type: &NSString) -> *mut NSString;
    }
}

fn appkit_functions(_mtm: MainThreadMarker) {
    unsafe {
        let mut exact = Bool::NO;
        assert_eq!(NSBestDepth(NSDeviceRGBColorSpace, 8, 32, false, &mut exact).0, 0x208);
        assert!(exact.as_bool());
        assert_eq!(NSBestDepth(NSCalibratedWhiteColorSpace, 8, 8, false, std::ptr::null_mut()).0, 0x108);
        assert_eq!(NSBestDepth(NSDeviceRGBColorSpace, 16, 64, false, std::ptr::null_mut()).0, 0x210);
        assert_eq!(NSNumberOfColorComponents(NSDeviceRGBColorSpace), 3);
        assert_eq!(NSNumberOfColorComponents(NSCalibratedWhiteColorSpace), 1);
        assert_eq!(NSNumberOfColorComponents(NSDeviceCMYKColorSpace), 4);
        let depth = NSWindowDepth(0x208);
        assert_eq!(NSBitsPerSampleFromDepth(depth), 8);
        assert_eq!(NSBitsPerPixelFromDepth(depth), 24);
        assert_eq!(NSBitsPerPixelFromDepth(NSWindowDepth(0x210)), 64);
        assert_eq!(NSColorSpaceFromDepth(depth).map(|c| c.to_string()).as_deref(), Some("NSCalibratedRGBColorSpace"));
        let depths = NSAvailableWindowDepths();
        let mut list = Vec::new();
        while {
            let d = *depths.as_ptr().add(list.len());
            d.0 != 0
        } {
            list.push((*depths.as_ptr().add(list.len())).0);
        }
        assert_eq!(list, [0x108, 0x204, 0x208, 0x210, 0x220]);
        // Despite their names, the caller doesn't own the strings: they
        // are autoreleased, as measured on macOS. (objc2-app-kit's
        // wrappers take ownership, so the C functions are called here.)
        let (names, contents) = objc2::rc::autoreleasepool(|_| {
            let t = s("txt");
            (
                Retained::retain_autoreleased(raw::NSCreateFilenamePboardType(&t)).expect("a type"),
                Retained::retain_autoreleased(raw::NSCreateFileContentsPboardType(&t)).expect("a type"),
            )
        });
        assert_eq!(names.retainCount(), 1, "the pool released its reference");
        assert_eq!(contents.retainCount(), 1);
        assert_eq!(names.to_string(), "NSTypedFilenamesPboardType:txt");
        assert_eq!(contents.to_string(), "NXTypedFileContentsPboardType:txt");
        assert_eq!(NSGetFileType(&names).map(|t| t.to_string()).as_deref(), Some("txt"));
        assert!(NSGetFileType(&s("public.png")).is_none());
        let types = NSArray::from_retained_slice(&[contents, s("public.png"), s("NSTypedFilenamesPboardType:rtf")]);
        let files = NSGetFileTypes(&types).map(|a| a.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        assert_eq!(files, Some(vec!["rtf".to_owned(), "txt".to_owned()]));
        assert!(NSGetFileTypes(&NSArray::from_retained_slice(&[s("public.png")])).is_none());
        let mut count = -1isize;
        #[allow(deprecated)]
        NSCountWindows(NonNull::from(&mut count));
        assert_eq!(count, 0, "no window is on screen");
    }
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("the test's main runs on the main thread");
    // NSApp: nil until the application is made, which its initializer
    // sets, for a subclass too; +[NSApplication sharedApplication] is then
    // the same object.
    assert!(app_symbol().is_null());
    let app: Retained<AnyObject> = unsafe { msg_send![SweepApplication::class(), sharedApplication] };
    assert_eq!(SEEN_IN_INIT.with(Cell::get), Retained::as_ptr(&app) as usize);
    assert_eq!(app_symbol(), Retained::as_ptr(&app).cast_mut());
    let shared = NSApplication::sharedApplication(mtm);
    assert!(same(&*shared, &*app));
    shared.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    println!("test app_symbol ... ok");
    let tests: &[Test] = &[
        ("booleans", booleans),
        ("constants", constants),
        ("font_manager", font_manager),
        ("haptics", haptics),
        ("accessibility_element", accessibility_element),
        ("custom_action", custom_action),
        ("windows_menu", windows_menu),
        ("clips_to_bounds", clips_to_bounds),
        ("view_accessibility", view_accessibility),
        ("window_center", window_center),
        ("running_application", running_application),
        ("responder_hooks", responder_hooks),
        ("property_list_files", property_list_files),
        ("resource_values", resource_values),
        ("call_stack", call_stack),
        ("zones_and_pages", zones_and_pages),
        ("appkit_functions", appkit_functions),
    ];
    for (name, test) in tests {
        test(mtm);
        println!("test {name} ... ok");
    }
}
