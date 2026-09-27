//! `NSControl`: a view that shows a cell and sends its action.
//!
//! A control makes its cell from `+cellClass` when it's made (subclasses
//! name their cell class; `+setCellClass:` replaces it for a class and its
//! subclasses) and forwards the cell's properties to it by message, so a
//! cell subclass's overrides apply. The control's `tag` is its own; the
//! selected tag is the cell's. A control without a cell still keeps a
//! value, enabled state and the rest, as AppKit's does.
//!
//! The control remembers its intrinsic size until the cell changes size
//! (the cell calls [`cell_changed`], which is what `updateCell:` does), so
//! sizing a window full of labels measures each once; changes to a cell's
//! look alone (`updateCellInside:`) only redraw.
//!
//! `sizeThatFits:` is the cell's size rounded up, except along an axis
//! where the cell has no size of its own, which takes the proposed size;
//! a control without a cell fits whatever it's offered (text fields answer
//! their own way; see `text_field`).

use std::cell::{Cell, RefCell};
use std::sync::Mutex;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackgroundStyle, NSCell, NSControl, NSControlSize, NSEvent, NSEventMask, NSFont, NSLineBreakMode,
    NSResponder, NSTextAlignment, NSView, NSWritingDirection,
};
use objc2_foundation::{NSAttributedString, NSCopying, NSPoint, NSRect, NSSize, NSString};

use super::cell;
use super::value::Value;
use crate::theme::metrics;
use crate::views;

/// No intrinsic size, along one axis (`NSViewNoIntrinsicMetric`).
pub(crate) const NO_METRIC: f64 = -1.0;

/// Cell classes: each control class's own (registered when its shell
/// loads) and any set with `+setCellClass:`, by the class they belong to.
/// Read when a control is made.
static CELL_CLASSES: Mutex<Vec<(usize, Option<&'static AnyClass>)>> = Mutex::new(Vec::new());

/// Give `control` (a control class) `cell` as its cell class, until a
/// program sets another.
pub(crate) fn register_cell_class(control: &'static AnyClass, cell: &'static AnyClass) {
    set_cell_class_of(control, Some(cell));
}

fn set_cell_class_of(control: &AnyClass, cell: Option<&'static AnyClass>) {
    let key = control as *const AnyClass as usize;
    let mut classes = CELL_CLASSES.lock().unwrap_or_else(|e| e.into_inner());
    match classes.iter_mut().find(|(k, _)| *k == key) {
        Some(entry) => entry.1 = cell,
        None => classes.push((key, cell)),
    }
}

/// `+[NSControl cellClass]`: the cell class set on the receiver or its
/// nearest superclass that has one.
extern "C-unwind" fn cell_class_imp(receiver: &AnyClass, _cmd: Sel) -> Option<&'static AnyClass> {
    let classes = CELL_CLASSES.lock().unwrap_or_else(|e| e.into_inner());
    let mut class = Some(receiver);
    while let Some(c) = class {
        let key = c as *const AnyClass as usize;
        if let Some((_, cell)) = classes.iter().find(|(k, _)| *k == key) {
            return *cell;
        }
        class = c.superclass();
    }
    None
}

/// `+[NSControl setCellClass:]`, for the receiver and its subclasses.
extern "C-unwind" fn set_cell_class_imp(receiver: &AnyClass, _cmd: Sel, cell: Option<&AnyClass>) {
    // SAFETY: classes live as long as the program.
    let cell: Option<&'static AnyClass> = cell.map(|c| unsafe { &*(c as *const AnyClass) });
    set_cell_class_of(receiver, cell);
}

/// Give NSControl's metaclass `+cellClass` and `+setCellClass:`, which
/// need their receiver (a subclass's cell class is its own); `define_class!`
/// class methods don't see theirs.
pub(crate) fn install_class_methods(control: &AnyClass) {
    let meta = control.metaclass();
    // SAFETY: the functions take the receiver and selector, then the
    // arguments the encodings give, and return what they say; methods are
    // called with the C ABI objc2 uses for these types.
    unsafe {
        let get: unsafe extern "C-unwind" fn() =
            std::mem::transmute(cell_class_imp as extern "C-unwind" fn(&AnyClass, Sel) -> Option<&'static AnyClass>);
        objc2::ffi::class_addMethod((meta as *const AnyClass).cast_mut(), sel!(cellClass), get, c"#@:".as_ptr());
        let set: unsafe extern "C-unwind" fn() =
            std::mem::transmute(set_cell_class_imp as extern "C-unwind" fn(&AnyClass, Sel, Option<&AnyClass>));
        objc2::ffi::class_addMethod((meta as *const AnyClass).cast_mut(), sel!(setCellClass:), set, c"v@:#".as_ptr());
    }
}

/// What a control keeps when it has no cell (`NSSwitch` has none).
struct Loose {
    value: Value,
    enabled: bool,
    target: Weak<AnyObject>,
    action: Option<Sel>,
}

pub(crate) struct ControlIvars {
    cell: RefCell<Option<Retained<NSCell>>>,
    tag: Cell<isize>,
    ignores_multi_click: Cell<bool>,
    expansion_tool_tips: Cell<bool>,
    loose: RefCell<Loose>,
    /// The intrinsic size, once measured, until the cell changes.
    intrinsic: Cell<Option<NSSize>>,
}

impl Default for ControlIvars {
    fn default() -> Self {
        ControlIvars {
            cell: RefCell::new(None),
            tag: Cell::new(0),
            ignores_multi_click: Cell::new(false),
            expansion_tool_tips: Cell::new(false),
            loose: RefCell::new(Loose { value: Value::Empty, enabled: true, target: Weak::default(), action: None }),
            intrinsic: Cell::new(None),
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSControl"]
    #[ivars = ControlIvars]
    pub(crate) struct NSControlImpl;

    impl NSControlImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ControlIvars::default());
            // SAFETY: NSView's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
            // SAFETY: +cellClass takes nothing and returns a class or nil.
            let class: Option<&AnyClass> = unsafe { msg_send![this.class(), cellClass] };
            if let Some(class) = class {
                let cell = cell::make(class, this.mtm());
                install_cell(&this, Some(&cell));
            }
            this
        }

        #[unsafe(method_id(cell))]
        fn cell(&self) -> Option<Retained<NSCell>> {
            self.ivars().cell.borrow().clone()
        }

        #[unsafe(method(setCell:))]
        fn set_cell(&self, cell: Option<&NSCell>) {
            install_cell(self, cell);
        }

        #[unsafe(method_id(selectedCell))]
        fn selected_cell(&self) -> Option<Retained<NSCell>> {
            self.ivars().cell.borrow().clone()
        }

        #[unsafe(method(selectedTag))]
        fn selected_tag(&self) -> isize {
            self.the_cell().map_or(-1, |c| c.tag())
        }

        #[unsafe(method(tag))]
        fn tag(&self) -> isize {
            self.ivars().tag.get()
        }

        #[unsafe(method(setTag:))]
        fn set_tag(&self, tag: isize) {
            self.ivars().tag.set(tag);
        }

        #[unsafe(method_id(target))]
        fn target(&self) -> Option<Retained<AnyObject>> {
            match self.the_cell() {
                Some(c) => c.target(),
                None => self.ivars().loose.borrow().target.load(),
            }
        }

        #[unsafe(method(setTarget:))]
        fn set_target(&self, target: Option<&AnyObject>) {
            match self.the_cell() {
                // SAFETY: the cell keeps the target weakly.
                Some(c) => unsafe { c.setTarget(target) },
                None => self.ivars().loose.borrow_mut().target = target.map_or_else(Weak::default, Weak::new),
            }
        }

        #[unsafe(method(action))]
        fn action(&self) -> Option<Sel> {
            match self.the_cell() {
                Some(c) => c.action(),
                None => self.ivars().loose.borrow().action,
            }
        }

        #[unsafe(method(setAction:))]
        fn set_action(&self, action: Option<Sel>) {
            match self.the_cell() {
                // SAFETY: any selector may be an action.
                Some(c) => unsafe { c.setAction(action) },
                None => self.ivars().loose.borrow_mut().action = action,
            }
        }

        #[unsafe(method(ignoresMultiClick))]
        fn ignores_multi_click(&self) -> bool {
            self.ivars().ignores_multi_click.get()
        }

        #[unsafe(method(setIgnoresMultiClick:))]
        fn set_ignores_multi_click(&self, flag: bool) {
            self.ivars().ignores_multi_click.set(flag);
        }

        #[unsafe(method(isContinuous))]
        fn is_continuous(&self) -> bool {
            self.the_cell().is_some_and(|c| c.isContinuous())
        }

        #[unsafe(method(setContinuous:))]
        fn set_continuous(&self, flag: bool) {
            if let Some(c) = self.the_cell() {
                c.setContinuous(flag);
            }
        }

        #[unsafe(method(isEnabled))]
        fn is_enabled(&self) -> bool {
            match self.the_cell() {
                Some(c) => c.isEnabled(),
                None => self.ivars().loose.borrow().enabled,
            }
        }

        #[unsafe(method(setEnabled:))]
        fn set_enabled(&self, flag: bool) {
            match self.the_cell() {
                // The cell redraws the control if that changed anything.
                Some(c) => c.setEnabled(flag),
                None => {
                    let was = std::mem::replace(&mut self.ivars().loose.borrow_mut().enabled, flag);
                    if was != flag {
                        self.as_view().setNeedsDisplay(true);
                    }
                }
            }
        }

        #[unsafe(method(refusesFirstResponder))]
        fn refuses_first_responder(&self) -> bool {
            self.the_cell().is_some_and(|c| c.refusesFirstResponder())
        }

        #[unsafe(method(setRefusesFirstResponder:))]
        fn set_refuses_first_responder(&self, flag: bool) {
            if let Some(c) = self.the_cell() {
                c.setRefusesFirstResponder(flag);
            }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.the_cell().is_none_or(|c| c.acceptsFirstResponder())
        }

        #[unsafe(method(isHighlighted))]
        fn is_highlighted(&self) -> bool {
            self.the_cell().is_some_and(|c| c.isHighlighted())
        }

        #[unsafe(method(setHighlighted:))]
        fn set_highlighted(&self, flag: bool) {
            if let Some(c) = self.the_cell() {
                c.setHighlighted(flag);
            }
        }

        #[unsafe(method(controlSize))]
        fn control_size(&self) -> NSControlSize {
            self.the_cell().map_or(NSControlSize::Regular, |c| c.controlSize())
        }

        #[unsafe(method(setControlSize:))]
        fn set_control_size(&self, size: NSControlSize) {
            if let Some(c) = self.the_cell() {
                c.setControlSize(size);
            }
        }

        /// The cell's, as on macOS, where a control answers
        /// `backgroundStyle` but has no setter: whoever gives a control a
        /// background style gives it to the cell
        /// (`conformance/tests/cell_backgrounds.rs`, `who_takes_styles`).
        #[unsafe(method(backgroundStyle))]
        fn background_style(&self) -> NSBackgroundStyle {
            self.the_cell().map_or(NSBackgroundStyle::Normal, |c| c.backgroundStyle())
        }

        #[unsafe(method_id(formatter))]
        fn formatter(&self) -> Option<Retained<AnyObject>> {
            // SAFETY: formatter takes nothing and returns a formatter or nil.
            self.the_cell().and_then(|c| unsafe { msg_send![&*c, formatter] })
        }

        #[unsafe(method(setFormatter:))]
        fn set_formatter(&self, formatter: Option<&AnyObject>) {
            if let Some(c) = self.the_cell() {
                // SAFETY: setFormatter: takes a formatter or nil.
                unsafe { msg_send![&*c, setFormatter: formatter] }
            }
        }

        // The value, from the cell or kept here without one.

        #[unsafe(method_id(objectValue))]
        fn object_value(&self) -> Option<Retained<AnyObject>> {
            match self.the_cell() {
                Some(c) => c.objectValue(),
                None => self.loose_value().object(),
            }
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, object: Option<&AnyObject>) {
            match self.the_cell() {
                // SAFETY: any object, or nil, is an object value.
                Some(c) => unsafe { c.setObjectValue(object) },
                None => self.set_loose(Value::from_object(object)),
            }
        }

        #[unsafe(method_id(stringValue))]
        fn string_value(&self) -> Retained<NSString> {
            match self.the_cell() {
                Some(c) => c.stringValue(),
                None => self.loose_value().string(),
            }
        }

        #[unsafe(method(setStringValue:))]
        fn set_string_value(&self, string: &NSString) {
            match self.the_cell() {
                Some(c) => c.setStringValue(string),
                None => self.set_loose(Value::String(string.copy())),
            }
        }

        #[unsafe(method_id(attributedStringValue))]
        fn attributed_string_value(&self) -> Retained<NSAttributedString> {
            match self.the_cell() {
                Some(c) => c.attributedStringValue(),
                None => cell::attributed(
                    &self.loose_value().string(),
                    &NSFont::systemFontOfSize(metrics::FONT_SIZE[0]),
                    NSTextAlignment::Left,
                    NSLineBreakMode::ByWordWrapping,
                    &cell::text_color(sel!(controlTextColor)),
                ),
            }
        }

        #[unsafe(method(setAttributedStringValue:))]
        fn set_attributed_string_value(&self, string: &NSAttributedString) {
            match self.the_cell() {
                Some(c) => c.setAttributedStringValue(string),
                None => self.set_loose(Value::from_object(Some(string))),
            }
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            match self.the_cell() {
                Some(c) => c.intValue(),
                None => self.loose_value().int(),
            }
        }

        #[unsafe(method(setIntValue:))]
        fn set_int_value(&self, value: i32) {
            match self.the_cell() {
                Some(c) => c.setIntValue(value),
                None => self.set_loose(Value::Int(i64::from(value))),
            }
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> isize {
            match self.the_cell() {
                Some(c) => c.integerValue(),
                None => self.loose_value().integer(),
            }
        }

        #[unsafe(method(setIntegerValue:))]
        fn set_integer_value(&self, value: isize) {
            match self.the_cell() {
                Some(c) => c.setIntegerValue(value),
                None => self.set_loose(Value::Int(value as i64)),
            }
        }

        #[unsafe(method(floatValue))]
        fn float_value(&self) -> f32 {
            match self.the_cell() {
                Some(c) => c.floatValue(),
                None => self.loose_value().float(),
            }
        }

        #[unsafe(method(setFloatValue:))]
        fn set_float_value(&self, value: f32) {
            match self.the_cell() {
                Some(c) => c.setFloatValue(value),
                None => self.set_loose(Value::Float(value)),
            }
        }

        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            match self.the_cell() {
                Some(c) => c.doubleValue(),
                None => self.loose_value().double(),
            }
        }

        #[unsafe(method(setDoubleValue:))]
        fn set_double_value(&self, value: f64) {
            match self.the_cell() {
                Some(c) => c.setDoubleValue(value),
                None => self.set_loose(Value::Double(value)),
            }
        }

        #[unsafe(method(takeIntValueFrom:))]
        fn take_int_value_from(&self, sender: Option<&AnyObject>) {
            // SAFETY: the cell's method, with the sender.
            self.forward(|c| unsafe { c.takeIntValueFrom(sender) });
        }

        #[unsafe(method(takeIntegerValueFrom:))]
        fn take_integer_value_from(&self, sender: Option<&AnyObject>) {
            // SAFETY: as above.
            self.forward(|c| unsafe { c.takeIntegerValueFrom(sender) });
        }

        #[unsafe(method(takeFloatValueFrom:))]
        fn take_float_value_from(&self, sender: Option<&AnyObject>) {
            // SAFETY: as above.
            self.forward(|c| unsafe { c.takeFloatValueFrom(sender) });
        }

        #[unsafe(method(takeDoubleValueFrom:))]
        fn take_double_value_from(&self, sender: Option<&AnyObject>) {
            // SAFETY: as above.
            self.forward(|c| unsafe { c.takeDoubleValueFrom(sender) });
        }

        #[unsafe(method(takeStringValueFrom:))]
        fn take_string_value_from(&self, sender: Option<&AnyObject>) {
            // SAFETY: as above.
            self.forward(|c| unsafe { c.takeStringValueFrom(sender) });
        }

        #[unsafe(method(takeObjectValueFrom:))]
        fn take_object_value_from(&self, sender: Option<&AnyObject>) {
            // SAFETY: as above.
            self.forward(|c| unsafe { c.takeObjectValueFrom(sender) });
        }

        // Text settings, the cell's.

        #[unsafe(method_id(font))]
        fn font(&self) -> Option<Retained<NSFont>> {
            self.the_cell().and_then(|c| c.font())
        }

        #[unsafe(method(setFont:))]
        fn set_font(&self, font: Option<&NSFont>) {
            self.forward(|c| c.setFont(font));
        }

        #[unsafe(method(usesSingleLineMode))]
        fn uses_single_line_mode(&self) -> bool {
            self.the_cell().is_some_and(|c| c.usesSingleLineMode())
        }

        #[unsafe(method(setUsesSingleLineMode:))]
        fn set_uses_single_line_mode(&self, flag: bool) {
            self.forward(|c| c.setUsesSingleLineMode(flag));
        }

        #[unsafe(method(lineBreakMode))]
        fn line_break_mode(&self) -> NSLineBreakMode {
            self.the_cell().map_or(NSLineBreakMode::ByWordWrapping, |c| c.lineBreakMode())
        }

        #[unsafe(method(setLineBreakMode:))]
        fn set_line_break_mode(&self, mode: NSLineBreakMode) {
            self.forward(|c| c.setLineBreakMode(mode));
        }

        #[unsafe(method(alignment))]
        fn alignment(&self) -> NSTextAlignment {
            self.the_cell().map_or(NSTextAlignment::Left, |c| c.alignment())
        }

        #[unsafe(method(setAlignment:))]
        fn set_alignment(&self, alignment: NSTextAlignment) {
            self.forward(|c| c.setAlignment(alignment));
        }

        #[unsafe(method(baseWritingDirection))]
        fn base_writing_direction(&self) -> NSWritingDirection {
            self.the_cell().map_or(NSWritingDirection::Natural, |c| c.baseWritingDirection())
        }

        #[unsafe(method(setBaseWritingDirection:))]
        fn set_base_writing_direction(&self, direction: NSWritingDirection) {
            self.forward(|c| c.setBaseWritingDirection(direction));
        }

        #[unsafe(method(allowsExpansionToolTips))]
        fn allows_expansion_tool_tips(&self) -> bool {
            self.ivars().expansion_tool_tips.get()
        }

        #[unsafe(method(setAllowsExpansionToolTips:))]
        fn set_allows_expansion_tool_tips(&self, flag: bool) {
            self.ivars().expansion_tool_tips.set(flag);
        }

        #[unsafe(method(expansionFrameWithFrame:))]
        fn expansion_frame_with_frame(&self, _frame: NSRect) -> NSRect {
            NSRect::ZERO
        }

        #[unsafe(method(drawWithExpansionFrame:inView:))]
        fn draw_with_expansion_frame(&self, _frame: NSRect, _view: &NSView) {}

        #[unsafe(method(setFloatingPointFormat:left:right:))]
        fn set_floating_point_format(&self, _auto_range: bool, _left: usize, _right: usize) {}

        // Actions.

        #[unsafe(method(sendActionOn:))]
        fn send_action_on(&self, mask: NSEventMask) -> isize {
            self.the_cell().map_or(0, |c| c.sendActionOn(mask))
        }

        #[unsafe(method(sendAction:to:))]
        fn send_action(&self, action: Option<Sel>, target: Option<&AnyObject>) -> bool {
            send_action(self.as_view(), action, target)
        }

        #[unsafe(method(performClick:))]
        fn perform_click(&self, sender: Option<&AnyObject>) {
            if let Some(c) = self.the_cell() {
                // SAFETY: the cell's performClick: takes a sender.
                unsafe { c.performClick(sender) };
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            super::track::control_mouse_down(self.as_control(), event);
        }

        // Sizing.

        #[unsafe(method(sizeThatFits:))]
        fn size_that_fits(&self, size: NSSize) -> NSSize {
            match self.the_cell() {
                Some(c) => {
                    let s = c.cellSize();
                    let axis = |v: f64, proposed: f64| if v >= metrics::UNBOUNDED_CELL { proposed } else { v.ceil() };
                    NSSize::new(axis(s.width, size.width), axis(s.height, size.height))
                }
                None => size,
            }
        }

        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {
            // SAFETY: sizeThatFits: takes and returns a size.
            let size: NSSize = unsafe { msg_send![self, sizeThatFits: views::frame(views::imp(self.as_view())).size] };
            self.as_view().setFrameSize(size);
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            if let Some(size) = self.ivars().intrinsic.get() {
                return size;
            }
            let size = match self.the_cell() {
                Some(c) => {
                    let s = c.cellSize();
                    let axis = |v: f64| if v >= metrics::UNBOUNDED_CELL { NO_METRIC } else { v.ceil() };
                    NSSize::new(axis(s.width), axis(s.height))
                }
                None => NSSize::new(NO_METRIC, NO_METRIC),
            };
            self.ivars().intrinsic.set(Some(size));
            size
        }

        #[unsafe(method(invalidateIntrinsicContentSize))]
        fn invalidate_intrinsic_content_size(&self) {
            self.ivars().intrinsic.set(None);
        }

        #[unsafe(method(invalidateIntrinsicContentSizeForCell:))]
        fn invalidate_intrinsic_content_size_for_cell(&self, _cell: &NSCell) {
            self.ivars().intrinsic.set(None);
        }

        #[unsafe(method(calcSize))]
        fn calc_size(&self) {}

        // Redrawing when the cell changes.

        #[unsafe(method(setNeedsDisplay))]
        fn set_needs_display(&self) {
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(updateCell:))]
        fn update_cell(&self, _cell: &NSCell) {
            self.ivars().intrinsic.set(None);
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(updateCellInside:))]
        fn update_cell_inside(&self, _cell: &NSCell) {
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(drawCell:))]
        fn draw_cell(&self, _cell: &NSCell) {
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(drawCellInside:))]
        fn draw_cell_inside(&self, _cell: &NSCell) {
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(selectCell:))]
        fn select_cell(&self, cell: &NSCell) {
            if self.the_cell().is_some_and(|c| std::ptr::eq(&*c, cell)) {
                cell.setState(1);
            }
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if let Some(c) = self.the_cell() {
                let bounds = views::bounds(views::imp(self.as_view()));
                c.drawWithFrame_inView(bounds, self.as_view());
            }
        }

        // Editing, which the text-editing work fills in.

        #[unsafe(method_id(currentEditor))]
        fn current_editor(&self) -> Option<Retained<AnyObject>> {
            super::text_field::current_editor(self.as_control())
        }

        #[unsafe(method(abortEditing))]
        fn abort_editing(&self) -> bool {
            super::text_field::abort_editing(self.as_control())
        }

        #[unsafe(method(validateEditing))]
        fn validate_editing(&self) {
            super::text_field::validate_editing(self.as_control());
        }

        #[unsafe(method(editWithFrame:editor:delegate:event:))]
        fn edit_with_frame(&self, frame: NSRect, editor: &AnyObject, delegate: Option<&AnyObject>, event: &NSEvent) {
            if let Some(c) = self.the_cell() {
                // SAFETY: the cell's method, with the control as its view.
                unsafe { msg_send![&*c, editWithFrame: frame, inView: self.as_view(), editor: editor, delegate: delegate, event: event] }
            }
        }

        #[unsafe(method(selectWithFrame:editor:delegate:start:length:))]
        fn select_with_frame(&self, frame: NSRect, editor: &AnyObject, delegate: Option<&AnyObject>, start: isize, length: isize) {
            if let Some(c) = self.the_cell() {
                // SAFETY: as above.
                unsafe {
                    msg_send![&*c, selectWithFrame: frame, inView: self.as_view(), editor: editor, delegate: delegate, start: start, length: length]
                }
            }
        }

        #[unsafe(method(endEditing:))]
        fn end_editing(&self, editor: &AnyObject) {
            if let Some(c) = self.the_cell() {
                // SAFETY: the cell's endEditing: takes the editor.
                unsafe { msg_send![&*c, endEditing: editor] }
            }
        }
    }

    unsafe impl NSObjectProtocol for NSControlImpl {}
);

impl NSControlImpl {
    fn the_cell(&self) -> Option<Retained<NSCell>> {
        self.ivars().cell.borrow().clone()
    }

    fn forward(&self, f: impl FnOnce(&NSCell)) {
        if let Some(c) = self.the_cell() {
            f(&c);
        }
    }

    /// The value kept without a cell, cloned out so that converting it
    /// holds no borrow.
    fn loose_value(&self) -> Value {
        self.ivars().loose.borrow().value.clone()
    }

    fn set_loose(&self, value: Value) {
        // Compared outside the borrow: comparing may message objects.
        if self.loose_value().same(&value) {
            return;
        }
        let old = std::mem::replace(&mut self.ivars().loose.borrow_mut().value, value);
        drop(old);
        self.as_view().setNeedsDisplay(true);
    }

    pub(crate) fn as_view(&self) -> &NSView {
        // SAFETY: NSControl is a subclass of NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    pub(crate) fn as_control(&self) -> &NSControl {
        // SAFETY: NSControlImpl is the class NSControl names.
        unsafe { &*(self as *const Self).cast::<NSControl>() }
    }
}

/// A control's intrinsic size, measured by `compute` once and remembered
/// until its cell changes (`updateCell:`) or it's told the size is stale
/// (`invalidateIntrinsicContentSize`).
pub(crate) fn cached_intrinsic(control: &NSControl, compute: impl FnOnce() -> NSSize) -> NSSize {
    let ivars = imp(control).ivars();
    if let Some(size) = ivars.intrinsic.get() {
        return size;
    }
    let size = compute();
    ivars.intrinsic.set(Some(size));
    size
}

/// Any control as the implementation class, which every control inherits
/// from.
pub(crate) fn imp(control: &NSControl) -> &NSControlImpl {
    // SAFETY: NSControl is NSControlImpl's class; subclasses share its
    // layout.
    unsafe { &*(control as *const NSControl).cast::<NSControlImpl>() }
}

/// `view` as a control, if it is one.
pub(crate) fn as_control(view: &NSView) -> Option<&NSControlImpl> {
    // SAFETY: NSControlImpl is the class NSControl names.
    unsafe { super::impl_of::<NSControl, NSControlImpl>(view) }
}

/// A cell of `control` changed what it shows: tell the control, as AppKit
/// does, through `updateCell:` (which subclasses may override).
pub(crate) fn cell_changed(control: &NSControlImpl, cell: &NSCell) {
    control.as_control().updateCell(cell);
}

/// Make `cell` the control's, and the control its cell's view.
fn install_cell(control: &NSControlImpl, cell: Option<&NSCell>) {
    let old = control.ivars().cell.replace(cell.map(|c| c.retain()));
    if let Some(old) = &old
        && cell.is_none_or(|c| !std::ptr::eq(&**old, c))
    {
        // SAFETY: clearing the cell's back link.
        unsafe { old.setControlView(None) };
    }
    if let Some(cell) = cell {
        // SAFETY: the control owns the cell; the link back is weak.
        unsafe { cell.setControlView(Some(control.as_view())) };
    }
    drop(old);
    control.ivars().intrinsic.set(None);
    control.as_view().setNeedsDisplay(true);
}

/// `-[NSControl sendAction:to:]`: through the application, which finds a
/// target along the responder chain when none is given. Nothing is sent
/// without an action.
pub(crate) fn send_action(control: &NSView, action: Option<Sel>, target: Option<&AnyObject>) -> bool {
    let Some(action) = action else { return false };
    let mtm = MainThreadMarker::from(control);
    let app = NSApplication::sharedApplication(mtm);
    let sender: &AnyObject = control;
    // SAFETY: the action is a selector taking the sender.
    unsafe { app.sendAction_to_from(action, target, Some(sender)) }
}

/// Where the point of `event` is in `view`.
pub(crate) fn event_point(view: &NSView, event: &NSEvent) -> NSPoint {
    view.convertPoint_fromView(event.locationInWindow(), None)
}
