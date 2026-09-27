//! Key-value coding of layers, as Core Animation extends it: a layer's
//! properties by name (through its getters and setters, so subclasses'
//! overrides count), the parts of its struct values by key path
//! (`position.x`, `bounds.size.width`, `transform.rotation.z`,
//! `transform.scale`, …, measured), and any other key kept on the layer
//! as given. A nil value for a number or BOOL key sets 0, and a string's
//! number counts as a number (measured).

use objc2::encode::{EncodeArgument, EncodeReturn};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, MessageReceiver, Sel};
use objc2::{Message, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGPath};
use objc2_foundation::NSString;
use objc2_quartz_core::CATransform3D;

use super::layer::{CALayerImpl, apply_object, as_layer};
use super::objects;
use super::props::{Key, KeyPath, Value};

/// The getter's name for a key: `isHidden` and the like for the boolean
/// properties.
fn getter(key: &str) -> String {
    match key {
        "hidden" | "doubleSided" | "geometryFlipped" | "opaque" => {
            let mut chars = key.chars();
            let first = chars.next().map(|c| c.to_ascii_uppercase()).unwrap_or_default();
            format!("is{first}{}", chars.as_str())
        }
        _ => key.to_owned(),
    }
}

/// The selector of a name; none for one no selector can have (with a NUL).
fn sel_of(name: &str) -> Option<Sel> {
    std::ffi::CString::new(name).ok().map(|c| Sel::register(&c))
}

/// The setter of a key (`setFoo:` for `foo`); none for a key no setter can
/// have (empty, or not a plain name).
pub(crate) fn setter(key: &str) -> Option<Sel> {
    let mut chars = key.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() || !key.is_ascii() {
        return None;
    }
    sel_of(&format!("set{}{}:", first.to_ascii_uppercase(), chars.as_str()))
}

fn responds(obj: &AnyObject, sel: Sel) -> bool {
    // SAFETY: -respondsToSelector: takes a selector.
    unsafe { msg_send![obj, respondsToSelector: sel] }
}

/// `valueForKey:`.
pub(crate) fn value_for_key(layer: &CALayerImpl, key: &str) -> Option<Retained<AnyObject>> {
    let obj: &AnyObject = as_layer(layer);
    if key == "frame" {
        // SAFETY: -frame returns a rectangle.
        let r: CGRect = unsafe { msg_send![obj, frame] };
        return objects::to_object(&Value::Rect([r.origin.x, r.origin.y, r.size.width, r.size.height]));
    }
    if let Some(k) = Key::named(key) {
        let sel = sel_of(&getter(key))?;
        // A shape's or a gradient's key on another layer: nothing to get,
        // but a value kept for it.
        if !responds(obj, sel) {
            return extra(layer, key);
        }
        // Through the getters, which subclasses may override.
        // SAFETY (each): the property's getter returns the type matched.
        unsafe {
            return match k {
                Key::Bounds | Key::ContentsRect | Key::ContentsCenter => {
                    let r: CGRect = send(obj, sel);
                    objects::to_object(&Value::Rect([r.origin.x, r.origin.y, r.size.width, r.size.height]))
                }
                Key::Position | Key::AnchorPoint | Key::StartPoint | Key::EndPoint => {
                    let p: CGPoint = send(obj, sel);
                    objects::to_object(&Value::Point([p.x, p.y]))
                }
                Key::ShadowOffset => {
                    let s: CGSize = send(obj, sel);
                    objects::to_object(&Value::Size([s.width, s.height]))
                }
                Key::Transform | Key::SublayerTransform => {
                    let t: CATransform3D = send(obj, sel);
                    Some(objects::any(super::transform::value_of(&t)))
                }
                Key::Opacity | Key::ShadowOpacity => Some(objects::number(send::<f32>(obj, sel) as f64)),
                Key::ZPosition
                | Key::AnchorPointZ
                | Key::CornerRadius
                | Key::BorderWidth
                | Key::ShadowRadius
                | Key::ContentsScale
                | Key::StrokeStart
                | Key::StrokeEnd
                | Key::LineWidth
                | Key::MiterLimit
                | Key::LineDashPhase => Some(objects::number(send::<f64>(obj, sel))),
                Key::Hidden | Key::DoubleSided | Key::GeometryFlipped | Key::MasksToBounds => {
                    Some(objects::boolean(send::<objc2::runtime::Bool>(obj, sel).as_bool()))
                }
                // CoreGraphics objects, sent as the getters declare them.
                Key::BackgroundColor | Key::BorderColor | Key::ShadowColor | Key::FillColor | Key::StrokeColor => {
                    let p: *mut CGColor = send(obj, sel);
                    Retained::retain(p.cast::<AnyObject>())
                }
                Key::ShadowPath | Key::Path => {
                    let p: *mut CGPath = send(obj, sel);
                    Retained::retain(p.cast::<AnyObject>())
                }
                Key::Contents
                | Key::Mask
                | Key::Sublayers
                | Key::Filters
                | Key::BackgroundFilters
                | Key::CompositingFilter
                | Key::Colors
                | Key::Locations => send_obj(obj, sel),
            };
        }
    }
    // Other properties with getters returning objects.
    let object_props = [
        "name",
        "contentsGravity",
        "contentsFormat",
        "minificationFilter",
        "magnificationFilter",
        "cornerCurve",
        "actions",
        "style",
        "delegate",
        "superlayer",
        "layoutManager",
        "fillMode",
        "fillRule",
        "lineCap",
        "lineJoin",
        "lineDashPattern",
        "type",
    ];
    if object_props.contains(&key)
        && let Some(sel) = sel_of(key).filter(|s| responds(obj, *s))
    {
        // SAFETY: these getters return objects.
        return unsafe { send_obj(obj, sel) };
    }
    let sel = || sel_of(key).expect("a plain key");
    if ["beginTime", "duration", "timeOffset", "repeatDuration", "rasterizationScale"].contains(&key) {
        // SAFETY: these getters return doubles.
        return Some(objects::number(unsafe { send::<f64>(obj, sel()) }));
    }
    if ["speed", "repeatCount"].contains(&key) {
        // SAFETY: these getters return floats.
        return Some(objects::number(unsafe { send::<f32>(obj, sel()) } as f64));
    }
    if [
        "autoreverses",
        "shouldRasterize",
        "needsDisplayOnBoundsChange",
        "drawsAsynchronously",
        "allowsGroupOpacity",
        "allowsEdgeAntialiasing",
    ]
    .contains(&key)
    {
        // SAFETY: these getters return BOOL.
        return Some(objects::boolean(unsafe { send::<objc2::runtime::Bool>(obj, sel()) }.as_bool()));
    }
    extra(layer, key)
}

/// A value kept for a key that isn't a property.
fn extra(layer: &CALayerImpl, key: &str) -> Option<Retained<AnyObject>> {
    layer.read(|m| m.extras.iter().rev().find(|(n, _)| n.to_string() == key).map(|(_, v)| v.clone()))
}

/// `setValue:forKey:`.
pub(crate) fn set_value_for_key(layer: &CALayerImpl, value: Option<&AnyObject>, key: &NSString) {
    let k = key.to_string();
    let obj: &AnyObject = as_layer(layer);
    if let Some(prop) = Key::named(&k) {
        let settable = setter(&k).is_some_and(|s| responds(obj, s));
        if settable || matches!(prop, Key::Contents | Key::Mask | Key::Sublayers) {
            apply_object(layer, prop, value, true);
            return;
        }
    }
    let object_setters = [
        "name",
        "contentsGravity",
        "contentsFormat",
        "minificationFilter",
        "magnificationFilter",
        "cornerCurve",
        "actions",
        "style",
        "delegate",
        "layoutManager",
        "fillMode",
        "fillRule",
        "lineCap",
        "lineJoin",
        "lineDashPattern",
        "type",
    ];
    if object_setters.contains(&k.as_str())
        && let Some(sel) = setter(&k).filter(|s| responds(obj, *s))
    {
        let p: *const AnyObject = value.map_or(std::ptr::null(), |v| v as *const AnyObject);
        // SAFETY: these setters take an object.
        unsafe { send_set(obj, sel, p) };
        return;
    }
    let number = objects::number_of(value).unwrap_or(0.0);
    // SAFETY (each): the setters take the types sent.
    unsafe {
        match k.as_str() {
            "beginTime" => return msg_send![obj, setBeginTime: number],
            "duration" => return msg_send![obj, setDuration: number],
            "timeOffset" => return msg_send![obj, setTimeOffset: number],
            "repeatDuration" => return msg_send![obj, setRepeatDuration: number],
            "rasterizationScale" => return msg_send![obj, setRasterizationScale: number],
            "speed" => return msg_send![obj, setSpeed: number as f32],
            "repeatCount" => return msg_send![obj, setRepeatCount: number as f32],
            "autoreverses" => return msg_send![obj, setAutoreverses: number != 0.0],
            "shouldRasterize" => return msg_send![obj, setShouldRasterize: number != 0.0],
            "needsDisplayOnBoundsChange" => return msg_send![obj, setNeedsDisplayOnBoundsChange: number != 0.0],
            _ => {}
        }
    }
    let value = value.map(Message::retain);
    let name: Retained<NSString> = objc2_foundation::NSCopying::copy(key);
    layer.write(|m| {
        m.extras.retain(|(n, _)| n.to_string() != k);
        if let Some(v) = value {
            m.extras.push((name, v));
        }
    });
}

/// `valueForKeyPath:`: a layer key path's part (`position.x`), or key by
/// key through the objects the path reaches.
pub(crate) fn value_for_key_path(layer: &CALayerImpl, path: &str) -> Option<Retained<AnyObject>> {
    if let Some(kp) = KeyPath::parse(path)
        && kp.part != super::props::Part::Whole
    {
        let whole = layer.read(|m| m.props.get(kp.key))?;
        return objects::to_object(&super::props::part_of(&whole, kp.part)?);
    }
    let (head, rest) = match path.split_once('.') {
        Some((h, r)) => (h, Some(r)),
        None => (path, None),
    };
    let value = value_for_key(layer, head)?;
    match rest {
        None => Some(value),
        Some(rest) => {
            let rest = NSString::from_str(rest);
            // SAFETY: -valueForKeyPath: takes a path and returns an object.
            unsafe { msg_send![&*value, valueForKeyPath: &*rest] }
        }
    }
}

/// `setValue:forKeyPath:`.
pub(crate) fn set_value_for_key_path(layer: &CALayerImpl, value: Option<&AnyObject>, path: &str) {
    if let Some(kp) = KeyPath::parse(path)
        && kp.part != super::props::Part::Whole
    {
        let Some(whole) = layer.read(|m| m.props.get(kp.key)) else { return };
        let Some(part) = super::props::part_of(&whole, kp.part) else { return };
        let Some(v) = objects::to_value(value, &part) else { return };
        let Some(new) = super::props::with_part(&whole, kp.part, &v) else { return };
        let obj = objects::to_object(&new);
        apply_object(layer, kp.key, obj.as_deref(), true);
        return;
    }
    match path.split_once('.') {
        None => set_value_for_key(layer, value, &NSString::from_str(path)),
        Some((head, rest)) => {
            if let Some(target) = value_for_key(layer, head) {
                let rest = NSString::from_str(rest);
                // SAFETY: -setValue:forKeyPath: takes an object and a path.
                let _: () = unsafe { msg_send![&*target, setValue: value, forKeyPath: &*rest] };
            }
        }
    }
}

/// Set a key through its setter (`sel`), from a value of its kind.
pub(crate) fn send_setter(layer: &CALayerImpl, sel: Sel, key: Key, v: &Value, obj: Option<Retained<AnyObject>>) {
    let o: &AnyObject = as_layer(layer);
    let ptr = obj.as_deref().map_or(std::ptr::null(), |o| o as *const AnyObject);
    // SAFETY (each): the key's setter takes the type sent.
    unsafe {
        match (key, v) {
            (Key::Bounds | Key::ContentsRect | Key::ContentsCenter, Value::Rect(r)) => {
                send_set(o, sel, CGRect::new(CGPoint::new(r[0], r[1]), CGSize::new(r[2], r[3])))
            }
            (_, Value::Point(p)) => send_set(o, sel, CGPoint::new(p[0], p[1])),
            (_, Value::Size(s)) => send_set(o, sel, CGSize::new(s[0], s[1])),
            (_, Value::Transform(m)) => send_set(o, sel, super::transform::from_mat(m)),
            (Key::Opacity | Key::ShadowOpacity, Value::Number(n)) => send_set(o, sel, *n as f32),
            (_, Value::Number(n)) => send_set(o, sel, *n),
            (_, Value::Bool(b)) => send_set(o, sel, objc2::runtime::Bool::new(*b)),
            // CoreGraphics objects, sent as the setters declare them.
            (_, Value::Color(_)) => send_set(o, sel, ptr.cast::<CGColor>()),
            (_, Value::Path(_)) => send_set(o, sel, ptr.cast::<CGPath>()),
            (_, Value::Colors(_) | Value::Numbers(_)) => send_set(o, sel, ptr),
            // A rectangle for a key that isn't one: not sent.
            (_, Value::Rect(_)) => {}
        }
    }
}

// Message sends by selector, for the types key-value coding meets.

/// Send `sel` with no arguments, returning `R`.
///
/// # Safety
///
/// The method `sel` names takes no arguments and returns `R`.
unsafe fn send<R: EncodeReturn>(obj: &AnyObject, sel: Sel) -> R {
    // SAFETY: as the caller promises.
    unsafe { MessageReceiver::send_message(obj, sel, ()) }
}

/// # Safety
///
/// The method `sel` names takes no arguments and returns an object at +0.
unsafe fn send_obj(obj: &AnyObject, sel: Sel) -> Option<Retained<AnyObject>> {
    // SAFETY: as the caller promises.
    let p: *mut AnyObject = unsafe { send(obj, sel) };
    // SAFETY: a live object, retained for the caller.
    unsafe { Retained::retain(p) }
}

/// Send `sel` with one argument.
///
/// # Safety
///
/// The method `sel` names takes one argument of type `T` and returns
/// nothing.
pub(crate) unsafe fn send_set<T: EncodeArgument>(obj: &AnyObject, sel: Sel, v: T) {
    // SAFETY: as the caller promises.
    let _: () = unsafe { MessageReceiver::send_message(obj, sel, (v,)) };
}
