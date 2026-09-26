//! NSTabView and NSTabViewItem, checked on macOS and on Linux alike: their
//! defaults, the content rectangle of each type, adding, inserting and
//! removing items, selecting them (by item, index, identifier and
//! neighbour) with the delegate asked and told in AppKit's order, the
//! selected item's view shown in the content rectangle, where the tabs
//! take clicks, items that outlive their tab view or move to another, and
//! a delegate that removes items while one is removed. On Linux, clicking
//! a tab too.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::DefinedClass;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSControlSize, NSFont, NSTabPosition, NSTabState, NSTabView, NSTabViewBorderType, NSTabViewDelegate, NSTabViewItem,
    NSTabViewType, NSView,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

fn label(item: Option<&NSTabViewItem>) -> String {
    item.map(|i| i.label().to_string()).unwrap_or_else(|| "nil".into())
}

define_class!(
    /// Logs what the tab view asks and tells; refuses items labelled "no".
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTabDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSTabViewDelegate for Delegate {
        #[unsafe(method(tabView:shouldSelectTabViewItem:))]
        fn should_select(&self, tab: &NSTabView, item: Option<&NSTabViewItem>) -> bool {
            log(format!("should {} from {}", label(item), label(tab.selectedTabViewItem().as_deref())));
            label(item) != "no"
        }

        #[unsafe(method(tabView:willSelectTabViewItem:))]
        fn will_select(&self, tab: &NSTabView, item: Option<&NSTabViewItem>) {
            log(format!("will {} from {}", label(item), label(tab.selectedTabViewItem().as_deref())));
        }

        #[unsafe(method(tabView:didSelectTabViewItem:))]
        fn did_select(&self, tab: &NSTabView, item: Option<&NSTabViewItem>) {
            log(format!("did {} now {}", label(item), label(tab.selectedTabViewItem().as_deref())));
        }

        #[unsafe(method(tabViewDidChangeNumberOfTabViewItems:))]
        fn count(&self, tab: &NSTabView) {
            log(format!("count {}", tab.numberOfTabViewItems()));
        }
    }
);

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn item(name: &str) -> Retained<NSTabViewItem> {
    let id = NSString::from_str(name);
    // SAFETY: the identifier is a string, as any object may be.
    let item = unsafe { NSTabViewItem::initWithIdentifier(NSTabViewItem::alloc(), Some(&id)) };
    item.setLabel(&NSString::from_str(name));
    item
}

fn tab_view(mtm: MainThreadMarker) -> Retained<NSTabView> {
    NSTabView::initWithFrame(NSTabView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0))
}

fn selected(t: &NSTabView) -> String {
    label(t.selectedTabViewItem().as_deref())
}

fn defaults(mtm: MainThreadMarker) {
    let t = tab_view(mtm);
    assert_eq!(t.tabViewType(), NSTabViewType::TopTabsBezelBorder);
    assert_eq!(t.tabPosition(), NSTabPosition::Top);
    assert_eq!(t.tabViewBorderType(), NSTabViewBorderType::Bezel);
    assert!(t.drawsBackground() && t.allowsTruncatedLabels() && t.isFlipped());
    assert_eq!(t.controlSize(), NSControlSize::Regular);
    assert_eq!(t.numberOfTabViewItems(), 0);
    assert!(t.selectedTabViewItem().is_none() && t.delegate().is_none());
    assert_eq!(t.font().pointSize(), NSFont::systemFontSize());

    let item = NSTabViewItem::new();
    assert_eq!(item.label().to_string(), "");
    assert!(item.identifier().is_none() && item.toolTip().is_none() && item.tabView(mtm).is_none());
    assert_eq!(item.tabState(), NSTabState::BackgroundTab);
    // It has an empty view to begin with.
    assert_eq!(item.view(mtm).expect("a view").frame(), NSRect::ZERO);
}

fn content_rects(mtm: MainThreadMarker) {
    let t = tab_view(mtm);
    let content = |kind| {
        t.setTabViewType(kind);
        t.contentRect()
    };
    // Room for the tabs, on their side, and the bezel.
    assert_eq!(content(NSTabViewType::TopTabsBezelBorder), rect(10.0, 33.0, 380.0, 254.0));
    assert_eq!(content(NSTabViewType::LeftTabsBezelBorder), rect(32.0, 7.0, 358.0, 280.0));
    assert_eq!(content(NSTabViewType::BottomTabsBezelBorder), rect(10.0, 7.0, 380.0, 262.0));
    assert_eq!(content(NSTabViewType::RightTabsBezelBorder), rect(10.0, 7.0, 359.0, 280.0));
    assert_eq!(content(NSTabViewType::NoTabsBezelBorder), rect(10.0, 7.0, 380.0, 280.0));
    assert_eq!(content(NSTabViewType::NoTabsLineBorder), rect(1.0, 1.0, 398.0, 298.0));
    assert_eq!(content(NSTabViewType::NoTabsNoBorder), rect(0.0, 0.0, 400.0, 300.0));
    // The type is a position and a border.
    let parts = |kind| {
        t.setTabViewType(kind);
        (t.tabPosition(), t.tabViewBorderType())
    };
    assert_eq!(parts(NSTabViewType::LeftTabsBezelBorder), (NSTabPosition::Left, NSTabViewBorderType::Bezel));
    assert_eq!(parts(NSTabViewType::NoTabsLineBorder), (NSTabPosition::None, NSTabViewBorderType::Line));
    assert_eq!(parts(NSTabViewType::NoTabsNoBorder), (NSTabPosition::None, NSTabViewBorderType::None));
}

fn selecting(mtm: MainThreadMarker) {
    let t = tab_view(mtm);
    // SAFETY: the class's initializer.
    let d: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    t.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    let (a, b, no) = (item("a"), item("b"), item("no"));
    let va = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    a.setView(Some(&va));
    // The first item added is selected, and its view shown.
    t.addTabViewItem(&a);
    assert_eq!(take(), ["should a from nil", "will a from nil", "did a now a", "count 1"]);
    assert_eq!(a.tabState(), NSTabState::SelectedTab);
    assert!(a.tabView(mtm).is_some_and(|x| std::ptr::eq(&*x, &*t)));
    // SAFETY: the superview is alive while the view is in it.
    assert!(unsafe { va.superview() }.is_some_and(|s| std::ptr::eq(&*s, &**t)));
    assert_eq!(va.frame(), t.contentRect());
    t.addTabViewItem(&b);
    t.addTabViewItem(&no);
    assert_eq!(take(), ["count 2", "count 3"]);
    assert_eq!((b.tabState(), selected(&t)), (NSTabState::BackgroundTab, "a".into()));

    t.selectTabViewItemAtIndex(1);
    assert_eq!(take(), ["should b from a", "will b from a", "did b now b"]);
    assert_eq!((a.tabState(), b.tabState()), (NSTabState::BackgroundTab, NSTabState::SelectedTab));
    // SAFETY: the superview is alive while the view is in it.
    assert!(unsafe { va.superview() }.is_none(), "the old view leaves");
    // Refused, or already selected: nothing changes.
    t.selectTabViewItemAtIndex(2);
    assert_eq!(take(), ["should no from b"]);
    t.selectTabViewItem(Some(&b));
    assert_eq!(take(), Vec::<String>::new());
    assert_eq!(selected(&t), "b");
    // By neighbour and by identifier.
    // SAFETY: the sender may be nil.
    unsafe { t.selectPreviousTabViewItem(None) };
    assert_eq!(selected(&t), "a");
    // SAFETY: the sender may be nil.
    unsafe { t.selectNextTabViewItem(None) };
    assert_eq!(selected(&t), "b");
    // SAFETY: the sender may be nil.
    unsafe { t.selectFirstTabViewItem(None) };
    assert_eq!(selected(&t), "a");
    let id = NSString::from_str("b");
    // SAFETY: the identifier is a string, as any object may be.
    assert_eq!((t.indexOfTabViewItem(&b), unsafe { t.indexOfTabViewItemWithIdentifier(&id) }), (1, 1));
    // SAFETY: the identifier is a string, as any object may be.
    assert_eq!(unsafe { t.indexOfTabViewItemWithIdentifier(&NSString::from_str("zz")) }, isize::MAX);
    // SAFETY: the identifier is a string, as any object may be.
    unsafe { t.selectTabViewItemWithIdentifier(&id) };
    assert_eq!(selected(&t), "b");
    t.selectTabViewItem(None);
    assert_eq!(selected(&t), "b");
    take();
    t.setDelegate(None);
}

fn showing_views(mtm: MainThreadMarker) {
    let t = tab_view(mtm);
    let (a, b) = (item("a"), item("b"));
    let va = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    a.setView(Some(&va));
    t.addTabViewItem(&a);
    t.addTabViewItem(&b);
    // Only the selected item's view is placed.
    assert_eq!(b.view(mtm).unwrap().frame(), NSRect::ZERO);
    t.setFrameSize(NSSize::new(500.0, 400.0));
    assert_eq!(t.contentRect(), rect(10.0, 33.0, 480.0, 354.0));
    assert_eq!(va.frame(), t.contentRect());
    t.setTabViewType(NSTabViewType::NoTabsNoBorder);
    assert_eq!(va.frame(), rect(0.0, 0.0, 500.0, 400.0));
    // Nothing is under the content.
    assert!(t.tabViewItemAtPoint(NSPoint::new(250.0, 200.0)).is_none());
}

fn adding_and_removing(mtm: MainThreadMarker) {
    let t = tab_view(mtm);
    // SAFETY: the class's initializer.
    let d: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    t.setDelegate(Some(ProtocolObject::from_ref(&*d)));
    let (a, b, c, e) = (item("a"), item("b"), item("c"), item("e"));
    for i in [&a, &b, &c] {
        t.addTabViewItem(i);
    }
    t.selectTabViewItem(Some(&b));
    take();
    t.insertTabViewItem_atIndex(&e, 0);
    assert_eq!(take(), ["count 4"]);
    assert_eq!((t.indexOfTabViewItem(&b), selected(&t)), (2, "b".into()));
    // Removing the selected item selects its neighbour first.
    t.removeTabViewItem(&b);
    assert_eq!(take(), ["should a from b", "will a from b", "did a now a", "count 3"]);
    assert!(b.tabView(mtm).is_none());
    assert_eq!(b.tabState(), NSTabState::BackgroundTab);
    t.removeTabViewItem(&c);
    assert_eq!(take(), ["count 2"]);
    let labels = |t: &NSTabView| t.tabViewItems().iter().map(|i| i.label().to_string()).collect::<Vec<_>>();
    assert_eq!(labels(&t), ["e", "a"]);
    // Replacing them all.
    t.setTabViewItems(&NSArray::from_retained_slice(&[item("p"), item("q")]));
    assert_eq!(labels(&t), ["p", "q"]);
    assert_eq!(selected(&t), "p");
    take();
    t.setDelegate(None);
}

/// Clicking a tab, through the window.
#[cfg(not(target_vendor = "apple"))]
fn clicking(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSBackingStoreType, NSEvent, NSEventModifierFlags, NSEventType, NSWindow, NSWindowStyleMask};
    // SAFETY: a plain window, never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    let t = tab_view(mtm);
    w.setContentView(Some(&t));
    let (a, b) = (item("a"), item("b"));
    t.addTabViewItem(&a);
    t.addTabViewItem(&b);
    // The middle of the second tab (its row is 28 to 4 points above the
    // content), in window coordinates (y up).
    let tab = (0..t.bounds().size.width as i32)
        .map(|x| NSPoint::new(x as f64, 33.0 - 16.0))
        .find(|p| t.tabViewItemAtPoint(*p).is_some_and(|i| std::ptr::eq(&*i, &*b)))
        .expect("b's tab");
    let click = |kind| {
        let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            NSPoint::new(tab.x, 300.0 - tab.y),
            NSEventModifierFlags::empty(),
            0.0,
            w.windowNumber(),
            None,
            0,
            1,
            1.0,
        )
        .expect("an event");
        w.sendEvent(&event);
    };
    click(NSEventType::LeftMouseDown);
    click(NSEventType::LeftMouseUp);
    assert_eq!(selected(&t), "b");
    w.setContentView(None);
}

fn tab_rects(mtm: MainThreadMarker) {
    // Two one-letter tabs, top and bottom: 24 points deep, ending 4 points
    // above the content on top and starting 3 below it at the bottom. The
    // points are clear of the labels' widths, which depend on the font.
    let t = tab_view(mtm);
    t.setFrame(rect(0.0, 0.0, 400.0, 300.0));
    t.addTabViewItem(&item("a"));
    t.addTabViewItem(&item("b"));
    let at = |x, y| label(t.tabViewItemAtPoint(NSPoint::new(x, y)).as_deref());
    assert_eq!(t.contentRect().origin.y, 33.0);
    assert_eq!([at(183.0, 4.5), at(183.0, 5.5), at(183.0, 28.5), at(183.0, 29.5)], ["nil", "a", "a", "nil"]);
    assert_eq!([at(217.0, 6.0), at(150.0, 6.0), at(250.0, 6.0)], ["b", "nil", "nil"]);
    t.setTabViewType(NSTabViewType::BottomTabsBezelBorder);
    let bottom = t.contentRect().origin.y + t.contentRect().size.height;
    assert_eq!(bottom, 269.0);
    assert_eq!([at(183.0, 271.5), at(183.0, 272.5), at(183.0, 295.5), at(183.0, 296.5)], ["nil", "a", "a", "nil"]);
}

define_class!(
    /// Removes another item when told of a selection.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceTabRemover"]
    #[ivars = RefCell<Option<Retained<NSTabViewItem>>>]
    struct Remover;

    unsafe impl NSObjectProtocol for Remover {}

    unsafe impl NSTabViewDelegate for Remover {
        #[unsafe(method(tabView:didSelectTabViewItem:))]
        fn did_select(&self, tab: &NSTabView, item: Option<&NSTabViewItem>) {
            log(format!("did {}", label(item)));
            if let Some(victim) = self.ivars().take() {
                tab.removeTabViewItem(&victim);
            }
        }
    }
);

fn items_come_and_go(mtm: MainThreadMarker) {
    // An item that outlives its tab view has none.
    let a = item("a");
    objc2::rc::autoreleasepool(|_| tab_view(mtm).addTabViewItem(&a));
    assert!(a.tabView(mtm).is_none());
    a.setLabel(&NSString::from_str("z"));
    let _ = a.tabState();

    // One tab view at a time: added to another, it leaves the first.
    let (t1, t2) = (tab_view(mtm), tab_view(mtm));
    let b = item("b");
    t1.addTabViewItem(&b);
    t2.addTabViewItem(&b);
    assert_eq!((t1.numberOfTabViewItems(), t2.numberOfTabViewItems()), (0, 1));
    assert!(b.tabView(mtm).is_some_and(|t| std::ptr::eq(&*t, &*t2)));

    // A delegate removing an item while the selected one is removed.
    let t = tab_view(mtm);
    let items: Vec<Retained<NSTabViewItem>> = ["t0", "t1", "t2"].iter().map(|n| item(n)).collect();
    for i in &items {
        t.addTabViewItem(i);
    }
    t.selectTabViewItemAtIndex(2);
    let this = Remover::alloc(mtm).set_ivars(RefCell::new(Some(items[0].clone())));
    // SAFETY: the superclass's designated initializer.
    let remover: Retained<Remover> = unsafe { msg_send![super(this), init] };
    t.setDelegate(Some(ProtocolObject::from_ref(&*remover)));
    take();
    t.removeTabViewItem(&items[2]);
    assert_eq!(take(), ["did t1"]);
    let left: Vec<String> = t.tabViewItems().iter().map(|i| i.label().to_string()).collect();
    assert_eq!((left, selected(&t)), (vec!["t1".to_owned()], "t1".to_owned()));
    assert!(items[0].tabView(mtm).is_none() && items[2].tabView(mtm).is_none());
    t.setDelegate(None);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    #[allow(unused_mut)] // Linux adds a test.
    let mut tests: Vec<Test> = vec![
        ("defaults", defaults),
        ("content_rects", content_rects),
        ("selecting", selecting),
        ("showing_views", showing_views),
        ("adding_and_removing", adding_and_removing),
        ("tab_rects", tab_rects),
        ("items_come_and_go", items_come_and_go),
    ];
    #[cfg(not(target_vendor = "apple"))]
    tests.push(("clicking", clicking));
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        take();
        println!("test {name} ... ok");
    }
}
