//! Ops beyond plain fills: paths, strokes, images and gradients drawn with
//! tiny-skia, blend modes, groups and path clips.
//!
//! An op is drawn into a scratch pixmap covering only the pixels it may
//! touch: its bounds, within its clip and the damaged rectangle. The
//! scratch starts as a copy of those pixels and is copied back after, so
//! an op can't reach outside its clip rectangle and rectangular clips need
//! no mask; a clip with paths becomes a tiny-skia mask of the scratch's
//! size, kept while the next ops ask for the same clip over the same
//! pixels and made in the last one's memory otherwise ([`Masks`]). The
//! scratch buffers, layers and shadow buffers are the thread's own and
//! are reused. What drawing still allocates is tiny-skia's: transforming
//! and stroking a path, a gradient's shader.
//!
//! A group (`BeginGroup` … `EndGroup`) is drawn into a transparent layer
//! covering what its ops can reach within its clip and the damage (not
//! all of its clip: text under a path clip is a group per string), then
//! composited with its alpha and blend mode.

use std::cell::RefCell;
use std::sync::Arc;

use tiny_skia::{BlendMode, FillRule, Mask, PixmapMut, PixmapPaint, Transform};

use super::{Canvas, Glyphs, as_bytes, premultiplied};
use crate::protocol::{Blend, ClipImage, ClipPath, Draw, Op, Paint, Rect, ShadowSpec, StrokeSpec};

/// What a thread keeps between ops: scratch pixels, spare layers and the
/// last clip mask.
#[derive(Default)]
struct State {
    scratch: Vec<u32>,
    /// A second scratch, for blend modes tiny-skia lacks.
    source: Vec<u32>,
    spare: Vec<Vec<u32>>,
    masks: Masks,
}

/// The last clip mask made: for which clip (held, so it can't be freed and
/// another made at its address), over which pixels, and the mask.
#[derive(Default)]
pub(crate) struct Masks {
    last: Option<(Arc<[ClipPath]>, MaskKey, Mask)>,
}

/// Where a mask lies: its origin on the layer's grid, its size, and the
/// bits of the grid's scale.
type MaskKey = (i32, i32, u32, u32, u32);

impl Masks {
    /// The coverage of `paths` (all of them) over a `w` × `h` region whose
    /// top left pixel is `origin` on the layer's pixel grid, at its scale:
    /// the last mask if it was for the same, else a new one in its memory.
    pub fn get(&mut self, paths: Option<&Arc<[ClipPath]>>, w: u32, h: u32, origin: (i32, i32, f32)) -> Option<&Mask> {
        let paths = paths?;
        let key = (origin.0, origin.1, w, h, origin.2.to_bits());
        let hit = matches!(&self.last, Some((p, k, _)) if Arc::ptr_eq(p, paths) && *k == key);
        if !hit {
            let mut data = self.last.take().map(|(.., m)| m.take()).unwrap_or_default();
            data.clear();
            data.resize(w as usize * h as usize, 0);
            let mut mask = Mask::from_vec(data, tiny_skia::IntSize::from_wh(w, h)?)?;
            fill_clip(&mut mask, paths, origin);
            self.last = Some((paths.clone(), key, mask));
        }
        self.last.as_ref().map(|(.., m)| m)
    }
}

thread_local!(static STATE: RefCell<State> = RefCell::default());

/// A group being drawn: its pixels and where they go.
struct Layer {
    px: Vec<u32>,
    width: u32,
    height: u32,
    /// On the base canvas's pixel grid.
    x0: i32,
    y0: i32,
    alpha: f32,
    draw: Draw,
}

impl Layer {
    fn canvas<'a>(&'a mut self, base: &Canvas) -> Canvas<'a> {
        Canvas {
            px: &mut self.px,
            width: self.width,
            height: self.height,
            stride: self.width as usize,
            origin_y: base.origin_y,
            scale: base.scale,
            x0: self.x0,
            y0: self.y0,
        }
    }
}

/// Draw `ops` into `damage` (layer points) of `canvas`.
pub(crate) fn run(canvas: &mut Canvas, glyphs: &mut Glyphs, damage: &Rect, ops: &[Op]) {
    STATE.with(|state| {
        let mut fresh = State::default();
        let mut borrowed = state.try_borrow_mut();
        let st = match borrowed.as_deref_mut() {
            Ok(st) => st,
            Err(_) => &mut fresh,
        };
        let mut layers: Vec<Layer> = Vec::new();
        for (i, op) in ops.iter().enumerate() {
            match op {
                Op::BeginGroup { alpha, draw } => {
                    let parent = match layers.last_mut() {
                        Some(l) => (l.x0, l.y0, l.width, l.height),
                        None => (canvas.x0, canvas.y0, canvas.width, canvas.height),
                    };
                    let probe = Canvas {
                        px: &mut [],
                        width: parent.2,
                        height: parent.3,
                        x0: parent.0,
                        y0: parent.1,
                        ..canvas.reborrow()
                    };
                    // What the group's ops can reach, within its clip.
                    let clip = probe.clip_pixels(draw, damage);
                    let reach = contents(&probe, damage, &ops[i + 1..]);
                    let (x0, y0, x1, y1) = clip
                        .zip(reach)
                        .map(|(c, r)| (c.0.max(r.0), c.1.max(r.1), c.2.min(r.2), c.3.min(r.3)))
                        .filter(|r| r.0 < r.2 && r.1 < r.3)
                        .unwrap_or((0, 0, 0, 0));
                    let (width, height) = ((x1 - x0) as u32, (y1 - y0) as u32);
                    let mut px = st.spare.pop().unwrap_or_default();
                    px.clear();
                    px.resize(width as usize * height as usize, 0);
                    layers.push(Layer {
                        px,
                        width,
                        height,
                        x0: parent.0 + x0 as i32,
                        y0: parent.1 + y0 as i32,
                        alpha: *alpha,
                        draw: draw.clone(),
                    });
                }
                Op::EndGroup => {
                    if let Some(layer) = layers.pop() {
                        close_group(canvas, &mut layers, st, layer, damage);
                    }
                }
                op => match layers.last_mut() {
                    Some(layer) => draw_op(&mut layer.canvas(canvas), glyphs, st, damage, op),
                    None => draw_op(canvas, glyphs, st, damage, op),
                },
            }
        }
        while let Some(layer) = layers.pop() {
            close_group(canvas, &mut layers, st, layer, damage);
        }
    });
}

/// Composite a finished group into what's under it, with its shadow (a
/// transparency layer's) under it.
fn close_group(base: &mut Canvas, layers: &mut [Layer], st: &mut State, mut layer: Layer, damage: &Rect) {
    if layer.width > 0 && layer.height > 0 && layer.alpha > 0.0 {
        let src = tiny_skia::PixmapRef::from_bytes(as_bytes(&mut layer.px), layer.width, layer.height);
        if let Some(src) = src {
            let mut target = match layers.last_mut() {
                Some(parent) => parent.canvas(base),
                None => base.reborrow(),
            };
            let (lx, ly) = ((layer.x0 - target.x0) as usize, (layer.y0 - target.y0) as usize);
            let mut region = (lx, ly, lx + layer.width as usize, ly + layer.height as usize);
            // A shadow reaches anywhere in the group's clip.
            if layer.draw.shadow.is_some()
                && let Some(c) = target.clip_pixels(&layer.draw, damage)
            {
                region = union_region(region, c);
            }
            let (at_x, at_y) = ((lx - region.0) as i32, (ly - region.1) as i32);
            let paint = PixmapPaint {
                opacity: layer.alpha.clamp(0.0, 1.0),
                blend_mode: blend_mode(layer.draw.blend),
                quality: tiny_skia::FilterQuality::Nearest,
            };
            let shadow = layer.draw.shadow.clone();
            with_region(&mut target, st, region, |pm, spare, masks, origin| {
                let mask = masks.get(layer.draw.mask.as_ref(), pm.width(), pm.height(), origin);
                if let Some(shadow) = &shadow {
                    let faded = ShadowSpec {
                        color: [shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3] * paint.opacity],
                        ..(**shadow).clone()
                    };
                    super::effects::shadow(pm, &faded, origin.2, mask, |target, moved| {
                        target.draw_pixmap(at_x, at_y, src, &PixmapPaint::default(), moved, None)
                    });
                }
                masked(pm, spare, mask, layer.draw.blend, |pm, mask| {
                    pm.draw_pixmap(at_x, at_y, src, &paint, Transform::identity(), mask)
                });
            });
        }
    }
    st.spare.push(std::mem::take(&mut layer.px));
}

fn draw_op(canvas: &mut Canvas, glyphs: &mut Glyphs, st: &mut State, damage: &Rect, op: &Op) {
    match op {
        Op::Fill { rect, color } => super::fill(canvas, &rect.intersect(damage), *color),
        Op::FillWith { rect, color, blend } => {
            let rect = rect.intersect(damage);
            match blend {
                Blend::SourceOver | Blend::Highlight => super::fill(canvas, &rect, *color),
                Blend::Copy => super::replace(canvas, &rect, premultiplied(*color)),
                Blend::Clear => super::replace(canvas, &rect, 0),
                _ => {
                    let Some(path) = rect_path(&rect) else { return };
                    let draw = Draw {
                        xf: Transform::identity(),
                        blend: *blend,
                        aa: false,
                        clip: rect,
                        mask: None,
                        shadow: None,
                    };
                    shape(canvas, st, damage, &draw, &Shape::Fill(&path, false), &Paint::Solid(*color));
                }
            }
        }
        Op::Glyphs(run) => crate::text::draw_glyphs(canvas, glyphs, run, &run.clip.intersect(damage)),
        Op::FillPath { path, even_odd, paint, draw } => {
            shape(canvas, st, damage, draw, &Shape::Fill(path, *even_odd), paint)
        }
        Op::StrokePath { path, stroke, paint, draw } => {
            shape(canvas, st, damage, draw, &Shape::Stroke(path, stroke), paint)
        }
        Op::Image { image, src, dst, alpha, quality, tint, tiled, draw } => {
            super::images::draw(canvas, st, damage, image, (src, dst), *alpha, *quality, *tint, *tiled, draw)
        }
        Op::BeginGroup { .. } | Op::EndGroup => {}
    }
}

fn rect_path(r: &Rect) -> Option<tiny_skia::Path> {
    tiny_skia::Rect::from_ltrb(r.x0, r.y0, r.x1, r.y1).map(tiny_skia::PathBuilder::from_rect)
}

/// Two pixel regions' union (x0, y0, x1, y1).
fn union_region(a: (usize, usize, usize, usize), b: (usize, usize, usize, usize)) -> (usize, usize, usize, usize) {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// The pixels (of `canvas`) ops can reach within `damage`, up to the end
/// of the group they're in (or all of them), groups and all: x0, y0, x1,
/// y1; `None` for none.
pub(crate) fn contents(canvas: &Canvas, damage: &Rect, ops: &[Op]) -> Option<(usize, usize, usize, usize)> {
    let mut depth = 0;
    let mut all: Option<(usize, usize, usize, usize)> = None;
    // Groups casting shadows reach as far as their clips let the shadows.
    let mut shadowed: Option<(usize, usize, usize, usize)> = None;
    for op in ops {
        let reach = match op {
            Op::BeginGroup { draw, .. } => {
                if depth == 0
                    && draw.shadow.is_some()
                    && let Some(r) = canvas.clip_pixels(draw, damage)
                {
                    shadowed = Some(shadowed.map_or(r, |a| union_region(a, r)));
                }
                depth += 1;
                continue;
            }
            Op::EndGroup if depth == 0 => break,
            Op::EndGroup => {
                depth -= 1;
                continue;
            }
            Op::Fill { rect, .. } | Op::FillWith { rect, .. } => canvas.pixels(&rect.intersect(damage)),
            Op::Glyphs(run) => glyph_reach(run).and_then(|r| canvas.pixels(&r.intersect(&run.clip).intersect(damage))),
            Op::FillPath { path, draw, .. } => region(canvas, damage, draw, path.bounds(), 1.0),
            Op::StrokePath { path, stroke, draw, .. } => {
                region(canvas, damage, draw, path.bounds(), stroke_outset(canvas, draw, stroke))
            }
            Op::Image { dst, draw, tiled: false, .. } => {
                super::images::dst_bounds(dst).and_then(|b| region(canvas, damage, draw, b, 1.0))
            }
            Op::Image { draw, tiled: true, .. } => canvas.clip_pixels(draw, damage),
        };
        if let Some(r) = reach {
            all = Some(all.map_or(r, |a| union_region(a, r)));
        }
    }
    match (all, shadowed) {
        (Some(a), Some(s)) => Some(union_region(a, s)),
        (a, _) => a,
    }
}

/// Where a run's glyphs may put ink (layer points): around their origins
/// by three times the font size, more than glyphs reach.
fn glyph_reach(run: &crate::protocol::GlyphRun) -> Option<Rect> {
    let mut b: Option<(f32, f32, f32, f32)> = None;
    for g in run.glyphs.iter() {
        b = Some(b.map_or((g.x, g.y, g.x, g.y), |b| (b.0.min(g.x), b.1.min(g.y), b.2.max(g.x), b.3.max(g.y))));
    }
    let (x0, y0, x1, y1) = b?;
    let reach = run.size * 3.0;
    Some(Rect::new(run.x + x0 - reach, run.y + y0 - reach, run.x + x1 + reach, run.y + y1 + reach))
}

/// How far (pixels) a stroke reaches beyond its path's bounds.
fn stroke_outset(canvas: &Canvas, draw: &Draw, stroke: &StrokeSpec) -> f32 {
    let scale = canvas.transform().pre_concat(draw.xf);
    let factor = (scale.sx.hypot(scale.ky)).max(scale.kx.hypot(scale.sy));
    let join = if stroke.join == 0 { stroke.miter.max(1.0) } else { 1.0 };
    let cap = if stroke.cap == 2 { std::f32::consts::SQRT_2 } else { 1.0 };
    (stroke.width * 0.5 * join.max(cap) * factor).max(0.5) + 1.0
}

pub(crate) enum Shape<'a> {
    Fill(&'a tiny_skia::Path, bool),
    Stroke(&'a tiny_skia::Path, &'a StrokeSpec),
}

/// The pixels (of `canvas`) a shape drawn with `draw` may touch, shadow
/// included: x0, y0, x1, y1.
pub(crate) fn region(
    canvas: &Canvas,
    damage: &Rect,
    draw: &Draw,
    bounds: tiny_skia::Rect,
    outset: f32,
) -> Option<(usize, usize, usize, usize)> {
    let (cx0, cy0, cx1, cy1) = canvas.clip_pixels(draw, damage)?;
    let dev = bounds.transform(canvas.transform().pre_concat(draw.xf))?;
    let mut b = (dev.left() - outset, dev.top() - outset, dev.right() + outset, dev.bottom() + outset);
    if let Some(s) = &draw.shadow {
        let (dx, dy, spread) = (s.dx * canvas.scale, s.dy * canvas.scale, super::effects::reach(s.blur * canvas.scale));
        b = (
            b.0.min(b.0 + dx - spread),
            b.1.min(b.1 + dy - spread),
            b.2.max(b.2 + dx + spread),
            b.3.max(b.3 + dy + spread),
        );
    }
    let x0 = (b.0.floor().max(0.0) as usize).max(cx0);
    let y0 = (b.1.floor().max(0.0) as usize).max(cy0);
    let x1 = (b.2.ceil().max(0.0) as usize).min(cx1);
    let y1 = (b.3.ceil().max(0.0) as usize).min(cy1);
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
}

fn shape(canvas: &mut Canvas, st: &mut State, damage: &Rect, draw: &Draw, shape: &Shape, paint: &Paint) {
    let (bounds, outset) = match shape {
        Shape::Fill(path, _) => (path.bounds(), 1.0),
        Shape::Stroke(path, stroke) => (path.bounds(), stroke_outset(canvas, draw, stroke)),
    };
    let Some(region) = region(canvas, damage, draw, bounds, outset) else { return };
    let base = canvas.transform().pre_concat(draw.xf);
    with_region(canvas, st, region, |pm, source, masks, origin| {
        // The region's pixels, from the canvas's (the origin is on the
        // layer's grid, for masks).
        let xf = base.post_translate(-(region.0 as f32), -(region.1 as f32));
        let mask = masks.get(draw.mask.as_ref(), pm.width(), pm.height(), origin);
        if let Some(shadow) = &draw.shadow {
            // The shadow is as opaque as the paint casting it.
            if let Some(caster) = shadow_paint(paint) {
                super::effects::shadow(pm, shadow, origin.2, mask, |target, moved| {
                    paint_shape(target, shape, &caster, draw.aa, xf.post_concat(moved), None)
                });
            }
        }
        let Some(mut paint) = to_paint(paint) else { return };
        paint.anti_alias = draw.aa;
        match draw.blend {
            Blend::PlusDarker | Blend::ColorBurn | Blend::SoftLight => {
                // Draw alone, then combine by hand.
                let (w, h) = (pm.width(), pm.height());
                source.clear();
                source.resize(w as usize * h as usize, 0);
                if let Some(mut layer) = PixmapMut::from_bytes(as_bytes(source), w, h) {
                    paint_shape(&mut layer, shape, &paint, draw.aa, xf, mask);
                }
                match draw.blend {
                    Blend::ColorBurn => separable(pm, source, color_burn),
                    Blend::SoftLight => separable(pm, source, soft_light),
                    _ => plus_darker(pm, source),
                }
            }
            blend => {
                paint.blend_mode = blend_mode(blend);
                masked(pm, source, mask, blend, |pm, mask| paint_shape(pm, shape, &paint, draw.aa, xf, mask));
            }
        }
    });
}

/// What a shape painted with `paint` casts as a shadow: black, with the
/// paint's alpha (a gradient's at each of its stops).
fn shadow_paint(paint: &Paint) -> Option<tiny_skia::Paint<'static>> {
    match paint {
        Paint::Solid(c) => Some(solid([0.0, 0.0, 0.0, c[3]])),
        Paint::Gradient(g) => {
            let mut black = (**g).clone();
            black.stops.iter_mut().for_each(|(_, c)| *c = [0.0, 0.0, 0.0, c[3]]);
            to_paint(&Paint::Gradient(Arc::new(black)))
        }
    }
}

pub(crate) fn solid<'a>(c: crate::protocol::Color) -> tiny_skia::Paint<'a> {
    let mut paint = tiny_skia::Paint::default();
    paint.set_color(color(c));
    paint
}

pub(crate) fn color(c: crate::protocol::Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0), c[3].clamp(0.0, 1.0))
        .unwrap_or(tiny_skia::Color::BLACK)
}

fn paint_shape(
    pm: &mut PixmapMut,
    shape: &Shape,
    paint: &tiny_skia::Paint,
    aa: bool,
    xf: Transform,
    mask: Option<&Mask>,
) {
    let mut paint = paint.clone();
    paint.anti_alias = aa;
    match shape {
        Shape::Fill(path, even_odd) => {
            let rule = if *even_odd { FillRule::EvenOdd } else { FillRule::Winding };
            pm.fill_path(path, &paint, rule, xf, mask);
        }
        Shape::Stroke(path, spec) => pm.stroke_path(path, &paint, &stroke(spec), xf, mask),
    }
}

fn stroke(spec: &StrokeSpec) -> tiny_skia::Stroke {
    tiny_skia::Stroke {
        width: spec.width.max(0.0),
        miter_limit: spec.miter.max(1.0),
        line_cap: match spec.cap {
            1 => tiny_skia::LineCap::Round,
            2 => tiny_skia::LineCap::Square,
            _ => tiny_skia::LineCap::Butt,
        },
        line_join: match spec.join {
            1 => tiny_skia::LineJoin::Round,
            2 => tiny_skia::LineJoin::Bevel,
            _ => tiny_skia::LineJoin::Miter,
        },
        dash: spec.dash.as_ref().and_then(|(pattern, phase)| {
            // tiny-skia wants an even number of lengths, as PostScript
            // repeats an odd pattern twice.
            let mut p = pattern.clone();
            if p.len() % 2 == 1 {
                p.extend_from_within(..);
            }
            tiny_skia::StrokeDash::new(p, *phase)
        }),
    }
}

/// The tiny-skia paint for `paint`. Gradients are in user space, which
/// the transform a shape is drawn with maps for its shader too.
fn to_paint(paint: &Paint) -> Option<tiny_skia::Paint<'static>> {
    Some(match paint {
        Paint::Solid(c) => solid(*c),
        Paint::Gradient(g) => tiny_skia::Paint { shader: super::effects::gradient(g)?, ..Default::default() },
    })
}

pub(crate) fn blend_mode(blend: Blend) -> BlendMode {
    match blend {
        Blend::Clear => BlendMode::Clear,
        Blend::Copy => BlendMode::Source,
        Blend::SourceOver | Blend::Highlight | Blend::PlusDarker => BlendMode::SourceOver,
        Blend::SourceIn => BlendMode::SourceIn,
        Blend::SourceOut => BlendMode::SourceOut,
        Blend::SourceAtop => BlendMode::SourceAtop,
        Blend::DestinationOver => BlendMode::DestinationOver,
        Blend::DestinationIn => BlendMode::DestinationIn,
        Blend::DestinationOut => BlendMode::DestinationOut,
        Blend::DestinationAtop => BlendMode::DestinationAtop,
        Blend::Xor => BlendMode::Xor,
        Blend::PlusLighter => BlendMode::Plus,
        Blend::Multiply => BlendMode::Multiply,
        Blend::Screen => BlendMode::Screen,
        Blend::Overlay => BlendMode::Overlay,
        Blend::Darken => BlendMode::Darken,
        Blend::Lighten => BlendMode::Lighten,
        Blend::ColorDodge => BlendMode::ColorDodge,
        Blend::ColorBurn => BlendMode::ColorBurn,
        Blend::SoftLight => BlendMode::SoftLight,
        Blend::HardLight => BlendMode::HardLight,
        Blend::Difference => BlendMode::Difference,
        Blend::Exclusion => BlendMode::Exclusion,
        Blend::Hue => BlendMode::Hue,
        Blend::Saturation => BlendMode::Saturation,
        Blend::Color => BlendMode::Color,
        Blend::Luminosity => BlendMode::Luminosity,
    }
}

/// Whether a transparent source leaves the destination as it is in
/// `blend`: then tiny-skia's masks (which scale the source) clip, and
/// otherwise they would clear what they leave out.
fn transparent_keeps(blend: Blend) -> bool {
    !matches!(
        blend,
        Blend::Clear | Blend::Copy | Blend::SourceIn | Blend::SourceOut | Blend::DestinationIn | Blend::DestinationAtop
    )
}

/// Run `f`, which draws into `pm` composited by `blend`, inside `mask`:
/// for blend modes a mask can't clip by scaling the source, `f` draws
/// unmasked and the result is mixed back toward what was there by the
/// mask. `spare` holds the pixels meanwhile.
pub(crate) fn masked(
    pm: &mut PixmapMut,
    spare: &mut Vec<u32>,
    mask: Option<&Mask>,
    blend: Blend,
    f: impl FnOnce(&mut PixmapMut, Option<&Mask>),
) {
    let Some(mask) = mask.filter(|_| !transparent_keeps(blend)) else {
        f(pm, mask);
        return;
    };
    spare.clear();
    spare.extend(pm.data_mut().as_chunks::<4>().0.iter().map(|p| u32::from_ne_bytes(*p)));
    f(pm, None);
    for ((p, &was), &m) in pm.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(spare.iter()).zip(mask.data()) {
        *p = super::lerp(was, u32::from_ne_bytes(*p), u32::from(m)).to_ne_bytes();
    }
}

/// `R = max(0, 1 − ((1 − D) + (1 − S)))`, for premultiplied pixels: the
/// alpha adds up (to 1 at most) and each channel is what the alpha leaves
/// after both colors' distances from white.
fn plus_darker(pm: &mut PixmapMut, source: &[u32]) {
    for (d, &s) in pm.pixels_mut().iter_mut().zip(source) {
        let s = s.to_ne_bytes().map(i32::from);
        if s[3] == 0 {
            continue;
        }
        let (dr, dg, db, da) = (i32::from(d.red()), i32::from(d.green()), i32::from(d.blue()), i32::from(d.alpha()));
        let a = (s[3] + da).min(255);
        let ch = |sc: i32, dc: i32| (a - (s[3] - sc) - (da - dc)).clamp(0, a) as u8;
        if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(ch(s[0], dr), ch(s[1], dg), ch(s[2], db), a as u8) {
            *d = p;
        }
    }
}

/// Composite premultiplied `source` pixels onto `pm` with a separable blend
/// function `b(backdrop, source)` of unpremultiplied colors:
/// `Sa·Da·b(Dc, Sc) + Sca·(1 − Da) + Dca·(1 − Sa)`, clamped to the result's
/// alpha.
fn separable(pm: &mut PixmapMut, source: &[u32], b: fn(f32, f32) -> f32) {
    for (d, &s) in pm.pixels_mut().iter_mut().zip(source) {
        let s = s.to_ne_bytes().map(|v| f32::from(v) / 255.0);
        if s[3] <= 0.0 {
            continue;
        }
        let dv = [d.red(), d.green(), d.blue(), d.alpha()].map(|v| f32::from(v) / 255.0);
        let (sa, da) = (s[3], dv[3]);
        let ra = sa + da - sa * da;
        let ch = |sc: f32, dc: f32| {
            let both = sa * da;
            let blended = if both > 0.0 { both * b(dc / da, sc / sa) } else { 0.0 };
            let v = blended + sc * (1.0 - da) + dc * (1.0 - sa);
            (if v.is_nan() { 0.0 } else { v.clamp(0.0, ra) } * 255.0).round() as u8
        };
        let a = (ra * 255.0).round() as u8;
        if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(
            ch(s[0], dv[0]).min(a),
            ch(s[1], dv[1]).min(a),
            ch(s[2], dv[2]).min(a),
            a,
        ) {
            *d = p;
        }
    }
}

/// Color burn as CoreGraphics works it out (measured on macOS): the
/// backdrop's distance from white divided by the source, from white, not
/// held at black, so it darkens past it (the result is clamped after).
fn color_burn(backdrop: f32, source: f32) -> f32 {
    if backdrop >= 1.0 {
        1.0
    } else if source <= 0.0 {
        -1e6
    } else {
        1.0 - (1.0 - backdrop) / source
    }
}

/// Soft light as CoreGraphics works it out (measured on macOS): the
/// quadratic form, `(1 − 2S)·B² + 2S·B`.
fn soft_light(backdrop: f32, source: f32) -> f32 {
    (1.0 - 2.0 * source) * backdrop * backdrop + 2.0 * source * backdrop
}

/// Run `f` on a pixmap holding `region` (x0, y0, x1, y1 pixels) of
/// `canvas`, then put the pixels back. `f` also gets a spare buffer, the
/// thread's clip masks, and the region's origin on the canvas and the
/// canvas's scale.
pub(crate) fn with_region(
    canvas: &mut Canvas,
    st_: &mut dyn ScratchSource,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
    f: impl FnOnce(&mut PixmapMut, &mut Vec<u32>, &mut Masks, (i32, i32, f32)),
) {
    let (w, h) = (x1 - x0, y1 - y0);
    if w == 0 || h == 0 {
        return;
    }
    let (scratch, source, masks) = st_.buffers();
    scratch.clear();
    scratch.reserve(w * h);
    let stride = canvas.stride;
    for y in y0..y1 {
        scratch.extend_from_slice(&canvas.px[y * stride + x0..y * stride + x1]);
    }
    if let Some(mut pm) = PixmapMut::from_bytes(as_bytes(scratch), w as u32, h as u32) {
        f(&mut pm, source, masks, (x0 as i32 + canvas.x0, y0 as i32 + canvas.y0, canvas.scale));
    }
    for (row, y) in (y0..y1).enumerate() {
        canvas.px[y * stride + x0..y * stride + x1].copy_from_slice(&scratch[row * w..(row + 1) * w]);
    }
}

/// Where [`with_region`] gets its buffers: a scratch for the region, a
/// spare, and the clip masks.
pub(crate) trait ScratchSource {
    fn buffers(&mut self) -> (&mut Vec<u32>, &mut Vec<u32>, &mut Masks);
}

impl ScratchSource for State {
    fn buffers(&mut self) -> (&mut Vec<u32>, &mut Vec<u32>, &mut Masks) {
        (&mut self.scratch, &mut self.source, &mut self.masks)
    }
}

/// Fill a zeroed `mask` with the coverage of `paths` (all of them), the
/// mask's top left pixel being `origin` on the layer's pixel grid.
fn fill_clip(mask: &mut Mask, paths: &[ClipPath], origin: (i32, i32, f32)) {
    let (ox, oy, scale) = origin;
    // Layer points to the region's pixels. The layer grid's origin_y is
    // already in `oy` (it counts from the canvas's first row).
    let to_region =
        |xf: Transform| Transform::from_scale(scale, scale).pre_concat(xf).post_translate(-(ox as f32), -(oy as f32));
    for (i, clip) in paths.iter().enumerate() {
        if let Some(image) = &clip.image {
            let cover = image_coverage(image, mask.width(), mask.height(), to_region(clip.xf));
            let data = mask.data_mut();
            if i == 0 {
                data.copy_from_slice(&cover);
            } else {
                for (m, c) in data.iter_mut().zip(cover) {
                    *m = ((u32::from(*m) * u32::from(c) + 127) / 255) as u8;
                }
            }
            continue;
        }
        let rule = if clip.even_odd { FillRule::EvenOdd } else { FillRule::Winding };
        if i == 0 {
            mask.fill_path(&clip.path, rule, clip.aa, to_region(clip.xf));
        } else {
            mask.intersect_path(&clip.path, rule, clip.aa, to_region(clip.xf));
        }
    }
}

/// How much of each of a `w` × `h` region's pixels an image clip leaves:
/// its image's alpha stretched over its rectangle (`xf` maps user space to
/// the region's pixels), nothing outside it.
fn image_coverage(clip: &ClipImage, w: u32, h: u32, xf: Transform) -> Vec<u8> {
    let mut out = vec![0u8; w as usize * h as usize];
    let data = &clip.image;
    let super::images::Pixels::Rgba(bytes) = &data.pixels else { return out };
    let (Some(src), Some(mut pm)) =
        (tiny_skia::PixmapRef::from_bytes(bytes, data.width, data.height), tiny_skia::Pixmap::new(w, h))
    else {
        return out;
    };
    let d = clip.dst;
    let placed = Transform::from_row(
        (d.x1 - d.x0) / data.width as f32,
        0.0,
        0.0,
        (d.y1 - d.y0) / data.height as f32,
        d.x0,
        d.y0,
    );
    let paint = tiny_skia::Paint {
        shader: tiny_skia::Pattern::new(
            src,
            tiny_skia::SpreadMode::Pad,
            match clip.quality {
                crate::protocol::Quality::None => tiny_skia::FilterQuality::Nearest,
                _ => tiny_skia::FilterQuality::Bilinear,
            },
            1.0,
            placed,
        ),
        ..Default::default()
    };
    if let Some(r) = super::images::dst_bounds(&d) {
        pm.fill_rect(r, &paint, xf, None);
    }
    for (o, p) in out.iter_mut().zip(pm.pixels()) {
        *o = p.alpha();
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::protocol::ShadowSpec;
    use crate::raster::{channels, paint};

    fn draw(clip: Rect) -> Draw {
        Draw { xf: Transform::identity(), blend: Blend::SourceOver, aa: true, clip, mask: None, shadow: None }
    }

    fn square(x0: f32, y0: f32, x1: f32, y1: f32) -> Arc<tiny_skia::Path> {
        Arc::new(rect_path(&Rect::new(x0, y0, x1, y1)).unwrap())
    }

    fn render(w: u32, h: u32, scale: f32, ops: &[Op]) -> Vec<u32> {
        let (pw, ph) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let mut px = vec![0u32; (pw * ph) as usize];
        let mut canvas = Canvas::new(&mut px, pw, ph, 0.0, scale);
        paint(&mut canvas, &mut Glyphs::default(), &[Rect::new(0.0, 0.0, w as f32, h as f32)], ops);
        px
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

    #[test]
    fn paths_stay_in_their_clip_rect() {
        let op = Op::FillPath {
            path: square(0.0, 0.0, 10.0, 10.0),
            even_odd: false,
            paint: Paint::Solid(RED),
            draw: draw(Rect::new(2.0, 2.0, 5.0, 5.0)),
        };
        let px = render(10, 10, 1.0, &[op]);
        let inside = |i: usize| (2..5).contains(&(i % 10)) && (2..5).contains(&(i / 10));
        for (i, p) in px.iter().enumerate() {
            assert_eq!(*p != 0, inside(i), "pixel {i}");
        }
    }

    #[test]
    fn transforms_move_and_scale_shapes() {
        // A unit square scaled by 4 and moved by (2, 3), at 2 pixels a point.
        let mut d = draw(Rect::new(0.0, 0.0, 10.0, 10.0));
        d.xf = Transform::from_row(4.0, 0.0, 0.0, 4.0, 2.0, 3.0);
        let op = Op::FillPath { path: square(0.0, 0.0, 1.0, 1.0), even_odd: false, paint: Paint::Solid(RED), draw: d };
        let px = render(10, 10, 2.0, &[op]);
        let at = |x: usize, y: usize| channels(px[y * 20 + x]);
        assert_eq!(at(4, 6), [255, 0, 0, 255]);
        assert_eq!(at(11, 13), [255, 0, 0, 255]);
        assert_eq!(at(3, 6), [0; 4]);
        assert_eq!(at(12, 6), [0; 4]);
        assert_eq!(px.iter().filter(|&&p| p != 0).count(), 64);
    }

    #[test]
    fn path_clips_mask_their_ops() {
        // A 10 × 10 fill clipped to the triangle below its diagonal.
        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(0.0, 0.0);
        pb.line_to(0.0, 10.0);
        pb.line_to(10.0, 10.0);
        pb.close();
        let clip = ClipPath {
            path: Arc::new(pb.finish().unwrap()),
            even_odd: false,
            xf: Transform::identity(),
            aa: true,
            image: None,
        };
        let mut d = draw(Rect::new(0.0, 0.0, 10.0, 10.0));
        d.mask = Some(Arc::from([clip]));
        let op =
            Op::FillPath { path: square(0.0, 0.0, 10.0, 10.0), even_odd: false, paint: Paint::Solid(RED), draw: d };
        let px = render(10, 10, 1.0, &[op]);
        assert_eq!(channels(px[8 * 10 + 1]), [255, 0, 0, 255], "below the diagonal");
        assert_eq!(channels(px[10 + 8]), [0; 4], "above it");
    }

    #[test]
    fn copies_keep_to_their_clip_paths() {
        // A copy of a square clipped to a circle leaves the corners as
        // they were.
        let under = Op::Fill { rect: Rect::new(0.0, 0.0, 10.0, 10.0), color: BLUE };
        let mut pb = tiny_skia::PathBuilder::new();
        pb.push_circle(5.0, 5.0, 4.0);
        let clip = ClipPath {
            path: Arc::new(pb.finish().unwrap()),
            even_odd: false,
            xf: Transform::identity(),
            aa: true,
            image: None,
        };
        let mut d = draw(Rect::new(1.0, 1.0, 9.0, 9.0));
        d.mask = Some(Arc::from([clip]));
        d.blend = Blend::Copy;
        let op =
            Op::FillPath { path: square(0.0, 0.0, 10.0, 10.0), even_odd: false, paint: Paint::Solid(RED), draw: d };
        let px = render(10, 10, 1.0, &[under, op]);
        assert_eq!(channels(px[5 * 10 + 5]), [255, 0, 0, 255], "inside");
        assert_eq!(channels(px[10 + 1]), [0, 0, 255, 255], "a corner, outside the circle");
    }

    #[test]
    fn every_blend_mode_draws() {
        let under = Op::Fill { rect: Rect::new(0.0, 0.0, 4.0, 4.0), color: BLUE };
        for raw in 0..29 {
            let blend = Blend::from_raw(raw);
            let mut d = draw(Rect::new(0.0, 0.0, 4.0, 4.0));
            d.blend = blend;
            let op =
                Op::FillPath { path: square(0.0, 0.0, 2.0, 4.0), even_odd: false, paint: Paint::Solid(RED), draw: d };
            let px = render(4, 4, 1.0, &[under.clone(), op]);
            let (inside, outside) = (channels(px[0]), channels(px[3]));
            assert_eq!(outside, [0, 0, 255, 255], "{blend:?} leaves what's outside the shape alone");
            let want = match blend {
                Blend::Clear => [0, 0, 0, 0],
                Blend::Copy | Blend::SourceOver | Blend::SourceIn | Blend::SourceAtop | Blend::Highlight => {
                    [255, 0, 0, 255]
                }
                Blend::DestinationOver | Blend::DestinationIn | Blend::DestinationAtop => [0, 0, 255, 255],
                Blend::SourceOut | Blend::DestinationOut | Blend::Xor => [0, 0, 0, 0],
                Blend::PlusLighter | Blend::Screen | Blend::Lighten => [255, 0, 255, 255],
                Blend::PlusDarker | Blend::Multiply | Blend::Darken => [0, 0, 0, 255],
                _ => inside,
            };
            assert_eq!(inside, want, "{blend:?}");
        }
    }

    #[test]
    fn strokes_and_hairlines() {
        // Each line centered on a row of device pixels (a hairline at 2× on
        // a row's middle, a 1-point line at 2× across two rows).
        for (y, width, scale, rows) in [(2.5, 1.0, 1.0, 1), (2.5, 0.0, 1.0, 1), (2.25, 0.0, 2.0, 1), (2.5, 1.0, 2.0, 2)]
        {
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(0.0, y);
            pb.line_to(10.0, y);
            let line = Arc::new(pb.finish().unwrap());
            let spec = StrokeSpec { width, cap: 0, join: 0, miter: 10.0, dash: None };
            let op = Op::StrokePath {
                path: line,
                stroke: Arc::new(spec),
                paint: Paint::Solid(RED),
                draw: draw(Rect::new(0.0, 0.0, 10.0, 10.0)),
            };
            let px = render(10, 10, scale, &[op]);
            let w = (10.0 * scale) as usize;
            let covered = (0..w).filter(|&y| channels(px[y * w + w / 2])[3] > 200).count();
            assert_eq!(covered, rows, "width {width} at {scale}×");
        }
    }

    #[test]
    fn groups_fade_as_one() {
        // Two overlapping opaque fills in a half-transparent group: the
        // overlap is no darker than the rest.
        let d = draw(Rect::new(0.0, 0.0, 10.0, 10.0));
        let ops = [
            Op::BeginGroup { alpha: 0.5, draw: d.clone() },
            Op::Fill { rect: Rect::new(0.0, 0.0, 6.0, 10.0), color: RED },
            Op::Fill { rect: Rect::new(4.0, 0.0, 10.0, 10.0), color: RED },
            Op::EndGroup,
        ];
        let px = render(10, 10, 1.0, &ops);
        assert_eq!(channels(px[1]), [128, 0, 0, 128]);
        assert_eq!(channels(px[5]), [128, 0, 0, 128]);
    }

    #[test]
    fn groups_away_from_the_origin_keep_their_shapes_in_place() {
        // A path in a group whose layer starts at (4, 3).
        let d = draw(Rect::new(4.0, 3.0, 10.0, 10.0));
        let ops = [
            Op::BeginGroup { alpha: 1.0, draw: d.clone() },
            Op::FillPath { path: square(5.0, 5.0, 8.0, 8.0), even_odd: false, paint: Paint::Solid(RED), draw: d },
            Op::EndGroup,
        ];
        let px = render(10, 10, 2.0, &ops);
        let at = |x: usize, y: usize| channels(px[y * 20 + x]);
        assert_eq!(at(11, 11), [255, 0, 0, 255]);
        assert_eq!(at(9, 11), [0; 4]);
        assert_eq!(at(16, 11), [0; 4]);
    }

    /// The drawing gallery's bench frame (2,000 rounded-rect fills, 1,000
    /// strokes, 300 images, half of them downscaled 8 times) as ops, and
    /// how long rasterizing it takes at 1× and 2×.
    #[test]
    #[ignore = "a benchmark; run in release mode"]
    fn timing_bench_frame() {
        use kurbo::Shape;

        use crate::raster::images::{ImageData, Pixels};
        let (w, h) = (984.0, 396.0);
        let d = draw(Rect::new(0.0, 0.0, w, h));
        let mut ops = Vec::new();
        for k in 0..2000 {
            let (x, y) = ((k % 50) as f64 * 19.0 + 4.0, (k / 50) as f64 * 9.0 + 4.0);
            let rr = kurbo::RoundedRect::new(x, y, x + 16.0, y + 7.0, 3.0).to_path(0.1);
            let path = crate::path::to_skia(&rr).unwrap();
            ops.push(Op::FillPath { path, even_odd: false, paint: Paint::Solid(RED), draw: d.clone() });
        }
        let stroke = Arc::new(StrokeSpec { width: 1.0, cap: 0, join: 0, miter: 10.0, dash: None });
        for k in 0..1000 {
            let (x, y) = ((k % 40) as f32 * 24.0 + 2.0, (k / 40) as f32 * 14.0 + 2.0);
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(x, y);
            pb.line_to(x + 20.0, y + 12.0);
            let path = Arc::new(pb.finish().unwrap());
            ops.push(Op::StrokePath { path, stroke: stroke.clone(), paint: Paint::Solid(BLUE), draw: d.clone() });
        }
        let image = |key: u64, n: u32| {
            let data: Vec<u8> = (0..n * n).flat_map(|i| [(i % 251) as u8, 90, 200, 255]).collect();
            Arc::new(ImageData { key, generation: 0, width: n, height: n, pixels: Pixels::Rgba(data.into()) })
        };
        let (small, big) = (image(9001, 32), image(9002, 256));
        for k in 0..300 {
            let (x, y) = ((k % 25) as f32 * 38.0 + 4.0, (k / 25) as f32 * 30.0 + 4.0);
            let (img, n) = if k % 2 == 0 { (small.clone(), 32.0) } else { (big.clone(), 256.0) };
            ops.push(Op::Image {
                image: img,
                src: Rect::new(0.0, 0.0, n, n),
                dst: Rect::new(x, y, x + 28.0, y + 28.0),
                alpha: 1.0,
                quality: crate::protocol::Quality::Medium,
                tint: None,
                tiled: false,
                draw: d.clone(),
            });
        }
        for scale in [1.0f32, 2.0] {
            let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
            let mut px = vec![0u32; (pw * ph) as usize];
            let mut glyphs = Glyphs::default();
            let mut time = |ops: &[Op]| {
                crate::backend::median(|| {
                    let mut canvas = Canvas::new(&mut px, pw, ph, 0.0, scale);
                    paint(&mut canvas, &mut glyphs, &[Rect::new(0.0, 0.0, w, h)], ops);
                })
            };
            let (all, fills, strokes, images) =
                (time(&ops), time(&ops[..2000]), time(&ops[2000..3000]), time(&ops[3000..]));
            println!(
                "the bench frame at {scale}x: {all:.2} ms (fills {fills:.2}, strokes {strokes:.2}, images {images:.2})"
            );
        }
    }

    #[test]
    fn groups_are_as_large_as_what_they_draw() {
        // A small fill in a group whose clip is the whole canvas: the
        // layer covers the fill, not the clip.
        let d = draw(Rect::new(0.0, 0.0, 100.0, 100.0));
        let ops = [
            Op::FillPath {
                path: square(10.0, 20.0, 14.0, 22.0),
                even_odd: false,
                paint: Paint::Solid(RED),
                draw: d.clone(),
            },
            Op::BeginGroup { alpha: 1.0, draw: d.clone() },
            Op::Fill { rect: Rect::new(50.0, 50.0, 52.0, 51.0), color: RED },
            Op::EndGroup,
        ];
        let mut px = vec![0u32; 200 * 200];
        let canvas = Canvas::new(&mut px, 200, 200, 0.0, 2.0);
        let damage = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(contents(&canvas, &damage, &ops[..1]), Some((19, 39, 29, 45)), "a path, outset a pixel");
        assert_eq!(contents(&canvas, &damage, &ops[2..]), Some((100, 100, 104, 102)), "up to the group's end");
        assert_eq!(contents(&canvas, &damage, &ops[3..]), None);
        let px = render(100, 100, 2.0, &ops[1..]);
        assert_eq!(channels(px[100 * 200 + 101]), [255, 0, 0, 255]);
        assert_eq!(px.iter().filter(|&&p| p != 0).count(), 8);
    }

    #[test]
    fn shadows_do_not_depend_on_how_damage_is_cut() {
        let mut d = draw(Rect::new(0.0, 0.0, 40.0, 40.0));
        d.shadow = Some(Arc::new(ShadowSpec { dx: 0.0, dy: 12.0, blur: 4.0, color: [0.0, 0.0, 0.0, 1.0] }));
        let op =
            Op::FillPath { path: square(10.0, 2.0, 30.0, 12.0), even_odd: false, paint: Paint::Solid(RED), draw: d };
        let paint_in = |rects: &[Rect]| {
            let mut px = vec![0u32; 40 * 40];
            let mut canvas = Canvas::new(&mut px, 40, 40, 0.0, 1.0);
            paint(&mut canvas, &mut Glyphs::default(), rects, std::slice::from_ref(&op));
            px
        };
        let whole = paint_in(&[Rect::new(0.0, 0.0, 40.0, 40.0)]);
        // Cut between the shape and its shadow, and through the blur.
        for cut in [20.0, 24.0, 27.0] {
            let split = paint_in(&[Rect::new(0.0, 0.0, 40.0, cut), Rect::new(0.0, cut, 40.0, 40.0)]);
            let differ = whole.iter().zip(&split).filter(|(a, b)| a != b).count();
            assert_eq!(differ, 0, "cut at {cut}");
        }
        assert!(channels(whole[19 * 40 + 20])[3] > 200, "the shadow is there");
    }

    /// Fifty small draws, each in its own group masked by a rounded clip
    /// over a 1000 × 800 view (text under `addClip` is drawn so), at 1×
    /// and 2×: each group's layer covers its draw, not the clip.
    #[test]
    #[ignore = "a benchmark; run in release mode"]
    fn timing_small_groups_under_a_clip() {
        use kurbo::Shape;
        let (w, h) = (1000.0f32, 800.0f32);
        let rounded = kurbo::RoundedRect::new(0.0, 0.0, f64::from(w), f64::from(h), 12.0).to_path(0.1);
        let path = crate::path::to_skia(&rounded).unwrap();
        let clip = ClipPath { path, even_odd: false, xf: Transform::identity(), aa: true, image: None };
        let mut d = draw(Rect::new(0.0, 0.0, w, h));
        d.mask = Some(Arc::from([clip]));
        let mut ops = Vec::new();
        for k in 0..50 {
            let (x, y) = ((k % 10) as f32 * 90.0 + 20.0, (k / 10) as f32 * 150.0 + 20.0);
            ops.push(Op::BeginGroup { alpha: 1.0, draw: d.clone() });
            ops.push(Op::Fill { rect: Rect::new(x, y, x + 60.0, y + 14.0), color: RED });
            ops.push(Op::EndGroup);
        }
        for scale in [1.0f32, 2.0] {
            let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
            let mut px = vec![0u32; (pw * ph) as usize];
            let mut glyphs = Glyphs::default();
            let ms = crate::backend::median(|| {
                let mut canvas = Canvas::new(&mut px, pw, ph, 0.0, scale);
                paint(&mut canvas, &mut glyphs, &[Rect::new(0.0, 0.0, w, h)], &ops);
            });
            println!("50 small groups under a rounded clip at {scale}x: {ms:.2} ms");
        }
    }

    #[test]
    fn shadows_fall_beside_their_shapes() {
        let mut d = draw(Rect::new(0.0, 0.0, 20.0, 20.0));
        d.shadow = Some(Arc::new(ShadowSpec { dx: 5.0, dy: 5.0, blur: 0.0, color: [0.0, 0.0, 0.0, 1.0] }));
        let op = Op::FillPath { path: square(2.0, 2.0, 8.0, 8.0), even_odd: false, paint: Paint::Solid(RED), draw: d };
        let px = render(20, 20, 1.0, &[op]);
        assert_eq!(channels(px[4 * 20 + 4]), [255, 0, 0, 255], "the shape on top");
        assert_eq!(channels(px[11 * 20 + 11]), [0, 0, 0, 255], "its shadow below and right");
        assert_eq!(channels(px[15 * 20 + 15]), [0; 4]);
    }
}
