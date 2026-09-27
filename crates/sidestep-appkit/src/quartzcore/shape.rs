//! `CAShapeLayer` and `CAGradientLayer`: layers that draw a path or a
//! gradient over their background, and animate them (`path`, `strokeEnd`,
//! `fillColor`, `colors`, `locations`, …, with the same implicit
//! animations as a layer's own keys).

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{Message, define_class, msg_send};
use objc2_core_foundation::{CGFloat, CGPoint};
use objc2_core_graphics::{CGColor, CGPath};
use objc2_foundation::{NSArray, NSNumber, NSString};
use objc2_quartz_core::CALayer;

use super::layer::{CALayerImpl, Model, autoreleased as cf_ptr, change, imp, quiet_change};
use super::objects;
use super::props::{GradientKind, GradientProps, Key, Kind, ShapeProps};

sidestep_foundation::constant_string!(kCAFillRuleNonZero = "non-zero");
sidestep_foundation::constant_string!(kCAFillRuleEvenOdd = "even-odd");
sidestep_foundation::constant_string!(kCALineJoinMiter = "miter");
sidestep_foundation::constant_string!(kCALineJoinRound = "round");
sidestep_foundation::constant_string!(kCALineJoinBevel = "bevel");
sidestep_foundation::constant_string!(kCALineCapButt = "butt");
sidestep_foundation::constant_string!(kCALineCapRound = "round");
sidestep_foundation::constant_string!(kCALineCapSquare = "square");
sidestep_foundation::constant_string!(kCAGradientLayerAxial = "axial");
sidestep_foundation::constant_string!(kCAGradientLayerRadial = "radial");
sidestep_foundation::constant_string!(kCAGradientLayerConic = "conic");

/// The objects a shape or gradient layer was given.
#[derive(Clone, Default)]
pub(crate) struct KindObjects {
    path: Option<Retained<CGPath>>,
    fill_color: Option<Option<Retained<CGColor>>>,
    stroke_color: Option<Retained<CGColor>>,
    fill_rule: Option<Retained<NSString>>,
    line_cap: Option<Retained<NSString>>,
    line_join: Option<Retained<NSString>>,
    dash_pattern: Option<Retained<NSArray<NSNumber>>>,
    colors: Option<Retained<NSArray>>,
    locations: Option<Retained<NSArray<NSNumber>>>,
    kind: Option<Retained<NSString>>,
}

fn objects_for<R>(layer: &CALayerImpl, f: impl FnOnce(&mut KindObjects) -> R) -> R {
    layer.write(|m| f(&mut m.kind_objs))
}

/// A presentation copy's objects: the model's, but where its animations
/// change a value, one made from the value shown.
pub(crate) fn present_objects(copy: &CALayerImpl, model: &super::props::Props) {
    let (shown, mut objs) = copy.read(|m| (m.props.kind.clone(), m.kind_objs.clone()));
    match (&shown, &model.kind) {
        (Kind::Shape(now), Kind::Shape(was)) => {
            if now.path != was.path {
                objs.path = now.path.clone().map(objects::path);
            }
            if now.fill_color != was.fill_color {
                objs.fill_color = Some(now.fill_color.map(objects::color));
            }
            if now.stroke_color != was.stroke_color {
                objs.stroke_color = now.stroke_color.map(objects::color);
            }
        }
        (Kind::Gradient(now), Kind::Gradient(was)) if now.colors != was.colors => {
            objs.colors = objects::to_object(&super::props::Value::Colors(now.colors.clone()))
                .and_then(|o| o.downcast::<NSArray>().ok());
        }
        _ => {}
    }
    copy.write(|m| m.kind_objs = objs);
}

fn shape(layer: &CALayerImpl) -> ShapeProps {
    layer.read(|m| match &m.props.kind {
        Kind::Shape(s) => (**s).clone(),
        _ => ShapeProps::default(),
    })
}

fn edit_shape(layer: &CALayerImpl, key: Option<Key>, f: impl FnOnce(&mut ShapeProps)) {
    let apply = |m: &mut Model| {
        if !matches!(m.props.kind, Kind::Shape(_)) {
            m.props.kind = Kind::Shape(Box::default());
        }
        if let Kind::Shape(s) = &mut m.props.kind {
            f(s);
        }
    };
    match key {
        Some(key) => change(layer, key, apply),
        None => quiet_change(layer, apply),
    }
}

fn gradient(layer: &CALayerImpl) -> GradientProps {
    layer.read(|m| match &m.props.kind {
        Kind::Gradient(g) => (**g).clone(),
        _ => GradientProps::default(),
    })
}

fn edit_gradient(layer: &CALayerImpl, key: Option<Key>, f: impl FnOnce(&mut GradientProps)) {
    let apply = |m: &mut Model| {
        if !matches!(m.props.kind, Kind::Gradient(_)) {
            m.props.kind = Kind::Gradient(Box::default());
        }
        if let Kind::Gradient(g) = &mut m.props.kind {
            f(g);
        }
    };
    match key {
        Some(key) => change(layer, key, apply),
        None => quiet_change(layer, apply),
    }
}

fn this(l: &AnyObject) -> &CALayerImpl {
    // SAFETY: the classes below are CALayer's subclasses.
    imp(unsafe { &*(l as *const AnyObject).cast::<CALayer>() })
}

/// A shape layer's `+defaultValueForKey:` answers beyond a layer's
/// (measured).
fn shape_default(key: &str) -> Option<Retained<AnyObject>> {
    let s = |v: &str| objects::any(NSString::from_str(v));
    Some(match key {
        "fillColor" => objects::any(objects::color([0.0, 0.0, 0.0, 1.0])),
        "lineWidth" | "strokeEnd" => objects::number(1.0),
        "miterLimit" => objects::number(10.0),
        "lineCap" => s("butt"),
        "lineJoin" => s("miter"),
        "fillRule" => s("non-zero"),
        k => return super::layer::default_value(k),
    })
}

/// A gradient layer's `+defaultValueForKey:` answers beyond a layer's
/// (measured).
fn gradient_default(key: &str) -> Option<Retained<AnyObject>> {
    Some(match key {
        "startPoint" => objects::to_object(&super::props::Value::Point([0.5, 0.0]))?,
        "endPoint" => objects::to_object(&super::props::Value::Point([0.5, 1.0]))?,
        "type" => objects::any(NSString::from_str("axial")),
        k => return super::layer::default_value(k),
    })
}

define_class!(
    // SAFETY: as CALayer; the shape lives in the layer's properties.
    #[unsafe(super(CALayer, NSObject))]
    #[name = "CAShapeLayer"]
    pub(crate) struct CAShapeLayerImpl;

    impl CAShapeLayerImpl {
        #[unsafe(method_id(defaultValueForKey:))]
        fn default_value_for_key(key: &NSString) -> Option<Retained<AnyObject>> {
            shape_default(&key.to_string())
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(());
            // SAFETY: CALayer's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            let l = self::this(&this);
            l.write(|m| m.props.kind = Kind::Shape(Box::default()));
            this
        }

        #[unsafe(method(path))]
        fn path(&self) -> *mut CGPath {
            cf_ptr(objects_for(this(self), |o| o.path.clone()))
        }

        #[unsafe(method(setPath:))]
        fn set_path(&self, path: Option<&CGPath>) {
            let shape = path.map(objects::path_shape);
            let copy = shape.clone().map(objects::path);
            objects_for(this(self), |o| o.path = copy);
            edit_shape(this(self), Some(Key::Path), |s| s.path = shape);
        }

        #[unsafe(method(fillColor))]
        fn fill_color(&self) -> *mut CGColor {
            let given = objects_for(this(self), |o| o.fill_color.clone());
            match given {
                Some(c) => cf_ptr(c),
                None => cf_ptr(shape(this(self)).fill_color.map(objects::color)),
            }
        }

        #[unsafe(method(setFillColor:))]
        fn set_fill_color(&self, color: Option<&CGColor>) {
            let rgba = color.map(objects::cg_rgba);
            let color = color.map(Message::retain);
            objects_for(this(self), |o| o.fill_color = Some(color));
            edit_shape(this(self), Some(Key::FillColor), |s| s.fill_color = rgba);
        }

        #[unsafe(method_id(fillRule))]
        fn fill_rule(&self) -> Retained<NSString> {
            objects_for(this(self), |o| o.fill_rule.clone()).unwrap_or_else(|| NSString::from_str("non-zero"))
        }

        #[unsafe(method(setFillRule:))]
        fn set_fill_rule(&self, rule: &NSString) {
            let even = rule.to_string() == "even-odd";
            let copy = objc2_foundation::NSCopying::copy(rule);
            objects_for(this(self), |o| o.fill_rule = Some(copy));
            edit_shape(this(self), None, |s| s.even_odd = even);
        }

        #[unsafe(method(strokeColor))]
        fn stroke_color(&self) -> *mut CGColor {
            cf_ptr(objects_for(this(self), |o| o.stroke_color.clone()))
        }

        #[unsafe(method(setStrokeColor:))]
        fn set_stroke_color(&self, color: Option<&CGColor>) {
            let rgba = color.map(objects::cg_rgba);
            let color = color.map(Message::retain);
            objects_for(this(self), |o| o.stroke_color = color);
            edit_shape(this(self), Some(Key::StrokeColor), |s| s.stroke_color = rgba);
        }

        #[unsafe(method(strokeStart))]
        fn stroke_start(&self) -> CGFloat {
            shape(this(self)).stroke_start
        }

        #[unsafe(method(setStrokeStart:))]
        fn set_stroke_start(&self, v: CGFloat) {
            edit_shape(this(self), Some(Key::StrokeStart), |s| s.stroke_start = v);
        }

        #[unsafe(method(strokeEnd))]
        fn stroke_end(&self) -> CGFloat {
            shape(this(self)).stroke_end
        }

        #[unsafe(method(setStrokeEnd:))]
        fn set_stroke_end(&self, v: CGFloat) {
            edit_shape(this(self), Some(Key::StrokeEnd), |s| s.stroke_end = v);
        }

        #[unsafe(method(lineWidth))]
        fn line_width(&self) -> CGFloat {
            shape(this(self)).line_width
        }

        #[unsafe(method(setLineWidth:))]
        fn set_line_width(&self, v: CGFloat) {
            edit_shape(this(self), Some(Key::LineWidth), |s| s.line_width = v);
        }

        #[unsafe(method(miterLimit))]
        fn miter_limit(&self) -> CGFloat {
            shape(this(self)).miter_limit
        }

        #[unsafe(method(setMiterLimit:))]
        fn set_miter_limit(&self, v: CGFloat) {
            edit_shape(this(self), Some(Key::MiterLimit), |s| s.miter_limit = v);
        }

        #[unsafe(method_id(lineCap))]
        fn line_cap(&self) -> Retained<NSString> {
            objects_for(this(self), |o| o.line_cap.clone()).unwrap_or_else(|| NSString::from_str("butt"))
        }

        #[unsafe(method(setLineCap:))]
        fn set_line_cap(&self, cap: &NSString) {
            let v = match cap.to_string().as_str() {
                "round" => 1,
                "square" => 2,
                _ => 0,
            };
            let copy = objc2_foundation::NSCopying::copy(cap);
            objects_for(this(self), |o| o.line_cap = Some(copy));
            edit_shape(this(self), None, |s| s.line_cap = v);
        }

        #[unsafe(method_id(lineJoin))]
        fn line_join(&self) -> Retained<NSString> {
            objects_for(this(self), |o| o.line_join.clone()).unwrap_or_else(|| NSString::from_str("miter"))
        }

        #[unsafe(method(setLineJoin:))]
        fn set_line_join(&self, join: &NSString) {
            let v = match join.to_string().as_str() {
                "round" => 1,
                "bevel" => 2,
                _ => 0,
            };
            let copy = objc2_foundation::NSCopying::copy(join);
            objects_for(this(self), |o| o.line_join = Some(copy));
            edit_shape(this(self), None, |s| s.line_join = v);
        }

        #[unsafe(method(lineDashPhase))]
        fn line_dash_phase(&self) -> CGFloat {
            shape(this(self)).dash_phase
        }

        #[unsafe(method(setLineDashPhase:))]
        fn set_line_dash_phase(&self, v: CGFloat) {
            edit_shape(this(self), Some(Key::LineDashPhase), |s| s.dash_phase = v);
        }

        #[unsafe(method_id(lineDashPattern))]
        fn line_dash_pattern(&self) -> Option<Retained<NSArray<NSNumber>>> {
            objects_for(this(self), |o| o.dash_pattern.clone())
        }

        #[unsafe(method(setLineDashPattern:))]
        fn set_line_dash_pattern(&self, pattern: Option<&NSArray<NSNumber>>) {
            let values: Option<Vec<f64>> = pattern.map(|p| p.iter().map(|n| n.doubleValue()).collect());
            let copy = pattern.map(objc2_foundation::NSCopying::copy);
            objects_for(this(self), |o| o.dash_pattern = copy);
            edit_shape(this(self), None, |s| s.dash_pattern = values);
        }
    }
);

define_class!(
    // SAFETY: as CALayer; the gradient lives in the layer's properties.
    #[unsafe(super(CALayer, NSObject))]
    #[name = "CAGradientLayer"]
    pub(crate) struct CAGradientLayerImpl;

    impl CAGradientLayerImpl {
        #[unsafe(method_id(defaultValueForKey:))]
        fn default_value_for_key(key: &NSString) -> Option<Retained<AnyObject>> {
            gradient_default(&key.to_string())
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(());
            // SAFETY: CALayer's designated initializer.
            let this: Retained<Self> = unsafe { msg_send![super(this), init] };
            let l = self::this(&this);
            l.write(|m| m.props.kind = Kind::Gradient(Box::default()));
            this
        }

        #[unsafe(method_id(colors))]
        fn colors(&self) -> Option<Retained<NSArray>> {
            objects_for(this(self), |o| o.colors.clone())
        }

        #[unsafe(method(setColors:))]
        fn set_colors(&self, colors: Option<&NSArray>) {
            let values: Vec<[f64; 4]> =
                colors.map(|c| c.iter().filter_map(|o| objects::rgba_of(&o)).collect()).unwrap_or_default();
            let copy = colors.map(objc2_foundation::NSCopying::copy);
            objects_for(this(self), |o| o.colors = copy);
            edit_gradient(this(self), Some(Key::Colors), |g| g.colors = values);
        }

        #[unsafe(method_id(locations))]
        fn locations(&self) -> Option<Retained<NSArray<NSNumber>>> {
            objects_for(this(self), |o| o.locations.clone())
        }

        #[unsafe(method(setLocations:))]
        fn set_locations(&self, locations: Option<&NSArray<NSNumber>>) {
            let values: Option<Vec<f64>> = locations.map(|l| l.iter().map(|n| n.doubleValue()).collect());
            let copy = locations.map(objc2_foundation::NSCopying::copy);
            objects_for(this(self), |o| o.locations = copy);
            edit_gradient(this(self), Some(Key::Locations), |g| g.locations = values);
        }

        #[unsafe(method(startPoint))]
        fn start_point(&self) -> CGPoint {
            let [x, y] = gradient(this(self)).start;
            CGPoint::new(x, y)
        }

        #[unsafe(method(setStartPoint:))]
        fn set_start_point(&self, p: CGPoint) {
            edit_gradient(this(self), Some(Key::StartPoint), |g| g.start = [p.x, p.y]);
        }

        #[unsafe(method(endPoint))]
        fn end_point(&self) -> CGPoint {
            let [x, y] = gradient(this(self)).end;
            CGPoint::new(x, y)
        }

        #[unsafe(method(setEndPoint:))]
        fn set_end_point(&self, p: CGPoint) {
            edit_gradient(this(self), Some(Key::EndPoint), |g| g.end = [p.x, p.y]);
        }

        #[unsafe(method_id(type))]
        fn kind(&self) -> Retained<NSString> {
            objects_for(this(self), |o| o.kind.clone()).unwrap_or_else(|| NSString::from_str("axial"))
        }

        #[unsafe(method(setType:))]
        fn set_kind(&self, kind: &NSString) {
            let k = match kind.to_string().as_str() {
                "radial" => GradientKind::Radial,
                "conic" => GradientKind::Conic,
                _ => GradientKind::Axial,
            };
            let copy = objc2_foundation::NSCopying::copy(kind);
            objects_for(this(self), |o| o.kind = Some(copy));
            edit_gradient(this(self), None, |g| g.kind = k);
        }
    }
);
