//! `NSSearchField` and `NSSearchFieldCell`: a rounded field with a
//! magnifying glass at the leading end and, when it holds text, a clear
//! button at the trailing end.
//!
//! The rectangles are AppKit's (`conformance/tests/controls.rs`,
//! `search_fields`): the text runs from 22 points in to 25 points from the
//! end, a line tall and centered; the magnifier's rect is 16 by 9 points
//! at 6 points in, the clear button's 15 by 9 at 19 points from the end.
//! Clicking the clear button empties the field and sends the action;
//! clicking the magnifier sends the action. Recents, the menu template and
//! the sending options are kept for programs that set them.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSActionCell, NSButtonCell, NSCell, NSControl, NSEvent, NSResponder, NSTextField, NSTextFieldBezelStyle,
    NSTextFieldCell, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::cell::{Flags, imp as cell_imp};
use super::control;
use crate::theme::{self, parts};

/// The magnifier's and clear button's rects' size.
const SEARCH_BUTTON: (f64, f64) = (16.0, 9.0);
const CANCEL_BUTTON: (f64, f64) = (15.0, 9.0);
/// Where the text starts, and where it stops short of the end.
const TEXT_START: f64 = 22.0;
const TEXT_END: f64 = 25.0;

pub(crate) struct SearchCellIvars {
    search_button: RefCell<Option<Retained<NSButtonCell>>>,
    cancel_button: RefCell<Option<Retained<NSButtonCell>>>,
    menu_template: RefCell<Option<Retained<AnyObject>>>,
    whole_string: Cell<bool>,
    immediately: Cell<bool>,
    maximum_recents: Cell<isize>,
    recents: RefCell<Option<Retained<AnyObject>>>,
    autosave_name: RefCell<Option<Retained<NSString>>>,
}

define_class!(
    #[unsafe(super(NSTextFieldCell, NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSearchFieldCell"]
    #[ivars = SearchCellIvars]
    pub(crate) struct NSSearchFieldCellImpl;

    impl NSSearchFieldCellImpl {
        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(SearchCellIvars {
                search_button: RefCell::new(None),
                cancel_button: RefCell::new(None),
                menu_template: RefCell::new(None),
                whole_string: Cell::new(false),
                immediately: Cell::new(false),
                maximum_recents: Cell::new(-1),
                recents: RefCell::new(None),
                autosave_name: RefCell::new(None),
            });
            // SAFETY: NSTextFieldCell's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initTextCell: string] };
            let cell: &NSTextFieldCell = &this;
            cell.setBezelStyle(NSTextFieldBezelStyle::RoundedBezel);
            cell.setBezeled(true);
            cell.setEditable(true);
            cell.setScrollable(false);
            cell.setWraps(false);
            cell.setUsesSingleLineMode(true);
            cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByClipping);
            this
        }

        #[unsafe(method_id(searchButtonCell))]
        fn search_button_cell(&self) -> Option<Retained<NSButtonCell>> {
            self.ivars().search_button.borrow().clone()
        }

        #[unsafe(method(setSearchButtonCell:))]
        fn set_search_button_cell(&self, cell: Option<&NSButtonCell>) {
            self.ivars().search_button.replace(cell.map(|c| c.retain()));
        }

        #[unsafe(method_id(cancelButtonCell))]
        fn cancel_button_cell(&self) -> Option<Retained<NSButtonCell>> {
            self.ivars().cancel_button.borrow().clone()
        }

        #[unsafe(method(setCancelButtonCell:))]
        fn set_cancel_button_cell(&self, cell: Option<&NSButtonCell>) {
            self.ivars().cancel_button.replace(cell.map(|c| c.retain()));
        }

        #[unsafe(method(resetSearchButtonCell))]
        fn reset_search_button_cell(&self) {
            self.ivars().search_button.replace(None);
        }

        #[unsafe(method(resetCancelButtonCell))]
        fn reset_cancel_button_cell(&self) {
            self.ivars().cancel_button.replace(None);
        }

        #[unsafe(method(searchTextRectForBounds:))]
        fn search_text_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            text_rect(self, bounds)
        }

        #[unsafe(method(searchButtonRectForBounds:))]
        fn search_button_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            search_button_rect(bounds)
        }

        #[unsafe(method(cancelButtonRectForBounds:))]
        fn cancel_button_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            cancel_button_rect(bounds)
        }

        #[unsafe(method(titleRectForBounds:))]
        fn title_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            text_rect(self, bounds)
        }

        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            text_rect(self, bounds)
        }

        #[unsafe(method(drawInteriorWithFrame:inView:))]
        fn draw_interior_with_frame(&self, frame: NSRect, view: &NSView) {
            // SAFETY: NSTextFieldCell's own interior: the text.
            let _: () = unsafe { msg_send![super(self), drawInteriorWithFrame: frame, inView: view] };
            draw_glyphs(self, frame, view);
        }

        #[unsafe(method_id(searchMenuTemplate))]
        fn search_menu_template(&self) -> Option<Retained<AnyObject>> {
            self.ivars().menu_template.borrow().clone()
        }

        #[unsafe(method(setSearchMenuTemplate:))]
        fn set_search_menu_template(&self, menu: Option<&AnyObject>) {
            self.ivars().menu_template.replace(menu.map(|m| m.retain()));
        }

        #[unsafe(method(sendsWholeSearchString))]
        fn sends_whole_search_string(&self) -> bool {
            self.ivars().whole_string.get()
        }

        #[unsafe(method(setSendsWholeSearchString:))]
        fn set_sends_whole_search_string(&self, flag: bool) {
            self.ivars().whole_string.set(flag);
        }

        #[unsafe(method(sendsSearchStringImmediately))]
        fn sends_search_string_immediately(&self) -> bool {
            self.ivars().immediately.get()
        }

        #[unsafe(method(setSendsSearchStringImmediately:))]
        fn set_sends_search_string_immediately(&self, flag: bool) {
            self.ivars().immediately.set(flag);
        }

        #[unsafe(method(maximumRecents))]
        fn maximum_recents(&self) -> isize {
            self.ivars().maximum_recents.get()
        }

        #[unsafe(method(setMaximumRecents:))]
        fn set_maximum_recents(&self, count: isize) {
            self.ivars().maximum_recents.set(count);
        }

        #[unsafe(method_id(recentSearches))]
        fn recent_searches(&self) -> Retained<AnyObject> {
            let set = self.ivars().recents.borrow().clone();
            set.unwrap_or_else(|| crate::app::array_of::<AnyObject>(&[]))
        }

        #[unsafe(method(setRecentSearches:))]
        fn set_recent_searches(&self, searches: Option<&AnyObject>) {
            self.ivars().recents.replace(searches.map(|s| s.retain()));
        }

        #[unsafe(method_id(recentsAutosaveName))]
        fn recents_autosave_name(&self) -> Option<Retained<NSString>> {
            self.ivars().autosave_name.borrow().clone()
        }

        #[unsafe(method(setRecentsAutosaveName:))]
        fn set_recents_autosave_name(&self, name: Option<&NSString>) {
            self.ivars().autosave_name.replace(name.map(|n| n.retain()));
        }
    }

    unsafe impl NSObjectProtocol for NSSearchFieldCellImpl {}
);

/// The text's rect: between the magnifier and the clear button, a line
/// tall, centered.
fn text_rect(cell: &NSSearchFieldCellImpl, b: NSRect) -> NSRect {
    let base = cell_imp(as_cell(cell));
    let font = super::cell::font_of(base);
    let lh = super::cell::Styled::plain(String::new(), super::cell::attrs(base, &font, [0.0; 4])).size(None).height;
    NSRect::new(
        NSPoint::new(b.origin.x + TEXT_START, b.origin.y + ((b.size.height - lh) / 2.0).floor()),
        NSSize::new((b.size.width - TEXT_START - TEXT_END).max(0.0), lh),
    )
}

fn search_button_rect(b: NSRect) -> NSRect {
    let (w, h) = SEARCH_BUTTON;
    NSRect::new(NSPoint::new(b.origin.x + 6.0, b.origin.y + ((b.size.height - h) / 2.0).round()), NSSize::new(w, h))
}

fn cancel_button_rect(b: NSRect) -> NSRect {
    let (w, h) = CANCEL_BUTTON;
    NSRect::new(
        NSPoint::new(b.origin.x + b.size.width - 19.0, b.origin.y + ((b.size.height - h) / 2.0).round()),
        NSSize::new(w, h),
    )
}

fn as_cell(cell: &NSSearchFieldCellImpl) -> &NSCell {
    // SAFETY: NSSearchFieldCell is a subclass of NSCell.
    unsafe { &*(cell as *const NSSearchFieldCellImpl).cast::<NSCell>() }
}

/// The magnifier, and the clear button when there's text.
fn draw_glyphs(cell: &NSSearchFieldCellImpl, frame: NSRect, view: &NSView) {
    if !theme::paint::recording() {
        return;
    }
    let p = theme::palette();
    let base = cell_imp(as_cell(cell));
    let disabled = !base.has(Flags::ENABLED);
    let ink = if disabled { theme::palette::dimmed(p.secondary_label) } else { p.secondary_label };
    let axis = parts::Axis { flipped: view.isFlipped() };
    // A lens and a handle, centered on the button's rect.
    let r = search_button_rect(frame);
    let c = NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0);
    let lens = NSRect::new(NSPoint::new(c.x - 4.5, axis.down(c.y, -1.0) - 4.5), NSSize::new(9.0, 9.0));
    theme::paint::stroke_ellipse(lens, 1.5, ink);
    theme::paint::stroke_polyline(
        &[NSPoint::new(c.x + 2.8, axis.down(c.y, 1.8)), NSPoint::new(c.x + 5.5, axis.down(c.y, 4.5))],
        1.5,
        ink,
    );
    if base.value().string().length() > 0 {
        let r = cancel_button_rect(frame);
        let c = NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0);
        let disc = NSRect::new(NSPoint::new(c.x - 7.0, c.y - 7.0), NSSize::new(14.0, 14.0));
        theme::paint::fill_ellipse(disc, theme::palette::faded(ink, 0.6));
        let d = 2.5;
        let knock = if theme::dark() { p.window } else { p.view };
        theme::paint::stroke_polyline(&[NSPoint::new(c.x - d, c.y - d), NSPoint::new(c.x + d, c.y + d)], 1.5, knock);
        theme::paint::stroke_polyline(&[NSPoint::new(c.x - d, c.y + d), NSPoint::new(c.x + d, c.y - d)], 1.5, knock);
    }
}

// NSSearchField

#[derive(Default)]
pub(crate) struct SearchFieldIvars {
    centers_placeholder: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSTextField, NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSearchField"]
    #[ivars = SearchFieldIvars]
    pub(crate) struct NSSearchFieldImpl;

    impl NSSearchFieldImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(SearchFieldIvars::default());
            // SAFETY: NSTextField's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            let field: &NSTextField = &this;
            // The cell's own settings stand: no background of its own.
            field.setDrawsBackground(false);
            field.setBezelStyle(NSTextFieldBezelStyle::RoundedBezel);
            this
        }

        #[unsafe(method(searchTextBounds))]
        fn search_text_bounds(&self) -> NSRect {
            self.cell_rect(|c, b| c.searchTextRectForBounds(b))
        }

        #[unsafe(method(searchButtonBounds))]
        fn search_button_bounds(&self) -> NSRect {
            self.cell_rect(|c, b| c.searchButtonRectForBounds(b))
        }

        #[unsafe(method(cancelButtonBounds))]
        fn cancel_button_bounds(&self) -> NSRect {
            self.cell_rect(|c, b| c.cancelButtonRectForBounds(b))
        }

        #[unsafe(method(rectForSearchTextWhenCentered:))]
        fn rect_for_search_text_when_centered(&self, _centered: bool) -> NSRect {
            self.cell_rect(|c, b| c.searchTextRectForBounds(b))
        }

        #[unsafe(method(rectForSearchButtonWhenCentered:))]
        fn rect_for_search_button_when_centered(&self, _centered: bool) -> NSRect {
            self.cell_rect(|c, b| c.searchButtonRectForBounds(b))
        }

        #[unsafe(method(rectForCancelButtonWhenCentered:))]
        fn rect_for_cancel_button_when_centered(&self, _centered: bool) -> NSRect {
            self.cell_rect(|c, b| c.cancelButtonRectForBounds(b))
        }

        #[unsafe(method(centersPlaceholder))]
        fn centers_placeholder(&self) -> bool {
            self.ivars().centers_placeholder.get()
        }

        #[unsafe(method(setCentersPlaceholder:))]
        fn set_centers_placeholder(&self, flag: bool) {
            self.ivars().centers_placeholder.set(flag);
        }

        #[unsafe(method_id(recentSearches))]
        fn recent_searches(&self) -> Retained<AnyObject> {
            // SAFETY: the cell's recentSearches returns an array.
            self.search_cell().map_or_else(|| crate::app::array_of::<AnyObject>(&[]), |c| unsafe { msg_send![&*c, recentSearches] })
        }

        #[unsafe(method(setRecentSearches:))]
        fn set_recent_searches(&self, searches: &AnyObject) {
            if let Some(c) = self.search_cell() {
                // SAFETY: setRecentSearches: takes an array.
                let _: () = unsafe { msg_send![&*c, setRecentSearches: searches] };
            }
        }

        #[unsafe(method_id(recentsAutosaveName))]
        fn recents_autosave_name(&self) -> Option<Retained<NSString>> {
            self.search_cell().and_then(|c| c.recentsAutosaveName())
        }

        #[unsafe(method(setRecentsAutosaveName:))]
        fn set_recents_autosave_name(&self, name: Option<&NSString>) {
            if let Some(c) = self.search_cell() {
                c.setRecentsAutosaveName(name);
            }
        }

        #[unsafe(method_id(searchMenuTemplate))]
        fn search_menu_template(&self) -> Option<Retained<AnyObject>> {
            // SAFETY: the cell's searchMenuTemplate returns a menu or nil.
            self.search_cell().and_then(|c| unsafe { msg_send![&*c, searchMenuTemplate] })
        }

        #[unsafe(method(setSearchMenuTemplate:))]
        fn set_search_menu_template(&self, menu: Option<&AnyObject>) {
            if let Some(c) = self.search_cell() {
                // SAFETY: setSearchMenuTemplate: takes a menu or nil.
                let _: () = unsafe { msg_send![&*c, setSearchMenuTemplate: menu] };
            }
        }

        #[unsafe(method(sendsWholeSearchString))]
        fn sends_whole_search_string(&self) -> bool {
            self.search_cell().is_some_and(|c| c.sendsWholeSearchString())
        }

        #[unsafe(method(setSendsWholeSearchString:))]
        fn set_sends_whole_search_string(&self, flag: bool) {
            if let Some(c) = self.search_cell() {
                c.setSendsWholeSearchString(flag);
            }
        }

        #[unsafe(method(maximumRecents))]
        fn maximum_recents(&self) -> isize {
            self.search_cell().map_or(-1, |c| c.maximumRecents())
        }

        #[unsafe(method(setMaximumRecents:))]
        fn set_maximum_recents(&self, count: isize) {
            if let Some(c) = self.search_cell() {
                c.setMaximumRecents(count);
            }
        }

        #[unsafe(method(sendsSearchStringImmediately))]
        fn sends_search_string_immediately(&self) -> bool {
            self.search_cell().is_some_and(|c| c.sendsSearchStringImmediately())
        }

        #[unsafe(method(setSendsSearchStringImmediately:))]
        fn set_sends_search_string_immediately(&self, flag: bool) {
            if let Some(c) = self.search_cell() {
                c.setSendsSearchStringImmediately(flag);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let view: &NSView = self.as_field();
            let at = control::event_point(view, event);
            let flipped = view.isFlipped();
            let control: &NSControl = self.as_field();
            let bounds = view.bounds();
            let has_text = control.stringValue().length() > 0;
            if has_text && super::track::mouse_in_rect(at, cancel_button_rect(bounds), flipped) {
                // The clear button empties the field and says so.
                control.setStringValue(&NSString::new());
                // SAFETY: the field's own action and target.
                let _ = unsafe { control.sendAction_to(control.action(), control.target().as_deref()) };
                return;
            }
            if super::track::mouse_in_rect(at, search_button_rect(bounds), flipped) {
                // SAFETY: as above.
                let _ = unsafe { control.sendAction_to(control.action(), control.target().as_deref()) };
                return;
            }
            // SAFETY: NSTextField's mouseDown: starts editing.
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
        }
    }
);

impl NSSearchFieldImpl {
    fn as_field(&self) -> &NSTextField {
        // SAFETY: NSSearchField is a subclass of NSTextField.
        unsafe { &*(self as *const Self).cast::<NSTextField>() }
    }

    fn search_cell(&self) -> Option<Retained<objc2_app_kit::NSSearchFieldCell>> {
        let control: &NSControl = self.as_field();
        let cell = control.cell()?;
        let target = <objc2_app_kit::NSSearchFieldCell as objc2::ClassType>::class();
        let mut class = Some((&*cell as &AnyObject).class());
        while let Some(c) = class {
            if std::ptr::eq(c, target) {
                // SAFETY: the cell's class descends from NSSearchFieldCell.
                return Some(unsafe { Retained::cast_unchecked(cell) });
            }
            class = c.superclass();
        }
        None
    }

    fn cell_rect(&self, f: impl FnOnce(&objc2_app_kit::NSSearchFieldCell, NSRect) -> NSRect) -> NSRect {
        let view: &NSView = self.as_field();
        self.search_cell().map_or(NSRect::ZERO, |c| f(&c, view.bounds()))
    }
}
