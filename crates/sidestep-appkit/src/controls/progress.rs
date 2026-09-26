//! `NSProgressIndicator`: a bar (determinate or not) or a spinner.
//!
//! Values stay between the limits as they're set, and moving a limit
//! leaves the value where it is, as AppKit does. Sizes follow the control
//! size (`conformance/tests/controls.rs`, `progress_indicators`).
//!
//! Animation runs on the window's frame clock: after each frame the
//! render thread shows, every animating indicator in that window that is
//! on screen and not hidden asks to be redrawn, and draws at a phase taken
//! from the time. So an indicator animates at the display's rate while its
//! window is shown, and nothing runs at all while it's stopped, hidden or
//! out of a window, or while its window isn't shown (no frames come). A
//! change that shows it again (unhiding, joining a window, the window
//! being shown) redraws it, which brings the next frame and starts the
//! clock again.

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::time::Instant;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSControlSize, NSControlTint, NSProgressIndicatorStyle, NSResponder, NSView};
use objc2_foundation::{NSRect, NSSize};

use crate::theme::{self, metrics, parts};
use crate::views::{self, NSViewImpl};
use crate::window::NSWindowImpl;

thread_local! {
    /// Indicators between `startAnimation:` and `stopAnimation:`.
    static ANIMATING: RefCell<Vec<Weak<NSView>>> = const { RefCell::new(Vec::new()) };
}

/// The phase of an animation that repeats every `period` seconds, from 0
/// to 1: the same for every indicator, from one clock.
fn phase(period: f64) -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    (t / period).fract()
}

/// After `window` showed a frame: redraw its animating indicators that
/// are showing. Called by the window for each frame the render thread
/// shows.
pub(crate) fn frame(window: &NSWindowImpl) {
    let live: Vec<Retained<NSView>> = ANIMATING.with(|a| {
        let mut animating = a.borrow_mut();
        animating.retain(|w| w.load().is_some());
        animating.iter().filter_map(Weak::load).collect()
    });
    for view in live {
        let v = views::imp(&view);
        let here = views::window_of(v).is_some_and(|w| std::ptr::eq(w, window));
        if here && !views::is_hidden_or_has_hidden_ancestor(v) {
            view.setNeedsDisplay(true);
        }
    }
}

pub(crate) struct ProgressIvars {
    style: Cell<NSProgressIndicatorStyle>,
    indeterminate: Cell<bool>,
    bezeled: Cell<bool>,
    size: Cell<NSControlSize>,
    tint: Cell<NSControlTint>,
    min: Cell<f64>,
    max: Cell<f64>,
    value: Cell<f64>,
    animating: Cell<bool>,
    displayed_when_stopped: Cell<bool>,
    threaded: Cell<bool>,
    delay: Cell<f64>,
    observed: RefCell<Option<Retained<AnyObject>>>,
    /// Steps taken with `animate:`, for indicators animated by hand.
    steps: Cell<u32>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSProgressIndicator"]
    #[ivars = ProgressIvars]
    pub(crate) struct NSProgressIndicatorImpl;

    impl NSProgressIndicatorImpl {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ProgressIvars {
                style: Cell::new(NSProgressIndicatorStyle::Bar),
                indeterminate: Cell::new(true),
                bezeled: Cell::new(true),
                size: Cell::new(NSControlSize::Regular),
                tint: Cell::new(NSControlTint::DefaultControlTint),
                min: Cell::new(0.0),
                max: Cell::new(100.0),
                value: Cell::new(0.0),
                animating: Cell::new(false),
                displayed_when_stopped: Cell::new(true),
                threaded: Cell::new(true),
                delay: Cell::new(5.0 / 60.0),
                observed: RefCell::new(None),
                steps: Cell::new(0),
            });
            // SAFETY: NSView's designated initializer.
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(style))]
        fn style(&self) -> NSProgressIndicatorStyle {
            self.ivars().style.get()
        }

        #[unsafe(method(setStyle:))]
        fn set_style(&self, style: NSProgressIndicatorStyle) {
            self.ivars().style.set(style);
            self.redraw();
        }

        #[unsafe(method(isIndeterminate))]
        fn is_indeterminate(&self) -> bool {
            self.ivars().indeterminate.get()
        }

        #[unsafe(method(setIndeterminate:))]
        fn set_indeterminate(&self, flag: bool) {
            self.ivars().indeterminate.set(flag);
            self.redraw();
        }

        #[unsafe(method(isBezeled))]
        fn is_bezeled(&self) -> bool {
            self.ivars().bezeled.get()
        }

        #[unsafe(method(setBezeled:))]
        fn set_bezeled(&self, flag: bool) {
            self.ivars().bezeled.set(flag);
            self.redraw();
        }

        #[unsafe(method(controlSize))]
        fn control_size(&self) -> NSControlSize {
            self.ivars().size.get()
        }

        #[unsafe(method(setControlSize:))]
        fn set_control_size(&self, size: NSControlSize) {
            self.ivars().size.set(size);
            self.redraw();
        }

        #[unsafe(method(controlTint))]
        fn control_tint(&self) -> NSControlTint {
            self.ivars().tint.get()
        }

        #[unsafe(method(setControlTint:))]
        fn set_control_tint(&self, tint: NSControlTint) {
            self.ivars().tint.set(tint);
        }

        #[unsafe(method(minValue))]
        fn min_value(&self) -> f64 {
            self.ivars().min.get()
        }

        #[unsafe(method(setMinValue:))]
        fn set_min_value(&self, value: f64) {
            self.ivars().min.set(value);
            self.redraw();
        }

        #[unsafe(method(maxValue))]
        fn max_value(&self) -> f64 {
            self.ivars().max.get()
        }

        #[unsafe(method(setMaxValue:))]
        fn set_max_value(&self, value: f64) {
            self.ivars().max.set(value);
            self.redraw();
        }

        #[unsafe(method(doubleValue))]
        fn double_value(&self) -> f64 {
            self.ivars().value.get()
        }

        #[unsafe(method(setDoubleValue:))]
        fn set_double_value(&self, value: f64) {
            self.set_value(value);
        }

        #[unsafe(method(incrementBy:))]
        fn increment_by(&self, delta: f64) {
            self.set_value(self.ivars().value.get() + delta);
        }

        #[unsafe(method_id(observedProgress))]
        fn observed_progress(&self) -> Option<Retained<AnyObject>> {
            self.ivars().observed.borrow().clone()
        }

        #[unsafe(method(setObservedProgress:))]
        fn set_observed_progress(&self, progress: Option<&AnyObject>) {
            self.ivars().observed.replace(progress.map(|p| p.retain()));
        }

        #[unsafe(method(usesThreadedAnimation))]
        fn uses_threaded_animation(&self) -> bool {
            self.ivars().threaded.get()
        }

        #[unsafe(method(setUsesThreadedAnimation:))]
        fn set_uses_threaded_animation(&self, flag: bool) {
            self.ivars().threaded.set(flag);
        }

        #[unsafe(method(animationDelay))]
        fn animation_delay(&self) -> f64 {
            self.ivars().delay.get()
        }

        #[unsafe(method(setAnimationDelay:))]
        fn set_animation_delay(&self, delay: f64) {
            self.ivars().delay.set(delay);
        }

        #[unsafe(method(startAnimation:))]
        fn start_animation(&self, _sender: Option<&AnyObject>) {
            if self.ivars().animating.replace(true) {
                return;
            }
            let view = self.as_view();
            ANIMATING.with(|a| a.borrow_mut().push(Weak::new(view)));
            // A first redraw brings the frames that keep it going.
            view.setNeedsDisplay(true);
        }

        #[unsafe(method(stopAnimation:))]
        fn stop_animation(&self, _sender: Option<&AnyObject>) {
            if !self.ivars().animating.replace(false) {
                return;
            }
            let me: *const NSView = self.as_view();
            let gone: Vec<Weak<NSView>> = ANIMATING.with(|a| {
                let mut animating = a.borrow_mut();
                let (gone, kept) = animating.drain(..).partition(|w| w.load().is_none_or(|v| std::ptr::eq(&*v, me)));
                *animating = kept;
                gone
            });
            drop(gone);
            self.as_view().setNeedsDisplay(true);
        }

        #[unsafe(method(animate:))]
        fn animate(&self, _sender: Option<&AnyObject>) {
            self.ivars().steps.set(self.ivars().steps.get().wrapping_add(1));
            self.redraw();
        }

        #[unsafe(method(isDisplayedWhenStopped))]
        fn is_displayed_when_stopped(&self) -> bool {
            self.ivars().displayed_when_stopped.get()
        }

        #[unsafe(method(setDisplayedWhenStopped:))]
        fn set_displayed_when_stopped(&self, flag: bool) {
            self.ivars().displayed_when_stopped.set(flag);
            self.redraw();
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            let i = metrics::size_index(self.ivars().size.get());
            if self.ivars().style.get() == NSProgressIndicatorStyle::Spinning {
                NSSize::new(metrics::SPINNER[i], metrics::SPINNER[i])
            } else {
                NSSize::new(super::control::NO_METRIC, metrics::BAR_HEIGHT[i])
            }
        }

        #[unsafe(method(sizeToFit))]
        fn size_to_fit(&self) {
            // SAFETY: intrinsicContentSize takes nothing and returns a size.
            let size: NSSize = unsafe { msg_send![self, intrinsicContentSize] };
            let current = views::frame(self.view_imp()).size;
            let width = if size.width < 0.0 { current.width } else { size.width };
            self.as_view().setFrameSize(NSSize::new(width, size.height));
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw(self);
        }
    }

    unsafe impl NSObjectProtocol for NSProgressIndicatorImpl {}
);

impl NSProgressIndicatorImpl {
    fn as_view(&self) -> &NSView {
        // SAFETY: NSProgressIndicator is a subclass of NSView.
        unsafe { &*(self as *const Self).cast::<NSView>() }
    }

    fn view_imp(&self) -> &NSViewImpl {
        views::imp(self.as_view())
    }

    fn redraw(&self) {
        self.as_view().setNeedsDisplay(true);
    }

    fn set_value(&self, value: f64) {
        let (min, max) = (self.ivars().min.get(), self.ivars().max.get());
        let value = if value > max {
            max
        } else if value < min {
            min
        } else {
            value
        };
        if self.ivars().value.replace(value) != value {
            self.redraw();
        }
    }
}

impl Drop for ProgressIvars {
    fn drop(&mut self) {
        // Forget dead indicators; the list holds them weakly.
        let _ = ANIMATING.try_with(|a| {
            if let Ok(mut animating) = a.try_borrow_mut() {
                animating.retain(|w| w.load().is_some());
            }
        });
    }
}

fn draw(p: &NSProgressIndicatorImpl) {
    if !theme::paint::recording() {
        return;
    }
    let ivars = p.ivars();
    let animating = ivars.animating.get();
    if !animating && !ivars.displayed_when_stopped.get() && ivars.indeterminate.get() {
        return;
    }
    let palette = theme::palette();
    let bounds = views::bounds(p.view_imp());
    let state = parts::State::default();
    let range = ivars.max.get() - ivars.min.get();
    let fraction = if range > 0.0 { (ivars.value.get() - ivars.min.get()) / range } else { 0.0 };
    let steps = f64::from(ivars.steps.get()) / 12.0;
    if ivars.style.get() == NSProgressIndicatorStyle::Spinning {
        let r = parts::centered_square(bounds, bounds.size.width.min(bounds.size.height));
        if ivars.indeterminate.get() {
            let t = if animating { phase(1.0) } else { steps.fract() };
            parts::spinner(palette, r, t, state);
        } else {
            // A determinate spinner fills its ring as the value grows.
            let c = objc2_foundation::NSPoint::new(r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0);
            let width = (r.size.width / 8.0).max(1.5);
            let radius = r.size.width / 2.0 - width;
            theme::paint::stroke_ellipse(
                parts::centered_square(r, 2.0 * (radius + width / 2.0)),
                width,
                theme::palette::faded(palette.label, 0.15),
            );
            theme::paint::stroke_arc(
                c,
                radius,
                0.0,
                fraction.clamp(0.0, 1.0) * std::f64::consts::TAU,
                width,
                palette.accent,
            );
        }
    } else if ivars.indeterminate.get() {
        if animating {
            parts::progress_pulse(palette, bounds, phase(1.5), state);
        } else {
            parts::progress_bar(palette, bounds, 0.0, state);
        }
    } else {
        parts::progress_bar(palette, bounds, fraction, state);
    }
}
