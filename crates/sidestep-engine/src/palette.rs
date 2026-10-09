//! Sidestep's system colors: what `labelColor`, `windowBackgroundColor` and
//! the other catalog colors are under each appearance.
//!
//! The tables are Sidestep's own design, made to sit well on GNOME and KDE
//! desktops (their window and view backgrounds, their fixed palette of
//! hues), not values read off macOS. There are four: light, dark, and a
//! high-contrast version of each, where labels are opaque and separators
//! strong. The accent comes from the desktop (the settings portal's
//! `accent-color`), blue unless it says otherwise, and the selection
//! colors derive from it.
//!
//! What AppKit programs rely on is the relations, which the tests check:
//! labels are dark on light backgrounds and light on dark ones, and the
//! label hierarchy fades from primary to quinary.

use std::sync::RwLock;

use crate::protocol::Color;

/// Which table colors come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    Light,
    Dark,
    LightContrast,
    DarkContrast,
}

impl Look {
    pub fn dark(self) -> bool {
        matches!(self, Look::Dark | Look::DarkContrast)
    }

    pub fn contrast(self) -> bool {
        matches!(self, Look::LightContrast | Look::DarkContrast)
    }

    fn index(self) -> usize {
        self as usize
    }
}

macro_rules! system_colors {
    ($($variant:ident = $selector:literal,)*) => {
        /// A catalog color, named as its `NSColor` class method is.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum System {
            $($variant,)*
        }

        impl System {
            pub const ALL: &[System] = &[$(System::$variant,)*];

            /// The selector that makes it, which is also its name in the
            /// "System" catalog.
            pub fn name(self) -> &'static str {
                match self {
                    $(System::$variant => $selector,)*
                }
            }
        }
    };
}

system_colors! {
    Label = "labelColor",
    SecondaryLabel = "secondaryLabelColor",
    TertiaryLabel = "tertiaryLabelColor",
    QuaternaryLabel = "quaternaryLabelColor",
    QuinaryLabel = "quinaryLabelColor",
    Link = "linkColor",
    PlaceholderText = "placeholderTextColor",
    WindowFrameText = "windowFrameTextColor",
    SelectedMenuItemText = "selectedMenuItemTextColor",
    AlternateSelectedControlText = "alternateSelectedControlTextColor",
    HeaderText = "headerTextColor",
    Separator = "separatorColor",
    Grid = "gridColor",
    WindowBackground = "windowBackgroundColor",
    UnderPageBackground = "underPageBackgroundColor",
    ControlBackground = "controlBackgroundColor",
    SelectedContentBackground = "selectedContentBackgroundColor",
    UnemphasizedSelectedContentBackground = "unemphasizedSelectedContentBackgroundColor",
    AlternatingContentBackground = "alternatingContentBackgroundColor",
    FindHighlight = "findHighlightColor",
    Text = "textColor",
    TextBackground = "textBackgroundColor",
    TextInsertionPoint = "textInsertionPointColor",
    SelectedText = "selectedTextColor",
    SelectedTextBackground = "selectedTextBackgroundColor",
    UnemphasizedSelectedTextBackground = "unemphasizedSelectedTextBackgroundColor",
    UnemphasizedSelectedText = "unemphasizedSelectedTextColor",
    Control = "controlColor",
    ControlText = "controlTextColor",
    SelectedControl = "selectedControlColor",
    SelectedControlText = "selectedControlTextColor",
    DisabledControlText = "disabledControlTextColor",
    KeyboardFocusIndicator = "keyboardFocusIndicatorColor",
    ScrubberTexturedBackground = "scrubberTexturedBackgroundColor",
    ControlAccent = "controlAccentColor",
    Highlight = "highlightColor",
    Shadow = "shadowColor",
    Red = "systemRedColor",
    Green = "systemGreenColor",
    Blue = "systemBlueColor",
    Orange = "systemOrangeColor",
    Yellow = "systemYellowColor",
    Brown = "systemBrownColor",
    Pink = "systemPinkColor",
    Purple = "systemPurpleColor",
    Gray = "systemGrayColor",
    Teal = "systemTealColor",
    Indigo = "systemIndigoColor",
    Mint = "systemMintColor",
    Cyan = "systemCyanColor",
    Fill = "systemFillColor",
    SecondaryFill = "secondarySystemFillColor",
    TertiaryFill = "tertiarySystemFillColor",
    QuaternaryFill = "quaternarySystemFillColor",
    QuinaryFill = "quinarySystemFillColor",
    ControlHighlight = "controlHighlightColor",
    ControlLightHighlight = "controlLightHighlightColor",
    ControlShadow = "controlShadowColor",
    ControlDarkShadow = "controlDarkShadowColor",
    ScrollBar = "scrollBarColor",
    Knob = "knobColor",
    SelectedKnob = "selectedKnobColor",
    WindowFrame = "windowFrameColor",
    SelectedMenuItem = "selectedMenuItemColor",
    Header = "headerColor",
    SecondarySelectedControl = "secondarySelectedControlColor",
    AlternateSelectedControl = "alternateSelectedControlColor",
    AlternatingRowBackground = "alternatingRowBackgroundColor",
}

/// 0xRRGGBB and an alpha.
const fn rgb(hex: u32, a: f32) -> Color {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0, a]
}

/// Where a color comes from in a look: a fixed color, or the accent.
#[derive(Clone, Copy)]
enum Source {
    Fixed(Color),
    /// The accent with this alpha.
    Accent(f32),
    /// The accent mixed toward the background by this much (0: the accent).
    AccentMuted(f32),
}

use Source::{Accent, AccentMuted, Fixed};

/// A color in each look: light, dark, light high contrast, dark high
/// contrast.
fn source(c: System, look: Look) -> Source {
    use System::*;
    let (light, dark, lhc, dhc) = match c {
        // Labels: the text color, fading down the hierarchy.
        Label => (
            Fixed(rgb(0x000000, 0.85)),
            Fixed(rgb(0xffffff, 0.87)),
            Fixed(rgb(0x000000, 1.0)),
            Fixed(rgb(0xffffff, 1.0)),
        ),
        SecondaryLabel => (
            Fixed(rgb(0x000000, 0.55)),
            Fixed(rgb(0xffffff, 0.57)),
            Fixed(rgb(0x000000, 0.8)),
            Fixed(rgb(0xffffff, 0.8)),
        ),
        TertiaryLabel => {
            (Fixed(rgb(0x000000, 0.3)), Fixed(rgb(0xffffff, 0.3)), Fixed(rgb(0x000000, 0.6)), Fixed(rgb(0xffffff, 0.6)))
        }
        QuaternaryLabel => (
            Fixed(rgb(0x000000, 0.12)),
            Fixed(rgb(0xffffff, 0.12)),
            Fixed(rgb(0x000000, 0.4)),
            Fixed(rgb(0xffffff, 0.4)),
        ),
        QuinaryLabel => (
            Fixed(rgb(0x000000, 0.05)),
            Fixed(rgb(0xffffff, 0.05)),
            Fixed(rgb(0x000000, 0.2)),
            Fixed(rgb(0xffffff, 0.2)),
        ),
        PlaceholderText | DisabledControlText => {
            (Fixed(rgb(0x000000, 0.3)), Fixed(rgb(0xffffff, 0.3)), Fixed(rgb(0x000000, 0.6)), Fixed(rgb(0xffffff, 0.6)))
        }
        Text | ControlText | HeaderText | WindowFrameText => {
            (Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0xffffff, 1.0)))
        }
        SelectedText | UnemphasizedSelectedText => {
            (Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0xffffff, 1.0)))
        }
        TextInsertionPoint => (Accent(1.0), Accent(1.0), Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0xffffff, 1.0))),
        SelectedMenuItemText | AlternateSelectedControlText | SelectedControlText => {
            (Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x000000, 1.0)))
        }
        Link => {
            (Fixed(rgb(0x1c71d8, 1.0)), Fixed(rgb(0x78aeed, 1.0)), Fixed(rgb(0x0b4a9e, 1.0)), Fixed(rgb(0x99c1f1, 1.0)))
        }
        // Backgrounds: GNOME's window and view grays.
        WindowBackground => {
            (Fixed(rgb(0xfafafa, 1.0)), Fixed(rgb(0x242424, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x000000, 1.0)))
        }
        UnderPageBackground => {
            (Fixed(rgb(0xebebeb, 1.0)), Fixed(rgb(0x1c1c1c, 1.0)), Fixed(rgb(0xe0e0e0, 1.0)), Fixed(rgb(0x101010, 1.0)))
        }
        ControlBackground | TextBackground | AlternatingContentBackground => {
            (Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x1e1e1e, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x000000, 1.0)))
        }
        AlternatingRowBackground => {
            (Fixed(rgb(0xf4f4f5, 1.0)), Fixed(rgb(0x2a2a2b, 1.0)), Fixed(rgb(0xebebeb, 1.0)), Fixed(rgb(0x1a1a1a, 1.0)))
        }
        ScrubberTexturedBackground => {
            (Fixed(rgb(0xdeddda, 1.0)), Fixed(rgb(0x3d3846, 1.0)), Fixed(rgb(0xc0bfbc, 1.0)), Fixed(rgb(0x241f31, 1.0)))
        }
        Separator => (
            Fixed(rgb(0x000000, 0.12)),
            Fixed(rgb(0xffffff, 0.14)),
            Fixed(rgb(0x000000, 0.5)),
            Fixed(rgb(0xffffff, 0.5)),
        ),
        Grid => {
            (Fixed(rgb(0xe6e6e6, 1.0)), Fixed(rgb(0x3a3a3a, 1.0)), Fixed(rgb(0x9a9996, 1.0)), Fixed(rgb(0x77767b, 1.0)))
        }
        // Selection follows the accent.
        ControlAccent | SelectedContentBackground | SelectedMenuItem | AlternateSelectedControl => {
            (Accent(1.0), Accent(1.0), Accent(1.0), Accent(1.0))
        }
        KeyboardFocusIndicator => (Accent(0.5), Accent(0.5), Accent(1.0), Accent(1.0)),
        SelectedTextBackground | SelectedControl => {
            (AccentMuted(0.65), AccentMuted(0.55), AccentMuted(0.4), AccentMuted(0.35))
        }
        UnemphasizedSelectedContentBackground | UnemphasizedSelectedTextBackground | SecondarySelectedControl => {
            (Fixed(rgb(0xdcdcdc, 1.0)), Fixed(rgb(0x464646, 1.0)), Fixed(rgb(0xc0bfbc, 1.0)), Fixed(rgb(0x5e5c64, 1.0)))
        }
        FindHighlight => {
            (Fixed(rgb(0xf6d32d, 1.0)), Fixed(rgb(0xe5a50a, 1.0)), Fixed(rgb(0xf5c211, 1.0)), Fixed(rgb(0xe5a50a, 1.0)))
        }
        // Controls of the old appearance.
        Control | Header => {
            (Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x3a3a3a, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x000000, 1.0)))
        }
        ControlHighlight | Highlight => {
            (Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x5e5c64, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0xffffff, 1.0)))
        }
        ControlLightHighlight => {
            (Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x77767b, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0xffffff, 1.0)))
        }
        ControlShadow | WindowFrame => {
            (Fixed(rgb(0x9a9996, 1.0)), Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0x5e5c64, 1.0)), Fixed(rgb(0x000000, 1.0)))
        }
        ControlDarkShadow | Shadow => {
            (Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0x000000, 1.0)))
        }
        ScrollBar => {
            (Fixed(rgb(0xf6f5f4, 1.0)), Fixed(rgb(0x303030, 1.0)), Fixed(rgb(0xffffff, 1.0)), Fixed(rgb(0x000000, 1.0)))
        }
        Knob => {
            (Fixed(rgb(0x000000, 0.4)), Fixed(rgb(0xffffff, 0.4)), Fixed(rgb(0x000000, 0.8)), Fixed(rgb(0xffffff, 0.8)))
        }
        SelectedKnob => {
            (Fixed(rgb(0x000000, 0.6)), Fixed(rgb(0xffffff, 0.6)), Fixed(rgb(0x000000, 1.0)), Fixed(rgb(0xffffff, 1.0)))
        }
        // GNOME's palette of hues, a shade lighter on dark backgrounds.
        Red => hue(0xe01b24, 0xf66151),
        Green => hue(0x2ec27e, 0x57e389),
        Blue => hue(0x3584e4, 0x62a0ea),
        Orange => hue(0xff7800, 0xffa348),
        Yellow => hue(0xf5c211, 0xf8e45c),
        Brown => hue(0x986a44, 0xb5835a),
        Pink => hue(0xe0548f, 0xf283b0),
        Purple => hue(0x9141ac, 0xc061cb),
        Gray => hue(0x8e8d8a, 0x9a9996),
        Teal => hue(0x2190a4, 0x33c7de),
        Indigo => hue(0x4e4ab5, 0x7b77e0),
        Mint => hue(0x26a269, 0x5ed6a8),
        Cyan => hue(0x1a9fc4, 0x4fd0f0),
        // Fills for shapes over content, fading like the labels.
        Fill => fill(0.16),
        SecondaryFill => fill(0.12),
        TertiaryFill => fill(0.08),
        QuaternaryFill => fill(0.05),
        QuinaryFill => fill(0.03),
    };
    match look {
        Look::Light => light,
        Look::Dark => dark,
        Look::LightContrast => lhc,
        Look::DarkContrast => dhc,
    }
}

fn hue(light: u32, dark: u32) -> (Source, Source, Source, Source) {
    (Fixed(rgb(light, 1.0)), Fixed(rgb(dark, 1.0)), Fixed(rgb(light, 1.0)), Fixed(rgb(dark, 1.0)))
}

fn fill(a: f32) -> (Source, Source, Source, Source) {
    (Fixed(rgb(0x000000, a)), Fixed(rgb(0xffffff, a)), Fixed(rgb(0x000000, a * 2.0)), Fixed(rgb(0xffffff, a * 2.0)))
}

/// The accent when the desktop names none: GNOME's blue.
pub const DEFAULT_ACCENT: Color = rgb(0x3584e4, 1.0);

/// Every system color in every look, worked out once per accent.
struct Palette {
    accent: Color,
    tables: [Vec<Color>; 4],
}

static PALETTE: RwLock<Option<Palette>> = RwLock::new(None);

fn build(accent: Color) -> Palette {
    let table = |look: Look| {
        let background = color_in(System::ControlBackground, look, accent);
        System::ALL.iter().map(|&c| color_in(c, look, accent).unwrap_or(background.unwrap_or([0.0; 4]))).collect()
    };
    Palette {
        accent,
        tables: [table(Look::Light), table(Look::Dark), table(Look::LightContrast), table(Look::DarkContrast)],
    }
}

fn color_in(c: System, look: Look, accent: Color) -> Option<Color> {
    Some(match source(c, look) {
        Fixed(color) => color,
        Accent(a) => [accent[0], accent[1], accent[2], a],
        AccentMuted(t) => {
            let bg = if look.dark() { rgb(0x1e1e1e, 1.0) } else { rgb(0xffffff, 1.0) };
            let mix = |i: usize| accent[i] + (bg[i] - accent[i]) * t;
            [mix(0), mix(1), mix(2), 1.0]
        }
    })
}

/// `c` in `look`.
pub fn get(c: System, look: Look) -> Color {
    if let Ok(p) = PALETTE.read()
        && let Some(p) = p.as_ref()
    {
        return p.tables[look.index()][c as usize];
    }
    let mut p = PALETTE.write().unwrap_or_else(|e| e.into_inner());
    let p = p.get_or_insert_with(|| build(DEFAULT_ACCENT));
    p.tables[look.index()][c as usize]
}

/// What `c` turns into on an emphasized background (a selected row of a
/// key window's focused table, a selected source-list item), where cells
/// draw light on the accent: the label colors, and the text colors that
/// stand in for them, become the text color for selections at their own
/// strength, and the disabled text color an opaque light gray; every other
/// color stays (`None`). macOS maps the same set, by the color itself: a
/// color made from one of them, by a dynamic provider or as components,
/// isn't mapped (`conformance/tests/cell_backgrounds.rs`,
/// `emphasized_text`). The strengths are Sidestep's.
pub fn emphasized(c: System, look: Look) -> Option<Color> {
    use System::*;
    let contrast = look.contrast();
    let ink = get(AlternateSelectedControlText, look);
    // The alpha in the usual looks, and in the high-contrast ones.
    let (plain, strong) = match c {
        Label
        | ControlText
        | HeaderText
        | WindowFrameText
        | SelectedControlText
        | AlternateSelectedControlText
        | SelectedMenuItemText => (1.0, 1.0),
        SecondaryLabel => (0.75, 0.85),
        TertiaryLabel => (0.45, 0.65),
        QuaternaryLabel => (0.25, 0.45),
        QuinaryLabel => (0.12, 0.25),
        // Opaque, and a little toward gray from the ink (white ink comes
        // to about macOS's 82%).
        DisabledControlText => {
            let t = if contrast { 0.2 } else { 0.36 };
            let v = ink.map(|x| x + (0.5 - x) * t);
            return Some([v[0], v[1], v[2], 1.0]);
        }
        _ => return None,
    };
    Some([ink[0], ink[1], ink[2], if contrast { strong } else { plain }])
}

/// `c` in `look`, as it is on an emphasized background if `emphasized`
/// (see [`emphasized`]).
pub fn get_on(c: System, look: Look, emphasized: bool) -> Color {
    let mapped = if emphasized { self::emphasized(c, look) } else { None };
    mapped.unwrap_or_else(|| get(c, look))
}

/// The desktop's accent changed (or was first read); `None` for the
/// default.
pub fn set_accent(accent: Option<Color>) -> bool {
    let accent = accent.unwrap_or(DEFAULT_ACCENT);
    let mut p = PALETTE.write().unwrap_or_else(|e| e.into_inner());
    if p.as_ref().is_some_and(|p| p.accent == accent) {
        return false;
    }
    *p = Some(build(accent));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOOKS: [Look; 4] = [Look::Light, Look::Dark, Look::LightContrast, Look::DarkContrast];

    fn luminance(c: Color) -> f32 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }

    /// `c` over `bg`, opaque.
    fn over(c: Color, bg: Color) -> Color {
        [0, 1, 2]
            .map(|i| c[i] * c[3] + bg[i] * (1.0 - c[3]))
            .into_iter()
            .chain([1.0])
            .collect::<Vec<_>>()
            .try_into()
            .unwrap()
    }

    fn contrast(a: Color, b: Color) -> f32 {
        (luminance(a) - luminance(b)).abs()
    }

    #[test]
    fn every_system_color_resolves_in_every_look() {
        for look in LOOKS {
            for &c in System::ALL {
                let v = get(c, look);
                assert!(v.iter().all(|x| (0.0..=1.0).contains(x)), "{c:?} in {look:?}: {v:?}");
                assert!(v[3] > 0.0, "{c:?} in {look:?} is invisible");
            }
        }
    }

    #[test]
    fn labels_contrast_with_backgrounds_and_fade_down_the_hierarchy() {
        use System::*;
        for look in LOOKS {
            let bg = get(WindowBackground, look);
            let label = over(get(Label, look), bg);
            assert_eq!(luminance(label) < 0.5, !look.dark(), "{look:?}");
            assert_eq!(luminance(bg) < 0.5, look.dark(), "{look:?}");
            let hierarchy = [Label, SecondaryLabel, TertiaryLabel, QuaternaryLabel, QuinaryLabel];
            let contrasts: Vec<f32> = hierarchy.iter().map(|&c| contrast(over(get(c, look), bg), bg)).collect();
            assert!(contrasts.windows(2).all(|w| w[0] > w[1]), "{look:?}: {contrasts:?}");
            // High contrast is higher.
            if look.contrast() {
                let normal = if look.dark() { Look::Dark } else { Look::Light };
                let plain = contrast(
                    over(get(SecondaryLabel, normal), get(WindowBackground, normal)),
                    get(WindowBackground, normal),
                );
                assert!(contrasts[1] > plain, "{look:?}");
            }
        }
    }

    #[test]
    fn emphasized_labels_stand_out_on_the_selection_and_keep_their_hierarchy() {
        use System::*;
        for look in LOOKS {
            let bg = get(SelectedContentBackground, look);
            let hierarchy = [Label, SecondaryLabel, TertiaryLabel, QuaternaryLabel, QuinaryLabel];
            let on: Vec<Color> = hierarchy.iter().map(|&c| emphasized(c, look).expect("a label maps")).collect();
            let contrasts: Vec<f32> = on.iter().map(|&c| contrast(over(c, bg), bg)).collect();
            assert!(contrasts.windows(2).all(|w| w[0] > w[1]), "{look:?}: {contrasts:?}");
            assert_eq!(emphasized(ControlText, look), emphasized(Label, look));
            // Disabled text is opaque there, and weaker than a label.
            let disabled = emphasized(DisabledControlText, look).expect("disabled text maps");
            assert_eq!(disabled[3], 1.0, "{look:?}");
            assert!(contrast(disabled, bg) < contrasts[0], "{look:?}");
            // Colors that aren't text for labels stay.
            for c in [Text, PlaceholderText, Link, Red, ControlAccent, WindowBackground] {
                assert_eq!(emphasized(c, look), None, "{c:?}");
            }
        }
    }

    #[test]
    fn selection_follows_the_accent() {
        let green = rgb(0x2ec27e, 1.0);
        assert!(set_accent(Some(green)));
        assert_eq!(get(System::ControlAccent, Look::Light), green);
        assert_eq!(get(System::SelectedContentBackground, Look::Dark), green);
        assert!(!set_accent(Some(green)), "no change");
        assert!(set_accent(None));
        assert_eq!(get(System::ControlAccent, Look::Light), DEFAULT_ACCENT);
    }
}
