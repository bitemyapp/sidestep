//! The main menu as a bar in windows.
//!
//! Linux has no menu bar of its own, so, as GTK and KDE programs do, every
//! titled window shows `NSApp.mainMenu` along its top, except panels and
//! sheets. The bar is window chrome, like the title bar: it sits under the
//! title bar Sidestep draws (or at the top, when the compositor draws the
//! decorations), the content view keeps its size, `frame` and
//! `frameRectForContentRect:` take it in and `contentLayoutRect` leaves it
//! out. The render thread shows it (`backend::menubar`) as ops drawn here:
//! the menus' titles in the menu bar font, the first one, when it has no
//! title, the program's name. It is drawn again only when what it shows
//! changes (the main menu or its menus' titles, the title shown open), at
//! the end of the turn in which that happened.
//!
//! A click on a title opens its menu below it, through the menu tracking
//! loop; while one is open, moving over another title opens that one, and
//! Left and Right at the menu's top go to the neighbouring menus. F10
//! opens the first menu from the keyboard. The bar hides without a main
//! menu, after `+[NSMenu setMenuBarVisible:NO]`, in full screen, and with
//! `SIDESTEP_MENUBAR=hidden`. `menuBarHeight` is its height for the main
//! menu and 0 for other menus.

use std::cell::{Cell, RefCell};

use objc2::rc::{Retained, Weak};
use objc2::{ClassType, MainThreadMarker, Message};
use objc2_app_kit::{NSApplication, NSEvent, NSFont, NSMenu, NSPanel, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSProcessInfo, NSRect, NSSize};
use sidestep_foundation::runloop::{self, Mode};

use crate::graphics::{self, Xf};
use crate::menu::{item_ivars, menu_ivars};
use crate::menu_tracking::{self, BarLink, Opening};
use crate::protocol::{Op, Rect, ToRender};
use crate::text::layout::{Attrs, LineBreak, Options, Run};
use crate::theme::{self, paint};

/// Room left of the first title.
const LEAD: f64 = 6.0;
/// Room each side of a title.
const PAD: f64 = 10.0;

/// A title on the bar: the main menu's item it shows, and where.
#[derive(Clone, Debug, PartialEq)]
struct Title {
    item: usize,
    text: String,
    x0: f64,
    x1: f64,
}

/// What a window's bar shows.
struct Shown {
    window: Weak<NSWindow>,
    /// The render thread's name for the window's showing it was sent to.
    id: u32,
    height: u32,
    titles: Vec<Title>,
    open: Option<usize>,
}

thread_local! {
    static SHOWN: RefCell<Vec<Shown>> = const { RefCell::new(Vec::new()) };
    /// A refresh waits for the end of the turn.
    static PENDING: Cell<bool> = const { Cell::new(false) };
    /// The title whose menu is open, and its window.
    static OPEN: RefCell<Option<(Weak<NSWindow>, usize)>> = const { RefCell::new(None) };
    /// The bar's height, once measured.
    static HEIGHT: Cell<u32> = const { Cell::new(0) };
}

/// Whether bars show at all: not with `SIDESTEP_MENUBAR=hidden`, nor after
/// `setMenuBarVisible:NO`.
fn enabled() -> bool {
    static HIDDEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let hidden = *HIDDEN.get_or_init(|| std::env::var("SIDESTEP_MENUBAR").is_ok_and(|v| v == "hidden"));
    !hidden && crate::menu::bar_visible()
}

/// The main menu, if the program set one that is a menu (anything with
/// `performKeyEquivalent:` will do for key equivalents; only menus show).
fn main_menu() -> Option<Retained<NSMenu>> {
    crate::app::existing().and_then(|app| app.mainMenu()).filter(|m| crate::controls::kind_of(m, NSMenu::class()))
}

fn attrs() -> Attrs {
    let mut a = Attrs::new(crate::font::text_font(&NSFont::menuBarFontOfSize(0.0)));
    a.paragraph.line_break = LineBreak::Clip;
    a
}

fn measure(text: &str, attrs: &Attrs) -> (f64, f64) {
    let run = [Run { start: 0, end: text.len(), attrs: 0 }];
    let laid = crate::text::layout::lay_out(text, std::slice::from_ref(attrs), &run, &Options::UNBOUNDED);
    (f64::from(laid.width).ceil(), f64::from(laid.height).ceil())
}

/// The bar's height, in points: the font's line and 5 points each side, 24
/// at least.
fn bar_height() -> u32 {
    let known = HEIGHT.with(Cell::get);
    if known > 0 {
        return known;
    }
    let line = measure("Ag", &attrs()).1;
    let height = (line + 10.0).max(24.0).ceil() as u32;
    HEIGHT.with(|h| h.set(height));
    height
}

/// `-[NSMenu menuBarHeight]`: the bar's for the main menu, else none.
pub(crate) fn height_for(menu: &NSMenu) -> f64 {
    let main = main_menu().is_some_and(|m| std::ptr::eq(&*m, menu));
    if main && enabled() { bar_height() as f64 } else { 0.0 }
}

/// The height a window with `style` will have a bar of, before the render
/// thread says (see `window`'s predicted title bar).
pub(crate) fn predicted_height(style: NSWindowStyleMask) -> f64 {
    let titled = style.contains(NSWindowStyleMask::Titled) && !style.contains(NSWindowStyleMask::UtilityWindow);
    if titled && enabled() && main_menu().is_some() { bar_height() as f64 } else { 0.0 }
}

/// The titles the main menu shows, laid out from the left.
fn titles(main: &NSMenu) -> Vec<Title> {
    let attrs = attrs();
    let ivars = menu_ivars(main);
    let mut x = LEAD;
    let mut out = Vec::new();
    for i in 0..ivars.len() {
        let Some(item) = ivars.item(i) else { break };
        if item_ivars(&item).is_hidden() {
            continue;
        }
        let mut text = match item_ivars(&item).submenu() {
            Some(sub) => sub.title().to_string(),
            None => item.title().to_string(),
        };
        if text.is_empty() && out.is_empty() {
            text = NSProcessInfo::processInfo().processName().to_string();
        }
        if text.is_empty() {
            continue;
        }
        let width = measure(&text, &attrs).0 + 2.0 * PAD;
        out.push(Title { item: i, text, x0: x, x1: x + width });
        x += width;
    }
    out
}

/// The bar's ops: its ground, a line along its bottom, and the titles,
/// the open one on the accent.
fn draw(titles: &[Title], height: u32, open: Option<usize>) -> Vec<Op> {
    let p = theme::palette();
    let h = height as f64;
    let wide = 1.0e5;
    graphics::begin_recording();
    graphics::set_view(Xf::IDENTITY, Rect::new(0.0, 0.0, wide as f32, height as f32));
    paint::fill_rect(NSRect::new(NSPoint::ZERO, NSSize::new(wide, h)), p.window);
    paint::fill_rect(NSRect::new(NSPoint::new(0.0, h - 1.0), NSSize::new(wide, 1.0)), p.separator);
    let base = attrs();
    let line = measure("Ag", &base).1;
    for (i, t) in titles.iter().enumerate() {
        let lit = open == Some(i);
        if lit {
            let r = NSRect::new(NSPoint::new(t.x0, 3.0), NSSize::new(t.x1 - t.x0, h - 6.0));
            paint::fill_round_rect(r, paint::radii(5.0), p.accent);
        }
        let mut a = base.clone();
        a.color = if lit { p.accent_text_on } else { p.label };
        let y = ((h - line) / 2.0).round();
        paint::text(&t.text, &a, NSRect::new(NSPoint::new(t.x0 + PAD, y), NSSize::new(t.x1 - t.x0, line)));
    }
    graphics::end_recording()
}

/// Whether `window` shows a bar: titled, on screen, not a panel or sheet.
fn wants_bar(window: &NSWindow) -> bool {
    window.styleMask().contains(NSWindowStyleMask::Titled)
        && window.isVisible()
        && !window.isSheet()
        && !crate::controls::kind_of(window, NSPanel::class())
}

/// Something the bars show may have changed: look again at the end of the
/// turn.
fn schedule() {
    if PENDING.with(|p| p.replace(true)) {
        return;
    }
    runloop::main().perform(&[Mode::COMMON], refresh);
}

/// A menu changed (see `menu::bump`): the bars look again if it is the
/// main menu. (Its menus' items changing, as validation makes them, shows
/// nothing on the bar.)
pub(crate) fn menu_changed(menu: &NSMenu) {
    if SHOWN.with(|s| s.borrow().is_empty()) {
        return;
    }
    if main_menu().is_some_and(|m| std::ptr::eq(&*m, menu)) {
        schedule();
    }
}

/// A menu's title changed: the bars show the titles of the main menu's
/// menus.
pub(crate) fn title_changed(menu: &NSMenu) {
    if SHOWN.with(|s| s.borrow().is_empty()) {
        return;
    }
    // SAFETY: supermenu returns a menu or nil.
    let up = unsafe { menu.supermenu() };
    if up.is_some_and(|up| main_menu().is_some_and(|m| std::ptr::eq(&*m, &*up))) {
        schedule();
    }
}

/// `setMainMenu:` or `setMenuBarVisible:`.
pub(crate) fn visibility_changed() {
    schedule();
}

/// A window came on screen: a titled one may show a bar (menus, tooltips
/// and other borderless windows don't).
pub(crate) fn window_shown(window: &NSWindow) {
    if !window.styleMask().contains(NSWindowStyleMask::Titled) {
        return;
    }
    if main_menu().is_some() || SHOWN.with(|s| !s.borrow().is_empty()) {
        schedule();
    }
}

/// Bring every window's bar up to date.
fn refresh() {
    PENDING.with(|p| p.set(false));
    let main = main_menu().filter(|_| enabled());
    let titles = main.as_deref().map(titles).unwrap_or_default();
    let height = bar_height();
    let open = OPEN.with(|o| o.borrow().as_ref().and_then(|(w, i)| Some((w.load()?, *i))));
    let old: Vec<Shown> = SHOWN.with(|s| std::mem::take(&mut *s.borrow_mut()));
    let mut keep = Vec::new();
    let mut sends = Vec::new();
    for window in crate::app::windows_for_appearance() {
        if !window.isVisible() {
            continue;
        }
        let id = crate::window::imp(&window).id();
        let before = old.iter().find(|s| s.id == id && s.window.load().is_some_and(|w| std::ptr::eq(&*w, &*window)));
        if titles.is_empty() || !wants_bar(&window) {
            if before.is_some() {
                sends.push(ToRender::MenuBar { window: id, height: 0, ops: Vec::new() });
            }
            continue;
        }
        let open_here = open.as_ref().filter(|(w, _)| std::ptr::eq(&**w, &*window)).map(|(_, i)| *i);
        let same = before.is_some_and(|b| b.height == height && b.titles == titles && b.open == open_here);
        if !same {
            sends.push(ToRender::MenuBar { window: id, height, ops: draw(&titles, height, open_here) });
        }
        keep.push(Shown { window: Weak::new(&window), id, height, titles: titles.clone(), open: open_here });
    }
    SHOWN.with(|s| *s.borrow_mut() = keep);
    for msg in sends {
        crate::app::send(msg);
    }
}

/// The titles of `window`'s bar, as spans of x.
fn spans(window: &NSWindow) -> Vec<(f64, f64)> {
    SHOWN.with(|s| {
        let s = s.borrow();
        s.iter()
            .find(|b| b.window.load().is_some_and(|w| std::ptr::eq(&*w, window)))
            .map(|b| b.titles.iter().map(|t| (t.x0, t.x1)).collect())
            .unwrap_or_default()
    })
}

/// A left press in `window` (see `window`'s event handling): true if it
/// was on the bar, which then opened the menu under it.
pub(crate) fn mouse_down(window: &NSWindow, event: &NSEvent) -> bool {
    let at = event.locationInWindow();
    if at.y < crate::window::imp(window).content_height() {
        return false;
    }
    let hit = spans(window).iter().position(|&(x0, x1)| at.x >= x0 && at.x < x1);
    if let Some(index) = hit {
        open(window, index, true, false);
    }
    true
}

/// F10: open the key window's first menu from the keyboard, if it has a
/// bar.
pub(crate) fn open_with_keyboard() -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let Some(window) = NSApplication::sharedApplication(mtm).keyWindow() else { return false };
    if spans(&window).is_empty() {
        return false;
    }
    open(&window, 0, false, true);
    true
}

/// Show the menu of title `index` on `window`'s bar, and follow the
/// pointer and keys across the bar until a menu closes for good.
fn open(window: &NSWindow, index: usize, pressed: bool, keyboard: bool) {
    let Some(main) = main_menu() else { return };
    let (mut index, mut pressed, mut keyboard) = (index, pressed, keyboard);
    // The bar follows the pointer while a menu is open.
    let moves = window.acceptsMouseMovedEvents();
    window.setAcceptsMouseMovedEvents(true);
    loop {
        let titles = titles(&main);
        let Some(title) = titles.get(index).cloned() else { break };
        let Some(item) = menu_ivars(&main).item(title.item) else { break };
        let Some(submenu) = item_ivars(&item).submenu() else {
            // A title without a menu acts at once.
            crate::menu::send_action(&item);
            break;
        };
        set_open(window, Some(index));
        let content = crate::window::imp(window).content_height();
        let bar = BarLink {
            window: window.retain(),
            content,
            spans: titles.iter().map(|t| (t.x0, t.x1)).collect(),
            current: index,
        };
        let opening = Opening {
            parent: window.retain(),
            anchor: NSRect::new(NSPoint::new(title.x0, content), NSSize::new(title.x1 - title.x0, bar_height() as f64)),
            layout: menu_tracking::below(),
            positioning: None,
            min_width: 0.0,
            title_at: None,
            pressed,
            view: None,
            event: None,
        };
        match menu_tracking::track_bar(&submenu, opening, bar, keyboard) {
            Some((to, by_key)) => {
                index = to;
                (pressed, keyboard) = (false, by_key);
            }
            None => break,
        }
    }
    set_open(window, None);
    window.setAcceptsMouseMovedEvents(moves);
}

/// Show title `index` of `window`'s bar as open (or none), now.
fn set_open(window: &NSWindow, index: Option<usize>) {
    let old = OPEN.with(|o| o.replace(index.map(|i| (Weak::new(window), i))));
    drop(old);
    refresh();
}
