//! Core Animation on Linux, through the null render thread: layer-backed
//! views drawn into their layers' canvases and composited with the layers
//! programs add, animations the render thread draws at chosen times (the
//! main thread takes no part), transitions, masks, views that update their
//! layers themselves, what the main thread and the render thread don't do
//! when nothing animates, layers and views that go first, and display
//! links fed by frame ticks. Pixels are the null render thread's
//! (`testing::capture_pixels`), at scale 1, row 0 the top of the window's
//! content. What Apple's Core Animation does with the model is pinned by
//! `conformance/tests/quartzcore.rs`; this covers what only a shown
//! window's render thread does.
//!
//! Animations here last 1000 seconds and are drawn at chosen times, so no
//! assertion depends on how fast the machine runs; the one that must end
//! on the clock is waited for, with a deadline.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

#[cfg(target_vendor = "apple")]
fn main() {}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    linux::main();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::Cell;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSAnimatablePropertyContainer, NSAnimationContext, NSAppearance, NSAppearanceNameAqua, NSApplication,
        NSBackingStoreType, NSColor, NSRectFill, NSView, NSWindow, NSWindowStyleMask,
    };
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_core_graphics::CGColor;
    use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSValue};
    use objc2_quartz_core::{
        CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CATransaction, CATransform3D,
        NSValueCATransform3DAdditions, kCAFillModeForwards, kCAMediaTimingFunctionLinear,
    };
    use sidestep_appkit::testing::{self, Seen};

    type Test = (&'static str, fn(MainThreadMarker));

    /// How long the animations here last (seconds): they're drawn at
    /// chosen times long before they end.
    const LONG: f64 = 1000.0;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn cg(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    fn linear() -> Retained<CAMediaTimingFunction> {
        // SAFETY: the name is a constant string.
        CAMediaTimingFunction::functionWithName(unsafe { kCAMediaTimingFunctionLinear })
    }

    #[derive(Default)]
    struct PainterIvars {
        color: Cell<[f64; 3]>,
        draws: Cell<u32>,
    }

    define_class!(
        /// A view that fills itself with its color.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[ivars = PainterIvars]
        #[name = "LinuxLayersPainter"]
        struct Painter;

        impl Painter {
            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, dirty: NSRect) {
                let [r, g, b] = self.ivars().color.get();
                NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).setFill();
                NSRectFill(dirty);
                self.ivars().draws.set(self.ivars().draws.get() + 1);
            }
        }
    );

    impl Painter {
        fn new(mtm: MainThreadMarker, frame: NSRect, color: [f64; 3]) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(PainterIvars { color: Cell::new(color), draws: Cell::new(0) });
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }
    }

    define_class!(
        /// A view that updates its layer itself: green.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[ivars = Cell<u32>]
        #[name = "LinuxLayersUpdater"]
        struct Updater;

        impl Updater {
            #[unsafe(method(wantsUpdateLayer))]
            fn wants_update_layer(&self) -> bool {
                true
            }

            #[unsafe(method(updateLayer))]
            fn update_layer(&self) {
                self.ivars().set(self.ivars().get() + 1);
                if let Some(l) = self.layer() {
                    l.setBackgroundColor(Some(&CGColor::new_srgb(0.0, 1.0, 0.0, 1.0)));
                }
            }
        }
    );

    #[derive(Default)]
    struct TargetIvars {
        ticks: Cell<u32>,
        stops: Cell<u32>,
    }

    define_class!(
        /// A display link's target and an animation's delegate, counting.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = TargetIvars]
        #[name = "LinuxLayersTarget"]
        struct Target;

        unsafe impl NSObjectProtocol for Target {}

        impl Target {
            #[unsafe(method(tick:))]
            fn tick(&self, _link: &AnyObject) {
                self.ivars().ticks.set(self.ivars().ticks.get() + 1);
            }

            #[unsafe(method(animationDidStop:finished:))]
            fn did_stop(&self, _anim: &AnyObject, _finished: bool) {
                self.ivars().stops.set(self.ivars().stops.get() + 1);
            }
        }
    );

    fn target(mtm: MainThreadMarker) -> Retained<Target> {
        let this = Target::alloc(mtm).set_ivars(TargetIvars::default());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// A shown 200 × 100 window whose content view is a red painter that
    /// wants a layer.
    fn shown(mtm: MainThreadMarker) -> (Retained<NSWindow>, Retained<Painter>) {
        // SAFETY: a plain window.
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 200.0, 100.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Rust owns the window, so closing it mustn't release it.
        unsafe { w.setReleasedWhenClosed(false) };
        let painter = Painter::new(mtm, rect(0.0, 0.0, 200.0, 100.0), [1.0, 0.0, 0.0]);
        painter.setWantsLayer(true);
        w.setContentView(Some(&painter));
        w.makeKeyAndOrderFront(None);
        testing::settle_first_frames();
        (w, painter)
    }

    /// Commit, display and let the null render thread draw.
    fn show(w: &NSWindow) {
        testing::commit_layers();
        testing::settle();
        testing::display_now(w);
        testing::settle();
    }

    /// The pixel at (`x`, `y`) of the window's content, from its top left.
    fn px(w: &NSWindow, x: u32, y: u32) -> [u8; 4] {
        let (width, _, pixels) = testing::window_pixels(w).expect("captured pixels");
        pixels[(y * width + x) as usize]
    }

    #[track_caller]
    fn near(got: [u8; 4], want: [u8; 4], tol: u8) {
        assert!(got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= tol), "{got:?} is not {want:?}");
    }

    fn blue_square(root: &CALayer) -> Retained<CALayer> {
        let l = CALayer::new();
        l.setFrame(cg(10.0, 10.0, 20.0, 20.0));
        l.setBackgroundColor(Some(&CGColor::new_srgb(0.0, 0.0, 1.0, 1.0)));
        root.addSublayer(&l);
        l
    }

    fn asynchronous_pixel_crops(mtm: MainThreadMarker) {
        for scale in [1, 2] {
            testing::use_null_backend_scale(scale);
            let (w, painter) = shown(mtm);
            let blue = blue_square(&painter.layer().unwrap());
            show(&w);
            let crop = [5 * scale, 65 * scale, 30 * scale, 30 * scale];
            let reply = testing::request_window_pixels(&w, testing::media_time(), crop);
            // The render thread can answer without any main-thread pumping.
            let (width, height, pixels) = reply.recv_timeout(std::time::Duration::from_secs(5))
                .expect("render thread answered").expect("valid crop");
            assert_eq!((width, height), (crop[2], crop[3]));
            assert_eq!(pixels.len(), (width * height) as usize);
            near(pixels[0], [255, 0, 0, 255], 0);
            near(pixels[((15 * scale) * width + 15 * scale) as usize], [0, 0, 255, 255], 0);
            for invalid in [[0, 0, 0, 1], [0, 100 * scale, 1, 1], [200 * scale, 0, 1, 1], [u32::MAX, 0, 2, 1]] {
                let reply = testing::request_window_pixels(&w, testing::media_time(), invalid);
                assert!(reply.recv_timeout(std::time::Duration::from_secs(5)).expect("render thread answered").is_none());
            }
            blue.removeFromSuperlayer();
            w.close();
            testing::settle();
        }
        testing::use_null_backend_scale(1);
    }

    fn views_and_layers_composite(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        // The view's drawing is its layer's canvas.
        near(px(&w, 100, 50), [255, 0, 0, 255], 0);
        assert!(painter.ivars().draws.get() >= 1);
        let root = painter.layer().expect("a layer");
        let blue = blue_square(&root);
        testing::settle();
        testing::display_now(&w);
        testing::settle();
        // The layer's y runs up: 10 to 30 from the bottom is 70 to 90 from
        // the top.
        near(px(&w, 20, 80), [0, 0, 255, 255], 0);
        near(px(&w, 20, 60), [255, 0, 0, 255], 0);
        assert!(testing::render_layer_count() >= 2);
        // Redrawing the view repaints its canvas, not the window.
        painter.ivars().color.set([0.0, 1.0, 0.0]);
        painter.setNeedsDisplay(true);
        testing::settle();
        testing::display_now(&w);
        testing::settle();
        near(px(&w, 100, 50), [0, 255, 0, 255], 0);
        near(px(&w, 20, 80), [0, 0, 255, 255], 0);
        blue.removeFromSuperlayer();
        w.close();
        testing::settle();
    }

    fn animations_draw_on_the_render_thread(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let root = painter.layer().expect("a layer");
        let blue = blue_square(&root);
        show(&w);
        // From x 20 to 120 (the square's middle), linearly.
        let a = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
        // SAFETY: points as values.
        unsafe {
            let from = NSValue::valueWithPoint(NSPoint::new(20.0, 20.0));
            let to = NSValue::valueWithPoint(NSPoint::new(120.0, 20.0));
            a.setFromValue(Some(&from));
            a.setToValue(Some(&to));
        }
        a.setDuration(LONG);
        a.setTimingFunction(Some(&linear()));
        blue.addAnimation_forKey(&a, Some(&NSString::from_str("move")));
        show(&w);
        let begin = unsafe { blue.animationForKey(&NSString::from_str("move")) }.expect("an animation").beginTime();
        assert!(begin > 0.0);
        // Halfway, as the render thread would draw it: the main thread
        // isn't asked for anything.
        let p = testing::presented_layer(&blue, begin + LONG / 2.0).expect("on the render thread");
        assert!((p.position[0] - 70.0).abs() < 1e-6, "{:?}", p.position);
        // Queue distinct times before receiving either: each reply must
        // own its frame, not the pixels from whichever composite ran last.
        let middle = testing::request_window_pixels(&w, begin + LONG / 2.0, [70, 80, 1, 1]);
        let end = testing::request_window_pixels(&w, begin + 2.0 * LONG, [70, 80, 1, 1]);
        let middle = middle.recv_timeout(std::time::Duration::from_secs(5)).unwrap().unwrap();
        let end = end.recv_timeout(std::time::Duration::from_secs(5)).unwrap().unwrap();
        near(middle.2[0], [0, 0, 255, 255], 0);
        near(end.2[0], [255, 0, 0, 255], 0);
        testing::composite_at(&w, begin + LONG / 2.0);
        near(px(&w, 70, 80), [0, 0, 255, 255], 0);
        near(px(&w, 20, 80), [255, 0, 0, 255], 0);
        // After it ends, the model's place.
        testing::composite_at(&w, begin + 2.0 * LONG);
        near(px(&w, 20, 80), [0, 0, 255, 255], 0);
        near(px(&w, 70, 80), [255, 0, 0, 255], 0);
        blue.removeFromSuperlayer();
        w.close();
        testing::settle();
    }

    fn animator_interpolates_without_enabling_implicit_changes(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let child = Painter::new(mtm, rect(10.0, 10.0, 20.0, 20.0), [0.0, 0.0, 1.0]);
        painter.addSubview(&child);
        show(&w);
        let layer = child.layer().unwrap();
        let moving = child.clone();
        let changes = block2::RcBlock::new(move |context: std::ptr::NonNull<NSAnimationContext>| {
            let context = unsafe { context.as_ref() };
            context.setDuration(LONG);
            context.setTimingFunction(Some(&linear()));
            assert!(!context.allowsImplicitAnimation());
            moving.animator().setFrame(rect(110.0, 10.0, 20.0, 20.0));
            moving.animator().setAlphaValue(0.5);
            // An ordinary setter in this same group still applies at once.
            moving.setBoundsOrigin(NSPoint::new(1.0, 0.0));
        });
        NSAnimationContext::runAnimationGroup(&changes);
        show(&w);
        let animation = unsafe { layer.animationForKey(&NSString::from_str("position")) }
            .expect("animator creates an animation even when allowsImplicitAnimation is false");
        let begin = animation.beginTime();
        let draws = child.ivars().draws.get();
        for step in 0..=20 {
            let progress = step as f64 / 20.0;
            let p = testing::presented_layer(&layer, begin + LONG * progress).unwrap();
            assert!((p.position[0] - (10.0 + 100.0 * progress)).abs() < 0.01, "{:?}", p.position);
            assert!((p.opacity - (1.0 - 0.5 * progress)).abs() < 0.01, "{}", p.opacity);
            assert_eq!(p.bounds[0], 1.0, "direct setters remain immediate");
            testing::composite_at(&w, begin + LONG * progress);
        }
        assert_eq!(child.ivars().draws.get(), draws, "animation reuses cached view drawing");
        w.close();
        testing::settle();
    }

    fn animator_resizes_and_completes_while_tracking(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let target = painter.clone();
        let changes = block2::RcBlock::new(move |context: std::ptr::NonNull<NSAnimationContext>| {
            // SAFETY: the animation group supplies its context.
            let context = unsafe { context.as_ref() };
            context.setDuration(LONG);
            context.setTimingFunction(Some(&linear()));
            target.animator().setFrameSize(NSSize::new(100.0, 100.0));
        });
        NSAnimationContext::runAnimationGroup(&changes);
        show(&w);
        let layer = painter.layer().unwrap();
        let bounds = unsafe { layer.animationForKey(&NSString::from_str("bounds")) }.expect("animated resize");
        let begin = bounds.beginTime();
        let draws = painter.ivars().draws.get();
        for step in 0..=20 {
            let progress = step as f64 / 20.0;
            let p = testing::presented_layer(&layer, begin + LONG * progress).unwrap();
            assert!((p.bounds[2] - (200.0 - 100.0 * progress)).abs() < 0.01);
            testing::composite_at(&w, begin + LONG * progress);
        }
        assert_eq!(painter.ivars().draws.get(), draws);

        let done = std::rc::Rc::new(Cell::new(false));
        let completed = done.clone();
        let changes = block2::RcBlock::new(|context: std::ptr::NonNull<NSAnimationContext>| {
            unsafe { context.as_ref() }.setDuration(0.0);
        });
        let completion = block2::RcBlock::new(move || completed.set(true));
        NSAnimationContext::runAnimationGroup_completionHandler(&changes, Some(&completion));
        // Keep the run loop in mouse tracking, as a held control does.
        unsafe {
            NSRunLoop::currentRunLoop().runMode_beforeDate(
                objc2_app_kit::NSEventTrackingRunLoopMode,
                &objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.02),
            );
        }
        assert!(done.get(), "animation completion must not wait for mouse tracking to end");
        w.close();
        testing::settle();
    }

    /// A segmented switch drawn under a control, as an application draws
    /// its own: a track, a pill behind the chosen segment that slides to
    /// the next on an animation of its translation, a wash and a seed shown
    /// on the press, and the seed sweeping the segment through `animator`
    /// as it fades out, while the window's content is drawn again around
    /// it. A view's drawing shows in the bounds its layer presents, so the
    /// seed grows from its sliver rather than showing whole at its old
    /// origin: nothing is drawn outside the track, the seed and the wash
    /// stay in their segment, the pill is never taller than one, and once
    /// it settles the window holds what drawing it still would.
    fn switch_moves_without_trails(mtm: MainThreadMarker) {
        // At 2× too, as most displays now are.
        for scale in [1, 2] {
            testing::use_null_backend_scale(scale);
            switch_at_scale(mtm, scale);
        }
        testing::use_null_backend_scale(1);
    }

    fn switch_at_scale(mtm: MainThreadMarker, scale: u32) {
        // SAFETY: a plain window.
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, 400.0, 100.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Rust owns the window, so closing it mustn't release it.
        unsafe { w.setReleasedWhenClosed(false) };
        let page = Painter::new(mtm, rect(0.0, 0.0, 400.0, 100.0), [0.0, 0.0, 0.0]);
        w.setContentView(Some(&page));
        let container = NSView::initWithFrame(NSView::alloc(mtm), rect(20.0, 30.0, 260.0, 30.0));
        container.setWantsLayer(true);
        page.addSubview(&container);
        // Parts as applications make them: boxes filled with a color, with
        // round corners.
        let part = |frame: NSRect, color: [f64; 3], radius: f64| {
            let b = objc2_app_kit::NSBox::initWithFrame(objc2_app_kit::NSBox::alloc(mtm), frame);
            b.setBoxType(objc2_app_kit::NSBoxType::Custom);
            b.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
            b.setContentViewMargins(NSSize::new(0.0, 0.0));
            b.setBorderWidth(0.0);
            b.setCornerRadius(radius);
            b.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(color[0], color[1], color[2], 1.0));
            b.setWantsLayer(true);
            container.addSubview(&b);
            let view: Retained<NSView> = Retained::into_super(b);
            view
        };
        let segment = |i: f64| rect(3.0 + 127.0 * i, 3.0, 127.0, 24.0);
        let seed_frame = |r: NSRect| rect(r.origin.x, r.origin.y + 4.0, 3.0, r.size.height - 8.0);
        let _track = part(rect(0.0, 0.0, 260.0, 30.0), [0.5, 0.5, 0.5], 15.0);
        let wash = part(segment(0.0), [0.0, 0.0, 1.0], 12.0);
        wash.setAlphaValue(0.0);
        let pill = part(segment(1.0), [1.0, 0.0, 0.0], 12.0);
        // Made the segment's height, as the application makes it, then given
        // its seed's frame at the press.
        let seed: Retained<NSView> = part(rect(0.0, 0.0, 3.0, 24.0), [0.0, 1.0, 0.0], 1.5);
        seed.setAlphaValue(0.0);
        w.makeKeyAndOrderFront(None);
        testing::settle_first_frames();
        show(&w);
        let group = |changes: &dyn Fn()| {
            let changes = block2::RcBlock::new(move |context: std::ptr::NonNull<NSAnimationContext>| {
                // SAFETY: the group hands its live context to the block.
                let context = unsafe { context.as_ref() };
                context.setDuration(LONG);
                context.setTimingFunction(Some(&linear()));
                changes();
            });
            NSAnimationContext::runAnimationGroup(&changes);
        };
        // The press on the first segment: the wash and the seed are in by the
        // time the button comes up, as a quick fade-in is by a click's end.
        wash.setFrame(segment(0.0));
        seed.setFrame(seed_frame(segment(0.0)));
        wash.setAlphaValue(0.38);
        seed.setAlphaValue(1.0);
        show(&w);
        // The release: the window's content changes all over (the switch's
        // action shows another surface), the pill slides over, the seed
        // sweeps and fades.
        page.setNeedsDisplay(true);
        pill.setFrame(segment(0.0));
        let slide = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("transform.translation.x")));
        // SAFETY: numbers are what a translation animates between.
        unsafe {
            slide.setFromValue(Some(&objc2_foundation::NSNumber::new_f64(127.0)));
            slide.setToValue(Some(&objc2_foundation::NSNumber::new_f64(0.0)));
        }
        slide.setDuration(LONG);
        slide.setTimingFunction(Some(&linear()));
        pill.layer().unwrap().addAnimation_forKey(&slide, Some(&NSString::from_str("slide")));
        group(&|| {
            seed.animator().setFrame(segment(0.0));
            seed.animator().setAlphaValue(0.0);
            wash.animator().setAlphaValue(0.0);
        });
        show(&w);
        let seed_layer = seed.layer().unwrap();
        let begin = unsafe { seed_layer.animationForKey(&NSString::from_str("position")) }
            .or_else(|| unsafe { seed_layer.animationForKey(&NSString::from_str("bounds")) })
            .expect("the seed's frame animates")
            .beginTime();
        // The track's pixels, from the window content's top left, and the
        // first segment's (where the seed sweeps and the wash tints).
        let track = (20 * scale, 40 * scale, 280 * scale, 70 * scale);
        let first = (23 * scale, 43 * scale, 150 * scale, 67 * scale);
        let check = |t: f64, at: &str| {
            testing::composite_at(&w, t);
            let (width, height, pixels) = testing::window_pixels(&w).expect("captured pixels");
            assert_eq!((width, height), (400 * scale, 100 * scale), "captured at {scale}×");
            let (mut red_top, mut red_bottom) = (u32::MAX, 0);
            for y in 0..height {
                for x in 0..width {
                    let [r, g, b, _] = pixels[(y * width + x) as usize];
                    let inside = x >= track.0 && x < track.2 && y >= track.1 && y < track.3;
                    assert!(inside || (r, g, b) == (0, 0, 0), "{at}: ({r}, {g}, {b}) at ({x}, {y}), outside the track");
                    if r > 200 && g < 60 && b < 60 {
                        red_top = red_top.min(y);
                        red_bottom = red_bottom.max(y + 1);
                    }
                    let in_first = x >= first.0 && x < first.2 && y >= first.1 && y < first.3;
                    // The seed (green) and the wash (blue) stay in the
                    // segment they sweep and tint.
                    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
                    assert!(in_first || g <= r.max(b) + 24, "{at}: the seed at ({x}, {y}), outside its segment");
                    assert!(in_first || b <= r.max(g) + 24, "{at}: the wash at ({x}, {y}), outside its segment");
                }
            }
            assert!(red_bottom > red_top, "{at}: the pill shows");
            assert!(
                red_bottom - red_top <= 24 * scale,
                "{at}: the pill spans rows {red_top}..{red_bottom}, more than a segment"
            );
            pixels
        };
        for step in 0..=20 {
            let t = begin + LONG * f64::from(step) / 20.0;
            check(t, &format!("{scale}×, at {step}/20"));
        }
        // Settled: what drawing the switch still would.
        let settled = check(begin + LONG * 2.0, &format!("{scale}×, settled"));
        for layer in [pill.layer().unwrap(), seed_layer.clone(), wash.layer().unwrap()] {
            layer.removeAllAnimations();
        }
        page.setNeedsDisplay(true);
        container.setNeedsDisplay(true);
        show(&w);
        testing::composite_at(&w, begin + LONG * 2.0);
        let (_, _, still) = testing::window_pixels(&w).expect("captured pixels");
        assert!(settled == still, "{scale}×: the settled switch differs from drawing it still");
        w.close();
        testing::settle();
    }

    fn opacity_and_transitions(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let root = painter.layer().expect("a layer");
        let blue = blue_square(&root);
        show(&w);
        // A layer committed in a window's tree animates implicitly: its
        // opacity, from 1 to 0 over the transaction's duration.
        CATransaction::begin();
        CATransaction::setAnimationDuration(LONG);
        CATransaction::setAnimationTimingFunction(Some(&linear()));
        blue.setOpacity(0.0);
        CATransaction::commit();
        testing::settle();
        let begin =
            unsafe { blue.animationForKey(&NSString::from_str("opacity")) }.expect("an implicit animation").beginTime();
        testing::composite_at(&w, begin + LONG / 2.0);
        // Half blue over red.
        near(px(&w, 20, 80), [128, 0, 128, 255], 3);
        testing::composite_at(&w, begin + 2.0 * LONG);
        near(px(&w, 20, 80), [255, 0, 0, 255], 0);
        // Showing it again with its opacity back, then hiding it: a fade
        // transition.
        blue.removeAllAnimations();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        blue.setOpacity(1.0);
        CATransaction::commit();
        testing::settle();
        CATransaction::begin();
        CATransaction::setAnimationDuration(LONG);
        CATransaction::setAnimationTimingFunction(Some(&linear()));
        blue.setHidden(true);
        CATransaction::commit();
        testing::settle();
        let t = unsafe { blue.animationForKey(&NSString::from_str("transition")) }.expect("a transition").beginTime();
        testing::composite_at(&w, t + LONG / 2.0);
        near(px(&w, 20, 80), [128, 0, 128, 255], 3);
        testing::composite_at(&w, t + 2.0 * LONG);
        near(px(&w, 20, 80), [255, 0, 0, 255], 0);
        blue.removeFromSuperlayer();
        w.close();
        testing::settle();
    }

    fn masks_hide_what_they_leave_out(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let root = painter.layer().expect("a layer");
        // A 40 × 40 blue layer masked by its left half.
        let l = CALayer::new();
        l.setFrame(cg(10.0, 10.0, 40.0, 40.0));
        l.setBackgroundColor(Some(&CGColor::new_srgb(0.0, 0.0, 1.0, 1.0)));
        let mask = CALayer::new();
        mask.setFrame(cg(0.0, 0.0, 20.0, 40.0));
        mask.setBackgroundColor(Some(&CGColor::new_srgb(0.0, 0.0, 0.0, 1.0)));
        // SAFETY: a layer as the mask.
        unsafe { l.setMask(Some(&mask)) };
        root.addSublayer(&l);
        show(&w);
        // Layer x 10 to 30 shows, 30 to 50 doesn't (y 10 to 50 from the
        // bottom: rows 50 to 90).
        near(px(&w, 20, 70), [0, 0, 255, 255], 0);
        near(px(&w, 40, 70), [255, 0, 0, 255], 0);
        l.removeFromSuperlayer();
        w.close();
        testing::settle();
    }

    fn views_update_their_layers(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        // Added after the window showed: its layer displays at the next
        // commit, through updateLayer.
        let u: Retained<Updater> = {
            let this = Updater::alloc(mtm).set_ivars(Cell::new(0));
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: rect(100.0, 10.0, 40.0, 40.0)] }
        };
        u.setWantsLayer(true);
        painter.addSubview(&u);
        show(&w);
        assert_eq!(u.ivars().get(), 1);
        near(px(&w, 120, 70), [0, 255, 0, 255], 0);
        // Again when it needs display.
        u.setNeedsDisplay(true);
        show(&w);
        assert_eq!(u.ivars().get(), 2);
        u.removeFromSuperview();
        w.close();
        testing::settle();
    }

    fn kept_animations_end_once(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let root = painter.layer().expect("a layer");
        let blue = blue_square(&root);
        show(&w);
        let watcher = target(mtm);
        // A short one, on the clock, kept when it ends.
        let a = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
        // SAFETY: numbers as values.
        unsafe {
            a.setFromValue(Some(&objc2_foundation::NSNumber::new_f64(0.0)));
            a.setToValue(Some(&objc2_foundation::NSNumber::new_f64(0.5)));
        }
        a.setDuration(0.05);
        a.setRemovedOnCompletion(false);
        // SAFETY: the constant is a string.
        a.setFillMode(unsafe { kCAFillModeForwards });
        let _: () = unsafe { msg_send![&*a, setDelegate: &*watcher] };
        blue.addAnimation_forKey(&a, Some(&NSString::from_str("kept")));
        testing::commit_layers();
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while watcher.ivars().stops.get() == 0 && std::time::Instant::now() < give_up {
            testing::run_for(10);
        }
        // Its delegate heard once; no timer waits for it any more (it would
        // go off at once, again and again, keeping the main thread busy).
        assert_eq!(watcher.ivars().stops.get(), 1);
        assert!(!testing::layer_endings_scheduled());
        testing::run_for(20);
        assert_eq!(watcher.ivars().stops.get(), 1);
        assert!(unsafe { blue.animationForKey(&NSString::from_str("kept")) }.is_some());
        blue.removeFromSuperlayer();
        w.close();
        testing::settle();
    }

    fn still_layers_want_no_frames(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let root = painter.layer().expect("a layer");
        let turn = || {
            let a = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("transform")));
            // SAFETY: transforms as values.
            unsafe {
                a.setToValue(Some(&NSValue::valueWithCATransform3D(CATransform3D::new_rotation(3.0, 0.0, 0.0, 1.0))));
            }
            a.setDuration(1.0);
            a.setRepeatCount(f32::INFINITY);
            a
        };
        // A paused layer's animation stands still: no frames.
        let paused = blue_square(&root);
        paused.setSpeed(0.0);
        paused.addAnimation_forKey(&turn(), Some(&NSString::from_str("spin")));
        // A hidden one's shows nothing: no frames.
        let hidden = blue_square(&root);
        hidden.setHidden(true);
        hidden.addAnimation_forKey(&turn(), Some(&NSString::from_str("spin")));
        show(&w);
        testing::composite_at(&w, testing::media_time());
        assert!(!testing::layers_want_frame(&w), "nothing that shows moves");
        // One that shows and runs wants them.
        let running = blue_square(&root);
        running.addAnimation_forKey(&turn(), Some(&NSString::from_str("spin")));
        show(&w);
        assert!(testing::layers_want_frame(&w));
        for l in [paused, hidden, running] {
            l.removeFromSuperlayer();
        }
        w.close();
        testing::settle();
    }

    fn layers_and_views_go(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let root = painter.layer().expect("a layer");
        // A view added and dropped in the same turn: its layer (still
        // waiting to be committed) doesn't reach for it.
        // (In a pool of its own, so nothing it autoreleased keeps it.)
        let kept_layer = objc2::rc::autoreleasepool(|_| {
            let v = Painter::new(mtm, rect(0.0, 0.0, 20.0, 20.0), [0.0, 0.0, 1.0]);
            v.setWantsLayer(true);
            painter.addSubview(&v);
            let l = v.layer().expect("a layer");
            v.removeFromSuperview();
            l
        });
        show(&w);
        assert!(kept_layer.delegate().is_none(), "its view is gone");
        kept_layer.setNeedsDisplay();
        kept_layer.displayIfNeeded();
        drop(kept_layer);
        // A layer whose presentation layer was asked for goes when the
        // program lets it go, and the render thread forgets it.
        let before = objc2::rc::autoreleasepool(|_| {
            let l = blue_square(&root);
            show(&w);
            let before = testing::render_layer_count();
            assert!(unsafe { l.presentationLayer() }.is_some());
            l.removeFromSuperlayer();
            before
        });
        show(&w);
        assert!(testing::render_layer_count() < before, "the render thread let it go");
        w.close();
        testing::settle();
    }

    fn display_links_follow_frames(mtm: MainThreadMarker) {
        let (w, painter) = shown(mtm);
        let id = testing::showing_id(&w);
        let target = target(mtm);
        let _ = testing::take_render_log();
        // SAFETY: the target answers tick:.
        let link = unsafe { painter.displayLinkWithTarget_selector(&target, sel!(tick:)) };
        assert_eq!(link.duration(), 0.0, "no frame yet");
        // SAFETY: adding to the main run loop in the common modes.
        unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        testing::settle();
        assert!(testing::take_render_log().contains(&Seen::FrameTicks { window: id, on: true }));
        testing::inject_tick(id, 12.5);
        testing::settle();
        assert_eq!(target.ivars().ticks.get(), 1);
        assert_eq!(link.timestamp(), 12.5);
        assert!(link.duration() > 0.0);
        link.setPaused(true);
        testing::settle();
        assert!(testing::take_render_log().contains(&Seen::FrameTicks { window: id, on: false }));
        testing::inject_tick(id, 13.0);
        testing::settle();
        assert_eq!(target.ivars().ticks.get(), 1, "a paused link doesn't tick");
        link.invalidate();
        // A window's link follows that window's frames, kept alive by the
        // run loop while it's in one.
        {
            // SAFETY: the target answers tick:.
            let link = unsafe { w.displayLinkWithTarget_selector(&target, sel!(tick:)) };
            // SAFETY: adding to the main run loop in the common modes.
            unsafe { link.addToRunLoop_forMode(&NSRunLoop::mainRunLoop(), NSRunLoopCommonModes) };
        }
        testing::settle();
        assert!(testing::take_render_log().contains(&Seen::FrameTicks { window: id, on: true }));
        // Two frames the loop hears at once: the latest counts.
        testing::inject_tick(id, 14.0);
        testing::inject_tick(id, 14.5);
        testing::settle();
        assert_eq!(target.ivars().ticks.get(), 2);
        // A screen's link ticks at the screen's rate, from a timer.
        if let Some(screen) = objc2_app_kit::NSScreen::mainScreen(mtm) {
            // SAFETY: the target answers tick:.
            let link = unsafe { screen.displayLinkWithTarget_selector(&target, sel!(tick:)) };
            assert_eq!(link.duration(), 0.0);
            link.invalidate();
        }
        w.close();
        testing::settle();
    }

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::use_null_backend();
        testing::capture_pixels(true);
        // SAFETY: the name is a constant string.
        let aqua = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }).expect("Aqua");
        NSApplication::sharedApplication(mtm).setAppearance(Some(&aqua));
        let tests: &[Test] = &[
            ("asynchronous_pixel_crops", asynchronous_pixel_crops),
            ("views_and_layers_composite", views_and_layers_composite),
            ("animations_draw_on_the_render_thread", animations_draw_on_the_render_thread),
            (
                "animator_interpolates_without_enabling_implicit_changes",
                animator_interpolates_without_enabling_implicit_changes,
            ),
            ("animator_resizes_and_completes_while_tracking", animator_resizes_and_completes_while_tracking),
            ("switch_moves_without_trails", switch_moves_without_trails),
            ("opacity_and_transitions", opacity_and_transitions),
            ("masks_hide_what_they_leave_out", masks_hide_what_they_leave_out),
            ("views_update_their_layers", views_update_their_layers),
            ("kept_animations_end_once", kept_animations_end_once),
            ("still_layers_want_no_frames", still_layers_want_no_frames),
            ("layers_and_views_go", layers_and_views_go),
            ("display_links_follow_frames", display_links_follow_frames),
        ];
        for (name, test) in tests {
            objc2::rc::autoreleasepool(|_| test(mtm));
            println!("test {name} ... ok");
        }
    }
}
