//! `NSPopUpButton` and its cell without showing a menu: defaults, which
//! item is selected as items come and go, titles and states in pop-up and
//! pull-down mode, and the actions choosing an item sends (through the
//! menu's `performActionForItemAtIndex:`, as a click on the item would).
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::*;
use objc2_foundation::{NSArray, NSPoint, NSRect, NSRectEdge, NSSize, NSString};

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

fn sender_name(sender: &AnyObject) -> String {
    if sender.class().name().to_str().is_ok_and(|n| n.contains("PopUp")) || is_kind(sender, NSPopUpButton::class()) {
        "button".into()
    } else if is_kind(sender, NSMenuItem::class()) {
        // SAFETY: checked to be an item.
        let item = unsafe { &*(sender as *const AnyObject).cast::<NSMenuItem>() };
        format!("item {}", item.title())
    } else {
        sender.class().name().to_string_lossy().into_owned()
    }
}

fn is_kind(object: &AnyObject, class: &objc2::runtime::AnyClass) -> bool {
    // SAFETY: isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![object, isKindOfClass: class] }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformancePopUpTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(picked:))]
        fn picked(&self, sender: &AnyObject) {
            log(format!("picked: from {}", sender_name(sender)));
        }

        #[unsafe(method(own:))]
        fn own(&self, sender: &AnyObject) {
            log(format!("own: from {}", sender_name(sender)));
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

fn target(mtm: MainThreadMarker) -> Retained<Target> {
    unsafe { msg_send![super(Target::alloc(mtm).set_ivars(())), init] }
}

fn button(mtm: MainThreadMarker, pulls_down: bool) -> Retained<NSPopUpButton> {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(120.0, 24.0));
    NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, pulls_down)
}

fn titles(b: &NSPopUpButton) -> Vec<String> {
    b.itemTitles().iter().map(|t| t.to_string()).collect()
}

/// The items that are on.
fn on(b: &NSPopUpButton) -> Vec<String> {
    b.itemArray().iter().filter(|i| i.state() == 1).map(|i| i.title().to_string()).collect()
}

fn selected(b: &NSPopUpButton) -> (isize, String) {
    (b.indexOfSelectedItem(), b.titleOfSelectedItem().map(|t| t.to_string()).unwrap_or_else(|| "nil".into()))
}

fn defaults(mtm: MainThreadMarker) {
    let b = button(mtm, false);
    assert_eq!(b.numberOfItems(), 0);
    assert_eq!(b.indexOfSelectedItem(), -1);
    assert!(b.titleOfSelectedItem().is_none());
    assert!(b.selectedItem().is_none());
    assert_eq!(b.title().to_string(), "");
    assert_eq!(b.selectedTag(), -1);
    assert!(b.autoenablesItems());
    assert_eq!(b.preferredEdge(), NSRectEdge::MinY);
    assert!(b.altersStateOfSelectedItem());
    assert!(b.usesItemFromMenu());
    assert!(!b.pullsDown());
    assert!(b.menu().is_some());
    assert!(b.lastItem().is_none());
    let cell = b.cell().expect("a cell");
    assert!(is_kind(&cell, NSPopUpButtonCell::class()));
    assert!(is_kind(&cell, NSMenuItemCell::class()));
    let cell_class: &objc2::runtime::AnyClass = unsafe { msg_send![NSPopUpButton::class(), cellClass] };
    assert_eq!(cell_class.name().to_str(), Ok("NSPopUpButtonCell"));
    let pull = button(mtm, true);
    assert!(pull.pullsDown());
    assert_eq!(pull.preferredEdge(), NSRectEdge::MinY);
    let plain = NSPopUpButton::new(mtm);
    assert!(!plain.pullsDown());
    assert_eq!(unsafe { NSPopUpButtonWillPopUpNotification }.to_string(), "NSPopUpButtonWillPopUpNotification");
    assert_eq!(unsafe { NSPopUpButtonCellWillPopUpNotification }.to_string(), "NSPopUpButtonCellWillPopUpNotification");
    // A pop-up button is a push button with its arrow at the bottom.
    assert_eq!(b.bezelStyle(), NSBezelStyle::Push);
    let popup_cell = unsafe { &*(Retained::as_ptr(&cell) as *const NSPopUpButtonCell) };
    assert_eq!(popup_cell.arrowPosition(), NSPopUpArrowPosition::ArrowAtBottom);
    // A cell made with a title has an item of that title.
    for pulls_down in [false, true] {
        let c = NSPopUpButtonCell::initTextCell_pullsDown(NSPopUpButtonCell::alloc(mtm), &s("Cell"), pulls_down);
        assert_eq!(c.numberOfItems(), 1);
        assert_eq!(c.itemTitleAtIndex(0).to_string(), "Cell");
        assert_eq!(c.title().to_string(), "Cell");
        assert_eq!(c.arrowPosition(), NSPopUpArrowPosition::ArrowAtBottom);
    }
}

fn selection_in_pop_up_mode(mtm: MainThreadMarker) {
    let b = button(mtm, false);
    b.addItemWithTitle(&s("A"));
    // The first item is selected, and on.
    assert_eq!(selected(&b), (0, "A".into()));
    assert_eq!(on(&b), ["A"]);
    assert_eq!(b.title().to_string(), "A");
    b.addItemsWithTitles(&NSArray::from_retained_slice(&[s("B"), s("C")]));
    assert_eq!(titles(&b), ["A", "B", "C"]);
    assert_eq!(selected(&b), (0, "A".into()));
    b.selectItemAtIndex(2);
    assert_eq!(selected(&b), (2, "C".into()));
    assert_eq!(on(&b), ["C"]);
    assert_eq!(b.title().to_string(), "C");
    // Adding a title that's there moves it to the end.
    b.addItemWithTitle(&s("A"));
    assert_eq!(titles(&b), ["B", "C", "A"]);
    assert_eq!(selected(&b), (1, "C".into()));
    assert_eq!(on(&b), ["C"]);
    // An unknown title, or -1, selects nothing.
    b.selectItemWithTitle(&s("nope"));
    assert_eq!(selected(&b), (-1, "nil".into()));
    assert_eq!(b.title().to_string(), "");
    assert!(on(&b).is_empty());
    b.selectItemAtIndex(1);
    b.selectItemAtIndex(-1);
    assert_eq!(selected(&b), (-1, "nil".into()));
    // Inserting keeps the selected item selected.
    b.selectItemWithTitle(&s("C"));
    b.insertItemWithTitle_atIndex(&s("Z"), 0);
    assert_eq!(titles(&b), ["Z", "B", "C", "A"]);
    assert_eq!(selected(&b), (2, "C".into()));
    // setTitle: selects an item with the title, or adds one.
    b.setTitle(&s("B"));
    assert_eq!(selected(&b), (1, "B".into()));
    b.setTitle(&s("New"));
    assert_eq!(titles(&b), ["Z", "B", "C", "A", "New"]);
    assert_eq!(selected(&b), (4, "New".into()));
    assert_eq!(on(&b), ["New"]);
    // Removing the selected item selects the first.
    b.removeItemAtIndex(4);
    assert_eq!(selected(&b), (0, "Z".into()));
    b.removeItemWithTitle(&s("B"));
    assert_eq!(titles(&b), ["Z", "C", "A"]);
    // Tags.
    b.itemAtIndex(2).unwrap().setTag(7);
    assert!(b.selectItemWithTag(7));
    assert_eq!(selected(&b), (2, "A".into()));
    assert_eq!(b.selectedTag(), 7);
    assert!(!b.selectItemWithTag(99));
    assert_eq!(selected(&b), (2, "A".into()));
    assert_eq!(b.indexOfItemWithTag(7), 2);
    assert_eq!(b.indexOfItemWithTitle(&s("C")), 1);
    assert_eq!(b.itemTitleAtIndex(1).to_string(), "C");
    assert_eq!(b.lastItem().unwrap().title().to_string(), "A");
    let c = b.itemWithTitle(&s("C")).unwrap();
    assert_eq!(b.indexOfItem(&c), 1);
    b.selectItem(Some(&c));
    assert_eq!(selected(&b), (1, "C".into()));
    b.selectItem(None);
    assert_eq!(selected(&b), (-1, "nil".into()));
    b.removeAllItems();
    assert_eq!(b.numberOfItems(), 0);
    assert_eq!(selected(&b), (-1, "nil".into()));
    // Without altering states, the selection leaves them alone.
    b.setAltersStateOfSelectedItem(false);
    b.addItemWithTitle(&s("P"));
    b.addItemWithTitle(&s("Q"));
    assert_eq!(selected(&b), (0, "P".into()));
    assert!(on(&b).is_empty());
    b.selectItemAtIndex(1);
    assert!(on(&b).is_empty());
    // A batch of titles is taken title by title: one there moves to the
    // end, even one the batch added.
    let d = button(mtm, false);
    d.addItemsWithTitles(&NSArray::from_retained_slice(&[s("A"), s("B"), s("C")]));
    d.addItemsWithTitles(&NSArray::from_retained_slice(&[s("B"), s("D"), s("D"), s("A")]));
    assert_eq!(titles(&d), ["C", "B", "D", "A"]);
}

fn pull_downs(mtm: MainThreadMarker) {
    let b = button(mtm, true);
    b.addItemsWithTitles(&NSArray::from_retained_slice(&[s("Title"), s("One"), s("Two")]));
    // Nothing is selected by itself; the title is the first item's, and
    // selecting changes neither it nor the states.
    assert_eq!(selected(&b), (-1, "nil".into()));
    assert_eq!(b.title().to_string(), "Title");
    assert!(on(&b).is_empty());
    b.selectItemAtIndex(2);
    assert_eq!(b.indexOfSelectedItem(), 2);
    assert_eq!(b.title().to_string(), "Title");
    assert!(on(&b).is_empty());
    // setTitle: renames the first item.
    b.setTitle(&s("Renamed"));
    assert_eq!(titles(&b), ["Renamed", "One", "Two"]);
    assert_eq!(b.title().to_string(), "Renamed");
    assert_eq!(selected(&b), (2, "Two".into()));
}

fn choosing_sends_the_action(mtm: MainThreadMarker) {
    let t = target(mtm);
    let b = button(mtm, false);
    unsafe {
        b.setTarget(Some(&t));
        b.setAction(Some(sel!(picked:)));
    }
    b.addItemsWithTitles(&NSArray::from_retained_slice(&[s("A"), s("B"), s("C")]));
    let menu = b.menu().unwrap();
    // The button's items act through the button, with an action of the
    // cell's.
    let item = b.itemAtIndex(1).unwrap();
    assert!(item.action().is_some() && item.target().is_some());
    take_log();
    menu.performActionForItemAtIndex(1);
    assert_eq!(take_log(), ["picked: from button"]);
    assert_eq!(selected(&b), (1, "B".into()));
    assert_eq!(on(&b), ["B"]);
    // An item with its own target and action sends that, and is selected.
    let c = b.itemAtIndex(2).unwrap();
    unsafe {
        c.setTarget(Some(&t));
        c.setAction(Some(sel!(own:)));
    }
    menu.performActionForItemAtIndex(2);
    assert_eq!(take_log(), ["own: from item C"]);
    assert_eq!(selected(&b), (2, "C".into()));
    // Selecting in code sends nothing.
    b.selectItemAtIndex(0);
    assert!(take_log().is_empty());
    // Validation brings back items set disabled.
    let a = b.itemAtIndex(0).unwrap();
    a.setEnabled(false);
    menu.update();
    assert!(a.isEnabled());
    // The button's autoenabling is its menu's.
    b.setAutoenablesItems(false);
    assert!(!menu.autoenablesItems());
    a.setEnabled(false);
    menu.update();
    assert!(!a.isEnabled());
}

fn the_menu_is_the_cells(mtm: MainThreadMarker) {
    let b = button(mtm, false);
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &s("Mine"));
    unsafe { menu.addItemWithTitle_action_keyEquivalent(&s("X"), None, &s("")) };
    unsafe { menu.addItemWithTitle_action_keyEquivalent(&s("Y"), None, &s("")) };
    b.setMenu(Some(&menu));
    assert!(b.menu().is_some_and(|m| std::ptr::eq(&*m, &*menu)));
    // The first item is selected.
    assert_eq!(selected(&b), (0, "X".into()));
    assert_eq!(on(&b), ["X"]);
    assert_eq!(b.title().to_string(), "X");
    // Items without an action, there or added later, act through the
    // button; items with one keep it.
    let x_action = menu.itemAtIndex(0).unwrap().action();
    assert!(x_action.is_some());
    let z = unsafe { menu.addItemWithTitle_action_keyEquivalent(&s("Z"), None, &s("")) };
    assert_eq!(z.action(), x_action);
    let own = unsafe { menu.addItemWithTitle_action_keyEquivalent(&s("Own"), Some(sel!(own:)), &s("")) };
    assert_eq!(own.action(), Some(sel!(own:)));
    assert!(own.target().is_none());
    menu.removeItem(&own);
    assert_eq!(titles(&b), ["X", "Y", "Z"]);
    let cell = b.cell().unwrap();
    let cell: &NSPopUpButtonCell = unsafe { &*(Retained::as_ptr(&cell) as *const NSPopUpButtonCell) };
    assert!(cell.menu().is_some_and(|m| std::ptr::eq(&*m, &*menu)));
    b.selectItemAtIndex(1);
    take_log();
    // The cell's menu item is the selected item.
    assert!(cell.menuItem().is_some_and(|i| i.title().to_string() == "Y"));
    menu.performActionForItemAtIndex(2);
    assert_eq!(selected(&b), (2, "Z".into()));
    assert!(take_log().is_empty());
    let t = target(mtm);
    unsafe {
        b.setTarget(Some(&t));
        b.setAction(Some(sel!(picked:)));
    }
    menu.performActionForItemAtIndex(0);
    assert_eq!(selected(&b), (0, "X".into()));
    assert_eq!(take_log(), ["picked: from button"]);
    cell.synchronizeTitleAndSelectedItem();
    assert_eq!(b.title().to_string(), "X");
}

fn text_width(text: &str) -> f64 {
    let font = NSFont::systemFontOfSize(0.0);
    let key = unsafe { NSFontAttributeName };
    let attrs = objc2_foundation::NSDictionary::from_slices(&[key], &[&*font as &AnyObject]);
    unsafe { s(text).sizeWithAttributes(Some(&attrs)) }.width
}

/// Sizes follow the titles: the widest item's (the first's, pulling
/// down) plus a margin by control size, as tall as a push button.
fn geometry(mtm: MainThreadMarker) {
    let widths = |items: &[&str]| items.iter().map(|i| text_width(i).ceil()).fold(0.0, f64::max);
    for (pulls, items) in [
        (false, vec!["A"]),
        (false, vec!["Alpha", "Beta gamma delta"]),
        (false, vec!["Beta gamma delta", "Alpha"]),
        (true, vec!["Alpha", "Beta gamma delta"]),
    ] {
        let b = button(mtm, pulls);
        for i in &items {
            b.addItemWithTitle(&s(i));
        }
        let title = if pulls { widths(&items[..1]) } else { widths(&items) };
        assert_eq!(b.intrinsicContentSize(), NSSize::new(title + 48.0, 24.0), "{items:?}");
        let cell = b.cell().unwrap();
        assert_eq!(cell.cellSize(), NSSize::new(title + 48.0, 24.0));
        // The title where the shown item's goes: 12 points in, centered.
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 25.0));
        let r = cell.titleRectForBounds(bounds);
        assert_eq!(r.origin.x, 12.0);
        assert_eq!(r.size.width, widths(&items[..1]));
        assert_eq!(r.origin.y, ((25.0 - r.size.height) / 2.0 + 0.5).floor());
        b.sizeToFit();
        assert_eq!(b.frame().size, NSSize::new(title + 48.0, 24.0));
    }
    for (size, extra, height, x) in [
        (NSControlSize::Small, 40.0, 20.0, 10.0),
        (NSControlSize::Mini, 32.0, 16.0, 8.0),
        (NSControlSize::Large, 56.0, 28.0, 14.0),
    ] {
        let b = button(mtm, false);
        b.addItemWithTitle(&s("Alpha"));
        b.setControlSize(size);
        // The font stays the regular one.
        assert_eq!(b.intrinsicContentSize(), NSSize::new(widths(&["Alpha"]) + extra, height), "{size:?}");
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 30.0));
        assert_eq!(b.cell().unwrap().titleRectForBounds(bounds).origin.x, x);
    }
    // The menu shows in the menu font, as wide as it needs.
    let pb = button(mtm, false);
    pb.addItemWithTitle(&s("Alpha"));
    let menu = pb.menu().unwrap();
    assert_eq!(menu.font().map(|f| f.pointSize()), Some(NSFont::menuFontOfSize(0.0).pointSize()));
    assert_eq!(menu.minimumWidth(), 0.0);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let tests: &[Test] = &[
        ("defaults", defaults),
        ("selection_in_pop_up_mode", selection_in_pop_up_mode),
        ("pull_downs", pull_downs),
        ("choosing_sends_the_action", choosing_sends_the_action),
        ("the_menu_is_the_cells", the_menu_is_the_cells),
        ("geometry", geometry),
    ];
    // The sizes `geometry` pins were measured on macOS 26 with a 2x screen;
    // on a 1x screen (CI's macOS runner) AppKit may round them otherwise,
    // so there a difference is printed, not failed (as in controls.rs).
    #[cfg(target_vendor = "apple")]
    let scale = NSScreen::mainScreen(mtm).map_or(1.0, |s| s.backingScaleFactor());
    #[cfg(not(target_vendor = "apple"))]
    let scale = 2.0;
    let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
    for (name, test) in tests {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        if scale != 2.0 && *name == "geometry" {
            let run = std::panic::AssertUnwindSafe(|| objc2::rc::autoreleasepool(|_| test(mtm)));
            match std::panic::catch_unwind(run) {
                Ok(()) => println!("test {name} ... ok"),
                Err(_) => println!("test {name} ... differs on a {scale}x screen (see above), not failed"),
            }
            continue;
        }
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
