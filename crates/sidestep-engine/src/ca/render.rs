//! Compositing: a layer tree as drawing ops.
//!
//! Both threads composite the same way. A tree is first taken as
//! [`Node`]s (each layer's properties, as its animations show them at the
//! time, its contents as pixels, its sublayers in the order they draw,
//! and any transition running on it), and [`emit`] turns the nodes into
//! the ops the rasterizer draws: the render thread for a window's layer
//! trees each frame, `renderInContext:` for the model tree into a
//! CGContext. A layer draws, in its own space mapped to the target: its
//! shadow (cast by everything below, or by its `shadowPath`), its
//! background (a rectangle rounded by its `cornerRadius`, circular or
//! continuous, at its `maskedCorners`), its contents (placed by its
//! `contentsGravity` and cut by its `contentsRect`), its sublayers
//! (through its `sublayerTransform`, clipped to its rounded bounds when it
//! `masksToBounds`), then its border; below full `opacity`, or with a
//! shadow or a mask layer, all of it as one group. A 3-D transform draws
//! as the affine map it gives the plane (an orthographic view; a
//! perspective transform as the map through three projected corners).

use std::sync::Arc;

use kurbo::{BezPath, Point, Shape as _};

use super::props::{GradientKind, Gravity, Kind, Props, ShapeProps, compose};
use super::spec::{Direction, TransitionKind};
use crate::protocol::{Blend, ClipPath, Draw, GradientSpec, Op, Paint, Quality, Rect, ShadowSpec, StrokeSpec};
use crate::raster::images::{ImageData, Pixels};

/// What a layer shows of its own.
#[derive(Clone)]
pub enum NodeContent {
    None,
    /// An image (contents given, or drawn): `size` in points (its pixels
    /// over the contents scale), placed by the layer's gravity.
    Image {
        image: Arc<ImageData>,
        size: [f64; 2],
    },
    /// A view's drawing: pixels covering `rect` of the layer's space (its
    /// bounds, or the part of them kept), the first row at the edge the
    /// view's top is.
    Canvas {
        image: Arc<ImageData>,
        rect: [f64; 4],
    },
}

/// A layer as it composites.
#[derive(Clone)]
pub struct Node {
    pub props: Props,
    pub content: NodeContent,
    pub children: Vec<Node>,
    pub mask: Option<Box<Node>>,
    /// A transition running on the layer: how it looked when it began, how
    /// far along it is, its kind and direction.
    pub transition: Option<(Box<Node>, f64, TransitionKind, Direction)>,
}

/// Where ops may draw: a rectangle and paths, in target points.
#[derive(Clone)]
pub struct Clip {
    pub rect: Rect,
    pub paths: Vec<ClipPath>,
}

impl Clip {
    fn draw(&self, xf: &[f64; 6]) -> Draw {
        Draw {
            xf: skia(xf),
            blend: Blend::SourceOver,
            aa: true,
            clip: self.rect,
            mask: (!self.paths.is_empty()).then(|| Arc::from(self.paths.clone())),
            shadow: None,
        }
    }

    /// This clip narrowed to `path` (in the space `xf` maps to targets).
    fn with_path(&self, path: Arc<tiny_skia::Path>, xf: &[f64; 6]) -> Clip {
        let b = path.bounds();
        let r = map_bounds(xf, [b.left() as f64, b.top() as f64, b.width() as f64, b.height() as f64]);
        let mut paths = self.paths.clone();
        let axis = xf[1] == 0.0 && xf[2] == 0.0;
        let is_rect =
            path.points().len() <= 5 && path.compute_tight_bounds().is_some_and(|t| t == b) && is_rect_path(&path);
        if !(axis && is_rect) {
            paths.push(ClipPath { path, even_odd: false, xf: skia(xf), aa: true, image: None });
        }
        Clip { rect: self.rect.intersect(&r), paths }
    }
}

fn is_rect_path(p: &tiny_skia::Path) -> bool {
    use tiny_skia::PathSegment;
    let mut lines = 0;
    for seg in p.segments() {
        match seg {
            PathSegment::MoveTo(_) | PathSegment::Close => {}
            PathSegment::LineTo(_) => lines += 1,
            _ => return false,
        }
    }
    lines <= 4
}

pub fn skia(m: &[f64; 6]) -> tiny_skia::Transform {
    tiny_skia::Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32)
}

/// The box around a rectangle's image under `m`.
pub fn map_bounds(m: &[f64; 6], r: [f64; 4]) -> Rect {
    let [x, y, w, h] = r;
    let pts = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].map(|(px, py)| super::props::apply_affine(m, px, py));
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (px, py) in pts {
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    Rect::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32)
}

// Shapes.

/// A corner of a rounded rectangle: where it is, the directions of the
/// edges coming in and going out, and whether it's rounded.
type Corner = ((f64, f64), (f64, f64), (f64, f64), bool);

/// A rectangle with its corners rounded as a layer rounds them: `corners`
/// the `CACornerMask` bits of the corners that are, circular or
/// continuous.
pub fn rounded_rect(r: [f64; 4], radius: f64, corners: u8, continuous: bool) -> BezPath {
    let [x, y, w, h] = r;
    let (x0, y0, x1, y1) = (x, y, x + w, y + h);
    let max = (w.min(h) / 2.0).max(0.0);
    if radius <= 0.0 || corners == 0 || max <= 0.0 {
        return kurbo::Rect::new(x0, y0, x1, y1).to_path(0.1);
    }
    let cont = continuous && radius * CONTINUOUS_EXPANSION <= max;
    let rad = radius.min(max);
    let on = |bit: u8| corners & bit != 0;
    let mut p = BezPath::new();
    // Corners in order around: (min, min), (max, min), (max, max),
    // (min, max); each given with the directions of the edge coming in
    // and going out.
    let spec: [Corner; 4] = [
        ((x0, y0), (0.0, -1.0), (1.0, 0.0), on(1)),
        ((x1, y0), (1.0, 0.0), (0.0, 1.0), on(2)),
        ((x1, y1), (0.0, 1.0), (-1.0, 0.0), on(8)),
        ((x0, y1), (-1.0, 0.0), (0.0, -1.0), on(4)),
    ];
    for (i, &((cx, cy), (ax, ay), (bx, by), rounded)) in spec.iter().enumerate() {
        // A point at (s, t): back along the incoming edge by s, out along
        // the outgoing one by t (in radii).
        let at = |s: f64, t: f64| Point::new(cx - ax * s * rad + bx * t * rad, cy - ay * s * rad + by * t * rad);
        if !rounded {
            if i == 0 {
                p.move_to(Point::new(cx, cy));
            } else {
                p.line_to(Point::new(cx, cy));
            }
            continue;
        }
        if cont {
            // A continuous corner: three cubics from 1.5287 radii back
            // along one edge to as far along the next. Only that reach is
            // measured (`+cornerCurveExpansionFactor:`); the control points
            // between aren't, and this curve is tighter near the edge than
            // macOS's (row 1 of a 10-point corner drawn by
            // `renderInContext:`: alpha 48 and 191 here, 84 and 224 there).
            const K: [(f64, f64); 10] = [
                (1.528_664_83, 0.0),
                (1.088_492_99, 0.0),
                (0.868_407_01, 0.0),
                (0.669_934_27, 0.065_496),
                (0.372_823_92, 0.192_506_66),
                (0.192_506_66, 0.372_823_92),
                (0.065_496, 0.669_934_27),
                (0.0, 0.868_407_01),
                (0.0, 1.088_492_99),
                (0.0, 1.528_664_83),
            ];
            let start = at(K[0].0, K[0].1);
            if i == 0 {
                p.move_to(start);
            } else {
                p.line_to(start);
            }
            for c in 0..3 {
                let (a, b, e) = (K[1 + 3 * c], K[2 + 3 * c], K[3 + 3 * c]);
                p.curve_to(at(a.0, a.1), at(b.0, b.1), at(e.0, e.1));
            }
        } else {
            let k = 0.552_284_749_830_793_4;
            let start = at(1.0, 0.0);
            if i == 0 {
                p.move_to(start);
            } else {
                p.line_to(start);
            }
            p.curve_to(at(1.0 - k, 0.0), at(0.0, 1.0 - k), at(0.0, 1.0));
        }
    }
    p.close_path();
    p
}

/// The layer's bounds as its corners round them.
fn bounds_path(props: &Props) -> BezPath {
    rounded_rect(props.bounds, props.corner_radius, props.masked_corners, props.continuous_corners)
}

fn color(c: [f64; 4]) -> [f32; 4] {
    c.map(|v| v as f32)
}

// Emitting ops.

/// Emit `node`'s ops into `out`: `parent` maps its superlayer's space to
/// the target's points, `down` says whether that space's y runs down the
/// target, and `clip` is where it may draw.
pub fn emit(node: &Node, parent: &[f64; 6], down: bool, clip: &Clip, out: &mut Vec<Op>) {
    if let Some((old, progress, kind, dir)) = &node.transition {
        emit_transition(node, old, *progress, *kind, *dir, parent, down, clip, out);
        return;
    }
    emit_layer(node, parent, down, clip, out);
}

#[allow(clippy::too_many_arguments)]
fn emit_transition(
    node: &Node,
    old: &Node,
    p: f64,
    kind: TransitionKind,
    dir: Direction,
    parent: &[f64; 6],
    down: bool,
    clip: &Clip,
    out: &mut Vec<Op>,
) {
    let p = p.clamp(0.0, 1.0);
    let group = |alpha: f64, out: &mut Vec<Op>| {
        out.push(Op::BeginGroup { alpha: alpha as f32, draw: clip.draw(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]) });
    };
    // The distance a push moves: the layer's width or height, in its
    // superlayer's space, toward the side the subtype names.
    let [_, _, w, h] = node.props.bounds;
    let vis = |d: Direction| -> (f64, f64) {
        let y = if down { 1.0 } else { -1.0 };
        match d {
            Direction::Left => (w, 0.0),
            Direction::Right => (-w, 0.0),
            Direction::Top => (0.0, h * y),
            Direction::Bottom => (0.0, -h * y),
        }
    };
    let moved = |dx: f64, dy: f64| compose(&[1.0, 0.0, 0.0, 1.0, dx, dy], parent);
    let (dx, dy) = vis(dir);
    match kind {
        TransitionKind::Fade => {
            group(1.0 - p, out);
            emit_layer(old, parent, down, clip, out);
            out.push(Op::EndGroup);
            group(p, out);
            emit_layer(node, parent, down, clip, out);
            out.push(Op::EndGroup);
        }
        TransitionKind::Push => {
            emit_layer(old, &moved(dx * p, dy * p), down, clip, out);
            emit_layer(node, &moved(dx * (p - 1.0), dy * (p - 1.0)), down, clip, out);
        }
        TransitionKind::MoveIn => {
            emit_layer(old, parent, down, clip, out);
            emit_layer(node, &moved(dx * (p - 1.0), dy * (p - 1.0)), down, clip, out);
        }
        TransitionKind::Reveal => {
            emit_layer(node, parent, down, clip, out);
            emit_layer(old, &moved(dx * p, dy * p), down, clip, out);
        }
    }
}

fn emit_layer(node: &Node, parent: &[f64; 6], down: bool, clip: &Clip, out: &mut Vec<Op>) {
    let p = &node.props;
    if p.hidden || p.opacity <= 0.0 {
        return;
    }
    let m = compose(&p.to_superlayer(), parent);
    // A layer turned away (its plane's map reverses orientation relative
    // to its superlayer's) isn't drawn when it isn't double-sided.
    let det = m[0] * m[3] - m[1] * m[2];
    let pdet = parent[0] * parent[3] - parent[1] * parent[2];
    if !p.double_sided && det * pdet < 0.0 && p.transform[10] < 0.0 {
        return;
    }
    if det.abs() < 1e-12 {
        return;
    }
    let down = down ^ p.geometry_flipped;
    let shadow = shadow_of(p, parent);
    let bounds = bounds_path(p);
    let bounds_skia = crate::path::to_skia(&bounds);
    // Masking to bounds clips everything the layer draws, its shadow too.
    let inner_clip = match (&bounds_skia, p.masks_to_bounds) {
        (Some(path), true) => clip.with_path(path.clone(), &m),
        _ => clip.clone(),
    };
    let grouped = p.opacity < 1.0 || shadow.is_some() || node.mask.is_some();
    if grouped {
        let mut draw = inner_clip.draw(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        if p.shadow_path.is_none() {
            draw.shadow = shadow.clone();
        }
        // A layer and its sublayers fade as one (as with group opacity).
        out.push(Op::BeginGroup { alpha: p.opacity as f32, draw });
        if let (Some(spec), Some(sp)) = (&shadow, &p.shadow_path)
            && let Some(path) = sp.drawn()
        {
            let mut draw = clip.draw(&m);
            draw.shadow = Some(Arc::new(ShadowSpec { only: true, ..(**spec).clone() }));
            out.push(Op::FillPath { path, even_odd: false, paint: Paint::Solid([0.0, 0.0, 0.0, 1.0]), draw });
        }
    }
    // The background.
    if let (Some(bg), Some(path)) = (p.background_color, &bounds_skia)
        && bg[3] > 0.0
    {
        out.push(Op::FillPath {
            path: path.clone(),
            even_odd: false,
            paint: Paint::Solid(color(bg)),
            draw: inner_clip.draw(&m),
        });
    }
    // Shape and gradient layers draw after the background.
    match &p.kind {
        Kind::Shape(s) => emit_shape(s, &m, &inner_clip, out),
        Kind::Gradient(g) => emit_gradient(p, g, &m, &inner_clip, out),
        Kind::Plain => {}
    }
    emit_content(node, &m, down, &inner_clip, out);
    // Sublayers, by their z position (in order among equals), through the
    // sublayer transform.
    if !node.children.is_empty() {
        let sub = sublayer_map(p, &m);
        let mut order: Vec<&Node> = node.children.iter().collect();
        order.sort_by(|a, b| a.props.z_position.partial_cmp(&b.props.z_position).unwrap_or(std::cmp::Ordering::Equal));
        for child in order {
            emit(child, &sub, down, &inner_clip, out);
        }
    }
    // The border, inside the bounds, over the sublayers.
    if let Some(bc) = p.border_color
        && p.border_width > 0.0
        && bc[3] > 0.0
    {
        let bw = p.border_width.min(p.bounds[2] / 2.0).min(p.bounds[3] / 2.0);
        let [x, y, w, h] = p.bounds;
        let mut ring = bounds.clone();
        let inner = rounded_rect(
            [x + bw, y + bw, (w - 2.0 * bw).max(0.0), (h - 2.0 * bw).max(0.0)],
            (p.corner_radius - bw).max(0.0),
            p.masked_corners,
            p.continuous_corners,
        );
        ring.extend(inner.iter());
        if let Some(path) = crate::path::to_skia(&ring) {
            out.push(Op::FillPath { path, even_odd: true, paint: Paint::Solid(color(bc)), draw: inner_clip.draw(&m) });
        }
    }
    // A mask layer keeps what's under its alpha, and nothing outside it:
    // its group reaches the whole clip (a transparent fill over all of
    // it), so what the mask doesn't draw clears the layer there.
    if let Some(mask) = &node.mask {
        let mut draw = clip.draw(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        draw.blend = Blend::DestinationIn;
        out.push(Op::BeginGroup { alpha: 1.0, draw });
        out.push(Op::FillWith { rect: clip.rect, color: [0.0; 4], blend: Blend::SourceOver });
        // The mask is placed in the layer's own space.
        emit(mask, &m, down, clip, out);
        out.push(Op::EndGroup);
    }
    if grouped {
        out.push(Op::EndGroup);
    }
}

/// The map from the space a layer's sublayers are placed in to the target,
/// given `m`, the layer's own space's: through its sublayer transform.
pub fn sublayer_map(p: &Props, m: &[f64; 6]) -> [f64; 6] {
    p.sublayer_map().map_or(*m, |st| compose(&st, m))
}

/// A layer's shadow in target terms: its offset through the superlayer's
/// map (so up is up in a space whose y runs up), and its radius as the
/// Gaussian's deviation (`ShadowSpec`'s blur is twice that).
fn shadow_of(p: &Props, parent: &[f64; 6]) -> Option<Arc<ShadowSpec>> {
    let c = p.shadow_color?;
    let alpha = c[3] * p.shadow_opacity.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return None;
    }
    let [ox, oy] = p.shadow_offset;
    let dx = ox * parent[0] + oy * parent[2];
    let dy = ox * parent[1] + oy * parent[3];
    let scale = (parent[0] * parent[3] - parent[1] * parent[2]).abs().sqrt();
    Some(Arc::new(ShadowSpec {
        dx: dx as f32,
        dy: dy as f32,
        blur: (2.0 * p.shadow_radius.max(0.0) * scale) as f32,
        color: [c[0] as f32, c[1] as f32, c[2] as f32, alpha as f32],
        only: false,
    }))
}

/// Where contents of `size` (points) go in `bounds` by `gravity`. The
/// gravities name the edges of the layer's own space with y up, whichever
/// way it shows (so `top` is the far y end even in a flipped layer, as
/// measured: the layer's `contentsAreFlipped` turns the contents over, not
/// their place).
/// Where a view's canvas, drawn for `rect` of the layer's bounds when they
/// were `drawn_for`, shows in the bounds the layer presents now, `bounds`:
/// the canvas is the layer's contents, placed by its gravity as an image
/// of the bounds it was drawn for would be (by default resized with them,
/// as a view's layer shows while an animation changes its frame).
pub fn fit_canvas(rect: [f64; 4], drawn_for: [f64; 4], bounds: [f64; 4], gravity: Gravity) -> [f64; 4] {
    if drawn_for == bounds || drawn_for[2] <= 0.0 || drawn_for[3] <= 0.0 {
        return rect;
    }
    let [dx, dy, dw, dh] = place(gravity, bounds, [drawn_for[2], drawn_for[3]]);
    let (sx, sy) = (dw / drawn_for[2], dh / drawn_for[3]);
    [dx + (rect[0] - drawn_for[0]) * sx, dy + (rect[1] - drawn_for[1]) * sy, rect[2] * sx, rect[3] * sy]
}

fn place(gravity: Gravity, bounds: [f64; 4], size: [f64; 2]) -> [f64; 4] {
    let [x, y, w, h] = bounds;
    let [iw, ih] = size;
    let fit = |s: f64| [iw * s, ih * s];
    let [dw, dh] = match gravity {
        Gravity::Resize => [w, h],
        Gravity::ResizeAspect => {
            if iw <= 0.0 || ih <= 0.0 {
                [0.0, 0.0]
            } else {
                fit((w / iw).min(h / ih))
            }
        }
        Gravity::ResizeAspectFill => {
            if iw <= 0.0 || ih <= 0.0 {
                [0.0, 0.0]
            } else {
                fit((w / iw).max(h / ih))
            }
        }
        _ => [iw, ih],
    };
    let (left, center_x, right) = (x, x + (w - dw) / 2.0, x + w - dw);
    let (top, center_y, bottom) = (y + h - dh, y + (h - dh) / 2.0, y);
    let (dx, dy) = match gravity {
        Gravity::Top => (center_x, top),
        Gravity::Bottom => (center_x, bottom),
        Gravity::Left => (left, center_y),
        Gravity::Right => (right, center_y),
        Gravity::TopLeft => (left, top),
        Gravity::TopRight => (right, top),
        Gravity::BottomLeft => (left, bottom),
        Gravity::BottomRight => (right, bottom),
        _ => (center_x, center_y),
    };
    [dx, dy, dw, dh]
}

fn emit_content(node: &Node, m: &[f64; 6], down: bool, clip: &Clip, out: &mut Vec<Op>) {
    let p = &node.props;
    let quality = |nearest: bool| if nearest { Quality::None } else { Quality::Medium };
    match &node.content {
        NodeContent::None => {}
        NodeContent::Image { image, size } => {
            let r = place(p.gravity, p.bounds, *size);
            let (iw, ih) = (image.width as f64, image.height as f64);
            let [cx, cy, cw, ch] = p.contents_rect;
            // The part of the image the contents rectangle takes: in unit
            // coordinates of the layer's space, so their y runs from the
            // image's bottom row where the layer's y runs up, and from its
            // top row where it runs down (measured, as the image turns).
            let (y0, y1) = if down { (cy, cy + ch) } else { (1.0 - cy - ch, 1.0 - cy) };
            let src = Rect::new((cx * iw) as f32, (y0 * ih) as f32, ((cx + cw) * iw) as f32, (y1 * ih) as f32);
            let top = if down { r[1] } else { r[1] + r[3] };
            let bottom = if down { r[1] + r[3] } else { r[1] };
            let dst = Rect::new(r[0] as f32, top as f32, (r[0] + r[2]) as f32, bottom as f32);
            let scale_up = (m[0] * m[3] - m[1] * m[2]).abs().sqrt() * r[2].max(1e-9) / (iw * cw).max(1e-9) > 1.0;
            out.push(Op::Image {
                image: image.clone(),
                src,
                dst,
                alpha: 1.0,
                quality: quality(if scale_up { p.mag_nearest } else { p.min_nearest }),
                tint: None,
                tiled: false,
                draw: clip.draw(m),
            });
        }
        NodeContent::Canvas { image, rect } => {
            let [x, y, w, h] = *rect;
            let (top, bottom) = if down { (y, y + h) } else { (y + h, y) };
            out.push(Op::Image {
                image: image.clone(),
                src: Rect::new(0.0, 0.0, image.width as f32, image.height as f32),
                dst: Rect::new(x as f32, top as f32, (x + w) as f32, bottom as f32),
                alpha: 1.0,
                quality: Quality::Medium,
                tint: None,
                tiled: false,
                draw: clip.draw(m),
            });
        }
    }
}

/// A shape layer's fill and stroke, the stroke trimmed to its start and
/// end.
fn emit_shape(s: &ShapeProps, m: &[f64; 6], clip: &Clip, out: &mut Vec<Op>) {
    let Some(shape) = &s.path else { return };
    if let Some(fill) = s.fill_color
        && fill[3] > 0.0
        && let Some(path) = shape.drawn()
    {
        out.push(Op::FillPath { path, even_odd: s.even_odd, paint: Paint::Solid(color(fill)), draw: clip.draw(m) });
    }
    let Some(stroke) = s.stroke_color else { return };
    if stroke[3] <= 0.0 || s.line_width <= 0.0 {
        return;
    }
    let (a, b) = (s.stroke_start.clamp(0.0, 1.0), s.stroke_end.clamp(0.0, 1.0));
    if b <= a {
        return;
    }
    let trimmed = if a <= 0.0 && b >= 1.0 { Some(shape.path.clone()) } else { trim(&shape.path, a, b) };
    let Some(path) = trimmed.as_ref().and_then(crate::path::to_skia) else { return };
    let spec = StrokeSpec {
        width: s.line_width as f32,
        cap: s.line_cap,
        join: s.line_join,
        miter: s.miter_limit as f32,
        dash: s
            .dash_pattern
            .as_ref()
            .filter(|d| !d.is_empty())
            .map(|d| (d.iter().map(|v| *v as f32).collect(), s.dash_phase as f32)),
    };
    out.push(Op::StrokePath { path, stroke: Arc::new(spec), paint: Paint::Solid(color(stroke)), draw: clip.draw(m) });
}

/// The part of a path from `a` to `b` of its length (0 to 1).
fn trim(path: &BezPath, a: f64, b: f64) -> Option<BezPath> {
    use kurbo::{ParamCurve, ParamCurveArclen, PathSeg};
    let segs: Vec<PathSeg> = path.segments().collect();
    let lengths: Vec<f64> = segs.iter().map(|s| s.arclen(1e-3)).collect();
    let total: f64 = lengths.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let (start, end) = (a * total, b * total);
    let mut out = BezPath::new();
    let mut at = 0.0;
    let mut open = false;
    for (seg, len) in segs.iter().zip(&lengths) {
        let (s0, s1) = (at, at + len);
        at = s1;
        if s1 <= start || s0 >= end || *len <= 0.0 {
            open = false;
            continue;
        }
        let t0 = if start > s0 { seg.inv_arclen(start - s0, 1e-3) } else { 0.0 };
        let t1 = if end < s1 { seg.inv_arclen(end - s0, 1e-3) } else { 1.0 };
        let part = seg.subsegment(t0..t1);
        if !open {
            out.move_to(part.start());
            open = true;
        }
        match part {
            PathSeg::Line(l) => out.line_to(l.p1),
            PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
            PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
        }
    }
    Some(out)
}

/// A gradient layer's gradient over its bounds (start and end in unit
/// coordinates of the bounds).
fn emit_gradient(p: &Props, g: &super::props::GradientProps, m: &[f64; 6], clip: &Clip, out: &mut Vec<Op>) {
    if g.colors.is_empty() {
        return;
    }
    let [x, y, w, h] = p.bounds;
    let n = g.colors.len();
    let stops: Vec<(f32, [f32; 4])> = g
        .colors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let loc = g.locations.as_ref().and_then(|l| l.get(i).copied()).unwrap_or(if n > 1 {
                i as f64 / (n - 1) as f64
            } else {
                0.0
            });
            (loc as f32, color(*c))
        })
        .collect();
    let at = |u: [f64; 2]| ((x + u[0] * w) as f32, (y + u[1] * h) as f32);
    let (start, end) = (at(g.start), at(g.end));
    if g.kind == GradientKind::Radial {
        // An ellipse about the start point, its radii the end point's
        // distances from it along each axis (measured); none when either
        // is 0.
        let (rx, ry) = (((g.end[0] - g.start[0]) * w).abs(), ((g.end[1] - g.start[1]) * h).abs());
        if rx <= 0.0 || ry <= 0.0 {
            return;
        }
        // Drawn as a circle of radius rx in a space squeezed along y about
        // the start point.
        let sy = start.1 as f64;
        let k = ry / rx;
        let squeeze = [1.0, 0.0, 0.0, k, 0.0, sy - k * sy];
        let spec = GradientSpec { stops, start, end: start, radii: Some((0.0, rx as f32)), extend: (true, true) };
        let (y0, y1) = (sy + (y - sy) / k, sy + (y + h - sy) / k);
        let Some(path) = crate::path::to_skia(&kurbo::Rect::new(x, y0.min(y1), x + w, y0.max(y1)).to_path(0.1)) else {
            return;
        };
        out.push(Op::FillPath {
            path,
            even_odd: false,
            paint: Paint::Gradient(Arc::new(spec)),
            draw: clip.draw(&compose(&squeeze, m)),
        });
        return;
    }
    let spec = GradientSpec { stops, start, end, radii: None, extend: (true, true) };
    let Some(path) = crate::path::to_skia(&kurbo::Rect::new(x, y, x + w, y + h).to_path(0.1)) else { return };
    out.push(Op::FillPath { path, even_odd: false, paint: Paint::Gradient(Arc::new(spec)), draw: clip.draw(m) });
}

// Contents as pixels.

thread_local! {
    static GLYPHS: std::cell::RefCell<crate::raster::Glyphs> = std::cell::RefCell::default();
}

/// Ops (in points from a top-left origin) drawn into `w` × `h` pixels at
/// `scale`, as an image.
pub fn rasterize(ops: &[Op], w: u32, h: u32, scale: f64) -> Option<Arc<ImageData>> {
    if w == 0 || h == 0 || (w as usize).checked_mul(h as usize)? > 64 << 20 {
        return None;
    }
    let mut px = vec![0u32; w as usize * h as usize];
    let mut canvas = crate::raster::Canvas::new(&mut px, w, h, 0.0, scale as f32);
    let damage = Rect::new(0.0, 0.0, w as f32 / scale as f32, h as f32 / scale as f32);
    GLYPHS.with(|g| match g.try_borrow_mut() {
        Ok(mut g) => crate::raster::paint(&mut canvas, &mut g, &[damage], ops),
        Err(_) => crate::raster::paint(&mut canvas, &mut Default::default(), &[damage], ops),
    });
    let bytes: Vec<u8> = px.iter().flat_map(|p| p.to_ne_bytes()).collect();
    Some(Arc::new(ImageData {
        key: crate::raster::images::next_key(),
        generation: 0,
        width: w,
        height: h,
        pixels: Pixels::Rgba(Arc::from(bytes)),
    }))
}

/// Expansion of a continuous corner beyond its radius (measured:
/// `+cornerCurveExpansionFactor:`).
pub const CONTINUOUS_EXPANSION: f64 = 1.528665;
