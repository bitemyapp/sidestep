//! Colors: [`Color`], the desktop's [`Appearance`] and the [`SystemColor`]s
//! that follow it and its accent.

use sidestep_engine::palette::{self, Look, System};
use sidestep_engine::settings;

/// A color: straight (not premultiplied) sRGB red, green, blue and alpha,
/// each 0 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Color = Color::rgb(0.0, 0.0, 0.0);
    pub const WHITE: Color = Color::rgb(1.0, 1.0, 1.0);

    pub const fn rgb(r: f32, g: f32, b: f32) -> Color {
        Color { r, g, b, a: 1.0 }
    }

    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
        Color { r, g, b, a }
    }

    /// From 8-bit channels.
    pub const fn rgb8(r: u8, g: u8, b: u8) -> Color {
        Color::rgba8(r, g, b, 255)
    }

    pub const fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: a as f32 / 255.0 }
    }

    /// `0xRRGGBB`, opaque.
    pub const fn hex(rgb: u32) -> Color {
        Color::rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// A CSS-style hex string: `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`
    /// (the `#` optional).
    pub fn parse(s: &str) -> Option<Color> {
        let s = s.trim().trim_start_matches('#');
        let digit = |i: usize| u8::from_str_radix(s.get(i..i + 1)?, 16).ok().map(|d| d * 17);
        let pair = |i: usize| u8::from_str_radix(s.get(i..i + 2)?, 16).ok();
        match s.len() {
            3 => Some(Color::rgb8(digit(0)?, digit(1)?, digit(2)?)),
            4 => Some(Color::rgba8(digit(0)?, digit(1)?, digit(2)?, digit(3)?)),
            6 => Some(Color::rgb8(pair(0)?, pair(2)?, pair(4)?)),
            8 => Some(Color::rgba8(pair(0)?, pair(2)?, pair(4)?, pair(6)?)),
            _ => None,
        }
    }

    /// This color with its alpha multiplied by `alpha`.
    pub fn with_alpha(self, alpha: f32) -> Color {
        Color { a: self.a * alpha, ..self }
    }

    /// Mixed toward `other` by `t` (0: this color, 1: `other`), channel by
    /// channel.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Color { r: m(self.r, other.r), g: m(self.g, other.g), b: m(self.b, other.b), a: m(self.a, other.a) }
    }

    pub(crate) fn raw(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub(crate) fn from_raw([r, g, b, a]: [f32; 4]) -> Color {
        Color { r, g, b, a }
    }
}

/// How the desktop wants programs to look: light or dark, and whether in
/// high contrast. It follows the desktop's settings (xdg-desktop-portal)
/// as they change; `SIDESTEP_APPEARANCE=light` or `dark` overrides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Appearance {
    pub dark: bool,
    pub high_contrast: bool,
}

impl Appearance {
    pub const LIGHT: Appearance = Appearance { dark: false, high_contrast: false };
    pub const DARK: Appearance = Appearance { dark: true, high_contrast: false };

    /// The desktop's appearance, as last told.
    pub fn system() -> Appearance {
        let look = settings::system_look();
        Appearance { dark: look.dark(), high_contrast: look.contrast() }
    }

    fn look(self) -> Look {
        match (self.dark, self.high_contrast) {
            (false, false) => Look::Light,
            (true, false) => Look::Dark,
            (false, true) => Look::LightContrast,
            (true, true) => Look::DarkContrast,
        }
    }

    /// `color` in this appearance, with the desktop's accent.
    pub fn color(self, color: SystemColor) -> Color {
        Color::from_raw(palette::get(color.system(), self.look()))
    }
}

/// A color that depends on the appearance and the desktop's accent color:
/// the same colors AppKit's semantic colors resolve to on Sidestep.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SystemColor {
    /// A window's background.
    WindowBackground,
    /// Behind a document's pages.
    UnderPageBackground,
    /// The background of content: lists, tables, text fields.
    ContentBackground,
    /// Every other row of a list, after `ContentBackground`.
    AlternatingContentBackground,
    /// Selected content, where the selection has the focus.
    SelectedContentBackground,
    /// Selected content, without the focus.
    UnemphasizedSelectedContentBackground,
    /// Text and symbols: primary, secondary, tertiary and quaternary.
    Label,
    SecondaryLabel,
    TertiaryLabel,
    QuaternaryLabel,
    /// Text in a text view or field, and its background.
    Text,
    TextBackground,
    PlaceholderText,
    SelectedText,
    SelectedTextBackground,
    /// The text caret.
    TextInsertionPoint,
    Link,
    Separator,
    Grid,
    /// A control's face, its text, and its text while disabled.
    Control,
    ControlText,
    DisabledControlText,
    /// The desktop's accent color.
    Accent,
    /// A keyboard focus ring.
    FocusIndicator,
    /// Fills for shapes and control backgrounds, strongest first.
    Fill,
    SecondaryFill,
    TertiaryFill,
    QuaternaryFill,
    Shadow,
    Red,
    Orange,
    Yellow,
    Green,
    Mint,
    Teal,
    Cyan,
    Blue,
    Indigo,
    Purple,
    Pink,
    Brown,
    Gray,
}

impl SystemColor {
    fn system(self) -> System {
        use SystemColor::*;
        match self {
            WindowBackground => System::WindowBackground,
            UnderPageBackground => System::UnderPageBackground,
            ContentBackground => System::ControlBackground,
            AlternatingContentBackground => System::AlternatingContentBackground,
            SelectedContentBackground => System::SelectedContentBackground,
            UnemphasizedSelectedContentBackground => System::UnemphasizedSelectedContentBackground,
            Label => System::Label,
            SecondaryLabel => System::SecondaryLabel,
            TertiaryLabel => System::TertiaryLabel,
            QuaternaryLabel => System::QuaternaryLabel,
            Text => System::Text,
            TextBackground => System::TextBackground,
            PlaceholderText => System::PlaceholderText,
            SelectedText => System::SelectedText,
            SelectedTextBackground => System::SelectedTextBackground,
            TextInsertionPoint => System::TextInsertionPoint,
            Link => System::Link,
            Separator => System::Separator,
            Grid => System::Grid,
            Control => System::Control,
            ControlText => System::ControlText,
            DisabledControlText => System::DisabledControlText,
            Accent => System::ControlAccent,
            FocusIndicator => System::KeyboardFocusIndicator,
            Fill => System::Fill,
            SecondaryFill => System::SecondaryFill,
            TertiaryFill => System::TertiaryFill,
            QuaternaryFill => System::QuaternaryFill,
            Shadow => System::Shadow,
            Red => System::Red,
            Orange => System::Orange,
            Yellow => System::Yellow,
            Green => System::Green,
            Mint => System::Mint,
            Teal => System::Teal,
            Cyan => System::Cyan,
            Blue => System::Blue,
            Indigo => System::Indigo,
            Purple => System::Purple,
            Pink => System::Pink,
            Brown => System::Brown,
            Gray => System::Gray,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_strings_parse() {
        assert_eq!(Color::parse("#3584e4"), Some(Color::hex(0x3584e4)));
        assert_eq!(Color::parse("fff"), Some(Color::WHITE));
        assert_eq!(Color::parse("#0000"), Some(Color::TRANSPARENT));
        assert_eq!(Color::parse("#ff000080").map(|c| (c.r, c.a)), Some((1.0, 128.0 / 255.0)));
        assert_eq!(Color::parse("#12345"), None);
        assert_eq!(Color::parse("#gggggg"), None);
    }

    #[test]
    fn appearances_resolve_system_colors() {
        let light = Appearance::LIGHT.color(SystemColor::WindowBackground);
        let dark = Appearance::DARK.color(SystemColor::WindowBackground);
        assert!(light.r > dark.r, "a light window is lighter than a dark one: {light:?} {dark:?}");
        assert_eq!(light.a, 1.0);
    }
}
