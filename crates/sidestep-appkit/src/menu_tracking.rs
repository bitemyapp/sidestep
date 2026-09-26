//! Menus on screen: pop-up and context menus, submenus, and the loop that
//! tracks them while they show.
//!
//! A menu opens as a grabbing xdg_popup of the window it belongs to (see
//! `menu_view` for its window and view), placed through the positioner:
//! against a point for pop-up and context menus (so the item asked for
//! sits at the point, the menu sliding along the output's edges, flipping
//! up or shrinking where it doesn't fit), against the row of the item it
//! comes from for a submenu (to its right, flipping to the left), below
//! its title for the menu bar. Wayland doesn't say where windows are, so a
//! point given in screen coordinates (no view) is taken as the pointer's
//! place in the key window.
//!
//! Before the outermost menu shows, in this order (AppKit's, as
//! `conformance/tests/menus.rs` pins it): its delegate's `menuNeedsUpdate:`
//! (with `numberOfItemsInMenu:` and `menu:updateItem:atIndex:shouldCancel:`
//! when the delegate has them), `NSMenuDidBeginTrackingNotification`,
//! `menuWillOpen:`, the view's `willOpenMenu:withEvent:`, and `update`
//! (validation); a submenu has the same without the notification and the
//! view. Then, as in AppKit, the call runs a loop of its own in
//! `NSEventTrackingRunLoopMode` (`nextEventMatchingMask:…`, so timers of
//! that mode and the common modes fire) until the menu closes:
//!
//! - the pointer highlights enabled items (the delegate hears
//!   `menu:willHighlightItem:`); resting 150 ms on an item with a submenu
//!   opens it, and moving to another item closes an open one 150 ms later
//!   unless the pointer reaches it first;
//! - releasing a button on an item chooses it, both after pressing,
//!   dragging and releasing, and after a click that opened the menu and a
//!   click on the item; a press outside the menus closes them, and so does
//!   a release outside after the pointer went through them;
//! - Up and Down move the highlight, Right and Left open and close
//!   submenus, Return, Enter and Space choose, Escape closes, and a
//!   letter highlights the next item starting with it;
//! - `cancelTracking`, the compositor dismissing the popups (a click in
//!   another program), or the window they belong to leaving the screen
//!   closes them (the popups first: one can't outlive its parent).
//!
//! On closing, in this order: `menu:willHighlightItem:` nil for each menu
//! that had an item highlighted, then `menuDidClose:`, both innermost menu
//! first, the view's `didCloseMenu:withEvent:`,
//! `NSMenuDidEndTrackingNotification`, and, if an item was chosen, its
//! action (`menu::send_action`, between the will-send and did-send
//! notifications). Then the call returns.
//!
//! A menu may change while it shows (a timer, the delegate): the loop
//! hears of it (`menu_changed`), lays it out again, keeps the highlight on
//! the same item, and shows it again at its new size if that changed. The
//! loop keeps items, not rows, for what it will do later, so a row that
//! moved or went away is never taken for another.
//!
//! One menu tracks at a time; asking for another meanwhile opens nothing.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, Message, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventTrackingRunLoopMode, NSEventType, NSFont, NSMenu,
    NSMenuItem, NSView, NSWindow,
};
use objc2_foundation::{NSDate, NSPoint, NSRect, NSSize};

use crate::menu::{self, item_ivars, menu_ivars, note};
use crate::menu_view::{self, EDGE, MenuView};
use crate::protocol::{Corner, PopupLayout};

/// How long the pointer rests before a submenu opens or closes.
const SUBMENU_DELAY: Duration = Duration::from_millis(150);
/// A release this soon after a press opened the menu leaves it open, for
/// a second click to choose.
const CLICK: Duration = Duration::from_millis(400);
/// The application-defined event that wakes the loop (its subtype and
/// first datum).
const WAKE_SUBTYPE: i16 = 0x5344;
const WAKE_DATA: isize = 0x4d45_4e55;

thread_local! {
    /// A loop is running.
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    /// The menus it shows and their windows, outermost first, for
    /// `cancelTracking`, changes and windows leaving.
    static OPEN: RefCell<Vec<(Retained<NSMenu>, Retained<NSWindow>)>> = const { RefCell::new(Vec::new()) };
    /// The window the outermost menu belongs to.
    static ROOT: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static CANCELLED: Cell<bool> = const { Cell::new(false) };
    /// A menu showing changed since the loop last looked.
    static CHANGED: Cell<bool> = const { Cell::new(false) };
    /// The loop is ordering its own windows out.
    static CLOSING: Cell<bool> = const { Cell::new(false) };
    /// A wake-up event waits in the queue.
    static WOKEN: Cell<bool> = const { Cell::new(false) };
}

/// Where and how a menu opens.
pub(crate) struct Opening {
    /// The window it opens over.
    pub parent: Retained<NSWindow>,
    /// A rectangle in the parent's window coordinates to open against.
    pub anchor: NSRect,
    pub layout: PopupLayout,
    /// Put this item at the anchor's top left, and highlight it.
    pub positioning: Option<Retained<NSMenuItem>>,
    /// The least width (a pop-up button's).
    pub min_width: f64,
    /// A pop-up button's menu: the positioning item's middle goes at the
    /// anchor, and the titles this far right of it (the button's title
    /// inset), the menu growing by what that takes.
    pub title_at: Option<f64>,
    /// A mouse press opened it: its release may choose.
    pub pressed: bool,
    /// The view told `willOpenMenu:withEvent:` and `didCloseMenu:withEvent:`,
    /// and the event it is told of (else the application's current one).
    pub view: Option<Retained<NSView>>,
    pub event: Option<Retained<NSEvent>>,
}

/// The menu bar a menu opened from (see `menubar`): its window, where the
/// content ends (the bar is above), the titles' spans of x, and the title
/// open.
pub(crate) struct BarLink {
    pub window: Retained<NSWindow>,
    pub content: f64,
    pub spans: Vec<(f64, f64)>,
    pub current: usize,
}

/// A point anchor's placement: the menu's top left at the point, giving
/// way at the edges.
pub(crate) fn at_point() -> PopupLayout {
    PopupLayout {
        corner: Corner::TopLeft,
        gravity: Corner::BottomRight,
        offset: (0, 0),
        flip_x: false,
        flip_y: true,
        slide_x: true,
        slide_y: true,
        resize_y: true,
    }
}

/// A point in window coordinates as an anchor whose top left it is.
pub(crate) fn point(at: NSPoint) -> NSRect {
    NSRect::new(NSPoint::new(at.x, at.y - 1.0), NSSize::new(1.0, 1.0))
}

/// Below the anchor, as the menu bar's menus and pull-downs open.
pub(crate) fn below() -> PopupLayout {
    PopupLayout { corner: Corner::BottomLeft, ..at_point() }
}

/// `+[NSMenu popUpContextMenu:withEvent:forView:]`: at the event's place in
/// its window.
pub(crate) fn pop_up_context_menu(menu: &NSMenu, event: &NSEvent, view: &NSView, _font: Option<&NSFont>) {
    let mtm = MainThreadMarker::from(menu);
    let Some(parent) = event.window(mtm).or_else(|| view.window()) else { return };
    let at = event.locationInWindow();
    let pressed = matches!(
        event.r#type(),
        NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown
    );
    let opening = Opening {
        parent,
        anchor: point(at),
        layout: at_point(),
        positioning: None,
        min_width: 0.0,
        title_at: None,
        pressed,
        view: Some(view.retain()),
        event: Some(event.retain()),
    };
    track(menu, opening);
}

/// `-[NSMenu popUpMenuPositioningItem:atLocation:inView:]`: YES if an item
/// was chosen.
pub(crate) fn pop_up_positioning(
    menu: &NSMenu,
    item: Option<&NSMenuItem>,
    location: NSPoint,
    view: Option<&NSView>,
) -> bool {
    pop_up_with(menu, item, location, view, 0.0).is_some()
}

/// As `popUpMenuPositioningItem:atLocation:inView:`, the menu at least
/// `min_width` wide; the item chosen, if one was.
pub(crate) fn pop_up_with(
    menu: &NSMenu,
    item: Option<&NSMenuItem>,
    location: NSPoint,
    view: Option<&NSView>,
    min_width: f64,
) -> Option<Retained<NSMenuItem>> {
    let mtm = MainThreadMarker::from(menu);
    let (parent, at) = match view {
        Some(v) => (v.window()?, v.convertPoint_toView(location, None)),
        None => {
            // Screen coordinates, which Wayland doesn't have: the pointer
            // in the key window.
            let app = NSApplication::sharedApplication(mtm);
            let w = app.keyWindow().or_else(|| app.mainWindow())?;
            let at = crate::window::imp(&w).pointer().unwrap_or(NSPoint::ZERO);
            (w, at)
        }
    };
    let pressed = NSApplication::sharedApplication(mtm).currentEvent().is_some_and(|e| {
        matches!(e.r#type(), NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown)
    });
    let opening = Opening {
        parent,
        anchor: point(at),
        layout: at_point(),
        positioning: item.map(|i| i.retain()),
        min_width,
        title_at: None,
        pressed,
        view: view.map(|v| v.retain()),
        event: None,
    };
    track(menu, opening)
}

/// `cancelTracking`: close the menus if `menu` is one of them.
pub(crate) fn cancel(menu: &NSMenu) {
    let open = OPEN.with(|o| o.borrow().iter().any(|(m, _)| std::ptr::eq(&**m, menu)));
    if open {
        CANCELLED.with(|c| c.set(true));
        wake();
    }
}

/// A menu changed (see `menu::bump`): if it shows, the loop lays it out
/// again.
pub(crate) fn menu_changed(menu: &NSMenu) {
    if !TRACKING.with(Cell::get) {
        return;
    }
    let open = OPEN.with(|o| o.borrow().iter().any(|(m, _)| std::ptr::eq(&**m, menu)));
    if open {
        CHANGED.with(|c| c.set(true));
        wake();
    }
}

/// A menu window left the screen: unless the loop closed it, the
/// compositor dismissed the menus.
pub(crate) fn window_left(_window: &NSWindow) {
    if TRACKING.with(Cell::get) && !CLOSING.with(Cell::get) {
        CANCELLED.with(|c| c.set(true));
        wake();
    }
}

/// `window` is leaving the screen (see `window`'s `order_out`): if the
/// menus belong to it, or it is a menu with submenus open, those close
/// first, innermost first (a popup can't outlive its parent), and the loop
/// ends.
pub(crate) fn window_leaving(window: &NSWindow) {
    if !TRACKING.with(Cell::get) || CLOSING.with(Cell::get) {
        return;
    }
    let root = ROOT.with(|r| r.borrow().as_ref().is_some_and(|r| std::ptr::eq(&**r, window)));
    let open: Vec<Retained<NSWindow>> = OPEN.with(|o| o.borrow().iter().map(|(_, w)| w.clone()).collect());
    let from = if root { Some(0) } else { open.iter().position(|w| std::ptr::eq(&**w, window)).map(|i| i + 1) };
    let Some(from) = from else { return };
    CLOSING.with(|c| c.set(true));
    for w in open[from.min(open.len())..].iter().rev() {
        w.orderOut(None);
    }
    CLOSING.with(|c| c.set(false));
    CANCELLED.with(|c| c.set(true));
    wake();
}

/// Have the loop look at its flags: an application-defined event at the
/// head of the queue.
fn wake() {
    if !TRACKING.with(Cell::get) || WOKEN.with(|w| w.replace(true)) {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::ZERO,
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        WAKE_SUBTYPE,
        WAKE_DATA,
        0,
    );
    if let Some(event) = event {
        NSApplication::sharedApplication(mtm).postEvent_atStart(&event, true);
    }
}

fn is_wake(event: &NSEvent) -> bool {
    event.r#type() == NSEventType::ApplicationDefined && event.subtype().0 == WAKE_SUBTYPE && event.data1() == WAKE_DATA
}

fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// The delegate's say on what `menu` holds: `menuNeedsUpdate:`, then
/// `numberOfItemsInMenu:` and the items filled in.
fn ask_delegate(menu: &NSMenu) {
    let Some(d) = menu_ivars(menu).delegate() else { return };
    if responds(&d, sel!(menuNeedsUpdate:)) {
        menu::needs_update(menu, &d);
    }
    if responds(&d, sel!(numberOfItemsInMenu:)) {
        // SAFETY: numberOfItemsInMenu: takes the menu and returns a count,
        // negative to leave the items as they are.
        let count: isize = unsafe { msg_send![&*d, numberOfItemsInMenu: menu] };
        if count >= 0 {
            fill(menu, &d, count as usize);
        }
    }
}

/// Bring a submenu up to date before it shows: its delegate's say,
/// `menuWillOpen:`, then validation.
fn prepare(menu: &NSMenu) {
    ask_delegate(menu);
    tell_delegate(menu, sel!(menuWillOpen:));
    menu.update();
}

/// Give `menu` `count` items and have the delegate fill each in, until it
/// says to stop.
fn fill(menu: &NSMenu, delegate: &AnyObject, count: usize) {
    let mtm = MainThreadMarker::from(menu);
    while (menu.numberOfItems() as usize) > count {
        menu.removeItemAtIndex(menu.numberOfItems() - 1);
    }
    while (menu.numberOfItems() as usize) < count {
        let empty = objc2_foundation::NSString::new();
        // SAFETY: the designated initializer.
        let blank =
            unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &empty, None, &empty) };
        menu.addItem(&blank);
    }
    if !responds(delegate, sel!(menu:updateItem:atIndex:shouldCancel:)) {
        return;
    }
    for i in 0..count {
        let Some(item) = menu_ivars(menu).item(i) else { break };
        // SAFETY: menu:updateItem:atIndex:shouldCancel: takes the menu, an
        // item, its index and a flag, and returns whether to go on.
        let go: bool =
            unsafe { msg_send![delegate, menu: menu, updateItem: &*item, atIndex: i as isize, shouldCancel: false] };
        if !go {
            break;
        }
    }
}

fn tell_delegate(menu: &NSMenu, selector: Sel) {
    if let Some(d) = menu_ivars(menu).delegate()
        && responds(&d, selector)
    {
        // SAFETY: menuWillOpen: and menuDidClose: take the menu.
        unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(&*d, selector, (menu,)) };
    }
}

/// Tell `menu`'s delegate the highlight moved to `item` (or none).
fn tell_highlight(menu: &NSMenu, item: Option<&NSMenuItem>) {
    if let Some(d) = menu_ivars(menu).delegate()
        && responds(&d, sel!(menu:willHighlightItem:))
    {
        // SAFETY: menu:willHighlightItem: takes the menu and an item or nil.
        let _: () = unsafe { msg_send![&*d, menu: menu, willHighlightItem: item] };
    }
}

/// Tell the delegates of menus that closed, innermost first: the highlights
/// gone, then the menus closed.
fn tell_closed(closed: &[(Retained<NSMenu>, bool)]) {
    for (menu, lit) in closed {
        if *lit {
            tell_highlight(menu, None);
        }
    }
    for (menu, _) in closed {
        tell_delegate(menu, sel!(menuDidClose:));
    }
}

/// A menu showing.
struct Level {
    menu: Retained<NSMenu>,
    view: Retained<MenuView>,
    window: Retained<NSWindow>,
    /// Where it was placed, to place it again at a new size.
    parent: Retained<NSWindow>,
    anchor: NSRect,
    layout: PopupLayout,
}

/// What the loop waits for besides input.
enum Pending {
    /// Open the submenu of `item`, in level `level`.
    Open { level: usize, item: Retained<NSMenuItem>, at: Instant },
    /// Close the levels above `keep`.
    Close { keep: usize, at: Instant },
}

impl Pending {
    fn at(&self) -> Instant {
        match self {
            Pending::Open { at, .. } | Pending::Close { at, .. } => *at,
        }
    }
}

struct Session {
    mtm: MainThreadMarker,
    levels: Vec<Level>,
    chosen: Option<Retained<NSMenuItem>>,
    pending: Option<Pending>,
    opened_at: Instant,
    pressed: bool,
    /// The pointer went into a menu.
    entered: bool,
    /// A button came up since the menu opened.
    released: bool,
    /// The menu bar the menu opened from, and the title to go on to (and
    /// whether a key asked).
    bar: Option<BarLink>,
    switch: Option<(usize, bool)>,
}

/// Show `menu` as `opening` says and track it until it closes; the item
/// chosen, if one was.
pub(crate) fn track(menu: &NSMenu, opening: Opening) -> Option<Retained<NSMenuItem>> {
    run_tracking(menu, opening, None, false).0
}

/// Track the menu of a menu bar's title: the title to go on to (and
/// whether a key asked), if the pointer or a key moved along the bar.
/// `select_first` highlights the first item, as a menu opened from the
/// keyboard does.
pub(crate) fn track_bar(menu: &NSMenu, opening: Opening, bar: BarLink, select_first: bool) -> Option<(usize, bool)> {
    run_tracking(menu, opening, Some(bar), select_first).1
}

/// Tell the view `willOpenMenu:withEvent:` or `didCloseMenu:withEvent:`,
/// with the event (the application's current one, which may be nil, when
/// none was given), as AppKit does.
fn tell_view(view: &NSView, menu: &NSMenu, event: Option<&NSEvent>, opening: bool) {
    let current =
        event.is_none().then(|| NSApplication::sharedApplication(MainThreadMarker::from(view)).currentEvent());
    let event = event.or(current.as_ref().and_then(|e| e.as_deref()));
    if opening {
        // SAFETY: willOpenMenu:withEvent: takes the menu and an event, which
        // AppKit passes nil for when there is no current event.
        let _: () = unsafe { msg_send![view, willOpenMenu: menu, withEvent: event] };
    } else {
        // SAFETY: didCloseMenu:withEvent: takes the menu and an event or
        // nil; AppKit passes nil.
        let _: () = unsafe { msg_send![view, didCloseMenu: menu, withEvent: None::<&NSEvent>] };
    }
}

fn run_tracking(
    menu: &NSMenu,
    opening: Opening,
    bar: Option<BarLink>,
    select_first: bool,
) -> (Option<Retained<NSMenuItem>>, Option<(usize, bool)>) {
    let mtm = MainThreadMarker::from(menu);
    if TRACKING.with(Cell::get) || !opening.parent.isVisible() {
        return (None, None);
    }
    TRACKING.with(|t| t.set(true));
    CANCELLED.with(|c| c.set(false));
    CHANGED.with(|c| c.set(false));
    ROOT.with(|r| r.replace(Some(opening.parent.clone())));
    ask_delegate(menu);
    crate::notifications::post(note!(NSMenuDidBeginTrackingNotification), menu);
    tell_delegate(menu, sel!(menuWillOpen:));
    if let Some(view) = &opening.view {
        tell_view(view, menu, opening.event.as_deref(), true);
    }
    menu.update();
    let mut s = Session {
        mtm,
        levels: Vec::new(),
        chosen: None,
        pending: None,
        opened_at: Instant::now(),
        pressed: opening.pressed,
        entered: false,
        released: false,
        bar,
        switch: None,
    };
    // Placed now that the delegate and validation have had their say.
    let view = MenuView::new(mtm, menu, opening.min_width);
    let mut layout = opening.layout;
    if let Some(inset) = opening.title_at {
        let shift = (view.with_layout(|l| l.title_x()) - inset).max(0.0);
        view.set_min_width(opening.min_width + shift);
        layout.offset.0 = -(shift.round() as i32);
    }
    let positioned = opening.positioning.as_deref().and_then(|item| {
        let (row, top, height) = view.with_layout(|l| {
            let row = l.row_of_item(menu, item)?;
            Some((row, l.rows[row].top, l.rows[row].height))
        })?;
        let middle = if opening.title_at.is_some() { height / 2.0 } else { 0.0 };
        layout.offset.1 = -((top + middle).round() as i32);
        Some(row)
    });
    s.show(menu.retain(), view, &opening.parent, opening.anchor, layout);
    if let Some(row) = positioned {
        s.highlight(0, Some(row));
    } else if select_first {
        s.step(0, None, 1);
    }
    s.run();
    let closed = s.close_all();
    finish_wake();
    OPEN.with(|o| o.borrow_mut().clear());
    let root = ROOT.with(|r| r.take());
    TRACKING.with(|t| t.set(false));
    // Tell in this order: the menus' delegates (the innermost first), the
    // view, the notification, then the action.
    tell_closed(&closed);
    if let Some(view) = &opening.view {
        tell_view(view, menu, None, false);
    }
    crate::notifications::post(note!(NSMenuDidEndTrackingNotification), menu);
    if let Some(item) = &s.chosen {
        menu::send_action(item);
    }
    drop(root);
    (s.chosen, s.switch)
}

/// Take a wake-up event the loop didn't get to.
fn finish_wake() {
    if !WOKEN.with(|w| w.replace(false)) {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: the mode is AppKit's constant.
    let mode = unsafe { NSEventTrackingRunLoopMode };
    while let Some(e) =
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, None, mode, false)
    {
        if !is_wake(&e) {
            break;
        }
        app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::ApplicationDefined, None, mode, true);
    }
}

impl Session {
    /// Show `menu` in a new level over `parent`, against `anchor`.
    fn show(
        &mut self,
        menu: Retained<NSMenu>,
        view: Retained<MenuView>,
        parent: &NSWindow,
        anchor: NSRect,
        layout: PopupLayout,
    ) {
        let window = menu_view::window_for(self.mtm, &view);
        crate::window::show_as_menu(&window, parent, anchor, layout);
        OPEN.with(|o| o.borrow_mut().push((menu.clone(), window.clone())));
        self.levels.push(Level { menu, view, window, parent: parent.retain(), anchor, layout });
    }

    fn run(&mut self) {
        let app = NSApplication::sharedApplication(self.mtm);
        // SAFETY: the mode is AppKit's constant.
        let mode = unsafe { NSEventTrackingRunLoopMode };
        while !CANCELLED.with(Cell::get) && self.chosen.is_none() && self.switch.is_none() {
            let date = self.pending.as_ref().map(|p| {
                let wait = p.at().saturating_duration_since(Instant::now());
                NSDate::dateWithTimeIntervalSinceNow(wait.as_secs_f64())
            });
            let date = date.unwrap_or_else(NSDate::distantFuture);
            let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&date), mode, true);
            if let Some(event) = event {
                self.handle(&event);
            }
            self.due();
            if CHANGED.with(|c| c.replace(false)) {
                self.menus_changed();
            }
        }
    }

    /// Open or close a submenu whose time has come.
    fn due(&mut self) {
        if self.pending.as_ref().is_none_or(|p| p.at() > Instant::now()) {
            return;
        }
        match self.pending.take() {
            Some(Pending::Open { level, item, .. }) => self.open_submenu(level, &item, false),
            Some(Pending::Close { keep, .. }) => self.close_above(keep),
            None => {}
        }
    }

    /// Menus showing changed: lay them out again, close submenus whose
    /// hosts went away, and show again at its new size a menu whose size
    /// changed (with the submenus above it closed: a popup's size is fixed
    /// once it shows).
    fn menus_changed(&mut self) {
        let mut level = 0;
        while level < self.levels.len() {
            if level > 0 && self.host_row(level).is_none() {
                self.close_above(level - 1);
                break;
            }
            let l = &self.levels[level];
            l.view.refresh();
            let size = l.view.with_layout(|lay| lay.size);
            if size != l.window.frame().size {
                self.close_above(level);
                let l = &self.levels[level];
                CLOSING.with(|c| c.set(true));
                l.window.orderOut(None);
                CLOSING.with(|c| c.set(false));
                l.window.setContentSize(size);
                l.view.setFrameSize(size);
                crate::window::show_as_menu(&l.window, &l.parent, l.anchor, l.layout);
                break;
            }
            level += 1;
        }
    }

    fn handle(&mut self, event: &NSEvent) {
        match event.r#type() {
            NSEventType::MouseMoved
            | NSEventType::LeftMouseDragged
            | NSEventType::RightMouseDragged
            | NSEventType::OtherMouseDragged => self.pointer(event),
            NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown => {
                // A press on another of the bar's titles opens its menu;
                // anywhere else outside the menus closes them.
                match self.bar_hit(event) {
                    Some(Some(title)) if self.bar.as_ref().is_some_and(|b| b.current != title) => {
                        self.switch = Some((title, false));
                    }
                    _ if self.hit(event).is_none() => CANCELLED.with(|c| c.set(true)),
                    _ => {}
                }
            }
            NSEventType::LeftMouseUp | NSEventType::RightMouseUp | NSEventType::OtherMouseUp => self.release(event),
            NSEventType::KeyDown => self.key(event),
            NSEventType::ApplicationDefined if is_wake(event) => WOKEN.with(|w| w.set(false)),
            _ => {}
        }
    }

    /// Which level's window the event is in, and the row under it there.
    fn hit(&self, event: &NSEvent) -> Option<(usize, Option<usize>)> {
        let window = event.window(self.mtm)?;
        let level = self.levels.iter().position(|l| std::ptr::eq(&*l.window, &*window))?;
        let view = &self.levels[level].view;
        let p = view.convertPoint_fromView(event.locationInWindow(), None);
        let row = view.with_layout(|l| {
            let inside = p.x >= 0.0 && p.x < l.size.width;
            if inside { l.row_at(p.y) } else { None }
        });
        Some((level, row))
    }

    /// Whether the event is over the menu bar the menu came from, and the
    /// title there, if any.
    fn bar_hit(&self, event: &NSEvent) -> Option<Option<usize>> {
        let bar = self.bar.as_ref()?;
        let window = event.window(self.mtm)?;
        let at = event.locationInWindow();
        if !std::ptr::eq(&*window, &*bar.window) || at.y < bar.content {
            return None;
        }
        Some(bar.spans.iter().position(|&(x0, x1)| at.x >= x0 && at.x < x1))
    }

    /// Row `row` of level `level`: whether it can be chosen, and its item
    /// and whether that has a submenu; None past the last row.
    fn row(&self, level: usize, row: usize) -> Option<(bool, Retained<NSMenuItem>, bool)> {
        let l = self.levels.get(level)?;
        let (selectable, index, submenu) = l.view.with_layout(|lay| {
            let r = lay.rows.get(row)?;
            Some((r.selectable(), r.item, r.submenu))
        })?;
        Some((selectable, menu_ivars(&l.menu).item(index)?, submenu))
    }

    fn pointer(&mut self, event: &NSEvent) {
        // Along the bar to another title: its menu instead.
        if let Some(Some(title)) = self.bar_hit(event)
            && self.bar.as_ref().is_some_and(|b| b.current != title)
        {
            self.switch = Some((title, false));
            return;
        }
        let Some((level, row)) = self.hit(event) else {
            // Outside the menus: the deepest menu shows nothing
            // highlighted, unless it is showing a submenu.
            let deepest = self.levels.len() - 1;
            if matches!(self.pending, Some(Pending::Open { .. })) {
                self.pending = None;
            }
            self.highlight(deepest, None);
            return;
        };
        self.entered = true;
        // Coming into a submenu keeps it, and its host highlighted.
        if let Some(Pending::Close { keep, .. }) = self.pending
            && level >= keep
        {
            self.pending = None;
            self.rehighlight_hosts(level);
        }
        let found = row.and_then(|r| self.row(level, r)).filter(|(selectable, ..)| *selectable);
        let row = found.as_ref().and(row);
        if row == self.levels[level].view.highlighted() {
            return;
        }
        self.highlight(level, row);
        let deeper_open = self.levels.len() > level + 1;
        let now = Instant::now();
        self.pending = match found {
            // Another submenu waits its turn; an open one goes when it comes.
            Some((_, item, true)) => Some(Pending::Open { level, item, at: now + SUBMENU_DELAY }),
            _ if deeper_open => Some(Pending::Close { keep: level + 1, at: now + SUBMENU_DELAY }),
            _ => None,
        };
    }

    /// The row of the item in level `level - 1` whose submenu level `level`
    /// shows, if it is still there.
    fn host_row(&self, level: usize) -> Option<usize> {
        let (host, sub) = (&self.levels[level - 1], &self.levels[level].menu);
        host.view.with_layout(|lay| {
            lay.rows.iter().position(|r| {
                menu_ivars(&host.menu)
                    .item(r.item)
                    .and_then(|i| item_ivars(&i).submenu())
                    .is_some_and(|s| std::ptr::eq(&*s, &**sub))
            })
        })
    }

    /// Keep the hosts of the levels up to `level` highlighted.
    fn rehighlight_hosts(&mut self, level: usize) {
        for l in 1..=level.min(self.levels.len() - 1) {
            let host_row = self.host_row(l);
            if host_row.is_some() {
                self.highlight(l - 1, host_row);
            }
        }
    }

    fn release(&mut self, event: &NSEvent) {
        let first = !std::mem::replace(&mut self.released, true);
        match self.hit(event) {
            Some((level, Some(row))) => self.choose(level, row),
            Some((_, None)) => {}
            None => {
                // A release that ends the press opening the menu leaves it
                // open for a click, unless the pointer went through it.
                let quick = self.opened_at.elapsed() < CLICK;
                if !(first && self.pressed && (quick || !self.entered)) {
                    CANCELLED.with(|c| c.set(true));
                }
            }
        }
    }

    /// Choose row `row` of level `level`: an item's action, or a submenu
    /// opening.
    fn choose(&mut self, level: usize, row: usize) {
        let Some((true, item, submenu)) = self.row(level, row) else { return };
        if submenu {
            self.open_submenu(level, &item, true);
        } else {
            self.chosen = Some(item);
        }
    }

    fn key(&mut self, event: &NSEvent) {
        let key = event.charactersIgnoringModifiers().and_then(|c| crate::keyequiv::first_char(&c).0);
        let Some(key) = key else { return };
        let deepest = self.levels.len() - 1;
        let current = self.levels[deepest].view.highlighted();
        self.pending = None;
        match key {
            '\u{1b}' => CANCELLED.with(|c| c.set(true)),
            '\u{F700}' => self.step(deepest, current, -1),
            '\u{F701}' => self.step(deepest, current, 1),
            '\u{F729}' => self.step(deepest, None, 1),
            '\u{F72B}' => self.step(deepest, None, -1),
            '\u{F702}' if deepest > 0 => self.close_above(deepest - 1),
            '\u{F702}' => self.along_bar(-1),
            '\u{F703}' => match current.and_then(|row| self.row(deepest, row)) {
                Some((true, item, true)) => self.open_submenu(deepest, &item, true),
                _ => self.along_bar(1),
            },
            '\r' | '\u{3}' | ' ' => {
                if let Some(row) = current {
                    self.choose(deepest, row);
                }
            }
            c if !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c) => {
                let c = c.to_lowercase().next().unwrap_or(c);
                let found = self.levels[deepest].view.with_layout(|l| {
                    let n = l.rows.len();
                    let start = current.map_or(0, |r| r + 1);
                    (0..n).map(|i| (start + i) % n).find(|&r| l.rows[r].selectable() && l.rows[r].initial() == Some(c))
                });
                if found.is_some() {
                    self.highlight(deepest, found);
                }
            }
            _ => {}
        }
    }

    /// Go on to the bar's next (1) or previous (-1) title's menu, from the
    /// keyboard, when the menu came from a bar.
    fn along_bar(&mut self, dir: isize) {
        if let Some(bar) = &self.bar {
            let n = bar.spans.len() as isize;
            if n > 1 {
                let to = (bar.current as isize + dir).rem_euclid(n) as usize;
                self.switch = Some((to, true));
            }
        }
    }

    /// Move level `level`'s highlight a row up or down from `from` (from
    /// the end with none), skipping what can't be chosen, wrapping round.
    fn step(&mut self, level: usize, from: Option<usize>, dir: isize) {
        let next = self.levels[level].view.with_layout(|l| {
            let n = l.rows.len() as isize;
            if n == 0 {
                return None;
            }
            let start = from.map_or(if dir > 0 { -1 } else { n }, |r| r as isize);
            (1..=n).map(|i| (start + dir * i).rem_euclid(n) as usize).find(|&r| l.rows[r].selectable())
        });
        if next.is_some() {
            self.highlight(level, next);
            if self.levels.len() > level + 1 {
                self.close_above(level);
            }
        }
    }

    /// Show row `row` of level `level` highlighted (or none), telling the
    /// menu and its delegate.
    fn highlight(&mut self, level: usize, row: Option<usize>) {
        let Some(l) = self.levels.get(level) else { return };
        if let Some(item) = l.view.set_highlighted(row) {
            let menu = l.menu.clone();
            tell_highlight(&menu, item.as_deref());
        }
    }

    /// Open the submenu of `item`, in level `level`; `select` highlights
    /// its first item, as keys and clicks do.
    fn open_submenu(&mut self, level: usize, item: &NSMenuItem, select: bool) {
        if level >= self.levels.len() {
            return;
        }
        let Some(sub) = item_ivars(item).submenu() else { return };
        // Already open?
        if self.levels.get(level + 1).is_some_and(|l| std::ptr::eq(&*l.menu, &*sub)) {
            if select {
                self.step(level + 1, None, 1);
            }
            return;
        }
        self.close_above(level);
        let menu = self.levels[level].menu.clone();
        let Some(row) = self.levels[level].view.with_layout(|l| l.row_of_item(&menu, item)) else { return };
        self.highlight(level, Some(row));
        prepare(&sub);
        // The delegates may have changed the menus: find the host again.
        if level + 1 != self.levels.len() || !std::ptr::eq(&*self.levels[level].menu, &*menu) {
            return;
        }
        let host_view = self.levels[level].view.clone();
        let Some(rect) = host_view.with_layout(|l| {
            let r = l.row_rect(l.row_of_item(&menu, item)?);
            Some(NSRect::new(NSPoint::new(0.0, r.origin.y), NSSize::new(l.size.width, r.size.height)))
        }) else {
            return;
        };
        let anchor = host_view.convertRect_toView(rect, None);
        let layout = PopupLayout {
            corner: Corner::TopRight,
            gravity: Corner::BottomRight,
            offset: (-1, -(EDGE as i32)),
            flip_x: true,
            flip_y: false,
            slide_x: false,
            slide_y: true,
            resize_y: true,
        };
        let view = MenuView::new(self.mtm, &sub, 0.0);
        let parent = self.levels[level].window.clone();
        self.show(sub, view, &parent, anchor, layout);
        if select {
            let deepest = self.levels.len() - 1;
            self.step(deepest, None, 1);
        }
    }

    /// Close every level above `keep`, innermost first, telling their
    /// delegates.
    fn close_above(&mut self, keep: usize) {
        let mut closed = Vec::new();
        while self.levels.len() > keep + 1 {
            let Some(level) = self.levels.pop() else { break };
            OPEN.with(|o| o.borrow_mut().pop());
            let lit = order_out(&level);
            closed.push((level.menu, lit));
        }
        tell_closed(&closed);
    }

    /// Close every level, innermost first; the menus and whether each had
    /// an item highlighted, for their delegates to be told once tracking
    /// has ended.
    fn close_all(&mut self) -> Vec<(Retained<NSMenu>, bool)> {
        let mut closed = Vec::with_capacity(self.levels.len());
        while let Some(level) = self.levels.pop() {
            let lit = order_out(&level);
            closed.push((level.menu, lit));
        }
        closed
    }
}

/// Take `level` off the screen and its highlight away; whether it had one.
fn order_out(level: &Level) -> bool {
    CLOSING.with(|c| c.set(true));
    level.window.orderOut(None);
    CLOSING.with(|c| c.set(false));
    level.view.clear_highlight()
}
