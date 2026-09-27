//! AppKit's CoreGraphics bridges, checked against macOS: `-[NSColor
//! CGColor]` and `+colorWithCGColor:`, `-[NSGraphicsContext CGContext]` and
//! `+graphicsContextWithCGContext:flipped:` (the two sharing one graphics
//! state), CoreGraphics in `drawRect:`, `NSImage` and `NSBitmapImageRep`
//! with `CGImage`s, and `NSBezierPath`'s `CGPath`. Views draw into bitmaps
//! (`cacheDisplayInRect:toBitmapImageRep:`), so no window is needed.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use common::*;
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBezierPath, NSBezierPathElement, NSBitmapFormat, NSBitmapImageRep, NSColor, NSColorType, NSCompositingOperation,
    NSDeviceRGBColorSpace, NSGraphicsContext, NSImage, NSWindingRule,
};
use objc2_core_foundation::{CFData, CFRetained, CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::*;
use objc2_foundation::{NSPoint, NSRect, NSSize};

use sidestep as _;

fn cg_rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

fn comps(c: &CGColor) -> Vec<f64> {
    let n = CGColor::number_of_components(Some(c));
    // SAFETY: the color has `n` components.
    unsafe { std::slice::from_raw_parts(CGColor::components(Some(c)), n) }.to_vec()
}

fn space_name(c: &CGColor) -> Option<String> {
    CGColorSpace::name(CGColor::color_space(Some(c)).as_deref()).map(|n| n.to_string())
}

fn srgb() -> CFRetained<CGColorSpace> {
    CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).unwrap()
}

fn cg_bitmap(w: usize, h: usize) -> CFRetained<CGContext> {
    // SAFETY: no data: the context allocates.
    unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&srgb()), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .unwrap()
}

fn cg_px(c: &CGContext, x: usize, y: usize) -> [u8; 4] {
    let d = CGBitmapContextGetData(Some(c)) as *const u8;
    let bpr = CGBitmapContextGetBytesPerRow(Some(c));
    // SAFETY: inside the context's memory.
    unsafe { std::ptr::read(d.add(y * bpr + x * 4).cast::<[u8; 4]>()) }
}

fn transform(a: f64, b: f64, c: f64, d: f64, tx: f64, ty: f64) -> CGAffineTransform {
    CGAffineTransform { a, b, c, d, tx, ty }
}

#[track_caller]
fn same_transform(got: CGAffineTransform, want: CGAffineTransform) {
    let pairs =
        [(got.a, want.a), (got.b, want.b), (got.c, want.c), (got.d, want.d), (got.tx, want.tx), (got.ty, want.ty)];
    assert!(pairs.iter().all(|(a, b)| (a - b).abs() < 1e-9), "{got:?}, want {want:?}");
}

fn colors(_: MainThreadMarker) {
    let cases: [(&str, Retained<NSColor>, &str, Vec<f64>); 11] = [
        (
            "sRGB",
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.0, 0.0, 1.0),
            "kCGColorSpaceSRGB",
            vec![1.0, 0.0, 0.0, 1.0],
        ),
        (
            "rgb",
            NSColor::colorWithRed_green_blue_alpha(0.2, 0.4, 0.6, 0.8),
            "kCGColorSpaceSRGB",
            vec![0.2, 0.4, 0.6, 0.8],
        ),
        (
            "calibrated",
            NSColor::colorWithCalibratedRed_green_blue_alpha(0.2, 0.4, 0.6, 1.0),
            "kCGColorSpaceGenericRGB",
            vec![0.2, 0.4, 0.6, 1.0],
        ),
        (
            "device",
            NSColor::colorWithDeviceRed_green_blue_alpha(0.2, 0.4, 0.6, 1.0),
            "kCGColorSpaceDeviceRGB",
            vec![0.2, 0.4, 0.6, 1.0],
        ),
        (
            "P3",
            NSColor::colorWithDisplayP3Red_green_blue_alpha(0.2, 0.4, 0.6, 1.0),
            "kCGColorSpaceDisplayP3",
            vec![0.2, 0.4, 0.6, 1.0],
        ),
        ("white", NSColor::colorWithWhite_alpha(0.5, 1.0), "kCGColorSpaceGenericGrayGamma2_2", vec![0.5, 1.0]),
        (
            "calibrated white",
            NSColor::colorWithCalibratedWhite_alpha(0.5, 1.0),
            "kCGColorSpaceGenericGray",
            vec![0.5, 1.0],
        ),
        ("device white", NSColor::colorWithDeviceWhite_alpha(0.5, 1.0), "kCGColorSpaceDeviceGray", vec![0.5, 1.0]),
        ("clear", NSColor::clearColor(), "kCGColorSpaceGenericGrayGamma2_2", vec![0.0, 0.0]),
        (
            "cmyk",
            NSColor::colorWithDeviceCyan_magenta_yellow_black_alpha(0.1, 0.2, 0.3, 0.4, 1.0),
            "kCGColorSpaceDeviceCMYK",
            vec![0.1, 0.2, 0.3, 0.4, 1.0],
        ),
        ("with alpha", NSColor::redColor().colorWithAlphaComponent(0.5), "kCGColorSpaceSRGB", vec![1.0, 0.0, 0.0, 0.5]),
    ];
    for (name, color, space, want) in cases {
        let cg = color.CGColor();
        assert_eq!(space_name(&cg).as_deref(), Some(space), "{name}");
        assert_eq!(comps(&cg), want, "{name}");
    }
    // A system color resolves in sRGB, whatever the appearance.
    let label = NSColor::labelColor().CGColor();
    assert_eq!((space_name(&label).as_deref(), comps(&label).len()), (Some("kCGColorSpaceSRGB"), 4));
    // And back: the CGColor's space and components.
    let back: [(&str, CFRetained<CGColor>, &str, Vec<f64>); 6] = [
        ("generic RGB", CGColor::new_generic_rgb(0.2, 0.4, 0.6, 0.8), "Generic RGB", vec![0.2, 0.4, 0.6, 0.8]),
        ("sRGB", CGColor::new_srgb(0.2, 0.4, 0.6, 0.8), "sRGB IEC61966-2.1", vec![0.2, 0.4, 0.6, 0.8]),
        ("generic gray", CGColor::new_generic_gray(0.5, 0.8), "Generic Gray", vec![0.5, 0.8]),
        ("gray gamma", CGColor::new_generic_gray_gamma2_2(0.5, 0.8), "Generic Gray Gamma 2.2 Profile", vec![0.5, 0.8]),
        ("CMYK", CGColor::new_generic_cmyk(0.1, 0.2, 0.3, 0.4, 0.8), "Generic CMYK", vec![0.1, 0.2, 0.3, 0.4, 0.8]),
        (
            "clear",
            CGColor::constant_color(Some(unsafe { kCGColorClear })).unwrap(),
            "Generic Gray Gamma 2.2 Profile",
            vec![0.0, 0.0],
        ),
    ];
    for (name, cg, space, want) in back {
        let c = NSColor::colorWithCGColor(&cg).expect(name);
        assert_eq!(c.r#type(), NSColorType::ComponentBased, "{name}");
        assert_eq!(c.colorSpace().localizedName().map(|s| s.to_string()).as_deref(), Some(space), "{name}");
        let mut got = vec![0.0; c.numberOfComponents() as usize];
        // SAFETY: room for every component.
        unsafe { c.getComponents(NonNull::new(got.as_mut_ptr()).unwrap()) };
        assert_eq!(got, want, "{name}");
        assert_eq!(comps(&c.CGColor()), want, "{name}: round trip");
    }
}

fn graphics_contexts(_: MainThreadMarker) {
    let cg = cg_bitmap(4, 4);
    let unflipped = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, false);
    let flipped = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, true);
    assert!(std::ptr::eq(&*unflipped.CGContext(), &*cg) && std::ptr::eq(&*flipped.CGContext(), &*cg));
    assert!(!std::ptr::eq(&*unflipped, &*flipped), "a context a call");
    assert!(!unflipped.isFlipped() && flipped.isFlipped());
    // Flipping is a flag: the CTM stays, and AppKit's drawing goes where
    // CoreGraphics' would.
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&flipped));
    same_transform(CGContext::ctm(Some(&cg)), transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0));
    NSColor::redColor().setFill();
    NSBezierPath::fillRect(rect(0.0, 0.0, 1.0, 1.0));
    // One graphics state: saves and restores interleave.
    flipped.saveGraphicsState();
    CGContext::translate_ctm(Some(&cg), 1.0, 0.0);
    flipped.restoreGraphicsState();
    same_transform(CGContext::ctm(Some(&cg)), transform(1.0, 0.0, 0.0, 1.0, 0.0, 0.0));
    CGContext::save_g_state(Some(&cg));
    NSColor::blueColor().setFill();
    CGContext::restore_g_state(Some(&cg));
    NSBezierPath::fillRect(rect(1.0, 0.0, 1.0, 1.0));
    CGContext::set_rgb_fill_color(Some(&cg), 0.0, 1.0, 0.0, 1.0);
    NSBezierPath::fillRect(rect(2.0, 0.0, 1.0, 1.0));
    flipped.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    assert_eq!(cg_px(&cg, 0, 3), [255, 0, 0, 255]);
    assert_eq!(cg_px(&cg, 1, 3), [255, 0, 0, 255], "the fill came back with the restore");
    assert_eq!(cg_px(&cg, 2, 3), [0, 255, 0, 255], "CoreGraphics' fill color is AppKit's");
    assert_eq!(cg_px(&cg, 0, 0), [0; 4]);

    // A bitmap's context: its CGContext draws into the bitmap, in points.
    // SAFETY: NULL planes make the rep allocate; the name is a constant.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), 4, 4, 8, 4, true, false, NSDeviceRGBColorSpace, NSBitmapFormat::empty(), 0, 32)
    }
    .unwrap();
    rep.setSize(NSSize::new(2.0, 2.0));
    let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).unwrap();
    let a = ctx.CGContext();
    assert!(std::ptr::eq(&*a, &*ctx.CGContext()), "the same CGContext each time");
    same_transform(CGContext::ctm(Some(&a)), transform(2.0, 0.0, 0.0, 2.0, 0.0, 0.0));
    same_transform(CGContext::user_space_to_device_space_transform(Some(&a)), transform(2.0, 0.0, 0.0, -2.0, 0.0, 4.0));
    let clip = CGContext::clip_bounding_box(Some(&a));
    assert_eq!((clip.size.width, clip.size.height), (2.0, 2.0));
    assert_eq!(CGBitmapContextGetData(Some(&a)).cast::<u8>(), rep.bitmapData());
    assert_eq!(
        (CGBitmapContextGetWidth(Some(&a)), CGBitmapContextGetBytesPerRow(Some(&a))),
        (4, rep.bytesPerRow() as usize)
    );
    CGContext::set_rgb_fill_color(Some(&a), 0.0, 1.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&a), cg_rect(0.0, 0.0, 1.0, 1.0));
    ctx.flushGraphics();
    for (x, y, green) in [(0, 2, 255), (1, 3, 255), (2, 3, 0), (0, 1, 0)] {
        assert_eq!(pixel(&rep, x, y)[1], green, "({x}, {y})");
    }
}

fn drawing_in_views(mtm: MainThreadMarker) {
    for flipped in [false, true] {
        let seen: Rc<RefCell<Vec<String>>> = Rc::default();
        let log = seen.clone();
        let view = draw_view(mtm, rect(5.0, 7.0, 6.0, 4.0), flipped, move |_, _| {
            let ctx = NSGraphicsContext::currentContext().unwrap();
            let cg = ctx.CGContext();
            assert!(std::ptr::eq(&*cg, &*ctx.CGContext()));
            let m = CGContext::ctm(Some(&cg));
            let u = CGContext::user_space_to_device_space_transform(Some(&cg));
            let clip = CGContext::clip_bounding_box(Some(&cg));
            log.borrow_mut().push(format!(
                "{} {} {} {} {} | {} {} {} | {} {}",
                m.a,
                m.d,
                m.ty,
                m.b,
                m.c + 0.0,
                u.a,
                u.d,
                u.ty,
                clip.size.width,
                clip.size.height
            ));
            CGContext::set_rgb_fill_color(Some(&cg), 1.0, 0.0, 0.0, 1.0);
            CGContext::fill_rect(Some(&cg), cg_rect(0.0, 0.0, 2.0, 1.0));
            CGContext::save_g_state(Some(&cg));
            NSColor::blueColor().setFill();
            CGContext::fill_rect(Some(&cg), cg_rect(0.0, 2.0, 1.0, 1.0));
            CGContext::restore_g_state(Some(&cg));
            NSBezierPath::fillRect(rect(3.0, 0.0, 1.0, 1.0));
        });
        for scale in [1.0, 2.0] {
            let rep = snapshot(&view, scale);
            let s = scale as isize;
            // The view's origin is at its bottom (or top, flipped).
            let row = |y: isize| if flipped { y * s } else { (3 - y) * s };
            assert_px(&rep, 0, row(0), RED);
            assert_px(&rep, s, row(0), RED);
            assert_px(&rep, 2 * s, row(0), CLEAR);
            assert_px(&rep, 3 * s, row(0), RED);
            assert_px(&rep, 0, row(2), BLUE);
        }
        let want = if flipped {
            ["1 -1 4 0 0 | 1 1 0 | 6 4", "2 -2 8 0 0 | 2 2 0 | 6 4"]
        } else {
            ["1 1 0 0 0 | 1 -1 4 | 6 4", "2 2 0 0 0 | 2 -2 8 | 6 4"]
        };
        assert_eq!(*seen.borrow(), want, "flipped {flipped}");
    }
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
            CGBitmapInfo(1),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap()
}

fn image_bytes(image: &CGImage) -> Vec<u8> {
    CGDataProvider::data(CGImage::data_provider(Some(image)).as_deref()).unwrap().to_vec()
}

fn images(_: MainThreadMarker) {
    let img = test_image();
    let image = NSImage::initWithCGImage_size(NSImage::alloc(), &img, NSSize::ZERO);
    assert_eq!(image.size(), NSSize::new(3.0, 2.0), "no size: the pixels'");
    assert_eq!(image.representations().len(), 1);
    let sized = NSImage::initWithCGImage_size(NSImage::alloc(), &img, NSSize::new(6.0, 1.0));
    assert_eq!(sized.size(), NSSize::new(6.0, 1.0));
    // The image comes back as it went in.
    let back = unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }.unwrap();
    assert!(std::ptr::eq(&*back, &*img));
    let mut proposed = rect(0.0, 0.0, 6.0, 4.0);
    let back = unsafe { image.CGImageForProposedRect_context_hints(&mut proposed, None, None) }.unwrap();
    assert!(std::ptr::eq(&*back, &*img));
    assert_eq!(proposed, rect(0.0, 0.0, 6.0, 4.0));
    // It draws as its pixels.
    let rep = bitmap(3, 2);
    draw_in(&rep, |_| image.drawInRect(rect(0.0, 0.0, 3.0, 2.0)));
    assert_px(&rep, 0, 0, RED);
    assert_px(&rep, 2, 0, BLUE);
    assert_px(&rep, 0, 1, WHITE);

    // A bitmap of a CGImage's pixels.
    let brep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &img);
    assert_eq!((brep.pixelsWide(), brep.pixelsHigh(), brep.size()), (3, 2, NSSize::new(3.0, 2.0)));
    assert_eq!((brep.bitsPerSample(), brep.samplesPerPixel(), brep.bitsPerPixel(), brep.bytesPerRow()), (8, 4, 32, 12));
    assert!(brep.hasAlpha() && !brep.isPlanar() && brep.bitmapFormat() == NSBitmapFormat::empty());
    assert_eq!(brep.colorSpaceName().to_string(), "NSCalibratedRGBColorSpace");
    // SAFETY: the rep holds 24 bytes.
    let bytes = unsafe { std::slice::from_raw_parts(brep.bitmapData(), 24) }.to_vec();
    assert_eq!(bytes, image_bytes(&img));
    let cg = brep.CGImage().unwrap();
    assert_eq!(
        (CGImage::width(Some(&cg)), CGImage::bytes_per_row(Some(&cg)), CGImage::bitmap_info(Some(&cg)).0),
        (3, 12, 1)
    );
    assert_eq!(image_bytes(&cg), bytes);
    // Written through bitmapData, it makes a new image.
    unsafe { *brep.bitmapData() = 7 };
    let after = brep.CGImage().unwrap();
    assert_eq!(image_bytes(&after)[0], 7);

    // A bitmap's own image: its layout, its space, the same image until
    // its pixels change.
    // SAFETY: NULL planes make the rep allocate; the name is a constant.
    let own = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), 2, 1, 8, 4, true, false, NSDeviceRGBColorSpace, NSBitmapFormat::empty(), 0, 32)
    }
    .unwrap();
    unsafe {
        *own.bitmapData() = 200;
        *own.bitmapData().add(3) = 255;
    }
    let oimg = own.CGImage().unwrap();
    assert_eq!(
        (CGImage::width(Some(&oimg)), CGImage::height(Some(&oimg)), CGImage::bytes_per_row(Some(&oimg))),
        (2, 1, 32)
    );
    assert_eq!((CGImage::bitmap_info(Some(&oimg)).0, CGImage::bits_per_component(Some(&oimg))), (1, 8));
    let space = CGImage::color_space(Some(&oimg));
    assert_eq!(CGColorSpace::name(space.as_deref()).map(|n| n.to_string()).as_deref(), Some("kCGColorSpaceDeviceRGB"));
    assert_eq!(&image_bytes(&oimg)[..8], &[200, 0, 0, 255, 0, 0, 0, 0]);
    assert!(std::ptr::eq(&*oimg, &*own.CGImage().unwrap()));
    unsafe { *own.bitmapData() = 100 };
    assert_eq!(image_bytes(&own.CGImage().unwrap())[0], 100);
    // An image of a bitmap gives it as a CGImage.
    let im = NSImage::initWithSize(NSImage::alloc(), NSSize::new(2.0, 1.0));
    im.addRepresentation(&own);
    let ci = unsafe { im.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }.unwrap();
    assert_eq!((CGImage::width(Some(&ci)), CGImage::height(Some(&ci))), (2, 1));
}

fn elements(p: &NSBezierPath) -> Vec<(NSBezierPathElement, Vec<NSPoint>)> {
    (0..p.elementCount())
        .map(|i| {
            let mut pts = [NSPoint::new(0.0, 0.0); 3];
            // SAFETY: room for three points.
            let kind = unsafe { p.elementAtIndex_associatedPoints(i, pts.as_mut_ptr()) };
            let n = match kind {
                NSBezierPathElement::CubicCurveTo => 3,
                NSBezierPathElement::QuadraticCurveTo => 2,
                NSBezierPathElement::ClosePath => 0,
                _ => 1,
            };
            (kind, pts[..n].to_vec())
        })
        .collect()
}

fn cg_elements(p: &CGPath) -> Vec<(i32, Vec<CGPoint>)> {
    let mut out: Vec<(i32, Vec<CGPoint>)> = Vec::new();
    unsafe extern "C-unwind" fn f(info: *mut c_void, el: NonNull<CGPathElement>) {
        // SAFETY: the info is the list; the element lives through the call.
        let (out, el) = unsafe { (&mut *info.cast::<Vec<(i32, Vec<CGPoint>)>>(), el.as_ref()) };
        let n = match el.r#type {
            CGPathElementType::AddCurveToPoint => 3,
            CGPathElementType::AddQuadCurveToPoint => 2,
            CGPathElementType::CloseSubpath => 0,
            _ => 1,
        };
        // SAFETY: the element has `n` points.
        let pts = (0..n).map(|i| unsafe { *el.points.as_ptr().add(i) }).collect();
        out.push((el.r#type.0, pts));
    }
    // SAFETY: the applier reads the list it's given.
    unsafe { CGPath::apply(Some(p), (&mut out as *mut Vec<(i32, Vec<CGPoint>)>).cast(), Some(f)) };
    out
}

fn bezier_paths(_: MainThreadMarker) {
    let (m, l, q, c, z) = (0, 1, 2, 3, 4);
    let p = NSBezierPath::bezierPathWithRect(rect(1.0, 2.0, 3.0, 4.0));
    let kinds: Vec<i32> = cg_elements(&p.CGPath()).iter().map(|e| e.0).collect();
    assert_eq!(kinds, [m, l, l, l, z]);
    let oval = cg_elements(&NSBezierPath::bezierPathWithOvalInRect(rect(0.0, 0.0, 2.0, 2.0)).CGPath());
    assert_eq!(oval.iter().map(|e| e.0).collect::<Vec<_>>(), [m, c, c, c, c], "element for element: no close");
    assert!((oval[0].1[0].x - 1.707107).abs() < 1e-5);
    let p = NSBezierPath::bezierPath();
    p.moveToPoint(NSPoint::new(0.0, 0.0));
    p.curveToPoint_controlPoint(NSPoint::new(2.0, 0.0), NSPoint::new(1.0, 1.0));
    p.closePath();
    let els = cg_elements(&p.CGPath());
    assert_eq!(els.iter().map(|e| e.0).collect::<Vec<_>>(), [m, q, z, m]);
    assert_eq!(els[1].1, [CGPoint::new(1.0, 1.0), CGPoint::new(2.0, 0.0)]);
    // And back, element for element.
    let rounded = unsafe { CGPath::with_rounded_rect(cg_rect(0.0, 0.0, 10.0, 10.0), 2.0, 2.0, std::ptr::null()) };
    let from = NSBezierPath::bezierPathWithCGPath(&rounded);
    let kinds: Vec<NSBezierPathElement> = elements(&from).iter().map(|e| e.0).collect();
    use NSBezierPathElement as E;
    assert_eq!(
        kinds,
        [
            E::MoveTo,
            E::LineTo,
            E::CubicCurveTo,
            E::LineTo,
            E::CubicCurveTo,
            E::LineTo,
            E::CubicCurveTo,
            E::LineTo,
            E::CubicCurveTo,
            E::ClosePath
        ]
    );
    assert_eq!(elements(&from)[0].1, [NSPoint::new(10.0, 5.0)]);
    assert_eq!((from.lineWidth(), from.windingRule()), (1.0, NSWindingRule::NonZero));
    let m2 = CGMutablePath::new();
    unsafe {
        CGMutablePath::move_to_point(Some(&m2), std::ptr::null(), 0.0, 0.0);
        CGMutablePath::add_line_to_point(Some(&m2), std::ptr::null(), 1.0, 0.0);
    }
    CGMutablePath::close_subpath(Some(&m2));
    let closed = NSBezierPath::bezierPathWithCGPath(&m2);
    assert_eq!(
        elements(&closed).iter().map(|e| e.0).collect::<Vec<_>>(),
        [E::MoveTo, E::LineTo, E::ClosePath],
        "no move added"
    );
}

fn flipped_wrappers(mtm: MainThreadMarker) {
    // In an unflipped view, a flipped context wrapping the view's own
    // CGContext is flipped while it's current, and the view's context
    // isn't, before, during or after.
    let seen: Rc<RefCell<Vec<bool>>> = Rc::default();
    let log = seen.clone();
    let view = draw_view(mtm, rect(0.0, 0.0, 4.0, 4.0), false, move |_, _| {
        let cur = NSGraphicsContext::currentContext().unwrap();
        let before = cur.isFlipped();
        let cg = cur.CGContext();
        let wrapper = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, true);
        let made = cur.isFlipped();
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&wrapper));
        let inner = NSGraphicsContext::currentContext().unwrap().isFlipped();
        NSGraphicsContext::restoreGraphicsState_class();
        let current = NSGraphicsContext::currentContext().unwrap();
        log.borrow_mut().extend([before, made, inner, current.isFlipped(), std::ptr::eq(&*current, &*cur)]);
        // An image drawn respecting flippedness: upright but under the
        // flipped wrapper, which says the (unflipped) space is flipped.
        let data = [255, 0, 0, 255, 0, 0, 255, 255];
        let provider = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(&data))).unwrap();
        let img = unsafe {
            CGImage::new(
                1,
                2,
                8,
                32,
                4,
                Some(&srgb()),
                CGBitmapInfo(1),
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
        .unwrap();
        let image = NSImage::initWithCGImage_size(NSImage::alloc(), &img, NSSize::new(1.0, 2.0));
        let draw = |x: f64| unsafe {
            image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                rect(x, 0.0, 1.0, 2.0),
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
                true,
                None,
            )
        };
        draw(0.0);
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&wrapper));
        draw(2.0);
        NSGraphicsContext::restoreGraphicsState_class();
        draw(3.0);
    });
    let rep = snapshot(&view, 1.0);
    assert_eq!(*seen.borrow(), [false, false, true, false, true]);
    assert_px(&rep, 0, 2, RED);
    assert_px(&rep, 0, 3, BLUE);
    assert_px(&rep, 2, 2, BLUE);
    assert_px(&rep, 2, 3, RED);
    assert_px(&rep, 3, 2, RED);
    assert_px(&rep, 3, 3, BLUE);
}

/// A 1-row CGImage of `data`.
fn row_image(w: usize, bpc: usize, bpp: usize, space: &CGColorSpace, info: u32, data: &[u8]) -> CFRetained<CGImage> {
    let provider = CGDataProvider::with_cf_data(Some(&CFData::from_bytes(data))).unwrap();
    unsafe {
        CGImage::new(
            w,
            1,
            bpc,
            bpp,
            w * bpp / 8,
            Some(space),
            CGBitmapInfo(info),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .unwrap()
}

/// `image` drawn into a 1-row sRGB bitmap context.
fn cg_drawn(image: &CGImage) -> Vec<[u8; 4]> {
    let w = CGImage::width(Some(image));
    let c = cg_bitmap(w, 1);
    CGContext::draw_image(Some(&c), cg_rect(0.0, 0.0, w as f64, 1.0), Some(image));
    (0..w).map(|x| cg_px(&c, x, 0)).collect()
}

fn bitmaps_of_cg_images(_: MainThreadMarker) {
    let rep_bytes = |r: &NSBitmapImageRep, n: usize| unsafe { std::slice::from_raw_parts(r.bitmapData(), n) }.to_vec();
    let order32 = CGImageByteOrderInfo::Order32Little.0;
    // A bitmap keeps a CGImage's layout and bytes where it has them.
    let gray = CGColorSpace::new_device_gray().unwrap();
    let img = row_image(2, 8, 8, &gray, 0, &[60, 200]);
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &img);
    assert_eq!((rep.samplesPerPixel(), rep.bitsPerPixel(), rep.hasAlpha()), (1, 8, false));
    assert_eq!(rep.colorSpaceName().to_string(), "NSDeviceWhiteColorSpace");
    assert_eq!(rep_bytes(&rep, 2), [60, 200]);
    // And its CGImage is in that layout: a copy of it draws as it did.
    let back = rep.CGImage().unwrap();
    assert_eq!((CGImage::bits_per_pixel(Some(&back)), CGImage::bitmap_info(Some(&back)).0), (8, 0));
    let space = CGImage::color_space(Some(&back));
    assert_eq!(CGColorSpace::name(space.as_deref()).map(|n| n.to_string()).as_deref(), Some("kCGColorSpaceDeviceGray"));
    let copy = CGImage::new_copy(Some(&back)).unwrap();
    assert_eq!(cg_drawn(&copy), [[60, 60, 60, 255], [200, 200, 200, 255]]);
    // Straight alpha stays straight; BGRA is turned to ARGB.
    let straight = row_image(1, 8, 32, &srgb(), CGImageAlphaInfo::Last.0, &[200, 100, 50, 128]);
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &straight);
    assert_eq!(rep.bitmapFormat(), NSBitmapFormat::AlphaNonpremultiplied);
    assert_eq!(rep_bytes(&rep, 4), [200, 100, 50, 128]);
    assert_eq!(CGImage::alpha_info(rep.CGImage().as_deref()), CGImageAlphaInfo::Last);
    let bgra = row_image(1, 8, 32, &srgb(), CGImageAlphaInfo::PremultipliedFirst.0 | order32, &[10, 20, 30, 255]);
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &bgra);
    assert_eq!((rep.bitmapFormat(), rep_bytes(&rep, 4)), (NSBitmapFormat::AlphaFirst, vec![255, 30, 20, 10]));
    // A padding sample last: no alpha.
    let skip = row_image(1, 8, 32, &srgb(), CGImageAlphaInfo::NoneSkipLast.0, &[200, 100, 50, 7]);
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &skip);
    assert_eq!((rep.samplesPerPixel(), rep.bitsPerPixel(), rep.hasAlpha()), (3, 32, false));
    assert_eq!(CGImage::alpha_info(rep.CGImage().as_deref()), CGImageAlphaInfo::NoneSkipLast);
    // Colors survive the round trip through a bitmap, whatever the space.
    let p3 = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceDisplayP3 })).unwrap();
    let wide = row_image(1, 8, 32, &p3, CGImageAlphaInfo::PremultipliedLast.0, &[200, 100, 50, 255]);
    let direct = cg_drawn(&wide)[0];
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &wide);
    let via = cg_drawn(&rep.CGImage().unwrap())[0];
    assert!(direct.iter().zip(&via).all(|(a, b)| a.abs_diff(*b) <= 1), "{direct:?} then {via:?}");
}

fn colors_and_contexts(_: MainThreadMarker) {
    // A pattern of an image of nothing has no CoreGraphics color: black.
    // (Sidestep makes no pattern colors, so any pattern's is.)
    let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(2.0, 2.0));
    let pattern = NSColor::colorWithPatternImage(&img);
    let cg = pattern.CGColor();
    assert_eq!(comps(&cg), [0.0, 1.0]);
    assert_eq!(space_name(&cg).as_deref(), Some("kCGColorSpaceGenericGrayGamma2_2"));
    // White beyond 0 to 1 is extended gray.
    let bright = NSColor::colorWithWhite_alpha(1.5, 1.0).CGColor();
    assert_eq!((space_name(&bright).as_deref(), comps(&bright)), (Some("kCGColorSpaceExtendedGray"), vec![1.5, 1.0]));
    // Indexed colors make no NSColor.
    let table = [255u8, 0, 0, 0, 0, 255];
    let indexed = unsafe { CGColorSpace::new_indexed(Some(&srgb()), 1, table.as_ptr()) }.unwrap();
    let c = unsafe { CGColor::new(Some(&indexed), [1.0, 1.0].as_ptr()) }.unwrap();
    assert!(NSColor::colorWithCGColor(&c).is_none());
    // Calibrated colors are Generic RGB's; blends mix in it.
    let black = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0);
    let white = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 1.0);
    let mid = black.blendedColorWithFraction_ofColor(0.5, &white).unwrap();
    let cg = cg_bitmap(2, 1);
    let ns = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, false);
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&ns));
    NSColor::colorWithCalibratedRed_green_blue_alpha(0.5, 0.25, 0.75, 1.0).setFill();
    NSBezierPath::fillRect(rect(0.0, 0.0, 1.0, 1.0));
    mid.setFill();
    NSBezierPath::fillRect(rect(1.0, 0.0, 1.0, 1.0));
    // CoreGraphics' blend mode isn't AppKit's compositing operation.
    CGContext::set_blend_mode(Some(&cg), CGBlendMode::Multiply);
    assert_eq!(ns.compositingOperation(), NSCompositingOperation::SourceOver);
    ns.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    assert!(near(cg_px(&cg, 0, 0), [147, 90, 203, 255], 1), "{:?}", cg_px(&cg, 0, 0));
    assert!(near(cg_px(&cg, 1, 0), [146, 146, 146, 255], 1), "{:?}", cg_px(&cg, 1, 0));
    // AppKit's drawing takes CoreGraphics' current path (setting a color
    // doesn't): a rectangle fill drops it, a path's fill or stroke draws
    // it too (the path is added to it).
    type Draw = fn();
    let cases: [(&str, Draw, [u8; 4]); 3] = [
        ("fillRect", || NSBezierPath::fillRect(rect(2.0, 0.0, 1.0, 1.0)), [0, 0, 255, 0]),
        ("fill", || NSBezierPath::bezierPathWithRect(rect(2.0, 0.0, 1.0, 1.0)).fill(), [255, 0, 255, 0]),
        ("setFill", || NSColor::greenColor().setFill(), [0, 0, 0, 0]),
    ];
    for (name, draw, want) in cases {
        let cg = cg_bitmap(4, 1);
        let ns = NSGraphicsContext::graphicsContextWithCGContext_flipped(&cg, false);
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&ns));
        NSColor::redColor().setFill();
        CGContext::add_rect(Some(&cg), cg_rect(0.0, 0.0, 1.0, 1.0));
        draw();
        assert_eq!(CGContext::is_path_empty(Some(&cg)), name != "setFill", "{name}");
        ns.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        assert_eq!((0..4).map(|x| cg_px(&cg, x, 0)[3]).collect::<Vec<_>>(), want, "{name}");
    }
}

fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= tol)
}

fn set_cg_path(_: MainThreadMarker) {
    let cg = CGMutablePath::new();
    unsafe {
        CGMutablePath::move_to_point(Some(&cg), std::ptr::null(), 0.0, 0.0);
        CGMutablePath::add_line_to_point(Some(&cg), std::ptr::null(), 1.0, 0.0);
        CGMutablePath::add_line_to_point(Some(&cg), std::ptr::null(), 1.0, 1.0);
    }
    CGMutablePath::close_subpath(Some(&cg));
    let p = NSBezierPath::bezierPathWithRect(rect(5.0, 5.0, 1.0, 1.0));
    p.setCGPath(&cg);
    use NSBezierPathElement as E;
    let els = elements(&p);
    assert_eq!(els.iter().map(|e| e.0).collect::<Vec<_>>(), [E::MoveTo, E::LineTo, E::LineTo, E::ClosePath]);
    // What follows starts from the closed subpath's start.
    p.relativeLineToPoint(NSPoint::new(2.0, 0.0));
    assert_eq!(p.currentPoint(), NSPoint::new(2.0, 0.0));
    assert_eq!(elements(&p).last().map(|e| e.0), Some(E::LineTo));
}

/// A view's drawing in a window's display pass: CoreGraphics' CTM, the
/// user-to-device transform and the clip, for a view off the window's
/// corner. Needs a window (`SIDESTEP_CONFORMANCE_WINDOWS=1`); on macOS the
/// application is never activated and the window is ordered out after.
fn window_display(mtm: MainThreadMarker) {
    use objc2::MainThreadOnly;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow, NSWindowStyleMask,
    };
    NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(40.0, 40.0, 60.0, 40.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    /// A view's name and its CTM, user-to-device transform and clip.
    type Seen = Vec<(&'static str, [f64; 16])>;
    let seen: Rc<RefCell<Seen>> = Rc::default();
    let logger = |name: &'static str| {
        let log = seen.clone();
        move |_: &objc2_app_kit::NSView, _: NSRect| {
            let cg = NSGraphicsContext::currentContext().unwrap().CGContext();
            let (m, u, c) = (
                CGContext::ctm(Some(&cg)),
                CGContext::user_space_to_device_space_transform(Some(&cg)),
                CGContext::clip_bounding_box(Some(&cg)),
            );
            log.borrow_mut().push((
                name,
                [
                    m.a,
                    m.b,
                    m.c,
                    m.d,
                    m.tx,
                    m.ty,
                    u.a,
                    u.b,
                    u.c,
                    u.d,
                    u.tx,
                    u.ty,
                    c.origin.x,
                    c.origin.y,
                    c.size.width,
                    c.size.height,
                ],
            ));
        }
    };
    let view = draw_view(mtm, rect(5.0, 7.0, 10.0, 8.0), false, logger("plain"));
    let flipped = draw_view(mtm, rect(20.0, 3.0, 10.0, 8.0), true, logger("flipped"));
    let inner = draw_view(mtm, rect(2.0, 3.0, 4.0, 4.0), false, logger("inner"));
    view.addSubview(&inner);
    let content = draw_view(mtm, rect(0.0, 0.0, 60.0, 40.0), false, |_, _| {});
    window.setContentView(Some(&content));
    content.addSubview(&view);
    content.addSubview(&flipped);
    window.orderFrontRegardless();
    view.display();
    flipped.display();
    // Layer-backed views draw when the run loop turns.
    for _ in 0..20 {
        if seen.borrow().len() >= 3 {
            break;
        }
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        unsafe {
            objc2_foundation::NSRunLoop::currentRunLoop()
                .runMode_beforeDate(objc2_foundation::NSDefaultRunLoopMode, &until)
        };
    }
    let s = window.backingScaleFactor();
    window.orderOut(None);
    // Each view draws in a space of its own, as into a layer of its own:
    // the CTM starts out as the identity; device space is the layer's
    // pixels, turned over for a flipped view. (The clip, which macOS
    // leaves at the window's frame in a view's coordinates, isn't checked:
    // Sidestep's is the view's visible part.)
    let seen = seen.borrow();
    for name in ["plain", "flipped", "inner"] {
        let (_, v) = seen.iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("{name} drew"));
        let down = if name == "flipped" { -s } else { s };
        assert_eq!(v[..12], [1.0, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, 0.0, down, 0.0, 0.0], "{name}: {v:?}");
    }
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    #[cfg(not(target_vendor = "apple"))]
    if std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        // SAFETY: nothing else runs yet to read the environment.
        unsafe { std::env::set_var("SIDESTEP_BACKEND", "null") };
    }
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("colors", colors),
        ("graphics_contexts", graphics_contexts),
        ("drawing_in_views", drawing_in_views),
        ("images", images),
        ("bezier_paths", bezier_paths),
        ("flipped_wrappers", flipped_wrappers),
        ("bitmaps_of_cg_images", bitmaps_of_cg_images),
        ("colors_and_contexts", colors_and_contexts),
        ("set_cg_path", set_cg_path),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
    if std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_some() {
        objc2::rc::autoreleasepool(|_| window_display(mtm));
        println!("test window_display ... ok");
    }
}
