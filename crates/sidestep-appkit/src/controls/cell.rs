//! `NSCell` and `NSActionCell`: the value, state and look of a control,
//! separate from its view.
//!
//! A cell keeps its settings in plain Rust (flags in one word, the value
//! as a [`Value`]) and reaches everything a subclass may override by
//! message: drawing (`drawWithFrame:inView:`, `drawInteriorWithFrame:…`),
//! geometry (`cellSize`, the `…RectForBounds:` family), tracking, and
//! `setNextState`. A setter that changes something tells the control
//! showing the cell (`-[NSControl updateCell:]`), which redraws it and
//! forgets its size; setting what's already there does nothing.
//!
//! Plain cells have no target, action or tag (a tag reads -1); action cells
//! keep them, the target weakly.
//!
//! Text is measured and drawn through the text engine with the cell's font
//! and paragraph settings: `wraps` off turns word and character wrapping
//! into clipping, as AppKit does for single-line cells. Measurements are
//! the text's size plus the insets `theme::metrics` lists; the layouts
//! themselves are cached by the engine, keyed by text and attributes.

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackgroundStyle, NSCell, NSCellAttribute, NSCellHitResult, NSCellType, NSColor, NSControlSize, NSControlTint,
    NSEvent, NSEventMask, NSFocusRingType, NSFont, NSLineBreakMode, NSTextAlignment, NSUserInterfaceLayoutDirection,
    NSView, NSWritingDirection,
};
use objc2_foundation::{NSAttributedString, NSCopying, NSPoint, NSRect, NSSize, NSString};

use super::value::{self, Value};
use crate::protocol::Color;
use crate::text::layout::{Align, Attrs, LineBreak, Options, Paragraph, Run};
use crate::theme::{self, metrics, parts};

/// Boolean settings, one bit each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Flags(u32);

impl Flags {
    pub const ENABLED: u32 = 1 << 0;
    pub const BORDERED: u32 = 1 << 1;
    pub const BEZELED: u32 = 1 << 2;
    pub const EDITABLE: u32 = 1 << 3;
    pub const SELECTABLE: u32 = 1 << 4;
    pub const SCROLLABLE: u32 = 1 << 5;
    pub const HIGHLIGHTED: u32 = 1 << 6;
    pub const CONTINUOUS: u32 = 1 << 7;
    pub const WRAPS: u32 = 1 << 8;
    pub const TRUNCATES_LAST: u32 = 1 << 9;
    pub const SINGLE_LINE: u32 = 1 << 10;
    pub const ALLOWS_MIXED: u32 = 1 << 11;
    pub const REFUSES_FIRST_RESPONDER: u32 = 1 << 12;
    pub const SHOWS_FIRST_RESPONDER: u32 = 1 << 13;
    pub const SENDS_ON_END_EDITING: u32 = 1 << 14;
    pub const ALLOWS_UNDO: u32 = 1 << 15;
    pub const EDITS_ATTRIBUTES: u32 = 1 << 16;
    pub const IMPORTS_GRAPHICS: u32 = 1 << 17;
    /// A program set the font (else the size follows the control size).
    pub const FONT_SET: u32 = 1 << 18;

    pub fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

/// Settings few cells change, kept out of line.
#[derive(Default)]
struct Extras {
    formatter: Option<Retained<AnyObject>>,
    menu: Option<Retained<AnyObject>>,
    writing_direction: Option<NSWritingDirection>,
    layout_direction: Option<NSUserInterfaceLayoutDirection>,
    control_tint: Option<NSControlTint>,
    entry_type: isize,
    mnemonic: Option<usize>,
}

pub(crate) struct CellIvars {
    kind: Cell<NSCellType>,
    state: Cell<isize>,
    value: RefCell<Value>,
    flags: Cell<Flags>,
    alignment: Cell<NSTextAlignment>,
    line_break: Cell<NSLineBreakMode>,
    font: RefCell<Option<Retained<NSFont>>>,
    image: RefCell<Option<Retained<AnyObject>>>,
    control_size: Cell<NSControlSize>,
    focus_ring: Cell<NSFocusRingType>,
    background_style: Cell<NSBackgroundStyle>,
    /// `sendActionOn:`'s mask.
    action_mask: Cell<u64>,
    control_view: RefCell<Weak<NSView>>,
    represented: RefCell<Option<Retained<AnyObject>>>,
    extras: RefCell<Option<Box<Extras>>>,
}

impl CellIvars {
    fn new(kind: NSCellType) -> Self {
        CellIvars {
            kind: Cell::new(kind),
            state: Cell::new(0),
            value: RefCell::new(Value::Empty),
            flags: Cell::new(Flags(Flags::ENABLED | Flags::WRAPS)),
            alignment: Cell::new(NSTextAlignment::Left),
            line_break: Cell::new(NSLineBreakMode::ByWordWrapping),
            font: RefCell::new(None),
            image: RefCell::new(None),
            control_size: Cell::new(NSControlSize::Regular),
            focus_ring: Cell::new(NSFocusRingType::Default),
            background_style: Cell::new(NSBackgroundStyle::Normal),
            action_mask: Cell::new(NSEventMask::LeftMouseUp.0),
            control_view: RefCell::new(Weak::default()),
            represented: RefCell::new(None),
            extras: RefCell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSCell"]
    #[ivars = CellIvars]
    pub(crate) struct NSCellImpl;

    impl NSCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(CellIvars::new(NSCellType::NullCellType));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(CellIvars::new(NSCellType::TextCellType));
            // SAFETY: NSObject's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            this.ivars().value.replace(Value::String(string.copy()));
            this
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, image: Option<&AnyObject>) -> Retained<Self> {
            let kind = if image.is_some() { NSCellType::ImageCellType } else { NSCellType::NullCellType };
            let this = this.set_ivars(CellIvars::new(kind));
            // SAFETY: NSObject's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            this.ivars().image.replace(image.map(|i| i.retain()));
            this
        }

        #[unsafe(method(prefersTrackingUntilMouseUp))]
        fn prefers_tracking_until_mouse_up() -> bool {
            false
        }

        #[unsafe(method(defaultFocusRingType))]
        fn default_focus_ring_type() -> NSFocusRingType {
            NSFocusRingType::Exterior
        }

        #[unsafe(method_id(defaultMenu))]
        fn default_menu() -> Option<Retained<AnyObject>> {
            None
        }

        // The view and the kind of content.

        #[unsafe(method_id(controlView))]
        fn control_view(&self) -> Option<Retained<NSView>> {
            self.ivars().control_view.borrow().load()
        }

        #[unsafe(method(setControlView:))]
        fn set_control_view(&self, view: Option<&NSView>) {
            self.ivars().control_view.replace(view.map_or_else(Weak::default, Weak::new));
        }

        #[unsafe(method(type))]
        fn kind(&self) -> NSCellType {
            self.ivars().kind.get()
        }

        #[unsafe(method(setType:))]
        fn set_kind(&self, kind: NSCellType) {
            if self.ivars().kind.replace(kind) != kind {
                if kind == NSCellType::TextCellType && self.ivars().value.borrow().is_empty() {
                    self.ivars().value.replace(Value::String(NSString::new()));
                }
                changed(self);
            }
        }

        // State.

        #[unsafe(method(state))]
        fn state(&self) -> isize {
            self.ivars().state.get()
        }

        #[unsafe(method(setState:))]
        fn set_state(&self, state: isize) {
            set_state(self, state);
        }

        #[unsafe(method(allowsMixedState))]
        fn allows_mixed_state(&self) -> bool {
            self.has(Flags::ALLOWS_MIXED)
        }

        #[unsafe(method(setAllowsMixedState:))]
        fn set_allows_mixed_state(&self, flag: bool) {
            self.set_flag(Flags::ALLOWS_MIXED, flag);
        }

        #[unsafe(method(nextState))]
        fn next_state(&self) -> isize {
            next_state(self.ivars().state.get(), self.has(Flags::ALLOWS_MIXED))
        }

        #[unsafe(method(setNextState))]
        fn set_next_state(&self) {
            // SAFETY: nextState and setState: are NSCell's, and subclasses
            // may override either.
            unsafe {
                let next: isize = msg_send![self, nextState];
                let _: () = msg_send![self, setState: next];
            }
        }

        // Target, action and tag: a plain cell has none.

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, _target: Option<&AnyObject>) {}

        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> {
            None
        }

        #[unsafe(method(setAction:))]
        fn set_action(&self, _action: Option<Sel>) {}

        #[unsafe(method(tag))]
        fn tag(&self) -> isize {
            -1
        }

        #[unsafe(method(setTag:))]
        fn set_tag(&self, _tag: isize) {}

        // The value.

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            // SAFETY: stringValue takes nothing and returns a string.
            unsafe { msg_send![self, stringValue] }
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            // SAFETY: setStringValue: takes a string.
            unsafe { msg_send![self, setStringValue: title] }
        }

        #[unsafe(method_id(objectValue))]
        fn object_value(&self) -> Option<Retained<AnyObject>> {
            self.ivars().value.borrow().object()
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, object: Option<&AnyObject>) {
            set_value(self, Value::from_object(object));
        }

        #[unsafe(method(hasValidObjectValue))]
        fn has_valid_object_value(&self) -> bool {
            true
        }

        #[unsafe(method_id(stringValue))]
        fn string_value(&self) -> Retained<NSString> {
            self.ivars().value.borrow().string()
        }

        #[unsafe(method(setStringValue:))]
        fn set_string_value(&self, string: &NSString) {
            set_value(self, Value::String(string.copy()));
        }

        #[unsafe(method_id(attributedStringValue))]
        fn attributed_string_value(&self) -> Retained<NSAttributedString> {
            let value = self.ivars().value.borrow().clone();
            match value {
                Value::Attributed(a) => a,
                other => value::attributed(&other.string()),
            }
        }

        #[unsafe(method(setAttributedStringValue:))]
        fn set_attributed_string_value(&self, string: &NSAttributedString) {
            set_value(self, Value::from_object(Some(string)));
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            self.ivars().value.borrow().int()
        }

        #[unsafe(method(setIntValue:))]
        fn set_int_value(&self, value: i32) {
            set_value(self, Value::Int(i64::from(value)));
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> isize {
            self.ivars().value.borrow().integer()
        }

        #[unsafe(method(setIntegerValue:))]
        fn set_integer_value(&self, value: isize) {
            set_value(self, Value::Int(value as i64));
        }

        #[unsafe(method(floatValue))]
        fn float_value(&self) -> f32 {
            self.ivars().value.borrow().float()
        }

        #[unsafe(method(setFloatValue:))]
        fn set_float_value(&self, value: f32) {
            set_value(self, Value::Float(value));
        }

        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            self.ivars().value.borrow().double()
        }

        #[unsafe(method(setDoubleValue:))]
        fn set_double_value(&self, value: f64) {
            set_value(self, Value::Double(value));
        }

        #[unsafe(method(takeIntValueFrom:))]
        fn take_int_value_from(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: the sender answers intValue, as the method requires.
            let v: i32 = unsafe { msg_send![sender, intValue] };
            // SAFETY: setIntValue: takes an int.
            unsafe { msg_send![self, setIntValue: v] }
        }

        #[unsafe(method(takeIntegerValueFrom:))]
        fn take_integer_value_from(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: as above, for integerValue.
            let v: isize = unsafe { msg_send![sender, integerValue] };
            // SAFETY: setIntegerValue: takes an NSInteger.
            unsafe { msg_send![self, setIntegerValue: v] }
        }

        #[unsafe(method(takeFloatValueFrom:))]
        fn take_float_value_from(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: as above, for floatValue.
            let v: f32 = unsafe { msg_send![sender, floatValue] };
            // SAFETY: setFloatValue: takes a float.
            unsafe { msg_send![self, setFloatValue: v] }
        }

        #[unsafe(method(takeDoubleValueFrom:))]
        fn take_double_value_from(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: as above, for doubleValue.
            let v: f64 = unsafe { msg_send![sender, doubleValue] };
            // SAFETY: setDoubleValue: takes a double.
            unsafe { msg_send![self, setDoubleValue: v] }
        }

        #[unsafe(method(takeStringValueFrom:))]
        fn take_string_value_from(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: as above, for stringValue.
            let v: Retained<NSString> = unsafe { msg_send![sender, stringValue] };
            // SAFETY: setStringValue: takes a string.
            unsafe { msg_send![self, setStringValue: &*v] }
        }

        #[unsafe(method(takeObjectValueFrom:))]
        fn take_object_value_from(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            // SAFETY: as above, for objectValue.
            let v: Option<Retained<AnyObject>> = unsafe { msg_send![sender, objectValue] };
            // SAFETY: setObjectValue: takes an object or nil.
            unsafe { msg_send![self, setObjectValue: v.as_deref()] }
        }

        #[unsafe(method_id(representedObject))]
        fn represented_object(&self) -> Option<Retained<AnyObject>> {
            self.ivars().represented.borrow().clone()
        }

        #[unsafe(method(setRepresentedObject:))]
        fn set_represented_object(&self, object: Option<&AnyObject>) {
            let old = self.ivars().represented.replace(object.map(|o| o.retain()));
            drop(old);
        }

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<AnyObject>> {
            self.ivars().image.borrow().clone()
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, image: Option<&AnyObject>) {
            let old = self.ivars().image.replace(image.map(|i| i.retain()));
            if image.is_some() && self.ivars().kind.get() == NSCellType::NullCellType {
                self.ivars().kind.set(NSCellType::ImageCellType);
            }
            drop(old);
            changed(self);
        }

        #[unsafe(method_id(formatter))]
        fn formatter(&self) -> Option<Retained<AnyObject>> {
            self.ivars().extras.borrow().as_ref().and_then(|e| e.formatter.clone())
        }

        #[unsafe(method(setFormatter:))]
        fn set_formatter(&self, formatter: Option<&AnyObject>) {
            let old = std::mem::replace(&mut extras(self).formatter, formatter.map(|f| f.retain()));
            drop(old);
        }

        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<AnyObject>> {
            self.ivars().extras.borrow().as_ref().and_then(|e| e.menu.clone())
        }

        #[unsafe(method(setMenu:))]
        fn set_menu(&self, menu: Option<&AnyObject>) {
            let old = std::mem::replace(&mut extras(self).menu, menu.map(|m| m.retain()));
            drop(old);
        }

        #[unsafe(method_id(menuForEvent:inRect:ofView:))]
        fn menu_for_event(&self, _event: &NSEvent, _frame: NSRect, _view: &NSView) -> Option<Retained<AnyObject>> {
            // SAFETY: menu takes nothing and returns a menu or nil.
            unsafe { msg_send![self, menu] }
        }

        #[unsafe(method(compare:))]
        fn compare(&self, other: &AnyObject) -> isize {
            // SAFETY: the other object is a cell, as the method requires.
            let theirs: Retained<NSString> = unsafe { msg_send![other, stringValue] };
            let mine = self.ivars().value.borrow().string();
            // SAFETY: compare: takes a string and returns NSComparisonResult.
            unsafe { msg_send![&*mine, compare: &*theirs] }
        }

        // Flags.

        #[unsafe(method(isEnabled))]
        fn is_enabled(&self) -> bool {
            self.has(Flags::ENABLED)
        }

        #[unsafe(method(setEnabled:))]
        fn set_enabled(&self, flag: bool) {
            self.set_flag(Flags::ENABLED, flag);
        }

        #[unsafe(method(isBordered))]
        fn is_bordered(&self) -> bool {
            self.has(Flags::BORDERED)
        }

        #[unsafe(method(setBordered:))]
        fn set_bordered(&self, flag: bool) {
            // Bordered and bezeled exclude each other.
            let mut f = self.ivars().flags.get();
            if flag {
                f.0 = (f.0 | Flags::BORDERED) & !Flags::BEZELED;
            } else {
                f.0 &= !Flags::BORDERED;
            }
            self.store_flags(f);
        }

        #[unsafe(method(isBezeled))]
        fn is_bezeled(&self) -> bool {
            self.has(Flags::BEZELED)
        }

        #[unsafe(method(setBezeled:))]
        fn set_bezeled(&self, flag: bool) {
            let mut f = self.ivars().flags.get();
            if flag {
                f.0 = (f.0 | Flags::BEZELED) & !Flags::BORDERED;
            } else {
                f.0 &= !Flags::BEZELED;
            }
            self.store_flags(f);
        }

        #[unsafe(method(isEditable))]
        fn is_editable(&self) -> bool {
            self.has(Flags::EDITABLE)
        }

        #[unsafe(method(setEditable:))]
        fn set_editable(&self, flag: bool) {
            // Editable text is selectable too.
            let mut f = self.ivars().flags.get();
            if flag {
                f.0 |= Flags::EDITABLE | Flags::SELECTABLE;
            } else {
                f.0 &= !Flags::EDITABLE;
            }
            self.store_flags(f);
        }

        #[unsafe(method(isSelectable))]
        fn is_selectable(&self) -> bool {
            self.has(Flags::SELECTABLE)
        }

        #[unsafe(method(setSelectable:))]
        fn set_selectable(&self, flag: bool) {
            let mut f = self.ivars().flags.get();
            if flag {
                f.0 |= Flags::SELECTABLE;
            } else {
                f.0 &= !(Flags::SELECTABLE | Flags::EDITABLE);
            }
            self.store_flags(f);
        }

        #[unsafe(method(isScrollable))]
        fn is_scrollable(&self) -> bool {
            self.has(Flags::SCROLLABLE)
        }

        #[unsafe(method(setScrollable:))]
        fn set_scrollable(&self, flag: bool) {
            // Scrolling text doesn't wrap.
            let mut f = self.ivars().flags.get();
            if flag {
                f.0 = (f.0 | Flags::SCROLLABLE) & !Flags::WRAPS;
            } else {
                f.0 &= !Flags::SCROLLABLE;
            }
            self.store_flags(f);
        }

        #[unsafe(method(wraps))]
        fn wraps(&self) -> bool {
            self.has(Flags::WRAPS)
        }

        #[unsafe(method(setWraps:))]
        fn set_wraps(&self, flag: bool) {
            let mut f = self.ivars().flags.get();
            if flag {
                f.0 = (f.0 | Flags::WRAPS) & !Flags::SCROLLABLE;
            } else {
                f.0 &= !Flags::WRAPS;
            }
            self.store_flags(f);
        }

        #[unsafe(method(isHighlighted))]
        fn is_highlighted(&self) -> bool {
            self.has(Flags::HIGHLIGHTED)
        }

        #[unsafe(method(setHighlighted:))]
        fn set_highlighted(&self, flag: bool) {
            self.set_flag(Flags::HIGHLIGHTED, flag);
        }

        #[unsafe(method(isContinuous))]
        fn is_continuous(&self) -> bool {
            self.has(Flags::CONTINUOUS)
        }

        #[unsafe(method(setContinuous:))]
        fn set_continuous(&self, flag: bool) {
            self.set_flag(Flags::CONTINUOUS, flag);
        }

        #[unsafe(method(truncatesLastVisibleLine))]
        fn truncates_last_visible_line(&self) -> bool {
            self.has(Flags::TRUNCATES_LAST)
        }

        #[unsafe(method(setTruncatesLastVisibleLine:))]
        fn set_truncates_last_visible_line(&self, flag: bool) {
            self.set_flag(Flags::TRUNCATES_LAST, flag);
        }

        #[unsafe(method(usesSingleLineMode))]
        fn uses_single_line_mode(&self) -> bool {
            self.has(Flags::SINGLE_LINE)
        }

        #[unsafe(method(setUsesSingleLineMode:))]
        fn set_uses_single_line_mode(&self, flag: bool) {
            self.set_flag(Flags::SINGLE_LINE, flag);
        }

        #[unsafe(method(refusesFirstResponder))]
        fn refuses_first_responder(&self) -> bool {
            self.has(Flags::REFUSES_FIRST_RESPONDER)
        }

        #[unsafe(method(setRefusesFirstResponder:))]
        fn set_refuses_first_responder(&self, flag: bool) {
            self.set_flag(Flags::REFUSES_FIRST_RESPONDER, flag);
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.has(Flags::ENABLED) && !self.has(Flags::REFUSES_FIRST_RESPONDER)
        }

        #[unsafe(method(showsFirstResponder))]
        fn shows_first_responder(&self) -> bool {
            self.has(Flags::SHOWS_FIRST_RESPONDER)
        }

        #[unsafe(method(setShowsFirstResponder:))]
        fn set_shows_first_responder(&self, flag: bool) {
            self.set_flag(Flags::SHOWS_FIRST_RESPONDER, flag);
        }

        #[unsafe(method(sendsActionOnEndEditing))]
        fn sends_action_on_end_editing(&self) -> bool {
            self.has(Flags::SENDS_ON_END_EDITING)
        }

        #[unsafe(method(setSendsActionOnEndEditing:))]
        fn set_sends_action_on_end_editing(&self, flag: bool) {
            self.set_flag(Flags::SENDS_ON_END_EDITING, flag);
        }

        #[unsafe(method(allowsUndo))]
        fn allows_undo(&self) -> bool {
            self.has(Flags::ALLOWS_UNDO)
        }

        #[unsafe(method(setAllowsUndo:))]
        fn set_allows_undo(&self, flag: bool) {
            self.set_flag(Flags::ALLOWS_UNDO, flag);
        }

        #[unsafe(method(allowsEditingTextAttributes))]
        fn allows_editing_text_attributes(&self) -> bool {
            self.has(Flags::EDITS_ATTRIBUTES)
        }

        #[unsafe(method(setAllowsEditingTextAttributes:))]
        fn set_allows_editing_text_attributes(&self, flag: bool) {
            self.set_flag(Flags::EDITS_ATTRIBUTES, flag);
        }

        #[unsafe(method(importsGraphics))]
        fn imports_graphics(&self) -> bool {
            self.has(Flags::IMPORTS_GRAPHICS)
        }

        #[unsafe(method(setImportsGraphics:))]
        fn set_imports_graphics(&self, flag: bool) {
            self.set_flag(Flags::IMPORTS_GRAPHICS, flag);
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            false
        }

        #[unsafe(method(wantsNotificationForMarkedText))]
        fn wants_notification_for_marked_text(&self) -> bool {
            false
        }

        #[unsafe(method(cellAttribute:))]
        fn cell_attribute(&self, attribute: NSCellAttribute) -> isize {
            let bit = |b| isize::from(self.has(b));
            match attribute {
                NSCellAttribute::CellDisabled => isize::from(!self.has(Flags::ENABLED)),
                NSCellAttribute::CellState => self.ivars().state.get(),
                NSCellAttribute::CellEditable => bit(Flags::EDITABLE),
                NSCellAttribute::CellHighlighted => bit(Flags::HIGHLIGHTED),
                NSCellAttribute::CellIsBordered => bit(Flags::BORDERED),
                NSCellAttribute::CellAllowsMixedState => bit(Flags::ALLOWS_MIXED),
                _ => 0,
            }
        }

        #[unsafe(method(setCellAttribute:to:))]
        fn set_cell_attribute(&self, attribute: NSCellAttribute, value: isize) {
            let on = value != 0;
            match attribute {
                NSCellAttribute::CellDisabled => self.set_flag(Flags::ENABLED, !on),
                // SAFETY: setState: takes the state.
                NSCellAttribute::CellState => unsafe { msg_send![self, setState: value] },
                NSCellAttribute::CellEditable => self.set_flag(Flags::EDITABLE, on),
                NSCellAttribute::CellHighlighted => self.set_flag(Flags::HIGHLIGHTED, on),
                NSCellAttribute::CellIsBordered => self.set_flag(Flags::BORDERED, on),
                NSCellAttribute::CellAllowsMixedState => self.set_flag(Flags::ALLOWS_MIXED, on),
                _ => {}
            }
        }

        // Text settings.

        #[unsafe(method(alignment))]
        fn alignment(&self) -> NSTextAlignment {
            self.ivars().alignment.get()
        }

        #[unsafe(method(setAlignment:))]
        fn set_alignment(&self, alignment: NSTextAlignment) {
            if self.ivars().alignment.replace(alignment) != alignment {
                changed(self);
            }
        }

        #[unsafe(method(lineBreakMode))]
        fn line_break_mode(&self) -> NSLineBreakMode {
            self.ivars().line_break.get()
        }

        #[unsafe(method(setLineBreakMode:))]
        fn set_line_break_mode(&self, mode: NSLineBreakMode) {
            if self.ivars().line_break.replace(mode) != mode {
                changed(self);
            }
        }

        #[unsafe(method_id(font))]
        fn font(&self) -> Option<Retained<NSFont>> {
            let font = self.ivars().font.borrow().clone();
            font.or_else(|| (self.ivars().kind.get() == NSCellType::TextCellType).then(|| default_font(self)))
        }

        #[unsafe(method(setFont:))]
        fn set_font(&self, font: Option<&NSFont>) {
            let same = match (&*self.ivars().font.borrow(), font) {
                (Some(a), Some(b)) => std::ptr::eq(&**a, b),
                (None, None) => true,
                _ => false,
            };
            if same {
                return;
            }
            let old = self.ivars().font.replace(font.map(|f| f.retain()));
            let mut f = self.ivars().flags.get();
            f.0 = if font.is_some() { f.0 | Flags::FONT_SET } else { f.0 & !Flags::FONT_SET };
            self.ivars().flags.set(f);
            drop(old);
            changed(self);
        }

        #[unsafe(method(baseWritingDirection))]
        fn base_writing_direction(&self) -> NSWritingDirection {
            self.ivars().extras.borrow().as_ref().and_then(|e| e.writing_direction).unwrap_or(NSWritingDirection::Natural)
        }

        #[unsafe(method(setBaseWritingDirection:))]
        fn set_base_writing_direction(&self, direction: NSWritingDirection) {
            extras(self).writing_direction = Some(direction);
            changed(self);
        }

        #[unsafe(method(userInterfaceLayoutDirection))]
        fn user_interface_layout_direction(&self) -> NSUserInterfaceLayoutDirection {
            self.ivars()
                .extras
                .borrow()
                .as_ref()
                .and_then(|e| e.layout_direction)
                .unwrap_or(NSUserInterfaceLayoutDirection::LeftToRight)
        }

        #[unsafe(method(setUserInterfaceLayoutDirection:))]
        fn set_user_interface_layout_direction(&self, direction: NSUserInterfaceLayoutDirection) {
            extras(self).layout_direction = Some(direction);
            changed(self);
        }

        #[unsafe(method(controlSize))]
        fn control_size(&self) -> NSControlSize {
            self.ivars().control_size.get()
        }

        #[unsafe(method(setControlSize:))]
        fn set_control_size(&self, size: NSControlSize) {
            if self.ivars().control_size.replace(size) != size {
                changed(self);
            }
        }

        #[unsafe(method(controlTint))]
        fn control_tint(&self) -> NSControlTint {
            self.ivars().extras.borrow().as_ref().and_then(|e| e.control_tint).unwrap_or(NSControlTint::DefaultControlTint)
        }

        #[unsafe(method(setControlTint:))]
        fn set_control_tint(&self, tint: NSControlTint) {
            extras(self).control_tint = Some(tint);
        }

        #[unsafe(method(focusRingType))]
        fn focus_ring_type(&self) -> NSFocusRingType {
            self.ivars().focus_ring.get()
        }

        #[unsafe(method(setFocusRingType:))]
        fn set_focus_ring_type(&self, kind: NSFocusRingType) {
            if self.ivars().focus_ring.replace(kind) != kind {
                changed(self);
            }
        }

        #[unsafe(method(backgroundStyle))]
        fn background_style(&self) -> NSBackgroundStyle {
            self.ivars().background_style.get()
        }

        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, style: NSBackgroundStyle) {
            if self.ivars().background_style.replace(style) != style {
                changed(self);
            }
        }

        #[unsafe(method(interiorBackgroundStyle))]
        fn interior_background_style(&self) -> NSBackgroundStyle {
            self.ivars().background_style.get()
        }

        #[unsafe(method(entryType))]
        fn entry_type(&self) -> isize {
            self.ivars().extras.borrow().as_ref().map_or(0, |e| e.entry_type)
        }

        #[unsafe(method(setEntryType:))]
        fn set_entry_type(&self, kind: isize) {
            extras(self).entry_type = kind;
        }

        #[unsafe(method(isEntryAcceptable:))]
        fn is_entry_acceptable(&self, _string: &NSString) -> bool {
            true
        }

        #[unsafe(method(setFloatingPointFormat:left:right:))]
        fn set_floating_point_format(&self, _auto_range: bool, _left: usize, _right: usize) {}

        #[unsafe(method(mnemonicLocation))]
        fn mnemonic_location(&self) -> usize {
            self.ivars().extras.borrow().as_ref().and_then(|e| e.mnemonic).unwrap_or(usize::MAX)
        }

        #[unsafe(method(setMnemonicLocation:))]
        fn set_mnemonic_location(&self, location: usize) {
            extras(self).mnemonic = Some(location);
        }

        #[unsafe(method_id(mnemonic))]
        fn mnemonic(&self) -> Retained<NSString> {
            NSString::new()
        }

        #[unsafe(method(setTitleWithMnemonic:))]
        fn set_title_with_mnemonic(&self, title: Option<&NSString>) {
            // Mnemonics are a Windows convention AppKit keeps for
            // compatibility: the ampersand goes, the title stays.
            let text = title.map(|t| t.to_string().replacen('&', "", 1)).unwrap_or_default();
            // SAFETY: setTitle: takes a string.
            unsafe { msg_send![self, setTitle: &*NSString::from_str(&text)] }
        }

        // Events.

        #[unsafe(method(sendActionOn:))]
        fn send_action_on(&self, mask: NSEventMask) -> isize {
            self.ivars().action_mask.replace(mask.0) as isize
        }

        #[unsafe(method(mouseDownFlags))]
        fn mouse_down_flags(&self) -> isize {
            0
        }

        #[unsafe(method(getPeriodicDelay:interval:))]
        fn get_periodic_delay(&self, delay: *mut f32, interval: *mut f32) {
            let (d, i) = super::track::PERIODIC;
            // SAFETY: the caller passes two floats to write.
            unsafe {
                if !delay.is_null() {
                    *delay = d;
                }
                if !interval.is_null() {
                    *interval = i;
                }
            }
        }

        #[unsafe(method(startTrackingAt:inView:))]
        fn start_tracking(&self, _at: NSPoint, _view: &NSView) -> bool {
            // Only cells that act while the mouse is down follow it.
            self.has(Flags::CONTINUOUS) || self.ivars().action_mask.get() & NSEventMask::LeftMouseDragged.0 != 0
        }

        #[unsafe(method(continueTracking:at:inView:))]
        fn continue_tracking(&self, _last: NSPoint, _at: NSPoint, _view: &NSView) -> bool {
            true
        }

        #[unsafe(method(stopTracking:at:inView:mouseIsUp:))]
        fn stop_tracking(&self, _last: NSPoint, _at: NSPoint, _view: &NSView, _up: bool) {}

        #[unsafe(method(trackMouse:inRect:ofView:untilMouseUp:))]
        fn track_mouse(&self, event: &NSEvent, frame: NSRect, view: &NSView, until_up: bool) -> bool {
            super::track::track_mouse(as_cell(self), event, frame, view, until_up)
        }

        #[unsafe(method(hitTestForEvent:inRect:ofView:))]
        fn hit_test_for_event(&self, event: &NSEvent, frame: NSRect, view: &NSView) -> NSCellHitResult {
            let at = view.convertPoint_fromView(event.locationInWindow(), None);
            if !super::track::mouse_in_rect(at, frame, view.isFlipped()) {
                return NSCellHitResult::None;
            }
            let mut result = NSCellHitResult::ContentArea;
            if self.has(Flags::ENABLED) {
                result.0 |= NSCellHitResult::TrackableArea.0;
            }
            if self.has(Flags::EDITABLE) || self.has(Flags::SELECTABLE) {
                result.0 |= NSCellHitResult::EditableTextArea.0;
            }
            result
        }

        #[unsafe(method(performClick:))]
        fn perform_click(&self, _sender: Option<&AnyObject>) {
            super::track::perform_click(as_cell(self));
        }

        #[unsafe(method(highlight:withFrame:inView:))]
        fn highlight(&self, flag: bool, frame: NSRect, view: &NSView) {
            // SAFETY: setHighlighted: takes a BOOL.
            unsafe { msg_send![self, setHighlighted: flag] }
            view.setNeedsDisplayInRect(frame);
        }

        #[unsafe(method_id(highlightColorWithFrame:inView:))]
        fn highlight_color(&self, _frame: NSRect, _view: &NSView) -> Option<Retained<NSColor>> {
            Some(theme::system_color(sel!(selectedContentBackgroundColor), |p| p.accent))
        }

        #[unsafe(method(resetCursorRect:inView:))]
        fn reset_cursor_rect(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(calcDrawInfo:))]
        fn calc_draw_info(&self, _rect: NSRect) {}

        // Geometry.

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            cell_size(self, None)
        }

        #[unsafe(method(cellSizeForBounds:))]
        fn cell_size_for_bounds(&self, bounds: NSRect) -> NSSize {
            if self.ivars().kind.get() != NSCellType::TextCellType {
                return bounds.size;
            }
            cell_size(self, Some(bounds.size.width))
        }

        #[unsafe(method(titleRectForBounds:))]
        fn title_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            text_inset(self, bounds)
        }

        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            text_inset(self, bounds)
        }

        #[unsafe(method(imageRectForBounds:))]
        fn image_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            bounds
        }

        #[unsafe(method(expansionFrameWithFrame:inView:))]
        fn expansion_frame(&self, _frame: NSRect, _view: &NSView) -> NSRect {
            NSRect::ZERO
        }

        #[unsafe(method(drawWithExpansionFrame:inView:))]
        fn draw_with_expansion_frame(&self, frame: NSRect, view: &NSView) {
            // SAFETY: drawWithFrame:inView: takes a rect and a view.
            unsafe { msg_send![self, drawWithFrame: frame, inView: view] }
        }

        // Drawing.

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, view: &NSView) {
            draw_frame(self, frame);
            // SAFETY: drawInteriorWithFrame:inView: takes a rect and a view.
            unsafe { msg_send![self, drawInteriorWithFrame: frame, inView: view] }
        }

        #[unsafe(method(drawInteriorWithFrame:inView:))]
        fn draw_interior_with_frame(&self, frame: NSRect, view: &NSView) {
            if self.ivars().kind.get() != NSCellType::TextCellType {
                return;
            }
            // SAFETY: titleRectForBounds: takes and returns a rect.
            let title: NSRect = unsafe { msg_send![self, titleRectForBounds: frame] };
            let p = theme::palette();
            let color = if self.has(Flags::ENABLED) { p.label } else { p.tertiary_label };
            draw_text(self, title, color, view.isFlipped());
        }

        #[unsafe(method(drawFocusRingMaskWithFrame:inView:))]
        fn draw_focus_ring_mask(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(focusRingMaskBoundsForFrame:inView:))]
        fn focus_ring_mask_bounds(&self, frame: NSRect, _view: &NSView) -> NSRect {
            frame
        }

        // Editing hooks: the field editor lands with the text-editing work.

        #[unsafe(method(editWithFrame:inView:editor:delegate:event:))]
        fn edit_with_frame(&self, frame: NSRect, view: &NSView, editor: &AnyObject, delegate: Option<&AnyObject>, event: Option<&NSEvent>) {
            super::text_field::edit_with_frame(as_cell(self), frame, view, editor, delegate, event);
        }

        #[unsafe(method(selectWithFrame:inView:editor:delegate:start:length:))]
        fn select_with_frame(&self, frame: NSRect, view: &NSView, editor: &AnyObject, delegate: Option<&AnyObject>, start: isize, length: isize) {
            super::text_field::select_with_frame(as_cell(self), frame, view, editor, delegate, start, length);
        }

        #[unsafe(method(endEditing:))]
        fn end_editing(&self, editor: &AnyObject) {
            super::text_field::end_editing(as_cell(self), editor);
        }

        #[unsafe(method_id(fieldEditorForView:))]
        fn field_editor_for_view(&self, view: &NSView) -> Option<Retained<AnyObject>> {
            super::text_field::field_editor(as_cell(self), view)
        }

        #[unsafe(method_id(setUpFieldEditorAttributes:))]
        fn set_up_field_editor_attributes(&self, editor: &AnyObject) -> Retained<AnyObject> {
            editor.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSCellImpl {}
);

impl NSCellImpl {
    pub(crate) fn has(&self, bit: u32) -> bool {
        self.ivars().flags.get().has(bit)
    }

    pub(crate) fn set_flag(&self, bit: u32, on: bool) {
        let f = self.ivars().flags.get();
        self.store_flags(Flags(if on { f.0 | bit } else { f.0 & !bit }));
    }

    fn store_flags(&self, f: Flags) {
        if self.ivars().flags.replace(f) != f {
            changed(self);
        }
    }

    pub(crate) fn value(&self) -> std::cell::Ref<'_, Value> {
        self.ivars().value.borrow()
    }

    pub(crate) fn replace_value(&self, value: Value) -> Value {
        self.ivars().value.replace(value)
    }

    pub(crate) fn raw_state(&self) -> &Cell<isize> {
        &self.ivars().state
    }

    pub(crate) fn action_mask(&self) -> u64 {
        self.ivars().action_mask.get()
    }

    pub(crate) fn control_size_index(&self) -> usize {
        metrics::size_index(self.ivars().control_size.get())
    }

    /// The font a program set, if it set one.
    pub(crate) fn font_set(&self) -> Option<Retained<NSFont>> {
        if self.has(Flags::FONT_SET) { self.ivars().font.borrow().clone() } else { None }
    }

    /// The view showing the cell.
    pub(crate) fn view(&self) -> Option<Retained<NSView>> {
        self.ivars().control_view.borrow().load()
    }
}

/// Any cell as the implementation class, which every cell inherits from.
pub(crate) fn imp(cell: &NSCell) -> &NSCellImpl {
    // SAFETY: NSCell is NSCellImpl's class; subclasses share its layout.
    unsafe { &*(cell as *const NSCell).cast::<NSCellImpl>() }
}

pub(crate) fn as_cell(cell: &NSCellImpl) -> &NSCell {
    // SAFETY: as in `imp`.
    unsafe { &*(cell as *const NSCellImpl).cast::<NSCell>() }
}

fn extras(cell: &NSCellImpl) -> std::cell::RefMut<'_, Extras> {
    std::cell::RefMut::map(cell.ivars().extras.borrow_mut(), |e| &mut **e.get_or_insert_with(Box::default))
}

/// The state after `state`: on after off, or after mixed; off after on;
/// and mixed after off, if allowed (`conformance/tests/controls.rs`,
/// `cell_state`).
pub(crate) fn next_state(state: isize, mixed: bool) -> isize {
    match state {
        0 if mixed => -1,
        0 => 1,
        1 => 0,
        _ => 1,
    }
}

/// Store a state: negative values are mixed where that's allowed, and
/// anything else but off is on.
pub(crate) fn set_state(cell: &NSCellImpl, state: isize) {
    let state = match state {
        0 => 0,
        s if s < 0 && cell.has(Flags::ALLOWS_MIXED) => -1,
        _ => 1,
    };
    if cell.ivars().state.replace(state) != state {
        changed(cell);
    }
}

/// Replace a cell's value. Giving a plain cell a value makes it a text cell.
fn set_value(cell: &NSCellImpl, value: Value) {
    if cell.ivars().kind.get() == NSCellType::NullCellType {
        cell.ivars().kind.set(NSCellType::TextCellType);
    }
    let old = cell.ivars().value.replace(value);
    drop(old);
    changed(cell);
}

/// Something that shows changed: tell the control showing the cell, which
/// redraws it and forgets its size.
pub(crate) fn changed(cell: &NSCellImpl) {
    let Some(view) = cell.ivars().control_view.borrow().load() else { return };
    if let Some(control) = super::control::as_control(&view) {
        super::control::cell_changed(control, as_cell(cell));
    } else {
        view.setNeedsDisplay(true);
    }
}

/// The font text cells use when none was set: the system font, whatever
/// the control size (buttons measure their titles at the size's own; see
/// `button`).
fn default_font(_cell: &NSCellImpl) -> Retained<NSFont> {
    NSFont::systemFontOfSize(metrics::FONT_SIZE[0])
}

/// The font to draw and measure with: the one set, or the default.
pub(crate) fn font_of(cell: &NSCellImpl) -> Retained<NSFont> {
    cell.ivars().font.borrow().clone().unwrap_or_else(|| default_font(cell))
}

/// The layout's alignment for `alignment`.
pub(crate) fn align_of(alignment: NSTextAlignment) -> Align {
    if alignment == NSTextAlignment::Left {
        Align::Left
    } else if alignment == NSTextAlignment::Right {
        Align::Right
    } else if alignment == NSTextAlignment::Center {
        Align::Center
    } else if alignment == NSTextAlignment::Justified {
        Align::Justified
    } else {
        Align::Natural
    }
}

/// The layout's line breaking for `mode` in a cell that wraps or doesn't.
pub(crate) fn line_break_of(mode: NSLineBreakMode, wraps: bool) -> LineBreak {
    match mode {
        NSLineBreakMode::ByCharWrapping if wraps => LineBreak::CharWrap,
        NSLineBreakMode::ByWordWrapping if wraps => LineBreak::WordWrap,
        NSLineBreakMode::ByTruncatingHead => LineBreak::TruncateHead,
        NSLineBreakMode::ByTruncatingTail => LineBreak::TruncateTail,
        NSLineBreakMode::ByTruncatingMiddle => LineBreak::TruncateMiddle,
        _ => LineBreak::Clip,
    }
}

/// The attributes text in `cell` is laid out with, in `color`, at `font`.
pub(crate) fn attrs(cell: &NSCellImpl, font: &NSFont, color: Color) -> Attrs {
    let mut a = Attrs::new(crate::font::text_font(font));
    a.color = color;
    a.paragraph = Paragraph {
        alignment: align_of(cell.ivars().alignment.get()),
        line_break: line_break_of(
            cell.ivars().line_break.get(),
            cell.has(Flags::WRAPS) && !cell.has(Flags::SINGLE_LINE),
        ),
        ..Paragraph::default()
    };
    a
}

/// A cell's text and its attribute runs: one run for a plain string, the
/// string's own runs over the cell's attributes for an attributed one.
pub(crate) struct Styled {
    pub text: String,
    pub attrs: Vec<Attrs>,
    pub runs: Vec<Run>,
}

impl Styled {
    pub fn plain(text: String, attrs: Attrs) -> Styled {
        let runs = vec![Run { start: 0, end: text.len(), attrs: 0 }];
        Styled { text, attrs: vec![attrs], runs }
    }

    /// An attributed string over `base`: attributes it doesn't set are
    /// `base`'s.
    pub fn attributed(string: &NSAttributedString, base: &Attrs) -> Styled {
        sidestep_foundation::with_runs(string, |text, runs| {
            let mut attrs = Vec::with_capacity(runs.len());
            let mut out = Vec::with_capacity(runs.len());
            for (i, run) in runs.iter().enumerate() {
                attrs.push(merged(&run.attrs, base));
                out.push(Run { start: run.utf8.start, end: run.utf8.end, attrs: i as u32 });
            }
            if out.is_empty() {
                attrs.push(base.clone());
                out.push(Run { start: 0, end: text.len(), attrs: 0 });
            }
            Styled { text: text.to_string(), attrs, runs: out }
        })
    }

    /// The styled text of `value` over `base`.
    pub fn of(value: &Value, base: Attrs) -> Styled {
        match value.attributed() {
            Some(a) => Styled::attributed(a, &base),
            None => Styled::plain(value.string().to_string(), base),
        }
    }

    /// The text's size laid out `width` wide (unbounded if none).
    pub fn size(&self, width: Option<f64>) -> NSSize {
        let opts = Options { width: width.map_or(f32::INFINITY, |w| w.max(0.0) as f32), ..Options::UNBOUNDED };
        let laid = crate::text::layout::lay_out(&self.text, &self.attrs, &self.runs, &opts);
        NSSize::new(f64::from(laid.width), f64::from(laid.height))
    }

    /// Draw into `r`, clipped to it.
    pub fn draw(&self, r: NSRect) {
        match self.attrs.as_slice() {
            [one] if self.runs.len() == 1 => theme::paint::text(&self.text, one, r),
            _ => theme::paint::text_runs(&self.text, &self.attrs, &self.runs, r),
        }
    }
}

/// A run's attribute dictionary over `base`: what it names replaces
/// `base`'s.
fn merged(dict: &objc2_foundation::NSDictionary<NSString, AnyObject>, base: &Attrs) -> Attrs {
    use objc2_app_kit::{NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName};
    let mut a = crate::string_drawing::attrs_of(Some(dict));
    // SAFETY: the keys are constants this crate exports.
    let (font, color, paragraph) =
        unsafe { (NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName) };
    if dict.objectForKey(font).is_none() {
        a.font = base.font.clone();
    }
    if dict.objectForKey(color).is_none() {
        a.color = base.color;
    }
    if dict.objectForKey(paragraph).is_none() {
        a.paragraph = base.paragraph.clone();
    }
    a
}

/// The insets text cells keep round their text: a bezel's, a border's, or
/// none.
fn frame_inset(cell: &NSCellImpl) -> f64 {
    if cell.has(Flags::BEZELED) {
        metrics::BEZEL_INSET
    } else if cell.has(Flags::BORDERED) {
        metrics::BORDER_INSET
    } else {
        0.0
    }
}

fn text_inset(cell: &NSCellImpl, bounds: NSRect) -> NSRect {
    let d = frame_inset(cell);
    NSRect::new(
        NSPoint::new(bounds.origin.x + d, bounds.origin.y + d),
        NSSize::new(bounds.size.width - 2.0 * d, bounds.size.height - 2.0 * d),
    )
}

/// A plain or text cell's size: unbounded without content; otherwise its
/// text's, padded, laid out `width` wide if given.
fn cell_size(cell: &NSCellImpl, width: Option<f64>) -> NSSize {
    if cell.ivars().kind.get() != NSCellType::TextCellType {
        return NSSize::new(metrics::UNBOUNDED_CELL, metrics::UNBOUNDED_CELL);
    }
    let inset = frame_inset(cell);
    let pad = 2.0 * (metrics::TEXT_PADDING + inset);
    let styled = styled(cell, [0.0; 4]);
    let wraps = cell.has(Flags::WRAPS) && !cell.has(Flags::SINGLE_LINE);
    let text = styled.size(width.filter(|_| wraps).map(|w| w - pad));
    NSSize::new(text.width + pad, text.height + 2.0 * inset)
}

/// The cell's value styled with its settings, in `color`.
pub(crate) fn styled(cell: &NSCellImpl, color: Color) -> Styled {
    let font = font_of(cell);
    let base = attrs(cell, &font, color);
    Styled::of(&cell.ivars().value.borrow(), base)
}

/// Draw the cell's text in `title` (the title rect), padded as text cells
/// pad it.
pub(crate) fn draw_text(cell: &NSCellImpl, title: NSRect, color: Color, _flipped: bool) {
    if !theme::paint::recording() {
        return;
    }
    let styled = styled(cell, color);
    let r = NSRect::new(
        NSPoint::new(title.origin.x + metrics::TEXT_PADDING, title.origin.y),
        NSSize::new(title.size.width - 2.0 * metrics::TEXT_PADDING, title.size.height),
    );
    styled.draw(r);
}

/// Draw a text cell's bezel or border.
fn draw_frame(cell: &NSCellImpl, frame: NSRect) {
    if !theme::paint::recording() || cell.ivars().kind.get() != NSCellType::TextCellType {
        return;
    }
    let p = theme::palette();
    let state = parts::State { disabled: !cell.has(Flags::ENABLED), ..parts::State::default() };
    if cell.has(Flags::BEZELED) {
        parts::entry(p, frame, None, state);
    } else if cell.has(Flags::BORDERED) {
        parts::plain_border(p, frame, state);
    }
}

// NSActionCell

pub(crate) struct ActionIvars {
    target: RefCell<Weak<AnyObject>>,
    action: Cell<Option<Sel>>,
    tag: Cell<isize>,
}

define_class!(
    #[unsafe(super(NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSActionCell"]
    #[ivars = ActionIvars]
    pub(crate) struct NSActionCellImpl;

    impl NSActionCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ActionIvars::new());
            // SAFETY: NSCell's initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, string: &NSString) -> Retained<Self> {
            let this = this.set_ivars(ActionIvars::new());
            // SAFETY: NSCell's initializer.
            unsafe { msg_send![super(this), initTextCell: string] }
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, image: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(ActionIvars::new());
            // SAFETY: NSCell's initializer.
            unsafe { msg_send![super(this), initImageCell: image] }
        }

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            self.ivars().target.borrow().load()
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            self.ivars().target.replace(target.map_or_else(Weak::default, Weak::new));
        }

        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> {
            self.ivars().action.get()
        }

        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) {
            self.ivars().action.set(action);
        }

        #[unsafe(method(tag))]
        fn tag(&self) -> isize {
            self.ivars().tag.get()
        }

        #[unsafe(method(setTag:))]
        fn set_tag(&self, tag: isize) {
            self.ivars().tag.set(tag);
        }
    }

    unsafe impl NSObjectProtocol for NSActionCellImpl {}
);

impl ActionIvars {
    fn new() -> Self {
        ActionIvars { target: RefCell::new(Weak::default()), action: Cell::new(None), tag: Cell::new(0) }
    }
}

/// A new cell of `class` (a cell class, found by message) made with `init`.
pub(crate) fn make(class: &AnyClass, mtm: MainThreadMarker) -> Retained<NSCell> {
    let _ = mtm;
    // SAFETY: cell classes answer alloc and init with a cell.
    unsafe {
        let allocated: Allocated<NSCell> = msg_send![class, alloc];
        msg_send![allocated, init]
    }
}
