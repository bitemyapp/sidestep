//! How Sidestep's controls look: an Adwaita-like theme, light or dark.
//!
//! Geometry is Apple's, pixels are Adwaita's. Every rectangle and size a
//! program can observe (a button's intrinsic size, a cell's title rect, a
//! box's content frame) matches macOS, measured by the conformance tests
//! and kept in [`metrics`]; what is drawn inside those rectangles is
//! GNOME's look: flat translucent washes, rounded corners, one accent
//! color.
//!
//! - [`palette`]: the colors, light and dark.
//! - [`metrics`]: Apple's geometry constants, each naming the test that
//!   pins it.
//! - [`paint`]: the only way painters reach the recorder.
//! - [`parts`]: painters for each part (bezels, check boxes, tracks,
//!   knobs, focus rings), in view coordinates.
//!
//! Painting allocates no Objective-C objects: painters take colors and
//! geometry as plain values.

#[cfg(test)]
mod golden;
pub(crate) mod metrics;
pub(crate) mod paint;
pub(crate) mod palette;
pub(crate) mod parts;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, Sel};
use objc2::{ClassType, msg_send};
use objc2_app_kit::NSColor;

use crate::protocol::Color;
pub(crate) use palette::Palette;

/// Whether to paint dark: the current drawing appearance's (a view's
/// effective appearance while it draws, else the application's, which
/// follows the desktop's preference and `SIDESTEP_APPEARANCE`).
pub(crate) fn dark() -> bool {
    crate::appearance::current_look().dark()
}

/// The palette to paint with now.
pub(crate) fn palette() -> &'static Palette {
    if dark() { &palette::DARK } else { &palette::LIGHT }
}

/// A named system color (`labelColor`, `controlAccentColor`, …) as
/// `NSColor` answers it where it has the method, else made from the
/// palette. Getters that must return an `NSColor` use this, so they work
/// before and after `NSColor` learns its semantic colors.
pub(crate) fn system_color(sel: Sel, fallback: impl FnOnce(&Palette) -> Color) -> Retained<NSColor> {
    let class: &AnyClass = NSColor::class();
    if class.class_method(sel).is_some() {
        // SAFETY: NSColor's named-color class methods take nothing and
        // return an autoreleased color.
        let color: *mut NSColor = unsafe { objc2::runtime::MessageReceiver::send_message(class, sel, ()) };
        // SAFETY: as above; the pointer is a color or nil.
        if let Some(color) = unsafe { Retained::retain_autoreleased(color) } {
            return color;
        }
    }
    let [r, g, b, a] = fallback(palette()).map(f64::from);
    // SAFETY: colorWithSRGBRed:green:blue:alpha: takes four components.
    unsafe { msg_send![class, colorWithSRGBRed: r, green: g, blue: b, alpha: a] }
}

/// The components of a color a program set, to paint with.
pub(crate) fn color_of(color: &NSColor) -> Color {
    crate::color::resolve(color)
}

/// A system color as it is on an emphasized background (a selected row of
/// the key window's focused table): the light text for selections for the
/// label colors, the color itself for the rest (see
/// `palette::emphasized`).
pub(crate) fn emphasized_color(c: crate::palette::System) -> Color {
    crate::palette::get_on(c, crate::appearance::current_look(), true)
}
