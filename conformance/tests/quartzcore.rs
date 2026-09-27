//! Core Animation, checked on macOS and on Linux alike: the layer model
//! (defaults, geometry, the tree, conversions, hit testing, key-value
//! coding, styles), timing functions and transforms, transactions,
//! implicit actions, presentation layers at chosen times, keyframes,
//! springs and groups, the layers AppKit gives views, and
//! `renderInContext:`'s pixels.
//!
//! Timing is never asserted against the wall clock: a layer whose speed is
//! 0 has the time its time offset says, so an animation added to one shows
//! at exactly the time a test sets (committing between, as the first
//! commit settles the animation's begin time). Implicit animations and
//! presentation layers need a layer committed into a window's layer tree,
//! so those tests use a window that is never shown.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.
// The C names of the transform functions, as programs call them.
#![allow(deprecated)]

mod common;

use std::cell::Cell;
use std::rc::Rc;

use block2::RcBlock;
use common::rect;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
use objc2_core_foundation::{CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGColor, CGColorSpace, CGContext,
    CGImageAlphaInfo, CGMutablePath, CGPath, kCGColorSpaceSRGB,
};
use objc2_foundation::{NSArray, NSDate, NSDictionary, NSNull, NSNumber, NSRunLoop, NSString, NSValue};
use objc2_quartz_core::*;

use sidestep as _;

type Test = (&'static str, fn(MainThreadMarker));

fn r(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

fn p(x: f64, y: f64) -> CGPoint {
    CGPoint::new(x, y)
}

#[track_caller]
fn close(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} is not {b} (within {tol})");
}

#[track_caller]
fn same_rect(a: CGRect, b: CGRect) {
    for (x, y) in [
        (a.origin.x, b.origin.x),
        (a.origin.y, b.origin.y),
        (a.size.width, b.size.width),
        (a.size.height, b.size.height),
    ] {
        assert!((x - y).abs() < 1e-6, "{a:?} is not {b:?}");
    }
}

fn keys(l: &CALayer) -> Vec<String> {
    l.animationKeys().map(|a| a.iter().map(|s| s.to_string()).collect()).unwrap_or_default()
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn num(v: f64) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(NSNumber::new_f64(v)) }
}

fn obj<T: objc2::Message>(o: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(o) }
}

fn cg_obj(c: &CGColor) -> &AnyObject {
    // SAFETY: CoreGraphics objects are objects.
    unsafe { &*(c as *const CGColor).cast::<AnyObject>() }
}

fn rgb(r: f64, g: f64, b: f64, a: f64) -> objc2_core_foundation::CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

/// Whether two objects are the same object.
fn same<A: objc2::Message, B: objc2::Message>(a: &A, b: &B) -> bool {
    std::ptr::eq((a as *const A).cast::<u8>(), (b as *const B).cast::<u8>())
}

fn description(o: Option<&AnyObject>) -> String {
    o.map_or("nil".into(), |o| {
        // SAFETY: -description returns a string.
        let d: Retained<NSString> = unsafe { msg_send![o, description] };
        d.to_string()
    })
}

/// Whether `f` raises (Sidestep panics where Apple raises).
fn raises(f: impl FnOnce()) -> bool {
    use std::panic::AssertUnwindSafe;
    #[cfg(target_vendor = "apple")]
    {
        objc2::exception::catch(AssertUnwindSafe(f)).is_err()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        std::panic::catch_unwind(AssertUnwindSafe(f)).is_err()
    }
}

/// A window (never shown) whose content view is layer-backed: layers in
/// its tree become live when committed. Not deferred: on macOS a deferred
/// window has no render context yet, so nothing in it goes live.
fn window(mtm: MainThreadMarker) -> (Retained<NSWindow>, Retained<CALayer>) {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    let content = w.contentView().expect("a content view");
    content.setWantsLayer(true);
    let root = content.layer().expect("a layer");
    CATransaction::flush();
    (w, root)
}

/// A layer in `root`'s tree, paused at time 0: animations added to it show
/// at the time `at` sets.
fn paused(root: &CALayer) -> Retained<CALayer> {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    let l = CALayer::new();
    l.setFrame(r(0.0, 0.0, 100.0, 100.0));
    l.setSpeed(0.0);
    l.setTimeOffset(0.0);
    root.addSublayer(&l);
    CATransaction::commit();
    CATransaction::flush();
    l
}

/// Add `anim` to a paused layer and commit it, so it begins at time 0.
fn add(l: &CALayer, anim: &CAAnimation, key: &str) {
    l.addAnimation_forKey(anim, Some(&ns(key)));
    CATransaction::flush();
}

/// The layer's presentation at time `t` of its paused clock.
fn at(l: &CALayer, t: f64) -> Retained<CALayer> {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    l.setTimeOffset(t);
    CATransaction::commit();
    CATransaction::flush();
    unsafe { l.presentationLayer() }.expect("a presentation layer")
}

fn basic(key: &str, from: Option<&AnyObject>, to: Option<&AnyObject>, dur: f64) -> Retained<CABasicAnimation> {
    let a = CABasicAnimation::animationWithKeyPath(Some(&ns(key)));
    unsafe {
        a.setFromValue(from);
        a.setToValue(to);
    }
    a.setDuration(dur);
    a
}

fn linear() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionLinear })
}

/// Wait on the run loop until `done` or a deadline (never asserting inside
/// a short window).
fn run_until(done: impl Fn() -> bool) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done() && std::time::Instant::now() < until {
        let date = NSDate::dateWithTimeIntervalSinceNow(0.01);
        NSRunLoop::currentRunLoop().runUntilDate(&date);
    }
}

// The model.

fn defaults(_: MainThreadMarker) {
    let l = CALayer::new();
    same_rect(l.bounds(), r(0.0, 0.0, 0.0, 0.0));
    assert_eq!((l.position(), l.anchorPoint(), l.anchorPointZ(), l.zPosition()), (p(0.0, 0.0), p(0.5, 0.5), 0.0, 0.0));
    assert!(!l.isHidden() && l.isDoubleSided() && !l.isGeometryFlipped() && !l.contentsAreFlipped());
    assert!(!l.masksToBounds() && !l.isOpaque() && !l.needsDisplayOnBoundsChange() && !l.drawsAsynchronously());
    same_rect(l.contentsRect(), r(0.0, 0.0, 1.0, 1.0));
    same_rect(l.contentsCenter(), r(0.0, 0.0, 1.0, 1.0));
    assert_eq!(l.contentsGravity().to_string(), "resize");
    assert_eq!(l.contentsScale(), 1.0);
    assert_eq!(l.contentsFormat().to_string(), "RGBA8");
    assert_eq!(
        (l.minificationFilter().to_string(), l.magnificationFilter().to_string()),
        ("linear".into(), "linear".into())
    );
    assert_eq!(l.edgeAntialiasingMask().0, 15);
    assert!(l.allowsEdgeAntialiasing() && l.allowsGroupOpacity() && !l.shouldRasterize());
    assert!(l.backgroundColor().is_none());
    assert_eq!((l.cornerRadius(), l.maskedCorners().0, l.cornerCurve().to_string()), (0.0, 15, "circular".into()));
    assert_eq!((l.borderWidth(), l.opacity(), l.rasterizationScale()), (0.0, 1.0, 1.0));
    let black = |c: Option<Retained<CGColor>>| {
        let c = c.expect("a color");
        assert_eq!(CGColor::alpha(Some(&c)), 1.0);
        assert!(description(Some(cg_obj(&c))).contains("0 0 0 1"), "{}", description(Some(cg_obj(&c))));
    };
    black(l.borderColor());
    black(l.shadowColor());
    assert_eq!((l.shadowOpacity(), l.shadowOffset(), l.shadowRadius()), (0.0, CGSize::new(0.0, -3.0), 3.0));
    assert!(l.shadowPath().is_none() && unsafe { l.contents() }.is_none());
    assert_eq!(l.autoresizingMask().0, 0);
    assert!(!l.needsDisplay() && !l.needsLayout());
    assert!(unsafe { l.sublayers() }.is_none() && l.superlayer().is_none() && l.name().is_none());
    assert!(unsafe { l.presentationLayer() }.is_none(), "a layer never committed has no presentation layer");
    assert_eq!(l.transform(), unsafe { CATransform3DIdentity });
    assert_eq!(l.preferredFrameSize(), CGSize::new(0.0, 0.0));
    // Timing.
    // A layer lasts forever.
    assert_eq!((l.beginTime(), l.duration(), l.speed(), l.timeOffset()), (0.0, f64::INFINITY, 1.0, 0.0));
    assert_eq!((l.repeatCount(), l.repeatDuration(), l.autoreverses()), (0.0, 0.0, false));
    assert_eq!(l.fillMode().to_string(), "removed");
    // The class's defaults.
    let d = |k: &str| description(unsafe { CALayer::defaultValueForKey(&ns(k)) }.as_deref());
    assert_eq!(d("opacity"), "1");
    assert_eq!(d("shadowRadius"), "3");
    assert_eq!(d("contentsScale"), "1");
    assert_eq!(d("hidden"), "0");
    for k in ["bounds", "position", "transform", "cornerRadius", "backgroundColor", "zPosition", "foo", "shadowOpacity"]
    {
        assert_eq!(d(k), "nil", "{k}");
    }
    let v = unsafe { CALayer::defaultValueForKey(&ns("anchorPoint")) }.expect("anchorPoint");
    let v: &NSValue = v.downcast_ref().expect("a value");
    assert_eq!(unsafe { v.pointValue() }, objc2_foundation::NSPoint::new(0.5, 0.5));
    for k in ["opacity", "bounds", "backgroundColor", "contents"] {
        assert!(!CALayer::needsDisplayForKey(&ns(k)), "{k}");
        assert!(CALayer::defaultActionForKey(&ns(k)).is_none(), "{k}");
    }
    close(CALayer::cornerCurveExpansionFactor(unsafe { kCACornerCurveCircular }), 1.0, 0.0);
    close(CALayer::cornerCurveExpansionFactor(unsafe { kCACornerCurveContinuous }), 1.528665, 1e-6);
}

fn constants(_: MainThreadMarker) {
    let all: [(&NSString, &str); 46] = unsafe {
        [
            (kCAFillModeForwards, "forwards"),
            (kCAFillModeBackwards, "backwards"),
            (kCAFillModeBoth, "both"),
            (kCAFillModeRemoved, "removed"),
            (kCAAnimationLinear, "linear"),
            (kCAAnimationDiscrete, "discrete"),
            (kCAAnimationPaced, "paced"),
            (kCAAnimationCubic, "cubic"),
            (kCAAnimationCubicPaced, "cubicPaced"),
            (kCAAnimationRotateAuto, "auto"),
            (kCAAnimationRotateAutoReverse, "autoReverse"),
            (kCATransitionFade, "fade"),
            (kCATransitionMoveIn, "moveIn"),
            (kCATransitionPush, "push"),
            (kCATransitionReveal, "reveal"),
            (kCATransitionFromRight, "fromRight"),
            (kCATransitionFromLeft, "fromLeft"),
            (kCATransitionFromTop, "fromTop"),
            (kCATransitionFromBottom, "fromBottom"),
            (kCATransactionAnimationDuration, "animationDuration"),
            (kCATransactionDisableActions, "disableActions"),
            (kCATransactionAnimationTimingFunction, "animationTimingFunction"),
            (kCATransactionCompletionBlock, "completionBlock"),
            (kCAOnOrderIn, "onOrderIn"),
            (kCAOnOrderOut, "onOrderOut"),
            (kCATransition, "transition"),
            (kCAGravityCenter, "center"),
            (kCAGravityResize, "resize"),
            (kCAGravityResizeAspect, "resizeAspect"),
            (kCAGravityResizeAspectFill, "resizeAspectFill"),
            (kCAGravityTopLeft, "topLeft"),
            (kCAFilterNearest, "nearest"),
            (kCAFilterLinear, "linear"),
            (kCAFilterTrilinear, "trilinear"),
            (kCACornerCurveCircular, "circular"),
            (kCACornerCurveContinuous, "continuous"),
            (kCAContentsFormatRGBA8Uint, "RGBA8"),
            (kCAContentsFormatRGBA16Float, "RGBAh"),
            (kCAContentsFormatGray8Uint, "Gray8"),
            (CAToneMapModeAutomatic, "automatic"),
            (CAToneMapModeNever, "never"),
            (CAToneMapModeIfSupported, "ifSupported"),
            (CADynamicRangeAutomatic, "automatic"),
            (CADynamicRangeStandard, "standard"),
            (CADynamicRangeConstrainedHigh, "constrainedHigh"),
            (CADynamicRangeHigh, "high"),
        ]
    };
    for (c, v) in all {
        assert_eq!(c.to_string(), v);
    }
    for (c, v) in unsafe {
        [
            (kCAMediaTimingFunctionLinear, "linear"),
            (kCAMediaTimingFunctionEaseIn, "easeIn"),
            (kCAMediaTimingFunctionEaseOut, "easeOut"),
            (kCAMediaTimingFunctionEaseInEaseOut, "easeInEaseOut"),
            (kCAMediaTimingFunctionDefault, "default"),
        ]
    } {
        assert_eq!(c.to_string(), v);
    }
    // CACurrentMediaTime is the time since boot, as the process info says.
    let a = CACurrentMediaTime();
    let up = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
    let b = CACurrentMediaTime();
    assert!(a <= up + 0.05 && up <= b + 0.05, "{a} {up} {b}");
}

fn geometry(_: MainThreadMarker) {
    let l = CALayer::new();
    l.setFrame(r(10.0, 20.0, 100.0, 50.0));
    same_rect(l.bounds(), r(0.0, 0.0, 100.0, 50.0));
    assert_eq!(l.position(), p(60.0, 45.0));
    l.setAnchorPoint(p(0.0, 0.0));
    same_rect(l.frame(), r(60.0, 45.0, 100.0, 50.0));
    l.setBounds(r(5.0, 5.0, 200.0, 100.0));
    same_rect(l.frame(), r(60.0, 45.0, 200.0, 100.0));
    l.setTransform(CATransform3DMakeScale(2.0, 3.0, 1.0));
    same_rect(l.frame(), r(60.0, 45.0, 400.0, 300.0));
    // A frame set through a transform sizes the bounds under it.
    l.setFrame(r(0.0, 0.0, 10.0, 10.0));
    assert_eq!(l.position(), p(0.0, 0.0));
    close(l.bounds().size.width, 5.0, 1e-9);
    close(l.bounds().size.height, 10.0 / 3.0, 1e-9);
    close(l.bounds().origin.x, 5.0, 0.0);
    l.setTransform(CATransform3DMakeRotation(0.5, 0.0, 0.0, 1.0));
    let f = l.frame();
    close(f.origin.x, -1.598085128680676, 1e-9);
    close(f.size.width, 5.98599793813254, 1e-9);
    close(f.size.height, 5.322402899322256, 1e-9);
    // Negative sizes are standardized.
    let m = CALayer::new();
    m.setFrame(r(10.0, 10.0, -20.0, -30.0));
    same_rect(m.bounds(), r(0.0, 0.0, 20.0, 30.0));
    assert_eq!(m.position(), p(0.0, -5.0));
    same_rect(m.frame(), r(-10.0, -20.0, 20.0, 30.0));
    // Affine transforms.
    let a = CALayer::new();
    let t = CGAffineTransform { a: 1.0, b: 2.0, c: 3.0, d: 4.0, tx: 5.0, ty: 6.0 };
    a.setAffineTransform(t);
    assert_eq!(a.affineTransform(), t);
    assert_eq!(a.transform().m21, 3.0);
}

fn conversions_and_hits(_: MainThreadMarker) {
    let a = CALayer::new();
    a.setFrame(r(0.0, 0.0, 300.0, 300.0));
    let b = CALayer::new();
    b.setFrame(r(50.0, 60.0, 100.0, 100.0));
    b.setBounds(r(10.0, 10.0, 100.0, 100.0));
    a.addSublayer(&b);
    assert_eq!(b.convertPoint_toLayer(p(0.0, 0.0), Some(&a)), p(40.0, 50.0));
    assert_eq!(a.convertPoint_fromLayer(p(0.0, 0.0), Some(&b)), p(40.0, 50.0));
    // A flipped layer's sublayers keep their coordinates in it.
    a.setGeometryFlipped(true);
    assert_eq!(b.convertPoint_toLayer(p(0.0, 0.0), Some(&a)), p(40.0, 50.0));
    a.setGeometryFlipped(false);
    b.setTransform(CATransform3DMakeScale(2.0, 2.0, 1.0));
    assert_eq!(b.convertPoint_toLayer(p(10.0, 10.0), Some(&a)), p(0.0, 10.0));
    assert_eq!(b.convertPoint_toLayer(p(60.0, 60.0), Some(&a)), p(100.0, 110.0));
    same_rect(b.convertRect_toLayer(r(10.0, 10.0, 10.0, 10.0), Some(&a)), r(0.0, 10.0, 20.0, 20.0));
    assert!(a.hitTest(p(100.0, 110.0)).is_some_and(|h| std::ptr::eq(&*h, &*b)));
    assert!(b.containsPoint(p(10.0, 10.0)) && !b.containsPoint(p(0.0, 0.0)));

    let a = CALayer::new();
    a.setBounds(r(0.0, 0.0, 100.0, 100.0));
    a.setPosition(p(50.0, 50.0));
    let b = CALayer::new();
    b.setFrame(r(80.0, 80.0, 50.0, 50.0));
    a.addSublayer(&b);
    // Outside the superlayer's bounds, a sublayer is hit unless the
    // superlayer masks.
    assert!(a.hitTest(p(120.0, 120.0)).is_some_and(|h| std::ptr::eq(&*h, &*b)));
    a.setMasksToBounds(true);
    assert!(a.hitTest(p(120.0, 120.0)).is_none());
    a.setMasksToBounds(false);
    // Hidden and transparent layers aren't hit.
    b.setHidden(true);
    assert!(a.hitTest(p(90.0, 90.0)).is_some_and(|h| std::ptr::eq(&*h, &*a)));
    b.setHidden(false);
    b.setOpacity(0.0);
    assert!(a.hitTest(p(90.0, 90.0)).is_some_and(|h| std::ptr::eq(&*h, &*a)));
    b.setOpacity(1.0);
    // A flipped layer turns the point it's given about its middle.
    a.setGeometryFlipped(true);
    assert!(a.hitTest(p(90.0, 90.0)).is_some_and(|h| std::ptr::eq(&*h, &*a)));
    assert!(a.hitTest(p(90.0, 15.0)).is_some_and(|h| std::ptr::eq(&*h, &*b)));
    // Times, between layers (with a speed, an offset, a begin time).
    let c = CALayer::new();
    let d = CALayer::new();
    c.addSublayer(&d);
    d.setSpeed(2.0);
    d.setTimeOffset(1.0);
    d.setBeginTime(3.0);
    close(d.convertTime_fromLayer(10.0, Some(&c)), 15.0, 1e-9);
    close(d.convertTime_toLayer(15.0, Some(&c)), 10.0, 1e-9);
    close(c.convertTime_toLayer(10.0, Some(&d)), 15.0, 1e-9);
}

fn tree(_: MainThreadMarker) {
    let p0 = CALayer::new();
    let (x, y, z) = (CALayer::new(), CALayer::new(), CALayer::new());
    let order = |l: &CALayer| -> Vec<usize> {
        unsafe { l.sublayers() }
            .map(|s| {
                s.iter()
                    .map(|q| {
                        if std::ptr::eq(&*q, &*x) {
                            0
                        } else if std::ptr::eq(&*q, &*y) {
                            1
                        } else {
                            2
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    p0.addSublayer(&x);
    p0.insertSublayer_atIndex(&y, 0);
    p0.insertSublayer_above(&z, Some(&y));
    assert_eq!(order(&p0), [1, 2, 0]);
    p0.insertSublayer_atIndex(&x, 100);
    assert_eq!(order(&p0), [1, 2, 0]);
    p0.insertSublayer_below(&x, None);
    assert_eq!(order(&p0), [0, 1, 2]);
    p0.insertSublayer_above(&x, None);
    assert_eq!(order(&p0), [1, 2, 0]);
    let q = CALayer::new();
    q.addSublayer(&x);
    assert_eq!(unsafe { p0.sublayers() }.expect("sublayers").count(), 2);
    assert!(x.superlayer().is_some_and(|s| std::ptr::eq(&*s, &*q)));
    unsafe { p0.replaceSublayer_with(&y, &x) };
    assert_eq!(unsafe { p0.sublayers() }.expect("sublayers").count(), 2);
    assert!(unsafe { q.sublayers() }.is_none(), "an emptied list is nil");
    assert!(y.superlayer().is_none());
    unsafe { p0.setSublayers(None) };
    assert!(unsafe { p0.sublayers() }.is_none() && x.superlayer().is_none());
    unsafe { p0.setSublayers(Some(&NSArray::<CALayer>::new())) };
    assert!(unsafe { p0.sublayers() }.is_none());
    p0.addSublayer(&x);
    x.removeFromSuperlayer();
    assert!(unsafe { p0.sublayers() }.is_none());
    // Layout and display flags.
    let n = CALayer::new();
    n.setNeedsDisplayOnBoundsChange(true);
    n.setBounds(r(0.0, 0.0, 10.0, 10.0));
    assert!(n.needsDisplay());
    n.displayIfNeeded();
    assert!(!n.needsDisplay());
    n.setNeedsDisplay();
    assert!(n.needsDisplay());
    n.display();
    assert!(!n.needsDisplay());
    n.setNeedsLayout();
    assert!(n.needsLayout());
    n.layoutIfNeeded();
    assert!(!n.needsLayout());
    let m = CALayer::new();
    m.addSublayer(&CALayer::new());
    assert!(m.needsLayout());
}

fn timing_functions(_: MainThreadMarker) {
    let points = |name: &NSString| -> Vec<[f32; 2]> {
        let f = CAMediaTimingFunction::functionWithName(name);
        (0..4usize)
            .map(|i| {
                let mut v = [0f32; 2];
                let ptr = v.as_mut_ptr();
                // SAFETY: the method writes two floats. (Sent without type
                // checking: objc2 would expect an array argument.)
                unsafe { send_control_point(&f, i, ptr) };
                v
            })
            .collect()
    };
    unsafe {
        assert_eq!(points(kCAMediaTimingFunctionLinear), [[0.0, 0.0], [0.0, 0.0], [1.0, 1.0], [1.0, 1.0]]);
        assert_eq!(points(kCAMediaTimingFunctionEaseIn), [[0.0, 0.0], [0.42, 0.0], [1.0, 1.0], [1.0, 1.0]]);
        assert_eq!(points(kCAMediaTimingFunctionEaseOut), [[0.0, 0.0], [0.0, 0.0], [0.58, 1.0], [1.0, 1.0]]);
        assert_eq!(points(kCAMediaTimingFunctionEaseInEaseOut), [[0.0, 0.0], [0.42, 0.0], [0.58, 1.0], [1.0, 1.0]]);
        assert_eq!(points(kCAMediaTimingFunctionDefault), [[0.0, 0.0], [0.25, 0.1], [0.25, 1.0], [1.0, 1.0]]);
        // One shared function per name.
        let a = CAMediaTimingFunction::functionWithName(kCAMediaTimingFunctionEaseOut);
        let b = CAMediaTimingFunction::functionWithName(kCAMediaTimingFunctionEaseOut);
        assert!(std::ptr::eq(&*a, &*b));
        assert_eq!(description(Some(&a)), "easeOut");
    }
    let f = CAMediaTimingFunction::functionWithControlPoints(0.1, 0.2, 0.3, 0.4);
    let g = CAMediaTimingFunction::functionWithControlPoints(0.1, 0.2, 0.3, 0.4);
    // Equal only to itself.
    assert!(!f.isEqual(Some(&g)) && f.isEqual(Some(&f)));
    assert!(raises(|| {
        let mut v = [0f32; 2];
        let ptr = v.as_mut_ptr();
        unsafe { send_control_point(&f, 5, ptr) };
    }));
}

/// `getControlPointAtIndex:values:`, whose array argument objc2's checks
/// would refuse a pointer for.
unsafe fn send_control_point(f: &CAMediaTimingFunction, index: usize, values: *mut f32) {
    let sel = sel!(getControlPointAtIndex:values:);
    let method = f.class().instance_method(sel).expect("the method");
    // SAFETY: as the caller promises: the method takes an index and a
    // pointer to two floats (an array argument decays to one).
    unsafe {
        let imp: unsafe extern "C-unwind" fn(&CAMediaTimingFunction, objc2::runtime::Sel, usize, *mut f32) =
            std::mem::transmute(method.implementation());
        imp(f, sel, index, values);
    }
}

fn transforms(_: MainThreadMarker) {
    let t = CATransform3DMakeScale(2.0, 3.0, 4.0);
    let rot = CATransform3DMakeRotation(0.5, 0.0, 0.0, 1.0);
    close(rot.m11, 0.8775825618903728, 1e-12);
    close(rot.m12, 0.479425538604203, 1e-12);
    close(rot.m21, -0.479425538604203, 1e-12);
    let rs = CATransform3DRotate(t, 0.5, 0.0, 0.0, 1.0);
    close(rs.m12, 1.438276615812609, 1e-12);
    close(rs.m21, -0.958851077208406, 1e-12);
    let tr = CATransform3DMakeTranslation(1.0, 2.0, 3.0);
    let c = CATransform3DConcat(t, tr);
    assert_eq!((c.m11, c.m41, c.m42, c.m43), (2.0, 1.0, 2.0, 3.0));
    let tt = CATransform3DTranslate(t, 1.0, 2.0, 3.0);
    assert_eq!((tt.m41, tt.m42, tt.m43), (2.0, 6.0, 12.0));
    let st = CATransform3DScale(tr, 2.0, 3.0, 4.0);
    assert_eq!((st.m11, st.m41), (2.0, 1.0));
    let inv = CATransform3DInvert(c);
    close(inv.m11, 0.5, 1e-12);
    close(inv.m41, -0.5, 1e-12);
    close(inv.m42, -0.6666666666666666, 1e-12);
    let sing = CATransform3DMakeScale(0.0, 1.0, 1.0);
    assert_eq!(CATransform3DInvert(sing), sing, "a singular transform inverts to itself");
    unsafe {
        assert!(CATransform3DIsIdentity(CATransform3DIdentity) && !CATransform3DIsIdentity(t));
    }
    assert!(CATransform3DIsAffine(rot) && !CATransform3DIsAffine(t));
    let aff = CGAffineTransform { a: 1.0, b: 2.0, c: 3.0, d: 4.0, tx: 5.0, ty: 6.0 };
    let m = CATransform3DMakeAffineTransform(aff);
    assert_eq!((m.m21, m.m41, m.m33), (3.0, 5.0, 1.0));
    assert_eq!(CATransform3DGetAffineTransform(m), aff);
    assert!(CATransform3DEqualToTransform(t, t) && !CATransform3DEqualToTransform(t, rot));
    let rx = CATransform3DMakeRotation(0.5, 2.0, 0.0, 0.0);
    close(rx.m23, 0.479425538604203, 1e-12);
    close(rx.m32, -0.479425538604203, 1e-12);
    unsafe {
        assert_eq!(CATransform3DMakeRotation(0.5, 0.0, 0.0, 0.0), CATransform3DIdentity);
    }
    let v = unsafe { NSValue::valueWithCATransform3D(t) };
    assert_eq!(unsafe { v.CATransform3DValue() }, t);
    let enc = unsafe { std::ffi::CStr::from_ptr(v.objCType().as_ptr()) }.to_str().unwrap_or_default().to_owned();
    assert!(enc.starts_with("{CATransform3D="), "{enc}");
}

fn key_value_coding(_: MainThreadMarker) {
    let l = CALayer::new();
    l.setFrame(r(0.0, 0.0, 100.0, 50.0));
    let get = |k: &str| -> String {
        let v: Option<Retained<AnyObject>> = unsafe { msg_send![&*l, valueForKeyPath: &*ns(k)] };
        description(v.as_deref())
    };
    assert_eq!(get("transform.scale"), "1");
    assert_eq!(get("transform.rotation.z"), "0");
    assert_eq!(get("transform.translation.x"), "0");
    assert_eq!(get("position.x"), "50");
    assert_eq!(get("bounds.size.width"), "100");
    assert_eq!(get("shadowOffset.height"), "-3");
    assert_eq!(get("anchorPoint.y"), "0.5");
    assert_eq!(get("opacity"), "1");
    assert_eq!(get("hidden"), "0");
    assert_eq!(get("contentsRect.size.width"), "1");
    assert_eq!(get("name"), "nil");
    assert_eq!(get("custom"), "nil");
    let set = |k: &str, v: f64| {
        let _: () = unsafe { msg_send![&*l, setValue: &*num(v), forKeyPath: &*ns(k)] };
    };
    set("transform.scale", 2.0);
    let t = l.transform();
    assert_eq!((t.m11, t.m22, t.m33), (2.0, 2.0, 2.0));
    set("transform.rotation.z", 0.5);
    let t = l.transform();
    close(t.m11, 1.7551651237807455, 1e-12);
    close(t.m12, 0.958851077208406, 1e-12);
    assert_eq!(get("transform.scale"), "2");
    close(get("transform.rotation.z").parse::<f64>().unwrap_or(0.0), 0.5, 1e-9);
    set("position.x", 7.0);
    assert_eq!(l.position(), p(7.0, 25.0));
    set("bounds.size.width", 9.0);
    same_rect(l.bounds(), r(0.0, 0.0, 9.0, 50.0));
    // Any other key is kept as given.
    let _: () = unsafe { msg_send![&*l, setValue: &*ns("v"), forKey: &*ns("custom")] };
    assert_eq!(get("custom"), "v");
    let _: () = unsafe { msg_send![&*l, setValue: &*num(0.25), forKey: &*ns("opacity")] };
    assert_eq!(l.opacity(), 0.25);
    // A style gives values to keys not set.
    let style = NSDictionary::from_retained_objects(&[&*ns("cornerRadius")], &[num(6.0)]);
    let fresh = CALayer::new();
    unsafe { fresh.setStyle(Some(&*(Retained::as_ptr(&style) as *const NSDictionary))) };
    assert_eq!(fresh.cornerRadius(), 6.0);
    // initWithLayer: copies nothing of a plain layer.
    l.setCornerRadius(5.0);
    let copy: Retained<CALayer> = unsafe { msg_send![CALayer::alloc(), initWithLayer: &*l] };
    assert_eq!(copy.cornerRadius(), 0.0);
}

fn animation_objects(_: MainThreadMarker) {
    let a = CABasicAnimation::animationWithKeyPath(Some(&ns("opacity")));
    assert!(a.class() == CABasicAnimation::class());
    assert_eq!((a.duration(), a.beginTime(), a.speed(), a.timeOffset()), (0.0, 0.0, 1.0, 0.0));
    assert_eq!((a.repeatCount(), a.repeatDuration(), a.autoreverses()), (0.0, 0.0, false));
    assert_eq!(a.fillMode().to_string(), "removed");
    assert!(a.isRemovedOnCompletion() && a.timingFunction().is_none() && a.delegate().is_none());
    assert!(!a.isAdditive() && !a.isCumulative());
    assert_eq!(a.keyPath().map(|k| k.to_string()), Some("opacity".into()));
    assert!(a.fromValue().is_none() && a.toValue().is_none() && a.byValue().is_none());
    let k = CAKeyframeAnimation::animationWithKeyPath(Some(&ns("position")));
    assert!(k.class() == CAKeyframeAnimation::class());
    assert_eq!(k.calculationMode().to_string(), "linear");
    assert!(k.rotationMode().is_none() && k.values().is_none() && k.keyTimes().is_none());
    let s = CASpringAnimation::animationWithKeyPath(Some(&ns("position")));
    assert!(s.class() == CASpringAnimation::class());
    assert_eq!((s.mass(), s.stiffness(), s.damping(), s.initialVelocity()), (1.0, 100.0, 10.0, 0.0));
    close(s.settlingDuration(), 1.4727003346780927, 1e-9);
    assert_eq!(s.duration(), 0.0);
    // Invalid values are refused.
    s.setMass(0.0);
    s.setStiffness(0.0);
    s.setDamping(-1.0);
    assert_eq!((s.mass(), s.stiffness(), s.damping()), (1.0, 100.0, 10.0));
    for (m, st, c, v, want) in [
        (1.0, 300.0, 20.0, 0.0, 0.7442555275721707),
        (2.0, 100.0, 5.0, 0.0, 5.658348137358159),
        (1.0, 100.0, 10.0, 5.0, 1.3815510557964275),
        (1.0, 100.0, 10.0, -5.0, 1.5350814063145797),
        (1.0, 100.0, 20.0, 0.0, 1.0),
        (1.0, 100.0, 30.0, 0.0, 1.0),
        (1.0, 400.0, 40.0, 0.0, 0.5),
        (1.0, 25.0, 10.0, 0.0, 1.9),
        (1.0, 100.0, 1.0, 0.0, 13.91321015404625),
    ] {
        s.setMass(m);
        s.setStiffness(st);
        s.setDamping(c);
        s.setInitialVelocity(v);
        close(s.settlingDuration(), want, 1e-6);
    }
    s.setDamping(0.0);
    assert_eq!(s.settlingDuration(), f32::MAX as f64);
    let t = CATransition::new();
    assert_eq!(t.r#type().to_string(), "fade");
    assert!(t.subtype().is_none());
    assert_eq!((t.startProgress(), t.endProgress()), (0.0, 1.0));
    let g = CAAnimationGroup::new();
    assert!(g.animations().is_none());
    let plain: Retained<CAAnimation> = CAAnimation::animation();
    assert!(plain.class() == CAAnimation::class());
    // Copies are distinct; any key is kept.
    let c: Retained<AnyObject> = unsafe { msg_send![&*a, copy] };
    assert!(!same(&*c, &*a));
    let _: () = unsafe { msg_send![&*a, setValue: &*ns("v"), forKey: &*ns("custom")] };
    let v: Option<Retained<AnyObject>> = unsafe { msg_send![&*a, valueForKey: &*ns("custom")] };
    assert_eq!(description(v.as_deref()), "v");
    let v: Option<Retained<AnyObject>> = unsafe { msg_send![&*a, valueForKey: &*ns("duration")] };
    assert_eq!(description(v.as_deref()), "0");
}

fn transactions(_: MainThreadMarker) {
    assert_eq!(CATransaction::animationDuration(), 0.25);
    assert!(!CATransaction::disableActions());
    assert!(CATransaction::animationTimingFunction().is_none());
    CATransaction::begin();
    CATransaction::setAnimationDuration(1.0);
    CATransaction::begin();
    assert_eq!(CATransaction::animationDuration(), 1.0, "a nested transaction starts with its parent's");
    CATransaction::setAnimationDuration(2.0);
    CATransaction::commit();
    assert_eq!(CATransaction::animationDuration(), 1.0);
    let v = CATransaction::valueForKey(&ns("animationDuration"));
    assert_eq!(description(v.as_deref()), "1");
    unsafe { CATransaction::setValue_forKey(Some(&ns("x")), &ns("custom")) };
    assert_eq!(description(CATransaction::valueForKey(&ns("custom")).as_deref()), "x");
    CATransaction::commit();
    assert_eq!(CATransaction::animationDuration(), 0.25);
    // A completion block with no animations runs after the commit, from
    // the run loop.
    let fired = Rc::new(Cell::new(false));
    let f = fired.clone();
    CATransaction::begin();
    let block = RcBlock::new(move || f.set(true));
    unsafe { CATransaction::setCompletionBlock(Some(&block)) };
    CATransaction::commit();
    assert!(!fired.get(), "not within the commit");
    run_until(|| fired.get());
    assert!(fired.get());
    unsafe {
        CATransaction::lock();
        CATransaction::lock();
        CATransaction::unlock();
        CATransaction::unlock();
    }
}

fn implicit_actions(mtm: MainThreadMarker) {
    // A layer never committed into a window's tree animates nothing.
    let lone = CALayer::new();
    lone.setOpacity(0.5);
    assert!(keys(&lone).is_empty());
    assert!(lone.actionForKey(&ns("opacity")).is_none());
    let (w, root) = window(mtm);
    let l = CALayer::new();
    root.addSublayer(&l);
    assert!(l.actionForKey(&ns("opacity")).is_none(), "not before it is committed");
    CATransaction::flush();
    assert!(unsafe { l.presentationLayer() }.is_some());
    // The default actions, measured.
    let action = |k: &str| -> String {
        match l.actionForKey(&ns(k)) {
            None => "nil".into(),
            Some(a) => {
                let o: &AnyObject = a.as_ref();
                o.class().name().to_str().unwrap_or_default().to_owned()
            }
        }
    };
    for k in [
        "bounds",
        "position",
        "zPosition",
        "anchorPoint",
        "anchorPointZ",
        "transform",
        "sublayerTransform",
        "contents",
        "contentsRect",
        "contentsCenter",
        "contentsScale",
        "cornerRadius",
        "borderWidth",
        "borderColor",
        "opacity",
        "shadowColor",
        "shadowOpacity",
        "shadowOffset",
        "shadowRadius",
        "transform.scale",
    ] {
        assert_eq!(action(k), "CABasicAnimation", "{k}");
    }
    for k in ["hidden", "geometryFlipped", "masksToBounds", "mask", "sublayers", "filters", "maskedCorners"] {
        assert_eq!(action(k), "CATransition", "{k}");
    }
    for k in
        ["doubleSided", "contentsGravity", "frame", "name", "onOrderIn", "position.x", "cornerCurve", "speed", "custom"]
    {
        assert_eq!(action(k), "nil", "{k}");
    }
    // No background color: nothing to animate from.
    assert_eq!(action("backgroundColor"), "nil");
    // The animation a change runs: from the value before, over the
    // transaction's duration, the default timing, filling backwards.
    l.setOpacity(0.3);
    assert_eq!(keys(&l), ["opacity"]);
    let an = unsafe { l.animationForKey(&ns("opacity")) }.expect("an animation");
    let an: Retained<CABasicAnimation> = unsafe { Retained::cast_unchecked(an) };
    assert_eq!(an.duration(), 0.25);
    assert_eq!(an.beginTime(), 0.0, "settled at the commit");
    assert_eq!(description(an.fromValue().as_deref()), "1");
    assert!(an.toValue().is_none());
    assert_eq!(an.fillMode().to_string(), "backwards");
    assert_eq!(description(an.timingFunction().as_deref().map(|f| f.as_ref())), "default");
    assert!(an.isRemovedOnCompletion());
    // Frozen once added.
    assert!(raises(|| an.setDuration(3.0)));
    CATransaction::flush();
    let an2 = unsafe { l.animationForKey(&ns("opacity")) }.expect("an animation");
    let now = CACurrentMediaTime();
    assert!(an2.beginTime() > 0.0 && an2.beginTime() <= now, "begins at the commit: {}", an2.beginTime());
    // A transaction's duration and timing function.
    l.removeAllAnimations();
    CATransaction::begin();
    CATransaction::setAnimationDuration(0.7);
    CATransaction::setAnimationTimingFunction(Some(&linear()));
    l.setCornerRadius(4.0);
    CATransaction::commit();
    let an = unsafe { l.animationForKey(&ns("cornerRadius")) }.expect("an animation");
    assert_eq!(an.duration(), 0.7);
    assert_eq!(description(an.timingFunction().as_deref().map(|f| f.as_ref())), "linear");
    // Disabled actions.
    l.removeAllAnimations();
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    l.setOpacity(0.1);
    CATransaction::commit();
    assert!(keys(&l).is_empty());
    // A frame animates position and bounds; hidden fades.
    l.setFrame(r(1.0, 2.0, 3.0, 4.0));
    assert_eq!(keys(&l), ["position", "bounds"]);
    l.removeAllAnimations();
    l.setHidden(true);
    assert_eq!(keys(&l), ["transition"]);
    l.removeAllAnimations();
    // A sublayer added fades its superlayer; one added and changed in the
    // same transaction doesn't animate.
    let s = CALayer::new();
    l.addSublayer(&s);
    assert_eq!(keys(&l), ["transition"]);
    s.setOpacity(0.2);
    assert!(keys(&s).is_empty());
    CATransaction::flush();
    s.setOpacity(0.4);
    assert_eq!(keys(&s), ["opacity"]);
    // NSNull in the actions stops one.
    s.removeAllAnimations();
    let actions = NSDictionary::from_retained_objects(&[&*ns("opacity")], &[obj(NSNull::null())]);
    s.setActions(Some(unsafe {
        &*(Retained::as_ptr(&actions) as *const NSDictionary<NSString, ProtocolObject<dyn CAAction>>)
    }));
    s.setOpacity(0.9);
    assert!(keys(&s).is_empty());
    w.close();
}

/// A change to an animation before it's added.
type Edit = Box<dyn Fn(&CABasicAnimation)>;

fn presentation_timing(mtm: MainThreadMarker) {
    let (w, root) = window(mtm);
    let tf = |n: &NSString| CAMediaTimingFunction::functionWithName(n);
    // From 0 to 1 over a second, by timing function (measured).
    for (f, want) in unsafe {
        [
            (None, [0.1, 0.25, 0.5, 0.75, 0.9]),
            (Some(tf(kCAMediaTimingFunctionEaseIn)), [0.017026613, 0.09346466, 0.31535682, 0.6218605, 0.83942777]),
            (Some(tf(kCAMediaTimingFunctionEaseOut)), [0.16057223, 0.37813953, 0.6846432, 0.9065353, 0.9829734]),
            (Some(tf(kCAMediaTimingFunctionEaseInEaseOut)), [0.019722449, 0.12916191, 0.5, 0.8708381, 0.98027754]),
            (Some(tf(kCAMediaTimingFunctionDefault)), [0.094795, 0.4085106, 0.8024034, 0.96045905, 0.99431646]),
        ]
    } {
        let l = paused(&root);
        let a = basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0);
        a.setTimingFunction(f.as_deref());
        add(&l, &a, "k");
        for (t, v) in [0.1, 0.25, 0.5, 0.75, 0.9].iter().zip(want) {
            close(at(&l, *t).opacity() as f64, v, 2e-3);
        }
        l.removeFromSuperlayer();
    }
    // Fill modes, with a begin time of 1 and a model value of 0.5.
    for (fill, want) in unsafe {
        [
            (kCAFillModeRemoved, [0.5, 0.5, 0.0, 0.5, 0.5]),
            (kCAFillModeForwards, [0.5, 0.5, 0.0, 0.5, 1.0]),
            (kCAFillModeBackwards, [0.0, 0.0, 0.0, 0.5, 0.5]),
            (kCAFillModeBoth, [0.0, 0.0, 0.0, 0.5, 1.0]),
        ]
    } {
        let l = paused(&root);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        l.setOpacity(0.5);
        CATransaction::commit();
        let a = basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0);
        a.setTimingFunction(Some(&linear()));
        a.setBeginTime(1.0);
        a.setFillMode(fill);
        a.setRemovedOnCompletion(false);
        add(&l, &a, "k");
        for (t, v) in [0.0, 0.5, 1.0, 1.5, 2.5].iter().zip(want) {
            close(at(&l, *t).opacity() as f64, v, 1e-3);
        }
        l.removeFromSuperlayer();
    }
    // Repeats, reversing, speed and offset (measured rows, model 0.5).
    let rows: Vec<(Edit, [f64; 10])> = vec![
        (Box::new(|a| a.setAutoreverses(true)), [0.0, 0.2, 0.5, 0.9, 0.8, 0.5, 0.1, 0.5, 0.5, 0.5]),
        (Box::new(|a| a.setRepeatCount(2.5)), [0.0, 0.2, 0.5, 0.9, 0.2, 0.5, 0.9, 0.1, 0.5, 0.5]),
        (Box::new(|a| a.setRepeatDuration(1.7)), [0.0, 0.2, 0.5, 0.9, 0.2, 0.5, 0.5, 0.5, 0.5, 0.5]),
        (Box::new(|a| a.setSpeed(2.0)), [0.0, 0.4, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]),
        (Box::new(|a| a.setTimeOffset(0.3)), [0.3, 0.5, 0.8, 0.2, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]),
        (Box::new(|a| a.setSpeed(-1.0)), [0.0, 0.8, 0.5, 0.1, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]),
        (Box::new(|a| a.setDuration(0.0)), [0.0, 0.8, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5]),
    ];
    for (set, want) in rows {
        let l = paused(&root);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        l.setOpacity(0.5);
        CATransaction::commit();
        let a = basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0);
        a.setTimingFunction(Some(&linear()));
        set(&a);
        a.setRemovedOnCompletion(false);
        add(&l, &a, "k");
        for (t, v) in [0.0, 0.2, 0.5, 0.9, 1.2, 1.5, 1.9, 2.1, 3.0, 4.5].iter().zip(want) {
            close(at(&l, *t).opacity() as f64, v, 1e-3);
        }
        l.removeFromSuperlayer();
    }
    // From, to and by, over a model value of 0.5.
    for (from, to, by, want) in [
        (Some(0.2), None, None, 0.35),
        (None, Some(0.8), None, 0.65),
        (None, None, Some(0.3), 0.65),
        (Some(0.2), None, Some(0.3), 0.35),
        (None, Some(0.9), Some(0.3), 0.75),
        (None, None, None, 0.5),
    ] {
        let l = paused(&root);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        l.setOpacity(0.5);
        CATransaction::commit();
        let a = CABasicAnimation::animationWithKeyPath(Some(&ns("opacity")));
        unsafe {
            a.setFromValue(from.map(num).as_deref());
            a.setToValue(to.map(num).as_deref());
            a.setByValue(by.map(num).as_deref());
        }
        a.setDuration(1.0);
        a.setTimingFunction(Some(&linear()));
        add(&l, &a, "k");
        close(at(&l, 0.5).opacity() as f64, want, 1e-3);
        l.removeFromSuperlayer();
    }
    // Additive and cumulative animations.
    let l = paused(&root);
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    l.setOpacity(0.5);
    CATransaction::commit();
    let a = basic("opacity", Some(&num(0.0)), Some(&num(0.2)), 1.0);
    a.setAdditive(true);
    a.setTimingFunction(Some(&linear()));
    add(&l, &a, "a");
    close(at(&l, 0.5).opacity() as f64, 0.6, 1e-3);
    // Back at 0, so the second begins with the first.
    at(&l, 0.0);
    let b = basic("opacity", Some(&num(0.0)), Some(&num(0.1)), 1.0);
    b.setAdditive(true);
    b.setTimingFunction(Some(&linear()));
    add(&l, &b, "b");
    close(at(&l, 0.5).opacity() as f64, 0.65, 1e-3);
    l.removeFromSuperlayer();
    let l = paused(&root);
    let a = basic("position.x", Some(&num(0.0)), Some(&num(10.0)), 1.0);
    a.setCumulative(true);
    a.setRepeatCount(3.0);
    a.setTimingFunction(Some(&linear()));
    add(&l, &a, "k");
    for (t, want) in [(0.5, 5.0), (1.5, 15.0), (2.5, 25.0)] {
        close(at(&l, t).position().x, want, 1e-3);
    }
    l.removeFromSuperlayer();
    // Key paths into structs and transforms.
    let l = paused(&root);
    add(&l, &basic("transform.scale", Some(&num(1.0)), Some(&num(3.0)), 1.0), "s");
    let pl = at(&l, 0.5);
    let t = pl.transform();
    close(t.m11, 2.0, 1e-3);
    close(t.m22, 2.0, 1e-3);
    l.removeFromSuperlayer();
    let l = paused(&root);
    add(&l, &basic("bounds.size.height", Some(&num(10.0)), Some(&num(30.0)), 1.0), "h");
    close(at(&l, 0.5).bounds().size.height, 20.0, 1e-3);
    l.removeFromSuperlayer();
    let l = paused(&root);
    add(&l, &basic("transform.rotation.z", Some(&num(0.0)), Some(&num(1.0)), 1.0), "r");
    let t = at(&l, 0.5).transform();
    close(t.m12, 0.5f64.sin(), 1e-3);
    l.removeFromSuperlayer();
    // Whole transforms interpolate taken apart: rotations by the shorter
    // way (270° as -90°), scale and rotation separately (measured, a
    // quarter of the way).
    let identity = unsafe { CATransform3DIdentity };
    for (from, to, want) in [
        (
            identity,
            CATransform3DMakeRotation(170f64.to_radians(), 0.0, 0.0, 1.0),
            [0.73728, 0.67559, 0.0, 0.0, -0.67559, 0.73728, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        ),
        (
            identity,
            CATransform3DMakeRotation(270f64.to_radians(), 0.0, 0.0, 1.0),
            [0.92388, -0.38268, 0.0, 0.0, 0.38268, 0.92388, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        ),
        (
            CATransform3DMakeScale(2.0, 1.0, 1.0),
            CATransform3DRotate(CATransform3DMakeTranslation(40.0, 0.0, 0.0), 1.0, 0.0, 0.0, 1.0),
            [1.69560, 0.43296, 0.0, 0.0, -0.24740, 0.96891, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 0.0, 0.0, 1.0],
        ),
        (
            identity,
            CATransform3DMakeRotation(1.2, 0.0, 1.0, 0.0),
            [0.95534, 0.0, -0.29552, 0.0, 0.0, 1.0, 0.0, 0.0, 0.29552, 0.0, 0.95534, 0.0, 0.0, 0.0, 0.0, 1.0],
        ),
    ] {
        let l = paused(&root);
        let a = basic(
            "transform",
            Some(&obj(unsafe { NSValue::valueWithCATransform3D(from) })),
            Some(&obj(unsafe { NSValue::valueWithCATransform3D(to) })),
            1.0,
        );
        a.setTimingFunction(Some(&linear()));
        add(&l, &a, "t");
        let m = at(&l, 0.25).transform();
        let got = [
            m.m11, m.m12, m.m13, m.m14, m.m21, m.m22, m.m23, m.m24, m.m31, m.m32, m.m33, m.m34, m.m41, m.m42, m.m43,
            m.m44,
        ];
        for (g, w) in got.iter().zip(want) {
            close(*g, w, 1e-4);
        }
        l.removeFromSuperlayer();
    }
    let l = paused(&root);
    let from = obj(unsafe { NSValue::valueWithPoint(objc2_foundation::NSPoint::new(0.0, 0.0)) });
    let to = obj(unsafe { NSValue::valueWithPoint(objc2_foundation::NSPoint::new(100.0, 50.0)) });
    add(&l, &basic("position", Some(&from), Some(&to), 1.0), "p");
    let pos = at(&l, 0.5).position();
    close(pos.x, 50.0, 1e-3);
    close(pos.y, 25.0, 1e-3);
    l.removeFromSuperlayer();
    // Colors interpolate by component.
    let l = paused(&root);
    let red = rgb(1.0, 0.0, 0.0, 1.0);
    let blue = rgb(0.0, 0.0, 1.0, 0.5);
    add(&l, &basic("backgroundColor", Some(cg_obj(&red)), Some(cg_obj(&blue)), 1.0), "c");
    let pl = at(&l, 0.5);
    let c = pl.backgroundColor().expect("a color");
    let comps = CGColor::components(Some(&c));
    let n = CGColor::number_of_components(Some(&c));
    assert_eq!(n, 4);
    let comps = unsafe { std::slice::from_raw_parts(comps, n) };
    for (got, want) in comps.iter().zip([0.5, 0.0, 0.5, 0.75]) {
        close(*got, want, 2e-3);
    }
    l.removeFromSuperlayer();
    // Two animations of one key: the later wins.
    let l = paused(&root);
    add(&l, &basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0), "a");
    add(&l, &basic("opacity", Some(&num(0.4)), Some(&num(0.4)), 1.0), "b");
    close(at(&l, 0.5).opacity() as f64, 0.4, 1e-3);
    // animationForKey: gives the same frozen copy each time.
    let a = basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0);
    add(&l, &a, "c");
    let got = unsafe { l.animationForKey(&ns("c")) }.expect("an animation");
    assert!(!same(&*got, &*a));
    assert!(std::ptr::eq(&*got, &*unsafe { l.animationForKey(&ns("c")) }.expect("an animation")));
    // Animations added without a key aren't listed.
    let t = CATransition::new();
    l.addAnimation_forKey(&t, None);
    assert!(keys(&l).contains(&"transition".to_string()));
    l.removeFromSuperlayer();
    // A presentation layer's own presentation layer is itself; its model
    // is the layer.
    let l = paused(&root);
    let pl = unsafe { l.presentationLayer() }.expect("a presentation layer");
    assert!(std::ptr::eq(&*unsafe { pl.modelLayer() }, &*l));
    assert!(unsafe { pl.presentationLayer() }.is_some_and(|q| std::ptr::eq(&*q, &*pl)));
    assert!(std::ptr::eq(&*unsafe { l.modelLayer() }, &*l));
    l.removeFromSuperlayer();
    w.close();
}

fn keyframes_springs_groups(mtm: MainThreadMarker) {
    let (w, root) = window(mtm);
    let times = [0.05, 0.2, 0.3, 0.4, 0.5, 0.7, 0.8, 0.9];
    let kt = NSArray::from_retained_slice(&[
        NSNumber::new_f64(0.0),
        NSNumber::new_f64(0.1),
        NSNumber::new_f64(0.6),
        NSNumber::new_f64(1.0),
    ]);
    for (mode, key_times, want) in unsafe {
        [
            (kCAAnimationLinear, false, [0.15, 0.6, 0.9, 0.9, 0.75, 0.52, 0.58, 0.64]),
            (kCAAnimationLinear, true, [0.5, 0.9, 0.8, 0.7, 0.6, 0.55, 0.6, 0.65]),
            (kCAAnimationDiscrete, false, [0.0, 0.0, 1.0, 1.0, 0.5, 0.5, 0.7, 0.7]),
            (kCAAnimationDiscrete, true, [0.0, 1.0, 1.0, 1.0, 1.0, 0.5, 0.5, 0.5]),
            (kCAAnimationPaced, false, [0.085, 0.34, 0.51, 0.68, 0.85, 0.81, 0.64, 0.53]),
            (kCAAnimationCubic, false, [0.16434374, 0.708, 0.96075, 0.9848, 0.8, 0.49165, 0.5296, 0.61795]),
            (kCAAnimationCubic, true, [0.6145833, 1.0066667, 0.9, 0.74, 0.58666664, 0.503125, 0.55833334, 0.634375]),
        ]
    } {
        let l = paused(&root);
        let k = CAKeyframeAnimation::animationWithKeyPath(Some(&ns("opacity")));
        let values = NSArray::from_retained_slice(&[num(0.0), num(1.0), num(0.5), num(0.7)]);
        unsafe { k.setValues(Some(&values)) };
        if key_times {
            k.setKeyTimes(Some(&kt));
        }
        k.setCalculationMode(mode);
        k.setDuration(1.0);
        add(&l, &k, "k");
        for (t, v) in times.iter().zip(want) {
            close(at(&l, *t).opacity() as f64, v, 2e-3);
        }
        l.removeFromSuperlayer();
    }
    // Along a path, a line then a curve: each element is a keyframe (a
    // curve followed by its parameter), paced by distance, or discrete at
    // the elements' ends (measured).
    let times = [0.0, 0.1, 0.25, 0.5, 0.6, 0.75, 0.9, 0.99];
    let path_kt =
        NSArray::from_retained_slice(&[NSNumber::new_f64(0.0), NSNumber::new_f64(0.2), NSNumber::new_f64(1.0)]);
    for (mode, key_times, want) in unsafe {
        [
            (
                kCAAnimationLinear,
                false,
                [
                    (0.0, 0.0),
                    (20.0, 0.0),
                    (50.0, 0.0),
                    (100.0, 0.0),
                    (129.6, 5.6),
                    (168.75, 31.25),
                    (194.4, 70.4),
                    (199.94, 97.0),
                ],
            ),
            (
                kCAAnimationLinear,
                true,
                [
                    (0.0, 0.0),
                    (50.0, 0.0),
                    (109.36, 0.57),
                    (153.61, 18.46),
                    (168.75, 31.25),
                    (186.88, 54.65),
                    (197.75, 81.35),
                    (199.98, 98.13),
                ],
            ),
            (
                kCAAnimationPaced,
                false,
                [
                    (0.0, 0.0),
                    (25.49, 0.0),
                    (63.72, 0.0),
                    (126.87, 4.62),
                    (149.72, 15.82),
                    (177.88, 41.5),
                    (195.99, 74.98),
                    (199.96, 97.46),
                ],
            ),
            (
                kCAAnimationDiscrete,
                false,
                [
                    (0.0, 0.0),
                    (0.0, 0.0),
                    (0.0, 0.0),
                    (100.0, 0.0),
                    (100.0, 0.0),
                    (100.0, 0.0),
                    (100.0, 0.0),
                    (100.0, 0.0),
                ],
            ),
        ]
    } {
        let l = paused(&root);
        let path = CGMutablePath::new();
        // SAFETY: a new path, no transform.
        unsafe {
            CGMutablePath::move_to_point(Some(&path), std::ptr::null(), 0.0, 0.0);
            CGMutablePath::add_line_to_point(Some(&path), std::ptr::null(), 100.0, 0.0);
            CGMutablePath::add_curve_to_point(Some(&path), std::ptr::null(), 150.0, 0.0, 200.0, 50.0, 200.0, 100.0);
        }
        let k = CAKeyframeAnimation::animationWithKeyPath(Some(&ns("position")));
        k.setPath(Some(&path));
        if key_times {
            k.setKeyTimes(Some(&path_kt));
        }
        k.setCalculationMode(mode);
        k.setDuration(1.0);
        add(&l, &k, "k");
        for (t, (x, y)) in times.iter().zip(want) {
            let p = at(&l, *t).position();
            close(p.x, x, 0.3);
            close(p.y, y, 0.3);
        }
        l.removeFromSuperlayer();
    }
    // Springs, from 0 to 100 on position.x (measured).
    for (m, st, c, v0, want) in [
        (1.0, 100.0, 10.0, 0.0, [10.44054701924324, 61.04925274848938, 102.33595371246338, 111.84459924697876]),
        (1.0, 100.0, 20.0, 0.0, [9.020400792360306, 44.217461347579956, 71.27025127410889, 93.89005303382874]),
        (1.0, 100.0, 30.0, 0.0, [9.020400792360306, 44.217461347579956, 71.27025127410889, 93.89005303382874]),
        (1.0, 100.0, 10.0, 5.0, [29.307806491851807, 87.320476770401, 116.04145765304565, 107.67215490341187]),
        (1.0, 100.0, 10.0, -5.0, [-8.426713198423386, 34.77803170681, 88.63046169281006, 116.01705551147461]),
    ] {
        let l = paused(&root);
        let s = CASpringAnimation::animationWithKeyPath(Some(&ns("position.x")));
        unsafe {
            s.setFromValue(Some(&num(0.0)));
            s.setToValue(Some(&num(100.0)));
        }
        s.setMass(m);
        s.setStiffness(st);
        s.setDamping(c);
        s.setInitialVelocity(v0);
        s.setDuration(s.settlingDuration());
        add(&l, &s, "k");
        for (t, v) in [0.05, 0.15, 0.25, 0.45].iter().zip(want) {
            close(at(&l, *t).position().x, v, 1e-2);
        }
        l.removeFromSuperlayer();
    }
    // A spring shorter than it settles, and one with a timing function.
    let l = paused(&root);
    let s = CASpringAnimation::animationWithKeyPath(Some(&ns("position.x")));
    unsafe {
        s.setFromValue(Some(&num(0.0)));
        s.setToValue(Some(&num(100.0)));
    }
    s.setDuration(0.5);
    add(&l, &s, "k");
    close(at(&l, 0.1).position().x, 34.02998447418213, 1e-2);
    close(at(&l, 0.49).position().x, 108.34387540817261, 1e-2);
    l.removeFromSuperlayer();
    // A group: children run in its time, one without a duration taking
    // the group's.
    let l = paused(&root);
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    l.setOpacity(0.5);
    CATransaction::commit();
    let a = basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0);
    a.setTimingFunction(Some(&linear()));
    a.setBeginTime(0.5);
    let b = basic("position.x", Some(&num(0.0)), Some(&num(100.0)), 0.0);
    b.setTimingFunction(Some(&linear()));
    let g = CAAnimationGroup::new();
    g.setAnimations(Some(&NSArray::from_retained_slice(&[
        Retained::into_super(Retained::into_super(a)),
        Retained::into_super(Retained::into_super(b)),
    ])));
    g.setDuration(2.0);
    add(&l, &g, "g");
    for (t, o, x) in [(0.25, 0.5, 12.5), (0.5, 0.0, 25.0), (1.0, 0.5, 50.0), (1.4, 0.9, 70.0), (1.6, 0.5, 80.0)] {
        let pl = at(&l, t);
        close(pl.opacity() as f64, o, 1e-3);
        close(pl.position().x, x, 1e-2);
    }
    l.removeFromSuperlayer();
    w.close();
}

// Views and their layers.

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "QuartzcoreFlippedView"]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

fn view_layers(mtm: MainThreadMarker) {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    let content = w.contentView().expect("a content view");
    assert!(!content.wantsLayer());
    assert!(content.layer().is_none(), "no layer until one is wanted");
    let sub = NSView::initWithFrame(NSView::alloc(mtm), rect(10.0, 20.0, 100.0, 50.0));
    content.addSubview(&sub);
    content.setWantsLayer(true);
    let cl = content.layer().expect("a layer");
    assert!(cl.delegate().is_some_and(|d| std::ptr::eq(
        Retained::as_ptr(&d).cast::<AnyObject>(),
        Retained::as_ptr(&content).cast::<AnyObject>()
    )));
    assert_eq!((cl.anchorPoint(), cl.position()), (p(0.0, 0.0), p(0.0, 0.0)));
    same_rect(cl.bounds(), r(0.0, 0.0, 400.0, 300.0));
    same_rect(cl.frame(), r(0.0, 0.0, 400.0, 300.0));
    assert!(!cl.isGeometryFlipped() && !cl.masksToBounds());
    // Subviews' layers come with the next commit.
    CATransaction::flush();
    let sl = sub.layer().expect("a subview's layer");
    assert!(!sub.wantsLayer());
    assert!(sl.superlayer().is_some_and(|s| std::ptr::eq(&*s, &*cl)));
    assert_eq!((sl.anchorPoint(), sl.position()), (p(0.0, 0.0), p(10.0, 20.0)));
    same_rect(sl.frame(), r(10.0, 20.0, 100.0, 50.0));
    // A view's layer doesn't animate implicitly.
    let action: Option<Retained<AnyObject>> =
        unsafe { msg_send![&*sub, actionForLayer: &*sl, forKey: &*ns("opacity")] };
    assert!(action.is_some_and(|a| a.class() == NSNull::class()));
    assert!(sl.actionForKey(&ns("opacity")).is_none());
    sl.setOpacity(0.5);
    assert!(keys(&sl).is_empty());
    assert_eq!(sub.alphaValue(), 1.0, "the layer doesn't change the view");
    // The view keeps its layer's geometry, visibility and opacity.
    sub.setFrame(rect(30.0, 40.0, 60.0, 70.0));
    same_rect(sl.frame(), r(30.0, 40.0, 60.0, 70.0));
    assert_eq!(sl.position(), p(30.0, 40.0));
    sub.setHidden(true);
    assert!(sl.isHidden());
    sub.setHidden(false);
    sub.setAlphaValue(0.25);
    assert_eq!(sl.opacity(), 0.25);
    sub.setBoundsOrigin(objc2_foundation::NSPoint::new(5.0, 6.0));
    same_rect(sl.bounds(), r(5.0, 6.0, 60.0, 70.0));
    sub.setBoundsOrigin(objc2_foundation::NSPoint::new(0.0, 0.0));
    // Sublayers the program added go below the subviews' layers when the
    // subviews change.
    let x = CALayer::new();
    cl.addSublayer(&x);
    let sub2 = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    content.addSubview(&sub2);
    CATransaction::flush();
    let order: Vec<&str> = unsafe { cl.sublayers() }
        .expect("sublayers")
        .iter()
        .map(|l| {
            if std::ptr::eq(&*l, &*x) {
                "app"
            } else if std::ptr::eq(&*l, &*sl) {
                "sub"
            } else if sub2.layer().is_some_and(|s| std::ptr::eq(&*s, &*l)) {
                "sub2"
            } else {
                "?"
            }
        })
        .collect();
    assert_eq!(order, ["app", "sub", "sub2"]);
    // A flipped view's layer is flipped; an unflipped view inside it is
    // flipped back.
    let f: Retained<FlippedView> =
        unsafe { msg_send![FlippedView::alloc(mtm), initWithFrame: rect(0.0, 100.0, 200.0, 150.0)] };
    content.addSubview(&f);
    let inner = NSView::initWithFrame(NSView::alloc(mtm), rect(10.0, 20.0, 30.0, 40.0));
    f.addSubview(&inner);
    CATransaction::flush();
    let (fl, il) = (f.layer().expect("a layer"), inner.layer().expect("a layer"));
    assert!(fl.isGeometryFlipped() && fl.contentsAreFlipped());
    assert!(il.isGeometryFlipped() && !il.contentsAreFlipped());
    same_rect(fl.frame(), r(0.0, 100.0, 200.0, 150.0));
    same_rect(il.frame(), r(10.0, 20.0, 30.0, 40.0));
    // A layer given to a view is hosted: kept, placed like a view's.
    let hv = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 50.0, 50.0));
    let hl = CALayer::new();
    hv.setLayer(Some(&hl));
    assert!(!hv.wantsLayer());
    assert!(hv.layer().is_some_and(|l| std::ptr::eq(&*l, &*hl)));
    assert_eq!(hl.anchorPoint(), p(0.0, 0.0));
    same_rect(hl.bounds(), r(0.0, 0.0, 50.0, 50.0));
    // A view outside a window has its layer at once; wanting none drops it.
    let lone = NSView::initWithFrame(NSView::alloc(mtm), rect(1.0, 2.0, 30.0, 40.0));
    lone.setWantsLayer(true);
    same_rect(lone.layer().expect("a layer").frame(), r(1.0, 2.0, 30.0, 40.0));
    lone.setWantsLayer(false);
    assert!(lone.layer().is_none());
    // A display link from a view.
    let link = unsafe { sub.displayLinkWithTarget_selector(&sub, sel!(description)) };
    assert!(!link.isPaused());
    link.invalidate();
    w.close();
}

// Pixels.

fn srgb() -> objc2_core_foundation::CFRetained<CGColorSpace> {
    CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB })).expect("sRGB")
}

/// A `w` × `h` bitmap context, 8-bit RGBA premultiplied, cleared.
fn context(w: usize, h: usize) -> objc2_core_foundation::CFRetained<CGContext> {
    unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, 0, Some(&srgb()), CGImageAlphaInfo::PremultipliedLast.0)
    }
    .expect("a bitmap context")
}

/// Pixel (`x`, `y`), row 0 the top.
fn px(c: &CGContext, x: usize, y: usize) -> [u8; 4] {
    let d = CGBitmapContextGetData(Some(c)) as *const u8;
    let bpr = CGBitmapContextGetBytesPerRow(Some(c));
    unsafe { std::ptr::read(d.add(y * bpr + x * 4).cast::<[u8; 4]>()) }
}

#[track_caller]
fn assert_px(c: &CGContext, x: usize, y: usize, want: [u8; 4], tol: u8) {
    let got = px(c, x, y);
    assert!(got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= tol), "pixel ({x}, {y}) is {got:?}, want {want:?}");
}

fn render_in_context(_: MainThreadMarker) {
    // A red square with a green sublayer, drawn in the layer's own space
    // (its position doesn't count): y runs up, from the bottom left.
    let l = CALayer::new();
    l.setBounds(r(0.0, 0.0, 40.0, 30.0));
    l.setPosition(p(500.0, 500.0));
    l.setBackgroundColor(Some(&rgb(1.0, 0.0, 0.0, 1.0)));
    let s = CALayer::new();
    s.setFrame(r(0.0, 0.0, 10.0, 10.0));
    s.setBackgroundColor(Some(&rgb(0.0, 1.0, 0.0, 1.0)));
    l.addSublayer(&s);
    let c = context(50, 50);
    l.renderInContext(&c);
    // Row 0 is the top: the layer covers rows 20 to 49.
    assert_px(&c, 20, 30, [255, 0, 0, 255], 0);
    assert_px(&c, 20, 10, [0, 0, 0, 0], 0);
    assert_px(&c, 5, 45, [0, 255, 0, 255], 0);
    assert_px(&c, 45, 45, [0, 0, 0, 0], 0);
    // A border inside the bounds, over the sublayers; opacity over all.
    s.setBorderWidth(2.0);
    s.setBorderColor(Some(&rgb(0.0, 0.0, 1.0, 1.0)));
    l.setOpacity(0.5);
    let c = context(50, 50);
    l.renderInContext(&c);
    assert_px(&c, 20, 30, [128, 0, 0, 128], 2);
    assert_px(&c, 5, 45, [0, 128, 0, 128], 2);
    assert_px(&c, 0, 45, [0, 0, 128, 128], 2);
    // Rounded corners and masking: the corner pixel is left out.
    let m = CALayer::new();
    m.setBounds(r(0.0, 0.0, 40.0, 40.0));
    m.setBackgroundColor(Some(&rgb(1.0, 0.0, 0.0, 1.0)));
    m.setCornerRadius(10.0);
    let big = CALayer::new();
    big.setFrame(r(-10.0, -10.0, 100.0, 100.0));
    big.setBackgroundColor(Some(&rgb(0.0, 0.0, 1.0, 1.0)));
    m.addSublayer(&big);
    let c = context(40, 40);
    m.renderInContext(&c);
    assert_px(&c, 20, 20, [0, 0, 255, 255], 0);
    assert_px(&c, 0, 0, [0, 0, 255, 255], 0);
    m.setMasksToBounds(true);
    let c = context(40, 40);
    m.renderInContext(&c);
    assert_px(&c, 0, 0, [0, 0, 0, 0], 0);
    assert_px(&c, 20, 20, [0, 0, 255, 255], 0);
    assert_px(&c, 39, 20, [0, 0, 255, 255], 0);
    // A sublayer's transform, about its anchor.
    let t = CALayer::new();
    t.setBounds(r(0.0, 0.0, 40.0, 40.0));
    let u = CALayer::new();
    u.setFrame(r(10.0, 10.0, 20.0, 20.0));
    u.setBackgroundColor(Some(&rgb(0.0, 0.0, 0.0, 1.0)));
    u.setTransform(CATransform3DMakeScale(0.5, 0.5, 1.0));
    t.addSublayer(&u);
    let c = context(40, 40);
    t.renderInContext(&c);
    assert_px(&c, 20, 20, [0, 0, 0, 255], 0);
    assert_px(&c, 13, 13, [0, 0, 0, 0], 0);
    assert_px(&c, 16, 24, [0, 0, 0, 255], 0);
    // Contents: an image drawn as the layer's gravity says.
    let img_ctx = context(2, 2);
    CGContext::set_rgb_fill_color(Some(&img_ctx), 0.0, 1.0, 0.0, 1.0);
    CGContext::fill_rect(Some(&img_ctx), r(0.0, 1.0, 2.0, 1.0));
    let image = objc2_core_graphics::CGBitmapContextCreateImage(Some(&img_ctx)).expect("an image");
    let k = CALayer::new();
    k.setBounds(r(0.0, 0.0, 20.0, 20.0));
    unsafe { k.setContents(Some(&*(objc2_core_foundation::CFRetained::as_ptr(&image).as_ptr() as *const AnyObject))) };
    k.setMagnificationFilter(unsafe { kCAFilterNearest });
    let c = context(20, 20);
    k.renderInContext(&c);
    // The image's top row (green) at the layer's top.
    assert_px(&c, 10, 2, [0, 255, 0, 255], 0);
    assert_px(&c, 10, 17, [0, 0, 0, 0], 0);
    // A layer that draws: drawInContext: through its delegate, recorded by
    // display.
    let d = CALayer::new();
    d.setBounds(r(0.0, 0.0, 20.0, 20.0));
    let delegate = Drawer::new();
    d.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    d.setNeedsDisplay();
    let c = context(20, 20);
    d.renderInContext(&c);
    assert!(delegate.ivars().drew.get() >= 1);
    // The delegate fills the bottom half (y up) blue.
    assert_px(&c, 10, 15, [0, 0, 255, 255], 0);
    assert_px(&c, 10, 5, [0, 0, 0, 0], 0);
    assert!(unsafe { d.contents() }.is_some(), "display left contents");
}

#[derive(Default)]
struct DrawerIvars {
    drew: Cell<u32>,
}

define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    #[ivars = DrawerIvars]
    #[name = "QuartzcoreDrawer"]
    struct Drawer;

    unsafe impl NSObjectProtocol for Drawer {}

    unsafe impl CALayerDelegate for Drawer {
        #[unsafe(method(drawLayer:inContext:))]
        fn draw_layer(&self, _layer: &CALayer, ctx: &CGContext) {
            self.ivars().drew.set(self.ivars().drew.get() + 1);
            CGContext::set_rgb_fill_color(Some(ctx), 0.0, 0.0, 1.0, 1.0);
            CGContext::fill_rect(Some(ctx), r(0.0, 0.0, 20.0, 10.0));
        }
    }
);

impl Drawer {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(DrawerIvars::default());
        unsafe { msg_send![super(this), init] }
    }
}

fn shape_layers(_: MainThreadMarker) {
    let s = CAShapeLayer::new();
    assert!(s.path().is_none());
    let fill = s.fillColor().expect("a fill color");
    assert!(description(Some(cg_obj(&fill))).contains("0 0 0 1"));
    assert!(s.strokeColor().is_none());
    assert_eq!((s.lineWidth(), s.miterLimit(), s.strokeStart(), s.strokeEnd()), (1.0, 10.0, 0.0, 1.0));
    assert_eq!(s.fillRule().to_string(), "non-zero");
    assert_eq!((s.lineCap().to_string(), s.lineJoin().to_string()), ("butt".into(), "miter".into()));
    assert!(s.lineDashPattern().is_none());
    // A filled rectangle path, stroked in blue.
    let path = unsafe { CGPath::with_rect(r(5.0, 5.0, 10.0, 10.0), std::ptr::null()) };
    s.setPath(Some(&path));
    s.setBounds(r(0.0, 0.0, 20.0, 20.0));
    s.setFillColor(Some(&rgb(1.0, 0.0, 0.0, 1.0)));
    let c = context(20, 20);
    s.renderInContext(&c);
    assert_px(&c, 10, 10, [255, 0, 0, 255], 0);
    assert_px(&c, 2, 2, [0, 0, 0, 0], 0);
    let g = CAGradientLayer::new();
    assert_eq!((g.startPoint(), g.endPoint()), (p(0.5, 0.0), p(0.5, 1.0)));
    assert_eq!(g.r#type().to_string(), "axial");
    assert!(g.colors().is_none() && g.locations().is_none());
    // From red at the start point (y 0, the bottom where y runs up) to
    // blue at the end.
    g.setBounds(r(0.0, 0.0, 20.0, 20.0));
    let colors = NSArray::from_slice(&[cg_obj(&rgb(1.0, 0.0, 0.0, 1.0)), cg_obj(&rgb(0.0, 0.0, 1.0, 1.0))]);
    unsafe { g.setColors(Some(&colors)) };
    let c = context(20, 20);
    g.renderInContext(&c);
    let bottom = px(&c, 10, 19);
    let top = px(&c, 10, 0);
    assert!(bottom[0] > 200 && bottom[2] < 40, "{bottom:?}");
    assert!(top[2] > 200 && top[0] < 40, "{top:?}");
}

// Measured details: the tree's errors and actions, key-value coding of
// CoreGraphics objects, transforms, value functions, keyframe edge cases,
// completion blocks and delegates, views, and pixels.

thread_local! {
    static ASKED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    static EVENTS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

define_class!(
    /// A layer noting the actions it's asked for, by its name.
    #[unsafe(super(CALayer, objc2::runtime::NSObject))]
    #[name = "QuartzcoreAskingLayer"]
    struct AskingLayer;

    impl AskingLayer {
        #[unsafe(method_id(actionForKey:))]
        fn action_for_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            let n = self.name().map(|n| n.to_string()).unwrap_or_default();
            ASKED.with(|a| a.borrow_mut().push(format!("{n}.{key}")));
            unsafe { msg_send![super(self), actionForKey: key] }
        }
    }
);

fn asking(name: &str) -> Retained<AskingLayer> {
    let l: Retained<AskingLayer> = unsafe { msg_send![AskingLayer::alloc(), init] };
    l.setName(Some(&ns(name)));
    l
}

/// The actions asked for since the last call, leaving out unnamed layers'.
fn asked() -> Vec<String> {
    ASKED.with(|a| std::mem::take(&mut *a.borrow_mut())).into_iter().filter(|k| !k.starts_with('.')).collect()
}

fn tree_details(mtm: MainThreadMarker) {
    // Programmer errors raise. (A cycle raises too, in a fresh process;
    // after other tests Core Animation may loop instead, so it isn't
    // tried here.)
    let a = CALayer::new();
    let b = CALayer::new();
    a.addSublayer(&b);
    let stranger = CALayer::new();
    assert!(raises(|| unsafe { a.replaceSublayer_with(&stranger, &CALayer::new()) }));
    assert!(raises(|| {
        CAMediaTimingFunction::functionWithName(&ns("bogus"));
    }));
    // Below a layer that isn't a sublayer: last.
    let z = CALayer::new();
    a.insertSublayer_below(&z, Some(&stranger));
    let subs = unsafe { a.sublayers() }.expect("sublayers");
    assert!(std::ptr::eq(&*subs.objectAtIndex(1), &*z));
    // A mask's superlayer is the layer it masks.
    let m = CALayer::new();
    a.addSublayer(&m);
    unsafe { a.setMask(Some(&m)) };
    assert!(m.superlayer().is_some_and(|s| std::ptr::eq(&*s, &*a)));
    assert_eq!(unsafe { a.sublayers() }.expect("sublayers").count(), 2);
    // Bounds are standardized; the edge mask keeps four bits.
    let l = CALayer::new();
    l.setBounds(r(5.0, 5.0, -10.0, -20.0));
    same_rect(l.bounds(), r(-5.0, -15.0, 10.0, 20.0));
    l.setEdgeAntialiasingMask(CAEdgeAntialiasingMask(0xff));
    assert_eq!(l.edgeAntialiasingMask().0, 15);
    l.setContentsGravity(&ns("bogus"));
    assert_eq!(l.contentsGravity().to_string(), "center");
    l.setCornerCurve(&ns("bogus"));
    assert_eq!(l.cornerCurve().to_string(), "circular");
    // The actions asked for as layers join and leave a tree (measured):
    // the superlayer's transition, then the layer's own.
    let (w, root) = window(mtm);
    let (pa, c, d, mk) = (asking("P"), asking("C"), asking("D"), asking("M"));
    root.addSublayer(&pa);
    CATransaction::flush();
    asked();
    pa.addSublayer(&c);
    assert_eq!(asked(), ["P.sublayers", "C.onOrderIn"]);
    // Laying a layer out asks for its onLayout action.
    CATransaction::flush();
    assert_eq!(asked(), ["P.onLayout"]);
    c.removeFromSuperlayer();
    assert_eq!(asked(), ["P.sublayers", "C.onOrderOut"]);
    pa.addSublayer(&c);
    CATransaction::flush();
    asked();
    unsafe { pa.replaceSublayer_with(&c, &d) };
    assert_eq!(asked(), ["P.sublayers", "C.onOrderOut", "D.onOrderIn"]);
    CATransaction::flush();
    asked();
    unsafe { pa.setSublayers(None) };
    assert_eq!(asked(), ["P.sublayers", "D.onOrderOut"]);
    unsafe { pa.setMask(Some(&mk)) };
    assert_eq!(asked(), ["P.mask", "M.onOrderIn"]);
    // Reordering asks only the superlayer.
    let (e, f) = (asking("E"), asking("F"));
    pa.addSublayer(&e);
    pa.addSublayer(&f);
    CATransaction::flush();
    asked();
    pa.insertSublayer_atIndex(&f, 0);
    assert_eq!(asked(), ["P.sublayers"]);
    // An onOrderIn or onOrderOut action runs.
    let g = CALayer::new();
    let appear = basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0);
    let actions = NSDictionary::from_retained_objects(&[&*ns("onOrderIn")], &[obj(appear)]);
    g.setActions(Some(unsafe {
        &*(Retained::as_ptr(&actions) as *const NSDictionary<NSString, ProtocolObject<dyn CAAction>>)
    }));
    root.addSublayer(&g);
    CATransaction::flush();
    assert_eq!(keys(&g), ["onOrderIn"]);
    g.removeFromSuperlayer();
    w.close();
}

fn kvc_details(_: MainThreadMarker) {
    let l = CALayer::new();
    let value = |k: &str| -> Option<Retained<AnyObject>> { unsafe { msg_send![&*l, valueForKey: &*ns(k)] } };
    let set = |v: Option<&AnyObject>, k: &str| {
        let _: () = unsafe { msg_send![&*l, setValue: v, forKey: &*ns(k)] };
    };
    // CoreGraphics objects by key.
    assert!(value("backgroundColor").is_none());
    let red = rgb(1.0, 0.0, 0.0, 1.0);
    set(Some(cg_obj(&red)), "backgroundColor");
    assert!(l.backgroundColor().is_some_and(|c| CGColor::equal_to_color(Some(&c), Some(&red))));
    assert!(value("backgroundColor").is_some());
    set(None, "backgroundColor");
    assert!(l.backgroundColor().is_none());
    assert!(value("borderColor").is_some(), "black by default");
    let path = unsafe { CGPath::with_rect(r(0.0, 0.0, 4.0, 4.0), std::ptr::null()) };
    set(
        Some(unsafe { &*(objc2_core_foundation::CFRetained::as_ptr(&path).as_ptr() as *const AnyObject) }),
        "shadowPath",
    );
    assert!(l.shadowPath().is_some());
    assert!(value("shadowPath").is_some());
    // A shape's key on a plain layer is kept as given.
    assert!(value("fillColor").is_none());
    let s = CAShapeLayer::new();
    let fill: Option<Retained<AnyObject>> = unsafe { msg_send![&*s, valueForKey: &*ns("fillColor")] };
    assert!(fill.is_some());
    let _: () = unsafe { msg_send![&*s, setValue: cg_obj(&red), forKey: &*ns("strokeColor")] };
    assert!(s.strokeColor().is_some());
    // A style's color.
    let red_obj: Retained<AnyObject> =
        unsafe { Retained::retain(cg_obj(&red) as *const AnyObject as *mut AnyObject) }.expect("a color");
    let style = NSDictionary::from_retained_objects(&[&*ns("backgroundColor")], &[red_obj]);
    let fresh = CALayer::new();
    unsafe { fresh.setStyle(Some(&*(Retained::as_ptr(&style) as *const NSDictionary))) };
    assert!(fresh.backgroundColor().is_some());
    // Nil is 0 for a number; a string's number counts.
    set(None, "opacity");
    assert_eq!(l.opacity(), 0.0);
    set(Some(&ns("0.5")), "opacity");
    assert_eq!(l.opacity(), 0.5);
    l.setFrame(r(1.0, 2.0, 3.0, 4.0));
    let f = value("frame").expect("a frame");
    let f: &NSValue = f.downcast_ref().expect("a value");
    let fr = unsafe { f.rectValue() };
    assert_eq!((fr.origin.x, fr.size.height), (1.0, 4.0));
    // Odd keys are kept.
    set(Some(&ns("v")), "größe");
    assert_eq!(description(value("größe").as_deref()), "v");
    // Defaults by key (measured).
    let d = |c: &objc2::runtime::AnyClass, k: &str| -> String {
        let v: Option<Retained<AnyObject>> = unsafe { msg_send![c, defaultValueForKey: &*ns(k)] };
        description(v.as_deref())
    };
    let layer = CALayer::class();
    for (k, want) in [
        ("contentsFormat", "RGBA8"),
        ("geometryFlipped", "0"),
        ("opaque", "0"),
        ("speed", "1"),
        ("duration", "inf"),
        ("fillMode", "removed"),
        ("contentsHeadroom", "0"),
        ("beginTime", "nil"),
    ] {
        assert_eq!(d(layer, k), want, "{k}");
    }
    let shape = CAShapeLayer::class();
    for (k, want) in [
        ("lineWidth", "1"),
        ("miterLimit", "10"),
        ("strokeEnd", "1"),
        ("strokeStart", "nil"),
        ("lineCap", "butt"),
        ("lineJoin", "miter"),
        ("fillRule", "non-zero"),
        ("opacity", "1"),
    ] {
        assert_eq!(d(shape, k), want, "{k}");
    }
    assert!(d(shape, "fillColor").contains("0 0 0 1"));
    let gradient = CAGradientLayer::class();
    assert_eq!(d(gradient, "type"), "axial");
    assert!(d(gradient, "endPoint").contains("{0.5, 1}"), "{}", d(gradient, "endPoint"));
    for (c, k, want) in [
        (CAAnimation::class(), "removedOnCompletion", "1"),
        (CAAnimation::class(), "speed", "1"),
        (CAAnimation::class(), "fillMode", "removed"),
        (CAAnimation::class(), "calculationMode", "linear"),
        (CAAnimation::class(), "type", "fade"),
        (CAAnimation::class(), "mass", "nil"),
        (CAAnimation::class(), "duration", "nil"),
        (CASpringAnimation::class(), "mass", "1"),
        (CASpringAnimation::class(), "stiffness", "100"),
        (CASpringAnimation::class(), "damping", "10"),
        (CASpringAnimation::class(), "speed", "1"),
    ] {
        assert_eq!(d(c, k), want, "{k}");
    }
}

fn timing_details(mtm: MainThreadMarker) {
    // cos 45° and sin 45°.
    const H: f64 = std::f64::consts::FRAC_1_SQRT_2;
    let (w, root) = window(mtm);
    let transform = |from: CATransform3D, to: CATransform3D, t: f64| -> [f64; 4] {
        let l = paused(&root);
        let a = basic(
            "transform",
            Some(&obj(unsafe { NSValue::valueWithCATransform3D(from) })),
            Some(&obj(unsafe { NSValue::valueWithCATransform3D(to) })),
            1.0,
        );
        a.setTimingFunction(Some(&linear()));
        add(&l, &a, "t");
        let m = at(&l, t).transform();
        l.removeFromSuperlayer();
        [m.m11, m.m12, m.m21, m.m22]
    };
    let identity = unsafe { CATransform3DIdentity };
    let shear =
        CATransform3DMakeAffineTransform(CGAffineTransform { a: 1.0, b: 0.0, c: 0.5, d: 1.0, tx: 0.0, ty: 0.0 });
    // Plane transforms interpolate as measured: a mirror flips one axis,
    // a half turn goes the negative way, a shear bends.
    for (from, to, t, want) in [
        (identity, CATransform3DMakeScale(-1.0, 1.0, 1.0), 0.25, [0.5, 0.0, 0.0, 1.0]),
        (identity, CATransform3DMakeScale(1.0, -1.0, 1.0), 0.25, [1.0, 0.0, 0.0, 0.5]),
        (identity, CATransform3DMakeScale(-1.0, -1.0, 1.0), 0.25, [H, -H, H, H]),
        (identity, CATransform3DMakeRotation(std::f64::consts::PI, 0.0, 0.0, 1.0), 0.5, [0.0, -1.0, 1.0, 0.0]),
        (identity, shear, 0.25, [1.0, 0.0, 0.1151, 1.0023]),
        (CATransform3DMakeScale(-1.0, 1.0, 1.0), CATransform3DMakeScale(1.0, -1.0, 1.0), 0.25, [-H, -H, -H, H]),
    ] {
        for (g, v) in transform(from, to, t).iter().zip(want) {
            close(*g, v, 2e-3);
        }
    }
    // Value functions: numbers made into transforms (three numbers for a
    // scale or translation).
    let vf = |name: &str, from: Retained<AnyObject>, to: Retained<AnyObject>, t: f64| -> CATransform3D {
        let l = paused(&root);
        let a = basic("transform", Some(&from), Some(&to), 1.0);
        a.setValueFunction(CAValueFunction::functionWithName(&ns(name)).as_deref());
        a.setTimingFunction(Some(&linear()));
        add(&l, &a, "v");
        let m = at(&l, t).transform();
        l.removeFromSuperlayer();
        m
    };
    let three = |v: [f64; 3]| obj(NSArray::from_retained_slice(&v.map(num)));
    let m = vf("rotateZ", num(0.0), num(std::f64::consts::PI), 0.25);
    close(m.m11, H, 1e-3);
    close(m.m12, H, 1e-3);
    let m = vf("translate", three([0.0, 0.0, 0.0]), three([10.0, 20.0, 0.0]), 0.5);
    close(m.m41, 5.0, 1e-6);
    close(m.m42, 10.0, 1e-6);
    let m = vf("scale", three([1.0, 1.0, 1.0]), three([3.0, 5.0, 7.0]), 0.5);
    for (got, want) in [(m.m11, 2.0), (m.m22, 3.0), (m.m33, 4.0)] {
        close(got, want, 1e-6);
    }
    let m = vf("translateY", num(0.0), num(10.0), 0.5);
    close(m.m42, 5.0, 1e-6);
    let m = vf("rotateX", num(0.0), num(1.0), 0.5);
    close(m.m23, 0.5f64.sin(), 1e-3);
    // Over the model's transform, which it replaces.
    let l = paused(&root);
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    l.setTransform(CATransform3DMakeScale(2.0, 2.0, 1.0));
    CATransaction::commit();
    let a = basic("transform", Some(&num(0.0)), Some(&num(1.0)), 1.0);
    a.setValueFunction(CAValueFunction::functionWithName(&ns("rotateZ")).as_deref());
    a.setTimingFunction(Some(&linear()));
    add(&l, &a, "v");
    close(at(&l, 0.5).transform().m11, 0.5f64.cos(), 1e-3);
    l.removeFromSuperlayer();
    // Keyframes and repeats through one.
    let l = paused(&root);
    let k = CAKeyframeAnimation::animationWithKeyPath(Some(&ns("transform")));
    unsafe { k.setValues(Some(&NSArray::from_retained_slice(&[num(0.0), num(1.0), num(3.0)]))) };
    k.setValueFunction(CAValueFunction::functionWithName(&ns("rotateZ")).as_deref());
    k.setDuration(1.0);
    add(&l, &k, "k");
    close(at(&l, 0.75).transform().m11, 2f64.cos(), 1e-3);
    l.removeFromSuperlayer();
    let l = paused(&root);
    let a = basic("transform", Some(&num(0.0)), Some(&num(1.0)), 1.0);
    a.setValueFunction(CAValueFunction::functionWithName(&ns("translateX")).as_deref());
    a.setCumulative(true);
    a.setRepeatCount(3.0);
    a.setTimingFunction(Some(&linear()));
    add(&l, &a, "c");
    close(at(&l, 2.5).transform().m41, 2.5, 1e-3);
    l.removeFromSuperlayer();
    // Keyframe edge cases: key times of another count take the common
    // prefix; discrete takes one key time more; one value does nothing.
    let keyframed = |values: &[f64], times: Option<&[f64]>, mode: &NSString, t: &[f64]| -> Vec<f64> {
        let l = paused(&root);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        l.setOpacity(0.5);
        CATransaction::commit();
        let k = CAKeyframeAnimation::animationWithKeyPath(Some(&ns("opacity")));
        unsafe {
            k.setValues(Some(&NSArray::from_retained_slice(&values.iter().map(|v| num(*v)).collect::<Vec<_>>())))
        };
        if let Some(times) = times {
            k.setKeyTimes(Some(&NSArray::from_retained_slice(
                &times.iter().map(|v| NSNumber::new_f64(*v)).collect::<Vec<_>>(),
            )));
        }
        k.setCalculationMode(mode);
        k.setDuration(1.0);
        add(&l, &k, "k");
        let out = t.iter().map(|t| at(&l, *t).opacity() as f64).collect();
        l.removeFromSuperlayer();
        out
    };
    let lin = unsafe { kCAAnimationLinear };
    let disc = unsafe { kCAAnimationDiscrete };
    for (got, want) in keyframed(&[0.0, 1.0, 0.5, 0.7], Some(&[0.0, 0.5, 1.0]), lin, &[0.1, 0.25, 0.4, 0.6, 0.75, 0.9])
        .iter()
        .zip([0.2, 0.5, 0.8, 0.9, 0.75, 0.6])
    {
        close(*got, want, 1e-3);
    }
    for (got, want) in keyframed(&[0.0, 1.0, 0.3], Some(&[0.0, 0.3, 0.6, 1.0]), disc, &[0.1, 0.35, 0.65, 0.95])
        .iter()
        .zip([0.0, 1.0, 0.3, 0.3])
    {
        close(*got, want, 1e-3);
    }
    for got in keyframed(&[0.3], None, lin, &[0.1, 0.9]) {
        close(got, 0.5, 1e-3);
    }
    // Repeats add up the end value, not going back and forth.
    let l = paused(&root);
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    l.setPosition(p(100.0, 0.0));
    CATransaction::commit();
    let a = CABasicAnimation::animationWithKeyPath(Some(&ns("position.x")));
    unsafe { a.setByValue(Some(&num(10.0))) };
    a.setCumulative(true);
    a.setRepeatCount(3.0);
    a.setDuration(1.0);
    a.setTimingFunction(Some(&linear()));
    add(&l, &a, "c");
    for (t, want) in [(0.5, 105.0), (1.5, 215.0), (2.5, 325.0)] {
        close(at(&l, t).position().x, want, 1e-3);
    }
    l.removeFromSuperlayer();
    let l = paused(&root);
    let a = basic("position.x", Some(&num(0.0)), Some(&num(10.0)), 1.0);
    a.setCumulative(true);
    a.setAutoreverses(true);
    a.setRepeatCount(2.0);
    a.setTimingFunction(Some(&linear()));
    add(&l, &a, "c");
    close(at(&l, 2.5).position().x, 5.0, 1e-3);
    l.removeFromSuperlayer();
    // A BOOL switches as soon as it starts; no color at one end shows none.
    let l = paused(&root);
    let a = basic("hidden", Some(&num(0.0)), Some(&num(1.0)), 1.0);
    a.setTimingFunction(Some(&linear()));
    add(&l, &a, "h");
    assert!(at(&l, 0.25).isHidden());
    l.removeFromSuperlayer();
    let l = paused(&root);
    let a = CABasicAnimation::animationWithKeyPath(Some(&ns("backgroundColor")));
    unsafe { a.setToValue(Some(cg_obj(&rgb(1.0, 0.0, 0.0, 1.0)))) };
    a.setDuration(1.0);
    add(&l, &a, "c");
    assert!(at(&l, 0.5).backgroundColor().is_none());
    l.removeFromSuperlayer();
    // Presentation layers: read only, with their model's animations, and
    // showing what was committed.
    let l = paused(&root);
    add(&l, &basic("opacity", Some(&num(0.0)), Some(&num(1.0)), 1.0), "k");
    let pl = at(&l, 0.5);
    assert!(raises(|| pl.setOpacity(0.1)));
    assert_eq!(keys(&pl), ["k"]);
    l.removeFromSuperlayer();
    let y = CALayer::new();
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    root.addSublayer(&y);
    CATransaction::commit();
    CATransaction::flush();
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    y.setOpacity(0.2);
    assert_eq!(unsafe { y.presentationLayer() }.expect("a presentation layer").opacity(), 1.0, "not committed");
    CATransaction::commit();
    assert_eq!(unsafe { y.presentationLayer() }.expect("a presentation layer").opacity(), 0.2);
    y.setOpacity(0.7);
    assert_eq!(unsafe { y.presentationLayer() }.expect("a presentation layer").opacity(), 0.2);
    y.removeFromSuperlayer();
    // A perceptual spring: its duration the settling one, overdamping
    // allowed.
    let s: Retained<CASpringAnimation> =
        CASpringAnimation::initWithPerceptualDuration_bounce(CASpringAnimation::alloc(), 0.5, 0.3);
    assert!(s.allowsOverdamping());
    close(s.duration(), s.settlingDuration(), 1e-9);
    close(s.duration(), 0.863, 1e-3);
    // Keys set on an animation once frozen are kept.
    let (layer_k, anim) = (paused(&root), basic("opacity", None, None, 1.0));
    add(&layer_k, &anim, "k");
    let frozen = unsafe { layer_k.animationForKey(&ns("k")) }.expect("an animation");
    let _: () = unsafe { msg_send![&*frozen, setValue: &*ns("v"), forKey: &*ns("tag")] };
    layer_k.removeFromSuperlayer();
    // Sublayer transforms turn about the anchor point (measured).
    let a = CALayer::new();
    a.setBounds(r(0.0, 0.0, 100.0, 100.0));
    a.setAnchorPoint(p(0.0, 0.0));
    let b = CALayer::new();
    b.setFrame(r(10.0, 10.0, 20.0, 20.0));
    a.addSublayer(&b);
    a.setSublayerTransform(CATransform3DMakeScale(2.0, 2.0, 1.0));
    assert_eq!(b.convertPoint_toLayer(p(0.0, 0.0), Some(&a)), p(20.0, 20.0));
    a.setBounds(r(5.0, 5.0, 100.0, 100.0));
    assert_eq!(b.convertPoint_toLayer(p(0.0, 0.0), Some(&a)), p(15.0, 15.0));
    // Nothing shows (or is hit) outside a mask.
    let host = CALayer::new();
    host.setFrame(r(0.0, 0.0, 100.0, 100.0));
    let mask = CALayer::new();
    mask.setFrame(r(0.0, 0.0, 40.0, 40.0));
    unsafe { host.setMask(Some(&mask)) };
    let holder = CALayer::new();
    holder.setFrame(r(0.0, 0.0, 100.0, 100.0));
    holder.addSublayer(&host);
    assert!(holder.hitTest(p(50.0, 50.0)).is_some_and(|h| std::ptr::eq(&*h, &*holder)));
    assert!(holder.hitTest(p(20.0, 20.0)).is_some_and(|h| std::ptr::eq(&*h, &*host)));
    // A paused layer's time for the root's is 0.
    let paused_layer = CALayer::new();
    paused_layer.setSpeed(0.0);
    assert_eq!(paused_layer.convertTime_toLayer(10.0, None), 0.0);
    w.close();
}

define_class!(
    /// An animation delegate noting starts and stops, by the animation's
    /// `tag`.
    #[unsafe(super(objc2::runtime::NSObject))]
    #[name = "QuartzcoreAnimationWatcher"]
    struct Watcher;

    unsafe impl NSObjectProtocol for Watcher {}

    impl Watcher {
        #[unsafe(method(animationDidStart:))]
        fn did_start(&self, anim: &CAAnimation) {
            EVENTS.with(|e| e.borrow_mut().push(format!("start{}", tag_of(anim))));
        }

        #[unsafe(method(animationDidStop:finished:))]
        fn did_stop(&self, anim: &CAAnimation, finished: bool) {
            EVENTS.with(|e| e.borrow_mut().push(format!("stop{}:{}", tag_of(anim), finished as u8)));
        }
    }
);

fn tag_of(anim: &CAAnimation) -> String {
    let t: Option<Retained<AnyObject>> = unsafe { msg_send![anim, valueForKey: &*ns("tag")] };
    t.map(|t| description(Some(&t))).unwrap_or_default()
}

fn event(e: &str) {
    EVENTS.with(|v| v.borrow_mut().push(e.to_owned()));
}

fn events() -> Vec<String> {
    EVENTS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

fn has_event(e: &str) -> bool {
    EVENTS.with(|v| v.borrow().iter().any(|x| x == e))
}

/// An opacity animation of `seconds` (wall clock) tagged `tag`, watched.
fn watched(seconds: f64, tag: &str, watcher: &Watcher) -> Retained<CABasicAnimation> {
    let a = basic("opacity", Some(&num(0.0)), Some(&num(1.0)), seconds);
    let _: () = unsafe { msg_send![&*a, setValue: &*ns(tag), forKey: &*ns("tag")] };
    let _: () = unsafe { msg_send![&*a, setDelegate: watcher] };
    a
}

/// The order of delegates' calls and completion blocks (measured). The
/// animations run on the wall clock, but only the order is checked, each
/// wait ending when the last call comes (or at a deadline).
fn completion_order(mtm: MainThreadMarker) {
    let (w, root) = window(mtm);
    let watcher: Retained<Watcher> = unsafe { msg_send![Watcher::alloc(), init] };
    let live = || {
        let l = CALayer::new();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        root.addSublayer(&l);
        CATransaction::commit();
        CATransaction::flush();
        l
    };
    let wait = |e: &str| run_until(|| has_event(e));
    events();
    // A transaction's block runs when its animation ends, before the
    // delegate hears; the start comes after the commit returns.
    let l = live();
    CATransaction::begin();
    let block = RcBlock::new(|| event("block"));
    unsafe { CATransaction::setCompletionBlock(Some(&block)) };
    l.addAnimation_forKey(&watched(0.1, "A", &watcher), Some(&ns("a")));
    event("committing");
    CATransaction::commit();
    event("committed");
    wait("stopA:1");
    assert_eq!(events(), ["committing", "committed", "startA", "block", "stopA:1"]);
    // One kept when it ends: its delegate hears once, and nothing waits
    // for it any more (Sidestep's ending timer is gone too).
    let l = live();
    let a = watched(0.1, "D", &watcher);
    a.setRemovedOnCompletion(false);
    a.setFillMode(unsafe { kCAFillModeForwards });
    l.addAnimation_forKey(&a, Some(&ns("d")));
    wait("stopD:1");
    assert_eq!(events(), ["startD", "stopD:1"]);
    assert_eq!(keys(&l), ["d"]);
    // A block set after the animation doesn't wait for it; one replaced
    // still runs, when its animations end.
    let l = live();
    CATransaction::begin();
    let first = RcBlock::new(|| event("first"));
    unsafe { CATransaction::setCompletionBlock(Some(&first)) };
    l.addAnimation_forKey(&watched(0.1, "J", &watcher), Some(&ns("j")));
    let second = RcBlock::new(|| event("second"));
    unsafe { CATransaction::setCompletionBlock(Some(&second)) };
    CATransaction::commit();
    wait("stopJ:1");
    assert_eq!(events(), ["startJ", "second", "first", "stopJ:1"]);
    // An outer block set first waits for an inner transaction's animation.
    let l = live();
    CATransaction::begin();
    let outer = RcBlock::new(|| event("outer"));
    unsafe { CATransaction::setCompletionBlock(Some(&outer)) };
    CATransaction::begin();
    l.addAnimation_forKey(&watched(0.1, "G", &watcher), Some(&ns("g")));
    CATransaction::commit();
    CATransaction::commit();
    wait("stopG:1");
    assert_eq!(events(), ["startG", "outer", "stopG:1"]);
    // Removed early: the block, then the delegate, told it didn't finish.
    let l = live();
    CATransaction::begin();
    let block = RcBlock::new(|| event("block"));
    unsafe { CATransaction::setCompletionBlock(Some(&block)) };
    l.addAnimation_forKey(&watched(30.0, "B", &watcher), Some(&ns("b")));
    CATransaction::commit();
    wait("startB");
    l.removeAnimationForKey(&ns("b"));
    wait("stopB:0");
    assert_eq!(events(), ["startB", "block", "stopB:0"]);
    // A layer leaving its window's tree stops its animations unfinished.
    let l = live();
    l.addAnimation_forKey(&watched(30.0, "F", &watcher), Some(&ns("f")));
    wait("startF");
    l.removeFromSuperlayer();
    CATransaction::flush();
    wait("stopF:0");
    assert_eq!(events(), ["startF", "stopF:0"]);
    assert!(keys(&l).is_empty());
    // One on a layer in no window: started, stopped unfinished and gone.
    let lone = CALayer::new();
    CATransaction::begin();
    let block = RcBlock::new(|| event("block"));
    unsafe { CATransaction::setCompletionBlock(Some(&block)) };
    lone.addAnimation_forKey(&watched(0.1, "M", &watcher), Some(&ns("m")));
    CATransaction::commit();
    wait("block");
    assert_eq!(events(), ["startM", "stopM:0", "block"]);
    assert!(keys(&lone).is_empty());
    w.close();
}

define_class!(
    /// A view that updates its layer itself, counting.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Cell<u32>]
    #[name = "QuartzcoreUpdatingView"]
    struct UpdatingView;

    impl UpdatingView {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.ivars().set(self.ivars().get() + 1);
        }
    }
);

define_class!(
    /// A view that draws.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "QuartzcoreDrawingView"]
    struct DrawingView;

    impl DrawingView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _r: objc2_foundation::NSRect) {}
    }
);

fn view_details(mtm: MainThreadMarker) {
    // A plain view updates its layer itself; one that draws doesn't.
    let plain = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert!(plain.wantsUpdateLayer());
    let drawing: Retained<DrawingView> =
        unsafe { msg_send![DrawingView::alloc(mtm), initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] };
    assert!(!drawing.wantsUpdateLayer());
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    let content = w.contentView().expect("a content view");
    content.setWantsLayer(true);
    CATransaction::flush();
    // updateLayer at the commit that finds the view in the window, and
    // after each time it needs display.
    let uv: Retained<UpdatingView> = unsafe {
        msg_send![super(UpdatingView::alloc(mtm).set_ivars(Cell::new(0))), initWithFrame: rect(0.0, 0.0, 50.0, 50.0)]
    };
    uv.setWantsLayer(true);
    CATransaction::flush();
    assert_eq!(uv.ivars().get(), 0, "not outside a window");
    content.addSubview(&uv);
    CATransaction::flush();
    assert_eq!(uv.ivars().get(), 1);
    uv.setNeedsDisplay(true);
    CATransaction::flush();
    assert_eq!(uv.ivars().get(), 2);
    // The view's clipping is its layer's.
    let cv = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    content.addSubview(&cv);
    CATransaction::flush();
    cv.setClipsToBounds(true);
    assert!(cv.layer().expect("a layer").masksToBounds());
    // A bounds size other than the frame's is the layer's scale.
    let sv = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 30.0, 40.0));
    content.addSubview(&sv);
    sv.setBoundsSize(objc2_foundation::NSSize::new(60.0, 80.0));
    CATransaction::flush();
    let sl = sv.layer().expect("a layer");
    same_rect(sl.bounds(), r(0.0, 0.0, 60.0, 80.0));
    close(sl.transform().m11, 0.5, 1e-9);
    same_rect(sl.frame(), r(0.0, 0.0, 30.0, 40.0));
    // A view keeps its layer when it leaves its superview.
    sv.removeFromSuperview();
    assert!(sv.layer().is_some());
    assert!(sl.superlayer().is_none());
    // Inside a group allowing implicit animation, a view's frame and alpha
    // animate its layer.
    let av = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 20.0, 20.0));
    content.addSubview(&av);
    CATransaction::flush();
    let al = av.layer().expect("a layer");
    let moved = av.clone();
    let block = RcBlock::new(move |ctx: std::ptr::NonNull<objc2_app_kit::NSAnimationContext>| {
        let ctx = unsafe { ctx.as_ref() };
        ctx.setDuration(0.5);
        ctx.setAllowsImplicitAnimation(true);
        moved.setFrame(rect(5.0, 5.0, 30.0, 30.0));
        moved.setAlphaValue(0.5);
    });
    objc2_app_kit::NSAnimationContext::runAnimationGroup(&block);
    CATransaction::flush();
    assert_eq!(keys(&al), ["position", "bounds", "opacity"]);
    w.close();
}

fn pixel_details(_: MainThreadMarker) {
    // A mask smaller than its layer: nothing shows outside it.
    let l = CALayer::new();
    l.setBounds(r(0.0, 0.0, 20.0, 20.0));
    l.setBackgroundColor(Some(&rgb(1.0, 0.0, 0.0, 1.0)));
    let m = CALayer::new();
    m.setFrame(r(0.0, 0.0, 10.0, 20.0));
    m.setBackgroundColor(Some(&rgb(0.0, 0.0, 0.0, 1.0)));
    unsafe { l.setMask(Some(&m)) };
    let c = context(20, 20);
    l.renderInContext(&c);
    assert_px(&c, 5, 10, [255, 0, 0, 255], 0);
    assert_px(&c, 15, 10, [0, 0, 0, 0], 0);
    // A 2 × 2 image: red and green over blue and white.
    let img_ctx = context(2, 2);
    for (x, y, rgb) in [
        (0.0, 1.0, [1.0, 0.0, 0.0]),
        (1.0, 1.0, [0.0, 1.0, 0.0]),
        (0.0, 0.0, [0.0, 0.0, 1.0]),
        (1.0, 0.0, [1.0, 1.0, 1.0]),
    ] {
        CGContext::set_rgb_fill_color(Some(&img_ctx), rgb[0], rgb[1], rgb[2], 1.0);
        CGContext::fill_rect(Some(&img_ctx), r(x, y, 1.0, 1.0));
    }
    let image = objc2_core_graphics::CGBitmapContextCreateImage(Some(&img_ctx)).expect("an image");
    let image_obj = unsafe { &*(objc2_core_foundation::CFRetained::as_ptr(&image).as_ptr() as *const AnyObject) };
    // contentsRect's origin is the bottom left where the layer's y runs
    // up, the top left where its contents are flipped (measured).
    let k = CALayer::new();
    k.setBounds(r(0.0, 0.0, 10.0, 10.0));
    unsafe { k.setContents(Some(image_obj)) };
    k.setMagnificationFilter(unsafe { kCAFilterNearest });
    k.setContentsRect(r(0.0, 0.0, 0.5, 0.5));
    let c = context(10, 10);
    k.renderInContext(&c);
    assert_px(&c, 5, 5, [0, 0, 255, 255], 0);
    k.setGeometryFlipped(true);
    let c = context(10, 10);
    k.renderInContext(&c);
    assert_px(&c, 5, 5, [255, 0, 0, 255], 0);
    // A flipped layer's contents are drawn turned over; its gravity keeps
    // its place.
    let g = CALayer::new();
    g.setBounds(r(0.0, 0.0, 20.0, 10.0));
    unsafe { g.setContents(Some(image_obj)) };
    g.setMagnificationFilter(unsafe { kCAFilterNearest });
    g.setContentsGravity(unsafe { kCAGravityTopLeft });
    g.setGeometryFlipped(true);
    let c = context(20, 10);
    g.renderInContext(&c);
    assert_px(&c, 0, 1, [255, 0, 0, 255], 0);
    assert_px(&c, 0, 0, [0, 0, 255, 255], 0);
    // A radial gradient is an ellipse; with no height, nothing.
    let rg = CAGradientLayer::new();
    rg.setBounds(r(0.0, 0.0, 40.0, 20.0));
    let colors = NSArray::from_slice(&[cg_obj(&rgb(1.0, 0.0, 0.0, 1.0)), cg_obj(&rgb(0.0, 0.0, 1.0, 1.0))]);
    unsafe { rg.setColors(Some(&colors)) };
    rg.setType(unsafe { kCAGradientLayerRadial });
    rg.setStartPoint(p(0.5, 0.5));
    rg.setEndPoint(p(1.0, 1.0));
    let c = context(40, 20);
    rg.renderInContext(&c);
    let (center, x_edge, y_edge) = (px(&c, 20, 10), px(&c, 39, 10), px(&c, 20, 19));
    assert!(center[0] > 200 && center[2] < 60, "{center:?}");
    assert!(x_edge[2] > 200 && x_edge[0] < 60, "{x_edge:?}");
    assert!(y_edge[2] > 200 && y_edge[0] < 60, "{y_edge:?}");
    rg.setEndPoint(p(1.0, 0.5));
    let c = context(40, 20);
    rg.renderInContext(&c);
    assert_px(&c, 20, 10, [0, 0, 0, 0], 0);
    // A sublayer transform scales about the anchor point.
    let t = CALayer::new();
    t.setBounds(r(0.0, 0.0, 40.0, 40.0));
    t.setAnchorPoint(p(0.0, 0.0));
    t.setSublayerTransform(CATransform3DMakeScale(0.5, 0.5, 1.0));
    let s = CALayer::new();
    s.setFrame(r(20.0, 20.0, 20.0, 20.0));
    s.setBackgroundColor(Some(&rgb(1.0, 0.0, 0.0, 1.0)));
    t.addSublayer(&s);
    let c = context(40, 40);
    t.renderInContext(&c);
    // Layer (10…20, 10…20): rows 20 to 29 from the top.
    assert_px(&c, 15, 25, [255, 0, 0, 255], 0);
    assert_px(&c, 25, 15, [0, 0, 0, 0], 0);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("defaults", defaults),
        ("constants", constants),
        ("geometry", geometry),
        ("conversions_and_hits", conversions_and_hits),
        ("tree", tree),
        ("timing_functions", timing_functions),
        ("transforms", transforms),
        ("key_value_coding", key_value_coding),
        ("animation_objects", animation_objects),
        ("transactions", transactions),
        ("implicit_actions", implicit_actions),
        ("presentation_timing", presentation_timing),
        ("keyframes_springs_groups", keyframes_springs_groups),
        ("view_layers", view_layers),
        ("render_in_context", render_in_context),
        ("shape_layers", shape_layers),
        ("tree_details", tree_details),
        ("kvc_details", kvc_details),
        ("timing_details", timing_details),
        ("completion_order", completion_order),
        ("view_details", view_details),
        ("pixel_details", pixel_details),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
