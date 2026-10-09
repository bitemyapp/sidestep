//! A layer's properties as plain data ([`Props`]), the keys naming them
//! ([`Key`], [`KeyPath`]) and the values animations carry ([`Value`]).
//!
//! The same `Props` is the model a `CALayer` keeps, the presentation its
//! animations make of it at a given time, and what the render thread
//! composites: nothing in it is an object, so it crosses threads. The
//! objects a layer was given (its `CGColor`s, `contents`, paths) stay with
//! the model layer, which hands them back as given.

use std::sync::Arc;

use super::math::{self, Components, IDENTITY, Mat};

/// Straight sRGB red, green, blue and alpha.
pub type Rgba = [f64; 4];

/// How contents fill a layer (`contentsGravity`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Gravity {
    Center,
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    #[default]
    Resize,
    ResizeAspect,
    ResizeAspectFill,
}

impl Gravity {
    pub const NAMES: [(&'static str, Gravity); 12] = [
        ("center", Gravity::Center),
        ("top", Gravity::Top),
        ("bottom", Gravity::Bottom),
        ("left", Gravity::Left),
        ("right", Gravity::Right),
        ("topLeft", Gravity::TopLeft),
        ("topRight", Gravity::TopRight),
        ("bottomLeft", Gravity::BottomLeft),
        ("bottomRight", Gravity::BottomRight),
        ("resize", Gravity::Resize),
        ("resizeAspect", Gravity::ResizeAspect),
        ("resizeAspectFill", Gravity::ResizeAspectFill),
    ];

    /// The gravity a name names, if it names one.
    pub fn known(name: &str) -> Option<Gravity> {
        Self::NAMES.iter().find(|(n, _)| *n == name).map(|(_, g)| *g)
    }
}

/// A layer's shape and gradient, for `CAShapeLayer` and `CAGradientLayer`.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeProps {
    pub path: Option<Arc<crate::path::Shape>>,
    pub fill_color: Option<Rgba>,
    pub even_odd: bool,
    pub stroke_color: Option<Rgba>,
    pub stroke_start: f64,
    pub stroke_end: f64,
    pub line_width: f64,
    pub miter_limit: f64,
    /// `CGLineCap` and `CGLineJoin` values.
    pub line_cap: u8,
    pub line_join: u8,
    pub dash_phase: f64,
    pub dash_pattern: Option<Vec<f64>>,
}

impl Default for ShapeProps {
    fn default() -> Self {
        ShapeProps {
            path: None,
            fill_color: Some([0.0, 0.0, 0.0, 1.0]),
            even_odd: false,
            stroke_color: None,
            stroke_start: 0.0,
            stroke_end: 1.0,
            line_width: 1.0,
            miter_limit: 10.0,
            line_cap: 0,
            line_join: 0,
            dash_phase: 0.0,
            dash_pattern: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GradientKind {
    #[default]
    Axial,
    Radial,
    Conic,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GradientProps {
    pub colors: Vec<Rgba>,
    pub locations: Option<Vec<f64>>,
    pub start: [f64; 2],
    pub end: [f64; 2],
    pub kind: GradientKind,
}

impl Default for GradientProps {
    fn default() -> Self {
        GradientProps {
            colors: Vec::new(),
            locations: None,
            start: [0.5, 0.0],
            end: [0.5, 1.0],
            kind: GradientKind::Axial,
        }
    }
}

/// What a layer is, besides a plain layer.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Kind {
    #[default]
    Plain,
    Shape(Box<ShapeProps>),
    Gradient(Box<GradientProps>),
}

/// A layer's timing, as it maps its superlayer's time to its own:
/// `(t - begin) · speed + offset`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerTime {
    pub begin: f64,
    pub speed: f64,
    pub offset: f64,
}

impl Default for LayerTime {
    fn default() -> Self {
        LayerTime { begin: 0.0, speed: 1.0, offset: 0.0 }
    }
}

impl LayerTime {
    pub fn local(&self, parent: f64) -> f64 {
        (parent - self.begin) * self.speed + self.offset
    }

    /// The superlayer's time for this layer's `t` (none while paused).
    pub fn parent(&self, t: f64) -> Option<f64> {
        (self.speed != 0.0).then(|| (t - self.offset) / self.speed + self.begin)
    }
}

/// Everything a layer draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct Props {
    pub bounds: [f64; 4],
    pub position: [f64; 2],
    pub z_position: f64,
    pub anchor: [f64; 2],
    pub anchor_z: f64,
    pub transform: Mat,
    pub sublayer_transform: Mat,
    pub hidden: bool,
    pub double_sided: bool,
    pub geometry_flipped: bool,
    pub masks_to_bounds: bool,
    pub contents_rect: [f64; 4],
    pub contents_center: [f64; 4],
    pub gravity: Gravity,
    pub contents_scale: f64,
    /// `kCAFilterNearest` for minifying and magnifying.
    pub min_nearest: bool,
    pub mag_nearest: bool,
    pub opaque: bool,
    pub background_color: Option<Rgba>,
    pub corner_radius: f64,
    /// `CACornerMask` bits.
    pub masked_corners: u8,
    pub continuous_corners: bool,
    pub border_width: f64,
    pub border_color: Option<Rgba>,
    pub opacity: f64,
    pub group_opacity: bool,
    pub shadow_color: Option<Rgba>,
    pub shadow_opacity: f64,
    pub shadow_offset: [f64; 2],
    pub shadow_radius: f64,
    pub shadow_path: Option<Arc<crate::path::Shape>>,
    pub time: LayerTime,
    pub kind: Kind,
}

impl Default for Props {
    fn default() -> Self {
        let black = Some([0.0, 0.0, 0.0, 1.0]);
        Props {
            bounds: [0.0; 4],
            position: [0.0; 2],
            z_position: 0.0,
            anchor: [0.5, 0.5],
            anchor_z: 0.0,
            transform: IDENTITY,
            sublayer_transform: IDENTITY,
            hidden: false,
            double_sided: true,
            geometry_flipped: false,
            masks_to_bounds: false,
            contents_rect: [0.0, 0.0, 1.0, 1.0],
            contents_center: [0.0, 0.0, 1.0, 1.0],
            gravity: Gravity::Resize,
            contents_scale: 1.0,
            min_nearest: false,
            mag_nearest: false,
            opaque: false,
            background_color: None,
            corner_radius: 0.0,
            masked_corners: 15,
            continuous_corners: false,
            border_width: 0.0,
            border_color: black,
            opacity: 1.0,
            group_opacity: true,
            shadow_color: black,
            shadow_opacity: 0.0,
            shadow_offset: [0.0, -3.0],
            shadow_radius: 3.0,
            shadow_path: None,
            time: LayerTime::default(),
            kind: Kind::Plain,
        }
    }
}

impl Props {
    /// `frame`: the bounds' box after the transform, in the superlayer.
    pub fn frame(&self) -> [f64; 4] {
        let [_, _, w, h] = self.bounds;
        let (ax, ay) = (self.anchor[0] * w, self.anchor[1] * h);
        let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in corners {
            let (px, py) = math::apply(&self.transform, x - ax, y - ay);
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
        [self.position[0] + x0, self.position[1] + y0, x1 - x0, y1 - y0]
    }

    /// Make `frame` `r` (standardized) as CoreAnimation does: the bounds'
    /// size is the frame's undone by the transform where it is affine and
    /// invertible, and the position is where the anchor falls in it.
    pub fn set_frame(&self, r: [f64; 4]) -> ([f64; 2], [f64; 4]) {
        let r = standardize(r);
        let mut size = [r[2], r[3]];
        if !math::is_affine(&self.transform) || self.transform[..2] != [1.0, 0.0] || self.transform[4..6] != [0.0, 1.0]
        {
            // The frame is the box around the transformed bounds: find the
            // size whose box that is, for scales and rotations alike.
            let t = &self.transform;
            let (a, b, c, d) = (t[0].abs(), t[1].abs(), t[4].abs(), t[5].abs());
            // |a|w + |c|h = W, |b|w + |d|h = H.
            let det = a * d - b * c;
            if det.abs() > 1e-12 {
                size = [(r[2] * d - r[3] * c) / det, (r[3] * a - r[2] * b) / det];
            } else if b == 0.0 && c == 0.0 {
                // A scale of 0 along an axis leaves no size there (measured).
                size = [if a > 0.0 { r[2] / a } else { 0.0 }, if d > 0.0 { r[3] / d } else { 0.0 }];
            }
        }
        let bounds = [self.bounds[0], self.bounds[1], size[0], size[1]];
        let position = [r[0] + self.anchor[0] * r[2], r[1] + self.anchor[1] * r[3]];
        (position, bounds)
    }

    /// The map from this layer's coordinates to its superlayer's, as an
    /// affine map in the plane (a, b, c, d, tx, ty): the bounds' origin,
    /// the flip about the bounds' middle when the geometry is flipped, the
    /// anchor, the transform and the position. The superlayer's
    /// `sublayerTransform` isn't part of it.
    pub fn to_superlayer(&self) -> [f64; 6] {
        let [bx, by, w, h] = self.bounds;
        // Layer point p → relative to the anchor point in the bounds.
        let (ax, ay) = (bx + self.anchor[0] * w, by + self.anchor[1] * h);
        let flip = self.geometry_flipped;
        // p' = (x - ax, fy(y) - ay) where fy flips about the bounds' middle.
        let (fa, fty) = if flip { (-1.0, 2.0 * by + h) } else { (1.0, 0.0) };
        // Pre-map: x → x - ax; y → fa·y + fty - ay.
        let pre = [1.0, 0.0, 0.0, fa, -ax, fty - ay];
        let t = math::plane_affine(&self.transform, -ax, fty - ay, w, h);
        let post = [1.0, 0.0, 0.0, 1.0, self.position[0], self.position[1]];
        compose(&compose(&pre, &t), &post)
    }
}

impl Props {
    /// The map from the space the layer's sublayers are placed in to its
    /// own: its `sublayerTransform` about its anchor point (measured), or
    /// none for the identity.
    pub fn sublayer_map(&self) -> Option<[f64; 6]> {
        if self.sublayer_transform == IDENTITY {
            return None;
        }
        let [bx, by, w, h] = self.bounds;
        let (ax, ay) = (bx + self.anchor[0] * w, by + self.anchor[1] * h);
        let t = math::plane_affine(&self.sublayer_transform, bx - ax, by - ay, w, h);
        Some(compose(&compose(&[1.0, 0.0, 0.0, 1.0, -ax, -ay], &t), &[1.0, 0.0, 0.0, 1.0, ax, ay]))
    }
}

/// Affine maps as (a, b, c, d, tx, ty), row vectors: `compose(m, n)`
/// applies `m` then `n`.
pub fn compose(m: &[f64; 6], n: &[f64; 6]) -> [f64; 6] {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

pub fn invert_affine(m: &[f64; 6]) -> Option<[f64; 6]> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)])
}

pub fn apply_affine(m: &[f64; 6], x: f64, y: f64) -> (f64, f64) {
    (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
}

/// A rectangle with a positive width and height.
pub fn standardize(r: [f64; 4]) -> [f64; 4] {
    let [mut x, mut y, mut w, mut h] = r;
    if w < 0.0 {
        x += w;
        w = -w;
    }
    if h < 0.0 {
        y += h;
        h = -h;
    }
    [x, y, w, h]
}

/// A value an animation interpolates, or a key's value in general.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    Point([f64; 2]),
    Size([f64; 2]),
    Rect([f64; 4]),
    Transform(Mat),
    /// None: no color (nil).
    Color(Option<Rgba>),
    Colors(Vec<Rgba>),
    Numbers(Vec<f64>),
    Path(Option<Arc<crate::path::Shape>>),
    Bool(bool),
}

impl Value {
    /// `self + f·(other − self)`; `None` for values that don't
    /// interpolate (the caller switches instead).
    pub fn lerp(&self, other: &Value, f: f64) -> Option<Value> {
        let l = |a: f64, b: f64| a + (b - a) * f;
        Some(match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(l(*a, *b)),
            (Value::Point(a), Value::Point(b)) => Value::Point([l(a[0], b[0]), l(a[1], b[1])]),
            (Value::Size(a), Value::Size(b)) => Value::Size([l(a[0], b[0]), l(a[1], b[1])]),
            (Value::Rect(a), Value::Rect(b)) => Value::Rect([0, 1, 2, 3].map(|i| l(a[i], b[i]))),
            (Value::Transform(a), Value::Transform(b)) => Value::Transform(math::interpolate(a, b, f)),
            // No color at either end shows none (measured).
            (Value::Color(Some(a)), Value::Color(Some(b))) => Value::Color(Some([0, 1, 2, 3].map(|i| l(a[i], b[i])))),
            (Value::Color(_), Value::Color(_)) => Value::Color(None),
            (Value::Colors(a), Value::Colors(b)) if a.len() == b.len() => {
                Value::Colors(a.iter().zip(b).map(|(a, b)| [0, 1, 2, 3].map(|i| l(a[i], b[i]))).collect())
            }
            (Value::Numbers(a), Value::Numbers(b)) if a.len() == b.len() => {
                Value::Numbers(a.iter().zip(b).map(|(a, b)| l(*a, *b)).collect())
            }
            (Value::Path(Some(a)), Value::Path(Some(b))) => Value::Path(lerp_path(a, b, f).map(Arc::new)),
            _ => return None,
        })
    }

    /// `self + other` (additive and cumulative animations).
    pub fn add(&self, other: &Value) -> Option<Value> {
        Some(match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a + b),
            (Value::Point(a), Value::Point(b)) => Value::Point([a[0] + b[0], a[1] + b[1]]),
            (Value::Size(a), Value::Size(b)) => Value::Size([a[0] + b[0], a[1] + b[1]]),
            (Value::Rect(a), Value::Rect(b)) => Value::Rect([0, 1, 2, 3].map(|i| a[i] + b[i])),
            (Value::Transform(a), Value::Transform(b)) => Value::Transform(math::concat(b, a)),
            (Value::Color(a), Value::Color(b)) => {
                let (a, b) = (a.unwrap_or([0.0; 4]), b.unwrap_or([0.0; 4]));
                Value::Color(Some([0, 1, 2, 3].map(|i| a[i] + b[i])))
            }
            (Value::Numbers(a), Value::Numbers(b)) if a.len() == b.len() => {
                Value::Numbers(a.iter().zip(b).map(|(a, b)| a + b).collect())
            }
            _ => return None,
        })
    }

    /// `self − other` (a `to` and a `by` give the `from`).
    pub fn sub(&self, other: &Value) -> Option<Value> {
        Some(match (self, other) {
            (Value::Number(a), Value::Number(b)) => Value::Number(a - b),
            (Value::Point(a), Value::Point(b)) => Value::Point([a[0] - b[0], a[1] - b[1]]),
            (Value::Size(a), Value::Size(b)) => Value::Size([a[0] - b[0], a[1] - b[1]]),
            (Value::Rect(a), Value::Rect(b)) => Value::Rect([0, 1, 2, 3].map(|i| a[i] - b[i])),
            (Value::Transform(a), Value::Transform(b)) => Value::Transform(math::concat(a, &math::invert(b)?)),
            (Value::Color(a), Value::Color(b)) => {
                let (a, b) = (a.unwrap_or([0.0; 4]), b.unwrap_or([0.0; 4]));
                Value::Color(Some([0, 1, 2, 3].map(|i| a[i] - b[i])))
            }
            (Value::Numbers(a), Value::Numbers(b)) if a.len() == b.len() => {
                Value::Numbers(a.iter().zip(b).map(|(a, b)| a - b).collect())
            }
            _ => return None,
        })
    }

    /// `self` scaled by `k` (cumulative animations add whole repeats).
    pub fn scaled(&self, k: f64) -> Option<Value> {
        Some(match self {
            Value::Number(a) => Value::Number(a * k),
            Value::Point(a) => Value::Point([a[0] * k, a[1] * k]),
            Value::Size(a) => Value::Size([a[0] * k, a[1] * k]),
            Value::Rect(a) => Value::Rect(a.map(|v| v * k)),
            Value::Numbers(a) => Value::Numbers(a.iter().map(|v| v * k).collect()),
            _ => return None,
        })
    }

    /// The distance between two values, for paced keyframes.
    pub fn distance(&self, other: &Value) -> f64 {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => (a - b).abs(),
            (Value::Point(a), Value::Point(b)) | (Value::Size(a), Value::Size(b)) => (a[0] - b[0]).hypot(a[1] - b[1]),
            (Value::Rect(a), Value::Rect(b)) => (0..4).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt(),
            (Value::Color(a), Value::Color(b)) => {
                let (a, b) = (a.unwrap_or([0.0; 4]), b.unwrap_or([0.0; 4]));
                (0..4).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
            }
            (Value::Transform(a), Value::Transform(b)) => (0..16).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt(),
            _ => 0.0,
        }
    }

    /// The numbers of a value, for cubic keyframe splines.
    pub fn components(&self) -> Option<Vec<f64>> {
        Some(match self {
            Value::Number(a) => vec![*a],
            Value::Point(a) | Value::Size(a) => a.to_vec(),
            Value::Rect(a) => a.to_vec(),
            Value::Color(a) => a.unwrap_or([0.0; 4]).to_vec(),
            _ => return None,
        })
    }

    /// A value of the same kind as `self` with these numbers.
    pub fn with_components(&self, c: &[f64]) -> Value {
        match self {
            Value::Number(_) => Value::Number(c[0]),
            Value::Point(_) => Value::Point([c[0], c[1]]),
            Value::Size(_) => Value::Size([c[0], c[1]]),
            Value::Rect(_) => Value::Rect([c[0], c[1], c[2], c[3]]),
            Value::Color(_) => Value::Color(Some([c[0], c[1], c[2], c[3]])),
            other => other.clone(),
        }
    }
}

/// Two paths with the same elements, interpolated point by point.
fn lerp_path(a: &crate::path::Shape, b: &crate::path::Shape, f: f64) -> Option<crate::path::Shape> {
    use kurbo::PathEl;
    let (ea, eb) = (a.elements(), b.elements());
    if ea.len() != eb.len() {
        return None;
    }
    let p = |x: kurbo::Point, y: kurbo::Point| x.lerp(y, f);
    let mut out = kurbo::BezPath::new();
    for (x, y) in ea.iter().zip(eb) {
        out.push(match (x, y) {
            (PathEl::MoveTo(a), PathEl::MoveTo(b)) => PathEl::MoveTo(p(*a, *b)),
            (PathEl::LineTo(a), PathEl::LineTo(b)) => PathEl::LineTo(p(*a, *b)),
            (PathEl::QuadTo(a1, a2), PathEl::QuadTo(b1, b2)) => PathEl::QuadTo(p(*a1, *b1), p(*a2, *b2)),
            (PathEl::CurveTo(a1, a2, a3), PathEl::CurveTo(b1, b2, b3)) => {
                PathEl::CurveTo(p(*a1, *b1), p(*a2, *b2), p(*a3, *b3))
            }
            (PathEl::ClosePath, PathEl::ClosePath) => PathEl::ClosePath,
            _ => return None,
        });
    }
    Some(crate::path::Shape::from_path(out))
}

/// A layer property a key names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Bounds,
    Position,
    ZPosition,
    AnchorPoint,
    AnchorPointZ,
    Transform,
    SublayerTransform,
    Hidden,
    DoubleSided,
    GeometryFlipped,
    MasksToBounds,
    Contents,
    ContentsRect,
    ContentsCenter,
    ContentsScale,
    BackgroundColor,
    CornerRadius,
    BorderWidth,
    BorderColor,
    Opacity,
    ShadowColor,
    ShadowOpacity,
    ShadowOffset,
    ShadowRadius,
    ShadowPath,
    Mask,
    Sublayers,
    Filters,
    BackgroundFilters,
    CompositingFilter,
    // CAShapeLayer.
    Path,
    FillColor,
    StrokeColor,
    StrokeStart,
    StrokeEnd,
    LineWidth,
    MiterLimit,
    LineDashPhase,
    // CAGradientLayer.
    Colors,
    Locations,
    StartPoint,
    EndPoint,
}

impl Key {
    pub const ALL: [(&'static str, Key); 42] = [
        ("bounds", Key::Bounds),
        ("position", Key::Position),
        ("zPosition", Key::ZPosition),
        ("anchorPoint", Key::AnchorPoint),
        ("anchorPointZ", Key::AnchorPointZ),
        ("transform", Key::Transform),
        ("sublayerTransform", Key::SublayerTransform),
        ("hidden", Key::Hidden),
        ("doubleSided", Key::DoubleSided),
        ("geometryFlipped", Key::GeometryFlipped),
        ("masksToBounds", Key::MasksToBounds),
        ("contents", Key::Contents),
        ("contentsRect", Key::ContentsRect),
        ("contentsCenter", Key::ContentsCenter),
        ("contentsScale", Key::ContentsScale),
        ("backgroundColor", Key::BackgroundColor),
        ("cornerRadius", Key::CornerRadius),
        ("borderWidth", Key::BorderWidth),
        ("borderColor", Key::BorderColor),
        ("opacity", Key::Opacity),
        ("shadowColor", Key::ShadowColor),
        ("shadowOpacity", Key::ShadowOpacity),
        ("shadowOffset", Key::ShadowOffset),
        ("shadowRadius", Key::ShadowRadius),
        ("shadowPath", Key::ShadowPath),
        ("mask", Key::Mask),
        ("sublayers", Key::Sublayers),
        ("filters", Key::Filters),
        ("backgroundFilters", Key::BackgroundFilters),
        ("compositingFilter", Key::CompositingFilter),
        ("path", Key::Path),
        ("fillColor", Key::FillColor),
        ("strokeColor", Key::StrokeColor),
        ("strokeStart", Key::StrokeStart),
        ("strokeEnd", Key::StrokeEnd),
        ("lineWidth", Key::LineWidth),
        ("miterLimit", Key::MiterLimit),
        ("lineDashPhase", Key::LineDashPhase),
        ("colors", Key::Colors),
        ("locations", Key::Locations),
        ("startPoint", Key::StartPoint),
        ("endPoint", Key::EndPoint),
    ];

    pub fn named(name: &str) -> Option<Key> {
        Self::ALL.iter().find(|(n, _)| *n == name).map(|(_, k)| *k)
    }

    pub fn name(self) -> &'static str {
        Self::ALL.iter().find(|(_, k)| *k == self).map_or("", |(n, _)| n)
    }
}

/// A part of a key's value that a key path names (`position.x`,
/// `transform.rotation.z`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Whole,
    X,
    Y,
    Width,
    Height,
    Origin,
    Size,
    OriginX,
    OriginY,
    SizeWidth,
    SizeHeight,
    Scale,
    ScaleX,
    ScaleY,
    ScaleZ,
    RotationX,
    RotationY,
    RotationZ,
    Translation,
    TranslationX,
    TranslationY,
    TranslationZ,
}

/// A key path an animation or key-value coding names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyPath {
    pub key: Key,
    pub part: Part,
}

impl KeyPath {
    /// A key path of a layer property, or `None` for any other.
    pub fn parse(path: &str) -> Option<KeyPath> {
        let (head, rest) = path.split_once('.').unwrap_or((path, ""));
        let key = Key::named(head)?;
        let part = match (key, rest) {
            (_, "") => Part::Whole,
            (Key::Position | Key::AnchorPoint | Key::StartPoint | Key::EndPoint, "x") => Part::X,
            (Key::Position | Key::AnchorPoint | Key::StartPoint | Key::EndPoint, "y") => Part::Y,
            (Key::ShadowOffset, "width") => Part::Width,
            (Key::ShadowOffset, "height") => Part::Height,
            (Key::Bounds | Key::ContentsRect | Key::ContentsCenter, "origin") => Part::Origin,
            (Key::Bounds | Key::ContentsRect | Key::ContentsCenter, "size") => Part::Size,
            (Key::Bounds | Key::ContentsRect | Key::ContentsCenter, "origin.x") => Part::OriginX,
            (Key::Bounds | Key::ContentsRect | Key::ContentsCenter, "origin.y") => Part::OriginY,
            (Key::Bounds | Key::ContentsRect | Key::ContentsCenter, "size.width") => Part::SizeWidth,
            (Key::Bounds | Key::ContentsRect | Key::ContentsCenter, "size.height") => Part::SizeHeight,
            (Key::Transform | Key::SublayerTransform, t) => match t {
                "scale" => Part::Scale,
                "scale.x" => Part::ScaleX,
                "scale.y" => Part::ScaleY,
                "scale.z" => Part::ScaleZ,
                "rotation" | "rotation.z" => Part::RotationZ,
                "rotation.x" => Part::RotationX,
                "rotation.y" => Part::RotationY,
                "translation" => Part::Translation,
                "translation.x" => Part::TranslationX,
                "translation.y" => Part::TranslationY,
                "translation.z" => Part::TranslationZ,
                _ => return None,
            },
            _ => return None,
        };
        Some(KeyPath { key, part })
    }
}

impl Props {
    fn shape(&self) -> Option<&ShapeProps> {
        match &self.kind {
            Kind::Shape(s) => Some(s),
            _ => None,
        }
    }

    fn shape_mut(&mut self) -> Option<&mut ShapeProps> {
        match &mut self.kind {
            Kind::Shape(s) => Some(s),
            _ => None,
        }
    }

    fn gradient(&self) -> Option<&GradientProps> {
        match &self.kind {
            Kind::Gradient(g) => Some(g),
            _ => None,
        }
    }

    fn gradient_mut(&mut self) -> Option<&mut GradientProps> {
        match &mut self.kind {
            Kind::Gradient(g) => Some(g),
            _ => None,
        }
    }

    /// A whole key's value, where it is data.
    pub fn get(&self, key: Key) -> Option<Value> {
        Some(match key {
            Key::Bounds => Value::Rect(self.bounds),
            Key::Position => Value::Point(self.position),
            Key::ZPosition => Value::Number(self.z_position),
            Key::AnchorPoint => Value::Point(self.anchor),
            Key::AnchorPointZ => Value::Number(self.anchor_z),
            Key::Transform => Value::Transform(self.transform),
            Key::SublayerTransform => Value::Transform(self.sublayer_transform),
            Key::Hidden => Value::Bool(self.hidden),
            Key::DoubleSided => Value::Bool(self.double_sided),
            Key::GeometryFlipped => Value::Bool(self.geometry_flipped),
            Key::MasksToBounds => Value::Bool(self.masks_to_bounds),
            Key::ContentsRect => Value::Rect(self.contents_rect),
            Key::ContentsCenter => Value::Rect(self.contents_center),
            Key::ContentsScale => Value::Number(self.contents_scale),
            Key::BackgroundColor => Value::Color(self.background_color),
            Key::CornerRadius => Value::Number(self.corner_radius),
            Key::BorderWidth => Value::Number(self.border_width),
            Key::BorderColor => Value::Color(self.border_color),
            Key::Opacity => Value::Number(self.opacity),
            Key::ShadowColor => Value::Color(self.shadow_color),
            Key::ShadowOpacity => Value::Number(self.shadow_opacity),
            Key::ShadowOffset => Value::Size(self.shadow_offset),
            Key::ShadowRadius => Value::Number(self.shadow_radius),
            Key::ShadowPath => Value::Path(self.shadow_path.clone()),
            Key::Path => Value::Path(self.shape()?.path.clone()),
            Key::FillColor => Value::Color(self.shape()?.fill_color),
            Key::StrokeColor => Value::Color(self.shape()?.stroke_color),
            Key::StrokeStart => Value::Number(self.shape()?.stroke_start),
            Key::StrokeEnd => Value::Number(self.shape()?.stroke_end),
            Key::LineWidth => Value::Number(self.shape()?.line_width),
            Key::MiterLimit => Value::Number(self.shape()?.miter_limit),
            Key::LineDashPhase => Value::Number(self.shape()?.dash_phase),
            Key::Colors => Value::Colors(self.gradient()?.colors.clone()),
            Key::Locations => Value::Numbers(self.gradient()?.locations.clone().unwrap_or_default()),
            Key::StartPoint => Value::Point(self.gradient()?.start),
            Key::EndPoint => Value::Point(self.gradient()?.end),
            Key::Contents
            | Key::Mask
            | Key::Sublayers
            | Key::Filters
            | Key::BackgroundFilters
            | Key::CompositingFilter => {
                return None;
            }
        })
    }

    /// Set a whole key's value, where it is data and of the key's kind.
    pub fn set(&mut self, key: Key, v: Value) {
        match (key, v) {
            (Key::Bounds, Value::Rect(r)) => self.bounds = r,
            (Key::Position, Value::Point(p)) => self.position = p,
            (Key::ZPosition, Value::Number(n)) => self.z_position = n,
            (Key::AnchorPoint, Value::Point(p)) => self.anchor = p,
            (Key::AnchorPointZ, Value::Number(n)) => self.anchor_z = n,
            (Key::Transform, Value::Transform(m)) => self.transform = m,
            (Key::SublayerTransform, Value::Transform(m)) => self.sublayer_transform = m,
            (Key::Hidden, Value::Bool(b)) => self.hidden = b,
            (Key::DoubleSided, Value::Bool(b)) => self.double_sided = b,
            (Key::GeometryFlipped, Value::Bool(b)) => self.geometry_flipped = b,
            (Key::MasksToBounds, Value::Bool(b)) => self.masks_to_bounds = b,
            (Key::ContentsRect, Value::Rect(r)) => self.contents_rect = r,
            (Key::ContentsCenter, Value::Rect(r)) => self.contents_center = r,
            (Key::ContentsScale, Value::Number(n)) => self.contents_scale = n,
            (Key::BackgroundColor, Value::Color(c)) => self.background_color = c,
            (Key::CornerRadius, Value::Number(n)) => self.corner_radius = n,
            (Key::BorderWidth, Value::Number(n)) => self.border_width = n,
            (Key::BorderColor, Value::Color(c)) => self.border_color = c,
            (Key::Opacity, Value::Number(n)) => self.opacity = n,
            (Key::ShadowColor, Value::Color(c)) => self.shadow_color = c,
            (Key::ShadowOpacity, Value::Number(n)) => self.shadow_opacity = n,
            (Key::ShadowOffset, Value::Size(s)) => self.shadow_offset = s,
            (Key::ShadowRadius, Value::Number(n)) => self.shadow_radius = n,
            (Key::ShadowPath, Value::Path(p)) => self.shadow_path = p,
            (Key::Path, Value::Path(p)) => {
                if let Some(s) = self.shape_mut() {
                    s.path = p
                }
            }
            (Key::FillColor, Value::Color(c)) => {
                if let Some(s) = self.shape_mut() {
                    s.fill_color = c
                }
            }
            (Key::StrokeColor, Value::Color(c)) => {
                if let Some(s) = self.shape_mut() {
                    s.stroke_color = c
                }
            }
            (Key::StrokeStart, Value::Number(n)) => {
                if let Some(s) = self.shape_mut() {
                    s.stroke_start = n
                }
            }
            (Key::StrokeEnd, Value::Number(n)) => {
                if let Some(s) = self.shape_mut() {
                    s.stroke_end = n
                }
            }
            (Key::LineWidth, Value::Number(n)) => {
                if let Some(s) = self.shape_mut() {
                    s.line_width = n
                }
            }
            (Key::MiterLimit, Value::Number(n)) => {
                if let Some(s) = self.shape_mut() {
                    s.miter_limit = n
                }
            }
            (Key::LineDashPhase, Value::Number(n)) => {
                if let Some(s) = self.shape_mut() {
                    s.dash_phase = n
                }
            }
            (Key::Colors, Value::Colors(c)) => {
                if let Some(g) = self.gradient_mut() {
                    g.colors = c
                }
            }
            (Key::Locations, Value::Numbers(l)) => {
                if let Some(g) = self.gradient_mut() {
                    g.locations = Some(l)
                }
            }
            (Key::StartPoint, Value::Point(p)) => {
                if let Some(g) = self.gradient_mut() {
                    g.start = p
                }
            }
            (Key::EndPoint, Value::Point(p)) => {
                if let Some(g) = self.gradient_mut() {
                    g.end = p
                }
            }
            _ => {}
        }
    }

    /// The value at a key path (a part of a key's value).
    pub fn get_path(&self, path: KeyPath) -> Option<Value> {
        let whole = self.get(path.key)?;
        part_of(&whole, path.part)
    }

    /// Set the value at a key path.
    pub fn set_path(&mut self, path: KeyPath, v: Value) {
        if path.part == Part::Whole {
            self.set(path.key, v);
            return;
        }
        let Some(whole) = self.get(path.key) else { return };
        if let Some(new) = with_part(&whole, path.part, &v) {
            self.set(path.key, new);
        }
    }
}

/// A part of a value (`x` of a point, the scale of a transform, …).
pub fn part_of(whole: &Value, part: Part) -> Option<Value> {
    Some(match (whole, part) {
        (v, Part::Whole) => v.clone(),
        (Value::Point(p), Part::X) => Value::Number(p[0]),
        (Value::Point(p), Part::Y) => Value::Number(p[1]),
        (Value::Size(s), Part::Width) => Value::Number(s[0]),
        (Value::Size(s), Part::Height) => Value::Number(s[1]),
        (Value::Rect(r), Part::Origin) => Value::Point([r[0], r[1]]),
        (Value::Rect(r), Part::Size) => Value::Size([r[2], r[3]]),
        (Value::Rect(r), Part::OriginX) => Value::Number(r[0]),
        (Value::Rect(r), Part::OriginY) => Value::Number(r[1]),
        (Value::Rect(r), Part::SizeWidth) => Value::Number(r[2]),
        (Value::Rect(r), Part::SizeHeight) => Value::Number(r[3]),
        (Value::Transform(m), part) => {
            let c = Components::of(m);
            match part {
                Part::Scale => Value::Number((c.scale[0] + c.scale[1] + c.scale[2]) / 3.0),
                Part::ScaleX => Value::Number(c.scale[0]),
                Part::ScaleY => Value::Number(c.scale[1]),
                Part::ScaleZ => Value::Number(c.scale[2]),
                Part::RotationX => Value::Number(c.rotate[0]),
                Part::RotationY => Value::Number(c.rotate[1]),
                Part::RotationZ => Value::Number(c.rotate[2]),
                Part::Translation => Value::Size([c.translate[0], c.translate[1]]),
                Part::TranslationX => Value::Number(c.translate[0]),
                Part::TranslationY => Value::Number(c.translate[1]),
                Part::TranslationZ => Value::Number(c.translate[2]),
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// `whole` with its `part` replaced by `v`.
pub fn with_part(whole: &Value, part: Part, v: &Value) -> Option<Value> {
    let n = match v {
        Value::Number(n) => Some(*n),
        _ => None,
    };
    Some(match (whole, part) {
        (_, Part::Whole) => v.clone(),
        (Value::Point(p), Part::X) => Value::Point([n?, p[1]]),
        (Value::Point(p), Part::Y) => Value::Point([p[0], n?]),
        (Value::Size(s), Part::Width) => Value::Size([n?, s[1]]),
        (Value::Size(s), Part::Height) => Value::Size([s[0], n?]),
        (Value::Rect(r), Part::Origin) => match v {
            Value::Point(p) => Value::Rect([p[0], p[1], r[2], r[3]]),
            _ => return None,
        },
        (Value::Rect(r), Part::Size) => match v {
            Value::Size(s) => Value::Rect([r[0], r[1], s[0], s[1]]),
            _ => return None,
        },
        (Value::Rect(r), Part::OriginX) => Value::Rect([n?, r[1], r[2], r[3]]),
        (Value::Rect(r), Part::OriginY) => Value::Rect([r[0], n?, r[2], r[3]]),
        (Value::Rect(r), Part::SizeWidth) => Value::Rect([r[0], r[1], n?, r[3]]),
        (Value::Rect(r), Part::SizeHeight) => Value::Rect([r[0], r[1], r[2], n?]),
        (Value::Transform(m), part) => {
            let mut c = Components::of(m);
            match (part, v) {
                (Part::Scale, _) => c.scale = [n?; 3],
                (Part::ScaleX, _) => c.scale[0] = n?,
                (Part::ScaleY, _) => c.scale[1] = n?,
                (Part::ScaleZ, _) => c.scale[2] = n?,
                (Part::RotationX, _) => c.rotate[0] = n?,
                (Part::RotationY, _) => c.rotate[1] = n?,
                (Part::RotationZ, _) => c.rotate[2] = n?,
                (Part::Translation, Value::Size(s)) => {
                    c.translate[0] = s[0];
                    c.translate[1] = s[1];
                }
                (Part::TranslationX, _) => c.translate[0] = n?,
                (Part::TranslationY, _) => c.translate[1] = n?,
                (Part::TranslationZ, _) => c.translate[2] = n?,
                _ => return None,
            }
            Value::Transform(c.matrix())
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_follow_core_animation() {
        // Measured on macOS.
        let mut p = Props::default();
        let (pos, b) = p.set_frame([10.0, 20.0, 100.0, 50.0]);
        p.position = pos;
        p.bounds = b;
        assert_eq!(p.position, [60.0, 45.0]);
        p.anchor = [0.0, 0.0];
        assert_eq!(p.frame(), [60.0, 45.0, 100.0, 50.0]);
        p.bounds = [5.0, 5.0, 200.0, 100.0];
        p.transform = math::scale(2.0, 3.0, 1.0);
        assert_eq!(p.frame(), [60.0, 45.0, 400.0, 300.0]);
        let (pos, b) = p.set_frame([0.0, 0.0, 10.0, 10.0]);
        assert_eq!(pos, [0.0, 0.0]);
        assert!((b[2] - 5.0).abs() < 1e-9 && (b[3] - 10.0 / 3.0).abs() < 1e-9 && b[0] == 5.0);
        let mut q = Props::default();
        let (pos, b) = q.set_frame([10.0, 10.0, -20.0, -30.0]);
        q.position = pos;
        q.bounds = b;
        assert_eq!(
            (q.bounds, q.position, q.frame()),
            ([0.0, 0.0, 20.0, 30.0], [0.0, -5.0], [-10.0, -20.0, 20.0, 30.0])
        );
    }

    #[test]
    fn key_paths_reach_parts() {
        let mut p = Props { bounds: [0.0, 0.0, 100.0, 50.0], ..Props::default() };
        let path = KeyPath::parse("bounds.size.width").expect("a key path");
        assert_eq!(p.get_path(path), Some(Value::Number(100.0)));
        p.set_path(path, Value::Number(9.0));
        assert_eq!(p.bounds, [0.0, 0.0, 9.0, 50.0]);
        let scale = KeyPath::parse("transform.scale").expect("a key path");
        p.set_path(scale, Value::Number(2.0));
        assert_eq!(p.transform, math::scale(2.0, 2.0, 2.0));
        let rot = KeyPath::parse("transform.rotation.z").expect("a key path");
        p.set_path(rot, Value::Number(0.5));
        assert!((p.transform[0] - 1.7551651237807455).abs() < 1e-12);
        assert_eq!(p.get_path(scale), Some(Value::Number(2.0)));
        assert!(KeyPath::parse("transform.bogus").is_none());
        assert!(KeyPath::parse("nothing").is_none());
    }

    #[test]
    fn flipped_geometry_turns_about_the_middle() {
        let p = Props {
            bounds: [0.0, 0.0, 100.0, 100.0],
            position: [50.0, 50.0],
            geometry_flipped: true,
            ..Props::default()
        };
        let m = p.to_superlayer();
        // The point 10 up from the bottom is 10 down from the top.
        assert_eq!(apply_affine(&m, 90.0, 10.0), (90.0, 90.0));
    }
}
