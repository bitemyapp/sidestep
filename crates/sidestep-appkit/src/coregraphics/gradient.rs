//! `CGGradient`, `CGFunction` and `CGShading`.
//!
//! A gradient is its stops, resolved to straight sRGB when it's made (its
//! colors are components, which don't change) and sorted by location.
//! Colors interpolate in the gradient's space, as in CoreGraphics: for a
//! space whose colors don't map to sRGB's linearly (linear light, Display
//! P3, Generic RGB), each span between stops is sampled into more stops
//! when the gradient is made, and drawing interpolates those in sRGB.
//! Drawing fills the clip's band the gradient runs through, or all of it
//! where the options extend it (`crate::gradient`, shared with
//! `NSGradient`). A shading samples its function across its domain into
//! stops (before the context's state is taken: the function is the
//! program's) and draws the same way.

use std::ffi::{c_float, c_void};
use std::ptr::NonNull;

use kurbo::Point;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_core_foundation::{CFArray, CFTypeID, CGFloat, CGPoint};
use objc2_core_graphics::{CGColorSpace, CGFunction, CGFunctionCallbacks, CGGradient, CGShading};
use objc2_foundation::NSArray;

use super::color::{CGColorSpaceImpl, color_imp, space_imp};
use crate::context::ContextState;
use crate::protocol::Color;

pub(crate) struct GradientIvars {
    stops: Vec<(f32, Color)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a gradient is
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGGradient"]
    #[ivars = GradientIvars]
    pub(crate) struct CGGradientImpl;

    unsafe impl NSObjectProtocol for CGGradientImpl {}
);

impl CGGradientImpl {
    pub(crate) fn stops(&self) -> Vec<(f32, Color)> {
        self.ivars().stops.clone()
    }
}

pub(crate) fn gradient_imp(g: &CGGradient) -> &CGGradientImpl {
    // SAFETY: every CGGradient is a CGGradientImpl.
    unsafe { &*(g as *const CGGradient).cast::<CGGradientImpl>() }
}

fn make(stops: Vec<(f32, Color)>) -> Retained<CGGradientImpl> {
    let this = CGGradientImpl::alloc().set_ivars(GradientIvars { stops });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// Locations for `count` stops: the given ones, or spread evenly from 0 to
/// 1; `None` if one given is outside 0 to 1, which makes no gradient, as in
/// CoreGraphics.
///
/// # Safety
///
/// `locations` is null or holds `count` values.
unsafe fn locations(locations: *const CGFloat, count: usize) -> Option<Vec<f64>> {
    if locations.is_null() {
        let last = count.saturating_sub(1).max(1) as f64;
        Some((0..count).map(|i| i as f64 / last).collect())
    } else {
        // SAFETY: as the caller promises.
        let given = unsafe { std::slice::from_raw_parts(locations, count) };
        given.iter().all(|v| (0.0..=1.0).contains(v)).then(|| given.to_vec())
    }
}

/// Whether colors in `space` map to sRGB's linearly, so that interpolating
/// them in sRGB is interpolating them in the space.
fn interpolates_as_srgb(space: &CGColorSpaceImpl) -> bool {
    use super::color::{Curve, Model};
    let info = space.info();
    info.curve == Curve::Srgb && matches!(info.model, Model::Rgb | Model::Gray)
}

/// Stops of components (`n` a color, alpha last) at `at` in `space`, sorted
/// by location, as straight sRGB: sampled between the given ones where the
/// space doesn't interpolate as sRGB does.
fn stops_in(space: &CGColorSpaceImpl, colors: Vec<(f64, Vec<CGFloat>)>) -> Vec<(f32, Color)> {
    let mut colors = colors;
    colors.sort_by(|a, b| a.0.total_cmp(&b.0));
    if interpolates_as_srgb(space) || colors.len() < 2 {
        return colors.iter().map(|(t, c)| (*t as f32, space.rgba(c))).collect();
    }
    // Enough samples a span that the sRGB interpolation between them is
    // within a level of the space's own.
    const SPAN: usize = 16;
    let mut out = Vec::with_capacity((colors.len() - 1) * SPAN + 1);
    for pair in colors.windows(2) {
        let ((t0, a), (t1, b)) = (&pair[0], &pair[1]);
        for k in 0..SPAN {
            let f = k as f64 / SPAN as f64;
            let c: Vec<CGFloat> = a.iter().zip(b).map(|(x, y)| x + (y - x) * f).collect();
            out.push(((t0 + (t1 - t0) * f) as f32, space.rgba(&c)));
        }
    }
    let (t, c) = &colors[colors.len() - 1];
    out.push((*t as f32, space.rgba(c)));
    out
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGGradientGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_GRADIENT
}

/// # Safety
///
/// `components` holds `count` colors of the space's components and alpha;
/// `locations` is null or holds `count` values.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGGradientCreateWithColorComponents(
    space: Option<&CGColorSpace>,
    components: *const CGFloat,
    locations: *const CGFloat,
    count: usize,
) -> Option<NonNull<CGGradient>> {
    let space = space_imp(space?);
    if components.is_null() || count == 0 {
        return None;
    }
    let n = space.info().components + 1;
    // SAFETY: as the caller promises.
    let comps = unsafe { std::slice::from_raw_parts(components, n * count) };
    // SAFETY: as the caller promises.
    let at = unsafe { self::locations(locations, count) }?;
    let colors = at.into_iter().zip(comps.chunks_exact(n)).map(|(t, c)| (t, c.to_vec())).collect();
    Some(super::owned(make(stops_in(space, colors))))
}

/// # Safety
///
/// As `CGGradientCreateWithColorComponents`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGGradientCreateWithContentHeadroom(
    _headroom: c_float,
    space: Option<&CGColorSpace>,
    components: *const CGFloat,
    locations: *const CGFloat,
    count: usize,
) -> Option<NonNull<CGGradient>> {
    // SAFETY: as the caller promises.
    unsafe { CGGradientCreateWithColorComponents(space, components, locations, count) }
}

/// # Safety
///
/// `colors` holds CGColors; `locations` is null or holds a value for each.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGGradientCreateWithColors(
    space: Option<&CGColorSpace>,
    colors: Option<&CFArray>,
    locations: *const CGFloat,
) -> Option<NonNull<CGGradient>> {
    // SAFETY: a CFArray is an NSArray here, of CGColors.
    let colors = unsafe { &*(colors? as *const CFArray).cast::<NSArray<AnyObject>>() };
    // SAFETY: the array holds CGColors.
    let cg = |c: &AnyObject| color_imp(unsafe { &*(c as *const AnyObject).cast::<objc2_core_graphics::CGColor>() });
    let colors: Vec<Retained<AnyObject>> = colors.iter().collect();
    if colors.is_empty() {
        return None;
    }
    // SAFETY: as the caller promises.
    let at = unsafe { self::locations(locations, colors.len()) }?;
    let stops = match space.map(space_imp) {
        // Colors in the gradient's space, interpolated there.
        Some(space) if !matches!(space.info().model, super::color::Model::Pattern | super::color::Model::Indexed) => {
            let colors = at
                .into_iter()
                .zip(&colors)
                .map(|(t, c)| {
                    let c = cg(c);
                    let comps = c.components();
                    let rgb = c.space().to_srgb(&comps[..comps.len().saturating_sub(1)]);
                    let mut mine = space.components_for_srgb(rgb);
                    mine.push(c.alpha());
                    (t, mine)
                })
                .collect();
            stops_in(space, colors)
        }
        _ => {
            let mut stops: Vec<(f32, Color)> =
                at.into_iter().zip(&colors).map(|(t, c)| (t as f32, cg(c).resolve())).collect();
            stops.sort_by(|a, b| a.0.total_cmp(&b.0));
            stops
        }
    };
    Some(super::owned(make(stops)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGGradientGetContentHeadroom(_gradient: Option<&CGGradient>) -> c_float {
    1.0
}

// Functions.

pub(crate) struct FunctionIvars {
    info: *mut c_void,
    /// How many inputs it takes.
    inputs: usize,
    domain: Vec<f64>,
    range: Vec<f64>,
    outputs: usize,
    evaluate: Option<unsafe extern "C-unwind" fn(*mut c_void, NonNull<CGFloat>, NonNull<CGFloat>)>,
    release: Option<unsafe extern "C-unwind" fn(*mut c_void)>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a function is
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGFunction"]
    #[ivars = FunctionIvars]
    pub(crate) struct CGFunctionImpl;

    unsafe impl NSObjectProtocol for CGFunctionImpl {}
);

impl Drop for CGFunctionImpl {
    fn drop(&mut self) {
        if let Some(release) = self.ivars().release {
            // SAFETY: the program's callback, with its info.
            unsafe { release(self.ivars().info) };
        }
    }
}

impl CGFunctionImpl {
    /// The outputs at `t`, clamped to the range: `wanted` of them (what the
    /// shading's space takes), or the range's count if that's more. The
    /// program's function writes as many outputs as its shading needs, so
    /// there's room for those and more; a function of more than one input
    /// gets the rest of its inputs at the domain's starts.
    fn at(&self, t: f64, wanted: usize) -> Vec<f64> {
        let i = self.ivars();
        let mut out = vec![0.0; i.outputs.max(wanted).max(1) + 8];
        let mut input = vec![t; i.inputs.max(1)];
        for (k, v) in input.iter_mut().enumerate().skip(1) {
            *v = i.domain.get(2 * k).copied().unwrap_or(0.0);
        }
        if let Some(evaluate) = i.evaluate {
            // SAFETY: the program's callback, with its inputs and room for
            // its outputs.
            unsafe { evaluate(i.info, NonNull::from(&input[0]), NonNull::from(&mut out[0])) };
        }
        out.truncate(i.outputs.max(wanted).max(1));
        for (k, v) in out.iter_mut().enumerate() {
            if let (Some(lo), Some(hi)) = (i.range.get(2 * k), i.range.get(2 * k + 1)) {
                *v = v.clamp(*lo, *hi);
            }
        }
        out
    }

    fn domain(&self) -> (f64, f64) {
        let d = &self.ivars().domain;
        (d.first().copied().unwrap_or(0.0), d.get(1).copied().unwrap_or(1.0))
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFunctionGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_FUNCTION
}

/// # Safety
///
/// `domain` and `range` hold two values a dimension (or are null);
/// `callbacks` points at callbacks valid for `info`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGFunctionCreate(
    info: *mut c_void,
    domain_dimension: usize,
    domain: *const CGFloat,
    range_dimension: usize,
    range: *const CGFloat,
    callbacks: *const CGFunctionCallbacks,
) -> Option<NonNull<CGFunction>> {
    if callbacks.is_null() {
        return None;
    }
    // SAFETY: as the caller promises.
    let (callbacks, domain, range) = unsafe {
        (
            &*callbacks,
            super::slice(domain, 2 * domain_dimension).to_vec(),
            super::slice(range, 2 * range_dimension).to_vec(),
        )
    };
    let outputs = if range_dimension > 0 { range_dimension } else { 4 };
    let ivars = FunctionIvars {
        info,
        inputs: domain_dimension,
        domain,
        range,
        outputs,
        evaluate: callbacks.evaluate,
        release: callbacks.releaseInfo,
    };
    let this = CGFunctionImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let this: Retained<CGFunctionImpl> = unsafe { msg_send![super(this), init] };
    Some(super::owned(this))
}

// Shadings.

pub(crate) struct ShadingIvars {
    space: Retained<CGColorSpaceImpl>,
    function: Retained<CGFunctionImpl>,
    start: (Point, f64),
    end: (Point, f64),
    radial: bool,
    extend: (bool, bool),
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a shading is
    // immutable.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGShading"]
    #[ivars = ShadingIvars]
    pub(crate) struct CGShadingImpl;

    unsafe impl NSObjectProtocol for CGShadingImpl {}
);

impl CGShadingImpl {
    /// Its function sampled across the domain, as gradient stops. This
    /// calls the program's function: not while a context's state is held.
    pub(crate) fn stops(&self) -> Vec<(f32, Color)> {
        let i = self.ivars();
        let (d0, d1) = i.function.domain();
        let wanted = i.space.info().components + 1;
        const SAMPLES: usize = 64;
        (0..=SAMPLES)
            .map(|k| {
                let t = k as f64 / SAMPLES as f64;
                (t as f32, i.space.rgba(&i.function.at(d0 + (d1 - d0) * t, wanted)))
            })
            .collect()
    }

    /// Draw with `stops`, which [`stops`](Self::stops) worked out.
    pub(crate) fn draw(&self, st: &mut ContextState, stops: Vec<(f32, Color)>) {
        let i = self.ivars();
        if i.radial {
            crate::gradient::radial_band(st, stops, i.start, i.end, i.extend);
        } else {
            crate::gradient::linear_band(st, stops, i.start.0, i.end.0, i.extend);
        }
    }
}

pub(crate) fn shading_imp(s: &CGShading) -> &CGShadingImpl {
    // SAFETY: every CGShading is a CGShadingImpl.
    unsafe { &*(s as *const CGShading).cast::<CGShadingImpl>() }
}

fn function_imp(f: &CGFunction) -> &CGFunctionImpl {
    // SAFETY: every CGFunction is a CGFunctionImpl.
    unsafe { &*(f as *const CGFunction).cast::<CGFunctionImpl>() }
}

fn shading(
    space: Option<&CGColorSpace>,
    function: Option<&CGFunction>,
    start: (Point, f64),
    end: (Point, f64),
    radial: bool,
    extend: (bool, bool),
) -> Option<NonNull<CGShading>> {
    let ivars = ShadingIvars {
        space: space_imp(space?).retain(),
        function: function_imp(function?).retain(),
        start,
        end,
        radial,
        extend,
    };
    let this = CGShadingImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let this: Retained<CGShadingImpl> = unsafe { msg_send![super(this), init] };
    Some(super::owned(this))
}

fn pt(p: CGPoint) -> Point {
    Point::new(p.x, p.y)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGShadingGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_SHADING
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGShadingCreateAxial(
    space: Option<&CGColorSpace>,
    start: CGPoint,
    end: CGPoint,
    function: Option<&CGFunction>,
    extend_start: bool,
    extend_end: bool,
) -> Option<NonNull<CGShading>> {
    shading(space, function, (pt(start), 0.0), (pt(end), 0.0), false, (extend_start, extend_end))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGShadingCreateAxialWithContentHeadroom(
    _headroom: c_float,
    space: Option<&CGColorSpace>,
    start: CGPoint,
    end: CGPoint,
    function: Option<&CGFunction>,
    extend_start: bool,
    extend_end: bool,
) -> Option<NonNull<CGShading>> {
    CGShadingCreateAxial(space, start, end, function, extend_start, extend_end)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGShadingCreateRadial(
    space: Option<&CGColorSpace>,
    start: CGPoint,
    start_radius: CGFloat,
    end: CGPoint,
    end_radius: CGFloat,
    function: Option<&CGFunction>,
    extend_start: bool,
    extend_end: bool,
) -> Option<NonNull<CGShading>> {
    shading(
        space,
        function,
        (pt(start), start_radius.max(0.0)),
        (pt(end), end_radius.max(0.0)),
        true,
        (extend_start, extend_end),
    )
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGShadingCreateRadialWithContentHeadroom(
    _headroom: c_float,
    space: Option<&CGColorSpace>,
    start: CGPoint,
    start_radius: CGFloat,
    end: CGPoint,
    end_radius: CGFloat,
    function: Option<&CGFunction>,
    extend_start: bool,
    extend_end: bool,
) -> Option<NonNull<CGShading>> {
    CGShadingCreateRadial(space, start, start_radius, end, end_radius, function, extend_start, extend_end)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGShadingGetContentHeadroom(_shading: Option<&CGShading>) -> c_float {
    1.0
}
