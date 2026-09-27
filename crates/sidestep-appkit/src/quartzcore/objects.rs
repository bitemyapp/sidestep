//! Core Animation's values as the objects programs hand over: `NSNumber`,
//! `NSValue` (points, sizes, rectangles, transforms), `CGColor`, `CGPath`
//! and arrays of them, to and from [`Value`].

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, msg_send};
use objc2_core_graphics::{CGColor, CGPath};
use objc2_foundation::{NSArray, NSNumber, NSPoint, NSRect, NSSize, NSValue};

use super::props::{Rgba, Value};
use crate::coregraphics::color::CGColorImpl;
use crate::coregraphics::path::CGPathImpl;

/// A weak reference to `object`, or to nothing.
pub(crate) fn weak_of<T: objc2::Message>(object: Option<&T>) -> objc2::rc::Weak<T> {
    object.map_or_else(objc2::rc::Weak::default, objc2::rc::Weak::new)
}

pub(crate) fn any<T: objc2::Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(object) }
}

pub(crate) fn number(v: f64) -> Retained<AnyObject> {
    any(NSNumber::new_f64(v))
}

pub(crate) fn boolean(v: bool) -> Retained<AnyObject> {
    any(NSNumber::new_bool(v))
}

/// A new sRGB color.
pub(crate) fn color(c: Rgba) -> Retained<CGColor> {
    let imp = crate::coregraphics::color::srgb_color(c);
    // SAFETY: CGColorImpl is what CGColor names.
    unsafe { Retained::cast_unchecked(imp) }
}

/// The straight sRGB of a color object (a `CGColor`, or an `NSColor`).
pub(crate) fn rgba_of(obj: &AnyObject) -> Option<Rgba> {
    if let Some(c) = obj.downcast_ref::<CGColorImpl>() {
        return Some(c.resolve().map(f64::from));
    }
    if let Some(c) = obj.downcast_ref::<objc2_app_kit::NSColor>() {
        return Some(crate::color::resolve(c).map(f64::from));
    }
    None
}

pub(crate) fn cg_rgba(c: &CGColor) -> Rgba {
    crate::coregraphics::color::color_imp(c).resolve().map(f64::from)
}

/// The shape of a path object (a `CGPath`).
pub(crate) fn shape_of(obj: &AnyObject) -> Option<Arc<crate::coregraphics::path::Shape>> {
    obj.downcast_ref::<CGPathImpl>().map(CGPathImpl::snapshot)
}

pub(crate) fn path_shape(p: &CGPath) -> Arc<crate::coregraphics::path::Shape> {
    crate::coregraphics::path::path_imp(p).snapshot()
}

/// A new immutable path of `shape`.
pub(crate) fn path(shape: Arc<crate::coregraphics::path::Shape>) -> Retained<CGPath> {
    let imp = crate::coregraphics::path::new_path(shape, false);
    // SAFETY: CGPathImpl is what CGPath names.
    unsafe { Retained::cast_unchecked(imp) }
}

fn is_number(obj: &AnyObject) -> bool {
    // SAFETY: -isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![obj, isKindOfClass: NSNumber::class()] }
}

fn is_array(obj: &AnyObject) -> bool {
    // SAFETY: as above.
    unsafe { msg_send![obj, isKindOfClass: NSArray::<AnyObject>::class()] }
}

fn is_string(obj: &AnyObject) -> bool {
    // SAFETY: -isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![obj, isKindOfClass: objc2_foundation::NSString::class()] }
}

fn double(obj: &AnyObject) -> f64 {
    // SAFETY: -doubleValue returns a double; objects that don't answer it
    // aren't passed here.
    unsafe { msg_send![obj, doubleValue] }
}

/// The number an object stands for: a number's, or a string's (as
/// key-value coding takes it, measured).
pub(crate) fn number_of(obj: Option<&AnyObject>) -> Option<f64> {
    let obj = obj?;
    (is_number(obj) || is_string(obj)).then(|| double(obj))
}

/// What kind of struct a value holds, by its encoding.
fn value_kind(v: &NSValue) -> Option<&'static str> {
    // SAFETY: -objCType returns a C string.
    let t: *const std::ffi::c_char = unsafe { msg_send![v, objCType] };
    if t.is_null() {
        return None;
    }
    // SAFETY: as -objCType promises.
    let enc = unsafe { std::ffi::CStr::from_ptr(t) }.to_bytes();
    let starts = |p: &[u8]| enc.starts_with(p);
    Some(if starts(b"{CGPoint") || starts(b"{_NSPoint") || starts(b"{NSPoint") {
        "point"
    } else if starts(b"{CGSize") || starts(b"{_NSSize") || starts(b"{NSSize") {
        "size"
    } else if starts(b"{CGRect") || starts(b"{_NSRect") || starts(b"{NSRect") {
        "rect"
    } else if starts(b"{CATransform3D") {
        "transform"
    } else if starts(b"{CGAffineTransform") {
        "affine"
    } else {
        return None;
    })
}

fn point_of(v: &NSValue) -> [f64; 2] {
    // SAFETY: the value holds a point.
    let p: NSPoint = unsafe { msg_send![v, pointValue] };
    [p.x, p.y]
}

fn size_of_value(v: &NSValue) -> [f64; 2] {
    // SAFETY: the value holds a size.
    let s: NSSize = unsafe { msg_send![v, sizeValue] };
    [s.width, s.height]
}

fn rect_of(v: &NSValue) -> [f64; 4] {
    // SAFETY: the value holds a rectangle.
    let r: NSRect = unsafe { msg_send![v, rectValue] };
    [r.origin.x, r.origin.y, r.size.width, r.size.height]
}

/// An object as a value of the kind `like` is: a number for a number (a
/// string's number too), a point for a point, and so on. `None` when it
/// isn't one; nil is no color or path, and 0 for a number or a BOOL (as
/// key-value coding sets it, measured).
pub(crate) fn to_value(obj: Option<&AnyObject>, like: &Value) -> Option<Value> {
    let Some(obj) = obj else {
        return match like {
            Value::Color(_) => Some(Value::Color(None)),
            Value::Path(_) => Some(Value::Path(None)),
            Value::Number(_) => Some(Value::Number(0.0)),
            Value::Bool(_) => Some(Value::Bool(false)),
            _ => None,
        };
    };
    match like {
        Value::Number(_) => number_of(Some(obj)).map(Value::Number),
        Value::Bool(_) => number_of(Some(obj)).map(|n| Value::Bool(n != 0.0)),
        Value::Point(_) | Value::Size(_) | Value::Rect(_) | Value::Transform(_) => {
            if is_number(obj) {
                // A number where a struct is expected is nothing.
                return None;
            }
            let v = obj.downcast_ref::<NSValue>()?;
            Some(match (like, value_kind(v)?) {
                (Value::Point(_), "point" | "size") => Value::Point(point_of(v)),
                (Value::Size(_), "size" | "point") => Value::Size(size_of_value(v)),
                (Value::Rect(_), "rect") => Value::Rect(rect_of(v)),
                (Value::Transform(_), "transform") => {
                    Value::Transform(super::transform::to_mat(&super::transform::transform_of(v)?))
                }
                _ => return None,
            })
        }
        Value::Color(_) => rgba_of(obj).map(|c| Value::Color(Some(c))),
        Value::Path(_) => shape_of(obj).map(|s| Value::Path(Some(s))),
        Value::Colors(_) => {
            if !is_array(obj) {
                return None;
            }
            // SAFETY: checked an array.
            let a: &NSArray<AnyObject> = unsafe { &*(obj as *const AnyObject).cast() };
            Some(Value::Colors(a.iter().filter_map(|c| rgba_of(&c)).collect()))
        }
        Value::Numbers(_) => {
            if !is_array(obj) {
                return None;
            }
            // SAFETY: checked an array.
            let a: &NSArray<AnyObject> = unsafe { &*(obj as *const AnyObject).cast() };
            Some(Value::Numbers(a.iter().filter(|n| is_number(n)).map(|n| double(&n)).collect()))
        }
    }
}

/// A value as the object Core Animation hands out for it.
pub(crate) fn to_object(v: &Value) -> Option<Retained<AnyObject>> {
    Some(match v {
        Value::Number(n) => number(*n),
        Value::Bool(b) => boolean(*b),
        // SAFETY (the three below): NSValue's struct constructors.
        Value::Point(p) => any(unsafe { NSValue::valueWithPoint(NSPoint::new(p[0], p[1])) }),
        Value::Size(s) => any(unsafe { NSValue::valueWithSize(NSSize::new(s[0], s[1])) }),
        Value::Rect(r) => {
            any(unsafe { NSValue::valueWithRect(NSRect::new(NSPoint::new(r[0], r[1]), NSSize::new(r[2], r[3]))) })
        }
        Value::Transform(m) => any(super::transform::value_of(&super::transform::from_mat(m))),
        Value::Color(c) => any(color((*c)?)),
        Value::Colors(cs) => {
            let colors: Vec<Retained<CGColor>> = cs.iter().map(|c| color(*c)).collect();
            // SAFETY: colors are objects.
            let objects: Vec<&AnyObject> =
                colors.iter().map(|c| unsafe { &*Retained::as_ptr(c).cast::<AnyObject>() }).collect();
            any(NSArray::from_slice(&objects))
        }
        Value::Numbers(ns) => {
            let numbers: Vec<Retained<NSNumber>> = ns.iter().map(|n| NSNumber::new_f64(*n)).collect();
            any(NSArray::from_retained_slice(&numbers))
        }
        Value::Path(p) => any(path(p.clone()?)),
    })
}
