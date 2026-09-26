//! `NSButton` and `NSButtonCell`: push buttons, check boxes, radio buttons
//! and the other bezels.
//!
//! A button cell's title is what a plain text cell would hold as its value;
//! its value is its state, so `stringValue` reads "0" or "1" and
//! `setIntValue:` sets the state. The button type sets how the cell
//! highlights and shows its state (`highlightsBy`, `showsStateBy`), and for
//! check boxes and radio buttons the look, as AppKit's `setButtonType:`
//! does. Radio buttons that share a superview and an action form a group:
//! turning one on turns the others off. Radio buttons without an action
//! are each on their own.
//!
//! Sizes and rects follow the bezel, the control size and the title,
//! measured on macOS for every bezel at every control size
//! (`conformance/tests/controls.rs`, `buttons_of_every_bezel`): a push
//! button is as tall as its control size says and adds that much again
//! round its title; flexible and circular buttons are their title plus an
//! inset all round; a check box is its box, a gap and its title. The title
//! is measured in the system font at the control size's size, unless the
//! program set a font; a button measures it once per change (the cell's
//! generation) and draws without making objects. A few bezels' frames
//! reach past their alignment rects, so their `cellSize` is larger than
//! their intrinsic size. Pixels are the theme's: a flat wash, the accent
//! for the default button (whose key equivalent is Return), red for a
//! destructive one, nothing at all for a transparent one.

// `NSGradientType`, mnemonics and the square bezel names are deprecated,
// but programs still set and read them.
#![allow(deprecated)]

use std::cell::{Cell, RefCell};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSActionCell, NSBezelStyle, NSButton, NSButtonCell, NSButtonType, NSCell, NSCellHitResult, NSCellImagePosition,
    NSCellStyleMask, NSColor, NSControl, NSControlBorderShape, NSEvent, NSEventModifierFlags, NSFont, NSGradientType,
    NSImageScaling, NSLineBreakMode, NSResponder, NSTextAlignment, NSView,
};
use objc2_foundation::{NSAttributedString, NSCopying, NSPoint, NSRect, NSSize, NSString, NSZone};

use super::cell::{self, Flags, NSCellImpl, Styled, imp as cell_imp};
use super::control;
use super::value::{self, Value};
use crate::theme::{self, metrics, parts};

/// The look a bezel style and button type come to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Look {
    Push,
    /// Flexible push and glass buttons: push buttons whose height follows
    /// their title.
    FlexiblePush,
    Circular,
    Help,
    Disclosure,
    PushDisclosure,
    SmallSquare,
    ShadowlessSquare,
    TexturedSquare,
    Toolbar,
    Badge,
    Check,
    Radio,
}

pub(crate) struct ButtonCellIvars {
    kind: Cell<NSButtonType>,
    bezel: Cell<NSBezelStyle>,
    highlights_by: Cell<NSCellStyleMask>,
    shows_state_by: Cell<NSCellStyleMask>,
    alternate_title: RefCell<Option<Retained<NSString>>>,
    alternate_image: RefCell<Option<Retained<AnyObject>>>,
    image_position: Cell<NSCellImagePosition>,
    image_scaling: Cell<NSImageScaling>,
    image_hugs_title: Cell<bool>,
    image_dims_when_disabled: Cell<bool>,
    key_equivalent: RefCell<Retained<NSString>>,
    key_mask: Cell<NSEventModifierFlags>,
    transparent: Cell<bool>,
    border_only_inside: Cell<bool>,
    bezel_color: RefCell<Option<Retained<NSColor>>>,
    content_tint: RefCell<Option<Retained<NSColor>>>,
    background: RefCell<Option<Retained<NSColor>>>,
    periodic: Cell<(f32, f32)>,
    destructive: Cell<bool>,
    sound: RefCell<Option<Retained<AnyObject>>>,
    gradient: Cell<NSGradientType>,
    key_font: RefCell<Option<Retained<NSFont>>>,
    alternate_mnemonic: Cell<usize>,
    /// The title's measured size and the cell generation it was measured
    /// at (see `cell::generation`).
    title_size: Cell<Option<(u32, NSSize)>>,
}

impl ButtonCellIvars {
    fn new() -> Self {
        ButtonCellIvars {
            kind: Cell::new(NSButtonType::MomentaryPushIn),
            bezel: Cell::new(NSBezelStyle::Automatic),
            highlights_by: Cell::new(NSCellStyleMask(14)),
            shows_state_by: Cell::new(NSCellStyleMask(0)),
            alternate_title: RefCell::new(None),
            alternate_image: RefCell::new(None),
            image_position: Cell::new(NSCellImagePosition::NoImage),
            image_scaling: Cell::new(NSImageScaling::ScaleProportionallyDown),
            image_hugs_title: Cell::new(false),
            image_dims_when_disabled: Cell::new(true),
            key_equivalent: RefCell::new(NSString::new()),
            key_mask: Cell::new(NSEventModifierFlags::empty()),
            transparent: Cell::new(false),
            border_only_inside: Cell::new(false),
            bezel_color: RefCell::new(None),
            content_tint: RefCell::new(None),
            background: RefCell::new(None),
            periodic: Cell::new(super::track::PERIODIC),
            destructive: Cell::new(false),
            sound: RefCell::new(None),
            gradient: Cell::new(NSGradientType::None),
            key_font: RefCell::new(None),
            alternate_mnemonic: Cell::new(usize::MAX),
            title_size: Cell::new(None),
        }
    }

    /// What `copyWithZone:` copies.
    fn copy_from(&self, other: &ButtonCellIvars) {
        self.kind.set(other.kind.get());
        self.bezel.set(other.bezel.get());
        self.highlights_by.set(other.highlights_by.get());
        self.shows_state_by.set(other.shows_state_by.get());
        self.alternate_title.replace(other.alternate_title.borrow().clone());
        self.alternate_image.replace(other.alternate_image.borrow().clone());
        self.image_position.set(other.image_position.get());
        self.image_scaling.set(other.image_scaling.get());
        self.image_hugs_title.set(other.image_hugs_title.get());
        self.image_dims_when_disabled.set(other.image_dims_when_disabled.get());
        self.key_equivalent.replace(other.key_equivalent.borrow().clone());
        self.key_mask.set(other.key_mask.get());
        self.transparent.set(other.transparent.get());
        self.border_only_inside.set(other.border_only_inside.get());
        self.bezel_color.replace(other.bezel_color.borrow().clone());
        self.content_tint.replace(other.content_tint.borrow().clone());
        self.background.replace(other.background.borrow().clone());
        self.periodic.set(other.periodic.get());
        self.destructive.set(other.destructive.get());
        self.sound.replace(other.sound.borrow().clone());
        self.gradient.set(other.gradient.get());
        self.key_font.replace(other.key_font.borrow().clone());
        self.alternate_mnemonic.set(other.alternate_mnemonic.get());
    }
}

define_class!(
    #[unsafe(super(NSActionCell, NSCell, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSButtonCell"]
    #[ivars = ButtonCellIvars]
    pub(crate) struct NSButtonCellImpl;

    impl NSButtonCellImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            // SAFETY: the designated initializer.
            unsafe { msg_send![this, initTextCell: &*NSString::from_str("Button")] }
        }

        #[unsafe(method_id(initTextCell:))]
        fn init_text_cell(this: Allocated<Self>, title: &NSString) -> Retained<Self> {
            let this = this.set_ivars(ButtonCellIvars::new());
            // SAFETY: NSActionCell's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initTextCell: title] };
            let c = this.base();
            c.set_flag(Flags::BORDERED, true);
            cell::as_cell(c).setAlignment(NSTextAlignment::Center);
            this
        }

        #[unsafe(method_id(initImageCell:))]
        fn init_image_cell(this: Allocated<Self>, image: Option<&AnyObject>) -> Retained<Self> {
            let this = this.set_ivars(ButtonCellIvars::new());
            // SAFETY: NSActionCell's initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), initTextCell: &*NSString::new()] };
            cell::as_cell(this.base()).setAlignment(NSTextAlignment::Center);
            this.base().set_flag(Flags::BORDERED, true);
            // SAFETY: setImage: takes an image or nil.
            let _: () = unsafe { msg_send![&*this, setImage: image] };
            this
        }

        // The title is the cell's text; the value is the state.

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            cell::text_of(self.base())
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: Option<&NSString>) {
            let title = title.map_or_else(NSString::new, |t| t.copy());
            set_title_value(self, Value::String(title));
        }

        #[unsafe(method_id(attributedTitle))]
        fn attributed_title(&self) -> Retained<NSAttributedString> {
            attributed_title(self)
        }

        #[unsafe(method(setAttributedTitle:))]
        fn set_attributed_title(&self, title: &NSAttributedString) {
            set_title_value(self, Value::from_object(Some(title)));
        }

        #[unsafe(method_id(alternateTitle))]
        fn alternate_title(&self) -> Retained<NSString> {
            self.ivars().alternate_title.borrow().clone().unwrap_or_default()
        }

        #[unsafe(method(setAlternateTitle:))]
        fn set_alternate_title(&self, title: &NSString) {
            set_alternate_title(self, title);
        }

        #[unsafe(method_id(attributedAlternateTitle))]
        fn attributed_alternate_title(&self) -> Retained<NSAttributedString> {
            value::attributed(&self.ivars().alternate_title.borrow().clone().unwrap_or_default())
        }

        #[unsafe(method(setAttributedAlternateTitle:))]
        fn set_attributed_alternate_title(&self, title: &NSAttributedString) {
            set_alternate_title(self, &title.string());
        }

        #[unsafe(method_id(objectValue))]
        fn object_value(&self) -> Option<Retained<AnyObject>> {
            Value::Int(self.base().raw_state().get() as i64).object()
        }

        #[unsafe(method(setObjectValue:))]
        fn set_object_value(&self, object: Option<&AnyObject>) {
            let state = Value::from_object(object).integer();
            self.as_cell().setState(state);
        }

        #[unsafe(method_id(stringValue))]
        fn string_value(&self) -> Retained<NSString> {
            NSString::from_str(&self.base().raw_state().get().to_string())
        }

        #[unsafe(method(setStringValue:))]
        fn set_string_value(&self, string: &NSString) {
            self.as_cell().setState(string.integerValue());
        }

        #[unsafe(method(intValue))]
        fn int_value(&self) -> i32 {
            self.base().raw_state().get() as i32
        }

        #[unsafe(method(setIntValue:))]
        fn set_int_value(&self, value: i32) {
            self.as_cell().setState(value as isize);
        }

        #[unsafe(method(integerValue))]
        fn integer_value(&self) -> isize {
            self.base().raw_state().get()
        }

        #[unsafe(method(setIntegerValue:))]
        fn set_integer_value(&self, value: isize) {
            self.as_cell().setState(value);
        }

        #[unsafe(method(floatValue))]
        fn float_value(&self) -> f32 {
            self.base().raw_state().get() as f32
        }

        #[unsafe(method(setFloatValue:))]
        fn set_float_value(&self, value: f32) {
            self.as_cell().setState(value as isize);
        }

        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            self.base().raw_state().get() as f64
        }

        #[unsafe(method(setDoubleValue:))]
        fn set_double_value(&self, value: f64) {
            self.as_cell().setState(value as isize);
        }

        #[unsafe(method(setState:))]
        fn set_state(&self, state: isize) {
            // SAFETY: NSCell's setState:.
            let _: () = unsafe { msg_send![super(self), setState: state] };
            if self.base().raw_state().get() == 1 && self.ivars().kind.get() == NSButtonType::Radio {
                turn_off_group(self);
            }
        }

        #[unsafe(method(nextState))]
        fn next_state(&self) -> isize {
            // A radio button stays on when clicked again.
            let state = self.base().raw_state().get();
            if self.ivars().kind.get() == NSButtonType::Radio && state == 1 {
                return 1;
            }
            cell::next_state(state, self.base().has(Flags::ALLOWS_MIXED))
        }

        // Types and bezels.

        #[unsafe(method(setButtonType:))]
        fn set_button_type(&self, kind: NSButtonType) {
            set_button_type(self, kind);
        }

        #[unsafe(method(bezelStyle))]
        fn bezel_style(&self) -> NSBezelStyle {
            self.ivars().bezel.get()
        }

        #[unsafe(method(setBezelStyle:))]
        fn set_bezel_style(&self, style: NSBezelStyle) {
            if self.ivars().bezel.replace(style) != style {
                cell::changed(self.base());
            }
        }

        #[unsafe(method(highlightsBy))]
        fn highlights_by(&self) -> NSCellStyleMask {
            self.ivars().highlights_by.get()
        }

        #[unsafe(method(setHighlightsBy:))]
        fn set_highlights_by(&self, mask: NSCellStyleMask) {
            self.ivars().highlights_by.set(mask);
        }

        #[unsafe(method(showsStateBy))]
        fn shows_state_by(&self) -> NSCellStyleMask {
            self.ivars().shows_state_by.get()
        }

        #[unsafe(method(setShowsStateBy:))]
        fn set_shows_state_by(&self, mask: NSCellStyleMask) {
            self.ivars().shows_state_by.set(mask);
        }

        #[unsafe(method(isTransparent))]
        fn is_transparent(&self) -> bool {
            self.ivars().transparent.get()
        }

        #[unsafe(method(setTransparent:))]
        fn set_transparent(&self, flag: bool) {
            if self.ivars().transparent.replace(flag) != flag {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            false
        }

        #[unsafe(method(showsBorderOnlyWhileMouseInside))]
        fn shows_border_only_while_mouse_inside(&self) -> bool {
            self.ivars().border_only_inside.get()
        }

        #[unsafe(method(setShowsBorderOnlyWhileMouseInside:))]
        fn set_shows_border_only_while_mouse_inside(&self, flag: bool) {
            if self.ivars().border_only_inside.replace(flag) != flag {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method_id(backgroundColor))]
        fn background_color(&self) -> Option<Retained<NSColor>> {
            self.ivars().background.borrow().clone()
        }

        #[unsafe(method(setBackgroundColor:))]
        fn set_background_color(&self, color: Option<&NSColor>) {
            set_color(self.base(), &self.ivars().background, color);
        }

        #[unsafe(method(gradientType))]
        fn gradient_type(&self) -> NSGradientType {
            self.ivars().gradient.get()
        }

        #[unsafe(method(setGradientType:))]
        fn set_gradient_type(&self, kind: NSGradientType) {
            self.ivars().gradient.set(kind);
        }

        // Images.

        #[unsafe(method_id(alternateImage))]
        fn alternate_image(&self) -> Option<Retained<AnyObject>> {
            self.ivars().alternate_image.borrow().clone()
        }

        #[unsafe(method(setAlternateImage:))]
        fn set_alternate_image(&self, image: Option<&AnyObject>) {
            let old = self.ivars().alternate_image.replace(image.map(|i| i.retain()));
            let same = match (&old, image) {
                (Some(a), Some(b)) => std::ptr::eq(&**a, b),
                (None, None) => true,
                _ => false,
            };
            drop(old);
            if !same {
                cell::redraw(self.base());
            }
        }

        #[unsafe(method(imagePosition))]
        fn image_position(&self) -> NSCellImagePosition {
            self.ivars().image_position.get()
        }

        #[unsafe(method(setImagePosition:))]
        fn set_image_position(&self, position: NSCellImagePosition) {
            if self.ivars().image_position.replace(position) != position {
                cell::changed(self.base());
            }
        }

        #[unsafe(method(imageScaling))]
        fn image_scaling(&self) -> NSImageScaling {
            self.ivars().image_scaling.get()
        }

        #[unsafe(method(setImageScaling:))]
        fn set_image_scaling(&self, scaling: NSImageScaling) {
            if self.ivars().image_scaling.replace(scaling) != scaling {
                cell::changed(self.base());
            }
        }

        #[unsafe(method(imageDimsWhenDisabled))]
        fn image_dims_when_disabled(&self) -> bool {
            self.ivars().image_dims_when_disabled.get()
        }

        #[unsafe(method(setImageDimsWhenDisabled:))]
        fn set_image_dims_when_disabled(&self, flag: bool) {
            self.ivars().image_dims_when_disabled.set(flag);
        }

        // Keys.

        #[unsafe(method_id(keyEquivalent))]
        fn key_equivalent(&self) -> Retained<NSString> {
            self.ivars().key_equivalent.borrow().clone()
        }

        #[unsafe(method(setKeyEquivalent:))]
        fn set_key_equivalent(&self, key: &NSString) {
            let old = self.ivars().key_equivalent.borrow().clone();
            if old.isEqualToString(key) {
                return;
            }
            self.ivars().key_equivalent.replace(key.copy());
            // Return makes the default button, which draws differently.
            cell::redraw(self.base());
        }

        #[unsafe(method(keyEquivalentModifierMask))]
        fn key_equivalent_modifier_mask(&self) -> NSEventModifierFlags {
            self.ivars().key_mask.get()
        }

        #[unsafe(method(setKeyEquivalentModifierMask:))]
        fn set_key_equivalent_modifier_mask(&self, mask: NSEventModifierFlags) {
            self.ivars().key_mask.set(mask);
        }

        #[unsafe(method_id(keyEquivalentFont))]
        fn key_equivalent_font(&self) -> Option<Retained<NSFont>> {
            self.ivars().key_font.borrow().clone()
        }

        #[unsafe(method(setKeyEquivalentFont:))]
        fn set_key_equivalent_font(&self, font: Option<&NSFont>) {
            self.ivars().key_font.replace(font.map(|f| f.retain()));
        }

        #[unsafe(method(setKeyEquivalentFont:size:))]
        fn set_key_equivalent_font_size(&self, name: &NSString, size: f64) {
            self.ivars().key_font.replace(NSFont::fontWithName_size(name, size));
        }

        #[unsafe(method(setAlternateTitleWithMnemonic:))]
        fn set_alternate_title_with_mnemonic(&self, title: Option<&NSString>) {
            let text = title.map(|t| t.to_string().replacen('&', "", 1)).unwrap_or_default();
            set_alternate_title(self, &NSString::from_str(&text));
        }

        #[unsafe(method(alternateMnemonicLocation))]
        fn alternate_mnemonic_location(&self) -> usize {
            self.ivars().alternate_mnemonic.get()
        }

        #[unsafe(method(setAlternateMnemonicLocation:))]
        fn set_alternate_mnemonic_location(&self, location: usize) {
            self.ivars().alternate_mnemonic.set(location);
        }

        #[unsafe(method_id(alternateMnemonic))]
        fn alternate_mnemonic(&self) -> Option<Retained<NSString>> {
            None
        }

        // Timing and sound.

        #[unsafe(method(setPeriodicDelay:interval:))]
        fn set_periodic_delay(&self, delay: f32, interval: f32) {
            self.ivars().periodic.set((delay, interval));
        }

        #[unsafe(method(getPeriodicDelay:interval:))]
        fn get_periodic_delay(&self, delay: *mut f32, interval: *mut f32) {
            let (d, i) = self.ivars().periodic.get();
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

        #[unsafe(method_id(sound))]
        fn sound(&self) -> Option<Retained<AnyObject>> {
            self.ivars().sound.borrow().clone()
        }

        #[unsafe(method(setSound:))]
        fn set_sound(&self, sound: Option<&AnyObject>) {
            self.ivars().sound.replace(sound.map(|s| s.retain()));
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {}

        // Geometry.

        #[unsafe(method(cellSize))]
        fn cell_size(&self) -> NSSize {
            cell_size(self)
        }

        #[unsafe(method(cellSizeForBounds:))]
        fn cell_size_for_bounds(&self, bounds: NSRect) -> NSSize {
            cell_size_for_bounds(self, bounds)
        }

        #[unsafe(method(titleRectForBounds:))]
        fn title_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            title_rect(self, bounds)
        }

        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            drawing_rect(self, bounds)
        }

        #[unsafe(method(imageRectForBounds:))]
        fn image_rect_for_bounds(&self, bounds: NSRect) -> NSRect {
            image_rect(self, bounds)
        }

        // Drawing.

        #[unsafe(method(drawWithFrame:inView:))]
        fn draw_with_frame(&self, frame: NSRect, view: &NSView) {
            if self.ivars().transparent.get() || !theme::paint::recording() {
                return;
            }
            // SAFETY: drawBezelWithFrame:inView: takes a rect and a view.
            let _: () = unsafe { msg_send![self, drawBezelWithFrame: frame, inView: view] };
            // SAFETY: drawInteriorWithFrame:inView: takes a rect and a view.
            let _: () = unsafe { msg_send![self, drawInteriorWithFrame: frame, inView: view] };
        }

        #[unsafe(method(drawBezelWithFrame:inView:))]
        fn draw_bezel_with_frame(&self, frame: NSRect, view: &NSView) {
            draw_bezel(self, frame, view);
        }

        #[unsafe(method(drawInteriorWithFrame:inView:))]
        fn draw_interior_with_frame(&self, frame: NSRect, view: &NSView) {
            if title_size(self).width == 0.0 {
                return;
            }
            // SAFETY: titleRectForBounds: takes and returns a rect.
            let title: NSRect = unsafe { msg_send![self, titleRectForBounds: frame] };
            if overrides_draw_title(self) {
                // A subclass draws the title: give it the attributed string
                // it expects.
                let attributed = attributed_title(self);
                // SAFETY: drawTitle:withFrame:inView: takes a string, a rect
                // and a view.
                let _: NSRect = unsafe { msg_send![self, drawTitle: &*attributed, withFrame: title, inView: view] };
            } else {
                draw_title(self, None, title);
            }
        }

        #[unsafe(method(drawTitle:withFrame:inView:))]
        fn draw_title(&self, title: &NSAttributedString, frame: NSRect, _view: &NSView) -> NSRect {
            draw_title(self, Some(title), frame);
            frame
        }

        #[unsafe(method(drawImage:withFrame:inView:))]
        fn draw_image(&self, _image: &AnyObject, _frame: NSRect, _view: &NSView) {
            // Images draw once the image work lands.
        }

        #[unsafe(method(performClick:))]
        fn perform_click(&self, _sender: Option<&AnyObject>) {
            super::track::perform_click(self.as_cell());
        }

        #[unsafe(method(hitTestForEvent:inRect:ofView:))]
        fn hit_test_for_event(&self, event: &NSEvent, frame: NSRect, view: &NSView) -> NSCellHitResult {
            hit_test(self, event, frame, view)
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, zone: *mut NSZone) -> Retained<NSCell> {
            // SAFETY: NSActionCell's copyWithZone: returns a cell of this
            // class.
            let copy: Retained<NSCell> = unsafe { msg_send![super(self), copyWithZone: zone] };
            imp_of_cell(&copy).ivars().copy_from(self.ivars());
            copy
        }
    }

    unsafe impl NSObjectProtocol for NSButtonCellImpl {}
);

impl NSButtonCellImpl {
    fn base(&self) -> &NSCellImpl {
        cell_imp(self.as_cell())
    }

    fn as_cell(&self) -> &NSCell {
        // SAFETY: NSButtonCell is a subclass of NSCell.
        unsafe { &*(self as *const Self).cast::<NSCell>() }
    }

    /// The look the bezel style and type come to.
    pub(crate) fn look(&self) -> Look {
        match self.ivars().kind.get() {
            NSButtonType::Switch => return Look::Check,
            NSButtonType::Radio => return Look::Radio,
            _ => {}
        }
        match self.ivars().bezel.get() {
            NSBezelStyle::FlexiblePush | NSBezelStyle::Glass => Look::FlexiblePush,
            NSBezelStyle::Circular => Look::Circular,
            NSBezelStyle::HelpButton => Look::Help,
            NSBezelStyle::Disclosure => Look::Disclosure,
            NSBezelStyle::PushDisclosure => Look::PushDisclosure,
            NSBezelStyle::SmallSquare => Look::SmallSquare,
            NSBezelStyle::ShadowlessSquare => Look::ShadowlessSquare,
            NSBezelStyle::TexturedSquare => Look::TexturedSquare,
            NSBezelStyle::Toolbar => Look::Toolbar,
            NSBezelStyle::Badge => Look::Badge,
            _ => Look::Push,
        }
    }

    /// The default button: Return is its key equivalent.
    pub(crate) fn is_default(&self) -> bool {
        let key = self.ivars().key_equivalent.borrow().clone();
        key.length() == 1 && key.characterAtIndex(0) == u16::from(b'\r')
    }
}

/// `attributedTitle`: an attributed title as it is; a plain one with the
/// cell's font, alignment and line breaking and the text color, as macOS
/// gives it (`conformance/tests/controls.rs`, `attributed_values`).
fn attributed_title(cell: &NSButtonCellImpl) -> Retained<NSAttributedString> {
    let base = cell.base();
    if let Value::Attributed(a) = base.value() {
        return a;
    }
    let color =
        if matches!(cell.look(), Look::Check | Look::Radio) { sel!(labelColor) } else { sel!(controlTextColor) };
    let c = cell::as_cell(base);
    cell::attributed(
        &cell::text_of(base),
        &title_font(cell),
        c.alignment(),
        c.lineBreakMode(),
        &cell::text_color(color),
    )
}

/// A new title, unless it's the one there.
fn set_title_value(cell: &NSButtonCellImpl, title: Value) {
    let base = cell.base();
    if base.value().same(&title) {
        return;
    }
    base.replace_value(title);
    cell::changed(base);
}

fn set_alternate_title(cell: &NSButtonCellImpl, title: &NSString) {
    let old = cell.ivars().alternate_title.replace(Some(title.copy()));
    if old.is_none_or(|old| !old.isEqualToString(title)) {
        cell::redraw(cell.base());
    }
}

/// Store a color setting, redrawing if it changed.
fn set_color(base: &NSCellImpl, slot: &RefCell<Option<Retained<NSColor>>>, color: Option<&NSColor>) {
    let old = slot.replace(color.map(|c| c.retain()));
    if !same_color(old.as_deref(), color) {
        cell::redraw(base);
    }
}

/// Whether two colors are one: the same object, or the same components.
pub(crate) fn same_color(a: Option<&NSColor>, b: Option<&NSColor>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => std::ptr::eq(a, b) || theme::color_of(a) == theme::color_of(b),
        _ => false,
    }
}

/// Whether the receiver's class draws titles its own way: then the title
/// goes to it as an attributed string, which is otherwise never made.
fn overrides_draw_title(cell: &NSButtonCellImpl) -> bool {
    let sel = sel!(drawTitle:withFrame:inView:);
    let ours = <NSButtonCellImpl as objc2::ClassType>::class().instance_method(sel).map(|m| m.implementation());
    let theirs = cell.class().instance_method(sel).map(|m| m.implementation());
    ours.map(|f| f as usize) != theirs.map(|f| f as usize)
}

/// A cell of this class (or a subclass) as the implementation.
fn imp_of_cell(cell: &NSCell) -> &NSButtonCellImpl {
    // SAFETY: only called on copies of button cells, which are instances
    // of the receiver's class.
    unsafe { &*(cell as *const NSCell).cast::<NSButtonCellImpl>() }
}

/// Any button cell as the implementation.
pub(crate) fn imp(cell: &NSButtonCell) -> &NSButtonCellImpl {
    // SAFETY: NSButtonCell is NSButtonCellImpl's class; subclasses share
    // its layout.
    unsafe { &*(cell as *const NSButtonCell).cast::<NSButtonCellImpl>() }
}

/// A cell as a button cell, if it is one.
pub(crate) fn as_button_cell(cell: &NSCell) -> Option<&NSButtonCellImpl> {
    // SAFETY: NSButtonCellImpl is the class NSButtonCell names.
    unsafe { super::impl_of::<NSButtonCell, NSButtonCellImpl>(cell) }
}

/// What `setButtonType:` sets, as macOS reports it back
/// (`controls.rs`, `button_types`).
fn set_button_type(cell: &NSButtonCellImpl, kind: NSButtonType) {
    let (highlights, shows) = match kind {
        NSButtonType::MomentaryLight | NSButtonType::Accelerator | NSButtonType::MultiLevelAccelerator => (12, 0),
        NSButtonType::PushOnPushOff => (14, 12),
        NSButtonType::Toggle => (3, 1),
        NSButtonType::Switch | NSButtonType::Radio => (1, 1),
        NSButtonType::MomentaryChange => (1, 0),
        NSButtonType::OnOff => (12, 12),
        _ => (14, 0),
    };
    cell.ivars().kind.set(kind);
    cell.ivars().highlights_by.set(NSCellStyleMask(highlights));
    cell.ivars().shows_state_by.set(NSCellStyleMask(shows));
    let base = cell.base();
    if matches!(kind, NSButtonType::Switch | NSButtonType::Radio) {
        base.set_flag(Flags::BORDERED, false);
        cell.ivars().image_position.set(NSCellImagePosition::ImageLeading);
        cell::as_cell(base).setAlignment(NSTextAlignment::Natural);
    }
    cell::changed(base);
}

/// Turn off the other radio buttons of `cell`'s group: buttons with radio
/// cells in the same superview, with the same action. Radio buttons
/// without an action belong to no group (`control_events.rs`,
/// `radio_buttons_are_exclusive`).
fn turn_off_group(cell: &NSButtonCellImpl) {
    let Some(action) = cell.as_cell().action() else { return };
    let Some(view) = cell.base().view() else { return };
    // SAFETY: superview takes nothing and returns a view or nil.
    let Some(superview) = (unsafe { view.superview() }) else { return };
    for sibling in crate::views::subviews(crate::views::imp(&superview)) {
        if std::ptr::eq(&*sibling, &*view) {
            continue;
        }
        let Some(control) = control::as_control(&sibling) else { continue };
        let Some(other) = control.as_control().cell() else { continue };
        let Some(other_button) = as_button_cell(&other) else { continue };
        if other_button.ivars().kind.get() == NSButtonType::Radio
            && other.action() == Some(action)
            && other_button.base().raw_state().get() != 0
        {
            other.setState(0);
        }
    }
}

/// The font a button's title is measured and drawn in: the one set, else
/// the system font at the control size's size (11 points for badges).
fn title_font(cell: &NSButtonCellImpl) -> Retained<NSFont> {
    let base = cell.base();
    base.font_set().unwrap_or_else(|| {
        let size = if cell.look() == Look::Badge { 11.0 } else { metrics::FONT_SIZE[base.control_size_index()] };
        NSFont::systemFontOfSize(size)
    })
}

/// The title's size, in its font, whole points wide: measured once per
/// change to the cell (its generation), since sizing and drawing ask for it
/// several times.
fn title_size(cell: &NSButtonCellImpl) -> NSSize {
    let base = cell.base();
    let generation = cell::generation(base);
    if let Some((at, size)) = cell.ivars().title_size.get()
        && at == generation
    {
        return size;
    }
    let size = if base.value().is_empty_text() {
        NSSize::ZERO
    } else {
        let font = title_font(cell);
        let attrs = cell::attrs(base, &font, [0.0; 4]);
        let size = cell::styled_with(base, attrs).size(None);
        NSSize::new(size.width.ceil(), size.height)
    };
    cell.ivars().title_size.set(Some((generation, size)));
    size
}

/// The line height of the title font, for titles that are empty.
fn line_height(cell: &NSButtonCellImpl) -> f64 {
    let font = title_font(cell);
    let attrs = cell::attrs(cell.base(), &font, [0.0; 4]);
    Styled::plain(String::new(), attrs).size(None).height
}

/// Half-way values round up, as AppKit rounds positions.
fn half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

/// The intrinsic size, by look and control size (`controls.rs`,
/// `push_buttons`, `check_boxes`, `buttons_of_every_bezel`).
fn intrinsic_size(cell: &NSButtonCellImpl) -> NSSize {
    let base = cell.base();
    let i = base.control_size_index();
    let t = title_size(cell);
    let empty = t.width == 0.0;
    let bordered = base.has(Flags::BORDERED);
    match cell.look() {
        Look::Check | Look::Radio => {
            let side = metrics::CHECK_BOX[i];
            if empty {
                return NSSize::new(side, side);
            }
            NSSize::new(side + metrics::CHECK_GAP[i] + t.width, side.max(t.height))
        }
        Look::Help | Look::PushDisclosure => NSSize::new(metrics::HELP[i], metrics::HELP[i]),
        Look::Disclosure => NSSize::new(metrics::DISCLOSURE, metrics::DISCLOSURE),
        _ if !bordered => NSSize::new(if empty { 0.0 } else { t.width + 4.0 }, line_height(cell).max(t.height)),
        Look::Push => {
            let h = metrics::PUSH_HEIGHT[i];
            NSSize::new(if empty { metrics::PUSH_EMPTY_CONTENT } else { t.width } + h, h)
        }
        Look::FlexiblePush => {
            let width = if empty { metrics::PUSH_EMPTY_CONTENT } else { t.width } + metrics::PUSH_HEIGHT[i];
            let height = if empty { metrics::ROUND_EMPTY[i] } else { t.height + 2.0 * metrics::ROUND_INSET[i] };
            NSSize::new(width, height)
        }
        Look::Circular => {
            if empty {
                return NSSize::new(metrics::ROUND_EMPTY[i], metrics::ROUND_EMPTY[i]);
            }
            let d = 2.0 * metrics::ROUND_INSET[i];
            NSSize::new(t.width + d, t.height + d)
        }
        Look::SmallSquare => {
            if empty {
                NSSize::new(2.0, 4.0)
            } else {
                NSSize::new(t.width + 6.0, t.height + 4.0)
            }
        }
        Look::ShadowlessSquare => {
            if empty {
                NSSize::new(6.0, 6.0)
            } else {
                NSSize::new(t.width + 10.0, t.height + 6.0)
            }
        }
        Look::TexturedSquare => {
            let pad = metrics::TEXTURED_PAD[i];
            NSSize::new(if empty { pad - 4.0 } else { t.width + pad }, metrics::TEXTURED_HEIGHT[i])
        }
        Look::Toolbar => {
            let pad = metrics::TOOLBAR_PAD[i];
            NSSize::new(if empty { pad - 4.0 } else { t.width + pad }, metrics::TOOLBAR_HEIGHT[i])
        }
        Look::Badge => {
            if empty {
                NSSize::new(18.0, 14.0)
            } else {
                NSSize::new(t.width + 10.0, 18.0)
            }
        }
    }
}

/// `cellSize`: the intrinsic size, and for the bezels whose frames reach
/// past their alignment rects, that much more.
fn cell_size(cell: &NSButtonCellImpl) -> NSSize {
    let size = intrinsic_size(cell);
    let (dw, dh) = match cell.look() {
        _ if !cell.base().has(Flags::BORDERED) => (0.0, 0.0),
        Look::TexturedSquare => metrics::TEXTURED_FRAME,
        Look::Toolbar => metrics::TOOLBAR_FRAME,
        Look::SmallSquare => metrics::SMALL_SQUARE_FRAME,
        _ => (0.0, 0.0),
    };
    NSSize::new(size.width + dw, size.height + dh)
}

/// `cellSizeForBounds:`: a push-like button gives up width it isn't
/// given, keeping its height (`controls.rs`, `sizes_that_fit`). Check
/// boxes and radio buttons answer their whole size.
fn cell_size_for_bounds(cell: &NSButtonCellImpl, bounds: NSRect) -> NSSize {
    let size = cell_size(cell);
    match cell.look() {
        Look::Check | Look::Radio => size,
        _ => NSSize::new(size.width.min(bounds.size.width.max(0.0)), size.height),
    }
}

/// `drawingRectForBounds:`: where the bezel goes, by look.
fn drawing_rect(cell: &NSButtonCellImpl, b: NSRect) -> NSRect {
    let base = cell.base();
    let i = base.control_size_index();
    let (x, y, w, h) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
    let inset = |dx: f64, dy: f64| NSRect::new(NSPoint::new(x + dx, y + dy), NSSize::new(w - 2.0 * dx, h - 2.0 * dy));
    let look = cell.look();
    if !base.has(Flags::BORDERED)
        && !matches!(look, Look::Disclosure | Look::Help | Look::PushDisclosure | Look::Check | Look::Radio)
    {
        return b;
    }
    // The ends of push-like bezels: half their height, or what the title
    // leaves.
    let push_pad = || {
        let t = title_size(cell).width;
        (metrics::PUSH_HEIGHT[i] / 2.0).min(((w - t) / 2.0).floor().max(0.0))
    };
    match look {
        Look::Push => {
            let height = metrics::PUSH_HEIGHT[i];
            let pad = push_pad();
            // A titled mini button's bezel sits a point lower.
            let lower = if i == 2 && title_size(cell).width > 0.0 { 1.0 } else { 0.0 };
            NSRect::new(
                NSPoint::new(x + pad, y + ((h - height) / 2.0).floor() + lower),
                NSSize::new(w - 2.0 * pad, height),
            )
        }
        Look::FlexiblePush => inset(push_pad(), metrics::ROUND_INSET[i]),
        Look::Circular => inset(metrics::ROUND_INSET[i], metrics::ROUND_INSET[i]),
        Look::SmallSquare => inset(1.0, 3.0),
        Look::ShadowlessSquare => inset(3.0, 3.0),
        Look::TexturedSquare => inset(metrics::TEXTURED_PAD[i] / 2.0, 2.0),
        Look::Toolbar => {
            let height = metrics::TOOLBAR_HEIGHT[i];
            NSRect::new(NSPoint::new(x + 3.0, y + (h - height) / 2.0 - 0.5), NSSize::new(w - 6.0, height))
        }
        Look::Badge => inset(4.0, 2.0),
        Look::Disclosure => {
            let side = metrics::DISCLOSURE;
            NSRect::new(NSPoint::new(x + (w - side) / 2.0, y + (h - side) / 2.0), NSSize::new(side, side))
        }
        Look::Help | Look::PushDisclosure => NSRect::ZERO,
        // The bounds, at least the box tall.
        Look::Check | Look::Radio => NSRect::new(b.origin, NSSize::new(w, h.max(metrics::CHECK_BOX[i]))),
    }
}

/// `titleRectForBounds:`: by look; nothing for an empty title.
fn title_rect(cell: &NSButtonCellImpl, b: NSRect) -> NSRect {
    let t = title_size(cell);
    if t.width == 0.0 {
        return NSRect::ZERO;
    }
    let base = cell.base();
    let i = base.control_size_index();
    let (x, y, w, h) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
    // Centered across, a band `dx` in from each side.
    let band = |dx: f64, top: f64| NSRect::new(NSPoint::new(x + dx, y + top), NSSize::new(w - 2.0 * dx, t.height));
    let middle = (h - t.height) / 2.0;
    match cell.look() {
        Look::Check | Look::Radio => {
            let left = metrics::CHECK_BOX[i] + metrics::CHECK_GAP[i];
            NSRect::new(NSPoint::new(x + left, y + half_up(middle)), t)
        }
        Look::Help | Look::PushDisclosure => NSRect::ZERO,
        Look::Disclosure => {
            let side = metrics::DISCLOSURE;
            NSRect::new(NSPoint::new(x + (w - side) / 2.0, y + middle), NSSize::new(side, t.height))
        }
        _ if !base.has(Flags::BORDERED) => band(0.0, half_up(middle)),
        Look::SmallSquare => band(1.0, middle),
        Look::ShadowlessSquare => band(3.0, middle),
        Look::TexturedSquare => band(metrics::TEXTURED_PAD[i] / 2.0, middle),
        Look::Toolbar => band(metrics::TOOLBAR_PAD[i] / 2.0 - 1.0, middle - 1.5),
        Look::Badge => {
            let width = t.width + 2.0;
            NSRect::new(NSPoint::new(x + half_up((w - width) / 2.0), y + half_up(middle)), NSSize::new(width, t.height))
        }
        Look::Push | Look::FlexiblePush | Look::Circular => {
            NSRect::new(NSPoint::new(x + half_up((w - t.width) / 2.0), y + half_up(middle)), t)
        }
    }
}

/// `imageRectForBounds:`: a check box's or radio button's box, at the
/// leading edge and centered across (a mini box a point lower).
fn image_rect(cell: &NSButtonCellImpl, bounds: NSRect) -> NSRect {
    match cell.look() {
        Look::Check | Look::Radio => {
            let i = cell.base().control_size_index();
            let side = metrics::CHECK_BOX[i];
            let y = bounds.origin.y + half_up((bounds.size.height - metrics::CHECK_CENTER[i]) / 2.0);
            NSRect::new(NSPoint::new(bounds.origin.x, y), NSSize::new(side, side))
        }
        _ => NSRect::ZERO,
    }
}

/// `hitTestForEvent:inRect:ofView:`: a push-like button takes the click
/// anywhere in its frame, a check box or radio button over its box and
/// title, enabled or not (`controls.rs`, `hit_tests`).
fn hit_test(cell: &NSButtonCellImpl, event: &NSEvent, frame: NSRect, view: &NSView) -> NSCellHitResult {
    let at = view.convertPoint_fromView(event.locationInWindow(), None);
    let flipped = view.isFlipped();
    let inside = |r: NSRect| super::track::mouse_in_rect(at, r, flipped);
    let hit = match cell.look() {
        Look::Check | Look::Radio => {
            let c = cell.as_cell();
            inside(c.imageRectForBounds(frame)) || inside(c.titleRectForBounds(frame))
        }
        _ => inside(frame),
    };
    if hit {
        NSCellHitResult(NSCellHitResult::ContentArea.0 | NSCellHitResult::TrackableArea.0)
    } else {
        NSCellHitResult::None
    }
}

/// The part's state, from the cell's.
fn part_state(cell: &NSButtonCellImpl) -> parts::State {
    let base = cell.base();
    let state = base.raw_state().get();
    parts::State {
        disabled: !base.has(Flags::ENABLED),
        pressed: base.has(Flags::HIGHLIGHTED),
        on: state == 1 && cell.ivars().shows_state_by.get().0 != 0,
        mixed: state == -1,
    }
}

fn emphasis(cell: &NSButtonCellImpl) -> parts::Emphasis {
    let tint = cell.ivars().bezel_color.borrow().clone();
    if let Some(c) = tint {
        return parts::Emphasis::Tinted(theme::color_of(&c));
    }
    if cell.ivars().destructive.get() {
        parts::Emphasis::Destructive
    } else if cell.is_default() {
        parts::Emphasis::Default
    } else {
        parts::Emphasis::Normal
    }
}

fn draw_bezel(cell: &NSButtonCellImpl, frame: NSRect, view: &NSView) {
    let p = theme::palette();
    let s = part_state(cell);
    let axis = parts::Axis { flipped: view.isFlipped() };
    let base = cell.base();
    let i = base.control_size_index();
    match cell.look() {
        Look::Check => parts::check_box(p, image_rect(cell, frame), axis, s),
        Look::Radio => parts::radio(p, image_rect(cell, frame), s),
        Look::Disclosure => parts::disclosure(p, frame, axis, s.on || base.raw_state().get() == 1, s),
        Look::Help => parts::help(p, parts::centered_square(frame, metrics::HELP[i].min(frame.size.height)), axis, s),
        _ if !base.has(Flags::BORDERED) => {}
        Look::PushDisclosure => {
            let r = parts::centered_square(frame, metrics::HELP[i].min(frame.size.height));
            let emphasis = emphasis(cell);
            parts::button_bezel(p, r, theme::paint::radii(r.size.width / 2.0), emphasis, s);
            parts::chevron(r, axis, 8.0, base.raw_state().get() == 1, parts::button_text(p, emphasis, s));
        }
        Look::Circular => {
            // The whole frame: a circle without a title, a pill with one.
            let r = if title_size(cell).width == 0.0 {
                parts::centered_square(frame, frame.size.width.min(frame.size.height))
            } else {
                frame
            };
            parts::button_bezel(p, r, theme::paint::radii(r.size.height / 2.0), emphasis(cell), s);
        }
        Look::Badge => {
            parts::button_bezel(p, frame, theme::paint::radii(frame.size.height / 2.0), emphasis(cell), s);
        }
        look => {
            let r = match look {
                Look::Push => {
                    let d = drawing_rect(cell, frame);
                    NSRect::new(
                        NSPoint::new(frame.origin.x, d.origin.y),
                        NSSize::new(frame.size.width, d.size.height.min(frame.size.height)),
                    )
                }
                _ => frame,
            };
            let radius = if matches!(look, Look::SmallSquare | Look::ShadowlessSquare) { 0.0 } else { parts::RADIUS };
            parts::button_bezel(p, r, theme::paint::radii(radius), emphasis(cell), s);
        }
    }
}

/// Draw the title in `frame`: `title` when a subclass's `drawTitle:…` was
/// given one, else the cell's own value, which makes no object.
fn draw_title(cell: &NSButtonCellImpl, title: Option<&NSAttributedString>, frame: NSRect) {
    let p = theme::palette();
    let s = part_state(cell);
    let base = cell.base();
    let bordered_push = base.has(Flags::BORDERED) && !matches!(cell.look(), Look::Check | Look::Radio);
    let mut color = if bordered_push { parts::button_text(p, emphasis(cell), s) } else { p.label };
    if !bordered_push {
        let tint = cell.ivars().content_tint.borrow().clone();
        if let Some(tint) = tint {
            color = theme::color_of(&tint);
        }
        if s.disabled {
            color = theme::palette::dimmed(color);
        }
    }
    let font = title_font(cell);
    let attrs = cell::attrs(base, &font, color);
    let styled = match title {
        Some(title) => Styled::attributed(title, &attrs),
        None => cell::styled_with(base, attrs),
    };
    styled.draw(frame);
}

// NSButton

define_class!(
    #[unsafe(super(NSControl, NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSButton"]
    pub(crate) struct NSButtonImpl;

    impl NSButtonImpl {
        #[unsafe(method_id(buttonWithTitle:image:target:action:))]
        fn button_with_title_image(
            title: &NSString,
            image: &AnyObject,
            target: Option<&AnyObject>,
            action: Option<Sel>,
        ) -> Retained<NSButton> {
            let b = push_button(title, target, action, main_thread());
            set_image(&b, image);
            b.setImagePosition(NSCellImagePosition::ImageLeading);
            b.sizeToFit();
            b
        }

        #[unsafe(method_id(buttonWithTitle:target:action:))]
        fn button_with_title(title: &NSString, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSButton> {
            push_button(title, target, action, main_thread())
        }

        #[unsafe(method_id(buttonWithImage:target:action:))]
        fn button_with_image(image: &AnyObject, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSButton> {
            let b = push_button(&NSString::new(), target, action, main_thread());
            set_image(&b, image);
            b.setImagePosition(NSCellImagePosition::ImageOnly);
            b.sizeToFit();
            b
        }

        #[unsafe(method_id(checkboxWithTitle:target:action:))]
        fn checkbox_with_title(title: &NSString, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSButton> {
            titled_button(title, NSButtonType::Switch, target, action, main_thread())
        }

        #[unsafe(method_id(radioButtonWithTitle:target:action:))]
        fn radio_button_with_title(title: &NSString, target: Option<&AnyObject>, action: Option<Sel>) -> Retained<NSButton> {
            titled_button(title, NSButtonType::Radio, target, action, main_thread())
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(setButtonType:))]
        fn set_button_type(&self, kind: NSButtonType) {
            self.with_cell(|c| c.setButtonType(kind));
        }

        #[unsafe(method_id(title))]
        fn title(&self) -> Retained<NSString> {
            self.button_cell().map_or_else(NSString::new, |c| c.title())
        }

        #[unsafe(method(setTitle:))]
        fn set_title(&self, title: &NSString) {
            self.with_cell(|c| c.setTitle(Some(title)));
        }

        #[unsafe(method_id(attributedTitle))]
        fn attributed_title(&self) -> Retained<NSAttributedString> {
            self.button_cell().map_or_else(|| value::attributed(&NSString::new()), |c| c.attributedTitle())
        }

        #[unsafe(method(setAttributedTitle:))]
        fn set_attributed_title(&self, title: &NSAttributedString) {
            self.with_cell(|c| c.setAttributedTitle(title));
        }

        #[unsafe(method_id(alternateTitle))]
        fn alternate_title(&self) -> Retained<NSString> {
            self.button_cell().map_or_else(NSString::new, |c| c.alternateTitle())
        }

        #[unsafe(method(setAlternateTitle:))]
        fn set_alternate_title(&self, title: &NSString) {
            self.with_cell(|c| c.setAlternateTitle(title));
        }

        #[unsafe(method_id(attributedAlternateTitle))]
        fn attributed_alternate_title(&self) -> Retained<NSAttributedString> {
            self.button_cell().map_or_else(|| value::attributed(&NSString::new()), |c| c.attributedAlternateTitle())
        }

        #[unsafe(method(setAttributedAlternateTitle:))]
        fn set_attributed_alternate_title(&self, title: &NSAttributedString) {
            self.with_cell(|c| c.setAttributedAlternateTitle(title));
        }

        #[unsafe(method(hasDestructiveAction))]
        fn has_destructive_action(&self) -> bool {
            self.button_cell().is_some_and(|c| imp(&c).ivars().destructive.get())
        }

        #[unsafe(method(setHasDestructiveAction:))]
        fn set_has_destructive_action(&self, flag: bool) {
            if let Some(c) = self.button_cell()
                && imp(&c).ivars().destructive.replace(flag) != flag
            {
                cell::redraw(imp(&c).base());
            }
        }

        #[unsafe(method_id(sound))]
        fn sound(&self) -> Option<Retained<AnyObject>> {
            // SAFETY: sound takes nothing and returns a sound or nil.
            self.button_cell().and_then(|c| unsafe { msg_send![&*c, sound] })
        }

        #[unsafe(method(setSound:))]
        fn set_sound(&self, sound: Option<&AnyObject>) {
            // SAFETY: setSound: takes a sound or nil.
            self.with_cell(|c| unsafe { msg_send![c, setSound: sound] });
        }

        #[unsafe(method(isSpringLoaded))]
        fn is_spring_loaded(&self) -> bool {
            false
        }

        #[unsafe(method(setSpringLoaded:))]
        fn set_spring_loaded(&self, _flag: bool) {}

        #[unsafe(method(maxAcceleratorLevel))]
        fn max_accelerator_level(&self) -> isize {
            1
        }

        #[unsafe(method(setMaxAcceleratorLevel:))]
        fn set_max_accelerator_level(&self, _level: isize) {}

        #[unsafe(method(setPeriodicDelay:interval:))]
        fn set_periodic_delay(&self, delay: f32, interval: f32) {
            self.with_cell(|c| c.setPeriodicDelay_interval(delay, interval));
        }

        #[unsafe(method(getPeriodicDelay:interval:))]
        fn get_periodic_delay(&self, delay: *mut f32, interval: *mut f32) {
            // SAFETY: the caller's pointers go on to the cell's method.
            self.with_cell(|c| unsafe { msg_send![c, getPeriodicDelay: delay, interval: interval] });
        }

        #[unsafe(method(bezelStyle))]
        fn bezel_style(&self) -> NSBezelStyle {
            self.button_cell().map_or(NSBezelStyle::Automatic, |c| c.bezelStyle())
        }

        #[unsafe(method(setBezelStyle:))]
        fn set_bezel_style(&self, style: NSBezelStyle) {
            self.with_cell(|c| c.setBezelStyle(style));
        }

        #[unsafe(method(isBordered))]
        fn is_bordered(&self) -> bool {
            self.button_cell().is_some_and(|c| c.isBordered())
        }

        #[unsafe(method(setBordered:))]
        fn set_bordered(&self, flag: bool) {
            self.with_cell(|c| c.setBordered(flag));
        }

        #[unsafe(method(isTransparent))]
        fn is_transparent(&self) -> bool {
            self.button_cell().is_some_and(|c| c.isTransparent())
        }

        #[unsafe(method(setTransparent:))]
        fn set_transparent(&self, flag: bool) {
            self.with_cell(|c| c.setTransparent(flag));
        }

        #[unsafe(method(showsBorderOnlyWhileMouseInside))]
        fn shows_border_only_while_mouse_inside(&self) -> bool {
            self.button_cell().is_some_and(|c| c.showsBorderOnlyWhileMouseInside())
        }

        #[unsafe(method(setShowsBorderOnlyWhileMouseInside:))]
        fn set_shows_border_only_while_mouse_inside(&self, flag: bool) {
            self.with_cell(|c| c.setShowsBorderOnlyWhileMouseInside(flag));
        }

        #[unsafe(method_id(bezelColor))]
        fn bezel_color(&self) -> Option<Retained<NSColor>> {
            self.button_cell().and_then(|c| imp(&c).ivars().bezel_color.borrow().clone())
        }

        #[unsafe(method(setBezelColor:))]
        fn set_bezel_color(&self, color: Option<&NSColor>) {
            if let Some(c) = self.button_cell() {
                set_color(imp(&c).base(), &imp(&c).ivars().bezel_color, color);
            }
        }

        #[unsafe(method_id(contentTintColor))]
        fn content_tint_color(&self) -> Option<Retained<NSColor>> {
            self.button_cell().and_then(|c| imp(&c).ivars().content_tint.borrow().clone())
        }

        #[unsafe(method(setContentTintColor:))]
        fn set_content_tint_color(&self, color: Option<&NSColor>) {
            if let Some(c) = self.button_cell() {
                set_color(imp(&c).base(), &imp(&c).ivars().content_tint, color);
            }
        }

        #[unsafe(method(tintProminence))]
        fn tint_prominence(&self) -> isize {
            0
        }

        #[unsafe(method(setTintProminence:))]
        fn set_tint_prominence(&self, _prominence: isize) {}

        #[unsafe(method(borderShape))]
        fn border_shape(&self) -> NSControlBorderShape {
            NSControlBorderShape::Automatic
        }

        #[unsafe(method(setBorderShape:))]
        fn set_border_shape(&self, _shape: NSControlBorderShape) {}

        #[unsafe(method_id(image))]
        fn image(&self) -> Option<Retained<AnyObject>> {
            // SAFETY: image takes nothing and returns an image or nil.
            self.the_cell().and_then(|c| unsafe { msg_send![&*c, image] })
        }

        #[unsafe(method(setImage:))]
        fn set_image(&self, image: Option<&AnyObject>) {
            // SAFETY: setImage: takes an image or nil.
            self.with_any_cell(|c| unsafe { msg_send![c, setImage: image] });
        }

        #[unsafe(method_id(alternateImage))]
        fn alternate_image(&self) -> Option<Retained<AnyObject>> {
            self.button_cell().and_then(|c| imp(&c).ivars().alternate_image.borrow().clone())
        }

        #[unsafe(method(setAlternateImage:))]
        fn set_alternate_image(&self, image: Option<&AnyObject>) {
            // SAFETY: setAlternateImage: takes an image or nil.
            self.with_cell(|c| unsafe { msg_send![c, setAlternateImage: image] });
        }

        #[unsafe(method(imagePosition))]
        fn image_position(&self) -> NSCellImagePosition {
            self.button_cell().map_or(NSCellImagePosition::NoImage, |c| c.imagePosition())
        }

        #[unsafe(method(setImagePosition:))]
        fn set_image_position(&self, position: NSCellImagePosition) {
            self.with_cell(|c| c.setImagePosition(position));
        }

        #[unsafe(method(imageScaling))]
        fn image_scaling(&self) -> NSImageScaling {
            self.button_cell().map_or(NSImageScaling::ScaleProportionallyDown, |c| c.imageScaling())
        }

        #[unsafe(method(setImageScaling:))]
        fn set_image_scaling(&self, scaling: NSImageScaling) {
            self.with_cell(|c| c.setImageScaling(scaling));
        }

        #[unsafe(method(imageHugsTitle))]
        fn image_hugs_title(&self) -> bool {
            self.button_cell().is_some_and(|c| imp(&c).ivars().image_hugs_title.get())
        }

        #[unsafe(method(setImageHugsTitle:))]
        fn set_image_hugs_title(&self, flag: bool) {
            if let Some(c) = self.button_cell()
                && imp(&c).ivars().image_hugs_title.replace(flag) != flag
            {
                cell::changed(imp(&c).base());
            }
        }

        #[unsafe(method_id(symbolConfiguration))]
        fn symbol_configuration(&self) -> Option<Retained<AnyObject>> {
            None
        }

        #[unsafe(method(setSymbolConfiguration:))]
        fn set_symbol_configuration(&self, _configuration: Option<&AnyObject>) {}

        #[unsafe(method(state))]
        fn state(&self) -> isize {
            self.the_cell().map_or(0, |c| c.state())
        }

        #[unsafe(method(setState:))]
        fn set_state(&self, state: isize) {
            self.with_any_cell(|c| c.setState(state));
        }

        #[unsafe(method(allowsMixedState))]
        fn allows_mixed_state(&self) -> bool {
            self.the_cell().is_some_and(|c| c.allowsMixedState())
        }

        #[unsafe(method(setAllowsMixedState:))]
        fn set_allows_mixed_state(&self, flag: bool) {
            self.with_any_cell(|c| c.setAllowsMixedState(flag));
        }

        #[unsafe(method(setNextState))]
        fn set_next_state(&self) {
            self.with_any_cell(|c| c.setNextState());
        }

        #[unsafe(method(highlight:))]
        fn highlight(&self, flag: bool) {
            self.with_any_cell(|c| c.setHighlighted(flag));
        }

        #[unsafe(method_id(keyEquivalent))]
        fn key_equivalent(&self) -> Retained<NSString> {
            self.button_cell().map_or_else(NSString::new, |c| c.keyEquivalent())
        }

        #[unsafe(method(setKeyEquivalent:))]
        fn set_key_equivalent(&self, key: &NSString) {
            self.with_cell(|c| c.setKeyEquivalent(key));
        }

        #[unsafe(method(keyEquivalentModifierMask))]
        fn key_equivalent_modifier_mask(&self) -> NSEventModifierFlags {
            self.button_cell().map_or(NSEventModifierFlags::empty(), |c| c.keyEquivalentModifierMask())
        }

        #[unsafe(method(setKeyEquivalentModifierMask:))]
        fn set_key_equivalent_modifier_mask(&self, mask: NSEventModifierFlags) {
            self.with_cell(|c| c.setKeyEquivalentModifierMask(mask));
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            super::focus::button_key_equivalent(self.as_button(), event)
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if !super::focus::button_key_down(self.as_button(), event) {
                // SAFETY: NSResponder's keyDown: passes the key on.
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        #[unsafe(method(setTitleWithMnemonic:))]
        fn set_title_with_mnemonic(&self, title: Option<&NSString>) {
            // SAFETY: the cell's method takes a string or nil.
            self.with_cell(|c| unsafe { c.setTitleWithMnemonic(title) });
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            control::cached_intrinsic(self.as_button(), || match self.the_cell() {
                // A button cell's intrinsic size leaves out what its frame
                // reaches past its alignment rect; other cells answer
                // cellSize.
                Some(c) => match as_button_cell(&c) {
                    Some(b) if !overrides_cell_size(b) => intrinsic_size(b),
                    _ => c.cellSize(),
                },
                None => NSSize::new(control::NO_METRIC, control::NO_METRIC),
            })
        }

        #[unsafe(method(compressWithPrioritizedCompressionOptions:))]
        fn compress(&self, _options: &AnyObject) {}

        #[unsafe(method(minimumSizeWithPrioritizedCompressionOptions:))]
        fn minimum_size(&self, _options: &AnyObject) -> NSSize {
            // SAFETY: intrinsicContentSize takes nothing and returns a size.
            unsafe { msg_send![self, intrinsicContentSize] }
        }

        #[unsafe(method_id(activeCompressionOptions))]
        fn active_compression_options(&self) -> Option<Retained<AnyObject>> {
            None
        }
    }
);

impl NSButtonImpl {
    fn as_button(&self) -> &NSButton {
        // SAFETY: NSButtonImpl is the class NSButton names.
        unsafe { &*(self as *const Self).cast::<NSButton>() }
    }

    fn the_cell(&self) -> Option<Retained<NSCell>> {
        let control: &NSControl = self.as_button();
        control.cell()
    }

    fn button_cell(&self) -> Option<Retained<NSButtonCell>> {
        let cell = self.the_cell()?;
        as_button_cell(&cell)?;
        // SAFETY: checked just above.
        Some(unsafe { Retained::cast_unchecked(cell) })
    }

    fn with_cell(&self, f: impl FnOnce(&NSButtonCell)) {
        if let Some(c) = self.button_cell() {
            f(&c);
        }
    }

    fn with_any_cell(&self, f: impl FnOnce(&NSCell)) {
        if let Some(c) = self.the_cell() {
            f(&c);
        }
    }
}

fn set_image(button: &NSButton, image: &AnyObject) {
    // SAFETY: setImage: takes an image.
    let _: () = unsafe { msg_send![button, setImage: image] };
}

/// Class methods run on the main thread, as AppKit's classes do.
fn main_thread() -> MainThreadMarker {
    MainThreadMarker::new().expect("sidestep: AppKit's controls belong to the main thread")
}

/// `buttonWithTitle:target:action:`: a push button sized to fit, its
/// title truncated at the tail when squeezed.
fn push_button(
    title: &NSString,
    target: Option<&AnyObject>,
    action: Option<Sel>,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    let b = NSButton::initWithFrame(NSButton::alloc(mtm), NSRect::ZERO);
    b.setTitle(title);
    b.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    // SAFETY: the button keeps the target weakly; any selector may be an
    // action.
    unsafe {
        b.setTarget(target);
        b.setAction(action);
    }
    b.sizeToFit();
    b
}

/// A check box or radio button with a title, sized to fit.
fn titled_button(
    title: &NSString,
    kind: NSButtonType,
    target: Option<&AnyObject>,
    action: Option<Sel>,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    let b = NSButton::initWithFrame(NSButton::alloc(mtm), NSRect::ZERO);
    b.setButtonType(kind);
    b.setBezelStyle(NSBezelStyle::FlexiblePush);
    b.setTitle(title);
    b.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    // SAFETY: as in `push_button`.
    unsafe {
        b.setTarget(target);
        b.setAction(action);
    }
    b.sizeToFit();
    b
}

/// Whether a button cell's class measures itself its own way, so that
/// its button asks it (`cellSize`) rather than computing the intrinsic
/// size here.
fn overrides_cell_size(cell: &NSButtonCellImpl) -> bool {
    let sel = sel!(cellSize);
    let ours = <NSButtonCellImpl as objc2::ClassType>::class().instance_method(sel).map(|m| m.implementation());
    let theirs = cell.class().instance_method(sel).map(|m| m.implementation());
    ours.map(|f| f as usize) != theirs.map(|f| f as usize)
}
