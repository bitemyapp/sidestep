//! Core Animation: layer-backed views and the layers programs add, with
//! the effects apps polish their interfaces with (continuous rounded
//! corners, borders, shadows, gradients, shapes, masks to bounds), and
//! animations Core Animation runs on its own once they're committed: a
//! spring, a rotation, a stroke drawn and undrawn, a keyframe path, and a
//! fade transition every second from a timer. On Linux the render thread
//! draws every animation frame; the main thread commits once a second.
//!
//! `LAYERDEMO_QUIT_AFTER=seconds` quits after that long. With
//! `SIDESTEP_TRACE_FRAMES=1`, Sidestep prints each commit's cost on the
//! main thread and each frame's compositing on the render thread.
//!
//! Runs on macOS too, for comparison.

use std::cell::Cell;
use std::f64::consts::PI;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSFont, NSRectFill, NSTextField, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGPath};
use objc2_foundation::{NSArray, NSNumber, NSPoint, NSRect, NSSize, NSString, NSTimer, NSValue};
use objc2_quartz_core::{
    CABasicAnimation, CAGradientLayer, CAKeyframeAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CAShapeLayer, CASpringAnimation, CATransaction, CATransition, kCAAnimationPaced, kCACornerCurveContinuous,
    kCAMediaTimingFunctionEaseInEaseOut, kCAMediaTimingFunctionLinear,
};

// Links Sidestep's runtime and frameworks on Linux; empty on macOS.
use sidestep as _;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn cg(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

fn color(r: f64, g: f64, b: f64, a: f64) -> objc2_core_foundation::CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

fn object<T: objc2::Message>(o: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(o) }
}

fn color_object(c: objc2_core_foundation::CFRetained<CGColor>) -> Retained<AnyObject> {
    // SAFETY: a CGColor is an object.
    unsafe { Retained::cast_unchecked(Retained::from(c)) }
}

fn number(v: f64) -> Retained<AnyObject> {
    object(NSNumber::new_f64(v))
}

fn function(name: &NSString) -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithName(name)
}

#[derive(Default)]
struct BackdropIvars;

define_class!(
    /// The window's content: a light backdrop with a stripe, drawn into
    /// its layer.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = BackdropIvars]
    #[name = "LayerDemoBackdrop"]
    struct Backdrop;

    impl Backdrop {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.93, 0.94, 0.96, 1.0).setFill();
            NSRectFill(self.bounds());
            NSColor::colorWithSRGBRed_green_blue_alpha(0.85, 0.87, 0.91, 1.0).setFill();
            NSRectFill(rect(0.0, 0.0, self.bounds().size.width, 24.0));
        }
    }
);

/// A card: a layer-backed view with rounded continuous corners and a
/// shadow, holding a label, breathing on a spring.
fn card(mtm: MainThreadMarker) -> Retained<NSView> {
    let view = NSView::initWithFrame(NSView::alloc(mtm), rect(24.0, 160.0, 210.0, 130.0));
    view.setWantsLayer(true);
    let layer = view.layer().expect("a layer");
    layer.setBackgroundColor(Some(&color(1.0, 1.0, 1.0, 1.0)));
    layer.setCornerRadius(16.0);
    layer.setCornerCurve(unsafe { kCACornerCurveContinuous });
    layer.setBorderWidth(1.0);
    layer.setBorderColor(Some(&color(0.0, 0.0, 0.0, 0.08)));
    layer.setShadowColor(Some(&color(0.1, 0.12, 0.2, 1.0)));
    layer.setShadowOpacity(0.25);
    layer.setShadowRadius(10.0);
    layer.setShadowOffset(CGSize::new(0.0, -4.0));
    let label = NSTextField::labelWithString(&NSString::from_str("Core Animation"), mtm);
    label.setFont(Some(&NSFont::boldSystemFontOfSize(17.0)));
    label.setFrame(rect(18.0, 84.0, 180.0, 26.0));
    view.addSubview(&label);
    let detail = NSTextField::labelWithString(&NSString::from_str("springs on the render thread"), mtm);
    detail.setFrame(rect(18.0, 60.0, 190.0, 20.0));
    view.addSubview(&detail);
    // A pill inside, clipped by its own rounded bounds.
    let pill = CAGradientLayer::new();
    pill.setFrame(cg(18.0, 18.0, 174.0, 28.0));
    let colors = NSArray::from_retained_slice(&[
        color_object(color(0.35, 0.2, 0.75, 1.0)),
        color_object(color(0.1, 0.65, 0.7, 1.0)),
    ]);
    unsafe { pill.setColors(Some(&colors)) };
    pill.setStartPoint(CGPoint::new(0.0, 0.5));
    pill.setEndPoint(CGPoint::new(1.0, 0.5));
    pill.setCornerRadius(14.0);
    pill.setMasksToBounds(true);
    layer.addSublayer(&pill);
    view
}

/// The spring the card breathes on: scale 0.94 to 1, settling, again.
fn breathe(layer: &CALayer) {
    let spring = CASpringAnimation::animationWithKeyPath(Some(&NSString::from_str("transform.scale")));
    unsafe {
        spring.setFromValue(Some(&number(0.94)));
        spring.setToValue(Some(&number(1.0)));
    }
    spring.setDamping(7.0);
    spring.setDuration(spring.settlingDuration());
    spring.setRepeatCount(f32::INFINITY);
    layer.addAnimation_forKey(&spring, Some(&NSString::from_str("breathe")));
}

fn spinner(root: &CALayer) -> Retained<CALayer> {
    let l = CALayer::new();
    l.setFrame(cg(290.0, 190.0, 72.0, 72.0));
    l.setBackgroundColor(Some(&color(0.98, 0.55, 0.15, 1.0)));
    l.setCornerRadius(14.0);
    l.setBorderWidth(3.0);
    l.setBorderColor(Some(&color(1.0, 1.0, 1.0, 0.9)));
    root.addSublayer(&l);
    let spin = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("transform.rotation.z")));
    unsafe {
        spin.setFromValue(Some(&number(0.0)));
        spin.setToValue(Some(&number(2.0 * PI)));
    }
    spin.setDuration(2.4);
    spin.setRepeatCount(f32::INFINITY);
    spin.setTimingFunction(Some(&function(unsafe { kCAMediaTimingFunctionLinear })));
    l.addAnimation_forKey(&spin, Some(&NSString::from_str("spin")));
    l
}

fn ring(root: &CALayer) -> Retained<CAShapeLayer> {
    let s = CAShapeLayer::new();
    s.setFrame(cg(390.0, 190.0, 72.0, 72.0));
    let path = unsafe { CGPath::with_ellipse_in_rect(cg(6.0, 6.0, 60.0, 60.0), std::ptr::null()) };
    s.setPath(Some(&path));
    s.setFillColor(None);
    s.setStrokeColor(Some(&color(0.2, 0.45, 0.95, 1.0)));
    s.setLineWidth(7.0);
    unsafe {
        s.setLineCap(objc2_quartz_core::kCALineCapRound);
        s.setLineJoin(objc2_quartz_core::kCALineJoinRound);
    }
    root.addSublayer(&s);
    let draw = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("strokeEnd")));
    unsafe {
        draw.setFromValue(Some(&number(0.0)));
        draw.setToValue(Some(&number(1.0)));
    }
    draw.setDuration(1.2);
    draw.setAutoreverses(true);
    draw.setRepeatCount(f32::INFINITY);
    draw.setTimingFunction(Some(&function(unsafe { kCAMediaTimingFunctionEaseInEaseOut })));
    s.addAnimation_forKey(&draw, Some(&NSString::from_str("draw")));
    s
}

fn orbit(root: &CALayer) -> Retained<CALayer> {
    let dot = CALayer::new();
    dot.setBounds(cg(0.0, 0.0, 18.0, 18.0));
    dot.setPosition(CGPoint::new(270.0, 60.0));
    dot.setCornerRadius(9.0);
    dot.setBackgroundColor(Some(&color(0.9, 0.2, 0.35, 1.0)));
    dot.setShadowOpacity(0.35);
    dot.setShadowRadius(4.0);
    root.addSublayer(&dot);
    let path = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    let points = [(270.0, 60.0), (450.0, 60.0), (450.0, 130.0), (270.0, 130.0), (270.0, 60.0)];
    let values: Vec<Retained<AnyObject>> =
        points.iter().map(|&(x, y)| object(unsafe { NSValue::valueWithPoint(NSPoint::new(x, y)) })).collect();
    unsafe { path.setValues(Some(&NSArray::from_retained_slice(&values))) };
    path.setCalculationMode(unsafe { kCAAnimationPaced });
    path.setDuration(3.0);
    path.setRepeatCount(f32::INFINITY);
    dot.addAnimation_forKey(&path, Some(&NSString::from_str("orbit")));
    dot
}

/// A tile that fades to its other color every second.
fn tile(root: &CALayer) -> Retained<CALayer> {
    let t = CALayer::new();
    t.setFrame(cg(24.0, 40.0, 210.0, 90.0));
    t.setCornerRadius(12.0);
    t.setBackgroundColor(Some(&color(0.2, 0.6, 0.4, 1.0)));
    root.addSublayer(&t);
    t
}

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 480.0, 320.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Layers"));
    let backdrop: Retained<Backdrop> = {
        let this = Backdrop::alloc(mtm).set_ivars(BackdropIvars);
        unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 480.0, 320.0)] }
    };
    backdrop.setWantsLayer(true);
    window.setContentView(Some(&backdrop));
    let root = backdrop.layer().expect("a layer");
    let card = card(mtm);
    backdrop.addSubview(&card);
    let _spinner = spinner(&root);
    let _ring = ring(&root);
    let _dot = orbit(&root);
    let tile = tile(&root);
    window.makeKeyAndOrderFront(None);
    // The card's layer exists once committed; then it breathes.
    CATransaction::flush();
    breathe(&card.layer().expect("a layer"));

    // Once a second, the tile fades to its other color: a transaction
    // with a transition, from a timer.
    let flip = Cell::new(false);
    let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
        flip.set(!flip.get());
        CATransaction::begin();
        CATransaction::setAnimationDuration(0.6);
        let fade = CATransition::new();
        fade.setDuration(0.6);
        tile.addAnimation_forKey(&fade, None);
        let c = if flip.get() { color(0.35, 0.3, 0.8, 1.0) } else { color(0.2, 0.6, 0.4, 1.0) };
        CATransaction::setDisableActions(true);
        tile.setBackgroundColor(Some(&c));
        CATransaction::commit();
    });
    let _timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block) };

    if let Some(secs) = std::env::var("LAYERDEMO_QUIT_AFTER").ok().and_then(|v| v.parse::<f64>().ok()) {
        let quit = RcBlock::new(|_t: NonNull<NSTimer>| {
            let mtm = MainThreadMarker::new().expect("the main thread");
            NSApplication::sharedApplication(mtm).terminate(None);
        });
        let _ = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &quit) };
    }
    app.run();
}
