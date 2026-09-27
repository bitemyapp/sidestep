//! Glyphs drawn into a CGContext's graphics state, as CoreText and
//! CoreGraphics draw them (measured on macOS): a glyph at position `p` has
//! its origin at `p` through the text matrix (whose translation is the
//! text position), its outline scaled by the size over the units per em,
//! y up, through the same matrix, then the CTM. Glyphs take the fill
//! color, or a line's own colors.
//!
//! Where that transform keeps glyphs upright on the layer, scaled the same
//! both ways (a view's usual drawing, with or without a flip in the CTM
//! that the text matrix undoes), the glyphs become the same
//! [`Op::Glyphs`](crate::protocol::Op) string drawing records, from the
//! render thread's glyph cache; the size is the font's times that scale.
//! Otherwise (text turned, mirrored or stretched), and whenever what the
//! glyph op can't carry is asked for (a shadow, a blend mode, stroked or
//! clipping text modes), the glyphs are drawn as their outlines, paths
//! filled or stroked with the graphics state like any other.
//!
//! [`outline`] gives a glyph's outline as a path in points (y up), which
//! `CTFontCreatePathForGlyph` hands out too.

use std::sync::Arc;

use kurbo::{Affine, BezPath, Point};
use objc2::DefinedClass;
use objc2_app_kit::NSFont;
use objc2_core_graphics::{CGContext, CGTextDrawingMode};
use parley::FontData;
use skrifa::MetadataProvider;
use skrifa::outline::OutlinePen;

use crate::context::{ContextState, to_skia};
use crate::protocol::{Color, Draw, Glyph, GlyphRun, Op, Paint, StrokeSpec};
use crate::text::fonts::{self, Synth};

/// A face at a size, as glyphs are drawn in it.
pub(crate) struct GlyphFace<'a> {
    pub font: &'a FontData,
    pub coords: &'a [i16],
    pub synth: Synth,
    pub size: f64,
}

/// How the glyphs are painted: fill (and stroke) colors, and a stroke
/// width in points for outlined text (0 for none).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Paints {
    pub fill: Option<Color>,
    pub stroke: Option<(Color, f64)>,
}

/// A face's normalized variation coordinates, as layout gives a run's.
pub(crate) fn coords_of(face: &fonts::Face) -> Vec<i16> {
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

/// The box of an outline's points, in font units.
#[derive(Default)]
pub(crate) struct BoundsPen {
    bounds: Option<[f64; 4]>,
}

impl BoundsPen {
    fn add(&mut self, x: f32, y: f32) {
        let (x, y) = (f64::from(x), f64::from(y));
        self.bounds = Some(match self.bounds {
            None => [x, y, x, y],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        });
    }

    pub(crate) fn bounds(&self) -> Option<[f64; 4]> {
        self.bounds.filter(|b| b[0] < b[2] || b[1] < b[3])
    }
}

impl OutlinePen for BoundsPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.add(cx0, cy0);
        self.add(x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.add(cx0, cy0);
        self.add(cx1, cy1);
        self.add(x, y);
    }

    fn close(&mut self) {}
}

/// A glyph's outline in points at `scale` points per font unit, y up; none
/// for a glyph with nothing to draw.
pub(crate) fn outline(font: &FontData, coords: &[i16], glyph: u16, scale: f64) -> Option<BezPath> {
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
pub(crate) fn units_per_em(font: &FontData) -> f64 {
    skrifa::FontRef::from_index(font.data.data(), font.index)
        .ok()
        .and_then(|f| {
            use skrifa::raw::TableProvider;
            f.head().ok().map(|h| f64::from(h.units_per_em()))
        })
        .unwrap_or(1000.0)
}

/// Whether `m` keeps glyphs upright on a y-down layer, scaled the same
/// both ways: the scale if so.
fn upright(m: Affine) -> Option<f64> {
    let [a, b, c, d, _, _] = m.as_coeffs();
    let eps = 1e-9 * a.abs().max(1.0);
    (a > 0.0 && b.abs() <= eps && c.abs() <= eps && (a + d).abs() <= 1e-6 * a).then_some(a)
}

/// Where glyph positions are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Positions {
    /// In text space, through the whole text matrix
    /// (`CTFontDrawGlyphs`, `CGContextShowGlyphsAtPositions`).
    Text,
    /// Offsets in user space from the text position, which the text
    /// matrix's scale and turn don't reach (a line's glyphs, as
    /// `CTLineDraw` places them: measured on macOS, a scaled text matrix
    /// scales the glyphs, not the gaps between them).
    Line,
}

/// Each glyph's origin in user space.
fn origins(text: Affine, positions: &[(f64, f64)], kind: Positions) -> Vec<Point> {
    let [.., tx, ty] = text.as_coeffs();
    positions
        .iter()
        .map(|&(x, y)| match kind {
            Positions::Text => text * Point::new(x, y),
            Positions::Line => Point::new(tx + x, ty + y),
        })
        .collect()
}

/// Draw `glyphs` of `face` at `positions` into `st`.
pub(crate) fn draw_glyphs(
    st: &mut ContextState,
    face: &GlyphFace<'_>,
    glyphs: &[u16],
    positions: &[(f64, f64)],
    kind: Positions,
    paints: Paints,
) {
    if glyphs.is_empty() {
        return;
    }
    let mode = st.gs.cg.text.mode;
    if mode == CGTextDrawingMode::Invisible {
        return;
    }
    let [a, b, c, d, _, _] = st.text_matrix.as_coeffs();
    let shape = Affine::new([a, b, c, d, 0.0, 0.0]);
    let origins = origins(st.text_matrix, positions, kind);
    let fills = matches!(
        mode,
        CGTextDrawingMode::Fill
            | CGTextDrawingMode::FillStroke
            | CGTextDrawingMode::FillClip
            | CGTextDrawingMode::FillStrokeClip
    );
    let strokes = matches!(
        mode,
        CGTextDrawingMode::Stroke
            | CGTextDrawingMode::FillStroke
            | CGTextDrawingMode::StrokeClip
            | CGTextDrawingMode::FillStrokeClip
    );
    let clips = matches!(
        mode,
        CGTextDrawingMode::FillClip
            | CGTextDrawingMode::StrokeClip
            | CGTextDrawingMode::FillStrokeClip
            | CGTextDrawingMode::Clip
    );
    let plain = mode == CGTextDrawingMode::Fill
        && paints.stroke.is_none()
        && st.gs.shadow.is_none()
        && st.gs.blend == crate::protocol::Blend::SourceOver;
    if plain && let (Some(scale), Some(fill)) = (upright(st.gs.ctm * shape), paints.fill) {
        let layer: Vec<Point> = origins.iter().map(|&o| st.gs.ctm * o).collect();
        push_run(st, face, glyphs, &layer, scale, fill);
        return;
    }
    // Outlines: the glyphs' paths in user space.
    let upem = units_per_em(face.font);
    let skew = if face.synth.skew != 0.0 {
        Affine::skew(f64::from(face.synth.skew).to_radians().tan(), 0.0)
    } else {
        Affine::IDENTITY
    };
    let mut path = BezPath::new();
    for (&g, &o) in glyphs.iter().zip(&origins) {
        if let Some(mut outline) = outline(face.font, face.coords, g, face.size / upem) {
            outline.apply_affine(Affine::translate(o.to_vec2()) * shape * skew);
            path.extend(outline.elements().iter().copied());
        }
    }
    let Some(skia) = crate::coregraphics::path::Shape::from_path(path).drawn() else { return };
    let draw: Draw = st.gs.draw();
    if fills && let Some(fill) = paints.fill.or((mode != CGTextDrawingMode::Fill).then_some(st.gs.fill)) {
        st.push(Op::FillPath { path: skia.clone(), even_odd: false, paint: Paint::Solid(fill), draw: draw.clone() });
    }
    let stroke = match paints.stroke {
        Some((color, width)) => Some((color, width)),
        None if strokes => Some((st.gs.stroke, st.gs.cg.line.drawn_width())),
        None => None,
    };
    if let Some((color, width)) = stroke {
        let spec = if paints.stroke.is_some() {
            StrokeSpec { width: width as f32, cap: 0, join: 0, miter: 10.0, dash: None }
        } else {
            st.gs.cg.line.spec()
        };
        st.push(Op::StrokePath { path: skia.clone(), stroke: Arc::new(spec), paint: Paint::Solid(color), draw });
    }
    if clips {
        st.clip_path(skia, false, false);
    }
}

/// Record glyphs whose origins are at `layer` points as a glyph run: the
/// render thread's cache draws them.
fn push_run(st: &mut ContextState, face: &GlyphFace<'_>, glyphs: &[u16], layer: &[Point], scale: f64, color: Color) {
    let id = fonts::register(face.font, face.coords, face.synth);
    let origin = layer[0];
    let run: Arc<[Glyph]> = glyphs
        .iter()
        .zip(layer)
        .map(|(&g, p)| Glyph { id: u32::from(g), x: (p.x - origin.x) as f32, y: (p.y - origin.y) as f32 })
        .collect();
    let op = Op::Glyphs(GlyphRun {
        font: id,
        size: (face.size * scale) as f32,
        x: origin.x as f32,
        y: origin.y as f32,
        glyphs: run,
        color,
        clip: st.gs.clip,
    });
    // A clip that isn't a rectangle masks a group the glyphs go in, as
    // string drawing's do.
    st.masked(|st| st.push(op));
}

/// Fill a rectangle of a line (x, y, width, height from the line's
/// origin, y up) with `color`: its backgrounds and underlines. Across, it
/// is placed as the line's glyphs are ([`Positions::Line`]); up and down,
/// through the text matrix, as their shapes are.
pub(crate) fn fill_text_rect(st: &mut ContextState, rect: [f64; 4], color: Color) {
    let [x, y, w, h] = rect;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let [a, b, c, d, tx, ty] = st.text_matrix.as_coeffs();
    let up = Affine::new([a, b, c, d, 0.0, 0.0]);
    let corner = |px: f64, py: f64| Point::new(tx + px, ty) + (up * Point::new(0.0, py)).to_vec2();
    let mut path = tiny_skia::PathBuilder::new();
    let points = [corner(x, y), corner(x + w, y), corner(x + w, y + h), corner(x, y + h)];
    path.move_to(points[0].x as f32, points[0].y as f32);
    for p in &points[1..] {
        path.line_to(p.x as f32, p.y as f32);
    }
    path.close();
    let Some(path) = path.finish() else { return };
    let mut draw = st.gs.draw();
    draw.xf = to_skia(st.gs.ctm);
    st.push(Op::FillPath { path: Arc::new(path), even_odd: false, paint: Paint::Solid(color), draw });
}

/// `CTFontDrawGlyphs`: `font`'s glyphs at `positions` in the fill color.
pub(crate) fn draw_font_glyphs(
    context: &CGContext,
    font: &NSFont,
    glyphs: &[u16],
    positions: &[objc2_core_foundation::CGPoint],
) {
    let (spec, face) = crate::font::parts(font);
    let Some(data) = face.font.clone() else { return };
    let coords = coords_of(face);
    let synth = Synth::default();
    let points: Vec<(f64, f64)> = positions.iter().map(|p| (p.x, p.y)).collect();
    let size = spec.size;
    crate::coregraphics::context::with_state(context, |st| {
        let fill = st.gs.fill;
        let face = GlyphFace { font: &data, coords: &coords, synth, size };
        draw_glyphs(st, &face, glyphs, &points, Positions::Text, Paints { fill: Some(fill), stroke: None });
    });
}

/// CoreGraphics' own glyph drawing: the context's font (`CGContextSetFont`)
/// at its size, glyphs at `positions` in text space.
pub(crate) fn draw_cg_glyphs(st: &mut ContextState, glyphs: &[u16], positions: &[(f64, f64)]) {
    let Some(font) = st.gs.cg.text.font.clone() else { return };
    let size = st.gs.cg.text.size;
    let data = font.ivars().data.clone();
    let fill = st.gs.fill;
    let face = GlyphFace { font: &data, coords: &[], synth: Synth::default(), size };
    let mode = st.gs.cg.text.mode;
    let paints = Paints { fill: (mode != CGTextDrawingMode::Stroke).then_some(fill), stroke: None };
    draw_glyphs(st, &face, glyphs, positions, Positions::Text, paints);
}

/// The advances of the context's font's glyphs in text space (with the
/// character spacing), for CoreGraphics' drawing at the text position.
pub(crate) fn cg_advances(st: &ContextState, glyphs: &[u16]) -> Vec<f64> {
    let Some(font) = st.gs.cg.text.font.as_ref() else { return vec![0.0; glyphs.len()] };
    let data = &font.ivars().data;
    let size = st.gs.cg.text.size;
    let spacing = st.gs.cg.text.spacing;
    let Ok(f) = skrifa::FontRef::from_index(data.data.data(), data.index) else { return vec![0.0; glyphs.len()] };
    let upem = units_per_em(data);
    let metrics = f.glyph_metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
    glyphs
        .iter()
        .map(|&g| {
            let a = f64::from(metrics.advance_width(skrifa::GlyphId::new(u32::from(g))).unwrap_or(0.0));
            a * size / upem + spacing
        })
        .collect()
}

/// The context's font's glyphs for Mac Roman text (`CGContextShowText`).
pub(crate) fn cg_glyphs_for_text(st: &ContextState, text: &[u8]) -> Vec<u16> {
    let Some(font) = st.gs.cg.text.font.as_ref() else { return Vec::new() };
    let data = &font.ivars().data;
    let Ok(f) = skrifa::FontRef::from_index(data.data.data(), data.index) else { return Vec::new() };
    let charmap = f.charmap();
    let decoded = sidestep_foundation::decode_mac_roman(text);
    decoded.chars().map(|c| charmap.map(c).map_or(0, |g| g.to_u32() as u16)).collect()
}
