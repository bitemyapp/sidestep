//! `NSGradient`: colors at locations from 0 to 1, drawn along a line or
//! between two circles.
//!
//! A gradient keeps its colors as given, so a dynamic or system color
//! resolves when the gradient is drawn, in the drawing appearance of the
//! moment. Stops are sorted by location, and colors interpolate in the
//! gradient's space (sRGB unless given another; there's no color
//! management, so every RGB space interpolates the same).
//!
//! Drawing records a path filled with a gradient paint, which tiny-skia
//! renders (two-point conical gradients for the radial forms). What's
//! filled follows AppKit: `drawInRect:angle:` and the path forms fill the
//! rectangle or path; `drawFromPoint:toPoint:options:` and
//! `drawFromCenter:…options:` fill the whole clip, but only the band the
//! gradient runs through unless the options extend it before its start or
//! after its end.

use std::sync::Arc;

use kurbo::{BezPath, Point, Shape, Vec2};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSBezierPath, NSColor, NSColorSpace, NSGradient, NSGradientDrawingOptions, NSWindingRule};
use objc2_foundation::{NSArray, NSCopying, NSInteger, NSPoint, NSRect, NSZone};

use crate::color::Space;
use crate::protocol::{Color, GradientSpec, Op, Paint};

sidestep_runtime::static_class!(pub NSGRADIENT, NSGRADIENT_META = "NSGradient", || {
    let _ = NSGradientImpl::class();
});

pub(crate) struct GradientIvars {
    /// Colors as given, by location.
    stops: Vec<(Retained<NSColor>, f64)>,
    space: Space,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a gradient doesn't
    // change once made.
    #[unsafe(super(NSObject))]
    #[name = "NSGradient"]
    #[ivars = GradientIvars]
    pub(crate) struct NSGradientImpl;

    impl NSGradientImpl {
        #[unsafe(method_id(initWithStartingColor:endingColor:))]
        fn init_start_end(this: Allocated<Self>, start: &NSColor, end: &NSColor) -> Option<Retained<Self>> {
            Some(init(this, vec![(start.retain(), 0.0), (end.retain(), 1.0)], Space::Srgb))
        }

        #[unsafe(method_id(initWithColors:))]
        fn init_colors(this: Allocated<Self>, colors: &NSArray<NSColor>) -> Option<Retained<Self>> {
            Some(init(this, spaced(colors.to_vec()), Space::Srgb))
        }

        #[unsafe(method_id(initWithColors:atLocations:colorSpace:))]
        fn init_colors_locations(
            this: Allocated<Self>,
            colors: &NSArray<NSColor>,
            locations: *const f64,
            space: &NSColorSpace,
        ) -> Option<Retained<Self>> {
            let colors = colors.to_vec();
            let stops = if locations.is_null() {
                spaced(colors)
            } else {
                // SAFETY: the caller passes a location for every color.
                let at = |i: usize| unsafe { *locations.add(i) };
                colors.into_iter().enumerate().map(|(i, c)| (c, at(i))).collect()
            };
            Some(init(this, stops, crate::color::space_of(space)))
        }

        #[unsafe(method_id(colorSpace))]
        fn color_space(&self) -> Retained<NSColorSpace> {
            crate::color::space(self.ivars().space)
        }

        #[unsafe(method(numberOfColorStops))]
        fn number_of_color_stops(&self) -> NSInteger {
            self.ivars().stops.len() as NSInteger
        }

        #[unsafe(method(getColor:location:atIndex:))]
        fn get_color(&self, color: *mut *mut NSColor, location: *mut f64, index: NSInteger) {
            let stops = &self.ivars().stops;
            let Some((c, at)) = usize::try_from(index).ok().and_then(|i| stops.get(i)) else {
                panic!("*** -[NSGradient getColor:location:atIndex:]: index ({index}) beyond bounds");
            };
            // A component color comes back in the gradient's space; a
            // system or dynamic one as it is.
            let c = crate::color::in_space(c, self.ivars().space);
            // SAFETY: each pointer is null or writable; the color goes out
            // autoreleased, as out-parameters are.
            unsafe {
                if !color.is_null() {
                    *color = Retained::autorelease_ptr(c);
                }
                if !location.is_null() {
                    *location = *at;
                }
            }
        }

        #[unsafe(method_id(interpolatedColorAtLocation:))]
        fn interpolated_color_at_location(&self, location: f64) -> Retained<NSColor> {
            let c = sample(&self.resolved(), location as f32);
            let c = c.map(f64::from);
            NSColor::colorWithSRGBRed_green_blue_alpha(c[0], c[1], c[2], c[3])
        }

        #[unsafe(method(drawFromPoint:toPoint:options:))]
        fn draw_from_point(&self, start: NSPoint, end: NSPoint, options: NSGradientDrawingOptions) {
            self.linear_band(pt(start), pt(end), options);
        }

        #[unsafe(method(drawInRect:angle:))]
        fn draw_in_rect_angle(&self, rect: NSRect, angle: f64) {
            self.fill_at_angle(&rect_path(rect), rect, angle, false);
        }

        #[unsafe(method(drawInBezierPath:angle:))]
        fn draw_in_path_angle(&self, path: &NSBezierPath, angle: f64) {
            let (shape, even_odd) = path_shape(path);
            self.fill_at_angle(&shape, path.bounds(), angle, even_odd);
        }

        #[unsafe(method(drawFromCenter:radius:toCenter:radius:options:))]
        fn draw_radial(&self, c0: NSPoint, r0: f64, c1: NSPoint, r1: f64, options: NSGradientDrawingOptions) {
            self.radial_band(pt(c0), r0.max(0.0), pt(c1), r1.max(0.0), options);
        }

        #[unsafe(method(drawInRect:relativeCenterPosition:))]
        fn draw_in_rect_relative(&self, rect: NSRect, relative: NSPoint) {
            self.fill_radial_relative(&rect_path(rect), rect, relative, false);
        }

        #[unsafe(method(drawInBezierPath:relativeCenterPosition:))]
        fn draw_in_path_relative(&self, path: &NSBezierPath, relative: NSPoint) {
            let (shape, even_odd) = path_shape(path);
            self.fill_radial_relative(&shape, path.bounds(), relative, even_odd);
        }
    }

    impl NSGradientImpl {
        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSGradient> {
            // Immutable: a copy is the gradient itself.
            // SAFETY: NSGradientImpl is the class NSGradient names.
            unsafe { Retained::cast_unchecked(self.retain()) }
        }
    }

    unsafe impl NSObjectProtocol for NSGradientImpl {}

    unsafe impl NSCopying for NSGradientImpl {}
);

fn init(
    this: Allocated<NSGradientImpl>,
    mut stops: Vec<(Retained<NSColor>, f64)>,
    space: Space,
) -> Retained<NSGradientImpl> {
    // AppKit makes a gradient of any number of colors: none is clear, one
    // is that color throughout.
    match stops.len() {
        0 => stops = vec![(NSColor::clearColor(), 0.0), (NSColor::clearColor(), 1.0)],
        1 => stops.push((stops[0].0.clone(), 1.0)),
        _ => {}
    }
    stops.sort_by(|a, b| a.1.total_cmp(&b.1));
    let this = this.set_ivars(GradientIvars { stops, space });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// Colors spread evenly from 0 to 1.
fn spaced(colors: Vec<Retained<NSColor>>) -> Vec<(Retained<NSColor>, f64)> {
    let last = colors.len().saturating_sub(1).max(1) as f64;
    colors.into_iter().enumerate().map(|(i, c)| (c, i as f64 / last)).collect()
}

fn pt(p: NSPoint) -> Point {
    Point::new(p.x, p.y)
}

fn rect_path(r: NSRect) -> BezPath {
    kurbo::Rect::new(r.origin.x, r.origin.y, r.origin.x + r.size.width, r.origin.y + r.size.height).to_path(0.1)
}

/// A path's shape and whether it fills even-odd.
fn path_shape(path: &NSBezierPath) -> (BezPath, bool) {
    (crate::path::bez_path(path), path.windingRule() == NSWindingRule::EvenOdd)
}

/// The color at `t` of stops sorted by location, clamped to the ends.
fn sample(stops: &[(f32, Color)], t: f32) -> Color {
    let Some(first) = stops.first() else { return [0.0; 4] };
    if t <= first.0 {
        return first.1;
    }
    for pair in stops.windows(2) {
        let ((t0, c0), (t1, c1)) = (pair[0], pair[1]);
        if t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
            return [0, 1, 2, 3].map(|i| c0[i] + (c1[i] - c0[i]) * f);
        }
    }
    stops[stops.len() - 1].1
}

impl NSGradientImpl {
    /// The stops as colors now, in the current drawing appearance.
    fn resolved(&self) -> Vec<(f32, Color)> {
        self.ivars().stops.iter().map(|(c, at)| (*at as f32, crate::color::resolve(c))).collect()
    }

    fn spec(&self, start: Point, end: Point, radii: Option<(f64, f64)>, extend: (bool, bool)) -> Arc<GradientSpec> {
        spec(self.resolved(), start, end, radii, extend)
    }

    /// Fill `shape` with the gradient running at `angle` degrees across
    /// `bounds`, from edge to edge.
    fn fill_at_angle(&self, shape: &BezPath, bounds: NSRect, angle: f64, even_odd: bool) {
        let (w, h) = (bounds.size.width, bounds.size.height);
        let center = Point::new(bounds.origin.x + w / 2.0, bounds.origin.y + h / 2.0);
        let a = angle.to_radians();
        let dir = Vec2::new(a.cos(), a.sin());
        // Half the rectangle's extent along the direction: its corners
        // project onto the ends.
        let half = (w * dir.x.abs() + h * dir.y.abs()) / 2.0;
        let spec = self.spec(center - dir * half, center + dir * half, None, (true, true));
        crate::context::with_state(|st| fill(st, shape, even_odd, spec));
    }

    /// Fill `shape` with a radial gradient from a point `relative` to its
    /// bounds' center (-1 to 1 across) out to its farthest corner.
    fn fill_radial_relative(&self, shape: &BezPath, bounds: NSRect, relative: NSPoint, even_odd: bool) {
        let (w, h) = (bounds.size.width, bounds.size.height);
        let (x0, y0) = (bounds.origin.x, bounds.origin.y);
        let center = Point::new(x0 + w / 2.0 * (1.0 + relative.x), y0 + h / 2.0 * (1.0 + relative.y));
        let corners = [Point::new(x0, y0), Point::new(x0 + w, y0), Point::new(x0, y0 + h), Point::new(x0 + w, y0 + h)];
        let radius = corners.iter().map(|c| c.distance(center)).fold(0.0, f64::max);
        let spec = self.spec(center, center, Some((0.0, radius)), (true, true));
        crate::context::with_state(|st| fill(st, shape, even_odd, spec));
    }

    fn linear_band(&self, start: Point, end: Point, options: NSGradientDrawingOptions) {
        let stops = self.resolved();
        crate::context::with_state(|st| linear_band(st, stops, start, end, extend_of(options)));
    }

    fn radial_band(&self, c0: Point, r0: f64, c1: Point, r1: f64, options: NSGradientDrawingOptions) {
        let stops = self.resolved();
        crate::context::with_state(|st| radial_band(st, stops, (c0, r0), (c1, r1), extend_of(options)));
    }
}

fn extend_of(options: NSGradientDrawingOptions) -> (bool, bool) {
    (
        options.contains(NSGradientDrawingOptions::DrawsBeforeStartingLocation),
        options.contains(NSGradientDrawingOptions::DrawsAfterEndingLocation),
    )
}

/// A gradient paint of `stops` (straight sRGB, by location) from `start`
/// to `end`, or between circles of `radii` around them.
pub(crate) fn spec(
    stops: Vec<(f32, Color)>,
    start: Point,
    end: Point,
    radii: Option<(f64, f64)>,
    extend: (bool, bool),
) -> Arc<GradientSpec> {
    Arc::new(GradientSpec {
        stops,
        start: (start.x as f32, start.y as f32),
        end: (end.x as f32, end.y as f32),
        radii: radii.map(|(a, b)| (a as f32, b as f32)),
        extend,
    })
}

/// Fill the clip with a linear gradient from `start` to `end` (user
/// space), only between the lines through them square to it unless
/// `extend` (before the start, after the end) says otherwise. A gradient
/// with no length draws nothing.
pub(crate) fn linear_band(
    st: &mut crate::context::ContextState,
    stops: Vec<(f32, Color)>,
    start: Point,
    end: Point,
    (before, after): (bool, bool),
) {
    let Some(area) = st.clip_in_user_space() else { return };
    let axis = end - start;
    let len = axis.hypot();
    if len <= 0.0 {
        return;
    }
    let (u, n) = (axis / len, Vec2::new(-axis.y, axis.x) / len);
    // Far enough to take in all of the clip.
    let far = [area.origin(), Point::new(area.x1, area.y0), Point::new(area.x0, area.y1), Point::new(area.x1, area.y1)]
        .iter()
        .map(|c| c.distance(start))
        .fold(len, f64::max)
        + 1.0;
    let (s0, s1) = (if before { -far } else { 0.0 }, if after { len + far } else { len });
    let mut band = BezPath::new();
    band.move_to(start + u * s0 - n * far);
    band.line_to(start + u * s1 - n * far);
    band.line_to(start + u * s1 + n * far);
    band.line_to(start + u * s0 + n * far);
    band.close_path();
    fill(st, &band, false, spec(stops, start, end, None, (before, after)));
}

/// Fill the clip with a gradient between two circles (center and radius,
/// user space): where the circles in between pass unless `extend` says
/// otherwise. As in CoreGraphics (measured on macOS), what's inside a
/// circle at the gradient's end that lies inside its start comes after the
/// end, as what's inside a start inside the end comes before the start;
/// and the circles' edges are hard, each pixel taking the color at its
/// center. Radii that aren't finite draw nothing; a circle taking in the
/// whole clip is the clip, so huge radii cost no more than others.
pub(crate) fn radial_band(
    st: &mut crate::context::ContextState,
    stops: Vec<(f32, Color)>,
    (c0, r0): (Point, f64),
    (c1, r1): (Point, f64),
    (before, after): (bool, bool),
) {
    if !(r0.is_finite() && r1.is_finite() && c0.is_finite() && c1.is_finite()) {
        return;
    }
    let Some(area) = st.clip_in_user_space() else { return };
    let corners =
        [area.origin(), Point::new(area.x1, area.y0), Point::new(area.x0, area.y1), Point::new(area.x1, area.y1)];
    let covers = |c: Point, r: f64| corners.iter().all(|p| p.distance(c) <= r);
    // Pixels whose centers lie on a circle's edge are the gradient's
    // (its parameter runs from 0 to 1, both ends in): circles bounding it
    // are grown a hair, circles cut out of it shrunk one.
    // (A twentieth of a device pixel: more than the rasterizer's
    // precision, less than would take in another pixel's center.)
    let per_pixel = crate::image_rep::device_scale(st).max(1e-9);
    let hair = |r: f64| (0.05 / per_pixel).max(1e-9 * r);
    let circle = |c: Point, r: f64| {
        if covers(c, r) { area.to_path(0.1) } else { kurbo::Circle::new(c, r).to_path((r * 1e-4).max(0.1)) }
    };
    let outer = |c: Point, r: f64| circle(c, r + hair(r));
    let inner = |c: Point, r: f64| circle(c, (r - hair(r)).max(0.0));
    let d = c0.distance(c1);
    let nested = d + r0 <= r1;
    let reversed = d + r1 <= r0;
    let mut shape = BezPath::new();
    let mut even_odd = false;
    if after {
        shape.extend(area.to_path(0.1));
    } else if nested {
        shape.extend(outer(c1, r1));
    } else if reversed {
        shape.extend(outer(c0, r0));
    } else {
        // The circles between sweep out the region: their union.
        for i in 0..=32 {
            let t = f64::from(i) / 32.0;
            let (c, r) = (c0.lerp(c1, t), r0 + (r1 - r0) * t);
            if covers(c, r) {
                shape = area.to_path(0.1);
                break;
            }
            shape.extend(outer(c, r));
        }
    }
    // Inside the starting circle comes before the start, when it lies
    // within the ending one; inside the ending one after the end, when it
    // lies within the starting one.
    if !before && nested && r0 > 0.0 {
        shape.extend(inner(c0, r0));
        even_odd = true;
    } else if !after && reversed && r1 > 0.0 {
        shape.extend(inner(c1, r1));
        even_odd = true;
    }
    let spec = spec(stops, c0, c1, Some((r0, r1)), (before, after));
    let Some(path) = crate::path::to_skia(&shape) else { return };
    let mut draw = st.gs.draw();
    draw.aa = false;
    st.push(Op::FillPath { path, even_odd, paint: Paint::Gradient(spec), draw });
}

/// Record `shape` (user space) filled with the gradient `spec`.
fn fill(st: &mut crate::context::ContextState, shape: &BezPath, even_odd: bool, spec: Arc<GradientSpec>) {
    let Some(path) = crate::path::to_skia(shape) else { return };
    let op = Op::FillPath { path, even_odd, paint: Paint::Gradient(spec), draw: st.gs.draw() };
    st.push(op);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_clamp_and_interpolate() {
        let stops = [(0.2, [1.0, 0.0, 0.0, 1.0]), (0.8, [0.0, 0.0, 1.0, 1.0])];
        assert_eq!(sample(&stops, 0.0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(sample(&stops, 0.5), [0.5, 0.0, 0.5, 1.0]);
        assert_eq!(sample(&stops, 1.0), [0.0, 0.0, 1.0, 1.0]);
    }
}
