//! CoreGraphics' geometry: rectangles, points, sizes and affine transforms,
//! as values. Rectangles with a negative width or height are taken
//! standardized, and the null rectangle (`CGRectNull`, at infinity) is
//! what an intersection of disjoint rectangles gives and what a union
//! ignores (handing back the other rectangle as it is), as in CoreGraphics
//! (`conformance/tests/coregraphics.rs`, and its edge cases measured on
//! macOS: every rectangle contains the null one, empty rectangles meeting
//! intersect, the infinite rectangle stays infinite under any transform).

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{
    CFDictionary, CGAffineTransform, CGAffineTransformComponents, CGFloat, CGPoint, CGRect, CGRectEdge, CGSize,
    CGVector,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

#[unsafe(no_mangle)]
pub static CGPointZero: CGPoint = CGPoint { x: 0.0, y: 0.0 };
#[unsafe(no_mangle)]
pub static CGSizeZero: CGSize = CGSize { width: 0.0, height: 0.0 };
#[unsafe(no_mangle)]
pub static CGRectZero: CGRect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: 0.0, height: 0.0 } };
#[unsafe(no_mangle)]
pub static CGRectNull: CGRect = NULL;
#[unsafe(no_mangle)]
pub static CGRectInfinite: CGRect = CGRect {
    origin: CGPoint { x: -f64::MAX / 2.0, y: -f64::MAX / 2.0 },
    size: CGSize { width: f64::MAX, height: f64::MAX },
};
#[unsafe(no_mangle)]
pub static CGAffineTransformIdentity: CGAffineTransform = IDENTITY;

pub(crate) const NULL: CGRect =
    CGRect { origin: CGPoint { x: f64::INFINITY, y: f64::INFINITY }, size: CGSize { width: 0.0, height: 0.0 } };

pub(crate) const IDENTITY: CGAffineTransform = CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

pub(crate) fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect { origin: CGPoint { x, y }, size: CGSize { width: w, height: h } }
}

pub(crate) fn is_null(r: CGRect) -> bool {
    r.origin.x == f64::INFINITY || r.origin.y == f64::INFINITY
}

/// `r` with a width and height of zero or more.
pub(crate) fn standardize(r: CGRect) -> CGRect {
    if is_null(r) {
        return NULL;
    }
    let (mut x, mut w) = (r.origin.x, r.size.width);
    let (mut y, mut h) = (r.origin.y, r.size.height);
    if w < 0.0 {
        x += w;
        w = -w;
    }
    if h < 0.0 {
        y += h;
        h = -h;
    }
    rect(x, y, w, h)
}

/// A rectangle's edges, standardized: x0, y0, x1, y1.
pub(crate) fn edges(r: CGRect) -> (f64, f64, f64, f64) {
    let s = standardize(r);
    (s.origin.x, s.origin.y, s.origin.x + s.size.width, s.origin.y + s.size.height)
}

fn from_edges(x0: f64, y0: f64, x1: f64, y1: f64) -> CGRect {
    rect(x0, y0, x1 - x0, y1 - y0)
}

pub(crate) fn kurbo_of(t: CGAffineTransform) -> kurbo::Affine {
    kurbo::Affine::new([t.a, t.b, t.c, t.d, t.tx, t.ty])
}

pub(crate) fn transform_of(a: kurbo::Affine) -> CGAffineTransform {
    let [a, b, c, d, tx, ty] = a.as_coeffs();
    CGAffineTransform { a, b, c, d, tx, ty }
}

/// The transform a nullable pointer holds, identity for none.
///
/// # Safety
///
/// `m` is null or points at a transform.
pub(crate) unsafe fn affine_at(m: *const CGAffineTransform) -> Option<kurbo::Affine> {
    // SAFETY: as the caller promises.
    (!m.is_null()).then(|| kurbo_of(unsafe { *m }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetMinX(rect: CGRect) -> CGFloat {
    edges(rect).0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetMidX(rect: CGRect) -> CGFloat {
    if is_null(rect) {
        return f64::INFINITY;
    }
    let (x0, _, x1, _) = edges(rect);
    x0 + (x1 - x0) / 2.0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetMaxX(rect: CGRect) -> CGFloat {
    edges(rect).2
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetMinY(rect: CGRect) -> CGFloat {
    edges(rect).1
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetMidY(rect: CGRect) -> CGFloat {
    if is_null(rect) {
        return f64::INFINITY;
    }
    let (_, y0, _, y1) = edges(rect);
    y0 + (y1 - y0) / 2.0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetMaxY(rect: CGRect) -> CGFloat {
    edges(rect).3
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetWidth(rect: CGRect) -> CGFloat {
    if is_null(rect) { 0.0 } else { rect.size.width.abs() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectGetHeight(rect: CGRect) -> CGFloat {
    if is_null(rect) { 0.0 } else { rect.size.height.abs() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPointEqualToPoint(point1: CGPoint, point2: CGPoint) -> bool {
    point1.x == point2.x && point1.y == point2.y
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGSizeEqualToSize(size1: CGSize, size2: CGSize) -> bool {
    size1.width == size2.width && size1.height == size2.height
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectEqualToRect(rect1: CGRect, rect2: CGRect) -> bool {
    match (is_null(rect1), is_null(rect2)) {
        (true, true) => true,
        (false, false) => {
            let (a, b) = (standardize(rect1), standardize(rect2));
            CGPointEqualToPoint(a.origin, b.origin) && CGSizeEqualToSize(a.size, b.size)
        }
        _ => false,
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectStandardize(rect: CGRect) -> CGRect {
    standardize(rect)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectIsEmpty(rect: CGRect) -> bool {
    is_null(rect) || rect.size.width == 0.0 || rect.size.height == 0.0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectIsNull(rect: CGRect) -> bool {
    is_null(rect)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectIsInfinite(rect: CGRect) -> bool {
    CGRectEqualToRect(rect, CGRectInfinite)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectInset(rect: CGRect, dx: CGFloat, dy: CGFloat) -> CGRect {
    if is_null(rect) {
        return NULL;
    }
    let s = standardize(rect);
    let (w, h) = (s.size.width - 2.0 * dx, s.size.height - 2.0 * dy);
    if w < 0.0 || h < 0.0 {
        return NULL;
    }
    self::rect(s.origin.x + dx, s.origin.y + dy, w, h)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectIntegral(rect: CGRect) -> CGRect {
    if is_null(rect) || CGRectIsInfinite(rect) {
        return rect;
    }
    let (x0, y0, x1, y1) = edges(rect);
    from_edges(x0.floor(), y0.floor(), x1.ceil(), y1.ceil())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectUnion(r1: CGRect, r2: CGRect) -> CGRect {
    if is_null(r1) {
        return r2;
    }
    if is_null(r2) {
        return r1;
    }
    let (a, b) = (edges(r1), edges(r2));
    from_edges(a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectIntersection(r1: CGRect, r2: CGRect) -> CGRect {
    if is_null(r1) || is_null(r2) {
        return NULL;
    }
    let (a, b) = (edges(r1), edges(r2));
    let (x0, y0, x1, y1) = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    if x1 < x0 || y1 < y0 {
        return NULL;
    }
    from_edges(x0, y0, x1, y1)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectOffset(rect: CGRect, dx: CGFloat, dy: CGFloat) -> CGRect {
    if is_null(rect) {
        return NULL;
    }
    let s = standardize(rect);
    self::rect(s.origin.x + dx, s.origin.y + dy, s.size.width, s.size.height)
}

/// # Safety
///
/// `slice` and `remainder` are valid to write.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGRectDivide(
    rect: CGRect,
    slice: NonNull<CGRect>,
    remainder: NonNull<CGRect>,
    amount: CGFloat,
    edge: CGRectEdge,
) {
    let (s, r) = divide(rect, amount, edge);
    // SAFETY: the caller passes places for both.
    unsafe {
        slice.write(s);
        remainder.write(r);
    }
}

fn divide(rect: CGRect, amount: f64, edge: CGRectEdge) -> (CGRect, CGRect) {
    if is_null(rect) {
        return (NULL, NULL);
    }
    let r = standardize(rect);
    let (x, y, w, h) = (r.origin.x, r.origin.y, r.size.width, r.size.height);
    let along = if matches!(edge, CGRectEdge::MinXEdge | CGRectEdge::MaxXEdge) { w } else { h };
    let a = amount.max(0.0).min(along);
    match edge {
        CGRectEdge::MinXEdge => (self::rect(x, y, a, h), self::rect(x + a, y, w - a, h)),
        CGRectEdge::MaxXEdge => (self::rect(x + w - a, y, a, h), self::rect(x, y, w - a, h)),
        CGRectEdge::MinYEdge => (self::rect(x, y, w, a), self::rect(x, y + a, w, h - a)),
        _ => (self::rect(x, y + h - a, w, a), self::rect(x, y, w, h - a)),
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectContainsPoint(rect: CGRect, point: CGPoint) -> bool {
    if is_null(rect) {
        return false;
    }
    let (x0, y0, x1, y1) = edges(rect);
    point.x >= x0 && point.x < x1 && point.y >= y0 && point.y < y1
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectContainsRect(rect1: CGRect, rect2: CGRect) -> bool {
    // Every rectangle contains the null one, the null one too.
    if is_null(rect2) {
        return true;
    }
    if is_null(rect1) {
        return false;
    }
    let (a, b) = (edges(rect1), edges(rect2));
    b.0 >= a.0 && b.1 >= a.1 && b.2 <= a.2 && b.3 <= a.3
}

/// Whether the rectangles overlap: rectangles that only touch don't; an
/// empty one meets another where it lies inside or on it.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectIntersectsRect(rect1: CGRect, rect2: CGRect) -> bool {
    if is_null(rect1) || is_null(rect2) {
        return false;
    }
    let (a, b) = (edges(rect1), edges(rect2));
    let (x0, y0, x1, y1) = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    let empty = |e: (f64, f64, f64, f64)| e.0 == e.2 || e.1 == e.3;
    if empty(a) || empty(b) { x0 <= x1 && y0 <= y1 } else { x0 < x1 && y0 < y1 }
}

/// A dictionary of numbers, for the dictionary representations.
fn number_dictionary(entries: &[(&str, f64)]) -> Retained<NSDictionary<NSString, AnyObject>> {
    let keys: Vec<Retained<NSString>> = entries.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let values: Vec<Retained<AnyObject>> = entries
        .iter()
        .map(|(_, v)| Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_f64(*v)))))
        .collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    NSDictionary::from_slices(&keys, &values)
}

fn dictionary_number(dict: Option<&CFDictionary>, key: &str) -> Option<f64> {
    let dict = dict?;
    // SAFETY: a CFDictionary is an NSDictionary here.
    let dict = unsafe { &*(dict as *const CFDictionary).cast::<NSDictionary<NSString, AnyObject>>() };
    let value = dict.objectForKey(&NSString::from_str(key))?;
    // SAFETY: numbers (and strings) answer -doubleValue.
    Some(unsafe { objc2::msg_send![&*value, doubleValue] })
}

fn cf_dictionary(entries: &[(&str, f64)]) -> Option<NonNull<CFDictionary>> {
    Some(super::owned(number_dictionary(entries)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPointCreateDictionaryRepresentation(point: CGPoint) -> Option<NonNull<CFDictionary>> {
    cf_dictionary(&[("X", point.x), ("Y", point.y)])
}

/// # Safety
///
/// `point` is null or valid to write.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGPointMakeWithDictionaryRepresentation(
    dict: Option<&CFDictionary>,
    point: *mut CGPoint,
) -> bool {
    let (Some(x), Some(y)) = (dictionary_number(dict, "X"), dictionary_number(dict, "Y")) else { return false };
    // SAFETY: as the caller promises.
    unsafe { super::store(point, CGPoint { x, y }) };
    true
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGSizeCreateDictionaryRepresentation(size: CGSize) -> Option<NonNull<CFDictionary>> {
    cf_dictionary(&[("Width", size.width), ("Height", size.height)])
}

/// # Safety
///
/// `size` is null or valid to write.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGSizeMakeWithDictionaryRepresentation(
    dict: Option<&CFDictionary>,
    size: *mut CGSize,
) -> bool {
    let (Some(w), Some(h)) = (dictionary_number(dict, "Width"), dictionary_number(dict, "Height")) else {
        return false;
    };
    // SAFETY: as the caller promises.
    unsafe { super::store(size, CGSize { width: w, height: h }) };
    true
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectCreateDictionaryRepresentation(param1: CGRect) -> Option<NonNull<CFDictionary>> {
    cf_dictionary(&[
        ("X", param1.origin.x),
        ("Y", param1.origin.y),
        ("Width", param1.size.width),
        ("Height", param1.size.height),
    ])
}

/// # Safety
///
/// `rect` is null or valid to write.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGRectMakeWithDictionaryRepresentation(
    dict: Option<&CFDictionary>,
    rect: *mut CGRect,
) -> bool {
    let n = |k| dictionary_number(dict, k);
    let (Some(x), Some(y)) = (n("X"), n("Y")) else { return false };
    let (Some(w), Some(h)) = (n("Width"), n("Height")) else {
        // A point's dictionary: its origin is written, and it's no
        // rectangle (as CoreGraphics does).
        if !rect.is_null() {
            // SAFETY: as the caller promises.
            unsafe { (*rect).origin = CGPoint { x, y } };
        }
        return false;
    };
    // SAFETY: as the caller promises.
    unsafe { super::store(rect, self::rect(x, y, w, h)) };
    true
}

// Affine transforms. A point (x, y) maps to (a·x + c·y + tx, b·x + d·y + ty).

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformMake(
    a: CGFloat,
    b: CGFloat,
    c: CGFloat,
    d: CGFloat,
    tx: CGFloat,
    ty: CGFloat,
) -> CGAffineTransform {
    CGAffineTransform { a, b, c, d, tx, ty }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformMakeTranslation(tx: CGFloat, ty: CGFloat) -> CGAffineTransform {
    CGAffineTransform { tx, ty, ..IDENTITY }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformMakeScale(sx: CGFloat, sy: CGFloat) -> CGAffineTransform {
    CGAffineTransform { a: sx, d: sy, ..IDENTITY }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformMakeRotation(angle: CGFloat) -> CGAffineTransform {
    let (s, c) = angle.sin_cos();
    CGAffineTransform { a: c, b: s, c: -s, d: c, tx: 0.0, ty: 0.0 }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformIsIdentity(t: CGAffineTransform) -> bool {
    CGAffineTransformEqualToTransform(t, IDENTITY)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformTranslate(
    t: CGAffineTransform,
    tx: CGFloat,
    ty: CGFloat,
) -> CGAffineTransform {
    CGAffineTransform { tx: tx * t.a + ty * t.c + t.tx, ty: tx * t.b + ty * t.d + t.ty, ..t }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformScale(t: CGAffineTransform, sx: CGFloat, sy: CGFloat) -> CGAffineTransform {
    CGAffineTransform { a: t.a * sx, b: t.b * sx, c: t.c * sy, d: t.d * sy, ..t }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformRotate(t: CGAffineTransform, angle: CGFloat) -> CGAffineTransform {
    // The whole product, so numbers that aren't reach every entry, as in
    // CoreGraphics.
    CGAffineTransformConcat(CGAffineTransformMakeRotation(angle), t)
}

/// The inverse; a transform that has none comes back as it was.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformInvert(t: CGAffineTransform) -> CGAffineTransform {
    // A transform that can't be undone comes back as it is; one of numbers
    // that aren't comes back as such numbers, as in CoreGraphics.
    let det = t.a * t.d - t.b * t.c;
    if det == 0.0 {
        return t;
    }
    let (a, b, c, d) = (t.d / det, -t.b / det, -t.c / det, t.a / det);
    CGAffineTransform { a, b, c, d, tx: -(t.tx * a + t.ty * c), ty: -(t.tx * b + t.ty * d) }
}

/// `t1`, then `t2`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformConcat(t1: CGAffineTransform, t2: CGAffineTransform) -> CGAffineTransform {
    CGAffineTransform {
        a: t1.a * t2.a + t1.b * t2.c,
        b: t1.a * t2.b + t1.b * t2.d,
        c: t1.c * t2.a + t1.d * t2.c,
        d: t1.c * t2.b + t1.d * t2.d,
        tx: t1.tx * t2.a + t1.ty * t2.c + t2.tx,
        ty: t1.tx * t2.b + t1.ty * t2.d + t2.ty,
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformEqualToTransform(t1: CGAffineTransform, t2: CGAffineTransform) -> bool {
    t1.a == t2.a && t1.b == t2.b && t1.c == t2.c && t1.d == t2.d && t1.tx == t2.tx && t1.ty == t2.ty
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGPointApplyAffineTransform(point: CGPoint, t: CGAffineTransform) -> CGPoint {
    CGPoint { x: t.a * point.x + t.c * point.y + t.tx, y: t.b * point.x + t.d * point.y + t.ty }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGSizeApplyAffineTransform(size: CGSize, t: CGAffineTransform) -> CGSize {
    CGSize { width: t.a * size.width + t.c * size.height, height: t.b * size.width + t.d * size.height }
}

/// The smallest rectangle holding the transformed corners.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGRectApplyAffineTransform(rect: CGRect, t: CGAffineTransform) -> CGRect {
    if is_null(rect) {
        return NULL;
    }
    if CGRectIsInfinite(rect) {
        return rect;
    }
    let (x0, y0, x1, y1) = edges(rect);
    let corners =
        [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| CGPointApplyAffineTransform(CGPoint { x, y }, t));
    let fold = |f: fn(f64, f64) -> f64, v: [f64; 4]| v.into_iter().reduce(f).unwrap_or(0.0);
    let xs = corners.map(|p| p.x);
    let ys = corners.map(|p| p.y);
    from_edges(fold(f64::min, xs), fold(f64::min, ys), fold(f64::max, xs), fold(f64::max, ys))
}

/// `t` as a rotation of a horizontal shear of a scale, then the
/// translation: the rotation of its first column, with both scales
/// negated (and a half turn taken off the rotation) when it mirrors.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformDecompose(transform: CGAffineTransform) -> CGAffineTransformComponents {
    let t = transform;
    let angle = t.b.atan2(t.a);
    let (s, c) = angle.sin_cos();
    // The second column with the rotation undone: (shear · sy, sy).
    let (u, v) = (t.c * c + t.d * s, -t.c * s + t.d * c);
    let (mut sx, mut sy) = (t.a.hypot(t.b), v);
    let shear = if v != 0.0 { u / v } else { 0.0 };
    let mut rotation = angle;
    if t.a * t.d - t.b * t.c < 0.0 {
        sx = -sx;
        sy = -sy;
        rotation -= std::f64::consts::PI;
    }
    CGAffineTransformComponents {
        scale: CGSize { width: sx, height: sy },
        horizontalShear: shear,
        rotation,
        translation: CGVector { dx: t.tx, dy: t.ty },
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGAffineTransformMakeWithComponents(
    components: CGAffineTransformComponents,
) -> CGAffineTransform {
    let (sx, sy) = (components.scale.width, components.scale.height);
    let h = components.horizontalShear;
    let (s, c) = components.rotation.sin_cos();
    CGAffineTransform {
        a: sx * c,
        b: sx * s,
        c: c * h * sy - s * sy,
        d: s * h * sy + c * sy,
        tx: components.translation.dx,
        ty: components.translation.dy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decomposing_round_trips() {
        for t in [
            CGAffineTransformMake(1.0, 2.0, 3.0, 4.0, 5.0, 6.0),
            CGAffineTransformMakeRotation(0.5),
            CGAffineTransformMakeScale(1.0, -1.0),
            CGAffineTransformMake(1.0, 0.0, 1.0, 1.0, 0.0, 0.0),
        ] {
            let back = CGAffineTransformMakeWithComponents(CGAffineTransformDecompose(t));
            for (a, b) in [(t.a, back.a), (t.b, back.b), (t.c, back.c), (t.d, back.d), (t.tx, back.tx)] {
                assert!((a - b).abs() < 1e-12, "{t:?} came back as {back:?}");
            }
        }
        let d = CGAffineTransformDecompose(CGAffineTransformMake(1.0, 2.0, 3.0, 4.0, 5.0, 6.0));
        assert!((d.scale.width + 5f64.sqrt()).abs() < 1e-12 && (d.horizontalShear + 5.5).abs() < 1e-9, "{d:?}");
    }
}
