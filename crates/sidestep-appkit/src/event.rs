//! `NSEvent`: made on the main thread from the render thread's input, or by
//! a program with the `…EventWithType:` constructors.
//!
//! Scrolling follows AppKit: touchpads give precise deltas in points; a
//! wheel gives imprecise ones counted in lines, [`WHEEL_LINES`] a detent,
//! with fractions from high-resolution wheels. Positive deltas scroll
//! toward the top and the left of the content.
//!
//! `keyCode` is the macOS virtual key code for the key (see `keycodes`).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::time::Instant;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSEvent, NSEventMask, NSEventModifierFlags, NSEventPhase, NSEventSubtype, NSEventType, NSTrackingArea, NSWindow,
};
use objc2_foundation::{NSCopying, NSPoint, NSString};

use crate::protocol::{Key, Modifiers};

/// Lines a wheel detent scrolls.
pub(crate) const WHEEL_LINES: f64 = 3.0;

thread_local! {
    /// The modifier flags as of the latest input event.
    static FLAGS: Cell<Modifiers> = const { Cell::new(0) };
    /// Mouse buttons held, bit n for button n.
    static BUTTONS: Cell<usize> = const { Cell::new(0) };
    /// Where the pointer last was over one of the program's windows, in
    /// screen coordinates.
    static MOUSE_LOCATION: Cell<NSPoint> = const { Cell::new(NSPoint::ZERO) };
}

/// A local event monitor: its token, the events it wants and its handler.
type Monitor = (Retained<AnyObject>, NSEventMask, RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>);

thread_local!(static MONITORS: RefCell<Vec<Monitor>> = const { RefCell::new(Vec::new()) });

fn add_monitor(mask: NSEventMask, handler: &DynBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>) -> Retained<AnyObject> {
    let token: Retained<AnyObject> = Retained::into_super(NSObject::new());
    MONITORS.with(|m| m.borrow_mut().push((token.clone(), mask, handler.copy())));
    token
}

/// Run the local monitors on an event about to be sent, in the order they
/// were added: each may pass it on, change it or swallow it (None).
pub(crate) fn monitor(event: &NSEvent) -> Option<Retained<NSEvent>> {
    let mut event = event.retain();
    let kind = event.r#type().0;
    // Handlers may add or remove monitors, so they run from a copy.
    let monitors: Vec<_> = MONITORS.with(|m| {
        m.borrow()
            .iter()
            .filter(|(_, mask, _)| kind < 64 && mask.0 & (1 << kind) != 0)
            .map(|(_, _, h)| h.clone())
            .collect()
    });
    for handler in monitors {
        let next = handler.call((NonNull::from(&*event),));
        // SAFETY: a handler returns an event (autoreleased, or the one it
        // was given) or nil.
        event = unsafe { Retained::retain(next) }?;
    }
    Some(event)
}

pub(crate) fn set_mouse_location(at: NSPoint) {
    MOUSE_LOCATION.with(|m| m.set(at));
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

/// The buttons held, bit n for button n, as input last left them.
pub(crate) fn pressed_buttons() -> usize {
    BUTTONS.with(Cell::get)
}

/// The compositor took the pointer: no button is held for us any more.
pub(crate) fn release_all_buttons() {
    BUTTONS.with(|b| b.set(0));
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
    event_number: isize,
    tracking_area: Option<Retained<NSTrackingArea>>,
    tracking_number: isize,
    user_data: *mut c_void,
    subtype: i16,
    data: (isize, isize),
    phase: NSEventPhase,
    momentum: NSEventPhase,
    /// The device's direction was reversed (natural scrolling).
    inverted: bool,
    magnification: f64,
    rotation: f32,
    /// A key's pending compose sequence (see `Key::composing`).
    composing: Option<String>,
    /// A press that gave its window the keyboard (see
    /// `window_events::mouse_down_reaches`).
    activating: Cell<bool>,
}

impl EventIvars {
    fn new(kind: NSEventType, location: NSPoint, modifiers: Modifiers, window: Option<&NSWindow>) -> Self {
        EventIvars {
            kind,
            location,
            modifiers: NSEventModifierFlags(modifiers),
            timestamp: uptime(),
            window: window.map(|w| w.retain()),
            window_number: window.map_or(0, |w| crate::window::imp(w).number()),
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
            event_number: 0,
            tracking_area: None,
            tracking_number: 0,
            user_data: std::ptr::null_mut(),
            subtype: 0,
            data: (0, 0),
            phase: NSEventPhase::None,
            momentum: NSEventPhase::None,
            inverted: false,
            magnification: 0.0,
            rotation: 0.0,
            composing: None,
            activating: Cell::new(false),
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
            event_number: isize,
            clicks: isize,
            pressure: f32,
        ) -> Option<Retained<NSEvent>> {
            let mut ivars = EventIvars::new(kind, location, flags.0, None);
            ivars.timestamp = time;
            ivars.window_number = window_number;
            ivars.event_number = event_number;
            ivars.clicks = clicks;
            ivars.pressure = pressure;
            Some(make(ivars))
        }

        #[unsafe(method_id(enterExitEventWithType:location:modifierFlags:timestamp:windowNumber:context:eventNumber:trackingNumber:userData:))]
        #[allow(clippy::too_many_arguments)]
        fn enter_exit_event_with_type(
            kind: NSEventType,
            location: NSPoint,
            flags: NSEventModifierFlags,
            time: f64,
            window_number: isize,
            _context: Option<&AnyObject>,
            event_number: isize,
            tracking_number: isize,
            user_data: *mut c_void,
        ) -> Option<Retained<NSEvent>> {
            let mut ivars = EventIvars::new(kind, location, flags.0, None);
            ivars.timestamp = time;
            ivars.window_number = window_number;
            ivars.event_number = event_number;
            ivars.tracking_number = tracking_number;
            ivars.user_data = user_data;
            Some(make(ivars))
        }

        #[unsafe(method_id(otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:))]
        #[allow(clippy::too_many_arguments)]
        fn other_event_with_type(
            kind: NSEventType,
            location: NSPoint,
            flags: NSEventModifierFlags,
            time: f64,
            window_number: isize,
            _context: Option<&AnyObject>,
            subtype: i16,
            data1: isize,
            data2: isize,
        ) -> Option<Retained<NSEvent>> {
            let mut ivars = EventIvars::new(kind, location, flags.0, None);
            ivars.timestamp = time;
            ivars.window_number = window_number;
            ivars.subtype = subtype;
            ivars.data = (data1, data2);
            Some(make(ivars))
        }

        #[unsafe(method_id(addLocalMonitorForEventsMatchingMask:handler:))]
        fn add_local_monitor(mask: NSEventMask, handler: &DynBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>) -> Option<Retained<AnyObject>> {
            Some(add_monitor(mask, handler))
        }

        /// Other programs' events never reach a Wayland client: the
        /// monitor never fires.
        #[unsafe(method_id(addGlobalMonitorForEventsMatchingMask:handler:))]
        fn add_global_monitor(_mask: NSEventMask, _handler: &DynBlock<dyn Fn(NonNull<NSEvent>)>) -> Option<Retained<AnyObject>> {
            Some(Retained::into_super(NSObject::new()))
        }

        #[unsafe(method(removeMonitor:))]
        fn remove_monitor(monitor: &AnyObject) {
            let gone = MONITORS.with(|m| {
                let mut monitors = m.borrow_mut();
                let at = monitors.iter().position(|(token, _, _)| std::ptr::eq(&**token, monitor));
                at.map(|i| monitors.remove(i))
            });
            drop(gone);
        }

        #[unsafe(method(startPeriodicEventsAfterDelay:withPeriod:))]
        fn start_periodic_events(delay: f64, period: f64) {
            crate::event_loop::start_periodic(delay, period);
        }

        #[unsafe(method(stopPeriodicEvents))]
        fn stop_periodic_events() {
            crate::event_loop::stop_periodic();
        }

        #[unsafe(method(mouseLocation))]
        fn mouse_location() -> NSPoint {
            MOUSE_LOCATION.with(Cell::get)
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
            self.ivars().event_number
        }

        #[unsafe(method_id(trackingArea))]
        fn tracking_area(&self) -> Option<Retained<NSTrackingArea>> {
            self.ivars().tracking_area.clone()
        }

        #[unsafe(method(magnification))]
        fn magnification(&self) -> f64 {
            self.ivars().magnification
        }

        #[unsafe(method(rotation))]
        fn rotation(&self) -> f32 {
            self.ivars().rotation
        }

        #[unsafe(method(subtype))]
        fn subtype(&self) -> NSEventSubtype {
            NSEventSubtype(self.ivars().subtype)
        }

        #[unsafe(method(data1))]
        fn data1(&self) -> isize {
            self.ivars().data.0
        }

        #[unsafe(method(data2))]
        fn data2(&self) -> isize {
            self.ivars().data.1
        }

        #[unsafe(method(trackingNumber))]
        fn tracking_number(&self) -> isize {
            self.ivars().tracking_number
        }

        #[unsafe(method(userData))]
        fn user_data(&self) -> *mut c_void {
            self.ivars().user_data
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
            self.ivars().phase
        }

        #[unsafe(method(momentumPhase))]
        fn momentum_phase(&self) -> NSEventPhase {
            self.ivars().momentum
        }

        #[unsafe(method(isDirectionInvertedFromDevice))]
        fn is_direction_inverted_from_device(&self) -> bool {
            self.ivars().inverted
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
    crate::load_shell::<objc2_app_kit::NSEvent>();
    let this = NSEventImpl::alloc().set_ivars(ivars);
    // SAFETY: NSObject's designated initializer.
    let event: Retained<NSEventImpl> = unsafe { msg_send![super(this), init] };
    // SAFETY: NSEventImpl is the class NSEvent names.
    unsafe { Retained::cast_unchecked(event) }
}

/// A periodic event (see `event_loop::start_periodic`).
pub(crate) fn periodic_event() -> Retained<NSEvent> {
    make(EventIvars::new(NSEventType::Periodic, NSPoint::ZERO, current_flags(), None))
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
/// touchpad, or detents from a wheel; in a gesture's `phase`, or coasting
/// after one in `momentum`. `inverted`: the compositor reversed the
/// device's direction (natural scrolling).
///
/// Shift turns a wheel's scroll sideways in the event itself, as macOS
/// does, so every view sees it that way.
pub(crate) fn scroll_event(
    location: NSPoint,
    window: &NSWindow,
    (dx, dy): (f64, f64),
    wheel: bool,
    modifiers: Modifiers,
    (phase, momentum): (NSEventPhase, NSEventPhase),
    inverted: bool,
) -> Retained<NSEvent> {
    let mut ivars = EventIvars::new(NSEventType::ScrollWheel, location, modifiers, Some(window));
    ivars.phase = phase;
    ivars.momentum = momentum;
    ivars.inverted = inverted;
    // Wayland counts toward the bottom and the right; AppKit toward the top
    // and the left. (Subtracted from zero, as negating 0 gives -0.)
    let (mut dx, mut dy) = (0.0 - dx, 0.0 - dy);
    if wheel && dx == 0.0 && NSEventModifierFlags(modifiers).contains(NSEventModifierFlags::Shift) {
        (dx, dy) = (dy, 0.0);
    }
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

/// A mouse-entered, mouse-exited or cursor-update event for a tracking
/// area.
pub(crate) fn tracking_event(
    kind: NSEventType,
    location: NSPoint,
    window: &NSWindow,
    area: &NSTrackingArea,
) -> Retained<NSEvent> {
    let mut ivars = EventIvars::new(kind, location, current_flags(), Some(window));
    let (number, data) = crate::tracking::event_fields(area);
    ivars.tracking_area = Some(area.retain());
    ivars.tracking_number = number;
    ivars.user_data = data;
    make(ivars)
}

/// A magnify or rotate event from a pinch: `amount` is the magnification
/// or the rotation, in degrees counterclockwise.
pub(crate) fn gesture_event(
    kind: NSEventType,
    location: NSPoint,
    window: &NSWindow,
    phase: NSEventPhase,
    amount: f64,
    modifiers: Modifiers,
) -> Retained<NSEvent> {
    let mut ivars = EventIvars::new(kind, location, modifiers, Some(window));
    ivars.phase = phase;
    if kind == NSEventType::Rotate {
        ivars.rotation = amount as f32;
    } else {
        ivars.magnification = amount;
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
    ivars.key_code = crate::keycodes::mac_key_code(key.code);
    ivars.composing = key.composing;
    make(ivars)
}

fn imp(event: &NSEvent) -> &NSEventImpl {
    // SAFETY: every NSEvent is an NSEventImpl.
    unsafe { &*(event as *const NSEvent).cast::<NSEventImpl>() }
}

/// Mark a mouse press as the one that gave its window the keyboard.
pub(crate) fn mark_activating(event: &NSEvent) {
    imp(event).ivars().activating.set(true);
}

/// Whether a mouse press gave its window the keyboard.
pub(crate) fn is_activating(event: &NSEvent) -> bool {
    imp(event).ivars().activating.get()
}

/// The compose sequence a key event left pending, if it touched one.
pub(crate) fn composing(event: &NSEvent) -> Option<&str> {
    // SAFETY: every NSEvent is an NSEventImpl.
    let event = unsafe { &*(event as *const NSEvent).cast::<NSEventImpl>() };
    event.ivars().composing.as_deref()
}

/// The modifier keys changed.
pub(crate) fn flags_changed_event(window: &NSWindow, modifiers: Modifiers, code: u16) -> Retained<NSEvent> {
    let mut ivars = EventIvars::new(NSEventType::FlagsChanged, NSPoint::ZERO, modifiers, Some(window));
    ivars.key_code = crate::keycodes::mac_key_code(code);
    make(ivars)
}
