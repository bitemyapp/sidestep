//! Controls: `NSCell`, `NSActionCell` and `NSControl`, and the controls
//! built on them.
//!
//! Cells are real, as in AppKit: a control makes its cell from
//! `+cellClass`, forwards its properties to it by message, and draws and
//! tracks through it, so programs that subclass either (or replace a
//! control's cell) get the behavior they would on macOS. Anything a
//! subclass may override is reached by message; the rest is plain Rust.
//!
//! - `value` holds what a cell holds, and how its value converts;
//! - `cell` is `NSCell` and `NSActionCell`, `control` is `NSControl`;
//! - `track` has the mouse-tracking loops and `performClick:`;
//! - `button` is `NSButton` and `NSButtonCell`;
//! - `focus` has key equivalents, the default button and focus rings;
//! - `text_field` has the text fields, and the hooks the field editor
//!   fills in; `search_field` is `NSSearchField` and its cell;
//! - `box_view` is `NSBox`;
//! - `progress` is `NSProgressIndicator`, animated on the window's frames;
//! - `segmented` is `NSSegmentedControl` and its cell;
//! - `stepper`, `slider` and `switch` are `NSStepper`, `NSSlider` and
//!   `NSSwitch`;
//! - `a11y` keeps the accessibility properties of views and cells.
//!
//! Geometry (sizes and rectangles a program can see) is Apple's, measured
//! by `conformance/tests/controls.rs` and kept in `theme::metrics`; what is
//! drawn is the theme's (`theme`).

pub(crate) mod a11y;
#[cfg(test)]
mod bench;
pub(crate) mod box_view;
pub(crate) mod button;
pub(crate) mod cell;
pub(crate) mod control;
pub(crate) mod focus;
pub(crate) mod progress;
pub(crate) mod search_field;
pub(crate) mod segmented;
pub(crate) mod slider;
pub(crate) mod stepper;
pub(crate) mod switch;
pub(crate) mod text_field;
pub(crate) mod track;
pub(crate) mod value;

use objc2::ClassType;
use objc2::runtime::{AnyClass, AnyObject};

sidestep_runtime::static_class!(pub NSCELL, NSCELL_META = "NSCell", || {
    a11y::install(cell::NSCellImpl::class());
});

sidestep_runtime::static_class!(pub NSACTIONCELL, NSACTIONCELL_META = "NSActionCell", || {
    let _ = cell::NSActionCellImpl::class();
});

sidestep_runtime::static_class!(pub NSCONTROL, NSCONTROL_META = "NSControl", || {
    control::install_class_methods(control::NSControlImpl::class());
});

sidestep_runtime::static_class!(pub NSBUTTONCELL, NSBUTTONCELL_META = "NSButtonCell", || {
    let _ = button::NSButtonCellImpl::class();
});

sidestep_runtime::static_class!(pub NSBUTTON, NSBUTTON_META = "NSButton", || {
    control::register_cell_class(button::NSButtonImpl::class(), objc2_app_kit::NSButtonCell::class());
});

sidestep_runtime::static_class!(pub NSTEXTFIELDCELL, NSTEXTFIELDCELL_META = "NSTextFieldCell", || {
    let _ = text_field::NSTextFieldCellImpl::class();
});

sidestep_runtime::static_class!(pub NSTEXTFIELD, NSTEXTFIELD_META = "NSTextField", || {
    control::register_cell_class(text_field::NSTextFieldImpl::class(), objc2_app_kit::NSTextFieldCell::class());
});

sidestep_runtime::static_class!(pub NSSECURETEXTFIELDCELL, NSSECURETEXTFIELDCELL_META = "NSSecureTextFieldCell", || {
    let _ = text_field::NSSecureTextFieldCellImpl::class();
});

sidestep_runtime::static_class!(pub NSSECURETEXTFIELD, NSSECURETEXTFIELD_META = "NSSecureTextField", || {
    control::register_cell_class(
        text_field::NSSecureTextFieldImpl::class(),
        objc2_app_kit::NSSecureTextFieldCell::class(),
    );
});

sidestep_runtime::static_class!(pub NSSEARCHFIELDCELL, NSSEARCHFIELDCELL_META = "NSSearchFieldCell", || {
    let _ = search_field::NSSearchFieldCellImpl::class();
});

sidestep_runtime::static_class!(pub NSSEARCHFIELD, NSSEARCHFIELD_META = "NSSearchField", || {
    control::register_cell_class(search_field::NSSearchFieldImpl::class(), objc2_app_kit::NSSearchFieldCell::class());
});

sidestep_runtime::static_class!(pub NSBOX, NSBOX_META = "NSBox", || {
    let _ = box_view::NSBoxImpl::class();
});

sidestep_runtime::static_class!(pub NSPROGRESSINDICATOR, NSPROGRESSINDICATOR_META = "NSProgressIndicator", || {
    let _ = progress::NSProgressIndicatorImpl::class();
});

sidestep_runtime::static_class!(pub NSSEGMENTEDCELL, NSSEGMENTEDCELL_META = "NSSegmentedCell", || {
    let _ = segmented::NSSegmentedCellImpl::class();
});

sidestep_runtime::static_class!(pub NSSEGMENTEDCONTROL, NSSEGMENTEDCONTROL_META = "NSSegmentedControl", || {
    control::register_cell_class(segmented::NSSegmentedControlImpl::class(), objc2_app_kit::NSSegmentedCell::class());
});

sidestep_runtime::static_class!(pub NSSTEPPERCELL, NSSTEPPERCELL_META = "NSStepperCell", || {
    let _ = stepper::NSStepperCellImpl::class();
});

sidestep_runtime::static_class!(pub NSSTEPPER, NSSTEPPER_META = "NSStepper", || {
    control::register_cell_class(stepper::NSStepperImpl::class(), objc2_app_kit::NSStepperCell::class());
});

sidestep_runtime::static_class!(pub NSSLIDERCELL, NSSLIDERCELL_META = "NSSliderCell", || {
    let _ = slider::NSSliderCellImpl::class();
});

sidestep_runtime::static_class!(pub NSSLIDER, NSSLIDER_META = "NSSlider", || {
    control::register_cell_class(slider::NSSliderImpl::class(), objc2_app_kit::NSSliderCell::class());
});

sidestep_runtime::static_class!(pub NSSWITCH, NSSWITCH_META = "NSSwitch", || {
    let _ = switch::NSSwitchImpl::class();
});

// Posted by text fields as their text is edited (the field editor posts
// them); the values are macOS's.
sidestep_foundation::constant_string!(
    NSControlTextDidBeginEditingNotification = "NSControlTextDidBeginEditingNotification"
);
sidestep_foundation::constant_string!(NSControlTextDidChangeNotification = "NSControlTextDidChangeNotification");
sidestep_foundation::constant_string!(
    NSControlTextDidEndEditingNotification = "NSControlTextDidEndEditingNotification"
);

/// A view without an intrinsic size along an axis says so with this.
#[unsafe(no_mangle)]
pub static NSViewNoIntrinsicMetric: f64 = control::NO_METRIC;

/// Whether `object` is an instance of `class` or of a subclass, found by
/// walking its class's superclasses: `isKindOfClass:` without a message.
pub(crate) fn kind_of(object: &AnyObject, class: &AnyClass) -> bool {
    let mut c = Some(object.class());
    while let Some(k) = c {
        if std::ptr::eq(k, class) {
            return true;
        }
        c = k.superclass();
    }
    false
}

/// `object` as `I`, the implementation class behind the binding type `T`,
/// if it is a `T` (or a subclass).
///
/// # Safety
///
/// `I` must be the `define_class!` type registered under `T`'s class name,
/// so that every instance of `T` or a subclass has `I`'s layout.
pub(crate) unsafe fn impl_of<T: ClassType, I>(object: &AnyObject) -> Option<&I> {
    // SAFETY: the object's class descends from T's, whose instances are
    // I's (the caller's promise).
    kind_of(object, T::class()).then(|| unsafe { &*(object as *const AnyObject).cast::<I>() })
}
