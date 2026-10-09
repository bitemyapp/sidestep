//! Glyph outlines as paths, for text drawn where the render thread's glyph
//! cache can't take it (turned, mirrored or stretched) and for programs
//! that ask for a glyph's path.

use kurbo::{BezPath, Point};
use parley::FontData;
use skrifa::MetadataProvider;
use skrifa::outline::OutlinePen;

use super::fonts;

/// A face's normalized variation coordinates, as layout gives a run's.
pub fn coords_of(face: &fonts::Face) -> Vec<i16> {
    let Some(data) = face.font.as_ref() else { return Vec::new() };
    let Ok(font) = skrifa::FontRef::from_index(data.data.data(), data.index) else { return Vec::new() };
    let location = font.axes().location(face.variations.iter().copied());
    location.coords().iter().map(|c| c.to_bits()).collect()
}

/// A pen building a kurbo path, points scaled by `scale`.
struct PathPen {
    path: BezPath,
    scale: f64,
}

impl OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(self.p(x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(self.p(x, y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.path.quad_to(self.p(cx0, cy0), self.p(x, y));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.path.curve_to(self.p(cx0, cy0), self.p(cx1, cy1), self.p(x, y));
    }

    fn close(&mut self) {
        self.path.close_path();
    }
}

impl PathPen {
    fn p(&self, x: f32, y: f32) -> Point {
        Point::new(f64::from(x) * self.scale, f64::from(y) * self.scale)
    }
}

/// A glyph's outline in points at `scale` points per font unit, y up; none
/// for a glyph with nothing to draw.
pub fn outline(font: &FontData, coords: &[i16], glyph: u16, scale: f64) -> Option<BezPath> {
    let f = skrifa::FontRef::from_index(font.data.data(), font.index).ok()?;
    let outlines = f.outline_glyphs();
    let g = outlines.get(skrifa::GlyphId::new(u32::from(glyph)))?;
    let coords: Vec<skrifa::instance::NormalizedCoord> =
        coords.iter().map(|&c| skrifa::instance::NormalizedCoord::from_bits(c)).collect();
    let location = skrifa::instance::LocationRef::new(&coords);
    let mut pen = PathPen { path: BezPath::new(), scale };
    g.draw(skrifa::outline::DrawSettings::unhinted(skrifa::instance::Size::unscaled(), location), &mut pen).ok()?;
    (!pen.path.elements().is_empty()).then_some(pen.path)
}

/// Units per em of a face's file.
pub fn units_per_em(font: &FontData) -> f64 {
    skrifa::FontRef::from_index(font.data.data(), font.index)
        .ok()
        .and_then(|f| {
            use skrifa::raw::TableProvider;
            f.head().ok().map(|h| f64::from(h.units_per_em()))
        })
        .unwrap_or(1000.0)
}
