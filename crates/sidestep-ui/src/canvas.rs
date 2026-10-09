//! Drawing: a [`Canvas`] records what a window draws as the render thread's
//! drawing ops (fills, paths, strokes, images, glyph runs), which the
//! render thread rasterizes at the window's scale into the parts of the
//! window that changed. Nothing here touches pixels.
//!
//! Coordinates are points from the window content's top left, y down,
//! through the canvas's current transform. Each pass starts with the
//! damaged parts of the window cleared to the window's background.

use std::sync::Arc;

use kurbo::{Affine, BezPath, Point, Rect, Shape, Vec2};
use sidestep_engine::path::to_skia;
use sidestep_engine::protocol::{
    self, Blend, ClipPath, Draw, GlyphRun, GradientSpec, Op, Quality, ShadowSpec, StrokeSpec,
};
use sidestep_engine::text::fonts;
use sidestep_engine::text::layout::{PlacedFill, PlacedRun};
use sidestep_engine::text::outline;

use crate::color::{Appearance, Color, SystemColor};
use crate::image::Image;
use crate::text::{TextLayout, TextStyle};

/// What a shape is filled or stroked with.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Color),
    Linear(LinearGradient),
    Radial(RadialGradient),
}

impl From<Color> for Paint {
    fn from(c: Color) -> Paint {
        Paint::Solid(c)
    }
}

impl From<LinearGradient> for Paint {
    fn from(g: LinearGradient) -> Paint {
        Paint::Linear(g)
    }
}

impl From<RadialGradient> for Paint {
    fn from(g: RadialGradient) -> Paint {
        Paint::Radial(g)
    }
}

/// Colors along a line from `start` to `end`, the end colors carried on
/// past them. `stops` are positions from 0 (start) to 1 (end), in order.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    pub start: Point,
    pub end: Point,
    pub stops: Vec<(f32, Color)>,
}

impl LinearGradient {
    pub fn new(start: Point, end: Point, from: Color, to: Color) -> LinearGradient {
        LinearGradient { start, end, stops: vec![(0.0, from), (1.0, to)] }
    }
}

/// Colors between two circles: the first stop on the circle around
/// `start_center` of `start_radius`, the last on the one around `center`
/// of `radius` (and beyond it).
#[derive(Clone, Debug, PartialEq)]
pub struct RadialGradient {
    pub start_center: Point,
    pub start_radius: f64,
    pub center: Point,
    pub radius: f64,
    pub stops: Vec<(f32, Color)>,
}

impl RadialGradient {
    /// From `inner` at `center` out to `outer` at `radius` from it.
    pub fn new(center: Point, radius: f64, inner: Color, outer: Color) -> RadialGradient {
        RadialGradient {
            start_center: center,
            start_radius: 0.0,
            center,
            radius,
            stops: vec![(0.0, inner), (1.0, outer)],
        }
    }
}

/// How a line ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

/// How lines meet at a corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

/// How a shape's outline is stroked.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    /// In points; 0 draws the thinnest line the screen can show.
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    /// Lengths of dashes and gaps, in turn, and how far into them to start.
    pub dash: Option<(Vec<f64>, f64)>,
}

impl StrokeStyle {
    pub fn new(width: f64) -> StrokeStyle {
        StrokeStyle { width, cap: LineCap::Butt, join: LineJoin::Miter, miter_limit: 10.0, dash: None }
    }

    pub fn with_cap(mut self, cap: LineCap) -> StrokeStyle {
        self.cap = cap;
        self
    }

    pub fn with_join(mut self, join: LineJoin) -> StrokeStyle {
        self.join = join;
        self
    }

    pub fn with_dash(mut self, lengths: Vec<f64>, offset: f64) -> StrokeStyle {
        self.dash = Some((lengths, offset));
        self
    }

    fn spec(&self) -> StrokeSpec {
        StrokeSpec {
            width: self.width.max(0.0) as f32,
            // NSLineCapStyle and NSLineJoinStyle values.
            cap: self.cap as u8,
            join: self.join as u8,
            miter: self.miter_limit.max(1.0) as f32,
            dash: self
                .dash
                .as_ref()
                .filter(|(l, _)| !l.is_empty())
                .map(|(l, o)| (l.iter().map(|&v| v as f32).collect(), *o as f32)),
        }
    }
}

/// A shadow cast by what's drawn while it is set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    /// In points, y down.
    pub offset: Vec2,
    /// The blur's radius, in points.
    pub blur: f64,
    pub color: Color,
}

/// How a drawing combines with what's under it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlendMode {
    /// Over what's there.
    #[default]
    Normal,
    /// In place of what's there, transparency included.
    Copy,
    /// Erase what's there where the drawing covers.
    Clear,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    /// Add the colors.
    Plus,
}

impl BlendMode {
    fn blend(self) -> Blend {
        match self {
            BlendMode::Normal => Blend::SourceOver,
            BlendMode::Copy => Blend::Copy,
            BlendMode::Clear => Blend::Clear,
            BlendMode::Multiply => Blend::Multiply,
            BlendMode::Screen => Blend::Screen,
            BlendMode::Overlay => Blend::Overlay,
            BlendMode::Darken => Blend::Darken,
            BlendMode::Lighten => Blend::Lighten,
            BlendMode::ColorDodge => Blend::ColorDodge,
            BlendMode::ColorBurn => Blend::ColorBurn,
            BlendMode::SoftLight => Blend::SoftLight,
            BlendMode::HardLight => Blend::HardLight,
            BlendMode::Difference => Blend::Difference,
            BlendMode::Exclusion => Blend::Exclusion,
            BlendMode::Plus => Blend::PlusLighter,
        }
    }
}

/// How an image is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageOptions {
    pub alpha: f32,
    /// Resample smoothly (`true`), or take the nearest pixel, for pixel
    /// art.
    pub smooth: bool,
    /// Draw the image's shape (its alpha) in this color: a symbol or icon
    /// made to be tinted.
    pub tint: Option<Color>,
}

impl Default for ImageOptions {
    fn default() -> ImageOptions {
        ImageOptions { alpha: 1.0, smooth: true, tint: None }
    }
}

/// The drawing state `save` keeps and `restore` brings back.
#[derive(Clone)]
struct State {
    xf: Affine,
    /// Where drawing may reach, in window points: the clip's bounds.
    clip: protocol::Rect,
    /// The clip's shape where it isn't `clip` itself.
    mask: Option<Arc<[ClipPath]>>,
    blend: BlendMode,
    shadow: Option<Shadow>,
    antialias: bool,
}

/// What a window draws in a pass: see the module's documentation.
pub struct Canvas {
    ops: Vec<Op>,
    state: State,
    saved: Vec<State>,
    groups: usize,
    size: kurbo::Size,
    scale: f64,
    damage: Vec<protocol::Rect>,
    appearance: Appearance,
}

fn prect(r: Rect) -> protocol::Rect {
    protocol::Rect::new(r.x0 as f32, r.y0 as f32, r.x1 as f32, r.y1 as f32)
}

fn krect(r: protocol::Rect) -> Rect {
    Rect::new(f64::from(r.x0), f64::from(r.y0), f64::from(r.x1), f64::from(r.y1))
}

fn skia(m: Affine) -> tiny_skia::Transform {
    let [a, b, c, d, e, f] = m.as_coeffs();
    tiny_skia::Transform::from_row(a as f32, b as f32, c as f32, d as f32, e as f32, f as f32)
}

/// The scale of `m` if it scales both ways alike without turning or
/// mirroring: glyphs stay upright through it.
fn upright(m: Affine) -> Option<f64> {
    let [a, b, c, d, _, _] = m.as_coeffs();
    let eps = 1e-9 * a.abs().max(1.0);
    (a > 0.0 && b.abs() <= eps && c.abs() <= eps && (a - d).abs() <= 1e-6 * a).then_some(a)
}

/// Whether `m` keeps rectangles axis-aligned without mirroring them.
fn axis_aligned(m: Affine) -> bool {
    let [a, b, c, d, _, _] = m.as_coeffs();
    b == 0.0 && c == 0.0 && a > 0.0 && d > 0.0
}

impl Canvas {
    pub(crate) fn new(
        size: kurbo::Size,
        scale: f64,
        damage: Vec<protocol::Rect>,
        background: Option<Color>,
        appearance: Appearance,
    ) -> Canvas {
        let bounds = protocol::Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
        let mut canvas = Canvas {
            ops: Vec::new(),
            state: State {
                xf: Affine::IDENTITY,
                clip: bounds,
                mask: None,
                blend: BlendMode::Normal,
                shadow: None,
                antialias: true,
            },
            saved: Vec::new(),
            groups: 0,
            size,
            scale,
            damage,
            appearance,
        };
        // Damaged pixels start over: as the background, or transparent.
        let ground = background.map_or([0.0; 4], Color::raw);
        for r in canvas.damage.clone() {
            canvas.ops.push(Op::FillWith { rect: r.intersect(&bounds), color: ground, blend: Blend::Copy });
        }
        canvas
    }

    /// The ops recorded, groups closed.
    pub(crate) fn finish(mut self) -> Vec<Op> {
        for _ in 0..self.groups {
            self.ops.push(Op::EndGroup);
        }
        self.ops
    }

    /// The window's content size, in points.
    pub fn size(&self) -> kurbo::Size {
        self.size
    }

    /// Device pixels per point.
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// The appearance the window draws in.
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    /// `color` in the window's appearance.
    pub fn system_color(&self, color: SystemColor) -> Color {
        self.appearance.color(color)
    }

    /// The parts of the window this pass redraws, in window points: drawing
    /// elsewhere is clipped away, so a program may skip it.
    pub fn damage(&self) -> Vec<Rect> {
        self.damage.iter().map(|r| krect(*r)).collect()
    }

    /// Whether `rect` (in the current coordinates) meets what this pass
    /// redraws.
    pub fn needs(&self, rect: Rect) -> bool {
        let r = prect(self.state.xf.transform_rect_bbox(rect)).intersect(&self.state.clip);
        !r.is_empty() && self.damage.iter().any(|d| !d.intersect(&r).is_empty())
    }

    // State.

    /// Keep the drawing state (transform, clip, blend mode, shadow) for
    /// [`restore`](Canvas::restore).
    pub fn save(&mut self) {
        self.saved.push(self.state.clone());
    }

    /// Bring back the state the matching [`save`](Canvas::save) kept.
    pub fn restore(&mut self) {
        if let Some(state) = self.saved.pop() {
            self.state = state;
        }
    }

    /// Run `f` with the drawing state saved around it.
    pub fn saved<R>(&mut self, f: impl FnOnce(&mut Canvas) -> R) -> R {
        self.save();
        let r = f(self);
        self.restore();
        r
    }

    pub fn transform(&self) -> Affine {
        self.state.xf
    }

    /// Draw through `m`, then the current transform.
    pub fn concat(&mut self, m: Affine) {
        self.state.xf *= m;
    }

    pub fn translate(&mut self, by: impl Into<Vec2>) {
        self.concat(Affine::translate(by.into()));
    }

    pub fn scale_by(&mut self, sx: f64, sy: f64) {
        self.concat(Affine::scale_non_uniform(sx, sy));
    }

    /// Turn by `radians`, clockwise on screen (y runs down).
    pub fn rotate(&mut self, radians: f64) {
        self.concat(Affine::rotate(radians));
    }

    /// Draw only inside `rect` as well.
    pub fn clip_rect(&mut self, rect: Rect) {
        if axis_aligned(self.state.xf) {
            self.state.clip = self.state.clip.intersect(&prect(self.state.xf.transform_rect_bbox(rect)));
        } else {
            self.clip(&rect);
        }
    }

    /// Draw only inside `shape` as well (nonzero winding).
    pub fn clip(&mut self, shape: &impl Shape) {
        let path = shape.to_path(0.1);
        let bounds = prect(self.state.xf.transform_rect_bbox(path.bounding_box()));
        self.state.clip = self.state.clip.intersect(&bounds);
        let Some(path) = to_skia(&path) else {
            self.state.clip = protocol::Rect::NOWHERE;
            return;
        };
        let clip = ClipPath { path, even_odd: false, xf: skia(self.state.xf), aa: self.state.antialias, image: None };
        let mut masks: Vec<ClipPath> = self.state.mask.as_deref().map(<[ClipPath]>::to_vec).unwrap_or_default();
        masks.push(clip);
        self.state.mask = Some(masks.into());
    }

    pub fn set_blend_mode(&mut self, mode: BlendMode) {
        self.state.blend = mode;
    }

    /// Cast `shadow` from what's drawn from now on (until restored), or
    /// none.
    pub fn set_shadow(&mut self, shadow: Option<Shadow>) {
        self.state.shadow = shadow;
    }

    /// Smooth shapes' edges (the default), or draw them on whole pixels.
    pub fn set_antialias(&mut self, on: bool) {
        self.state.antialias = on;
    }

    fn draw(&self) -> Draw {
        let shadow = self.state.shadow.map(|s| {
            // The offset is in points of the window, as the transform
            // doesn't reach it.
            Arc::new(ShadowSpec {
                dx: s.offset.x as f32,
                dy: s.offset.y as f32,
                blur: s.blur.max(0.0) as f32,
                color: s.color.raw(),
                only: false,
            })
        });
        Draw {
            xf: skia(self.state.xf),
            blend: self.state.blend.blend(),
            aa: self.state.antialias,
            clip: self.state.clip,
            mask: self.state.mask.clone(),
            shadow,
        }
    }

    /// Whether nothing can show: the clip is empty.
    fn clipped_out(&self) -> bool {
        self.state.clip.is_empty()
    }

    // Shapes.

    /// Fill `rect` with `color`: the device pixels whose centers it covers,
    /// with no smoothing, so rectangles side by side meet without seams.
    /// (Filled as a shape, [`fill`](Canvas::fill), a rectangle's edges are
    /// smoothed.)
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if self.clipped_out() {
            return;
        }
        let simple = axis_aligned(self.state.xf)
            && self.state.mask.is_none()
            && self.state.shadow.is_none()
            && self.state.blend == BlendMode::Normal;
        if simple {
            let r = prect(self.state.xf.transform_rect_bbox(rect)).intersect(&self.state.clip);
            if !r.is_empty() {
                self.ops.push(Op::Fill { rect: r, color: color.raw() });
            }
            return;
        }
        let antialias = std::mem::replace(&mut self.state.antialias, false);
        self.fill(&rect, color);
        self.state.antialias = antialias;
    }

    /// Fill `shape` (nonzero winding).
    pub fn fill(&mut self, shape: &impl Shape, paint: impl Into<Paint>) {
        self.fill_path(shape.to_path(0.1), false, paint.into());
    }

    /// Fill `shape` by the even-odd rule.
    pub fn fill_even_odd(&mut self, shape: &impl Shape, paint: impl Into<Paint>) {
        self.fill_path(shape.to_path(0.1), true, paint.into());
    }

    fn fill_path(&mut self, path: BezPath, even_odd: bool, paint: Paint) {
        if self.clipped_out() {
            return;
        }
        let Some(path) = to_skia(&path) else { return };
        let paint = self.paint(paint);
        self.ops.push(Op::FillPath { path, even_odd, paint, draw: self.draw() });
    }

    /// Stroke `shape`'s outline.
    pub fn stroke(&mut self, shape: &impl Shape, style: &StrokeStyle, paint: impl Into<Paint>) {
        if self.clipped_out() {
            return;
        }
        let Some(path) = to_skia(&shape.to_path(0.1)) else { return };
        let paint = self.paint(paint.into());
        self.ops.push(Op::StrokePath { path, stroke: Arc::new(style.spec()), paint, draw: self.draw() });
    }

    fn paint(&self, paint: Paint) -> protocol::Paint {
        let stops = |s: &[(f32, Color)]| s.iter().map(|&(t, c)| (t, c.raw())).collect();
        let p = |p: Point| (p.x as f32, p.y as f32);
        match paint {
            Paint::Solid(c) => protocol::Paint::Solid(c.raw()),
            Paint::Linear(g) => protocol::Paint::Gradient(Arc::new(GradientSpec {
                stops: stops(&g.stops),
                start: p(g.start),
                end: p(g.end),
                radii: None,
                extend: (true, true),
            })),
            Paint::Radial(g) => protocol::Paint::Gradient(Arc::new(GradientSpec {
                stops: stops(&g.stops),
                start: p(g.start_center),
                end: p(g.center),
                radii: Some((g.start_radius as f32, g.radius as f32)),
                extend: (true, true),
            })),
        }
    }

    // Images.

    /// Draw `image` stretched over `dst`.
    pub fn draw_image(&mut self, image: &Image, dst: Rect) {
        self.draw_image_with(image, None, dst, ImageOptions::default());
    }

    /// Draw the part `src` (in the image's pixels, from its top left) of
    /// `image`, or all of it, into `dst`.
    pub fn draw_image_with(&mut self, image: &Image, src: Option<Rect>, dst: Rect, options: ImageOptions) {
        if self.clipped_out() || dst.is_zero_area() {
            return;
        }
        let (w, h) = image.pixel_size();
        let src = src.map_or(protocol::Rect::new(0.0, 0.0, w as f32, h as f32), prect);
        self.ops.push(Op::Image {
            image: image.data().clone(),
            src,
            dst: prect(dst),
            alpha: options.alpha.clamp(0.0, 1.0),
            quality: if options.smooth { Quality::High } else { Quality::None },
            tint: options.tint.map(Color::raw),
            tiled: false,
            draw: self.draw(),
        });
    }

    // Groups.

    /// Draw what `f` draws as one layer, then composite it at `alpha`
    /// (with the blend mode and shadow set now): overlapping shapes inside
    /// don't show through each other.
    pub fn group(&mut self, alpha: f32, f: impl FnOnce(&mut Canvas)) {
        let draw = self.draw();
        self.ops.push(Op::BeginGroup { alpha: alpha.clamp(0.0, 1.0), draw });
        self.groups += 1;
        self.saved(|c| {
            c.state.shadow = None;
            c.state.blend = BlendMode::Normal;
            f(c);
        });
        self.groups -= 1;
        self.ops.push(Op::EndGroup);
    }

    // Text.

    /// Draw `layout` with its top left at `origin`.
    pub fn draw_text(&mut self, layout: &TextLayout, origin: Point) {
        if self.clipped_out() {
            return;
        }
        let width = layout.size().width.max(1.0);
        for line in layout.frame().lines() {
            let top = origin.y + f64::from(line.top());
            let height = f64::from(line.line.height);
            // Glyphs and decorations reach a little past their line's box;
            // lines nowhere near what this pass redraws are left out.
            let reach = Rect::new(origin.x, top, origin.x + width, top + height).inflate(height, height);
            if self.needs(reach) {
                self.emit_text(&line.line.runs, &line.line.fills, origin.x, top);
            }
        }
    }

    /// Draw `text` in `style` on one line per paragraph, its top left at
    /// `origin`: a label. The layout comes from the text engine's cache, so
    /// drawing the same label pass after pass lays it out once.
    pub fn draw_label(&mut self, text: &str, style: &TextStyle, origin: Point) {
        if self.clipped_out() || text.is_empty() {
            return;
        }
        let laid = crate::text::label(text, style);
        self.emit_text(&laid.runs, &laid.fills, origin.x, origin.y);
    }

    /// Glyph runs and fills placed from (`left`, `top`) in the current
    /// coordinates: backgrounds, then glyphs, then decorations.
    fn emit_text(&mut self, runs: &[PlacedRun], fills: &[PlacedFill], left: f64, top: f64) {
        for f in fills.iter().filter(|f| f.background) {
            let [x0, y0, x1, y1] = f.rect.map(f64::from);
            self.fill_rect(Rect::new(left + x0, top + y0, left + x1, top + y1), Color::from_raw(f.color));
        }
        let cached = self.state.shadow.is_none()
            && self.state.mask.is_none()
            && self.state.blend == BlendMode::Normal
            && upright(self.state.xf).is_some();
        for run in runs {
            let origin = Point::new(left + f64::from(run.x), top + f64::from(run.y));
            match upright(self.state.xf).filter(|_| cached) {
                Some(s) => {
                    let at = self.state.xf * origin;
                    let glyphs: Arc<[protocol::Glyph]> = if s == 1.0 {
                        run.glyphs.clone()
                    } else {
                        let s = s as f32;
                        run.glyphs.iter().map(|g| protocol::Glyph { id: g.id, x: g.x * s, y: g.y * s }).collect()
                    };
                    self.ops.push(Op::Glyphs(GlyphRun {
                        font: run.font,
                        size: run.size * s as f32,
                        x: at.x as f32,
                        y: at.y as f32,
                        glyphs,
                        color: run.color,
                        clip: self.state.clip,
                    }));
                }
                None => self.glyph_outlines(run, origin),
            }
        }
        for f in fills.iter().filter(|f| !f.background) {
            // Lines are drawn crisp: whole points, at least one thick.
            let [x0, y0, x1, y1] = f.rect.map(f64::from);
            let y = (top + y0).round();
            let thickness = (y1 - y0).round().max(1.0);
            self.fill_rect(Rect::new(left + x0, y, left + x1, y + thickness), Color::from_raw(f.color));
        }
    }

    /// A run's glyphs as their outlines, filled through the drawing state:
    /// text turned, mirrored or stretched, or with a shadow, a clip shape
    /// or a blend mode, which the glyph cache can't draw.
    fn glyph_outlines(&mut self, run: &PlacedRun, origin: Point) {
        let Some(face) = fonts::face_data(run.font) else { return };
        let scale = f64::from(run.size) / outline::units_per_em(&face.font);
        let skew = f64::from(face.skew).to_radians().tan();
        let mut path = BezPath::new();
        for g in run.glyphs.iter() {
            let Ok(id) = u16::try_from(g.id) else { continue };
            let Some(mut glyph) = outline::outline(&face.font, &face.coords, id, scale) else { continue };
            // Font units run up; the canvas runs down. A synthesized
            // oblique leans the glyph right.
            let at = Affine::translate((origin.x + f64::from(g.x), origin.y + f64::from(g.y)));
            glyph.apply_affine(at * Affine::new([1.0, 0.0, skew, -1.0, 0.0, 0.0]));
            path.extend(glyph);
        }
        if !path.elements().is_empty() {
            self.fill_path(path, false, Paint::Solid(Color::from_raw(run.color)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Canvas {
        let damage = vec![protocol::Rect::new(0.0, 0.0, 100.0, 100.0)];
        Canvas::new(kurbo::Size::new(100.0, 100.0), 1.0, damage, Some(Color::WHITE), Appearance::LIGHT)
    }

    #[test]
    fn passes_start_by_clearing_their_damage() {
        let ops = canvas().finish();
        assert!(matches!(ops[..], [Op::FillWith { blend: Blend::Copy, color: [1.0, 1.0, 1.0, 1.0], .. }]));
    }

    #[test]
    fn rectangles_fill_through_translations_and_clips() {
        let mut c = canvas();
        c.translate((10.0, 20.0));
        c.clip_rect(Rect::new(0.0, 0.0, 5.0, 5.0));
        c.fill_rect(Rect::new(-10.0, -10.0, 50.0, 50.0), Color::BLACK);
        let ops = c.finish();
        let Op::Fill { rect, .. } = &ops[1] else { panic!("a fill: {ops:?}") };
        assert_eq!(*rect, protocol::Rect::new(10.0, 20.0, 15.0, 25.0));
    }

    #[test]
    fn turned_rectangles_are_shapes() {
        let mut c = canvas();
        c.rotate(0.5);
        c.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::BLACK);
        assert!(matches!(c.finish()[1], Op::FillPath { draw: Draw { aa: false, .. }, .. }));
    }

    #[test]
    fn saved_state_comes_back() {
        let mut c = canvas();
        c.saved(|c| {
            c.translate((5.0, 5.0));
            c.clip_rect(Rect::new(0.0, 0.0, 1.0, 1.0));
        });
        assert_eq!(c.transform(), Affine::IDENTITY);
        assert!(c.needs(Rect::new(50.0, 50.0, 60.0, 60.0)));
    }

    #[test]
    fn groups_close_even_when_left_open() {
        let mut c = canvas();
        c.group(0.5, |c| c.fill(&Rect::new(0.0, 0.0, 10.0, 10.0), Color::BLACK));
        let ops = c.finish();
        assert!(matches!(ops[1], Op::BeginGroup { alpha: 0.5, .. }));
        assert!(matches!(ops[2], Op::FillPath { .. }));
        assert!(matches!(ops[3], Op::EndGroup));
    }

    #[test]
    fn text_is_glyph_runs_upright_and_outlines_otherwise() {
        let style = TextStyle::new(crate::Font::system(12.0), Color::BLACK);
        let mut c = canvas();
        c.draw_label("Hi", &style, Point::new(10.0, 10.0));
        c.save();
        c.scale_by(2.0, 2.0);
        c.draw_label("Hi", &style, Point::new(10.0, 10.0));
        c.restore();
        c.rotate(0.3);
        c.draw_label("Hi", &style, Point::new(10.0, 10.0));
        let ops = c.finish();
        let runs: Vec<&GlyphRun> =
            ops.iter().filter_map(|op| if let Op::Glyphs(r) = op { Some(r) } else { None }).collect();
        assert_eq!(runs.len(), 2, "{ops:?}");
        assert_eq!(runs[1].size, runs[0].size * 2.0);
        assert!((runs[1].x - runs[0].x * 2.0).abs() < 0.01);
        assert!(ops.iter().any(|op| matches!(op, Op::FillPath { .. })), "the turned label is outlines");
    }
}
