//! Text fields, as displays: `NSTextField`, `NSTextFieldCell`,
//! `NSSecureTextField` and its cell.
//!
//! A label is a text field that's neither editable, selectable, bezeled
//! nor bordered, and draws no background; the factories set those up as
//! AppKit's do. Sizes are AppKit's (`conformance/tests/controls.rs`): the
//! text plus 2 points each side, plus a bezel's 4 or a border's 2 all
//! round. A label's intrinsic size leaves the 2-point padding out (it's
//! outside Auto Layout's alignment rect); an editable field has no
//! intrinsic width. A wrapping label with a preferred width lays out that
//! wide, at most `maximumNumberOfLines` lines.
//!
//! Text is drawn with the text engine in the title rect, clipped to it, in
//! the text color (the label color unless set; dimmed when disabled), or
//! the placeholder in the placeholder color when there's no text. A
//! secure field draws one bullet per character it holds (combining marks
//! and joined sequences count with what they join).
//!
//! Editing belongs to the field editor, which the text-editing work
//! provides. The hooks at the end of this file are where it connects: a
//! cell's `editWithFrame:…`, `selectWithFrame:…` and `endEditing:`, a
//! control's `currentEditor`, `abortEditing` and `validateEditing`, and a
//! field becoming first responder (which selects its text). They do
//! nothing yet, and `currentEditor` answers nil. The field editor tells
//! the field what happens through `textShouldBeginEditing:`,
//! `textDidBeginEditing:`, `textDidChange:`, `textShouldEndEditing:` and
//! `textDidEndEditing:`, which ask and tell the delegate and post
//! `NSControlTextDid…Notification`, as AppKit's do
//! (`conformance/tests/control_events.rs`, `text_notifications`): one
//! notification from the field, whose user info holds the field editor
//! (`NSFieldEditor`) and, at the end, how editing ended
//! (`NSTextMovement`), goes to the delegate first and then to the center.
//! `textFieldWithString:` fields send their action whenever editing ends.
//!
//! `sizeThatFits:` lays the text out in the width offered (none, if it
//! offers none) and answers no wider than the text; `cellSizeForBounds:`
//! answers the text's size within the bounds, except that a bezeled cell
//! takes the bounds' width and its text's height (`controls.rs`,
//! `sizes_that_fit`).

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSActionCell, NSBackgroundStyle, NSCell, NSColor, NSControl, NSEvent, NSLineBreakMode, NSResponder,
    NSTextAlignment, NSTextField, NSTextFieldBezelStyle, NSTextFieldCell, NSView,
};
use objc2_foundation::{
    NSAttributedString, NSCopying, NSDictionary, NSNotification, NSPoint, NSRect, NSSize, NSString, NSZone,
};

use super::cell::{self, Flags, NSCellImpl, Styled, imp as cell_imp};
use super::control;
use super::value::{self, Value};
use crate::protocol::Color;
use crate::theme::{self, metrics, parts};

// NSTextFieldCell

pub(crate) struct FieldCellIvars {
    text_color: RefCell<Option<Retained<NSColor>>>,
    background: RefCell<Option<Retained<NSColor>>>,
    draws_background: Cell<bool>,
    bezel_style: Cell<NSTextFieldBezelStyle>,
    placeholder: RefCell<Option<Value>>,
    locales: RefCell<Option<Retained<AnyObject>>>,
    /// What the secure cell subclass adds: draw bullets for the text.
    secure: Cell<bool>,
    echos_bullets: Cell<bool>,
}

impl FieldCellIvars {
    fn new() -> Self {
        FieldCellIvars {
            text_color: RefCell::new(None),
            background: RefCell::new(None),
            draws_background: Cell::new(false),
            bezel_style: Cell::new(NSTextFieldBezelStyle::SquareBezel),
            placeholder: RefCell::new(None),
            locales: RefCell::new(None),
            secure: Cell::new(false),
            echos_bullets: Cell::new(true),
        }
    }
}

define_class!(
    #[unsafe(super(NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTextFieldCell"]
    #[ivars = FieldCellIvars]
    pub(crate) struct NSTextFieldCellImpl;

    impl NSTextFieldCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(FieldCellIvars::new());
            // SAFETY: NSActionCell's initializer.
            unsafe { msg_send![super(this), initTextCell: string] }
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, _image: Option<&AnyObject>) -> Retained<Self> {
            // Text field cells hold text.
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::new()] }
        }

        #[unsafe(method_id(textColor))]
        fn text_color(&self) -> Option<Retained<NSColor>> {
            let set = self.ivars().text_color.borrow().clone();
            set.or_else(|| Some(theme::system_color(sel!(controlTextColor), |p| p.label)))
        }

        #[unsafe(method(setTextColor:))]
        fn set_text_color(&self, color: Option<&NSColor>) {
            let old = self.ivars().text_color.replace(color.map(|c| c.retain()));
            if !super::button::same_color(old.as_deref(), color) {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Option<Retained<NSColor>> {
            let set = self.ivars().background.borrow().clone();
            set.or_else(|| Some(theme::system_color(sel!(textBackgroundColor), |p| p.view)))
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: Option<&NSColor>) {
            let old = self.ivars().background.replace(color.map(|c| c.retain()));
            if !super::button::same_color(old.as_deref(), color) {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method(drawsBackground))]
        fn draws_background(&self) -> bool {
            self.ivars().draws_background.get()
        }

        #[unsafe(method(setDrawsBackground:))]
        fn set_draws_background(&self, flag: bool) {
            if self.ivars().draws_background.replace(flag) != flag {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method(bezelStyle))]
        fn bezel_style(&self) -> NSTextFieldBezelStyle {
            self.ivars().bezel_style.get()
        }

        #[unsafe(method(setBezelStyle:))]
        fn set_bezel_style(&self, style: NSTextFieldBezelStyle) {
            if self.ivars().bezel_style.replace(style) != style {
                cell::changed(self.base());
            }
        }

        #[unsafe(method_id(placeholderString))]
        fn placeholder_string(&self) -> Option<Retained<NSString>> {
            let placeholder = self.ivars().placeholder.borrow().clone();
            placeholder.as_ref().map(Value::string)
        }

        #[unsafe(method(setPlaceholderString:))]
        fn set_placeholder_string(&self, string: Option<&NSString>) {
            set_placeholder(self, string.map(|s| Value::String(s.copy())));
        }

        #[unsafe(method_id(placeholderAttributedString))]
        fn placeholder_attributed_string(&self) -> Option<Retained<NSAttributedString>> {
            self.ivars().placeholder.borrow().as_ref().and_then(|v| v.attributed().map(|a| a.retain()))
        }

        #[unsafe(method(setPlaceholderAttributedString:))]
        fn set_placeholder_attributed_string(&self, string: Option<&NSAttributedString>) {
            set_placeholder(self, string.map(|s| Value::from_object(Some(s))));
        }

        #[unsafe(method(setWantsNotificationForMarkedText:))]
        fn set_wants_notification_for_marked_text(&self, _flag: bool) {}

        #[unsafe(method_id(allowedInputSourceLocales))]
        fn allowed_input_source_locales(&self) -> Option<Retained<AnyObject>> {
            self.ivars().locales.borrow().clone()
        }

        #[unsafe(method(setAllowedInputSourceLocales:))]
        fn set_allowed_input_source_locales(&self, locales: Option<&AnyObject>) {
            self.ivars().locales.replace(locales.map(|l| l.retain()));
        }

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            field_cell_size(self, None)
        }

        #[unsafe(method(cellSizeForBounds:))]
        fn cell_size_for_bounds(&self, bounds: NSRect) -> NSSize {
            let size = field_cell_size(self, Some(bounds.size.width));
            if self.base().has(Flags::BEZELED) {
                // The bounds' width (at least the bezel's), the text's
                // height.
                let pad = 2.0 * (metrics::TEXT_PADDING + metrics::FIELD_BEZEL_INSET);
                NSSize::new(bounds.size.width.max(pad), size.height)
            } else {
                NSSize::new(size.width.min(bounds.size.width), size.height.min(bounds.size.height))
            }
        }

        #[unsafe(method_id(attributedStringValue))]
        fn attributed_string_value(&self) -> Retained<NSAttributedString> {
            field_attributed(self)
        }

        #[unsafe(method(titleRectForBounds:))]
        fn title_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            field_title_rect(self, bounds)
        }

        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            field_title_rect(self, bounds)
        }

        /// A field that draws its own background (a bezel, or a background
        /// color) keeps its text as it is on any background; a label's
        /// text is on its superview's, as on macOS
        /// (`conformance/tests/cell_backgrounds.rs`, `interior_styles`).
        #[unsafe(method(interiorBackgroundStyle))]
        fn interior_background_style(&self) -> NSBackgroundStyle {
            if self.base().has(Flags::BEZELED) || self.ivars().draws_background.get() {
                NSBackgroundStyle::Normal
            } else {
                cell::as_cell(self.base()).backgroundStyle()
            }
        }

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, view: &NSView) {
            if !theme::paint::recording() {
                return;
            }
            draw_field_frame(self, frame, view);
            // SAFETY: drawInteriorWithFrame:inView: takes a rect and a view.
            let _: () = unsafe { msg_send![self, drawInteriorWithFrame: frame, inView: view] };
        }

        #[unsafe(method(drawInteriorWithFrame:inView:))]
        fn draw_interior_with_frame(&self, frame: NSRect, view: &NSView) {
            // SAFETY: titleRectForBounds: takes and returns a rect.
            let title: NSRect = unsafe { msg_send![self, titleRectForBounds: frame] };
            draw_field_text(self, title, view);
        }

        #[unsafe(method_id(setUpFieldEditorAttributes:))]
        fn set_up_field_editor_attributes(&self, editor: &AnyObject) -> Retained<AnyObject> {
            editor.retain()
        }

        // NSSecureTextFieldCell's setting, kept here so the one draw path
        // serves both.

        #[unsafe(method(echosBullets))]
        fn echos_bullets(&self) -> bool {
            self.ivars().echos_bullets.get()
        }

        #[unsafe(method(setEchosBullets:))]
        fn set_echos_bullets(&self, flag: bool) {
            if self.ivars().echos_bullets.replace(flag) != flag {
                cell::changed(self.base());
            }
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, zone: *mut NSZone) -> Retained<NSCell> {
            // SAFETY: NSActionCell's copyWithZone: returns a cell of this
            // class.
            let copy: Retained<NSCell> = unsafe { msg_send![super(self), copyWithZone: zone] };
            // SAFETY: the copy is an instance of the receiver's class.
            let theirs = unsafe { &*(Retained::as_ptr(&copy).cast::<NSTextFieldCellImpl>()) };
            let (mine, copied) = (self.ivars(), theirs.ivars());
            copied.text_color.replace(mine.text_color.borrow().clone());
            copied.background.replace(mine.background.borrow().clone());
            copied.draws_background.set(mine.draws_background.get());
            copied.bezel_style.set(mine.bezel_style.get());
            copied.placeholder.replace(mine.placeholder.borrow().clone());
            copied.locales.replace(mine.locales.borrow().clone());
            copied.secure.set(mine.secure.get());
            copied.echos_bullets.set(mine.echos_bullets.get());
            copy
        }
    }

    unsafe impl NSObjectProtocol for NSTextFieldCellImpl {}
);

impl NSTextFieldCellImpl {
    fn base(&self) -> &NSCellImpl {
        // SAFETY: NSTextFieldCell is a subclass of NSCell.
        cell_imp(unsafe { &*(self as *const Self).cast::<NSCell>() })
    }
}

/// Any text field cell as the implementation.
pub(crate) fn field_cell(cell: &NSCell) -> Option<&NSTextFieldCellImpl> {
    // SAFETY: NSTextFieldCellImpl is the class NSTextFieldCell names.
    unsafe { super::impl_of::<NSTextFieldCell, NSTextFieldCellImpl>(cell) }
}

/// `attributedStringValue`: an attributed value as it is; a plain one
/// with the cell's font, alignment, line breaking and text color, as on
/// macOS (`conformance/tests/controls.rs`, `attributed_values`).
fn field_attributed(cell: &NSTextFieldCellImpl) -> Retained<NSAttributedString> {
    let base = cell.base();
    if let Value::Attributed(a) = base.value() {
        return a;
    }
    let c = cell::as_cell(base);
    let color = cell.ivars().text_color.borrow().clone();
    let color = color.unwrap_or_else(|| cell::text_color(default_text_color(cell)));
    cell::attributed(&cell::text_of(base), &cell::font_of(base), c.alignment(), c.lineBreakMode(), &color)
}

/// A new placeholder, unless it's the one there.
fn set_placeholder(cell: &NSTextFieldCellImpl, placeholder: Option<Value>) {
    let old = cell.ivars().placeholder.borrow().clone();
    let same = match (&old, &placeholder) {
        (Some(a), Some(b)) => a.same(b),
        (None, None) => true,
        _ => false,
    };
    if !same {
        cell.ivars().placeholder.replace(placeholder);
        cell::changed(cell.base());
    }
}

/// The color a field's text is when none was set: a label's is the label
/// color, other fields' the control text color, as on macOS.
fn default_text_color(cell: &NSTextFieldCellImpl) -> objc2::runtime::Sel {
    let base = cell.base();
    if base.has(Flags::EDITABLE) || base.has(Flags::BEZELED) || base.has(Flags::BORDERED) {
        sel!(controlTextColor)
    } else {
        sel!(labelColor)
    }
}

/// Whether the field shows its placeholder: it has one, and no text.
fn shows_placeholder(cell: &NSTextFieldCellImpl) -> bool {
    let has = cell.ivars().placeholder.borrow().is_some();
    has && cell.base().value().is_empty_text()
}

/// The text (or placeholder) styled for measuring or drawing.
fn field_styled(cell: &NSTextFieldCellImpl, color: Color) -> Styled {
    let base = cell.base();
    let font = cell::font_of(base);
    let attrs = cell::attrs(base, &font, color);
    if shows_placeholder(cell) {
        let placeholder = cell.ivars().placeholder.borrow().clone().unwrap_or_default();
        return Styled::of(&placeholder, attrs);
    }
    if cell.ivars().secure.get() && cell.ivars().echos_bullets.get() {
        return Styled::plain(bullets(&cell::text_of(base).to_string()), attrs);
    }
    cell::styled_with(base, attrs)
}

/// The text a secure cell draws: a bullet for each character, where a
/// character takes in the marks and joined characters that follow it.
fn bullets(text: &str) -> String {
    use icu_properties::props::GeneralCategory;
    use icu_properties::{CodePointMapData, props::GeneralCategoryGroup};
    let categories = CodePointMapData::<GeneralCategory>::new();
    let mut count = 0;
    let mut joined = false;
    for c in text.chars() {
        let mark = GeneralCategoryGroup::Mark.contains(categories.get(c));
        let variation = ('\u{FE00}'..='\u{FE0F}').contains(&c);
        if c == '\u{200D}' {
            joined = true;
            continue;
        }
        if !(mark || variation || joined) {
            count += 1;
        }
        joined = false;
    }
    "\u{2022}".repeat(count)
}

/// `cellSize` (and `cellSizeForBounds:` with `width`): the text plus 2 each
/// side, plus a bezel's 4 (the whole rounded up) or a border's 2 all round.
fn field_cell_size(cell: &NSTextFieldCellImpl, width: Option<f64>) -> NSSize {
    let base = cell.base();
    let (pad_w, pad_h, round) = if base.has(Flags::BEZELED) {
        (2.0 * (metrics::TEXT_PADDING + metrics::FIELD_BEZEL_INSET), 2.0 * metrics::FIELD_BEZEL_INSET, true)
    } else if base.has(Flags::BORDERED) {
        (2.0 * (metrics::TEXT_PADDING + metrics::BORDER_INSET), metrics::FIELD_BORDER_HEIGHT, false)
    } else {
        (2.0 * metrics::TEXT_PADDING, 0.0, false)
    };
    let wraps = base.has(Flags::WRAPS) && !base.has(Flags::SINGLE_LINE);
    let text = field_styled(cell, [0.0; 4]).size(width.filter(|_| wraps).map(|w| w - pad_w));
    let w = text.width + pad_w;
    NSSize::new(if round { w.ceil() } else { w }, text.height + pad_h)
}

/// The line height of the field's font.
fn line_height(cell: &NSTextFieldCellImpl) -> f64 {
    let base = cell.base();
    let font = cell::font_of(base);
    Styled::plain(String::new(), cell::attrs(base, &font, [0.0; 4])).size(None).height
}

/// `titleRectForBounds:`: 4 in for a bezel (at least a line tall,
/// centered when there isn't room), a border's odd (2, 3, 2, 2), or the
/// bounds.
fn field_title_rect(cell: &NSTextFieldCellImpl, b: NSRect) -> NSRect {
    let base = cell.base();
    if base.has(Flags::BEZELED) {
        let inset = metrics::FIELD_BEZEL_INSET;
        let lh = line_height(cell);
        let (y, h) = if b.size.height - 2.0 * inset >= lh {
            (b.origin.y + inset, b.size.height - 2.0 * inset)
        } else {
            (b.origin.y + ((b.size.height - lh) / 2.0).floor(), lh)
        };
        NSRect::new(NSPoint::new(b.origin.x + inset, y), NSSize::new(b.size.width - 2.0 * inset, h))
    } else if base.has(Flags::BORDERED) {
        NSRect::new(
            NSPoint::new(b.origin.x + 2.0, b.origin.y + 3.0),
            NSSize::new(b.size.width - 4.0, b.size.height - 5.0),
        )
    } else {
        b
    }
}

fn draw_field_frame(cell: &NSTextFieldCellImpl, frame: NSRect, _view: &NSView) {
    let p = theme::palette();
    let base = cell.base();
    let state = parts::State { disabled: !base.has(Flags::ENABLED), ..parts::State::default() };
    let background = cell.ivars().background.borrow().clone();
    let background = background.as_deref().map(theme::color_of);
    let draws = cell.ivars().draws_background.get();
    if base.has(Flags::BEZELED) {
        let rounded = cell.ivars().bezel_style.get() == NSTextFieldBezelStyle::RoundedBezel;
        let bg = if draws { background } else { None };
        if rounded {
            let r = theme::paint::radii(frame.size.height / 2.0);
            if let Some(bg) = bg {
                theme::paint::fill_round_rect(frame, r, bg);
            }
            theme::paint::fill_round_rect(
                frame,
                r,
                if state.disabled { theme::palette::dimmed(p.entry) } else { p.entry },
            );
        } else {
            parts::entry(p, frame, bg, state);
        }
    } else {
        if draws {
            theme::paint::fill_rect(frame, background.unwrap_or(p.view));
        }
        if base.has(Flags::BORDERED) {
            parts::plain_border(p, frame, state);
        }
    }
}

/// Draw the text, or the placeholder, in the title rect. On an emphasized
/// background (a label in a selected row of the key window's focused
/// table) the label colors in the text color and in an attributed value's
/// runs turn light, and so does a text color with the label color's value,
/// while other colors and the placeholder stay, as on macOS
/// (`conformance/tests/cell_backgrounds.rs`, `emphasized_text`).
fn draw_field_text(cell: &NSTextFieldCellImpl, title: NSRect, _view: &NSView) {
    if !theme::paint::recording() {
        return;
    }
    let p = theme::palette();
    let base = cell.base();
    let placeholder = shows_placeholder(cell);
    let emphasized = !placeholder && cell::draws_on_emphasis(base);
    let text_color = cell.ivars().text_color.borrow().clone();
    let mut color = match text_color.as_deref() {
        _ if placeholder => p.tertiary_label,
        Some(c) if emphasized => crate::color::resolve_emphasized_text(c),
        Some(c) => theme::color_of(c),
        None if emphasized => theme::emphasized_color(crate::palette::System::Label),
        None => p.label,
    };
    if !base.has(Flags::ENABLED) {
        color = theme::palette::dimmed(color);
    }
    let styled = crate::color::with_emphasis(emphasized, || field_styled(cell, color));
    let r = NSRect::new(
        NSPoint::new(title.origin.x + metrics::TEXT_PADDING, title.origin.y),
        NSSize::new(title.size.width - 2.0 * metrics::TEXT_PADDING, title.size.height),
    );
    styled.draw(r);
}

// NSSecureTextFieldCell

define_class!(
    #[unsafe(super(NSTextFieldCell, NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSecureTextFieldCell"]
    pub(crate) struct NSSecureTextFieldCellImpl;

    impl NSSecureTextFieldCellImpl {
        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(());
            // SAFETY: NSTextFieldCell's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initTextCell: string] };
            // SAFETY: the object is an NSTextFieldCell.
            let cell = field_cell(unsafe { &*(Retained::as_ptr(&this) as *const NSCell) }).expect("a text field cell");
            cell.ivars().secure.set(true);
            this
        }
    }
);

define_class!(
    #[unsafe(super(NSTextField, NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSSecureTextField"]
    pub(crate) struct NSSecureTextFieldImpl;
);

// NSTextField

pub(crate) struct FieldIvars {
    delegate: RefCell<Weak<AnyObject>>,
    preferred_width: Cell<f64>,
    max_lines: Cell<isize>,
    tightening: Cell<bool>,
    line_break_strategy: Cell<usize>,
    completion: Cell<bool>,
    writing_tools: Cell<(bool, bool)>,
    character_picker: Cell<bool>,
    resolves_natural: Cell<bool>,
    placeholders: RefCell<Option<Retained<AnyObject>>>,
}

impl Default for FieldIvars {
    fn default() -> Self {
        FieldIvars {
            delegate: RefCell::new(Weak::default()),
            preferred_width: Cell::new(0.0),
            max_lines: Cell::new(0),
            tightening: Cell::new(false),
            line_break_strategy: Cell::new(0),
            completion: Cell::new(false),
            writing_tools: Cell::new((true, true)),
            character_picker: Cell::new(true),
            resolves_natural: Cell::new(false),
            placeholders: RefCell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSTextField"]
    #[ivars = FieldIvars]
    pub(crate) struct NSTextFieldImpl;

    impl NSTextFieldImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(FieldIvars::default());
            // SAFETY: NSControl's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            // An editable, bezeled field with a background.
            if let Some(c) = this.field_cell_retained()
                && let Some(fc) = field_cell(&c)
            {
                fc.base().set_flag(Flags::EDITABLE | Flags::SELECTABLE | Flags::BEZELED, true);
                fc.ivars().draws_background.set(true);
            }
            this
        }

        #[unsafe(method_id(labelWithString:))]
        fn label_with_string(string: &NSString) -> Retained<objc2_app_kit::NSTextField> {
            let field = make_field(main_thread());
            set_label(&field, NSLineBreakMode::ByClipping, false);
            field.setStringValue(string);
            field.sizeToFit();
            field
        }

        #[unsafe(method_id(wrappingLabelWithString:))]
        fn wrapping_label_with_string(string: &NSString) -> Retained<objc2_app_kit::NSTextField> {
            let field = make_field(main_thread());
            set_label(&field, NSLineBreakMode::ByWordWrapping, true);
            field.setSelectable(true);
            field.setStringValue(string);
            field.sizeToFit();
            field
        }

        #[unsafe(method_id(labelWithAttributedString:))]
        fn label_with_attributed_string(string: &NSAttributedString) -> Retained<objc2_app_kit::NSTextField> {
            let field = make_field(main_thread());
            set_label(&field, NSLineBreakMode::ByClipping, false);
            field.setAttributedStringValue(string);
            field.sizeToFit();
            field
        }

        #[unsafe(method_id(textFieldWithString:))]
        fn text_field_with_string(string: &NSString) -> Retained<objc2_app_kit::NSTextField> {
            let field = make_field(main_thread());
            field.setLineBreakMode(NSLineBreakMode::ByClipping);
            if let Some(c) = field.cell() {
                c.setScrollable(true);
                // Leaving the field, however, sends its action.
                c.setSendsActionOnEndEditing(true);
            }
            field.setStringValue(string);
            field.sizeToFit();
            field
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        // The cell's text settings.

        #[unsafe(method_id(placeholderString))]
        fn placeholder_string(&self) -> Option<Retained<NSString>> {
            self.field_cell_retained().and_then(|c| c.placeholderString())
        }

        #[unsafe(method(setPlaceholderString:))]
        fn set_placeholder_string(&self, string: Option<&NSString>) {
            if let Some(c) = self.field_cell_retained() {
                c.setPlaceholderString(string);
            }
        }

        #[unsafe(method_id(placeholderAttributedString))]
        fn placeholder_attributed_string(&self) -> Option<Retained<NSAttributedString>> {
            self.field_cell_retained().and_then(|c| c.placeholderAttributedString())
        }

        #[unsafe(method(setPlaceholderAttributedString:))]
        fn set_placeholder_attributed_string(&self, string: Option<&NSAttributedString>) {
            if let Some(c) = self.field_cell_retained() {
                c.setPlaceholderAttributedString(string);
            }
        }

        #[unsafe(method_id(placeholderStrings))]
        fn placeholder_strings(&self) -> Retained<AnyObject> {
            let set = self.ivars().placeholders.borrow().clone();
            set.unwrap_or_else(|| crate::app::array_of::<AnyObject>(&[]))
        }

        #[unsafe(method(setPlaceholderStrings:))]
        fn set_placeholder_strings(&self, strings: &AnyObject) {
            self.ivars().placeholders.replace(Some(strings.retain()));
        }

        #[unsafe(method_id(placeholderAttributedStrings))]
        fn placeholder_attributed_strings(&self) -> Retained<AnyObject> {
            crate::app::array_of::<AnyObject>(&[])
        }

        #[unsafe(method(setPlaceholderAttributedStrings:))]
        fn set_placeholder_attributed_strings(&self, _strings: &AnyObject) {}

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Option<Retained<NSColor>> {
            self.field_cell_retained().and_then(|c| c.backgroundColor())
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: Option<&NSColor>) {
            if let Some(c) = self.field_cell_retained() {
                c.setBackgroundColor(color);
            }
        }

        #[unsafe(method(drawsBackground))]
        fn draws_background(&self) -> bool {
            self.field_cell_retained().is_some_and(|c| c.drawsBackground())
        }

        #[unsafe(method(setDrawsBackground:))]
        fn set_draws_background(&self, flag: bool) {
            if let Some(c) = self.field_cell_retained() {
                c.setDrawsBackground(flag);
            }
        }

        #[unsafe(method_id(textColor))]
        fn text_color(&self) -> Option<Retained<NSColor>> {
            match self.field_cell_retained() {
                Some(c) if self.is_label() && text_color_unset(&c) => {
                    Some(theme::system_color(sel!(labelColor), |p| p.label))
                }
                Some(c) => c.textColor(),
                None => None,
            }
        }

        #[unsafe(method(setTextColor:))]
        fn set_text_color(&self, color: Option<&NSColor>) {
            if let Some(c) = self.field_cell_retained() {
                c.setTextColor(color);
            }
        }

        #[unsafe(method(isBordered))]
        fn is_bordered(&self) -> bool {
            self.cell_flag(Flags::BORDERED)
        }

        #[unsafe(method(setBordered:))]
        fn set_bordered(&self, flag: bool) {
            self.with_cell(|c| c.setBordered(flag));
        }

        #[unsafe(method(isBezeled))]
        fn is_bezeled(&self) -> bool {
            self.cell_flag(Flags::BEZELED)
        }

        #[unsafe(method(setBezeled:))]
        fn set_bezeled(&self, flag: bool) {
            self.with_cell(|c| c.setBezeled(flag));
        }

        #[unsafe(method(isEditable))]
        fn is_editable(&self) -> bool {
            self.cell_flag(Flags::EDITABLE)
        }

        #[unsafe(method(setEditable:))]
        fn set_editable(&self, flag: bool) {
            self.with_cell(|c| c.setEditable(flag));
        }

        #[unsafe(method(isSelectable))]
        fn is_selectable(&self) -> bool {
            self.cell_flag(Flags::SELECTABLE)
        }

        #[unsafe(method(setSelectable:))]
        fn set_selectable(&self, flag: bool) {
            self.with_cell(|c| c.setSelectable(flag));
        }

        #[unsafe(method(bezelStyle))]
        fn bezel_style(&self) -> NSTextFieldBezelStyle {
            self.field_cell_retained().map_or(NSTextFieldBezelStyle::SquareBezel, |c| c.bezelStyle())
        }

        #[unsafe(method(setBezelStyle:))]
        fn set_bezel_style(&self, style: NSTextFieldBezelStyle) {
            if let Some(c) = self.field_cell_retained() {
                c.setBezelStyle(style);
            }
        }

        #[unsafe(method(allowsEditingTextAttributes))]
        fn allows_editing_text_attributes(&self) -> bool {
            self.cell_flag(Flags::EDITS_ATTRIBUTES)
        }

        #[unsafe(method(setAllowsEditingTextAttributes:))]
        fn set_allows_editing_text_attributes(&self, flag: bool) {
            self.with_cell(|c| c.setAllowsEditingTextAttributes(flag));
        }

        #[unsafe(method(importsGraphics))]
        fn imports_graphics(&self) -> bool {
            self.cell_flag(Flags::IMPORTS_GRAPHICS)
        }

        #[unsafe(method(setImportsGraphics:))]
        fn set_imports_graphics(&self, flag: bool) {
            self.with_cell(|c| c.setImportsGraphics(flag));
        }

        // Layout settings.

        #[unsafe(method(preferredMaxLayoutWidth))]
        fn preferred_max_layout_width(&self) -> f64 {
            self.ivars().preferred_width.get()
        }

        #[unsafe(method(setPreferredMaxLayoutWidth:))]
        fn set_preferred_max_layout_width(&self, width: f64) {
            if self.ivars().preferred_width.replace(width) != width {
                self.forget();
            }
        }

        #[unsafe(method(maximumNumberOfLines))]
        fn maximum_number_of_lines(&self) -> isize {
            self.ivars().max_lines.get()
        }

        #[unsafe(method(setMaximumNumberOfLines:))]
        fn set_maximum_number_of_lines(&self, lines: isize) {
            if self.ivars().max_lines.replace(lines) != lines {
                self.forget();
            }
        }

        #[unsafe(method(allowsDefaultTighteningForTruncation))]
        fn allows_default_tightening_for_truncation(&self) -> bool {
            self.ivars().tightening.get()
        }

        #[unsafe(method(setAllowsDefaultTighteningForTruncation:))]
        fn set_allows_default_tightening_for_truncation(&self, flag: bool) {
            self.ivars().tightening.set(flag);
        }

        #[unsafe(method(lineBreakStrategy))]
        fn line_break_strategy(&self) -> usize {
            self.ivars().line_break_strategy.get()
        }

        #[unsafe(method(setLineBreakStrategy:))]
        fn set_line_break_strategy(&self, strategy: usize) {
            self.ivars().line_break_strategy.set(strategy);
        }

        #[unsafe(method(isAutomaticTextCompletionEnabled))]
        fn is_automatic_text_completion_enabled(&self) -> bool {
            self.ivars().completion.get()
        }

        #[unsafe(method(setAutomaticTextCompletionEnabled:))]
        fn set_automatic_text_completion_enabled(&self, flag: bool) {
            self.ivars().completion.set(flag);
        }

        #[unsafe(method(allowsCharacterPickerTouchBarItem))]
        fn allows_character_picker_touch_bar_item(&self) -> bool {
            self.ivars().character_picker.get()
        }

        #[unsafe(method(setAllowsCharacterPickerTouchBarItem:))]
        fn set_allows_character_picker_touch_bar_item(&self, flag: bool) {
            self.ivars().character_picker.set(flag);
        }

        #[unsafe(method(allowsWritingTools))]
        fn allows_writing_tools(&self) -> bool {
            self.ivars().writing_tools.get().0
        }

        #[unsafe(method(setAllowsWritingTools:))]
        fn set_allows_writing_tools(&self, flag: bool) {
            let (_, affordance) = self.ivars().writing_tools.get();
            self.ivars().writing_tools.set((flag, affordance));
        }

        #[unsafe(method(allowsWritingToolsAffordance))]
        fn allows_writing_tools_affordance(&self) -> bool {
            self.ivars().writing_tools.get().1
        }

        #[unsafe(method(setAllowsWritingToolsAffordance:))]
        fn set_allows_writing_tools_affordance(&self, flag: bool) {
            let (tools, _) = self.ivars().writing_tools.get();
            self.ivars().writing_tools.set((tools, flag));
        }

        #[unsafe(method(resolvesNaturalAlignmentWithBaseWritingDirection))]
        fn resolves_natural_alignment(&self) -> bool {
            self.ivars().resolves_natural.get()
        }

        #[unsafe(method(setResolvesNaturalAlignmentWithBaseWritingDirection:))]
        fn set_resolves_natural_alignment(&self, flag: bool) {
            self.ivars().resolves_natural.set(flag);
        }

        // Sizing.

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            control::cached_intrinsic(self.as_control(), || field_intrinsic_size(self))
        }

        #[unsafe(method(sizeThatFits:))]
        fn size_that_fits(&self, size: NSSize) -> NSSize {
            field_size_that_fits(self, size)
        }

        /// The cell's whole size, rounded up (`labels`), whatever the
        /// field's frame now.
        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {
            let control: &NSControl = self.as_control();
            if let Some(c) = control.cell() {
                let s = c.cellSize();
                let view: &NSView = control;
                view.setFrameSize(NSSize::new(s.width.ceil(), s.height.ceil()));
            }
        }

        #[unsafe(method(fittingSize))]
        fn fitting_size(&self) -> NSSize {
            let control: &NSControl = self.as_control();
            let size = control.cell().map_or(NSSize::ZERO, |c| c.cellSize());
            NSSize::new(size.width.ceil(), size.height.ceil())
        }

        // The keyboard and the mouse.

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            let control: &NSControl = self.as_control();
            control.isEnabled() && !control.refusesFirstResponder() && self.cell_flag(Flags::SELECTABLE)
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            // SAFETY: NSResponder's becomeFirstResponder.
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                select_text(self.as_control(), None);
            }
            ok
        }

        #[unsafe(method(selectText:))]
        fn select_text(&self, sender: Option<&AnyObject>) {
            select_text(self.as_control(), sender);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if self.cell_flag(Flags::SELECTABLE) {
                edit_on_click(self.as_control(), event);
            } else {
                // SAFETY: NSResponder's mouseDown: passes the event on.
                let _: () = unsafe { msg_send![super(self), mouseDown: event] };
            }
        }

        // The delegate, and what the field editor tells the field.

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            self.ivars().delegate.borrow().load()
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            self.ivars().delegate.replace(delegate.map_or_else(Weak::default, Weak::new));
        }

        #[unsafe(method(textShouldBeginEditing:))]
        fn text_should_begin_editing(&self, text: &AnyObject) -> bool {
            ask_delegate(self, sel!(control:textShouldBeginEditing:), text)
        }

        #[unsafe(method(textShouldEndEditing:))]
        fn text_should_end_editing(&self, text: &AnyObject) -> bool {
            ask_delegate(self, sel!(control:textShouldEndEditing:), text)
        }

        #[unsafe(method(textDidBeginEditing:))]
        fn text_did_begin_editing(&self, notification: &NSNotification) {
            relay(self, notification, "NSControlTextDidBeginEditingNotification", sel!(controlTextDidBeginEditing:), false);
        }

        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, notification: &NSNotification) {
            relay(self, notification, "NSControlTextDidChangeNotification", sel!(controlTextDidChange:), false);
        }

        #[unsafe(method(textDidEndEditing:))]
        fn text_did_end_editing(&self, notification: &NSNotification) {
            did_end_editing(self, notification);
        }
    }
);

impl NSTextFieldImpl {
    fn as_control(&self) -> &NSControl {
        // SAFETY: NSTextField is a subclass of NSControl.
        unsafe { &*(self as *const Self).cast::<NSControl>() }
    }

    fn field_cell_retained(&self) -> Option<Retained<NSTextFieldCell>> {
        let cell = self.as_control().cell()?;
        field_cell(&cell)?;
        // SAFETY: checked just above.
        Some(unsafe { Retained::cast_unchecked(cell) })
    }

    fn with_cell(&self, f: impl FnOnce(&NSCell)) {
        if let Some(c) = self.as_control().cell() {
            f(&c);
        }
    }

    fn cell_flag(&self, bit: u32) -> bool {
        self.as_control().cell().is_some_and(|c| cell_imp(&c).has(bit))
    }

    /// A label: neither editable nor selectable, bezeled nor bordered.
    fn is_label(&self) -> bool {
        !self.cell_flag(Flags::EDITABLE) && !self.cell_flag(Flags::BEZELED) && !self.cell_flag(Flags::BORDERED)
    }

    fn forget(&self) {
        let view: &NSView = self.as_control();
        view.invalidateIntrinsicContentSize();
        view.setNeedsDisplay(true);
    }
}

/// No text color was set: a label shows the label color.
fn text_color_unset(cell: &NSTextFieldCell) -> bool {
    field_cell(cell).is_none_or(|c| c.ivars().text_color.borrow().is_none())
}

/// Class methods run on the main thread, as AppKit's classes do.
fn main_thread() -> MainThreadMarker {
    MainThreadMarker::new().expect("sidestep: AppKit's controls belong to the main thread")
}

fn make_field(mtm: MainThreadMarker) -> Retained<objc2_app_kit::NSTextField> {
    objc2_app_kit::NSTextField::initWithFrame(objc2_app_kit::NSTextField::alloc(mtm), NSRect::ZERO)
}

/// Make `field` a label: not editable, bezeled or bordered, no background,
/// natural alignment.
fn set_label(field: &objc2_app_kit::NSTextField, line_break: NSLineBreakMode, wraps: bool) {
    field.setEditable(false);
    field.setSelectable(false);
    field.setBezeled(false);
    field.setBordered(false);
    field.setDrawsBackground(false);
    field.setAlignment(NSTextAlignment::Natural);
    field.setLineBreakMode(line_break);
    if let Some(c) = field.cell() {
        c.setWraps(wraps);
    }
}

/// `intrinsicContentSize`: editable fields have no intrinsic width; others
/// are their text's size (rounded up), without the 2-point padding; a
/// wrapping field with a preferred width lays out that wide, at most so
/// many lines.
fn field_intrinsic_size(field: &NSTextFieldImpl) -> NSSize {
    let control: &NSControl = field.as_control();
    let Some(c) = control.cell() else { return NSSize::new(control::NO_METRIC, control::NO_METRIC) };
    let base = cell_imp(&c);
    let Some(fc) = field_cell(&c) else { return c.cellSize() };
    let editable = base.has(Flags::EDITABLE);
    let preferred = field.ivars().preferred_width.get();
    let wraps = base.has(Flags::WRAPS) && !base.has(Flags::SINGLE_LINE);
    let full = field_cell_size(fc, None);
    let padding = 2.0 * metrics::TEXT_PADDING;
    let lines = field.ivars().max_lines.get();
    let size = if wraps && preferred > 0.0 {
        let laid = field_styled(fc, [0.0; 4]).size(Some(preferred - padding));
        let lh = line_height(fc);
        let mut h = laid.height;
        if lines > 0 && lh > 0.0 {
            h = h.min(lines as f64 * lh);
        }
        NSSize::new(((laid.width) * 2.0).ceil() / 2.0, h + (full.height - field_styled(fc, [0.0; 4]).size(None).height))
    } else {
        let text_w = (full.width - frame_padding(base) - padding).max(0.0);
        let chrome = frame_padding(base);
        NSSize::new(text_w.ceil() + chrome, full.height)
    };
    if editable { NSSize::new(control::NO_METRIC, size.height) } else { size }
}

/// What a field's bezel or border adds across.
fn frame_padding(base: &NSCellImpl) -> f64 {
    if base.has(Flags::BEZELED) {
        2.0 * metrics::FIELD_BEZEL_INSET
    } else if base.has(Flags::BORDERED) {
        2.0 * metrics::BORDER_INSET
    } else {
        0.0
    }
}

/// `-[NSTextField sizeThatFits:]`: the cell's size for the width offered
/// (unbounded if none), no wider than the text, at most so many lines.
fn field_size_that_fits(field: &NSTextFieldImpl, proposed: NSSize) -> NSSize {
    let control: &NSControl = field.as_control();
    let Some(c) = control.cell() else { return proposed };
    let natural = c.cellSize();
    let width = if proposed.width > 0.0 { proposed.width } else { natural.width };
    let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(width, f64::MAX));
    let size = c.cellSizeForBounds(bounds);
    let mut height = size.height;
    let lines = field.ivars().max_lines.get();
    if lines > 0
        && let Some(fc) = field_cell(&c)
    {
        let chrome = field_cell_size(fc, None).height - field_styled(fc, [0.0; 4]).size(None).height;
        height = height.min(lines as f64 * line_height(fc) + chrome);
    }
    NSSize::new(size.width.min(natural.width), height)
}

/// The delegate's answer to `sel` (`control:textShould…Editing:`), yes if
/// it has none.
fn ask_delegate(field: &NSTextFieldImpl, sel: Sel, text: &AnyObject) -> bool {
    // Loaded in a statement of its own: no borrow is held while the
    // delegate is asked, which may set another.
    let delegate = field.ivars().delegate.borrow().load();
    let Some(delegate) = delegate else { return true };
    if !responds(&delegate, sel) {
        return true;
    }
    let control: &NSControl = field.as_control();
    // SAFETY: the delegate methods take the control and the text object and
    // return BOOL.
    let answer: objc2::runtime::Bool =
        unsafe { objc2::runtime::MessageReceiver::send_message(&*delegate, sel, (control, text)) };
    answer.as_bool()
}

fn responds(object: &AnyObject, sel: Sel) -> bool {
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    unsafe { msg_send![object, respondsToSelector: sel] }
}

/// Tell the delegate and then everyone else that `name` happened, with
/// one notification from the field whose user info holds the field
/// editor (the text notification's object) as `NSFieldEditor` and, when
/// `movement` is asked for, the text notification's `NSTextMovement`.
/// Nothing is made when nobody listens.
fn relay(field: &NSTextFieldImpl, notification: &NSNotification, name: &str, delegate_sel: Sel, movement: bool) {
    let control: &NSControl = field.as_control();
    let name = NSString::from_str(name);
    // Loaded in a statement of its own: no borrow is held while the
    // delegate runs, which may set another delegate.
    let delegate = field.ivars().delegate.borrow().load();
    let delegate = delegate.filter(|d| responds(d, delegate_sel));
    if delegate.is_none() && !sidestep_foundation::notification_center::has_observers(&name) {
        return;
    }
    let mut keys: Vec<Retained<NSString>> = Vec::with_capacity(2);
    let mut values: Vec<Retained<AnyObject>> = Vec::with_capacity(2);
    if let Some(editor) = notification.object() {
        keys.push(NSString::from_str("NSFieldEditor"));
        values.push(editor);
    }
    if movement {
        let key = NSString::from_str("NSTextMovement");
        if let Some(m) = notification.userInfo().and_then(|info| info.objectForKey(&key)) {
            keys.push(key);
            values.push(m);
        }
    }
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    let info = NSDictionary::from_slices(&keys, &values);
    // SAFETY: the class method takes a name, an object and a dictionary.
    let note: Retained<NSNotification> = unsafe {
        msg_send![<NSNotification as objc2::ClassType>::class(), notificationWithName: &*name, object: Some(control as &AnyObject), userInfo: Some(&*info)]
    };
    if let Some(delegate) = delegate {
        // SAFETY: controlText…: take the notification.
        unsafe { objc2::runtime::MessageReceiver::send_message::<_, ()>(&*delegate, delegate_sel, (&*note,)) };
    }
    sidestep_foundation::notification_center::default_center().postNotification(&note);
}

/// `textDidEndEditing:`: take the text, tell everyone, and send the action
/// when Return ended the editing or the cell asks to on any ending.
fn did_end_editing(field: &NSTextFieldImpl, notification: &NSNotification) {
    // The delegate may take the field out of its window, the last hold on
    // it, while it's told; the action still goes out from a live field.
    let _keep = field.retain();
    let control: &NSControl = field.as_control();
    validate_editing(control);
    relay(field, notification, "NSControlTextDidEndEditingNotification", sel!(controlTextDidEndEditing:), true);
    let movement = notification.userInfo().and_then(|info| {
        let key = NSString::from_str("NSTextMovement");
        info.objectForKey(&key).map(|n| value::Value::from_object(Some(&n)).integer())
    });
    let sends_always = control.cell().is_some_and(|c| cell_imp(&c).has(Flags::SENDS_ON_END_EDITING));
    if movement == Some(0x10) || sends_always {
        // SAFETY: the control's own action and target.
        let _ = unsafe { control.sendAction_to(control.action(), control.target().as_deref()) };
    }
}

// Text-editing hooks: the field editor (`textkit::field_editor`) does the
// editing.

/// `-[NSTextField selectText:]`, and a field becoming first responder:
/// start editing with all the text selected.
pub(crate) fn select_text(control: &NSControl, sender: Option<&AnyObject>) {
    crate::textkit::field_editor::select_text(control, sender);
}

/// A click in a selectable field: start editing at the click.
pub(crate) fn edit_on_click(control: &NSControl, event: &NSEvent) {
    crate::textkit::field_editor::edit_on_click(control, event);
}

/// `-[NSCell editWithFrame:inView:editor:delegate:event:]`: start editing
/// in `editor`, the field editor, over `frame`.
pub(crate) fn edit_with_frame(
    cell: &NSCell,
    frame: NSRect,
    view: &NSView,
    editor: &AnyObject,
    delegate: Option<&AnyObject>,
    event: Option<&NSEvent>,
) {
    crate::textkit::field_editor::edit_with_frame(cell, frame, view, editor, delegate, event);
}

/// `-[NSCell selectWithFrame:inView:editor:delegate:start:length:]`: as
/// `edit_with_frame`, selecting a range.
pub(crate) fn select_with_frame(
    cell: &NSCell,
    frame: NSRect,
    view: &NSView,
    editor: &AnyObject,
    delegate: Option<&AnyObject>,
    start: isize,
    length: isize,
) {
    crate::textkit::field_editor::select_with_frame(cell, frame, view, editor, delegate, start, length);
}

/// `-[NSCell endEditing:]`: take the editor's text and let it go.
pub(crate) fn end_editing(cell: &NSCell, editor: &AnyObject) {
    crate::textkit::field_editor::end_editing(cell, editor);
}

/// `-[NSCell fieldEditorForView:]`: a custom field editor, or nil for the
/// window's (which `fieldEditor:forObject:` gives, the delegate asked
/// first).
pub(crate) fn field_editor(_cell: &NSCell, _view: &NSView) -> Option<Retained<AnyObject>> {
    None
}

/// `-[NSControl currentEditor]`: the field editor while the control is
/// being edited.
pub(crate) fn current_editor(control: &NSControl) -> Option<Retained<AnyObject>> {
    crate::textkit::field_editor::current_editor(control)
}

/// `-[NSControl abortEditing]`: stop editing without taking the text; true
/// if there was editing to stop.
pub(crate) fn abort_editing(control: &NSControl) -> bool {
    crate::textkit::field_editor::abort_editing(control)
}

/// `-[NSControl validateEditing]`: take the editor's text into the cell.
pub(crate) fn validate_editing(control: &NSControl) {
    crate::textkit::field_editor::validate_editing(control);
}

#[cfg(test)]
mod tests {
    #[test]
    fn secure_fields_draw_a_bullet_per_character() {
        assert_eq!(super::bullets("abc"), "•••");
        // A combining accent, a variation selector and a joined emoji count
        // with what they join.
        assert_eq!(super::bullets("e\u{301}x"), "••");
        assert_eq!(super::bullets("\u{2764}\u{FE0F}"), "•");
        assert_eq!(super::bullets("👩\u{200D}💻"), "•");
        assert_eq!(super::bullets(""), "");
    }
}
