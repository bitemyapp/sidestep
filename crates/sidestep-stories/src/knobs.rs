//! What a story is shown in: the appearance, the scale and the accent.

use std::fmt;

use sidestep_ui::{Appearance, Color};

/// The settings a story is rendered in. A story is checked in each of its
/// variants, and a golden image is kept for each, named by [`Knobs::id`]:
/// `light@1x`, `dark@2x`, `dark-hc@1x`, `light+e66100@1.5x`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Knobs {
    pub appearance: Appearance,
    /// Pixels a point. A program's window (an app story) takes a whole
    /// number only; a drawing takes any.
    pub scale: f64,
    /// The desktop's accent color, 0xrrggbb; `None` for Sidestep's default.
    pub accent: Option<u32>,
}

impl Default for Knobs {
    fn default() -> Knobs {
        Knobs::LIGHT
    }
}

impl Knobs {
    pub const LIGHT: Knobs = Knobs { appearance: Appearance::LIGHT, scale: 1.0, accent: None };
    pub const DARK: Knobs = Knobs { appearance: Appearance::DARK, scale: 1.0, accent: None };

    /// These knobs at `scale`.
    pub fn at(self, scale: f64) -> Knobs {
        Knobs { scale, ..self }
    }

    /// These knobs in high contrast.
    pub fn high_contrast(self) -> Knobs {
        Knobs { appearance: Appearance { high_contrast: true, ..self.appearance }, ..self }
    }

    /// These knobs with the accent `rgb` (0xrrggbb).
    pub fn accent(self, rgb: u32) -> Knobs {
        Knobs { accent: Some(rgb & 0xff_ffff), ..self }
    }

    /// The accent as a color, if one is set.
    pub fn accent_color(&self) -> Option<Color> {
        self.accent.map(Color::hex)
    }

    /// The name of these knobs, as golden images are named: `light@1x`,
    /// `dark-hc@2x`, `light+e66100@1.5x`.
    pub fn id(&self) -> String {
        self.to_string()
    }

    /// Knobs from their [`id`](Knobs::id).
    pub fn parse(id: &str) -> Option<Knobs> {
        let (look, scale) = id.split_once('@')?;
        let scale: f64 = scale.strip_suffix('x')?.parse().ok()?;
        if !(scale.is_finite() && (0.25..=8.0).contains(&scale)) {
            return None;
        }
        let (look, accent) = match look.split_once('+') {
            Some((look, hex)) if hex.len() == 6 => (look, Some(u32::from_str_radix(hex, 16).ok()?)),
            Some(_) => return None,
            None => (look, None),
        };
        let appearance = match look {
            "light" => Appearance::LIGHT,
            "dark" => Appearance::DARK,
            "light-hc" => Appearance { dark: false, high_contrast: true },
            "dark-hc" => Appearance { dark: true, high_contrast: true },
            _ => return None,
        };
        Some(Knobs { appearance, scale, accent })
    }
}

impl fmt::Display for Knobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.appearance.dark { "dark" } else { "light" })?;
        if self.appearance.high_contrast {
            f.write_str("-hc")?;
        }
        if let Some(rgb) = self.accent {
            write!(f, "+{rgb:06x}")?;
        }
        write!(f, "@{}x", self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for (knobs, id) in [
            (Knobs::LIGHT, "light@1x"),
            (Knobs::DARK.at(2.0), "dark@2x"),
            (Knobs::DARK.high_contrast(), "dark-hc@1x"),
            (Knobs::LIGHT.accent(0xe66100).at(1.5), "light+e66100@1.5x"),
        ] {
            assert_eq!(knobs.id(), id);
            assert_eq!(Knobs::parse(id), Some(knobs), "{id}");
        }
    }

    #[test]
    fn bad_ids_are_refused() {
        for id in ["", "light", "light@", "light@2", "grey@1x", "light+fff@1x", "light@0x", "light@nanx"] {
            assert_eq!(Knobs::parse(id), None, "{id}");
        }
    }
}
