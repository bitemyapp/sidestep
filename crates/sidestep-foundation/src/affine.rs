//! `NSAffineTransform`: a 2D affine transform, kept as the six numbers of
//! its `NSAffineTransformStruct`. A point maps as
//! `(x·m11 + y·m21 + tX, x·m12 + y·m22 + tY)`.
//!
//! As in Foundation, the building methods (`translateXBy:yBy:`,
//! `rotateByDegrees:`, `scaleBy:`) apply their step *before* the transform
//! built so far: the step made last is the first a point goes through.
//! `appendTransform:` makes the other transform apply after this one, and
//! `prependTransform:` before. AppKit's drawing methods (`-set`,
//! `-concat`, `-transformBezierPath:`) are added by sidestep-appkit.

use std::cell::Cell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSAffineTransform, NSAffineTransformStruct, NSCopying, NSPoint, NSSize, NSZone};

const IDENTITY: NSAffineTransformStruct =
    NSAffineTransformStruct { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, tX: 0.0, tY: 0.0 };

/// `a` then `b`: the transform taking a point through `a` first.
fn then(a: &NSAffineTransformStruct, b: &NSAffineTransformStruct) -> NSAffineTransformStruct {
    NSAffineTransformStruct {
        m11: a.m11 * b.m11 + a.m12 * b.m21,
        m12: a.m11 * b.m12 + a.m12 * b.m22,
        m21: a.m21 * b.m11 + a.m22 * b.m21,
        m22: a.m21 * b.m12 + a.m22 * b.m22,
        tX: a.tX * b.m11 + a.tY * b.m21 + b.tX,
        tY: a.tX * b.m12 + a.tY * b.m22 + b.tY,
    }
}

fn inverse(t: &NSAffineTransformStruct) -> Option<NSAffineTransformStruct> {
    let det = t.m11 * t.m22 - t.m12 * t.m21;
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let (m11, m12, m21, m22) = (t.m22 / det, -t.m12 / det, -t.m21 / det, t.m11 / det);
    Some(NSAffineTransformStruct { m11, m12, m21, m22, tX: -(t.tX * m11 + t.tY * m21), tY: -(t.tX * m12 + t.tY * m22) })
}

pub(crate) struct AffineIvars {
    t: Cell<NSAffineTransformStruct>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements.
    #[unsafe(super(NSObject))]
    #[name = "NSAffineTransform"]
    #[ivars = AffineIvars]
    pub(crate) struct NSAffineTransformImpl;

    impl NSAffineTransformImpl {
        #[unsafe(method_id(transform))]
        fn transform() -> Retained<NSAffineTransform> {
            make(IDENTITY)
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(AffineIvars { t: Cell::new(IDENTITY) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(initWithTransform:))]
        fn init_with_transform(this: Allocated<Self>, other: &NSAffineTransform) -> Retained<Self> {
            let this = this.set_ivars(AffineIvars { t: Cell::new(value(other)) });
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(translateXBy:yBy:))]
        fn translate(&self, x: f64, y: f64) {
            self.first(NSAffineTransformStruct { tX: x, tY: y, ..IDENTITY });
        }

        #[unsafe(method(rotateByDegrees:))]
        fn rotate_by_degrees(&self, degrees: f64) {
            self.rotate(degrees.to_radians());
        }

        #[unsafe(method(rotateByRadians:))]
        fn rotate_by_radians(&self, radians: f64) {
            self.rotate(radians);
        }

        #[unsafe(method(scaleBy:))]
        fn scale_by(&self, s: f64) {
            self.first(NSAffineTransformStruct { m11: s, m22: s, ..IDENTITY });
        }

        #[unsafe(method(scaleXBy:yBy:))]
        fn scale_xy(&self, x: f64, y: f64) {
            self.first(NSAffineTransformStruct { m11: x, m22: y, ..IDENTITY });
        }

        #[unsafe(method(invert))]
        fn invert(&self) {
            let t = self.ivars().t.get();
            match inverse(&t) {
                Some(inv) => self.ivars().t.set(inv),
                None => panic!("NSAffineTransform: transform has no inverse"),
            }
        }

        #[unsafe(method(appendTransform:))]
        fn append(&self, other: &NSAffineTransform) {
            let t = self.ivars().t.get();
            self.ivars().t.set(then(&t, &value(other)));
        }

        #[unsafe(method(prependTransform:))]
        fn prepend(&self, other: &NSAffineTransform) {
            self.first(value(other));
        }

        #[unsafe(method(transformPoint:))]
        fn transform_point(&self, p: NSPoint) -> NSPoint {
            let t = self.ivars().t.get();
            NSPoint::new(p.x * t.m11 + p.y * t.m21 + t.tX, p.x * t.m12 + p.y * t.m22 + t.tY)
        }

        #[unsafe(method(transformSize:))]
        fn transform_size(&self, s: NSSize) -> NSSize {
            let t = self.ivars().t.get();
            NSSize::new(s.width * t.m11 + s.height * t.m21, s.width * t.m12 + s.height * t.m22)
        }

        #[unsafe(method(transformStruct))]
        fn transform_struct(&self) -> NSAffineTransformStruct {
            self.ivars().t.get()
        }

        #[unsafe(method(setTransformStruct:))]
        fn set_transform_struct(&self, t: NSAffineTransformStruct) {
            self.ivars().t.set(t);
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<NSAffineTransform> {
            make(self.ivars().t.get())
        }
    }

    unsafe impl NSObjectProtocol for NSAffineTransformImpl {
        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&objc2::runtime::AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSAffineTransform>()).is_some_and(|o| {
                let (a, b) = (self.ivars().t.get(), value(o));
                (a.m11, a.m12, a.m21, a.m22, a.tX, a.tY) == (b.m11, b.m12, b.m21, b.m22, b.tX, b.tY)
            })
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> usize {
            let t = self.ivars().t.get();
            [t.m11, t.m12, t.m21, t.m22, t.tX, t.tY].iter().fold(0usize, |h, v| h.wrapping_mul(31) ^ v.to_bits() as usize)
        }
    }

    unsafe impl NSCopying for NSAffineTransformImpl {}
);

impl NSAffineTransformImpl {
    /// Make `step` the first thing a point goes through.
    fn first(&self, step: NSAffineTransformStruct) {
        let t = self.ivars().t.get();
        self.ivars().t.set(then(&step, &t));
    }

    fn rotate(&self, radians: f64) {
        let (sin, cos) = radians.sin_cos();
        self.first(NSAffineTransformStruct { m11: cos, m12: sin, m21: -sin, m22: cos, ..IDENTITY });
    }
}

fn make(t: NSAffineTransformStruct) -> Retained<NSAffineTransform> {
    let this = NSAffineTransformImpl::alloc().set_ivars(AffineIvars { t: Cell::new(t) });
    // SAFETY: NSObject's designated initializer.
    let this: Retained<NSAffineTransformImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSAffineTransformImpl is the class NSAffineTransform names.
    unsafe { Retained::cast_unchecked(this) }
}

/// The numbers of any transform, Sidestep's or an app subclass's.
fn value(t: &NSAffineTransform) -> NSAffineTransformStruct {
    t.transformStruct()
}
