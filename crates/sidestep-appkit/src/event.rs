//! `NSEvent`, made on the main thread from the render thread's input.

use std::sync::OnceLock;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSWindow};
use objc2_foundation::NSPoint;

pub(crate) struct EventIvars {
    kind: NSEventType,
    location: NSPoint,
    window: Option<Retained<NSWindow>>,
    window_number: isize,
    button: isize,
    delta_y: f64,
    timestamp: f64,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSEvent"]
    #[ivars = EventIvars]
    pub(crate) struct NSEventImpl;

    impl NSEventImpl {
        #[unsafe(method(type))]
        fn kind(&self) -> NSEventType {
            self.ivars().kind
        }

        #[unsafe(method(locationInWindow))]
        fn location_in_window(&self) -> NSPoint {
            self.ivars().location
        }

        #[unsafe(method_id(window))]
        fn window(&self) -> Option<Retained<NSWindow>> {
            self.ivars().window.clone()
        }

        #[unsafe(method(windowNumber))]
        fn window_number(&self) -> isize {
            self.ivars().window_number
        }

        #[unsafe(method(buttonNumber))]
        fn button_number(&self) -> isize {
            self.ivars().button
        }

        #[unsafe(method(clickCount))]
        fn click_count(&self) -> isize {
            1
        }

        #[unsafe(method(modifierFlags))]
        fn modifier_flags(&self) -> NSEventModifierFlags {
            NSEventModifierFlags(0)
        }

        #[unsafe(method(timestamp))]
        fn timestamp(&self) -> f64 {
            self.ivars().timestamp
        }

        #[unsafe(method(deltaX))]
        fn delta_x(&self) -> f64 {
            0.0
        }

        #[unsafe(method(deltaY))]
        fn delta_y(&self) -> f64 {
            // Line-based deltas: about one line per 10 pixels.
            self.ivars().delta_y / 10.0
        }

        #[unsafe(method(scrollingDeltaX))]
        fn scrolling_delta_x(&self) -> f64 {
            0.0
        }

        #[unsafe(method(scrollingDeltaY))]
        fn scrolling_delta_y(&self) -> f64 {
            self.ivars().delta_y
        }

        #[unsafe(method(hasPreciseScrollingDeltas))]
        fn has_precise_scrolling_deltas(&self) -> bool {
            true
        }
    }

    unsafe impl NSObjectProtocol for NSEventImpl {}
);

/// Seconds since the process started, as event timestamps count.
fn uptime() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// A mouse or scroll event. `delta_y` is in pixels, positive toward the top
/// of the content, as AppKit's scrolling deltas are.
pub(crate) fn mouse_event(
    kind: NSEventType,
    location: NSPoint,
    window: &NSWindow,
    button: isize,
    delta_y: f64,
) -> Retained<NSEvent> {
    let ivars = EventIvars {
        kind,
        location,
        window: Some(window.retain()),
        window_number: crate::window::imp(window).id() as isize,
        button,
        delta_y,
        timestamp: uptime(),
    };
    let this = NSEventImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let event: Retained<NSEventImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSEventImpl is the class NSEvent names.
    unsafe { Retained::cast_unchecked(event) }
}
