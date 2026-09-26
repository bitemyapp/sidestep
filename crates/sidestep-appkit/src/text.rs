//! Fonts. For now one sans and one monospaced face, found on disk and
//! rasterized with fontdue; shaping and fallback (parley) come later.

use std::sync::OnceLock;

pub(crate) struct Fonts {
    sans: fontdue::Font,
    mono: fontdue::Font,
}

const SANS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
];
const MONO: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansMono-Regular.ttf",
];

fn load(env: &str, candidates: &[&str]) -> fontdue::Font {
    let paths = std::env::var(env).ok().into_iter().chain(candidates.iter().map(|s| s.to_string()));
    for path in paths {
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(font) = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                return font;
            }
        }
    }
    panic!("sidestep: no usable font found; set {env} to a .ttf file (tried {candidates:?})");
}

pub(crate) fn fonts() -> &'static Fonts {
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    FONTS.get_or_init(|| Fonts { sans: load("SIDESTEP_FONT", SANS), mono: load("SIDESTEP_MONO_FONT", MONO) })
}

impl Fonts {
    pub fn face(&self, mono: bool) -> &fontdue::Font {
        if mono { &self.mono } else { &self.sans }
    }

    /// Ascent and line height at `size`.
    pub fn line_metrics(&self, mono: bool, size: f32) -> (f32, f32) {
        match self.face(mono).horizontal_line_metrics(size) {
            Some(m) => (m.ascent, m.new_line_size),
            None => (size * 0.8, size * 1.2),
        }
    }

    pub fn width(&self, mono: bool, size: f32, text: &str) -> f32 {
        let face = self.face(mono);
        text.chars().map(|c| face.metrics(c, size).advance_width).sum()
    }
}
