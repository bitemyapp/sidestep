//! The theme's colors, light and dark, after GNOME's Adwaita.
//!
//! Adwaita draws controls as flat, translucent washes of the foreground
//! color over whatever is behind them (a button is its text color at a
//! tenth of full strength, more when hovered or pressed), with one accent
//! color for what is on, chosen or in progress. The values are Adwaita's
//! published palette (libadwaita's named colors); how they're combined is
//! Sidestep's own.

use crate::protocol::Color;

/// Every color a painter asks for, for one appearance: the semantic slots
/// the theme covers, some of which wait for the controls that use them
/// (links, success and warning badges, hover).
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub(crate) struct Palette {
    /// Text, at full strength and in AppKit's three weaker tiers.
    pub label: Color,
    pub secondary_label: Color,
    pub tertiary_label: Color,
    pub quaternary_label: Color,
    /// Behind a window's content.
    pub window: Color,
    /// Behind text views and lists.
    pub view: Color,
    /// Cards: boxes that stand out from the window.
    pub card: Color,
    pub card_border: Color,
    pub separator: Color,
    /// The accent, and text drawn on it.
    pub accent: Color,
    pub accent_text_on: Color,
    /// The accent as text on the window (links, a selected label).
    pub accent_text: Color,
    pub destructive: Color,
    pub destructive_text: Color,
    pub success: Color,
    pub warning: Color,
    /// A button's wash: at rest, under the pointer, pressed.
    pub button: Color,
    pub button_hover: Color,
    pub button_pressed: Color,
    /// A text field's wash.
    pub entry: Color,
    /// Troughs: a progress bar's, a slider's, a switch's when off.
    pub trough: Color,
    /// The knob of a slider or a switch.
    pub knob: Color,
    pub knob_border: Color,
    /// Outlines of check boxes and radio buttons when off.
    pub outline: Color,
    pub focus_ring: Color,
}

const fn rgb(hex: u32) -> Color {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, 1.0]
}

const fn with_alpha(c: Color, a: f32) -> Color {
    [c[0], c[1], c[2], a]
}

/// Adwaita's blue 3, the default accent.
const BLUE: Color = rgb(0x3584e4);
/// Near-black with a touch of blue, Adwaita's light foreground.
const INK: Color = rgb(0x000006);

pub(crate) const LIGHT: Palette = Palette {
    label: with_alpha(INK, 0.8),
    secondary_label: with_alpha(INK, 0.55),
    tertiary_label: with_alpha(INK, 0.3),
    quaternary_label: with_alpha(INK, 0.18),
    window: rgb(0xfafafb),
    view: rgb(0xffffff),
    card: rgb(0xffffff),
    card_border: with_alpha(INK, 0.1),
    separator: with_alpha(INK, 0.15),
    accent: BLUE,
    accent_text_on: rgb(0xffffff),
    accent_text: rgb(0x1c71d8),
    destructive: rgb(0xe01b24),
    destructive_text: rgb(0xc01c28),
    success: rgb(0x2ec27e),
    warning: rgb(0xe5a50a),
    button: with_alpha(INK, 0.08),
    button_hover: with_alpha(INK, 0.12),
    button_pressed: with_alpha(INK, 0.25),
    entry: with_alpha(INK, 0.08),
    trough: with_alpha(INK, 0.15),
    knob: rgb(0xffffff),
    knob_border: with_alpha(INK, 0.2),
    outline: with_alpha(INK, 0.3),
    focus_ring: with_alpha(BLUE, 0.5),
};

pub(crate) const DARK: Palette = Palette {
    label: rgb(0xffffff),
    secondary_label: with_alpha(rgb(0xffffff), 0.55),
    tertiary_label: with_alpha(rgb(0xffffff), 0.3),
    quaternary_label: with_alpha(rgb(0xffffff), 0.18),
    window: rgb(0x222226),
    view: rgb(0x1d1d20),
    card: with_alpha(rgb(0xffffff), 0.08),
    card_border: with_alpha(rgb(0x000000), 0.36),
    separator: with_alpha(rgb(0xffffff), 0.15),
    accent: BLUE,
    accent_text_on: rgb(0xffffff),
    accent_text: rgb(0x78aeed),
    destructive: rgb(0xc01c28),
    destructive_text: rgb(0xff7b63),
    success: rgb(0x26a269),
    warning: rgb(0xcd9309),
    button: with_alpha(rgb(0xffffff), 0.1),
    button_hover: with_alpha(rgb(0xffffff), 0.15),
    button_pressed: with_alpha(rgb(0xffffff), 0.3),
    entry: with_alpha(rgb(0xffffff), 0.1),
    trough: with_alpha(rgb(0xffffff), 0.15),
    knob: rgb(0xdeddda),
    knob_border: with_alpha(rgb(0x000000), 0.3),
    outline: with_alpha(rgb(0xffffff), 0.3),
    focus_ring: with_alpha(BLUE, 0.5),
};

/// `c` as a disabled part draws it: at half its alpha.
pub(crate) fn dimmed(c: Color) -> Color {
    [c[0], c[1], c[2], c[3] * 0.5]
}

/// `c` at `alpha` times its alpha.
pub(crate) fn faded(c: Color, alpha: f32) -> Color {
    [c[0], c[1], c[2], c[3] * alpha]
}
