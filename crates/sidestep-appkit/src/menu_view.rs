//! Menus on screen: how a menu is laid out and drawn, in a borderless
//! window of its own that `menu_tracking` shows as a popup.
//!
//! A layout is worked out from the menu's items and kept until the menu's
//! generation moves (see `menu`): a row per item that shows (hidden and
//! alternate items take no room), each as tall as the menu font's line
//! plus 8 points (22 at least), separators 9 points, and 5 points above
//! and below. Across a row: 12 points of margin, a column for check marks
//! when the menu shows one and an item has a state, one for images when an
//! item has one, the title (indented 10 points a level), the key
//! equivalent's label right-aligned 24 points after the widest title, and
//! an arrow column when an item has a submenu. A menu is at least its
//! `minimumWidth` wide, and at least as wide as what it opens from.
//!
//! The view draws with the theme's painters and the text engine: the
//! menu's ground and edge, the highlighted row in the accent color,
//! disabled items dimmed, separators, check marks and dashes for states,
//! arrows for submenus. The item shown highlighted is the menu's own
//! (`highlightedItem`), so a menu that changes while it shows keeps its
//! highlight on the same item, wherever that item's row now is.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSCompositingOperation, NSEvent, NSFont, NSImage, NSMenu, NSMenuItem, NSResponder, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::menu::{item_ivars, menu_ivars};
use crate::protocol::Color;
use crate::text::layout::{Attrs, LineBreak, Options, Run};
use crate::theme::{self, paint};

/// Room above and below the rows, and each side of a row's highlight.
pub(crate) const EDGE: f64 = 5.0;
/// Room between a row's highlight and what it shows.
const SIDE: f64 = 12.0;
const STATE_COLUMN: f64 = 18.0;
const SEPARATOR: f64 = 9.0;
const INDENT: f64 = 10.0;
const KEY_GAP: f64 = 24.0;
const ARROW_COLUMN: f64 = 16.0;
const IMAGE_GAP: f64 = 6.0;
/// The shortest row.
const MIN_ROW: f64 = 22.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Item,
    Separator,
    Header,
}

/// A row: an item that shows.
pub(crate) struct Row {
    /// The item's index in the menu.
    pub item: usize,
    pub kind: Kind,
    /// From the top of the menu.
    pub top: f64,
    pub height: f64,
    pub enabled: bool,
    pub submenu: bool,
    title: String,
    key: String,
    /// The key label's width.
    key_width: f64,
    state: isize,
    indent: f64,
    image: Option<Retained<NSImage>>,
}

impl Row {
    /// Whether it can be highlighted and chosen.
    pub(crate) fn selectable(&self) -> bool {
        self.kind == Kind::Item && self.enabled
    }

    /// The first letter of its title, lowercased, for type-select.
    pub(crate) fn initial(&self) -> Option<char> {
        self.title.chars().next().and_then(|c| c.to_lowercase().next())
    }
}

/// A menu laid out.
pub(crate) struct Layout {
    /// The menu's generation it was worked out for, and the least width
    /// asked for.
    generation: u64,
    min_width: f64,
    pub size: NSSize,
    pub rows: Vec<Row>,
    attrs: Attrs,
    line: f64,
    state_x: f64,
    image_x: f64,
    image_side: f64,
    title_x: f64,
    arrows: bool,
}

impl Layout {
    /// The row at `y` from the top, if one is there.
    pub(crate) fn row_at(&self, y: f64) -> Option<usize> {
        self.rows.iter().position(|r| y >= r.top && y < r.top + r.height)
    }

    /// The row showing item `index`.
    pub(crate) fn row_of(&self, index: usize) -> Option<usize> {
        self.rows.iter().position(|r| r.item == index)
    }

    /// The row showing `item`, if it shows.
    pub(crate) fn row_of_item(&self, menu: &NSMenu, item: &NSMenuItem) -> Option<usize> {
        crate::menu::index_of(menu, item).and_then(|index| self.row_of(index))
    }

    /// The rectangle of row `row`, from the menu's top left, as its
    /// highlight covers it (empty past the last row).
    pub(crate) fn row_rect(&self, row: usize) -> NSRect {
        let Some(r) = self.rows.get(row) else { return NSRect::ZERO };
        NSRect::new(NSPoint::new(EDGE, r.top), NSSize::new(self.size.width - 2.0 * EDGE, r.height))
    }

    /// Where the titles start, from the menu's left edge.
    pub(crate) fn title_x(&self) -> f64 {
        self.title_x
    }
}

fn attrs_for(font: &NSFont) -> Attrs {
    let mut a = Attrs::new(crate::font::text_font(font));
    a.paragraph.line_break = LineBreak::Clip;
    a
}

/// The width and height of one line of `text`.
fn measure(text: &str, attrs: &Attrs) -> (f64, f64) {
    let run = [Run { start: 0, end: text.len(), attrs: 0 }];
    let laid = crate::text::layout::lay_out(text, std::slice::from_ref(attrs), &run, &Options::UNBOUNDED);
    (f64::from(laid.width).ceil(), f64::from(laid.height).ceil())
}

/// Lay `menu` out, at least `min_width` wide (besides its own minimum).
pub(crate) fn layout(menu: &NSMenu, min_width: f64) -> Layout {
    let ivars = menu_ivars(menu);
    let font = ivars.font().unwrap_or_else(|| NSFont::menuFontOfSize(0.0));
    let attrs = attrs_for(&font);
    let line = measure("Ag", &attrs).1;
    let row_height = (line + 8.0).max(MIN_ROW);
    let mut rows = Vec::new();
    let mut top = EDGE;
    let (mut any_state, mut any_image, mut any_submenu) = (false, false, false);
    let (mut widest_title, mut widest_key) = (0.0f64, 0.0f64);
    for index in 0..ivars.len() {
        let Some(item) = ivars.item(index) else { break };
        let it = item_ivars(&item);
        if it.is_hidden() || item.isAlternate() {
            continue;
        }
        let kind = if it.is_separator() {
            Kind::Separator
        } else if item.isSectionHeader() {
            Kind::Header
        } else {
            Kind::Item
        };
        let height = if kind == Kind::Separator { SEPARATOR } else { row_height };
        let title = if kind == Kind::Separator { String::new() } else { item.title().to_string() };
        let key = if kind == Kind::Item {
            crate::keyequiv::label(&item.keyEquivalent().to_string(), item.keyEquivalentModifierMask())
        } else {
            String::new()
        };
        let key_width = if key.is_empty() { 0.0 } else { measure(&key, &attrs).0 };
        let indent = item.indentationLevel() as f64 * INDENT;
        let image = item.image();
        let submenu = it.submenu().is_some();
        let state = item.state();
        any_state |= state != 0;
        any_image |= image.is_some();
        any_submenu |= submenu;
        if !title.is_empty() {
            widest_title = widest_title.max(indent + measure(&title, &attrs).0);
        }
        widest_key = widest_key.max(key_width);
        let enabled = item.isEnabled();
        rows.push(Row {
            item: index,
            kind,
            top,
            height,
            enabled,
            submenu,
            title,
            key,
            key_width,
            state,
            indent,
            image,
        });
        top += height;
    }
    let state_x = EDGE + SIDE;
    let state_width = if ivars.shows_state_column() && any_state { STATE_COLUMN } else { 0.0 };
    let image_x = state_x + state_width;
    let image_side = (row_height - 6.0).min(16.0);
    let image_width = if any_image { image_side + IMAGE_GAP } else { 0.0 };
    let title_x = image_x + image_width;
    let keys = if widest_key > 0.0 { KEY_GAP + widest_key } else { 0.0 };
    let arrow = if any_submenu { ARROW_COLUMN } else { 0.0 };
    let natural = title_x + widest_title + keys + arrow + SIDE + EDGE;
    let width = natural.max(ivars.minimum_width()).max(min_width).ceil();
    Layout {
        generation: ivars.generation(),
        min_width,
        size: NSSize::new(width, top + EDGE),
        rows,
        attrs,
        line,
        state_x,
        image_x,
        image_side,
        title_x,
        arrows: any_submenu,
    }
}

/// `-[NSMenu size]`.
pub(crate) fn size_of(menu: &NSMenu) -> NSSize {
    layout(menu, 0.0).size
}

/// The menu's ground.
fn ground(p: &theme::Palette) -> Color {
    if theme::dark() { [0.21, 0.21, 0.23, 1.0] } else { p.view }
}

/// Draw `layout` into a flipped view of its size, with row `highlighted`
/// highlighted.
fn draw(layout: &Layout, bounds: NSRect, highlighted: Option<usize>, dirty: NSRect) {
    let p = theme::palette();
    paint::fill_rect(dirty, ground(p));
    paint::stroke_round_rect(bounds, paint::radii(0.0), 1.0, p.card_border);
    let (top, bottom) = (dirty.origin.y, dirty.origin.y + dirty.size.height);
    let right = bounds.size.width - EDGE - SIDE;
    for (i, row) in layout.rows.iter().enumerate() {
        if row.top + row.height < top || row.top > bottom {
            continue;
        }
        match row.kind {
            Kind::Separator => {
                let y = (row.top + row.height / 2.0).floor();
                let width = bounds.size.width - 2.0 * EDGE - SIDE;
                paint::fill_rect(NSRect::new(NSPoint::new(EDGE + SIDE / 2.0, y), NSSize::new(width, 1.0)), p.separator);
                continue;
            }
            Kind::Header => {
                draw_text(layout, &row.title, layout.title_x + row.indent, row, p.secondary_label);
                continue;
            }
            Kind::Item => {}
        }
        let lit = highlighted == Some(i) && row.enabled;
        if lit {
            paint::fill_round_rect(layout.row_rect(i), paint::radii(5.0), p.accent);
        }
        let ink = if lit {
            p.accent_text_on
        } else if row.enabled {
            p.label
        } else {
            p.tertiary_label
        };
        if row.state != 0 && layout.image_x > layout.state_x {
            draw_state(layout, row, ink);
        }
        if let Some(image) = &row.image {
            let side = layout.image_side;
            let r =
                NSRect::new(NSPoint::new(layout.image_x, row.top + (row.height - side) / 2.0), NSSize::new(side, side));
            let fraction: f64 = if row.enabled { 1.0 } else { 0.5 };
            // SAFETY: drawInRect:fromRect:operation:fraction: takes two
            // rects, an operation and a fraction; a zero source rect is
            // the whole image.
            let _: () = unsafe {
                msg_send![&**image, drawInRect: r, fromRect: NSRect::ZERO, operation: NSCompositingOperation::SourceOver, fraction: fraction]
            };
        }
        draw_text(layout, &row.title, layout.title_x + row.indent, row, ink);
        if !row.key.is_empty() {
            let key_ink = if lit {
                ink
            } else if row.enabled {
                p.secondary_label
            } else {
                p.tertiary_label
            };
            let x = right - row.key_width - if layout.arrows { ARROW_COLUMN } else { 0.0 };
            draw_text(layout, &row.key, x, row, key_ink);
        }
        if row.submenu {
            draw_arrow(right, row, ink);
        }
    }
}

fn draw_text(layout: &Layout, text: &str, x: f64, row: &Row, color: Color) {
    if text.is_empty() {
        return;
    }
    let mut attrs = layout.attrs.clone();
    attrs.color = color;
    let y = row.top + ((row.height - layout.line) / 2.0).round();
    let r = NSRect::new(NSPoint::new(x, y), NSSize::new(layout.size.width - x, layout.line));
    paint::text(text, &attrs, r);
}

/// A check mark for on, a dash for mixed.
fn draw_state(layout: &Layout, row: &Row, ink: Color) {
    let cx = layout.state_x + STATE_COLUMN / 2.0 - 3.0;
    let cy = row.top + row.height / 2.0;
    if row.state > 0 {
        let points = [NSPoint::new(cx - 4.0, cy), NSPoint::new(cx - 1.0, cy + 3.5), NSPoint::new(cx + 5.0, cy - 4.0)];
        paint::stroke_polyline(&points, 1.8, ink);
    } else {
        paint::stroke_polyline(&[NSPoint::new(cx - 4.0, cy), NSPoint::new(cx + 4.0, cy)], 1.8, ink);
    }
}

/// A right-pointing chevron ending at `right`.
fn draw_arrow(right: f64, row: &Row, ink: Color) {
    let cy = row.top + row.height / 2.0;
    let x = right - 4.0;
    let points = [NSPoint::new(x - 3.5, cy - 4.0), NSPoint::new(x, cy), NSPoint::new(x - 3.5, cy + 4.0)];
    paint::stroke_polyline(&points, 1.6, ink);
}

// The view and its window.

pub(crate) struct ViewIvars {
    menu: Retained<NSMenu>,
    layout: RefCell<Option<Layout>>,
    min_width: Cell<f64>,
    /// The row showing the menu's highlighted item, in the layout kept.
    highlighted: Cell<Option<usize>>,
}

define_class!(
    // Draws a menu (see the module's description).
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepMenuView"]
    #[ivars = ViewIvars]
    pub(crate) struct MenuView;

    impl MenuView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            self.refresh();
            let layout = self.ivars().layout.borrow();
            if let Some(layout) = layout.as_ref() {
                draw(layout, self.bounds(), self.ivars().highlighted.get(), dirty);
            }
        }
    }

    unsafe impl NSObjectProtocol for MenuView {}
);

impl MenuView {
    pub(crate) fn new(mtm: MainThreadMarker, menu: &NSMenu, min_width: f64) -> Retained<MenuView> {
        crate::load_shell::<NSView>();
        let laid = layout(menu, min_width);
        let frame = NSRect::new(NSPoint::ZERO, laid.size);
        let this = MenuView::alloc(mtm).set_ivars(ViewIvars {
            menu: menu.retain(),
            layout: RefCell::new(Some(laid)),
            min_width: Cell::new(min_width),
            highlighted: Cell::new(None),
        });
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Lay the menu out again if it changed since, finding the row its
    /// highlighted item is now on; whether it did.
    pub(crate) fn refresh(&self) -> bool {
        let ivars = self.ivars();
        let generation = menu_ivars(&ivars.menu).generation();
        let min_width = ivars.min_width.get();
        let stale =
            ivars.layout.borrow().as_ref().is_none_or(|l| l.generation != generation || l.min_width != min_width);
        if !stale {
            return false;
        }
        let laid = layout(&ivars.menu, min_width);
        let lit = menu_ivars(&ivars.menu).highlighted_item();
        ivars.highlighted.set(lit.and_then(|item| laid.row_of_item(&ivars.menu, &item)));
        let old = ivars.layout.replace(Some(laid));
        drop(old);
        self.setNeedsDisplay(true);
        true
    }

    /// Make the menu at least `width` wide.
    pub(crate) fn set_min_width(&self, width: f64) {
        self.ivars().min_width.set(width);
        if self.refresh() {
            let size = self.with_layout(|l| l.size);
            self.setFrameSize(size);
        }
    }

    /// Run `f` with the layout, brought up to date.
    pub(crate) fn with_layout<R>(&self, f: impl FnOnce(&Layout) -> R) -> R {
        self.refresh();
        let layout = self.ivars().layout.borrow();
        f(layout.as_ref().expect("a menu view has a layout"))
    }

    /// The row shown highlighted, in the layout brought up to date.
    pub(crate) fn highlighted(&self) -> Option<usize> {
        self.refresh();
        self.ivars().highlighted.get()
    }

    /// Highlight row `row` (or none; a row past the last is none), making
    /// its item the menu's highlighted item and redrawing what changed;
    /// the item, if the highlight moved.
    pub(crate) fn set_highlighted(&self, row: Option<usize>) -> Option<Option<Retained<NSMenuItem>>> {
        let ivars = self.ivars();
        let (row, item, rects) = self.with_layout(|l| {
            let found = row.and_then(|r| Some((r, l.rows.get(r)?.item)));
            let row = found.map(|(r, _)| r);
            let old = ivars.highlighted.get();
            let rects: Vec<NSRect> = [old, row].into_iter().flatten().map(|r| l.row_rect(r)).collect();
            (row, found.and_then(|(_, index)| menu_ivars(&ivars.menu).item(index)), rects)
        });
        if ivars.highlighted.replace(row) == row {
            return None;
        }
        menu_ivars(&ivars.menu).set_highlighted(item.as_deref());
        for r in rects {
            self.setNeedsDisplayInRect(r);
        }
        Some(item)
    }

    /// Take the highlight away without telling anyone (the menu is
    /// closing); whether there was one.
    pub(crate) fn clear_highlight(&self) -> bool {
        self.ivars().highlighted.set(None);
        let had = menu_ivars(&self.ivars().menu).highlighted_item().is_some();
        menu_ivars(&self.ivars().menu).set_highlighted(None);
        had
    }
}

define_class!(
    // A menu's window: borderless, never key, following the pointer. It
    // tells the tracking loop when it leaves the screen, as it does when
    // the compositor dismisses the popup.
    #[unsafe(super(NSWindow, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "_SidestepMenuWindow"]
    pub(crate) struct MenuWindow;

    impl MenuWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }

        #[unsafe(method(orderOut:))]
        fn order_out(&self, sender: Option<&AnyObject>) {
            let was_visible = self.isVisible();
            // SAFETY: NSWindow's orderOut: takes a sender.
            let _: () = unsafe { msg_send![super(self), orderOut: sender] };
            if was_visible {
                crate::menu_tracking::window_left(self);
            }
        }
    }
);

/// A window showing `view`, sized to its menu.
pub(crate) fn window_for(mtm: MainThreadMarker, view: &MenuView) -> Retained<NSWindow> {
    crate::load_shell::<NSWindow>();
    let size = view.with_layout(|l| l.size);
    let frame = NSRect::new(NSPoint::ZERO, size);
    let this = MenuWindow::alloc(mtm).set_ivars(());
    // SAFETY: NSWindow's designated initializer.
    let window: Retained<MenuWindow> = unsafe {
        msg_send![
            super(this),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: true
        ]
    };
    let window: Retained<NSWindow> = Retained::into_super(window);
    // SAFETY: the tracking loop keeps its windows.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setAcceptsMouseMovedEvents(true);
    window.setContentView(Some(view));
    window
}
