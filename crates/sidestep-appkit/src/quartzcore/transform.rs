//! `CATransform3D`'s functions, `CACurrentMediaTime`, `CAFrameRateRange`'s,
//! and `NSValue`'s `CATransform3D` methods (a category).

use std::ffi::c_float;

use objc2::encode::Encode;
use objc2::rc::Retained;
use objc2::{AnyThread, ClassType, define_class, msg_send};
use objc2_core_foundation::{CFTimeInterval, CGAffineTransform, CGFloat};
use objc2_foundation::NSValue;
use objc2_quartz_core::{CAFrameRateRange, CATransform3D};

use super::math::{self, Mat};

pub(crate) fn to_mat(t: &CATransform3D) -> Mat {
    [t.m11, t.m12, t.m13, t.m14, t.m21, t.m22, t.m23, t.m24, t.m31, t.m32, t.m33, t.m34, t.m41, t.m42, t.m43, t.m44]
}

pub(crate) fn from_mat(m: &Mat) -> CATransform3D {
    CATransform3D {
        m11: m[0],
        m12: m[1],
        m13: m[2],
        m14: m[3],
        m21: m[4],
        m22: m[5],
        m23: m[6],
        m24: m[7],
        m31: m[8],
        m32: m[9],
        m33: m[10],
        m34: m[11],
        m41: m[12],
        m42: m[13],
        m43: m[14],
        m44: m[15],
    }
}

#[unsafe(no_mangle)]
pub static CATransform3DIdentity: CATransform3D = CATransform3D {
    m11: 1.0,
    m12: 0.0,
    m13: 0.0,
    m14: 0.0,
    m21: 0.0,
    m22: 1.0,
    m23: 0.0,
    m24: 0.0,
    m31: 0.0,
    m32: 0.0,
    m33: 1.0,
    m34: 0.0,
    m41: 0.0,
    m42: 0.0,
    m43: 0.0,
    m44: 1.0,
};

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DIsIdentity(t: CATransform3D) -> bool {
    to_mat(&t) == math::IDENTITY
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DEqualToTransform(a: CATransform3D, b: CATransform3D) -> bool {
    to_mat(&a) == to_mat(&b)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DMakeTranslation(tx: CGFloat, ty: CGFloat, tz: CGFloat) -> CATransform3D {
    from_mat(&math::translation(tx, ty, tz))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DMakeScale(sx: CGFloat, sy: CGFloat, sz: CGFloat) -> CATransform3D {
    from_mat(&math::scale(sx, sy, sz))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DMakeRotation(
    angle: CGFloat,
    x: CGFloat,
    y: CGFloat,
    z: CGFloat,
) -> CATransform3D {
    from_mat(&math::rotation(angle, x, y, z))
}

/// `t` translated first: the translation, then `t`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DTranslate(
    t: CATransform3D,
    tx: CGFloat,
    ty: CGFloat,
    tz: CGFloat,
) -> CATransform3D {
    from_mat(&math::concat(&math::translation(tx, ty, tz), &to_mat(&t)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DScale(t: CATransform3D, sx: CGFloat, sy: CGFloat, sz: CGFloat) -> CATransform3D {
    from_mat(&math::concat(&math::scale(sx, sy, sz), &to_mat(&t)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DRotate(
    t: CATransform3D,
    angle: CGFloat,
    x: CGFloat,
    y: CGFloat,
    z: CGFloat,
) -> CATransform3D {
    from_mat(&math::concat(&math::rotation(angle, x, y, z), &to_mat(&t)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DConcat(a: CATransform3D, b: CATransform3D) -> CATransform3D {
    from_mat(&math::concat(&to_mat(&a), &to_mat(&b)))
}

/// The inverse, or `t` itself when it has none (as on macOS).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DInvert(t: CATransform3D) -> CATransform3D {
    math::invert(&to_mat(&t)).map_or(t, |m| from_mat(&m))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DMakeAffineTransform(m: CGAffineTransform) -> CATransform3D {
    let mut t = math::IDENTITY;
    t[0] = m.a;
    t[1] = m.b;
    t[4] = m.c;
    t[5] = m.d;
    t[12] = m.tx;
    t[13] = m.ty;
    from_mat(&t)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DIsAffine(t: CATransform3D) -> bool {
    math::is_affine(&to_mat(&t))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CATransform3DGetAffineTransform(t: CATransform3D) -> CGAffineTransform {
    CGAffineTransform { a: t.m11, b: t.m12, c: t.m21, d: t.m22, tx: t.m41, ty: t.m42 }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CACurrentMediaTime() -> CFTimeInterval {
    math::media_now()
}

#[unsafe(no_mangle)]
pub static CAFrameRateRangeDefault: CAFrameRateRange = CAFrameRateRange { minimum: 0.0, maximum: 0.0, preferred: 0.0 };

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CAFrameRateRangeMake(
    minimum: c_float,
    maximum: c_float,
    preferred: c_float,
) -> CAFrameRateRange {
    CAFrameRateRange { minimum, maximum, preferred }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CAFrameRateRangeIsEqualToRange(range: CAFrameRateRange, other: CAFrameRateRange) -> bool {
    range == other
}

// NSValue's CATransform3D methods.

define_class!(
    // SAFETY: a helper whose methods a category adds to NSValue; they
    // don't touch their receiver's storage.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "_SidestepValueTransform3D"]
    struct ValueTransform3D;

    impl ValueTransform3D {
        #[unsafe(method_id(valueWithCATransform3D:))]
        fn value_with_transform(t: CATransform3D) -> Retained<NSValue> {
            value_of(&t)
        }

        #[unsafe(method(CATransform3DValue))]
        fn transform_value(&self) -> CATransform3D {
            // SAFETY: the category puts this on NSValue.
            let value: &NSValue = unsafe { &*(self as *const Self).cast::<NSValue>() };
            transform_of(value).unwrap_or(CATransform3DIdentity)
        }
    }
);

sidestep_runtime::category!("NSValue"(SidestepCATransform3D), |category| {
    // SAFETY: the helper's methods read their receiver only as an NSValue.
    unsafe { category.add_methods_of(ValueTransform3D::class()) };
});

/// A new value holding `t`, encoded as `CATransform3D`.
pub(crate) fn value_of(t: &CATransform3D) -> Retained<NSValue> {
    let encoding = std::ffi::CString::new(CATransform3D::ENCODING.to_string()).expect("an encoding");
    // SAFETY: the bytes are a CATransform3D, which the encoding describes.
    unsafe {
        msg_send![
            NSValue::alloc(),
            initWithBytes: (t as *const CATransform3D).cast::<std::ffi::c_void>(),
            objCType: encoding.as_ptr()
        ]
    }
}

/// The transform a value holds, if it holds one (a value whose encoding
/// is `CATransform3D`'s).
pub(crate) fn transform_of(value: &NSValue) -> Option<CATransform3D> {
    // SAFETY: -objCType returns the value's encoding, a C string.
    let t: *const std::ffi::c_char = unsafe { msg_send![value, objCType] };
    if t.is_null() {
        return None;
    }
    // SAFETY: as -objCType promises.
    let enc = unsafe { std::ffi::CStr::from_ptr(t) }.to_bytes();
    if !enc.starts_with(b"{CATransform3D") {
        return None;
    }
    let mut out = CATransform3DIdentity;
    // SAFETY: the value holds a CATransform3D.
    let _: () = unsafe {
        msg_send![value, getValue: (&mut out as *mut CATransform3D).cast::<std::ffi::c_void>(), size: size_of::<CATransform3D>()]
    };
    Some(out)
}
