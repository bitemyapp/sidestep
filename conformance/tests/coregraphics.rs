//! CoreGraphics without AppKit, checked against macOS: geometry and
//! transforms, colors and spaces, paths (their element structure, bounds,
//! hit testing), bitmap contexts in every layout they take and the pixels
//! drawing leaves in them, images, gradients, shadows, data providers and
//! fonts. Everything draws into bitmap contexts of explicit sizes, so no
//! window, screen scale or appearance is involved; CoreGraphics runs on any
//! thread, so the default harness does.
//!
//! Pixel values allow for antialiasing, which rasterizers compute a little
//! differently (within 3 of 255 unless a test says otherwise), and for
//! gradients, which macOS dithers in 8-bit contexts (within 12).

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_core_foundation::{
    CFData, CFEqual, CFGetRetainCount, CFGetTypeID, CFHash, CFRetained, CFString, CFType, CGAffineTransform, CGPoint,
    CGRect, CGRectEdge, CGSize, ConcreteType,
};
use objc2_core_graphics::*;

use sidestep as _;

fn r(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

fn pt(x: f64, y: f64) -> CGPoint {
    CGPoint::new(x, y)
}

fn srgb() -> CFRetained<CGColorSpace> {
    CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB")
}

/// A `w` × `h` context: 8-bit RGBA, premultiplied, alpha last, sRGB.
fn ctx(w: usize, h: usize) -> CFRetained<CGContext> {
    // SAFETY: no data: the context allocates.
    unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&srgb()), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .expect("a bitmap context")
}

/// The bytes of pixel (`x`, `y`) of a 32-bit context, row 0 the top.
fn px(c: &CGContext, x: usize, y: usize) -> [u8; 4] {
    let d = CGBitmapContextGetData(Some(c)) as *const u8;
    let bpr = CGBitmapContextGetBytesPerRow(Some(c));
    // SAFETY: inside the context's memory.
    unsafe { std::ptr::read(d.add(y * bpr + x * 4).cast::<[u8; 4]>()) }
}

fn alpha(c: &CGContext, x: usize, y: usize) -> u8 {
    px(c, x, y)[3]
}

fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= tol)
}

#[track_caller]
fn assert_px(c: &CGContext, x: usize, y: usize, want: [u8; 4], tol: u8) {
    let got = px(c, x, y);
    assert!(near(got, want, tol), "pixel ({x}, {y}) is {got:?}, want {want:?}");
}

#[track_caller]
fn close(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "{a} is not {b}");
}

#[track_caller]
fn same_rect(a: CGRect, b: CGRect) {
    for (x, y) in [
        (a.origin.x, b.origin.x),
        (a.origin.y, b.origin.y),
        (a.size.width, b.size.width),
        (a.size.height, b.size.height),
    ] {
        assert!((x - y).abs() < 1e-6 || (x == y), "{a:?} is not {b:?}");
    }
}

/// A path's elements: a letter each (M, L, Q, C, Z) and its points,
/// rounded to 4 decimals.
fn els(p: &CGPath) -> String {
    let mut out = String::new();
    unsafe extern "C-unwind" fn f(info: *mut c_void, el: NonNull<CGPathElement>) {
        // SAFETY: the info is the string; the element lives through the call.
        let (out, el) = unsafe { (&mut *info.cast::<String>(), el.as_ref()) };
        let (name, n) = match el.r#type {
            CGPathElementType::MoveToPoint => ("M", 1),
            CGPathElementType::AddLineToPoint => ("L", 1),
            CGPathElementType::AddQuadCurveToPoint => ("Q", 2),
            CGPathElementType::AddCurveToPoint => ("C", 3),
            _ => ("Z", 0),
        };
        out.push_str(name);
        for i in 0..n {
            // SAFETY: the element has `n` points.
            let p = unsafe { *el.points.as_ptr().add(i) };
            let f = |v: f64| {
                let s = format!("{:.4}", v + 0.0);
                if s == "-0.0000" { "0.0000".to_string() } else { s }
            };
            out.push_str(&format!(" {},{}", f(p.x), f(p.y)));
        }
        out.push_str("; ");
    }
    // SAFETY: the applier reads the string it's given.
    unsafe { CGPath::apply(Some(p), (&mut out as *mut String).cast(), Some(f)) };
    out.trim_end().to_string()
}

const NOXF: *const CGAffineTransform = std::ptr::null();

fn mutable(f: impl FnOnce(&CGMutablePath)) -> CFRetained<CGMutablePath> {
    let p = CGMutablePath::new();
    f(&p);
    p
}

// Types and objects.

#[test]
fn type_ids_and_objects() {
    let ids = [
        CGColor::type_id(),
        CGColorSpace::type_id(),
        CGPath::type_id(),
        CGContext::type_id(),
        CGImage::type_id(),
        CGGradient::type_id(),
        CGDataProvider::type_id(),
        CGDataConsumer::type_id(),
        CGFont::type_id(),
        CGShading::type_id(),
        CGFunction::type_id(),
    ];
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            assert_ne!(a, b, "type IDs are distinct");
        }
    }
    let names = ["CGColor", "CGColorSpace", "CGPath", "CGContext", "CGImage", "CGGradient", "CGDataProvider"];
    for (id, name) in ids.iter().zip(names) {
        let desc = objc2_core_foundation::CFCopyTypeIDDescription(*id).expect("a name");
        assert_eq!(desc.to_string(), name);
    }
    let color = CGColor::new_generic_rgb(1.0, 0.0, 0.0, 1.0);
    let path = unsafe { CGPath::with_rect(r(0.0, 0.0, 1.0, 1.0), NOXF) };
    let c = ctx(1, 1);
    assert_eq!(CFGetTypeID(Some(&color)), CGColor::type_id());
    assert_eq!(CFGetTypeID(Some(&path)), CGPath::type_id());
    assert_eq!(CFGetTypeID(Some(&c)), CGContext::type_id());
    assert_eq!(CFGetTypeID(Some(&srgb())), CGColorSpace::type_id());
    let mp = CGMutablePath::new();
    assert_eq!(CFGetTypeID(Some(&mp)), CGPath::type_id(), "mutable paths are paths");
    // Downcasting goes by type ID.
    let any: &CFType = &color;
    assert!(any.downcast_ref::<CGColor>().is_some());
    assert!(any.downcast_ref::<CGPath>().is_none());
    // A new object is the caller's alone; retaining adds one.
    assert_eq!(CFGetRetainCount(Some(&color)), 1);
    let again = color.clone();
    assert_eq!(CFGetRetainCount(Some(&color)), 2);
    drop(again);
    assert_eq!(CFGetRetainCount(Some(&color)), 1);
}

// Geometry.

#[test]
fn rectangles() {
    let null = unsafe { CGRectNull };
    let a = r(0.0, 0.0, 10.0, 10.0);
    same_rect(CGRectUnion(a, r(5.0, 5.0, 10.0, 10.0)), r(0.0, 0.0, 15.0, 15.0));
    same_rect(CGRectIntersection(a, r(5.0, 5.0, 10.0, 10.0)), r(5.0, 5.0, 5.0, 5.0));
    assert!(CGRectIsNull(CGRectIntersection(a, r(20.0, 20.0, 1.0, 1.0))), "disjoint: null");
    assert!(null.origin.x.is_infinite() && null.size.width == 0.0);
    // Touching rectangles intersect in an empty rectangle and don't
    // intersect; an empty one inside does.
    let touching = CGRectIntersection(a, r(10.0, 0.0, 5.0, 5.0));
    assert!(!CGRectIsNull(touching) && CGRectIsEmpty(touching));
    assert!(!CGRectIntersectsRect(a, r(10.0, 0.0, 5.0, 5.0)));
    assert!(CGRectIntersectsRect(a, r(5.0, 5.0, 0.0, 0.0)));
    // Negative sizes are standardized.
    let neg = r(10.0, 10.0, -5.0, -5.0);
    assert_eq!(
        (CGRectGetMinX(neg), CGRectGetMaxX(neg), CGRectGetMidX(neg), CGRectGetWidth(neg)),
        (5.0, 10.0, 7.5, 5.0)
    );
    same_rect(CGRectStandardize(neg), r(5.0, 5.0, 5.0, 5.0));
    same_rect(CGRectInset(neg, 1.0, 1.0), r(6.0, 6.0, 3.0, 3.0));
    same_rect(CGRectOffset(neg, 1.0, 1.0), r(6.0, 6.0, 5.0, 5.0));
    assert!(CGRectEqualToRect(neg, r(5.0, 5.0, 5.0, 5.0)));
    assert!(CGRectIsNull(CGRectInset(a, 6.0, 1.0)), "inset past nothing: null");
    // Containment is half open.
    assert!(CGRectContainsPoint(a, pt(0.0, 0.0)));
    assert!(!CGRectContainsPoint(a, pt(10.0, 5.0)));
    assert!(CGRectContainsRect(a, r(0.0, 0.0, 0.0, 0.0)));
    // The null rectangle: a union ignores it; others give it back.
    same_rect(CGRectUnion(a, null), a);
    same_rect(CGRectUnion(a, r(20.0, 20.0, 0.0, 0.0)), r(0.0, 0.0, 20.0, 20.0));
    assert!(CGRectIsNull(CGRectOffset(null, 1.0, 1.0)));
    assert!(CGRectIsEmpty(null) && !CGRectIsEmpty(unsafe { CGRectInfinite }));
    assert!(CGRectIsInfinite(unsafe { CGRectInfinite }));
    assert_eq!((CGRectGetMinX(null), CGRectGetWidth(null)), (f64::INFINITY, 0.0));
    same_rect(CGRectIntegral(r(0.5, 0.25, 1.3, 1.9)), r(0.0, 0.0, 2.0, 3.0));
    same_rect(CGRectIntegral(r(0.5, 0.25, 0.0, 0.0)), r(0.0, 0.0, 1.0, 1.0));
    let (mut slice, mut rem) = (CGRect::ZERO, CGRect::ZERO);
    unsafe { CGRectDivide(a, NonNull::from(&mut slice), NonNull::from(&mut rem), 3.0, CGRectEdge::MinYEdge) };
    same_rect(slice, r(0.0, 0.0, 10.0, 3.0));
    same_rect(rem, r(0.0, 3.0, 10.0, 7.0));
    unsafe { CGRectDivide(a, NonNull::from(&mut slice), NonNull::from(&mut rem), 30.0, CGRectEdge::MaxXEdge) };
    same_rect(slice, a);
    same_rect(rem, r(0.0, 0.0, 0.0, 10.0));
    same_rect(unsafe { CGRectZero }, r(0.0, 0.0, 0.0, 0.0));
    // Dictionary forms.
    let d = CGRectCreateDictionaryRepresentation(r(1.0, 2.0, 3.5, 4.0));
    let mut back = CGRect::ZERO;
    assert!(unsafe { CGRectMakeWithDictionaryRepresentation(Some(&d), &mut back) });
    same_rect(back, r(1.0, 2.0, 3.5, 4.0));
    let d = CGPointCreateDictionaryRepresentation(pt(1.5, 2.0));
    let mut p = CGPoint::ZERO;
    assert!(unsafe { CGPointMakeWithDictionaryRepresentation(Some(&d), &mut p) });
    assert_eq!(p, pt(1.5, 2.0));
}

#[test]
fn transforms() {
    let t = CGAffineTransformMakeRotation(std::f64::consts::FRAC_PI_2);
    close(t.b, 1.0);
    close(t.c, -1.0);
    let concat =
        CGAffineTransformConcat(CGAffineTransformMakeTranslation(1.0, 2.0), CGAffineTransformMakeScale(2.0, 3.0));
    assert_eq!((concat.a, concat.d, concat.tx, concat.ty), (2.0, 3.0, 2.0, 6.0), "translate, then scale");
    let t = CGAffineTransformTranslate(CGAffineTransformMakeScale(2.0, 3.0), 1.0, 1.0);
    assert_eq!((t.tx, t.ty), (2.0, 3.0), "translated in the transformed space");
    let t = CGAffineTransformScale(CGAffineTransformMakeTranslation(1.0, 2.0), 2.0, 3.0);
    assert_eq!((t.a, t.d, t.tx, t.ty), (2.0, 3.0, 1.0, 2.0));
    let t = CGAffineTransformRotate(CGAffineTransformMakeScale(2.0, 1.0), 1.0);
    close(t.a, 2.0 * 1f64.cos());
    close(t.b, 1f64.sin());
    close(t.c, -2.0 * 1f64.sin());
    let inv = CGAffineTransformInvert(concat);
    close(inv.a, 0.5);
    close(inv.tx, -1.0);
    close(inv.ty, -2.0);
    let singular = CGAffineTransformMakeScale(0.0, 1.0);
    assert!(CGAffineTransformEqualToTransform(CGAffineTransformInvert(singular), singular), "no inverse: unchanged");
    assert_eq!(CGPointApplyAffineTransform(pt(1.0, 1.0), concat), pt(4.0, 9.0));
    let s = CGSizeApplyAffineTransform(CGSize::new(1.0, 1.0), CGAffineTransformMakeRotation(1.0));
    close(s.width, 1f64.cos() - 1f64.sin());
    let b = CGRectApplyAffineTransform(r(0.0, 0.0, 10.0, 10.0), CGAffineTransformMakeRotation(0.5));
    close(b.origin.x, -10.0 * 0.5f64.sin());
    close(b.size.width, 10.0 * (0.5f64.sin() + 0.5f64.cos()));
    assert!(CGRectIsNull(CGRectApplyAffineTransform(unsafe { CGRectNull }, concat)));
    assert!(CGAffineTransformIsIdentity(unsafe { CGAffineTransformIdentity }));
    // Decomposition: a rotation of a shear of a scale, mirrored scales
    // negative.
    let d = CGAffineTransformDecompose(CGAffineTransformMake(1.0, 2.0, 3.0, 4.0, 5.0, 6.0));
    close(d.scale.width, -5f64.sqrt());
    close(d.scale.height, 2.0 / 5f64.sqrt());
    close(d.horizontalShear, -5.5);
    close(d.rotation, 2f64.atan2(1.0) - std::f64::consts::PI);
    assert_eq!((d.translation.dx, d.translation.dy), (5.0, 6.0));
    let d = CGAffineTransformDecompose(CGAffineTransformMake(1.0, 0.0, 1.0, 1.0, 0.0, 0.0));
    assert_eq!((d.scale.width, d.scale.height, d.horizontalShear, d.rotation), (1.0, 1.0, 1.0, 0.0));
    for t in [CGAffineTransformMake(1.0, 2.0, 3.0, 4.0, 5.0, 6.0), CGAffineTransformMakeScale(1.0, -1.0)] {
        let back = CGAffineTransformMakeWithComponents(CGAffineTransformDecompose(t));
        for (a, b) in [(t.a, back.a), (t.b, back.b), (t.c, back.c), (t.d, back.d), (t.tx, back.tx), (t.ty, back.ty)] {
            assert!((a - b).abs() < 1e-9, "{t:?} back as {back:?}");
        }
    }
}

// Colors.

fn comps(c: &CGColor) -> Vec<f64> {
    let n = CGColor::number_of_components(Some(c));
    // SAFETY: the color has `n` components.
    unsafe { std::slice::from_raw_parts(CGColor::components(Some(c)), n) }.to_vec()
}

fn space_name(s: Option<&CGColorSpace>) -> Option<String> {
    CGColorSpace::name(s).map(|n| n.to_string())
}

#[test]
#[allow(deprecated)]
fn color_spaces() {
    let names: [(&CFString, &str, CGColorSpaceModel, usize); 8] = unsafe {
        [
            (kCGColorSpaceSRGB, "kCGColorSpaceSRGB", CGColorSpaceModel::RGB, 3),
            (kCGColorSpaceGenericRGB, "kCGColorSpaceGenericRGB", CGColorSpaceModel::RGB, 3),
            (kCGColorSpaceGenericGray, "kCGColorSpaceGenericGray", CGColorSpaceModel::Monochrome, 1),
            (kCGColorSpaceGenericGrayGamma2_2, "kCGColorSpaceGenericGrayGamma2_2", CGColorSpaceModel::Monochrome, 1),
            (kCGColorSpaceDisplayP3, "kCGColorSpaceDisplayP3", CGColorSpaceModel::RGB, 3),
            (kCGColorSpaceExtendedSRGB, "kCGColorSpaceExtendedSRGB", CGColorSpaceModel::RGB, 3),
            (kCGColorSpaceLinearSRGB, "kCGColorSpaceLinearSRGB", CGColorSpaceModel::RGB, 3),
            (kCGColorSpaceGenericCMYK, "kCGColorSpaceGenericCMYK", CGColorSpaceModel::CMYK, 4),
        ]
    };
    for (constant, name, model, n) in names {
        assert_eq!(constant.to_string(), name);
        let s = CGColorSpace::with_name(Some(constant)).expect(name);
        assert_eq!(space_name(Some(&s)).as_deref(), Some(name));
        assert_eq!((CGColorSpace::model(Some(&s)), CGColorSpace::number_of_components(Some(&s))), (model, n), "{name}");
        let again = CGColorSpace::with_name(Some(constant)).expect(name);
        assert!(std::ptr::eq(&*s, &*again), "{name}: one shared space");
    }
    // Aliases of the HDR spaces name the new ones.
    assert_eq!(unsafe { kCGColorSpaceITUR_2020_PQ }.to_string(), "kCGColorSpaceITUR_2100_PQ");
    assert_eq!(unsafe { kCGColorSpaceDisplayP3_PQ_EOTF }.to_string(), "kCGColorSpaceDisplayP3_PQ");
    assert!(CGColorSpace::with_name(Some(&CFString::from_str("nope"))).is_none());
    let extended = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedSRGB })).unwrap();
    assert!(extended.uses_extended_range() && !srgb().uses_extended_range());
    let d = CGColorSpace::new_device_rgb().unwrap();
    assert_eq!(space_name(Some(&d)).as_deref(), Some("kCGColorSpaceDeviceRGB"));
    assert!(std::ptr::eq(&*d, &*CGColorSpace::new_device_rgb().unwrap()));
    assert_eq!(space_name(CGColorSpace::new_device_gray().as_deref()).as_deref(), Some("kCGColorSpaceDeviceGray"));
    assert_eq!(space_name(CGColorSpace::new_device_cmyk().as_deref()).as_deref(), Some("kCGColorSpaceDeviceCMYK"));
    let pattern = CGColorSpace::new_pattern(None).unwrap();
    assert_eq!(CGColorSpace::model(Some(&pattern)), CGColorSpaceModel::Pattern);
    let table = [255u8, 0, 0, 0, 0, 255];
    let indexed = unsafe { CGColorSpace::new_indexed(Some(&srgb()), 1, table.as_ptr()) }.unwrap();
    assert_eq!(
        (CGColorSpace::model(Some(&indexed)), CGColorSpace::color_table_count(Some(&indexed))),
        (CGColorSpaceModel::Indexed, 2)
    );
    assert!(CFEqual(Some(&srgb()), Some(&CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap())));
    // Only indexed spaces and pattern spaces made with one have a base
    // space; for the others CGColorSpaceCopyBaseColorSpace returns NULL
    // (objc2-core-graphics 0.3.2 declared it non-null, and panicked, before
    // the objc2 fork's fix).
    assert!(srgb().copy_base_color_space().is_none());
    assert!(d.copy_base_color_space().is_none());
    assert!(pattern.copy_base_color_space().is_none());
    let base = indexed.copy_base_color_space().expect("an indexed space's base");
    assert!(CFEqual(Some(&*base), Some(&srgb())));
    let colored = CGColorSpace::new_pattern(Some(&srgb())).unwrap();
    assert!(colored.copy_base_color_space().is_some_and(|b| CFEqual(Some(&*b), Some(&srgb()))));
    let copied = objc2_core_graphics::CGColorSpaceCopyBaseColorSpace(&indexed);
    assert!(copied.is_some_and(|b| CFEqual(Some(&*b), Some(&srgb()))));
}

#[test]
fn colors() {
    let c = CGColor::new_generic_rgb(1.0, 0.5, 0.25, 0.75);
    assert_eq!(comps(&c), [1.0, 0.5, 0.25, 0.75]);
    assert_eq!(CGColor::alpha(Some(&c)), 0.75);
    assert_eq!(space_name(CGColor::color_space(Some(&c)).as_deref()).as_deref(), Some("kCGColorSpaceGenericRGB"));
    // Components are clamped (the space isn't extended).
    assert_eq!(comps(&CGColor::new_generic_rgb(1.5, -0.5, 0.25, 2.0)), [1.0, 0.0, 0.25, 1.0]);
    let s = CGColor::new_srgb(1.5, -0.5, 0.25, 2.0);
    assert_eq!(comps(&s), [1.0, 0.0, 0.25, 1.0]);
    assert_eq!(space_name(CGColor::color_space(Some(&s)).as_deref()).as_deref(), Some("kCGColorSpaceSRGB"));
    let g = CGColor::new_generic_gray(0.5, 1.0);
    assert_eq!(comps(&g), [0.5, 1.0]);
    assert_eq!(space_name(CGColor::color_space(Some(&g)).as_deref()).as_deref(), Some("kCGColorSpaceGenericGray"));
    let k = CGColor::new_generic_cmyk(0.1, 0.2, 0.3, 0.4, 0.5);
    assert_eq!(comps(&k), [0.1, 0.2, 0.3, 0.4, 0.5]);
    // Constants: shared, gray gamma 2.2.
    for (name, want) in
        unsafe { [(kCGColorClear, [0.0, 0.0]), (kCGColorBlack, [0.0, 1.0]), (kCGColorWhite, [1.0, 1.0])] }
    {
        let a = CGColor::constant_color(Some(name)).unwrap();
        assert_eq!(comps(&a), want, "{name}");
        assert_eq!(
            space_name(CGColor::color_space(Some(&a)).as_deref()).as_deref(),
            Some("kCGColorSpaceGenericGrayGamma2_2")
        );
        assert!(std::ptr::eq(&*a, &*CGColor::constant_color(Some(name)).unwrap()));
    }
    assert_eq!(unsafe { kCGColorClear }.to_string(), "kCGColorClear");
    assert!(CGColor::constant_color(Some(&CFString::from_str("nope"))).is_none());
    // Equality: same space and components.
    let a = CGColor::new_generic_rgb(1.0, 0.0, 0.0, 1.0);
    let b = CGColor::new_generic_rgb(1.0, 0.0, 0.0, 1.0);
    assert!(CGColor::equal_to_color(Some(&a), Some(&b)) && CFEqual(Some(&a), Some(&b)));
    assert_eq!(CFHash(Some(&a)), CFHash(Some(&b)));
    assert!(!CGColor::equal_to_color(Some(&a), Some(&CGColor::new_srgb(1.0, 0.0, 0.0, 1.0))));
    assert_eq!(comps(&CGColor::new_copy_with_alpha(Some(&a), 0.5).unwrap()), [1.0, 0.0, 0.0, 0.5]);
    assert!(std::ptr::eq(&*a, &*CGColor::new_copy(Some(&a)).unwrap()), "a copy is the color");
    let d = unsafe { CGColor::new(CGColorSpace::new_device_rgb().as_deref(), [0.2, 0.4, 0.6, 0.8].as_ptr()) }.unwrap();
    assert_eq!(CGColor::number_of_components(Some(&d)), 4);
    assert!(unsafe { CGColor::new(None, [0.2, 0.4, 0.6, 0.8].as_ptr()) }.is_none(), "no space, no color");
    assert!(
        unsafe { CGColor::with_pattern(CGColorSpace::new_pattern(None).as_deref(), None, std::ptr::null()) }.is_none(),
        "no pattern, no color"
    );
}

// Paths.

#[test]
fn path_shapes() {
    let p = unsafe { CGPath::with_rect(r(1.0, 2.0, 3.0, 4.0), NOXF) };
    assert_eq!(els(&p), "M 1.0000,2.0000; L 4.0000,2.0000; L 4.0000,6.0000; L 1.0000,6.0000; Z;");
    let p = unsafe { CGPath::with_rect(r(1.0, 2.0, -3.0, 4.0), NOXF) };
    assert_eq!(els(&p), "M -2.0000,2.0000; L 1.0000,2.0000; L 1.0000,6.0000; L -2.0000,6.0000; Z;", "standardized");
    let p = unsafe { CGPath::with_ellipse_in_rect(r(0.0, 0.0, 20.0, 10.0), NOXF) };
    assert_eq!(
        els(&p),
        "M 20.0000,5.0000; C 20.0000,7.7614 15.5228,10.0000 10.0000,10.0000; \
         C 4.4772,10.0000 0.0000,7.7614 0.0000,5.0000; C 0.0000,2.2386 4.4772,0.0000 10.0000,0.0000; \
         C 15.5228,0.0000 20.0000,2.2386 20.0000,5.0000; Z;"
    );
    let p = unsafe { CGPath::with_rounded_rect(r(0.0, 0.0, 20.0, 10.0), 2.0, 3.0, NOXF) };
    assert_eq!(
        els(&p),
        "M 20.0000,5.0000; L 20.0000,7.0000; C 20.0000,8.6569 19.1046,10.0000 18.0000,10.0000; \
         L 2.0000,10.0000; C 0.8954,10.0000 0.0000,8.6569 0.0000,7.0000; L 0.0000,3.0000; \
         C 0.0000,1.3431 0.8954,0.0000 2.0000,0.0000; L 18.0000,0.0000; \
         C 19.1046,0.0000 20.0000,1.3431 20.0000,3.0000; Z;"
    );
    // Corners of half the height keep their (empty) sides.
    let p = unsafe { CGPath::with_rounded_rect(r(0.0, 0.0, 20.0, 10.0), 5.0, 5.0, NOXF) };
    assert!(els(&p).starts_with("M 20.0000,5.0000; L 20.0000,5.0000; C"), "{}", els(&p));
    // No corners: a rectangle.
    for (w, h) in [(0.0, 0.0), (0.0, 3.0), (-1.0, 3.0)] {
        let p = unsafe { CGPath::with_rounded_rect(r(0.0, 0.0, 20.0, 10.0), w, h, NOXF) };
        assert_eq!(els(&p), "M 0.0000,0.0000; L 20.0000,0.0000; L 20.0000,10.0000; L 0.0000,10.0000; Z;");
    }
    let m =
        mutable(|m| unsafe { CGMutablePath::add_rounded_rect(Some(m), NOXF, r(20.0, 10.0, -20.0, -10.0), 2.0, 3.0) });
    assert!(els(&m).starts_with("M 20.0000,5.0000; L 20.0000,7.0000;"), "{}", els(&m));
    // Transforms apply to each point.
    let t = CGAffineTransformMakeScale(2.0, 3.0);
    let m = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), &t, 1.0, 1.0);
        CGMutablePath::add_quad_curve_to_point(Some(m), &t, 2.0, 2.0, 3.0, 1.0);
        CGMutablePath::add_curve_to_point(Some(m), &t, 4.0, 0.0, 5.0, 2.0, 6.0, 1.0);
    });
    assert_eq!(
        els(&m),
        "M 2.0000,3.0000; Q 4.0000,6.0000 6.0000,3.0000; C 8.0000,0.0000 10.0000,6.0000 12.0000,3.0000;"
    );
    same_rect(CGPath::bounding_box(Some(&m)), r(2.0, 0.0, 10.0, 6.0));
    let tight = CGPath::path_bounding_box(Some(&m));
    close(tight.origin.y, 2.133974596215561);
    close(tight.size.height, 2.366025403784439);
    let tt = CGAffineTransformMakeTranslation(5.0, 5.0);
    let moved = unsafe { CGPath::new_copy_by_transforming_path(Some(&m), &tt) }.unwrap();
    assert!(els(&moved).starts_with("M 7.0000,8.0000; Q 9.0000,11.0000 11.0000,8.0000;"));
    let lines = mutable(|m| unsafe {
        let pts = [pt(1.0, 1.0), pt(2.0, 3.0), pt(4.0, 1.0)];
        CGMutablePath::add_lines(Some(m), NOXF, pts.as_ptr(), 3);
        CGMutablePath::add_lines(Some(m), NOXF, pts.as_ptr(), 3);
    });
    assert_eq!(
        els(&lines),
        "M 1.0000,1.0000; L 2.0000,3.0000; L 4.0000,1.0000; M 1.0000,1.0000; L 2.0000,3.0000; L 4.0000,1.0000;"
    );
}

#[test]
fn path_building() {
    // A close adds no move; a line after it starts at the subpath's start;
    // a second close adds nothing.
    let m = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 0.0, 0.0);
        CGMutablePath::add_line_to_point(Some(m), NOXF, 10.0, 0.0);
        CGMutablePath::add_line_to_point(Some(m), NOXF, 10.0, 10.0);
        CGMutablePath::close_subpath(Some(m));
        assert_eq!(CGPath::current_point(Some(m)), pt(0.0, 0.0));
        CGMutablePath::add_line_to_point(Some(m), NOXF, 5.0, 20.0);
        CGMutablePath::close_subpath(Some(m));
        CGMutablePath::close_subpath(Some(m));
    });
    assert_eq!(els(&m), "M 0.0000,0.0000; L 10.0000,0.0000; L 10.0000,10.0000; Z; L 5.0000,20.0000; Z;");
    // A segment with no current point is dropped; so is a close.
    let m = mutable(|m| unsafe {
        CGMutablePath::add_line_to_point(Some(m), NOXF, 5.0, 20.0);
        CGMutablePath::close_subpath(Some(m));
    });
    assert_eq!(els(&m), "");
    assert!(CGPath::is_empty(Some(&m)));
    // A move after a move replaces it.
    let m = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 1.0, 1.0);
        CGMutablePath::move_to_point(Some(m), NOXF, 2.0, 2.0);
    });
    assert_eq!(els(&m), "M 2.0000,2.0000;");
    assert!(!CGPath::is_empty(Some(&m)));
    same_rect(CGPath::bounding_box(Some(&m)), r(2.0, 2.0, 0.0, 0.0));
    let m = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 5.0, 5.0);
        CGMutablePath::add_rect(Some(m), NOXF, r(0.0, 0.0, 1.0, 1.0));
        CGMutablePath::add_line_to_point(Some(m), NOXF, 7.0, 7.0);
    });
    assert_eq!(els(&m), "M 0.0000,0.0000; L 1.0000,0.0000; L 1.0000,1.0000; L 0.0000,1.0000; Z; L 7.0000,7.0000;");
    // A close after a move alone is kept.
    let m = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 0.0, 0.0);
        CGMutablePath::close_subpath(Some(m));
    });
    assert_eq!(els(&m), "M 0.0000,0.0000; Z;");
    let empty = CGMutablePath::new();
    assert_eq!(CGPath::current_point(Some(&empty)), pt(0.0, 0.0));
    assert!(CGRectIsNull(CGPath::bounding_box(Some(&empty))) && CGRectIsNull(CGPath::path_bounding_box(Some(&empty))));
    // Paths added to others: a move then the whole path.
    let rect = unsafe { CGPath::with_rect(r(0.0, 0.0, 1.0, 1.0), NOXF) };
    let m = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 5.0, 5.0);
        CGMutablePath::add_line_to_point(Some(m), NOXF, 6.0, 5.0);
        CGMutablePath::add_ellipse_in_rect(Some(m), NOXF, r(0.0, 0.0, 1.0, 1.0));
        CGMutablePath::add_path(Some(m), NOXF, Some(&rect));
    });
    assert!(els(&m).starts_with("M 5.0000,5.0000; L 6.0000,5.0000; M 1.0000,0.5000; C"), "{}", els(&m));
    assert!(els(&m).ends_with("Z; M 0.0000,0.0000; L 1.0000,0.0000; L 1.0000,1.0000; L 0.0000,1.0000; Z;"));
    // Copies: equal, not the same object; a mutable copy grows on its own.
    let copy = CGPath::new_copy(Some(&rect)).unwrap();
    assert!(!std::ptr::eq(&*copy, &*rect) && CGPath::equal_to_path(Some(&copy), Some(&rect)));
    assert!(CFEqual(Some(&copy), Some(&rect)) && CFHash(Some(&copy)) == CFHash(Some(&rect)));
    let grown = CGMutablePath::new_copy(Some(&rect)).unwrap();
    unsafe { CGMutablePath::add_line_to_point(Some(&grown), NOXF, 5.0, 5.0) };
    assert!(els(&grown).ends_with("Z; L 5.0000,5.0000;") && els(&rect).ends_with("Z;"));
    let a = mutable(|m| unsafe { CGMutablePath::move_to_point(Some(m), NOXF, 1.0, 1.0) });
    let b = mutable(|m| unsafe { CGMutablePath::move_to_point(Some(m), NOXF, 2.0, 1.0) });
    assert!(!CGPath::equal_to_path(Some(&a), Some(&b)));
    assert!(CGPath::equal_to_path(Some(&CGMutablePath::new()), Some(&CGMutablePath::new())));
}

#[test]
fn arcs() {
    let pi = std::f64::consts::PI;
    let arc = |s: f64, e: f64, cw: bool| {
        els(&mutable(|m| unsafe { CGMutablePath::add_arc(Some(m), NOXF, 10.0, 10.0, 5.0, s, e, cw) }))
    };
    assert_eq!(
        arc(0.0, pi, false),
        "M 15.0000,10.0000; C 15.0000,12.7614 12.7614,15.0000 10.0000,15.0000; C 7.2386,15.0000 5.0000,12.7614 5.0000,10.0000;"
    );
    assert_eq!(
        arc(0.0, pi, true),
        "M 15.0000,10.0000; C 15.0000,7.2386 12.7614,5.0000 10.0000,5.0000; C 7.2386,5.0000 5.0000,7.2386 5.0000,10.0000;"
    );
    assert_eq!(arc(0.0, 0.5, false), "M 15.0000,10.0000; C 15.0000,10.8377 14.7895,11.6620 14.3879,12.3971;");
    assert_eq!(arc(0.0, 0.0, false), "M 15.0000,10.0000;");
    // How many curves: quarter turns from the start and what's left.
    let curves = |s: f64, e: f64, cw: bool| arc(s, e, cw).matches('C').count();
    assert_eq!(curves(0.0, 2.0 * pi, false), 4);
    assert_eq!(curves(1.0, 7.5, false), 5, "past a whole turn counterclockwise: that far");
    assert_eq!(curves(0.0, -1.0, false), 4, "backward counterclockwise: the rest of a turn");
    assert_eq!(curves(0.0, -2.0 * pi, false), 0);
    assert_eq!(curves(0.0, 4.0 * pi, false), 8);
    assert_eq!(curves(0.0, 2.0 * pi, true), 4, "a whole turn clockwise");
    assert_eq!(curves(0.0, 3.0 * pi, true), 2);
    assert_eq!(curves(0.0, 7.0, true), 4);
    assert_eq!(curves(0.0, -7.0, true), 5);
    assert_eq!(curves(2.0, 0.0, true), 2);
    assert!(
        arc(0.0, -7.0, false).ends_with("C 11.4453,5.0000 12.8199,5.6254 13.7695,6.7151;"),
        "{}",
        arc(0.0, -7.0, false)
    );
    // After a current point: a line to the arc's start, even there.
    let m = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 0.0, 0.0);
        CGMutablePath::add_arc(Some(m), NOXF, 10.0, 10.0, 5.0, 0.0, 1.0, false);
        CGMutablePath::add_arc(Some(m), NOXF, 10.0, 10.0, 5.0, 1.0, 2.0, false);
    });
    assert!(els(&m).starts_with("M 0.0000,0.0000; L 15.0000,10.0000; C"));
    assert!(els(&m).contains("; L 12.7015,14.2074; C"), "{}", els(&m));
    let rel =
        |d: f64| els(&mutable(|m| unsafe { CGMutablePath::add_relative_arc(Some(m), NOXF, 10.0, 10.0, 5.0, 0.5, d) }));
    assert_eq!(rel(1.0), "M 14.3879,12.3971; C 13.5718,13.8910 12.0517,14.8671 10.3537,14.9875;");
    assert_eq!(rel(-1.0), "M 14.3879,12.3971; C 15.2040,10.9032 15.2040,9.0968 14.3879,7.6029;");
    assert_eq!(rel(7.0).matches('C').count(), 5);
    assert_eq!(rel(0.0), "M 14.3879,12.3971;");
    // Tangent arcs.
    let to = |x1: f64, y1: f64, x2: f64, y2: f64, radius: f64| {
        els(&mutable(|m| unsafe {
            CGMutablePath::move_to_point(Some(m), NOXF, 0.0, 0.0);
            CGMutablePath::add_arc_to_point(Some(m), NOXF, x1, y1, x2, y2, radius);
        }))
    };
    assert_eq!(
        to(10.0, 0.0, 10.0, 10.0, 3.0),
        "M 0.0000,0.0000; L 7.0000,0.0000; C 8.6569,0.0000 10.0000,1.3431 10.0000,3.0000;"
    );
    assert_eq!(
        to(10.0, 0.0, 20.0, 10.0, 5.0),
        "M 0.0000,0.0000; L 7.9289,0.0000; C 9.2550,0.0000 10.5268,0.5268 11.4645,1.4645;"
    );
    assert_eq!(to(10.0, 0.0, 20.0, 0.0, 3.0), "M 0.0000,0.0000; L 10.0000,0.0000;", "collinear: a line");
    assert_eq!(to(10.0, 0.0, 10.0, 0.0, 3.0), "M 0.0000,0.0000; L 10.0000,0.0000;");
    assert_eq!(
        to(10.0, 0.0, 10.0, 10.0, 0.0),
        "M 0.0000,0.0000; L 10.0000,0.0000; C 10.0000,0.0000 10.0000,0.0000 10.0000,0.0000;"
    );
    assert_eq!(to(10.0, 0.0, 0.0, 1.0, 1.0).matches('C').count(), 2, "a sharp turn: two curves");
    let m = mutable(|m| unsafe { CGMutablePath::add_arc_to_point(Some(m), NOXF, 10.0, 0.0, 10.0, 10.0, 3.0) });
    assert_eq!(els(&m), "", "no current point: nothing");
    let neg = els(&mutable(|m| unsafe { CGMutablePath::add_arc(Some(m), NOXF, 0.0, 0.0, -1.0, 0.0, 1.0, false) }));
    assert_eq!(neg, "M -1.0000,0.0000; C -1.0000,-0.3405 -0.8268,-0.6575 -0.5403,-0.8415;");
}

#[test]
fn path_queries() {
    let p = unsafe { CGPath::with_rect(r(1.0, 2.0, 3.0, 4.0), NOXF) };
    let mut rr = CGRect::ZERO;
    assert!(unsafe { CGPath::is_rect(Some(&p), &mut rr) });
    same_rect(rr, r(1.0, 2.0, 3.0, 4.0));
    assert_eq!(CGPath::current_point(Some(&p)), pt(1.0, 2.0));
    assert!(!unsafe { CGPath::is_rect(Some(&CGPath::with_ellipse_in_rect(r(0.0, 0.0, 1.0, 1.0), NOXF)), &mut rr) });
    let corners = [pt(0.0, 0.0), pt(0.0, 2.0), pt(3.0, 2.0), pt(3.0, 0.0)];
    let open = mutable(|m| unsafe { CGMutablePath::add_lines(Some(m), NOXF, corners.as_ptr(), 4) });
    assert!(!unsafe { CGPath::is_rect(Some(&open), &mut rr) }, "open: not a rectangle");
    let closed = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 0.0, 0.0);
        for p in &corners[1..] {
            CGMutablePath::add_line_to_point(Some(m), NOXF, p.x, p.y);
        }
        CGMutablePath::close_subpath(Some(m));
    });
    assert!(unsafe { CGPath::is_rect(Some(&closed), &mut rr) }, "clockwise, closed");
    same_rect(rr, r(0.0, 0.0, 3.0, 2.0));
    // Containment: points on the outline count, edges included.
    let sq = unsafe { CGPath::with_rect(r(0.0, 0.0, 10.0, 10.0), NOXF) };
    let inside = |p: &CGPath, x: f64, y: f64, eo: bool| unsafe { CGPath::contains_point(Some(p), NOXF, pt(x, y), eo) };
    for (x, y) in [(0.0, 0.0), (10.0, 10.0), (5.0, 5.0), (10.0, 5.0), (5.0, 0.0)] {
        assert!(inside(&sq, x, y, false), "({x}, {y})");
    }
    assert!(!inside(&sq, -0.001, 5.0, false) && !inside(&sq, 10.001, 5.0, false));
    let nested = mutable(|m| unsafe {
        CGMutablePath::add_rect(Some(m), NOXF, r(0.0, 0.0, 10.0, 10.0));
        CGMutablePath::add_rect(Some(m), NOXF, r(2.0, 2.0, 6.0, 6.0));
    });
    assert!(inside(&nested, 5.0, 5.0, false) && !inside(&nested, 5.0, 5.0, true));
    let open = mutable(|m| unsafe {
        let pts = [pt(0.0, 0.0), pt(10.0, 0.0), pt(0.0, 10.0)];
        CGMutablePath::add_lines(Some(m), NOXF, pts.as_ptr(), 3);
    });
    assert!(inside(&open, 2.0, 2.0, false), "open subpaths fill as if closed");
    // The transform applies to the point.
    let t = CGAffineTransformMakeTranslation(100.0, 0.0);
    assert!(unsafe { CGPath::contains_point(Some(&sq), &t, pt(-95.0, 5.0), false) });
    assert!(!unsafe { CGPath::contains_point(Some(&sq), &t, pt(105.0, 5.0), false) });
    // Stroking and dashing.
    let line = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 0.0, 0.0);
        CGMutablePath::add_line_to_point(Some(m), NOXF, 10.0, 0.0);
    });
    let s =
        unsafe { CGPath::new_copy_by_stroking_path(Some(&line), NOXF, 2.0, CGLineCap::Butt, CGLineJoin::Miter, 10.0) }
            .unwrap();
    same_rect(CGPath::bounding_box(Some(&s)), r(0.0, -1.0, 10.0, 2.0));
    let s = unsafe {
        CGPath::new_copy_by_stroking_path(Some(&line), NOXF, 2.0, CGLineCap::Square, CGLineJoin::Miter, 10.0)
    }
    .unwrap();
    same_rect(CGPath::path_bounding_box(Some(&s)), r(-1.0, -1.0, 12.0, 2.0));
    let ring =
        unsafe { CGPath::new_copy_by_stroking_path(Some(&sq), NOXF, 2.0, CGLineCap::Butt, CGLineJoin::Miter, 10.0) }
            .unwrap();
    same_rect(CGPath::path_bounding_box(Some(&ring)), r(-1.0, -1.0, 12.0, 12.0));
    assert!(!inside(&ring, 5.0, 5.0, false) && inside(&ring, 0.0, 5.0, false), "a stroked outline is a ring");
    let lens = [2.0, 3.0];
    let d = unsafe { CGPath::new_copy_by_dashing_path(Some(&line), NOXF, 0.0, lens.as_ptr(), 2) }.unwrap();
    assert_eq!(els(&d), "M 0.0000,0.0000; L 2.0000,0.0000; M 5.0000,0.0000; L 7.0000,0.0000;");
    let d = unsafe { CGPath::new_copy_by_dashing_path(Some(&line), NOXF, 1.0, lens.as_ptr(), 2) }.unwrap();
    assert_eq!(
        els(&d),
        "M 0.0000,0.0000; L 1.0000,0.0000; M 4.0000,0.0000; L 6.0000,0.0000; M 9.0000,0.0000; L 10.0000,0.0000;"
    );
    // Flattening: lines only, near the curve.
    let circle = unsafe { CGPath::with_ellipse_in_rect(r(0.0, 0.0, 10.0, 10.0), NOXF) };
    let flat = CGPath::new_copy_by_flattening(Some(&circle), 0.5).unwrap();
    let text = els(&flat);
    assert!(!text.contains('C') && text.starts_with("M 10.0000,5.0000;") && text.ends_with("Z;"), "{text}");
    // Walking a path with a block.
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = count.clone();
    let block = block2::RcBlock::new(move |_el: NonNull<CGPathElement>| seen.set(seen.get() + 1));
    unsafe { sq.apply_with_block(&*block as *const _ as *mut _) };
    assert_eq!(count.get(), 5);
}

// Bitmap contexts.

/// (bits a component, row bytes, space, bitmap info) → (bits a pixel, row
/// bytes), or no context.
type LayoutCase<'a> = (usize, usize, Option<&'a CGColorSpace>, u32, Option<(usize, usize)>);

#[test]
fn bitmap_layouts() {
    let gray = CGColorSpace::new_device_gray().unwrap();
    let rgb = srgb();
    let order32 = CGImageByteOrderInfo::Order32Little.0;
    let float = CGImageComponentInfo::Float.0;
    let cases: [LayoutCase; 22] = [
        (8, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0, Some((32, 32))),
        (8, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedFirst.0, Some((32, 32))),
        (8, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedFirst.0 | order32, Some((32, 32))),
        (8, 0, Some(&rgb), CGImageAlphaInfo::NoneSkipLast.0, Some((32, 32))),
        (8, 0, Some(&rgb), CGImageAlphaInfo::NoneSkipFirst.0 | order32, Some((32, 32))),
        (8, 0, Some(&rgb), CGImageAlphaInfo::None.0, None),
        (8, 0, Some(&rgb), CGImageAlphaInfo::Last.0, None),
        (8, 0, Some(&rgb), CGImageAlphaInfo::First.0, None),
        (8, 0, Some(&gray), CGImageAlphaInfo::None.0, Some((8, 32))),
        (8, 0, Some(&gray), CGImageAlphaInfo::PremultipliedLast.0, Some((16, 32))),
        (8, 0, None, CGImageAlphaInfo::Only.0, Some((8, 32))),
        (8, 0, None, CGImageAlphaInfo::PremultipliedLast.0, None),
        (16, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0, Some((64, 64))),
        (16, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedFirst.0, None),
        (16, 0, Some(&gray), CGImageAlphaInfo::None.0, Some((16, 32))),
        (32, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0 | float, Some((128, 96))),
        (32, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0, None),
        (16, 0, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0 | float, None),
        (8, 24, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0, Some((32, 24))),
        (8, 21, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0, None),
        (8, 3, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0, None),
        (16, 44, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0, None),
    ];
    for (i, (bpc, bpr, space, info, want)) in cases.into_iter().enumerate() {
        let c = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 5, 3, bpc, bpr, space, info) };
        let got = c.as_ref().map(|c| (CGBitmapContextGetBitsPerPixel(Some(c)), CGBitmapContextGetBytesPerRow(Some(c))));
        match (got, want) {
            // How far macOS aligns rows it chooses is its own to change:
            // long enough and a multiple of 16 there, 32 here.
            (Some((bits, row)), Some((want_bits, _))) if bpr == 0 && cfg!(target_vendor = "apple") => {
                assert_eq!(bits, want_bits, "case {i}");
                assert!(row >= 5 * bits / 8 && row.is_multiple_of(16), "case {i}: {row} bytes a row");
            }
            _ => assert_eq!(got, want, "case {i}: {bpc} bits, {bpr} bytes a row, info {info:#x}"),
        }
        if let Some(c) = c {
            assert_eq!(CGBitmapContextGetBitmapInfo(Some(&c)).0, info, "case {i}");
            assert_eq!((CGBitmapContextGetWidth(Some(&c)), CGBitmapContextGetHeight(Some(&c))), (5, 3));
            assert_eq!(CGBitmapContextGetBitsPerComponent(Some(&c)), bpc);
            assert!(!CGBitmapContextGetData(Some(&c)).is_null());
        }
    }
    let bpr = |w, space: &CGColorSpace, info| unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, 1, 8, 0, Some(space), info)
            .map(|c| CGBitmapContextGetBytesPerRow(Some(&c)))
    };
    // Rows of a context's own memory are aligned (see
    // bitmap_contexts_over_program_memory for how far).
    assert!(bpr(9, &rgb, 1).is_some_and(|b| b >= 36 && b.is_multiple_of(16)));
    assert_eq!(unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 0, 3, 8, 0, Some(&rgb), 1) }.map(|_| ()), None);
    // Alpha alone reports no space.
    let only =
        unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 2, 2, 8, 0, Some(&gray), CGImageAlphaInfo::Only.0) }
            .unwrap();
    assert!(CGBitmapContextGetColorSpace(Some(&only)).is_none());
    assert_eq!(CGBitmapContextGetAlphaInfo(Some(&only)), CGImageAlphaInfo::Only);
    let c = ctx(1, 1);
    assert_eq!(space_name(CGBitmapContextGetColorSpace(Some(&c)).as_deref()).as_deref(), Some("kCGColorSpaceSRGB"));
}

/// The bytes a half-transparent orange (1, 0.5, 0, 0.5) leaves in pixel 1
/// of the bottom row of a 5 × 3 context of this layout, and which row they
/// land on.
fn orange_bytes(bpc: usize, space: &CGColorSpace, info: u32) -> (usize, Vec<u8>) {
    let c = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 5, 3, bpc, 0, Some(space), info) }.expect("a context");
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.5, 0.0, 0.5);
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 1.0, 1.0));
    let d = CGBitmapContextGetData(Some(&c)) as *const u8;
    let (bpr, bytes) = (CGBitmapContextGetBytesPerRow(Some(&c)), CGBitmapContextGetBitsPerPixel(Some(&c)) / 8);
    for y in 0..3 {
        // SAFETY: inside the context's memory.
        let row = unsafe { std::slice::from_raw_parts(d.add(y * bpr), bpr) };
        if row.iter().any(|&b| b != 0) {
            return (y, row[bytes..2 * bytes].to_vec());
        }
    }
    (usize::MAX, Vec::new())
}

#[test]
fn bitmap_pixels_in_every_layout() {
    let rgb = srgb();
    let gray = CGColorSpace::new_device_gray().unwrap();
    let order32 = CGImageByteOrderInfo::Order32Little.0;
    let order16 = CGImageByteOrderInfo::Order16Little.0;
    let float = CGImageComponentInfo::Float.0;
    let pl = CGImageAlphaInfo::PremultipliedLast.0;
    let pf = CGImageAlphaInfo::PremultipliedFirst.0;
    // The bottom row of user space is the last row in memory.
    assert_eq!(orange_bytes(8, &rgb, pl), (2, vec![128, 64, 0, 128]));
    assert_eq!(orange_bytes(8, &rgb, pf).1, [128, 128, 64, 0], "alpha first");
    assert_eq!(orange_bytes(8, &rgb, pf | order32).1, [0, 64, 128, 128], "BGRA");
    assert_eq!(orange_bytes(8, &rgb, pl | order32).1, [128, 0, 64, 128], "ABGR");
    assert_eq!(orange_bytes(8, &rgb, pl | CGImageByteOrderInfo::Order32Big.0).1, [128, 64, 0, 128]);
    // The padding sample holds what alpha would.
    assert_eq!(orange_bytes(8, &rgb, CGImageAlphaInfo::NoneSkipLast.0).1, [128, 64, 0, 128]);
    assert_eq!(orange_bytes(8, &rgb, CGImageAlphaInfo::NoneSkipFirst.0 | order32).1, [0, 64, 128, 128]);
    // 16-bit samples are big-endian unless said otherwise. (Sidestep
    // draws in 8 bits a sample, so the low bits needn't match: the values
    // are compared to within 1/255.)
    let words = |b: &[u8], big: bool| -> Vec<f64> {
        b.chunks(2)
            .map(|w| {
                f64::from(if big { u16::from_be_bytes([w[0], w[1]]) } else { u16::from_le_bytes([w[0], w[1]]) })
                    / 65535.0
            })
            .collect()
    };
    let within = |got: &[f64], want: &[f64]| got.iter().zip(want).all(|(a, b)| (a - b).abs() <= 1.0 / 255.0);
    let be = words(&orange_bytes(16, &rgb, pl).1, true);
    assert!(within(&be, &[0.5, 0.25, 0.0, 0.5]), "{be:?}");
    let le = words(&orange_bytes(16, &rgb, pl | order16).1, false);
    assert!(within(&le, &[0.5, 0.25, 0.0, 0.5]), "{le:?}");
    // So are floats.
    let floats = |b: &[u8], big: bool| -> Vec<f64> {
        b.chunks(4)
            .map(|w| {
                f64::from(if big {
                    f32::from_be_bytes([w[0], w[1], w[2], w[3]])
                } else {
                    f32::from_le_bytes([w[0], w[1], w[2], w[3]])
                })
            })
            .collect()
    };
    let be = floats(&orange_bytes(32, &rgb, pl | float).1, true);
    assert!(within(&be, &[0.5, 0.25, 0.0, 0.5]), "{be:?}");
    let le = floats(&orange_bytes(32, &rgb, pl | float | order32).1, false);
    assert!(within(&le, &[0.5, 0.25, 0.0, 0.5]), "{le:?}");
    // Gray is the color's gray (color management aside: within 3).
    let (row, g) = orange_bytes(8, &gray, CGImageAlphaInfo::None.0);
    assert_eq!(row, 2);
    assert!(g[0].abs_diff(82) <= 3, "{g:?}");
    let g = orange_bytes(8, &gray, pl).1;
    assert!(g[0].abs_diff(82) <= 3 && g[1] == 128, "{g:?}");
    assert_eq!(orange_bytes(8, &gray, CGImageAlphaInfo::Only.0).1, [128]);
}

#[test]
fn context_state() {
    let c = ctx(10, 8);
    let id = CGAffineTransform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };
    assert_eq!(CGContext::ctm(Some(&c)), id);
    let u2d = CGContext::user_space_to_device_space_transform(Some(&c));
    assert_eq!((u2d.a, u2d.d, u2d.tx, u2d.ty), (1.0, -1.0, 0.0, 8.0), "device space runs down from the top");
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(0.0, 0.0, 10.0, 8.0));
    assert_eq!(CGContext::interpolation_quality(Some(&c)), CGInterpolationQuality::Default);
    assert_eq!(CGContext::text_matrix(Some(&c)), id);
    assert_eq!(CGContext::text_position(Some(&c)), pt(0.0, 0.0));
    CGContext::translate_ctm(Some(&c), 1.0, 2.0);
    CGContext::scale_ctm(Some(&c), 2.0, 3.0);
    let m = CGContext::ctm(Some(&c));
    assert_eq!((m.a, m.d, m.tx, m.ty), (2.0, 3.0, 1.0, 2.0));
    CGContext::rotate_ctm(Some(&c), 0.5);
    let m = CGContext::ctm(Some(&c));
    close(m.a, 2.0 * 0.5f64.cos());
    close(m.b, 3.0 * 0.5f64.sin());
    let b = CGContext::clip_bounding_box(Some(&c));
    close(b.origin.x, -0.7584083066813215);
    close(b.size.height, 4.737347858062008);
    let dev = CGContext::convert_point_to_device_space(Some(&c), pt(1.0, 1.0));
    close(dev.x, 1.7963140465723395);
    close(dev.y, 1.9289756985162727);
    let user = CGContext::convert_point_to_user_space(Some(&c), pt(1.0, 1.0));
    close(user.x, 0.7990425643403383);
    let size = CGContext::convert_size_to_device_space(Some(&c), CGSize::new(1.0, 1.0));
    close(size.height, -4.071024301483727);
    // Text settings.
    CGContext::set_text_position(Some(&c), 3.0, 4.0);
    assert_eq!(CGContext::text_position(Some(&c)), pt(3.0, 4.0));
    assert_eq!(CGContext::text_matrix(Some(&c)).tx, 3.0);
    // Restores beyond the saves are ignored.
    let c = ctx(4, 4);
    CGContext::save_g_state(Some(&c));
    CGContext::scale_ctm(Some(&c), 2.0, 2.0);
    CGContext::restore_g_state(Some(&c));
    CGContext::restore_g_state(Some(&c));
    assert_eq!(CGContext::ctm(Some(&c)), id);
    CGContext::set_interpolation_quality(Some(&c), CGInterpolationQuality::High);
    assert_eq!(CGContext::interpolation_quality(Some(&c)), CGInterpolationQuality::High);
    CGContext::clip_to_rect(Some(&c), r(0.5, 0.5, 2.0, 2.0));
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(0.5, 0.5, 2.0, 2.0));
    CGContext::clip_to_rect(Some(&c), r(10.0, 10.0, 1.0, 1.0));
    assert!(CGRectIsNull(CGContext::clip_bounding_box(Some(&c))), "nothing left: null");
    CGContext::reset_clip(&c);
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(0.0, 0.0, 4.0, 4.0));
    let rects = [r(0.0, 0.0, 1.0, 1.0), r(2.0, 2.0, 1.0, 1.0)];
    unsafe { CGContext::clip_to_rects(Some(&c), NonNull::new(rects.as_ptr().cast_mut()).unwrap(), 2) };
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(0.0, 0.0, 3.0, 3.0));
}

#[test]
fn the_current_path() {
    // Points are fixed in device space when they're added.
    let c = ctx(10, 8);
    CGContext::save_g_state(Some(&c));
    CGContext::translate_ctm(Some(&c), 5.0, 5.0);
    CGContext::move_to_point(Some(&c), 1.0, 1.0);
    CGContext::restore_g_state(Some(&c));
    assert!(!CGContext::is_path_empty(Some(&c)), "the path isn't part of the graphics state");
    assert_eq!(CGContext::path_current_point(Some(&c)), pt(6.0, 6.0));
    CGContext::scale_ctm(Some(&c), 2.0, 2.0);
    assert_eq!(CGContext::path_current_point(Some(&c)), pt(3.0, 3.0), "in user space now");
    same_rect(CGContext::path_bounding_box(Some(&c)), r(3.0, 3.0, 0.0, 0.0));
    CGContext::add_line_to_point(Some(&c), 3.0, 1.0);
    let p = CGContext::path(Some(&c)).unwrap();
    assert_eq!(els(&p), "M 3.0000,3.0000; L 3.0000,1.0000;");
    CGContext::begin_path(Some(&c));
    assert!(CGContext::is_path_empty(Some(&c)) && CGContext::path(Some(&c)).is_none());
    assert!(CGRectIsNull(CGContext::path_bounding_box(Some(&c))));
    assert_eq!(CGContext::path_current_point(Some(&c)), pt(0.0, 0.0));
    // Drawing and clipping take the path.
    let c = ctx(4, 4);
    CGContext::add_rect(Some(&c), r(0.0, 0.0, 2.0, 2.0));
    CGContext::fill_path(Some(&c));
    assert!(CGContext::is_path_empty(Some(&c)));
    CGContext::add_rect(Some(&c), r(0.0, 0.0, 2.0, 2.0));
    CGContext::clip(Some(&c));
    assert!(CGContext::is_path_empty(Some(&c)));
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(0.0, 0.0, 2.0, 2.0));
    // Shapes, as CGPath builds them.
    let c = ctx(4, 4);
    CGContext::add_ellipse_in_rect(Some(&c), r(0.0, 0.0, 4.0, 4.0));
    assert!(
        els(&CGContext::path(Some(&c)).unwrap())
            .starts_with("M 4.0000,2.0000; C 4.0000,3.1046 3.1046,4.0000 2.0000,4.0000;")
    );
    assert!(CGContext::path_contains_point(Some(&c), pt(2.0, 2.0), CGPathDrawingMode::Fill));
    assert!(!CGContext::path_contains_point(Some(&c), pt(2.0, 2.0), CGPathDrawingMode::Stroke));
    assert!(CGContext::path_contains_point(Some(&c), pt(4.0, 2.0), CGPathDrawingMode::Stroke));
    let c = ctx(4, 4);
    CGContext::add_arc(Some(&c), 2.0, 2.0, 1.0, 0.0, 1.0, 1);
    assert_eq!(els(&CGContext::path(Some(&c)).unwrap()).matches('C').count(), 4, "clockwise from 0 to 1");
    let c = ctx(4, 4);
    CGContext::move_to_point(Some(&c), 1.0, 1.0);
    CGContext::add_line_to_point(Some(&c), 3.0, 1.0);
    CGContext::close_path(Some(&c));
    CGContext::add_line_to_point(Some(&c), 3.0, 3.0);
    assert_eq!(els(&CGContext::path(Some(&c)).unwrap()), "M 1.0000,1.0000; L 3.0000,1.0000; Z; L 3.0000,3.0000;");
    // A stroked path in its place: its outline.
    let c = ctx(4, 4);
    CGContext::set_line_width(Some(&c), 2.0);
    CGContext::move_to_point(Some(&c), 0.0, 1.0);
    CGContext::add_line_to_point(Some(&c), 4.0, 1.0);
    CGContext::replace_path_with_stroked_path(Some(&c));
    same_rect(CGContext::path_bounding_box(Some(&c)), r(0.0, 0.0, 4.0, 2.0));
    // An added path keeps the transform it was added under.
    let c = ctx(8, 8);
    let sq = unsafe { CGPath::with_rect(r(0.0, 0.0, 1.0, 1.0), NOXF) };
    CGContext::scale_ctm(Some(&c), 2.0, 2.0);
    CGContext::add_path(Some(&c), Some(&sq));
    CGContext::scale_ctm(Some(&c), 2.0, 2.0);
    same_rect(CGContext::path_bounding_box(Some(&c)), r(0.0, 0.0, 0.5, 0.5));
    CGContext::fill_path(Some(&c));
    assert_px(&c, 1, 7, [0, 0, 0, 255], 0);
    assert_px(&c, 2, 7, [0, 0, 0, 0], 0);
}

#[test]
fn fills_and_strokes() {
    let c = ctx(4, 3);
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    assert_px(&c, 0, 2, [255, 0, 0, 255], 0);
    assert_px(&c, 0, 0, [0; 4], 0);
    // Fractional edges cover pixels partly.
    let c = ctx(4, 3);
    CGContext::fill_rect(Some(&c), r(0.5, 0.5, 1.0, 1.0));
    for (x, y) in [(0, 1), (1, 1), (0, 2), (1, 2)] {
        assert!(alpha(&c, x, y).abs_diff(64) <= 3, "({x}, {y}): {}", alpha(&c, x, y));
    }
    // A line one wide on a whole coordinate straddles two rows.
    let c = ctx(4, 3);
    CGContext::move_to_point(Some(&c), 0.0, 1.0);
    CGContext::add_line_to_point(Some(&c), 4.0, 1.0);
    CGContext::stroke_path(Some(&c));
    assert!(alpha(&c, 1, 1).abs_diff(128) <= 3 && alpha(&c, 1, 2).abs_diff(128) <= 3);
    let c = ctx(4, 3);
    CGContext::move_to_point(Some(&c), 0.0, 1.5);
    CGContext::add_line_to_point(Some(&c), 4.0, 1.5);
    CGContext::stroke_path(Some(&c));
    assert!(alpha(&c, 1, 1) >= 252 && alpha(&c, 1, 0) == 0 && alpha(&c, 1, 2) == 0);
    // No width, no line.
    let c = ctx(4, 3);
    CGContext::set_line_width(Some(&c), 0.0);
    CGContext::move_to_point(Some(&c), 0.0, 1.5);
    CGContext::add_line_to_point(Some(&c), 4.0, 1.5);
    CGContext::stroke_path(Some(&c));
    assert!((0..4).all(|x| (0..3).all(|y| alpha(&c, x, y) == 0)));
    // The CTM at stroking time sets the width.
    let c = ctx(8, 4);
    CGContext::move_to_point(Some(&c), 0.0, 1.0);
    CGContext::add_line_to_point(Some(&c), 4.0, 1.0);
    CGContext::scale_ctm(Some(&c), 1.0, 2.0);
    CGContext::stroke_path(Some(&c));
    assert!(alpha(&c, 1, 2) >= 252 && alpha(&c, 1, 3) >= 252 && alpha(&c, 1, 1) == 0 && alpha(&c, 5, 2) == 0);
    // Dashes, and odd patterns repeated.
    let c = ctx(12, 3);
    unsafe { CGContext::set_line_dash(Some(&c), 0.0, [2.0, 1.0].as_ptr(), 2) };
    CGContext::move_to_point(Some(&c), 0.0, 1.5);
    CGContext::add_line_to_point(Some(&c), 12.0, 1.5);
    CGContext::stroke_path(Some(&c));
    let row: Vec<bool> = (0..12).map(|x| alpha(&c, x, 1) > 128).collect();
    assert_eq!(row, [true, true, false, true, true, false, true, true, false, true, true, false]);
    let c = ctx(12, 3);
    unsafe { CGContext::set_line_dash(Some(&c), 1.0, [2.0].as_ptr(), 1) };
    CGContext::move_to_point(Some(&c), 0.0, 1.5);
    CGContext::add_line_to_point(Some(&c), 12.0, 1.5);
    CGContext::stroke_path(Some(&c));
    let row: Vec<bool> = (0..12).map(|x| alpha(&c, x, 1) > 128).collect();
    assert_eq!(row, [true, false, false, true, true, false, false, true, true, false, false, true]);
    // Square caps reach half the width further.
    let c = ctx(8, 3);
    CGContext::set_line_cap(Some(&c), CGLineCap::Square);
    CGContext::move_to_point(Some(&c), 2.0, 1.5);
    CGContext::add_line_to_point(Some(&c), 5.0, 1.5);
    CGContext::stroke_path(Some(&c));
    assert!(alpha(&c, 1, 1).abs_diff(128) <= 3 && alpha(&c, 5, 1).abs_diff(128) <= 3 && alpha(&c, 3, 1) >= 252);
    // Even-odd, and fill then stroke.
    let c = ctx(6, 6);
    CGContext::add_rect(Some(&c), r(0.0, 0.0, 6.0, 6.0));
    CGContext::add_rect(Some(&c), r(2.0, 2.0, 2.0, 2.0));
    CGContext::eo_fill_path(Some(&c));
    assert!(alpha(&c, 3, 3) == 0 && alpha(&c, 0, 0) == 255);
    let c = ctx(6, 6);
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::set_rgb_stroke_color(Some(&c), 0.0, 0.0, 1.0, 1.0);
    CGContext::set_line_width(Some(&c), 2.0);
    CGContext::add_rect(Some(&c), r(1.0, 1.0, 4.0, 4.0));
    CGContext::draw_path(Some(&c), CGPathDrawingMode::FillStroke);
    assert_px(&c, 2, 2, [255, 0, 0, 255], 0);
    assert_px(&c, 1, 1, [0, 0, 255, 255], 0);
    assert_px(&c, 0, 0, [0, 0, 255, 255], 0);
    // Stroked rectangles, ellipses and segments.
    let c = ctx(6, 6);
    CGContext::stroke_rect_with_width(Some(&c), r(1.0, 1.0, 4.0, 4.0), 2.0);
    assert!(alpha(&c, 0, 0) == 255 && alpha(&c, 2, 2) == 0 && alpha(&c, 1, 3) == 255);
    let c = ctx(8, 8);
    CGContext::fill_ellipse_in_rect(Some(&c), r(0.0, 0.0, 8.0, 8.0));
    assert!(alpha(&c, 0, 0) == 0 && alpha(&c, 4, 4) == 255 && alpha(&c, 3, 0) >= 200, "{}", alpha(&c, 3, 0));
    let c = ctx(6, 3);
    let pts = [pt(0.0, 1.5), pt(2.0, 1.5), pt(4.0, 1.5), pt(6.0, 1.5)];
    unsafe { CGContext::stroke_line_segments(Some(&c), pts.as_ptr(), 4) };
    let row: Vec<bool> = (0..6).map(|x| alpha(&c, x, 1) > 128).collect();
    assert_eq!(row, [true, true, false, false, true, true]);
    // Transforms move and scale.
    let c = ctx(6, 6);
    CGContext::translate_ctm(Some(&c), 1.0, 2.0);
    CGContext::scale_ctm(Some(&c), 2.0, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    assert!(alpha(&c, 1, 3) == 255 && alpha(&c, 2, 3) == 255 && alpha(&c, 3, 3) == 0 && alpha(&c, 1, 2) == 0);
    // Antialiasing off: all or nothing.
    let c = ctx(4, 4);
    CGContext::set_should_antialias(Some(&c), false);
    CGContext::fill_rect(Some(&c), r(0.5, 0.5, 1.0, 1.0));
    CGContext::fill_ellipse_in_rect(Some(&c), r(1.2, 1.3, 2.5, 2.1));
    assert!((0..4).all(|x| (0..4).all(|y| matches!(alpha(&c, x, y), 0 | 255))));
    let c = ctx(4, 4);
    CGContext::save_g_state(Some(&c));
    CGContext::set_should_antialias(Some(&c), false);
    CGContext::restore_g_state(Some(&c));
    CGContext::fill_rect(Some(&c), r(0.5, 0.5, 1.0, 1.0));
    assert!(alpha(&c, 0, 2).abs_diff(64) <= 3, "antialiasing is part of the state");
    let c = ctx(4, 4);
    CGContext::set_allows_antialiasing(Some(&c), false);
    CGContext::fill_rect(Some(&c), r(0.5, 0.5, 1.0, 1.0));
    assert!((0..4).all(|x| (0..4).all(|y| matches!(alpha(&c, x, y), 0 | 255))), "not allowed, not done");
}

#[test]
fn colors_alpha_and_compositing() {
    let c = ctx(2, 1);
    CGContext::set_alpha(Some(&c), 0.5);
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 0.0, 1.0, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 0.0, 1.0, 0.5);
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 1.0, 1.0));
    assert_px(&c, 0, 0, [0, 0, 128, 128], 1);
    assert_px(&c, 1, 0, [0, 0, 64, 64], 1);
    // Clearing ignores the alpha and the blend mode.
    let c = ctx(3, 1);
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 1.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 3.0, 1.0));
    CGContext::set_alpha(Some(&c), 0.5);
    CGContext::set_blend_mode(Some(&c), CGBlendMode::Multiply);
    CGContext::clear_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    assert_px(&c, 0, 0, [0; 4], 0);
    assert_px(&c, 1, 0, [0, 255, 0, 255], 0);
    // Copy replaces.
    let c = ctx(2, 1);
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 1.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 2.0, 1.0));
    CGContext::set_blend_mode(Some(&c), CGBlendMode::Copy);
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 0.5);
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 1.0, 1.0));
    assert_px(&c, 1, 0, [128, 0, 0, 128], 1);
    // Color spaces and components.
    let c = ctx(5, 1);
    CGContext::set_gray_fill_color(Some(&c), 0.5, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    CGContext::set_fill_color_space(Some(&c), Some(&srgb()));
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 1.0, 1.0));
    unsafe { CGContext::set_fill_color(Some(&c), [0.0, 1.0, 0.0, 1.0].as_ptr()) };
    CGContext::fill_rect(Some(&c), r(2.0, 0.0, 1.0, 1.0));
    CGContext::set_fill_color_space(Some(&c), CGColorSpace::new_device_gray().as_deref());
    CGContext::fill_rect(Some(&c), r(3.0, 0.0, 1.0, 1.0));
    CGContext::set_fill_color_with_color(Some(&c), Some(&CGColor::new_srgb(1.0, 0.0, 1.0, 1.0)));
    CGContext::fill_rect(Some(&c), r(4.0, 0.0, 1.0, 1.0));
    assert_px(&c, 0, 0, [128, 128, 128, 255], 1);
    assert_px(&c, 1, 0, [0, 0, 0, 255], 0);
    assert_px(&c, 2, 0, [0, 255, 0, 255], 0);
    assert_px(&c, 3, 0, [0, 0, 0, 255], 0);
    assert_px(&c, 4, 0, [255, 0, 255, 255], 0);
    // Clips.
    let c = ctx(4, 4);
    CGContext::clip_to_rect(Some(&c), r(1.0, 1.0, 2.0, 1.0));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 4.0, 4.0));
    assert!(alpha(&c, 1, 2) == 255 && alpha(&c, 2, 2) == 255 && alpha(&c, 0, 2) == 0 && alpha(&c, 1, 1) == 0);
    let c = ctx(4, 4);
    CGContext::move_to_point(Some(&c), 0.0, 0.0);
    CGContext::add_line_to_point(Some(&c), 4.0, 0.0);
    CGContext::add_line_to_point(Some(&c), 0.0, 4.0);
    CGContext::clip(Some(&c));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 4.0, 4.0));
    assert!(alpha(&c, 0, 3) == 255 && alpha(&c, 3, 0) == 0 && alpha(&c, 1, 2) == 255);
    // Antialiased edges: rasterizers estimate coverage differently
    // (tiny-skia samples it), so only "partly".
    assert!((64..=192).contains(&alpha(&c, 1, 1)), "the diagonal partly covered: {}", alpha(&c, 1, 1));
}

fn two_stops() -> CFRetained<CGGradient> {
    let comps = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0];
    unsafe { CGGradient::with_color_components(Some(&srgb()), comps.as_ptr(), [0.0, 1.0].as_ptr(), 2) }.unwrap()
}

/// Whether a pixel is red and blue mixed with `t` of blue (gradients
/// dither: within 12).
#[track_caller]
fn mix(c: &CGContext, x: usize, y: usize, t: f64) {
    let want = [((1.0 - t) * 255.0).round() as u8, 0, (t * 255.0).round() as u8, 255];
    assert_px(c, x, y, want, 12);
}

#[test]
fn gradients() {
    let g = two_stops();
    let c = ctx(8, 1);
    CGContext::draw_linear_gradient(Some(&c), Some(&g), pt(2.0, 0.0), pt(6.0, 0.0), CGGradientDrawingOptions::empty());
    assert_px(&c, 1, 0, [0; 4], 0);
    assert_px(&c, 6, 0, [0; 4], 0);
    for (x, t) in [(2, 0.125), (3, 0.375), (4, 0.625), (5, 0.875)] {
        mix(&c, x, 0, t);
    }
    let c = ctx(8, 1);
    let both = CGGradientDrawingOptions::DrawsBeforeStartLocation | CGGradientDrawingOptions::DrawsAfterEndLocation;
    CGContext::draw_linear_gradient(Some(&c), Some(&g), pt(2.0, 0.0), pt(6.0, 0.0), both);
    assert_px(&c, 0, 0, [255, 0, 0, 255], 0);
    assert_px(&c, 7, 0, [0, 0, 255, 255], 0);
    // Three stops at no given locations: spread evenly.
    let comps = [1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0];
    let g3 = unsafe { CGGradient::with_color_components(Some(&srgb()), comps.as_ptr(), std::ptr::null(), 3) }.unwrap();
    let c = ctx(9, 1);
    CGContext::draw_linear_gradient(Some(&c), Some(&g3), pt(0.0, 0.0), pt(9.0, 0.0), CGGradientDrawingOptions::empty());
    // Pixel centers at 0.5 and 4.5 of 9: red a ninth of the way to green,
    // and green. macOS dithers a gradient this short by up to 18 levels
    // (it draws [237, 19, 0] and [18, 237, 0] here), so within 24.
    assert_px(&c, 4, 0, [0, 255, 0, 255], 24);
    assert_px(&c, 0, 0, [227, 28, 0, 255], 24);
    // Within the clip only.
    let c = ctx(8, 2);
    CGContext::clip_to_rect(Some(&c), r(0.0, 0.0, 8.0, 1.0));
    CGContext::draw_linear_gradient(Some(&c), Some(&g), pt(0.0, 0.0), pt(8.0, 0.0), CGGradientDrawingOptions::empty());
    assert!(alpha(&c, 3, 0) == 0 && alpha(&c, 3, 1) == 255);
    // One stop: that color throughout its band.
    let g1 =
        unsafe { CGGradient::with_color_components(Some(&srgb()), [1.0, 0.0, 0.0, 1.0].as_ptr(), [0.5].as_ptr(), 1) }
            .unwrap();
    let c = ctx(4, 1);
    CGContext::draw_linear_gradient(Some(&c), Some(&g1), pt(0.0, 0.0), pt(4.0, 0.0), CGGradientDrawingOptions::empty());
    assert!((0..4).all(|x| px(&c, x, 0) == [255, 0, 0, 255]));
    // From colors, in the gradient's space.
    let colors = objc2_core_foundation::CFArray::from_retained_objects(&[
        CGColor::new_srgb(1.0, 0.0, 0.0, 1.0),
        CGColor::new_srgb(0.0, 0.0, 0.0, 1.0),
    ]);
    let gc = unsafe { CGGradient::with_colors(Some(&srgb()), Some(colors.as_opaque()), std::ptr::null()) }.unwrap();
    let c = ctx(4, 1);
    CGContext::draw_linear_gradient(Some(&c), Some(&gc), pt(0.0, 0.0), pt(4.0, 0.0), CGGradientDrawingOptions::empty());
    assert_px(&c, 0, 0, [223, 0, 0, 255], 12);
    assert_px(&c, 3, 0, [32, 0, 0, 255], 12);
    // Radial: the disk it runs through, nothing outside.
    let c = ctx(9, 9);
    CGContext::draw_radial_gradient(
        Some(&c),
        Some(&g),
        pt(4.5, 4.5),
        0.0,
        pt(4.5, 4.5),
        4.0,
        CGGradientDrawingOptions::empty(),
    );
    assert_px(&c, 4, 4, [255, 0, 0, 255], 12);
    assert_eq!(alpha(&c, 0, 0), 0);
    assert_eq!(alpha(&c, 8, 8), 0);
    mix(&c, 6, 4, 0.5);
    let c = ctx(9, 1);
    CGContext::draw_radial_gradient(Some(&c), Some(&g), pt(4.5, 0.5), 1.0, pt(4.5, 0.5), 3.0, both);
    assert_px(&c, 4, 0, [255, 0, 0, 255], 0);
    assert_px(&c, 0, 0, [0, 0, 255, 255], 0);
    mix(&c, 2, 0, 0.5);
    let c = ctx(9, 1);
    CGContext::draw_radial_gradient(
        Some(&c),
        Some(&g),
        pt(4.5, 0.5),
        1.0,
        pt(4.5, 0.5),
        3.0,
        CGGradientDrawingOptions::empty(),
    );
    assert_eq!(alpha(&c, 4, 0), 0, "inside the starting circle: before the start");
    assert_eq!(alpha(&c, 0, 0), 0);
    mix(&c, 2, 0, 0.5);
}

#[test]
fn shadows() {
    let black = CGColor::new_generic_gray(0.0, 1.0);
    // The offset's height goes up.
    let c = ctx(8, 8);
    CGContext::set_shadow_with_color(Some(&c), CGSize::new(2.0, 2.0), 0.0, Some(&black));
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(1.0, 1.0, 3.0, 3.0));
    assert_px(&c, 1, 5, [255, 0, 0, 255], 0);
    assert_px(&c, 5, 2, [0, 0, 0, 255], 0);
    assert_px(&c, 1, 2, [0; 4], 0);
    // The CTM doesn't scale it.
    let c = ctx(12, 12);
    CGContext::scale_ctm(Some(&c), 2.0, 2.0);
    CGContext::set_shadow_with_color(Some(&c), CGSize::new(1.0, 1.0), 0.0, Some(&black));
    CGContext::fill_rect(Some(&c), r(1.0, 1.0, 2.0, 2.0));
    assert!(alpha(&c, 6, 5) == 255 && alpha(&c, 7, 5) == 0 && alpha(&c, 2, 5) == 0);
    // The default: black at a third.
    let c = ctx(8, 8);
    CGContext::set_shadow(Some(&c), CGSize::new(2.0, -2.0), 0.0);
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(2.0, 3.0, 3.0, 3.0));
    assert_px(&c, 5, 5, [0, 0, 0, 85], 1);
    // Blurred about as far as its radius.
    let c = ctx(12, 12);
    CGContext::set_shadow_with_color(Some(&c), CGSize::new(0.0, 0.0), 2.0, Some(&black));
    CGContext::fill_rect(Some(&c), r(4.0, 4.0, 4.0, 4.0));
    assert!(alpha(&c, 3, 5) > 20 && alpha(&c, 3, 5) < 200, "{}", alpha(&c, 3, 5));
    assert_eq!(alpha(&c, 0, 5), 0);
    // A transparency layer casts one shadow, faded as one.
    let c = ctx(4, 1);
    CGContext::set_alpha(Some(&c), 0.5);
    unsafe { CGContext::begin_transparency_layer(Some(&c), None) };
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 3.0, 1.0));
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 3.0, 1.0));
    CGContext::end_transparency_layer(Some(&c));
    assert!((0..4).all(|x| near(px(&c, x, 0), [128, 0, 0, 128], 1)), "{:?}", px(&c, 1, 0));
    let c = ctx(8, 8);
    CGContext::set_shadow_with_color(Some(&c), CGSize::new(2.0, -2.0), 0.0, Some(&CGColor::new_generic_gray(0.0, 0.5)));
    unsafe { CGContext::begin_transparency_layer(Some(&c), None) };
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(1.0, 4.0, 2.0, 2.0));
    CGContext::fill_rect(Some(&c), r(2.0, 3.0, 2.0, 2.0));
    CGContext::end_transparency_layer(Some(&c));
    assert_px(&c, 5, 6, [0, 0, 0, 128], 1);
    assert_px(&c, 1, 3, [255, 0, 0, 255], 0);
}

/// A 3 × 2 sRGB image: red, green, blue over white, black, clear.
fn test_image() -> CFRetained<CGImage> {
    let data: Vec<u8> =
        vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 0];
    let provider = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&data))).unwrap();
    unsafe {
        CGImage::new(
            3,
            2,
            8,
            32,
            12,
            Some(&srgb()),
            CGBitmapInfo(CGImageAlphaInfo::PremultipliedLast.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap()
}

#[test]
fn images() {
    let img = test_image();
    assert_eq!(
        (CGImage::width(Some(&img)), CGImage::height(Some(&img)), CGImage::bytes_per_row(Some(&img))),
        (3, 2, 12)
    );
    assert_eq!(CGImage::alpha_info(Some(&img)), CGImageAlphaInfo::PremultipliedLast);
    assert_eq!(CGImage::bitmap_info(Some(&img)).0, 1);
    assert!(!CGImage::is_mask(Some(&img)));
    // The image's first row at the top of the rectangle.
    let c = ctx(3, 2);
    CGContext::draw_image(Some(&c), r(0.0, 0.0, 3.0, 2.0), Some(&img));
    assert_px(&c, 0, 0, [255, 0, 0, 255], 0);
    assert_px(&c, 2, 0, [0, 0, 255, 255], 0);
    assert_px(&c, 0, 1, [255, 255, 255, 255], 0);
    assert_px(&c, 2, 1, [0; 4], 0);
    // Upside down under a flipped CTM.
    let c = ctx(3, 2);
    CGContext::translate_ctm(Some(&c), 0.0, 2.0);
    CGContext::scale_ctm(Some(&c), 1.0, -1.0);
    CGContext::draw_image(Some(&c), r(0.0, 0.0, 3.0, 2.0), Some(&img));
    assert_px(&c, 0, 1, [255, 0, 0, 255], 0);
    // Scaled with no interpolation: whole pixels.
    let c = ctx(6, 4);
    CGContext::set_interpolation_quality(Some(&c), CGInterpolationQuality::None);
    CGContext::draw_image(Some(&c), r(0.0, 0.0, 6.0, 4.0), Some(&img));
    assert_px(&c, 1, 1, [255, 0, 0, 255], 0);
    assert_px(&c, 2, 1, [0, 255, 0, 255], 0);
    assert_px(&c, 3, 2, [0, 0, 0, 255], 0);
    // Parts: the rectangle made whole, within the image.
    let sub = CGImage::with_image_in_rect(Some(&img), r(1.0, 0.0, 2.0, 1.0)).unwrap();
    assert_eq!((CGImage::width(Some(&sub)), CGImage::height(Some(&sub))), (2, 1));
    let c = ctx(2, 1);
    CGContext::draw_image(Some(&c), r(0.0, 0.0, 2.0, 1.0), Some(&sub));
    assert_px(&c, 0, 0, [0, 255, 0, 255], 0);
    assert_px(&c, 1, 0, [0, 0, 255, 255], 0);
    let frac = CGImage::with_image_in_rect(Some(&img), r(0.5, 0.5, 1.2, 0.2)).unwrap();
    assert_eq!((CGImage::width(Some(&frac)), CGImage::height(Some(&frac))), (2, 1));
    let past = CGImage::with_image_in_rect(Some(&img), r(2.0, 1.0, 5.0, 5.0)).unwrap();
    assert_eq!((CGImage::width(Some(&past)), CGImage::height(Some(&past))), (1, 1));
    assert!(CGImage::with_image_in_rect(Some(&img), r(5.0, 5.0, 1.0, 1.0)).is_none());
    // Tiled from the rectangle's origin over the clip.
    let rb: Vec<u8> = vec![255, 0, 0, 255, 0, 0, 255, 255];
    let p = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&rb))).unwrap();
    let tile = unsafe {
        CGImage::new(
            2,
            1,
            8,
            32,
            8,
            Some(&srgb()),
            CGBitmapInfo(1),
            Some(&p),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap();
    let c = ctx(5, 2);
    CGContext::set_interpolation_quality(Some(&c), CGInterpolationQuality::None);
    CGContext::draw_tiled_image(Some(&c), r(0.0, 0.0, 2.0, 1.0), Some(&tile));
    for (x, want) in [(0, [255, 0, 0, 255]), (1, [0, 0, 255, 255]), (2, [255, 0, 0, 255]), (4, [255, 0, 0, 255])] {
        assert_px(&c, x, 0, want, 0);
        assert_px(&c, x, 1, want, 0);
    }
    // A copy is another image.
    assert!(!std::ptr::eq(&*CGImage::new_copy(Some(&img)).unwrap(), &*img));
}

fn image_of(
    w: usize,
    bpc: usize,
    bpp: usize,
    bpr: usize,
    space: Option<&CGColorSpace>,
    info: u32,
    data: &[u8],
) -> Option<CFRetained<CGImage>> {
    let p = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(data))).unwrap();
    unsafe {
        CGImage::new(
            w,
            1,
            bpc,
            bpp,
            bpr,
            space,
            CGBitmapInfo(info),
            Some(&p),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
}

fn drawn(img: &CGImage) -> Vec<[u8; 4]> {
    let w = CGImage::width(Some(img));
    let c = ctx(w, 1);
    CGContext::draw_image(Some(&c), r(0.0, 0.0, w as f64, 1.0), Some(img));
    (0..w).map(|x| px(&c, x, 0)).collect()
}

/// (bits a component, bits a pixel, row bytes, space, bitmap info, the
/// bytes) → the pixels drawn.
type ImageCase<'a> = (usize, usize, usize, &'a CGColorSpace, u32, Vec<u8>, Vec<[u8; 4]>);

#[test]
fn image_layouts() {
    let rgb = srgb();
    let gray = CGColorSpace::new_device_gray().unwrap();
    let order32 = CGImageByteOrderInfo::Order32Little.0;
    let order16 = CGImageByteOrderInfo::Order16Little.0;
    let float = CGImageComponentInfo::Float.0;
    let cases: [ImageCase; 11] = [
        (8, 16, 2, &gray, CGImageAlphaInfo::PremultipliedLast.0, vec![128, 128], vec![[128, 128, 128, 128]]),
        (8, 16, 2, &gray, CGImageAlphaInfo::Last.0, vec![128, 128], vec![[64, 64, 64, 128]]),
        (8, 24, 3, &rgb, CGImageAlphaInfo::None.0, vec![1, 2, 3], vec![[1, 2, 3, 255]]),
        (8, 32, 4, &rgb, CGImageAlphaInfo::Last.0, vec![255, 0, 0, 128], vec![[128, 0, 0, 128]]),
        (8, 32, 4, &rgb, CGImageAlphaInfo::First.0, vec![128, 255, 0, 0], vec![[128, 0, 0, 128]]),
        (
            8,
            32,
            4,
            &rgb,
            CGImageAlphaInfo::PremultipliedFirst.0 | order32,
            vec![255, 0, 0, 255],
            vec![[0, 0, 255, 255]],
        ),
        (8, 32, 4, &rgb, CGImageAlphaInfo::NoneSkipFirst.0, vec![0, 10, 20, 30], vec![[10, 20, 30, 255]]),
        (16, 48, 6, &rgb, CGImageAlphaInfo::None.0, vec![0xff, 0xff, 0x80, 0x00, 0, 0], vec![[255, 128, 0, 255]]),
        (
            16,
            64,
            8,
            &rgb,
            CGImageAlphaInfo::Last.0 | order16,
            vec![0xff, 0xff, 0x00, 0x80, 0, 0, 0xff, 0xff],
            vec![[255, 128, 0, 255]],
        ),
        (
            32,
            128,
            16,
            &rgb,
            CGImageAlphaInfo::Last.0 | float | order32,
            [1.0f32, 0.5, 0.0, 1.0].iter().flat_map(|f| f.to_le_bytes()).collect(),
            vec![[255, 128, 0, 255]],
        ),
        (16, 16, 2, &gray, CGImageAlphaInfo::None.0, vec![0x80, 0x00], vec![[128, 128, 128, 255]]),
    ];
    for (i, (bpc, bpp, bpr, space, info, data, want)) in cases.into_iter().enumerate() {
        let img = image_of(1, bpc, bpp, bpr, Some(space), info, &data).unwrap_or_else(|| panic!("case {i}"));
        let got = drawn(&img);
        assert!(got.iter().zip(&want).all(|(a, b)| near(*a, *b, 1)), "case {i}: {got:?}, want {want:?}");
    }
    // Packed gray: the most significant bits first.
    let img = image_of(2, 1, 1, 1, Some(&gray), 0, &[0b1010_0000]).unwrap();
    assert_eq!(drawn(&img), [[255, 255, 255, 255], [0, 0, 0, 255]]);
    let img = image_of(2, 4, 4, 1, Some(&gray), 0, &[0x8f]).unwrap();
    assert_eq!(drawn(&img), [[136, 136, 136, 255], [255, 255, 255, 255]]);
    // Refused: rows too short, too little data, no space, alpha alone.
    assert!(image_of(3, 8, 32, 5, Some(&rgb), 1, &[0; 24]).is_none());
    assert!(image_of(3, 8, 32, 12, Some(&rgb), 1, &[1, 2, 3]).is_none());
    assert!(image_of(1, 8, 8, 1, None, CGImageAlphaInfo::Only.0, &[128]).is_none());
    // Five bits a component, a padding bit first, in a big-endian word (or
    // little-endian, as the byte order says).
    let red = image_of(1, 5, 16, 2, Some(&rgb), CGImageAlphaInfo::NoneSkipFirst.0, &[0x7c, 0]).unwrap();
    assert_eq!(drawn(&red), [[255, 0, 0, 255]]);
    let green = image_of(1, 5, 16, 2, Some(&rgb), CGImageAlphaInfo::NoneSkipFirst.0 | order16, &[0xe0, 0x03]).unwrap();
    assert_eq!(drawn(&green), [[0, 255, 0, 255]]);
}

#[test]
fn masks_and_context_images() {
    // Image masks paint the fill color where their samples are low.
    let data: Vec<u8> = vec![0, 255, 128, 64];
    let p = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&data))).unwrap();
    let mask = unsafe { CGImage::mask_create(2, 2, 8, 8, 2, Some(&p), std::ptr::null(), false) }.unwrap();
    assert!(CGImage::is_mask(Some(&mask)) && CGImage::color_space(Some(&mask)).is_none());
    let c = ctx(2, 2);
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 0.0, 1.0, 1.0);
    CGContext::draw_image(Some(&c), r(0.0, 0.0, 2.0, 2.0), Some(&mask));
    assert_px(&c, 0, 0, [0, 0, 255, 255], 1);
    assert_px(&c, 1, 0, [0; 4], 1);
    assert_px(&c, 0, 1, [0, 0, 127, 127], 2);
    assert_px(&c, 1, 1, [0, 0, 191, 191], 2);
    // Clipping to a mask: the same coverage; to a gray image: its levels.
    let c = ctx(2, 2);
    CGContext::clip_to_mask(Some(&c), r(0.0, 0.0, 2.0, 2.0), Some(&mask));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 2.0, 2.0));
    assert!(alpha(&c, 0, 0) == 255 && alpha(&c, 1, 0) == 0 && alpha(&c, 0, 1).abs_diff(127) <= 3);
    let gray = CGColorSpace::new_device_gray().unwrap();
    let gimg = unsafe {
        CGImage::new(
            2,
            2,
            8,
            8,
            2,
            Some(&gray),
            CGBitmapInfo(0),
            Some(&p),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap();
    let c = ctx(2, 2);
    CGContext::clip_to_mask(Some(&c), r(0.0, 0.0, 2.0, 2.0), Some(&gimg));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 2.0, 2.0));
    assert!(alpha(&c, 0, 0) == 0 && alpha(&c, 1, 0) == 255 && alpha(&c, 0, 1).abs_diff(128) <= 3);
    assert_eq!(
        drawn(
            &unsafe {
                CGImage::new(
                    2,
                    1,
                    8,
                    8,
                    2,
                    Some(&gray),
                    CGBitmapInfo(0),
                    Some(&p),
                    std::ptr::null(),
                    false,
                    CGColorRenderingIntent::RenderingIntentDefault,
                )
            }
            .unwrap()
        ),
        [[0, 0, 0, 255], [255, 255, 255, 255]]
    );
    // A decode array maps samples.
    let inverted = unsafe {
        CGImage::new(
            2,
            1,
            8,
            8,
            2,
            Some(&gray),
            CGBitmapInfo(0),
            Some(&p),
            [1.0, 0.0].as_ptr(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap();
    assert_eq!(drawn(&inverted), [[255, 255, 255, 255], [0, 0, 0, 255]]);
    // A context's image is a copy of what it holds, in its layout.
    let c = ctx(3, 2);
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    let img = CGBitmapContextCreateImage(Some(&c)).unwrap();
    assert_eq!(
        (CGImage::width(Some(&img)), CGImage::bytes_per_row(Some(&img)), CGImage::bitmap_info(Some(&img)).0),
        (3, 32, 1)
    );
    let bytes = CGDataProvider::data(CGImage::data_provider(Some(&img)).as_deref()).unwrap().to_vec();
    assert_eq!(bytes.len(), 64);
    assert_eq!(&bytes[32..36], &[255, 0, 0, 255]);
    CGContext::fill_rect(Some(&c), r(2.0, 1.0, 1.0, 1.0));
    let again = CGDataProvider::data(CGImage::data_provider(Some(&img)).as_deref()).unwrap().to_vec();
    assert_eq!(bytes, again, "drawing after doesn't reach the image");
    let back = ctx(3, 2);
    CGContext::draw_image(Some(&back), r(0.0, 0.0, 3.0, 2.0), Some(&img));
    assert_px(&back, 0, 1, [255, 0, 0, 255], 0);
    // A BGRA context's image draws the same.
    let bgra = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            2,
            1,
            8,
            0,
            Some(&srgb()),
            CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0,
        )
    }
    .unwrap();
    CGContext::set_rgb_fill_color(Some(&bgra), 0.0, 1.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&bgra), r(1.0, 0.0, 1.0, 1.0));
    let img = CGBitmapContextCreateImage(Some(&bgra)).unwrap();
    assert_eq!(drawn(&img), [[0, 0, 0, 0], [0, 255, 0, 255]]);
    // Contexts over the program's memory draw there.
    let mut mem = vec![0u8; 16];
    let c = unsafe { CGBitmapContextCreate(mem.as_mut_ptr().cast(), 2, 2, 8, 8, Some(&srgb()), 1) }.unwrap();
    CGContext::fill_rect(Some(&c), r(1.0, 1.0, 1.0, 1.0));
    drop(c);
    assert_eq!(&mem[4..8], &[0, 0, 0, 255]);
}

// Providers and fonts.

static RELEASED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[test]
fn data_providers() {
    unsafe extern "C-unwind" fn release(info: *mut c_void, _data: NonNull<c_void>, size: usize) {
        RELEASED.store(info as usize + size, std::sync::atomic::Ordering::SeqCst);
    }
    let bytes: &'static [u8] = &[9, 8, 7, 6];
    let p = unsafe { CGDataProvider::with_data(40 as *mut c_void, bytes.as_ptr().cast(), 4, Some(release)) }.unwrap();
    assert_eq!(CGDataProvider::data(Some(&p)).unwrap().to_vec(), [9, 8, 7, 6]);
    drop(p);
    assert_eq!(RELEASED.load(std::sync::atomic::Ordering::SeqCst), 44, "released with its info and size");
    let path = std::ffi::CString::new("/nonexistent/sidestep/file").unwrap();
    assert!(unsafe { CGDataProvider::with_filename(path.as_ptr()) }.is_none());
    let file = std::env::temp_dir().join(format!("sidestep-cg-{}.bin", std::process::id()));
    std::fs::write(&file, [1u8, 2, 3, 4, 5]).unwrap();
    let cpath = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
    let p = unsafe { CGDataProvider::with_filename(cpath.as_ptr()) }.unwrap();
    assert_eq!(CGDataProvider::data(Some(&p)).unwrap().to_vec(), [1, 2, 3, 4, 5]);
    let _ = std::fs::remove_file(&file);
}

/// A font file this system has: Arial or Geneva on macOS, DejaVu Sans on
/// Linux.
fn font_file() -> Option<&'static str> {
    [
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/System/Library/Fonts/Geneva.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ]
    .into_iter()
    .find(|p| std::fs::metadata(p).is_ok())
}

#[test]
fn fonts() {
    let bad = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&[1, 2, 3, 4]))).unwrap();
    assert!(CGFont::with_data_provider(&bad).is_none());
    let Some(path) = font_file() else {
        // Ubuntu (CI's too) has DejaVu: its absence there is a failure.
        if !cfg!(target_vendor = "apple") {
            panic!("no DejaVu Sans to read");
        }
        eprintln!("no font file to read: skipped");
        return;
    };
    let cpath = std::ffi::CString::new(path).unwrap();
    let provider = unsafe { CGDataProvider::with_filename(cpath.as_ptr()) }.unwrap();
    let font = CGFont::with_data_provider(&provider).expect("a font");
    // The values are the file's own: check them against its tables.
    let table = |tag: &[u8; 4]| CGFont::table_for_tag(Some(&font), u32::from_be_bytes(*tag)).map(|d| d.to_vec());
    let u16_at = |t: &[u8], at: usize| u16::from_be_bytes([t[at], t[at + 1]]);
    let i16_at = |t: &[u8], at: usize| i16::from_be_bytes([t[at], t[at + 1]]);
    let head = table(b"head").expect("head");
    assert_eq!(CGFont::units_per_em(Some(&font)), i32::from(u16_at(&head, 18)));
    let bbox = CGFont::font_b_box(Some(&font));
    assert_eq!((bbox.origin.x, bbox.origin.y), (f64::from(i16_at(&head, 36)), f64::from(i16_at(&head, 38))));
    assert_eq!(bbox.size.width, f64::from(i16_at(&head, 40)) - f64::from(i16_at(&head, 36)));
    let maxp = table(b"maxp").expect("maxp");
    assert_eq!(CGFont::number_of_glyphs(Some(&font)), usize::from(u16_at(&maxp, 4)));
    let hhea = table(b"hhea").expect("hhea");
    assert_eq!(CGFont::ascent(Some(&font)), i32::from(i16_at(&hhea, 4)));
    assert_eq!(CGFont::descent(Some(&font)), i32::from(i16_at(&hhea, 6)));
    assert_eq!(CGFont::leading(Some(&font)), i32::from(i16_at(&hhea, 8)));
    let os2 = table(b"OS/2").expect("OS/2");
    if os2.len() >= 90 {
        assert_eq!(CGFont::x_height(Some(&font)), i32::from(i16_at(&os2, 86)));
        assert_eq!(CGFont::cap_height(Some(&font)), i32::from(i16_at(&os2, 88)));
    }
    assert!(CGFont::x_height(Some(&font)) > 0 && CGFont::cap_height(Some(&font)) > CGFont::x_height(Some(&font)));
    assert!(CGFont::table_tags(Some(&font)).is_some_and(|t| t.count() >= 9));
    // Glyph names.
    let a = CGFont::glyph_with_glyph_name(Some(&font), Some(&CFString::from_str("A")));
    assert_ne!(a, 0);
    assert_eq!(CGFont::glyph_name_for_glyph(Some(&font), a).unwrap().to_string(), "A");
    assert_eq!(CGFont::glyph_name_for_glyph(Some(&font), 0).unwrap().to_string(), ".notdef");
    assert!(CGFont::glyph_name_for_glyph(Some(&font), 65000).is_none());
    assert_eq!(CGFont::glyph_with_glyph_name(Some(&font), Some(&CFString::from_str("nosuchglyph"))), 0);
    let (mut glyphs, mut advances, mut boxes) = ([a], [0i32], [CGRect::ZERO]);
    unsafe {
        assert!(CGFont::glyph_advances(
            Some(&font),
            NonNull::from(&mut glyphs).cast(),
            1,
            NonNull::from(&mut advances).cast()
        ));
        assert!(CGFont::glyph_b_boxes(
            Some(&font),
            NonNull::from(&mut glyphs).cast(),
            1,
            NonNull::from(&mut boxes).cast()
        ));
    }
    assert!(advances[0] > 0 && boxes[0].size.height > 0.0 && boxes[0].origin.y == 0.0, "{advances:?} {:?}", boxes[0]);
    // A glyph the font doesn't have advances nothing.
    let mut none = [65535u16];
    unsafe {
        assert!(CGFont::glyph_advances(
            Some(&font),
            NonNull::from(&mut none).cast(),
            1,
            NonNull::from(&mut advances).cast()
        ));
    }
    assert_eq!(advances[0], 0);
    // TrueType faces subset as Type 42.
    if table(b"glyf").is_some() {
        assert!(CGFont::can_create_post_script_subset(Some(&font), CGFontPostScriptFormat::Type42));
    }
    assert!(CGFont::post_script_name(Some(&font)).is_some_and(|n| !n.to_string().is_empty()));
}

// Edge cases the differential review measured on macOS.

#[test]
fn drawing_and_clipping_take_the_current_path() {
    // Every rectangle function (drawing and clipping) takes the path, as
    // the path functions do.
    let mask = image_of(1, 8, 8, 1, CGColorSpace::new_device_gray().as_deref(), 0, &[255]).unwrap();
    type Op<'a> = (&'a str, &'a dyn Fn(&CGContext));
    let ops: [Op; 7] = [
        ("fill_rect", &|c| CGContext::fill_rect(Some(c), r(0.0, 0.0, 1.0, 1.0))),
        ("fill_rects", &|c| unsafe { CGContext::fill_rects(Some(c), [r(0.0, 0.0, 1.0, 1.0)].as_ptr(), 1) }),
        ("stroke_rect", &|c| CGContext::stroke_rect(Some(c), r(0.0, 0.0, 1.0, 1.0))),
        ("stroke_rect_with_width", &|c| CGContext::stroke_rect_with_width(Some(c), r(0.0, 0.0, 1.0, 1.0), 2.0)),
        ("clear_rect", &|c| CGContext::clear_rect(Some(c), r(0.0, 0.0, 1.0, 1.0))),
        ("clip_to_rect", &|c| CGContext::clip_to_rect(Some(c), r(0.0, 0.0, 4.0, 4.0))),
        ("clip_to_mask", &|c| CGContext::clip_to_mask(Some(c), r(0.0, 0.0, 4.0, 4.0), Some(&mask))),
    ];
    for (name, op) in ops {
        let c = ctx(4, 4);
        CGContext::add_rect(Some(&c), r(1.0, 1.0, 2.0, 2.0));
        op(&c);
        assert!(CGContext::is_path_empty(Some(&c)), "{name}");
    }
    // So a path left over isn't filled by the next fill of a path.
    let c = ctx(4, 1);
    CGContext::add_rect(Some(&c), r(2.0, 0.0, 2.0, 1.0));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    CGContext::fill_path(Some(&c));
    assert_eq!((0..4).map(|x| alpha(&c, x, 0)).collect::<Vec<_>>(), [255, 0, 0, 0]);
    // Drawing images and gradients, and changing the line, keep it.
    let c = ctx(4, 1);
    CGContext::add_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    CGContext::draw_image(Some(&c), r(0.0, 0.0, 1.0, 1.0), Some(&test_image()));
    CGContext::set_line_width(Some(&c), 3.0);
    assert!(!CGContext::is_path_empty(Some(&c)));
}

#[test]
fn clips_of_nothing_and_of_no_area() {
    // A path with no elements leaves the clip as it was.
    for eo in [false, true] {
        let c = ctx(4, 4);
        CGContext::begin_path(Some(&c));
        if eo {
            CGContext::eo_clip(Some(&c));
        } else {
            CGContext::clip(Some(&c));
        }
        same_rect(CGContext::clip_bounding_box(Some(&c)), r(0.0, 0.0, 4.0, 4.0));
        CGContext::fill_rect(Some(&c), r(0.0, 0.0, 4.0, 4.0));
        assert!((0..4).all(|y| (0..4).all(|x| alpha(&c, x, y) == 255)), "eo {eo}");
    }
    // One with elements but no area leaves nothing, at its bounds.
    let c = ctx(4, 4);
    CGContext::move_to_point(Some(&c), 1.0, 1.0);
    CGContext::clip(Some(&c));
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(1.0, 1.0, 0.0, 0.0));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 4.0, 4.0));
    assert!((0..4).all(|y| (0..4).all(|x| alpha(&c, x, y) == 0)));
    let c = ctx(4, 4);
    CGContext::add_rect(Some(&c), r(1.0, 1.0, 0.0, 2.0));
    CGContext::clip(Some(&c));
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(1.0, 1.0, 0.0, 2.0));
    let c = ctx(4, 4);
    CGContext::clip_to_rect(Some(&c), r(1.0, 1.0, 0.0, 0.0));
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(1.0, 1.0, 0.0, 0.0));
    // A rectangle with edges inside pixels clips them antialiased.
    let c = ctx(4, 4);
    CGContext::clip_to_rect(Some(&c), r(0.5, 0.5, 1.0, 1.0));
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(0.5, 0.5, 1.0, 1.0));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 4.0, 4.0));
    for (x, y) in [(0, 2), (1, 2), (0, 3), (1, 3)] {
        assert!(alpha(&c, x, y).abs_diff(64) <= 2, "({x}, {y}): {}", alpha(&c, x, y));
    }
    assert_eq!(alpha(&c, 2, 2), 0);
}

#[test]
fn transparency_layers_save_on_the_stack() {
    // A layer's beginning saves the state on the stack the program's saves
    // use, and its end restores whatever is on top.
    let c = ctx(4, 1);
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::save_g_state(Some(&c));
    CGContext::set_rgb_fill_color(Some(&c), 0.0, 1.0, 0.0, 1.0);
    unsafe { CGContext::begin_transparency_layer(Some(&c), None) };
    CGContext::restore_g_state(Some(&c));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    CGContext::end_transparency_layer(Some(&c));
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 1.0, 1.0));
    CGContext::restore_g_state(Some(&c));
    CGContext::fill_rect(Some(&c), r(2.0, 0.0, 1.0, 1.0));
    CGContext::end_transparency_layer(Some(&c));
    assert_eq!(
        [px(&c, 0, 0), px(&c, 1, 0), px(&c, 2, 0), px(&c, 3, 0)],
        [[0, 255, 0, 255], [255, 0, 0, 255], [255, 0, 0, 255], [0, 0, 0, 0]]
    );
    // A layer's rectangle narrows the clip.
    let c = ctx(4, 4);
    unsafe { CGContext::begin_transparency_layer_with_rect(Some(&c), r(1.0, 1.0, 2.0, 2.0), None) };
    same_rect(CGContext::clip_bounding_box(Some(&c)), r(1.0, 1.0, 2.0, 2.0));
    CGContext::end_transparency_layer(Some(&c));
}

#[test]
fn translucent_paint_casts_translucent_shadows() {
    let black = CGColor::new_generic_gray(0.0, 1.0);
    let c = ctx(8, 8);
    CGContext::set_shadow_with_color(Some(&c), CGSize::new(2.0, -2.0), 0.0, Some(&black));
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 0.5);
    CGContext::fill_rect(Some(&c), r(1.0, 3.0, 3.0, 3.0));
    assert_px(&c, 4, 5, [0, 0, 0, 128], 1);
    assert_px(&c, 3, 4, [128, 0, 0, 192], 1);
    let c = ctx(8, 8);
    CGContext::set_shadow_with_color(Some(&c), CGSize::new(2.0, -2.0), 0.0, Some(&black));
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 0.25);
    CGContext::fill_ellipse_in_rect(Some(&c), r(0.0, 2.0, 5.0, 5.0));
    assert_px(&c, 5, 5, [0, 0, 0, 64], 2);
    // The global alpha fades shadow and shape once.
    let c = ctx(8, 8);
    CGContext::set_alpha(Some(&c), 0.5);
    CGContext::set_shadow_with_color(Some(&c), CGSize::new(2.0, -2.0), 0.0, Some(&black));
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&c), r(1.0, 3.0, 3.0, 3.0));
    assert_px(&c, 4, 5, [0, 0, 0, 128], 1);
    assert_px(&c, 1, 2, [128, 0, 0, 128], 1);
}

#[test]
fn generic_spaces_are_managed() {
    // Generic RGB and Generic Gray have a gamma of 1.8 and (RGB) primaries
    // of their own: drawn in sRGB, they're converted.
    let c = ctx(3, 1);
    CGContext::set_fill_color_with_color(Some(&c), Some(&CGColor::new_generic_gray(0.5, 1.0)));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    CGContext::set_fill_color_with_color(Some(&c), Some(&CGColor::new_generic_rgb(0.5, 0.25, 0.75, 1.0)));
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 1.0, 1.0));
    CGContext::set_fill_color_with_color(Some(&c), Some(&CGColor::new_generic_gray_gamma2_2(0.5, 1.0)));
    CGContext::fill_rect(Some(&c), r(2.0, 0.0, 1.0, 1.0));
    assert_px(&c, 0, 0, [146, 146, 146, 255], 1);
    assert_px(&c, 1, 0, [147, 90, 203, 255], 1);
    assert_px(&c, 2, 0, [128, 128, 128, 255], 1);
    let srgb = srgb();
    let matched = |c: &CGColor| {
        unsafe {
            CGColor::new_copy_by_matching_to_color_space(
                Some(&srgb),
                CGColorRenderingIntent::RenderingIntentDefault,
                Some(c),
                None,
            )
        }
        .map(|m| comps(&m))
        .unwrap()
    };
    let red = matched(&CGColor::new_generic_rgb(1.0, 0.0, 0.0, 1.0));
    assert!((red[0] - 1.0).abs() < 1e-3 && (red[1] - 0.1491).abs() < 2e-3 && red[2].abs() < 1e-3, "{red:?}");
    let gray = matched(&CGColor::new_generic_gray(0.5, 1.0));
    assert!(gray[..3].iter().all(|v| (v - 0.5723).abs() < 2e-3), "{gray:?}");
}

#[test]
fn derived_spaces() {
    let named = |n: &CFString| CGColorSpace::with_name(Some(n)).unwrap();
    let name =
        |s: Option<CFRetained<CGColorSpace>>| s.and_then(|s| CGColorSpace::name(Some(&s))).map(|n| n.to_string());
    unsafe {
        // A space with no standard-range counterpart is its own.
        let xyz = named(kCGColorSpaceGenericXYZ);
        assert_eq!(name(Some(xyz.copy_with_standard_range())).as_deref(), Some("kCGColorSpaceGenericXYZ"));
        let ext_p3 = named(kCGColorSpaceExtendedDisplayP3);
        assert_eq!(name(Some(ext_p3.copy_with_standard_range())).as_deref(), Some("kCGColorSpaceDisplayP3"));
        let ext_2020 = named(kCGColorSpaceExtendedITUR_2020);
        assert_eq!(name(Some(ext_2020.copy_with_standard_range())).as_deref(), Some("kCGColorSpaceITUR_2020"));
        assert_eq!(name(named(kCGColorSpaceSRGB).linearized()).as_deref(), Some("kCGColorSpaceLinearSRGB"));
        assert_eq!(name(named(kCGColorSpaceSRGB).extended()).as_deref(), Some("kCGColorSpaceExtendedSRGB"));
        // Generic RGB's linear form has no name; Lab has none.
        let lin = named(kCGColorSpaceGenericRGB).linearized().expect("a linear Generic RGB");
        assert!(CGColorSpace::name(Some(&lin)).is_none());
        assert!(named(kCGColorSpaceGenericLab).linearized().is_none());
        assert!(named(kCGColorSpaceGenericXYZ).supports_output() && named(kCGColorSpaceGenericLab).supports_output());
        let pattern = CGColorSpace::new_pattern(Some(&srgb())).unwrap();
        assert_eq!(CGColorSpace::number_of_components(Some(&pattern)), 3);
    }
    // A copy with another alpha keeps it as given.
    let c = CGColor::new_srgb(1.0, 0.0, 0.0, 1.0);
    assert_eq!(CGColor::new_copy_with_alpha(Some(&c), 2.0).map(|c| comps(&c)[3]), Some(2.0));
    // No color is black; a pattern space's colors draw nothing.
    let c = ctx(3, 1);
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::set_fill_color_with_color(Some(&c), None);
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
    CGContext::set_rgb_fill_color(Some(&c), 1.0, 0.0, 0.0, 1.0);
    CGContext::set_fill_color_space(Some(&c), None);
    CGContext::fill_rect(Some(&c), r(1.0, 0.0, 1.0, 1.0));
    CGContext::set_fill_color_space(Some(&c), CGColorSpace::new_pattern(None).as_deref());
    CGContext::fill_rect(Some(&c), r(2.0, 0.0, 1.0, 1.0));
    assert_eq!([px(&c, 0, 0), px(&c, 1, 0), px(&c, 2, 0)], [[0, 0, 0, 255], [0, 0, 0, 255], [0, 0, 0, 0]]);
}

#[test]
fn color_burn_and_soft_light() {
    // Over (0.2, 0.4, 0.6), opaque and half transparent, a source of
    // (0.8, 0.5, 0.2) at 0.75.
    let blended = |mode: CGBlendMode, dst_alpha: f64| {
        let c = ctx(1, 1);
        CGContext::set_rgb_fill_color(Some(&c), 0.2, 0.4, 0.6, dst_alpha);
        CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
        CGContext::set_blend_mode(Some(&c), mode);
        CGContext::set_rgb_fill_color(Some(&c), 0.8, 0.5, 0.2, 0.75);
        CGContext::fill_rect(Some(&c), r(0.0, 0.0, 1.0, 1.0));
        px(&c, 0, 0)
    };
    for (mode, dst, want) in [
        (CGBlendMode::ColorBurn, 1.0, [13, 0, 0, 255]),
        (CGBlendMode::ColorBurn, 0.5, [83, 42, 0, 223]),
        (CGBlendMode::SoftLight, 1.0, [69, 102, 125, 255]),
        (CGBlendMode::SoftLight, 0.5, [112, 99, 82, 223]),
    ] {
        let got = blended(mode, dst);
        assert!(near(got, want, 2), "{mode:?} over alpha {dst}: {got:?}, want {want:?}");
    }
}

#[test]
fn arcs_dashes_and_strokes_at_the_limits() {
    let pi = std::f64::consts::PI;
    let arc =
        |e: f64| els(&mutable(|m| unsafe { CGMutablePath::add_arc(Some(m), NOXF, 10.0, 10.0, 5.0, 0.0, e, false) }));
    // More than a thousand turns add nothing; just under, every quarter.
    assert_eq!(arc(2000.0 * pi + 1e-6), "");
    assert_eq!(arc(1e300), "");
    assert_eq!(arc(2000.0 * pi - 1e-6).matches('C').count(), 4000);
    let rel =
        |d: f64| els(&mutable(|m| unsafe { CGMutablePath::add_relative_arc(Some(m), NOXF, 0.0, 0.0, 1.0, 0.0, d) }));
    assert_eq!(rel(1e9), "");
    // No end: the start alone; next to no sweep: the same.
    assert_eq!(arc(f64::NAN), "M 15.0000,10.0000;");
    assert_eq!(arc(1e-9), "M 15.0000,10.0000;");
    // Dashing copies: lengths' sizes count; none adding up to anything
    // leaves nothing; a closed subpath's last dash doesn't join its first.
    let line = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 0.0, 0.0);
        CGMutablePath::add_line_to_point(Some(m), NOXF, 10.0, 0.0);
    });
    let dash = |p: &CGPath, lens: &[f64]| {
        unsafe { CGPath::new_copy_by_dashing_path(Some(p), NOXF, 0.0, lens.as_ptr(), lens.len()) }.map(|d| els(&d))
    };
    assert_eq!(dash(&line, &[0.0, 0.0]).as_deref(), Some(""));
    assert_eq!(
        dash(&line, &[2.0, -1.0]).as_deref(),
        Some(
            "M 0.0000,0.0000; L 2.0000,0.0000; M 3.0000,0.0000; L 5.0000,0.0000; M 6.0000,0.0000; L 8.0000,0.0000; M 9.0000,0.0000; L 10.0000,0.0000;"
        )
    );
    let square = unsafe { CGPath::with_rect(r(0.0, 0.0, 10.0, 10.0), NOXF) };
    let dashed = unsafe { CGPath::new_copy_by_dashing_path(Some(&square), NOXF, 0.0, [4.0, 2.0].as_ptr(), 2) }.unwrap();
    let text = els(&dashed);
    assert!(text.starts_with("M 0.0000,0.0000; L 4.0000,0.0000; M 6.0000,0.0000; L 10.0000,0.0000;"), "{text}");
    assert!(text.ends_with("M 0.0000,4.0000; L 0.0000,0.0000;"), "the last dash alone: {text}");
    // Stroking copies: a negative width is its size; a subpath of no
    // length with round caps is a dot.
    let s =
        unsafe { CGPath::new_copy_by_stroking_path(Some(&line), NOXF, -2.0, CGLineCap::Butt, CGLineJoin::Miter, 10.0) }
            .unwrap();
    same_rect(CGPath::path_bounding_box(Some(&s)), r(0.0, -1.0, 10.0, 2.0));
    let dot = mutable(|m| unsafe {
        CGMutablePath::move_to_point(Some(m), NOXF, 5.0, 5.0);
        CGMutablePath::add_line_to_point(Some(m), NOXF, 5.0, 5.0);
    });
    let s =
        unsafe { CGPath::new_copy_by_stroking_path(Some(&dot), NOXF, 2.0, CGLineCap::Round, CGLineJoin::Miter, 10.0) }
            .unwrap();
    same_rect(CGPath::path_bounding_box(Some(&s)), r(4.0, 4.0, 2.0, 2.0));
    // A negative line width strokes a line 1 wide.
    let c = ctx(4, 4);
    CGContext::set_line_width(Some(&c), -2.0);
    CGContext::move_to_point(Some(&c), 0.0, 2.0);
    CGContext::add_line_to_point(Some(&c), 4.0, 2.0);
    CGContext::stroke_path(Some(&c));
    assert!(alpha(&c, 0, 1).abs_diff(127) <= 1 && alpha(&c, 0, 2).abs_diff(127) <= 1 && alpha(&c, 0, 0) == 0);
    // Rectangles as CGPathIsRect sees them: made as one, whatever its size.
    let mut rr = CGRect::ZERO;
    let empty = unsafe { CGPath::with_rect(r(1.0, 1.0, 0.0, 0.0), NOXF) };
    assert!(unsafe { CGPath::is_rect(Some(&empty), &mut rr) });
    same_rect(rr, r(1.0, 1.0, 0.0, 0.0));
    let ccw = mutable(|m| unsafe {
        let pts = [pt(0.0, 0.0), pt(3.0, 0.0), pt(3.0, 2.0), pt(0.0, 2.0)];
        CGMutablePath::add_lines(Some(m), NOXF, pts.as_ptr(), 4);
        CGMutablePath::close_subpath(Some(m));
    });
    assert!(!unsafe { CGPath::is_rect(Some(&ccw), &mut rr) });
    let flip = CGAffineTransformMakeScale(1.0, -1.0);
    assert!(!unsafe { CGPath::is_rect(Some(&CGPath::with_rect(r(0.0, 0.0, 1.0, 1.0), &flip)), &mut rr) });
}

#[test]
fn geometry_edge_cases() {
    let zero = r(0.0, 0.0, 0.0, 0.0);
    let thin = r(0.0, 0.0, 0.0, 5.0);
    let a = r(1.0, 2.0, 3.0, 4.0);
    let neg = r(10.0, 10.0, -5.0, -5.0);
    let null = unsafe { CGRectNull };
    let inf = unsafe { CGRectInfinite };
    // Empty rectangles meeting intersect; touching ones don't.
    assert!(CGRectIntersectsRect(zero, zero) && CGRectIntersectsRect(zero, thin) && CGRectIntersectsRect(thin, thin));
    assert!(!CGRectIntersectsRect(zero, a) && !CGRectIntersectsRect(r(0.0, 0.0, 1.0, 1.0), r(1.0, 0.0, 1.0, 1.0)));
    // Every rectangle contains the null one.
    assert!(CGRectContainsRect(a, null) && CGRectContainsRect(null, null) && CGRectContainsRect(zero, null));
    assert!(!CGRectContainsRect(null, a));
    // A union with the null rectangle is the other, as it was.
    let u = CGRectUnion(neg, null);
    assert_eq!((u.origin.x, u.size.width), (10.0, -5.0));
    let u = CGRectUnion(null, neg);
    assert_eq!((u.origin.y, u.size.height), (10.0, -5.0));
    assert_eq!((CGRectGetMidX(null), CGRectGetMidY(null)), (f64::INFINITY, f64::INFINITY));
    let t = CGAffineTransformMakeScale(2.0, -2.0);
    assert!(CGRectIsInfinite(CGRectApplyAffineTransform(inf, t)));
}

#[test]
fn bitmap_contexts_over_program_memory() {
    let rgb = srgb();
    let gray = CGColorSpace::new_device_gray().unwrap();
    // The program's memory needs a row length.
    let mut mem = vec![0u8; 9 * 4 * 2];
    for space in [&rgb, &gray] {
        let info = if std::ptr::eq(space, &rgb) { CGImageAlphaInfo::PremultipliedLast.0 } else { 0 };
        let c = unsafe { CGBitmapContextCreate(mem.as_mut_ptr().cast(), 9, 2, 8, 0, Some(space), info) };
        assert!(c.is_none());
    }
    let c = unsafe {
        CGBitmapContextCreate(mem.as_mut_ptr().cast(), 9, 2, 8, 36, Some(&rgb), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .expect("rows of 36 bytes");
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 9.0, 2.0));
    drop(c);
    assert!(mem.iter().all(|&b| b == 0 || b == 255) && mem[3] == 255 && mem[71] == 255);
    // Its own memory's rows are aligned: long enough, a multiple of 16 on
    // macOS (the exact alignment is macOS's to choose), of 32 here.
    let own = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 9, 1, 8, 0, Some(&rgb), 1) }.unwrap();
    let bpr = CGBitmapContextGetBytesPerRow(Some(&own));
    if cfg!(target_vendor = "apple") {
        assert!(bpr >= 36 && bpr.is_multiple_of(16), "{bpr}");
    } else {
        assert_eq!(bpr, 64);
    }
    // A context of its own memory hands it to the release callback.
    // (objc2-core-graphics doesn't declare this function.)
    unsafe extern "C-unwind" {
        fn CGBitmapContextCreateWithData(
            data: *mut c_void,
            width: usize,
            height: usize,
            bpc: usize,
            bpr: usize,
            space: Option<&CGColorSpace>,
            info: u32,
            release: Option<unsafe extern "C-unwind" fn(*mut c_void, *mut c_void)>,
            release_info: *mut c_void,
        ) -> Option<NonNull<CGContext>>;
    }
    static CALLED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    unsafe extern "C-unwind" fn release(info: *mut c_void, data: *mut c_void) {
        if !data.is_null() {
            CALLED.store(info as usize, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let c = unsafe {
        CGBitmapContextCreateWithData(std::ptr::null_mut(), 2, 2, 8, 0, Some(&rgb), 1, Some(release), 9 as *mut c_void)
    };
    // SAFETY: a Create function's +1 reference.
    drop(c.map(|c| unsafe { CFRetained::from_raw(c) }));
    assert_eq!(CALLED.load(std::sync::atomic::Ordering::SeqCst), 9);
    // An image of a context of alpha alone is an image mask of it.
    let only =
        unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 2, 1, 8, 0, None, CGImageAlphaInfo::Only.0) }.unwrap();
    CGContext::set_rgb_fill_color(Some(&only), 0.0, 0.0, 0.0, 0.5);
    CGContext::fill_rect(Some(&only), r(0.0, 0.0, 1.0, 1.0));
    let img = CGBitmapContextCreateImage(Some(&only)).unwrap();
    assert!(CGImage::is_mask(Some(&img)) && !CGImage::decode(Some(&img)).is_null());
    assert_eq!(drawn(&img).iter().map(|p| p[3]).collect::<Vec<_>>(), [128, 0]);
    // Byte orders must fit the samples; extended spaces take floats.
    let ext = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceExtendedSRGB })).unwrap();
    let make = |bpc, space: &CGColorSpace, info| {
        unsafe { CGBitmapContextCreate(std::ptr::null_mut(), 2, 2, bpc, 0, Some(space), info) }.is_some()
    };
    let (o16, o32) = (CGImageByteOrderInfo::Order16Little.0, CGImageByteOrderInfo::Order32Little.0);
    let float = CGImageComponentInfo::Float.0;
    assert!(!make(8, &rgb, 1 | o16) && !make(16, &rgb, 1 | o32) && !make(32, &rgb, 1 | float | o16));
    assert!(!make(8, &ext, 1) && !make(16, &ext, 1) && make(32, &ext, 1 | float));
    assert!(make(8, &gray, CGImageAlphaInfo::NoneSkipLast.0));
}

#[test]
fn image_parts_decodes_and_layouts() {
    // A part's provider holds its rows.
    let sub = CGImage::with_image_in_rect(Some(&test_image()), r(1.0, 1.0, 2.0, 1.0)).unwrap();
    let bytes = CGDataProvider::data(CGImage::data_provider(Some(&sub)).as_deref()).map(|d| d.to_vec());
    assert_eq!(bytes, Some(vec![0, 0, 0, 255, 0, 0, 0, 0]));
    let straight =
        image_of(2, 8, 32, 8, Some(&srgb()), CGImageAlphaInfo::Last.0, &[200, 100, 50, 128, 10, 20, 30, 255]).unwrap();
    let sub = CGImage::with_image_in_rect(Some(&straight), r(0.0, 0.0, 1.0, 1.0)).unwrap();
    assert_eq!(drawn(&sub), [[100, 50, 25, 128]]);
    let bytes = CGDataProvider::data(CGImage::data_provider(Some(&sub)).as_deref()).map(|d| d.to_vec());
    assert_eq!(bytes, Some(vec![200, 100, 50, 128]));
    // Decode arrays map their range onto 0 to 1, and leave alpha alone.
    let decoded = |data: &[u8], bpp: usize, info: u32, decode: &[f64]| {
        let p = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(data))).unwrap();
        let img = unsafe {
            CGImage::new(
                1,
                1,
                8,
                bpp,
                bpp / 8,
                Some(&srgb()),
                CGBitmapInfo(info),
                Some(&p),
                decode.as_ptr(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
        .unwrap();
        drawn(&img)[0]
    };
    assert_eq!(decoded(&[255, 255, 0], 24, 0, &[1.0, 0.0, 0.0, 1.0, 0.5, 1.0]), [0, 255, 0, 255]);
    let alpha_last = CGImageAlphaInfo::Last.0;
    assert_eq!(decoded(&[255, 0, 0, 255], 32, alpha_last, &[0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0]), [255, 0, 0, 255]);
    // Layouts CoreGraphics takes, and one it doesn't.
    let rgb = srgb();
    assert!(image_of(1, 8, 32, 4, Some(&rgb), 0, &[10, 20, 30, 40]).is_none(), "RGB, no alpha, 32 bits");
    let half = CGImageComponentInfo::Float.0;
    let img = image_of(1, 16, 48, 6, Some(&rgb), half, &[0x3c, 0, 0x38, 0, 0, 0]).expect("half floats");
    assert_eq!(CGImage::byte_order_info(Some(&img)), CGImageByteOrderInfo::Order16Big);
    assert_eq!(drawn(&img), [[255, 128, 0, 255]]);
    let int32 = image_of(1, 32, 96, 12, Some(&rgb), 0, &[255; 12]).expect("32-bit integers");
    assert_eq!(drawn(&int32), [[255, 255, 255, 255]]);
    let p = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&[0, 0, 255, 255]))).unwrap();
    let m = unsafe { CGImage::mask_create(2, 1, 8, 16, 4, Some(&p), std::ptr::null(), false) }.expect("wider pixels");
    assert_eq!(drawn(&m).iter().map(|p| p[3]).collect::<Vec<_>>(), [255, 0]);
    // An image with alpha clips by its alpha; a color image without, by
    // its gray.
    let ga = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&[255, 255, 255, 0, 0, 255]))).unwrap();
    let ga = unsafe {
        CGImage::new(
            3,
            1,
            8,
            16,
            6,
            CGColorSpace::new_device_gray().as_deref(),
            CGBitmapInfo(CGImageAlphaInfo::Last.0),
            Some(&ga),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap();
    let c = ctx(3, 1);
    CGContext::clip_to_mask(Some(&c), r(0.0, 0.0, 3.0, 1.0), Some(&ga));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 3.0, 1.0));
    assert_eq!((0..3).map(|x| alpha(&c, x, 0)).collect::<Vec<_>>(), [255, 0, 255]);
    let rgb24 = image_of(3, 8, 24, 9, Some(&rgb), 0, &[255, 255, 255, 0, 0, 0, 128, 128, 128]).unwrap();
    let c = ctx(3, 1);
    CGContext::clip_to_mask(Some(&c), r(0.0, 0.0, 3.0, 1.0), Some(&rgb24));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 3.0, 1.0));
    assert!(near([alpha(&c, 0, 0), alpha(&c, 1, 0), alpha(&c, 2, 0), 0], [255, 0, 128, 0], 2));
    // Clipping to a mask stretched with no interpolation takes the
    // nearest samples.
    let g = image_of(2, 8, 8, 2, CGColorSpace::new_device_gray().as_deref(), 0, &[0, 255]).unwrap();
    let c = ctx(4, 1);
    CGContext::set_interpolation_quality(Some(&c), CGInterpolationQuality::None);
    CGContext::clip_to_mask(Some(&c), r(0.0, 0.0, 4.0, 1.0), Some(&g));
    CGContext::fill_rect(Some(&c), r(0.0, 0.0, 4.0, 1.0));
    assert_eq!((0..4).map(|x| alpha(&c, x, 0)).collect::<Vec<_>>(), [0, 0, 255, 255]);
}

#[test]
fn radial_gradients_after_their_end() {
    let g = two_stops();
    // Ending inside its start: what's inside the end comes after it.
    let c = ctx(9, 1);
    CGContext::draw_radial_gradient(
        Some(&c),
        Some(&g),
        pt(4.5, 0.5),
        3.0,
        pt(4.5, 0.5),
        1.0,
        CGGradientDrawingOptions::empty(),
    );
    assert_eq!(alpha(&c, 4, 0), 0, "after the end");
    assert_eq!((alpha(&c, 0, 0), alpha(&c, 8, 0)), (0, 0));
    // The circles' edges are hard: a pixel whose center is on one is in.
    assert_px(&c, 1, 0, [255, 0, 0, 255], 12);
    assert_px(&c, 7, 0, [255, 0, 0, 255], 12);
    mix(&c, 2, 0, 0.5);
    assert_px(&c, 3, 0, [0, 0, 255, 255], 12);
    // Locations outside 0 to 1 make no gradient.
    let comps = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0];
    let g = unsafe { CGGradient::with_color_components(Some(&srgb()), comps.as_ptr(), [-0.5, 1.5].as_ptr(), 2) };
    assert!(g.is_none());
    // Colors interpolate in the gradient's space: linear light here.
    let lin = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceLinearSRGB })).unwrap();
    let comps = [1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
    let g = unsafe { CGGradient::with_color_components(Some(&lin), comps.as_ptr(), std::ptr::null(), 2) }.unwrap();
    let c = ctx(4, 1);
    CGContext::draw_linear_gradient(Some(&c), Some(&g), pt(0.0, 0.0), pt(4.0, 0.0), CGGradientDrawingOptions::empty());
    assert_px(&c, 0, 0, [243, 93, 0, 255], 12);
    assert_px(&c, 1, 0, [205, 168, 0, 255], 12);
}

/// Two constants with macOS's values: the default HDR image content
/// headroom, and the adaptive bit depth option's name.
#[test]
fn hdr_and_adaptive_constants() {
    assert_eq!(unsafe { kCGDefaultHDRImageContentHeadroom }.to_bits(), 0x409d_a2ae);
    assert_eq!(unsafe { kCGAdaptiveMaximumBitDepth }.to_string(), "kCGAdaptiveMaximumBitDepth");
}
