//! `NSEvent`: made on the main thread from the render thread's input, or by
//! a program with the `…EventWithType:` constructors.
//!
//! Scrolling follows AppKit: touchpads give precise deltas in points; a
//! wheel gives imprecise ones counted in lines, [`WHEEL_LINES`] a detent,
//! with fractions from high-resolution wheels. Positive deltas scroll
//! toward the top and the left of the content.
//!
//! `keyCode` is the XKB keycode (see `backend::keyboard`).

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventPhase, NSEventType, NSWindow};
use objc2_foundation::{NSCopying, NSPoint, NSString};

use crate::protocol::{Key, Modifiers};

/// Lines a wheel detent scrolls.
pub(crate) const WHEEL_LINES: f64 = 3.0;

thread_local! {
    /// The modifier flags as of the latest input event.
    static FLAGS: Cell<Modifiers> = const { Cell::new(0) };
    /// Mouse buttons held, bit n for button n.
    static BUTTONS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn set_current_flags(flags: Modifiers) {
    FLAGS.with(|f| f.set(flags));
}

pub(crate) fn current_flags() -> Modifiers {
    FLAGS.with(Cell::get)
}

pub(crate) fn set_button_down(button: isize, down: bool) {
    let bit = 1usize << button.clamp(0, 31);
    BUTTONS.with(|b| b.set(if down { b.get() | bit } else { b.get() & !bit }));
}

pub(crate) struct EventIvars {
    kind: NSEventType,
    location: NSPoint,
    modifiers: NSEventModifierFlags,
    timestamp: f64,
    window: Option<Retained<NSWindow>>,
    window_number: isize,
    button: isize,
    clicks: isize,
    pressure: f32,
    /// `deltaX`, `deltaY`: lines for wheels, points / 10 for touchpads.
    delta: (f64, f64),
    /// `scrollingDeltaX`, `scrollingDeltaY`: lines or points.
    scrolling: (f64, f64),
    precise: bool,
    characters: Option<Retained<NSString>>,
    unmodified: Option<Retained<NSString>>,
    repeat: bool,
    key_code: u16,
}

impl EventIvars {
    fn new(kind: NSEventType, location: NSPoint, modifiers: Modifiers, window: Option<&NSWindow>) -> Self {
        EventIvars {
            kind,
            location,
            modifiers: NSEventModifierFlags(modifiers),
            timestamp: uptime(),
            window: window.map(|w| w.retain()),
            window_number: window.map_or(0, |w| crate::window::imp(w).id() as isize),
            button: 0,
            clicks: 0,
            pressure: 0.0,
            delta: (0.0, 0.0),
            scrolling: (0.0, 0.0),
            precise: false,
            characters: None,
            unmodified: None,
            repeat: false,
            key_code: 0,
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSEvent"]
    #[ivars = EventIvars]
    pub(crate) struct NSEventImpl;

    impl NSEventImpl {
        #[unsafe(method_id(keyEventWithType:location:modifierFlags:timestamp:windowNumber:context:characters:charactersIgnoringModifiers:isARepeat:keyCode:))]
        #[allow(clippy::too_many_arguments)]
        fn key_event_with_type(
            kind: NSEventType,
            location: NSPoint,
            flags: NSEventModifierFlags,
            time: f64,
            window_number: isize,
            _context: Option<&AnyObject>,
            characters: &NSString,
            unmodified: &NSString,
            repeat: bool,
            code: u16,
        ) -> Option<Retained<NSEvent>> {
            let mut ivars = EventIvars::new(kind, location, flags.0, None);
            ivars.timestamp = time;
            ivars.window_number = window_number;
            ivars.characters = Some(characters.copy());
            ivars.unmodified = Some(unmodified.copy());
            ivars.repeat = repeat;
            ivars.key_code = code;
            Some(make(ivars))
        }

        #[unsafe(method_id(mouseEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:clickCount:pressure:))]
        #[allow(clippy::too_many_arguments)]
        fn mouse_event_with_type(
            kind: NSEventType,
            location: NSPoint,
            flags: NSEventModifierFlags,
            time: f64,
            window_number: isize,
            _context: Option<&AnyObject>,
            _event_number: isize,
            clicks: isize,
            pressure: f32,
        ) -> Option<Retained<NSEvent>> {
            let mut ivars = EventIvars::new(kind, location, flags.0, None);
            ivars.timestamp = time;
            ivars.window_number = window_number;
            ivars.clicks = clicks;
            ivars.pressure = pressure;
            Some(make(ivars))
        }

        #[unsafe(method(modifierFlags))]
        fn current_modifier_flags() -> NSEventModifierFlags {
            NSEventModifierFlags(FLAGS.with(Cell::get))
        }

        #[unsafe(method(pressedMouseButtons))]
        fn pressed_mouse_buttons() -> usize {
            BUTTONS.with(Cell::get)
        }

        #[unsafe(method(doubleClickInterval))]
        fn double_click_interval() -> f64 {
            crate::backend::DOUBLE_CLICK_MS as f64 / 1000.0
        }

        #[unsafe(method(keyRepeatDelay))]
        fn key_repeat_delay() -> f64 {
            crate::backend::key_repeat().0
        }

        #[unsafe(method(keyRepeatInterval))]
        fn key_repeat_interval() -> f64 {
            // AppKit has no "doesn't repeat"; a long interval stands in.
            match crate::backend::key_repeat().1 {
                0.0 => 1.0,
                interval => interval,
            }
        }

        #[unsafe(method(type))]
        fn kind(&self) -> NSEventType {
            self.ivars().kind
        }

        #[unsafe(method(modifierFlags))]
        fn modifier_flags(&self) -> NSEventModifierFlags {
            self.ivars().modifiers
        }

        #[unsafe(method(timestamp))]
        fn timestamp(&self) -> f64 {
            self.ivars().timestamp
        }

        #[unsafe(method(locationInWindow))]
        fn location_in_window(&self) -> NSPoint {
            self.ivars().location
        }

        #[unsafe(method_id(window))]
        fn window(&self) -> Option<Retained<NSWindow>> {
            let ivars = self.ivars();
            ivars.window.clone().or_else(|| crate::app::window_by_number(ivars.window_number))
        }

        #[unsafe(method(windowNumber))]
        fn window_number(&self) -> isize {
            self.ivars().window_number
        }

        #[unsafe(method(eventNumber))]
        fn event_number(&self) -> isize {
            0
        }

        #[unsafe(method(buttonNumber))]
        fn button_number(&self) -> isize {
            self.ivars().button
        }

        #[unsafe(method(clickCount))]
        fn click_count(&self) -> isize {
            self.ivars().clicks
        }

        #[unsafe(method(pressure))]
        fn pressure(&self) -> f32 {
            self.ivars().pressure
        }

        #[unsafe(method(deltaX))]
        fn delta_x(&self) -> f64 {
            self.ivars().delta.0
        }

        #[unsafe(method(deltaY))]
        fn delta_y(&self) -> f64 {
            self.ivars().delta.1
        }

        #[unsafe(method(deltaZ))]
        fn delta_z(&self) -> f64 {
            0.0
        }

        #[unsafe(method(scrollingDeltaX))]
        fn scrolling_delta_x(&self) -> f64 {
            self.ivars().scrolling.0
        }

        #[unsafe(method(scrollingDeltaY))]
        fn scrolling_delta_y(&self) -> f64 {
            self.ivars().scrolling.1
        }

        #[unsafe(method(hasPreciseScrollingDeltas))]
        fn has_precise_scrolling_deltas(&self) -> bool {
            self.ivars().precise
        }

        #[unsafe(method(phase))]
        fn phase(&self) -> NSEventPhase {
            NSEventPhase::None
        }

        #[unsafe(method(momentumPhase))]
        fn momentum_phase(&self) -> NSEventPhase {
            NSEventPhase::None
        }

        #[unsafe(method(isDirectionInvertedFromDevice))]
        fn is_direction_inverted_from_device(&self) -> bool {
            false
        }

        #[unsafe(method_id(characters))]
        fn characters(&self) -> Option<Retained<NSString>> {
            self.ivars().characters.clone()
        }

        #[unsafe(method_id(charactersIgnoringModifiers))]
        fn characters_ignoring_modifiers(&self) -> Option<Retained<NSString>> {
            self.ivars().unmodified.clone()
        }

        #[unsafe(method(isARepeat))]
        fn is_a_repeat(&self) -> bool {
            self.ivars().repeat
        }

        #[unsafe(method(keyCode))]
        fn key_code(&self) -> u16 {
            self.ivars().key_code
        }
    }

    unsafe impl NSObjectProtocol for NSEventImpl {}
);

/// Seconds since the process started, as event timestamps count.
fn uptime() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn make(ivars: EventIvars) -> Retained<NSEvent> {
    let this = NSEventImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let event: Retained<NSEventImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSEventImpl is the class NSEvent names.
    unsafe { Retained::cast_unchecked(event) }
}

/// A mouse button event, or a move or drag.
pub(crate) fn mouse_event(
    kind: NSEventType,
    location: NSPoint,
    window: &NSWindow,
    button: isize,
    clicks: isize,
    modifiers: Modifiers,
) -> Retained<NSEvent> {
    let mut ivars = EventIvars::new(kind, location, modifiers, Some(window));
    ivars.button = button;
    ivars.clicks = clicks;
    let pressed = matches!(
        kind,
        NSEventType::LeftMouseDown
            | NSEventType::RightMouseDown
            | NSEventType::OtherMouseDown
            | NSEventType::LeftMouseDragged
            | NSEventType::RightMouseDragged
            | NSEventType::OtherMouseDragged
    );
    ivars.pressure = if pressed { 1.0 } else { 0.0 };
    make(ivars)
}

/// A scroll event from `dx`, `dy` in Wayland's directions: points from a
/// touchpad, or detents from a wheel.
pub(crate) fn scroll_event(
    location: NSPoint,
    window: &NSWindow,
    (dx, dy): (f64, f64),
    wheel: bool,
    modifiers: Modifiers,
) -> Retained<NSEvent> {
    let mut ivars = EventIvars::new(NSEventType::ScrollWheel, location, modifiers, Some(window));
    // Wayland counts toward the bottom and the right; AppKit toward the top
    // and the left. (Subtracted from zero, as negating 0 gives -0.)
    let (dx, dy) = (0.0 - dx, 0.0 - dy);
    if wheel {
        let lines = (dx * WHEEL_LINES, dy * WHEEL_LINES);
        ivars.delta = lines;
        ivars.scrolling = lines;
    } else {
        // About a line per 10 points.
        ivars.delta = (dx / 10.0, dy / 10.0);
        ivars.scrolling = (dx, dy);
        ivars.precise = true;
    }
    make(ivars)
}

/// A key down, key up or repeat.
pub(crate) fn key_event(window: &NSWindow, key: Key) -> Retained<NSEvent> {
    let kind = if key.down { NSEventType::KeyDown } else { NSEventType::KeyUp };
    let mut ivars = EventIvars::new(kind, NSPoint::ZERO, key.modifiers, Some(window));
    ivars.characters = Some(NSString::from_str(&key.characters));
    ivars.unmodified = Some(NSString::from_str(&key.unmodified));
    ivars.repeat = key.repeat;
    ivars.key_code = key.code;
    make(ivars)
}

/// The modifier keys changed.
pub(crate) fn flags_changed_event(window: &NSWindow, modifiers: Modifiers, code: u16) -> Retained<NSEvent> {
    let mut ivars = EventIvars::new(NSEventType::FlagsChanged, NSPoint::ZERO, modifiers, Some(window));
    ivars.key_code = code;
    make(ivars)
}
