//! Seats: pointers, keyboards and the cursor.
//!
//! Each wl_seat is bound here rather than through smithay-client-toolkit,
//! at up to version 9, so pointers report high-resolution wheel steps
//! (`axis_value120`); keyboards are translated with [`keyboard`] instead of
//! libxkbcommon.
//!
//! Pointer events over a window's own surface or its scroll tiles go to the
//! main thread in window coordinates (points from the content's top left);
//! over the decorations they stay here ([`decor`]). A pointer frame can
//! leave one of a window's surfaces and enter another, so crossings are
//! settled at the end of the frame: the main thread hears of entering and
//! leaving the window's content, not its surfaces. Clicks are counted here,
//! where the compositor's timestamps are.
//!
//! Keys repeat on this thread, at the compositor's rate and delay, with a
//! calloop timer, and arrive as key-down events marked as repeats.
//!
//! The cursor is a wp_cursor_shape_v1 shape where the compositor offers
//! the protocol, and otherwise an image from the cursor theme
//! (`XCURSOR_THEME`, `XCURSOR_SIZE`) at the window's scale.
//!
//! [`keyboard`]: super::keyboard
//! [`decor`]: super::decor

use std::collections::HashMap;
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use kbvm::{Components, GroupIndex, ModifierMask};
use smithay_client_toolkit::reexports::calloop::RegistrationToken;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_keyboard::{self, KeyState, KeymapFormat, WlKeyboard};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::{self, AxisSource, ButtonState, WlPointer};
use smithay_client_toolkit::reexports::client::protocol::wl_seat::{self, Capability, WlSeat};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use smithay_client_toolkit::registry::RegistryHandler;
use wayland_cursor::CursorTheme;

use super::keyboard::{Compose, Keymap};
use super::{DOUBLE_CLICK_DISTANCE, DOUBLE_CLICK_MS, REPEAT_DELAY_MS, REPEAT_RATE, Role, State, decor, selection};
use crate::protocol::{Button, Cursor, FromRender, Key, Modifiers, ScrollPhase, WindowId};
use smithay_client_toolkit::reexports::protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gesture_pinch_v1::{
    self, ZwpPointerGesturePinchV1,
};
use smithay_client_toolkit::reexports::protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::ZwpPointerGesturesV1;

/// The newest wl_seat this module handles: 8 brought axis_value120, 9 the
/// scroll direction; 10 moves key repeat into the compositor.
const SEAT_VERSION: u32 = 9;

pub struct Seats {
    list: Vec<Seat>,
    cursor_shapes: Option<WpCursorShapeManagerV1>,
    /// Cursor themes by integer scale, loaded when first needed.
    themes: HashMap<u32, CursorTheme>,
    /// The seat and serial of the latest key or button press, which
    /// setting the clipboard, popup grabs and activation need.
    latest: Option<(WlSeat, u32)>,
    /// The program hid the pointer over its content; `Some(true)` until it
    /// next moves.
    hidden: Option<bool>,
    gestures: Option<ZwpPointerGesturesV1>,
    /// Windows that got the keyboard since their last pointer event: a
    /// press on one is the click that activated it (AppKit's first mouse).
    activated: Vec<WindowId>,
}

struct Seat {
    /// The global's name in the registry.
    name: u32,
    seat: WlSeat,
    pointer: Option<Pointer>,
    keyboard: Option<Keyboard>,
}

struct Pointer {
    pointer: WlPointer,
    shape: Option<WpCursorShapeDeviceV1>,
    /// Shows theme cursors when there are no shapes.
    cursor_surface: WlSurface,
    /// The surface under the pointer, with its role.
    focus: Option<(WlSurface, Role)>,
    enter_serial: u32,
    /// Position on the focused surface, in points.
    x: f64,
    y: f64,
    /// The window whose content the main thread was last told the
    /// pointer is in.
    inside: Option<WindowId>,
    /// The cursor last set, and the scale it was set at.
    shown: Option<(Cursor, u32)>,
    /// The cursor is hidden.
    hidden: bool,
    axis: AxisFrame,
    /// A touchpad scroll under way: its recent moves, for the speed its
    /// end leaves.
    gesture: Option<Vec<(u32, f64, f64)>>,
    pinch: Option<ZwpPointerGesturePinchV1>,
    /// The scale a pinch under way last reached.
    pinch_scale: Option<f64>,
    clicks: Clicks,
    /// The compositor reverses the scroll direction (natural scrolling),
    /// horizontally and vertically, as it last said (wl_seat 9).
    inverted: [bool; 2],
}

#[derive(Default)]
struct AxisFrame {
    source: Option<AxisSource>,
    /// Horizontal and vertical.
    value: [f64; 2],
    value120: [i32; 2],
    discrete: [i32; 2],
    /// The fingers left the touchpad.
    stop: bool,
    /// The compositor's time of the frame's axis events, in milliseconds.
    time: u32,
}

/// Moves this recent count toward the speed a touchpad scroll ends with.
const FLICK_WINDOW_MS: u32 = 100;

/// Counting clicks toward double and triple clicks.
#[derive(Default)]
struct Clicks {
    time: u32,
    x: f64,
    y: f64,
    button: u32,
    /// The window, and whether the clicks are on its decorations.
    target: (WindowId, bool),
    count: u32,
}

struct Keyboard {
    keyboard: WlKeyboard,
    keymap: Option<Keymap>,
    compose: Compose,
    mods: Components,
    focus: Option<WindowId>,
    /// The modifier flags last reported.
    flags: Modifiers,
    /// Keys per second, and milliseconds before the first repeat.
    rate: i32,
    delay: i32,
    repeating: Option<(u32, RegistrationToken)>,
    /// The last modifier key pressed or released, for flags-changed events.
    modifier_key: u16,
}

impl Seats {
    pub fn new(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        Seats {
            list: Vec::new(),
            cursor_shapes: globals.bind::<WpCursorShapeManagerV1, _, _>(qh, 1..=1, ()).ok(),
            themes: HashMap::new(),
            latest: None,
            hidden: None,
            gestures: globals.bind::<ZwpPointerGesturesV1, _, _>(qh, 1..=3, ()).ok(),
            activated: Vec::new(),
        }
    }

    /// `window` got the keyboard: its next press is an activating one,
    /// unless another pointer event comes first.
    pub fn focus_gained(&mut self, window: WindowId) {
        if !self.activated.contains(&window) {
            self.activated.push(window);
        }
    }

    /// `window` lost the keyboard, or closed: a press on it later is no
    /// longer the one the keyboard came with.
    pub fn focus_lost(&mut self, window: WindowId) {
        self.activated.retain(|&w| w != window);
    }

    /// A pointer event reached `window`: whether it got the keyboard since
    /// the one before.
    fn pointer_event(&mut self, window: WindowId) -> bool {
        let at = self.activated.iter().position(|&w| w == window);
        at.map(|i| self.activated.swap_remove(i)).is_some()
    }

    pub fn latest_serial(&self) -> Option<(WlSeat, u32)> {
        self.latest.clone()
    }

    fn by_name(&mut self, name: u32) -> Option<&mut Seat> {
        self.list.iter_mut().find(|s| s.name == name)
    }

    /// The window with a keyboard's focus, if one of ours has it.
    pub fn keyboard_window(&self) -> Option<WindowId> {
        self.list.iter().filter_map(|s| s.keyboard.as_ref()).find_map(|k| k.focus)
    }

    /// The current modifier flags, from whichever keyboard has focus.
    fn flags(&self) -> Modifiers {
        self.list.iter().filter_map(|s| s.keyboard.as_ref()).find(|k| k.focus.is_some()).map_or(0, |k| k.flags)
    }
}

/// Bind the seats that exist at startup.
pub(super) fn bind_existing(state: &mut State, globals: &GlobalList) {
    let seats: Vec<(u32, u32)> = globals.contents().with_list(|list| {
        list.iter().filter(|g| g.interface == WlSeat::interface().name).map(|g| (g.name, g.version)).collect()
    });
    for (name, version) in seats {
        add_seat(state, name, version);
    }
}

fn add_seat(state: &mut State, name: u32, version: u32) {
    let version = version.min(SEAT_VERSION).min(WlSeat::interface().version);
    let seat: WlSeat = state.registry.registry().bind(name, version, &state.qh, name);
    selection::add_seat(state, &seat);
    super::textinput::add_seat(state, &seat);
    state.seats.list.push(Seat { name, seat, pointer: None, keyboard: None });
}

impl RegistryHandler<State> for Seats {
    fn new_global(state: &mut State, _: &Connection, _: &QueueHandle<State>, name: u32, interface: &str, version: u32) {
        if interface == WlSeat::interface().name {
            add_seat(state, name, version);
        }
    }

    fn remove_global(state: &mut State, _: &Connection, _: &QueueHandle<State>, name: u32, interface: &str) {
        if interface != WlSeat::interface().name {
            return;
        }
        if let Some(i) = state.seats.list.iter().position(|s| s.name == name) {
            let seat = state.seats.list.remove(i);
            selection::remove_seat(state, &seat.seat);
            super::textinput::remove_seat(state, &seat.seat);
            drop_pointer(state, seat.pointer);
            drop_keyboard(state, seat.keyboard);
            if state.seats.latest.as_ref().is_some_and(|(s, _)| *s == seat.seat) {
                state.seats.latest = None;
            }
            if seat.seat.version() >= 5 {
                seat.seat.release();
            }
        }
    }
}

fn drop_pointer(state: &mut State, pointer: Option<Pointer>) {
    let Some(p) = pointer else { return };
    if let Some(window) = p.inside {
        state.send(FromRender::Leave { window });
    }
    if let Some(shape) = p.shape {
        shape.destroy();
    }
    if let Some(pinch) = p.pinch {
        pinch.destroy();
    }
    p.cursor_surface.destroy();
    if p.pointer.version() >= 3 {
        p.pointer.release();
    }
}

fn drop_keyboard(state: &mut State, keyboard: Option<Keyboard>) {
    let Some(k) = keyboard else { return };
    if let Some((_, token)) = k.repeating {
        state.loop_handle.remove(token);
    }
    if let Some(window) = k.focus {
        state.send(FromRender::Focus { window, focused: false });
    }
    if k.keyboard.version() >= 3 {
        k.keyboard.release();
    }
    if !has_keyboard(state) {
        // Without a keyboard, the activated window stands for the focused one.
        let active: Vec<_> = state.windows.iter().filter(|(_, w)| w.state.activated).map(|(id, _)| *id).collect();
        for window in active {
            state.send(FromRender::Focus { window, focused: true });
        }
    }
}

/// Whether any seat has a keyboard. Without one, Wayland has no keyboard
/// focus, and the window the compositor calls activated is the key window.
pub(super) fn has_keyboard(state: &State) -> bool {
    state.seats.list.iter().any(|s| s.keyboard.is_some())
}

impl Dispatch<WlSeat, u32> for State {
    fn event(
        state: &mut State,
        seat: &WlSeat,
        event: wl_seat::Event,
        name: &u32,
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event else { return };
        let name = *name;
        let shapes = state.seats.cursor_shapes.clone();
        let gestures = state.seats.gestures.clone();
        let Some(entry) = state.seats.by_name(name) else { return };
        let (mut gone_pointer, mut gone_keyboard) = (None, None);
        if caps.contains(Capability::Pointer) {
            if entry.pointer.is_none() {
                let pointer = seat.get_pointer(qh, name);
                let shape = shapes.as_ref().map(|m| m.get_pointer(&pointer, qh, ()));
                let pinch = gestures.as_ref().map(|g| g.get_pinch_gesture(&pointer, qh, name));
                entry.pointer = Some(Pointer {
                    shape,
                    cursor_surface: state.compositor.create_surface(qh),
                    pointer,
                    focus: None,
                    enter_serial: 0,
                    x: 0.0,
                    y: 0.0,
                    inside: None,
                    shown: None,
                    hidden: false,
                    axis: AxisFrame::default(),
                    gesture: None,
                    pinch,
                    pinch_scale: None,
                    clicks: Clicks::default(),
                    inverted: [false; 2],
                });
            }
        } else {
            gone_pointer = entry.pointer.take();
        }
        let Some(entry) = state.seats.by_name(name) else { return };
        if caps.contains(Capability::Keyboard) {
            if entry.keyboard.is_none() {
                entry.keyboard = Some(Keyboard {
                    keyboard: seat.get_keyboard(qh, name),
                    keymap: None,
                    compose: Compose::new(),
                    mods: Components::default(),
                    focus: None,
                    flags: 0,
                    rate: 25,
                    delay: 600,
                    repeating: None,
                    modifier_key: 0,
                });
            }
        } else {
            gone_keyboard = entry.keyboard.take();
        }
        drop_pointer(state, gone_pointer);
        drop_keyboard(state, gone_keyboard);
    }
}

// Pointers.

fn pointer_of(state: &mut State, name: u32) -> Option<&mut Pointer> {
    state.seats.by_name(name)?.pointer.as_mut()
}

fn button(code: u32) -> Button {
    // Linux input codes: BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, then BTN_SIDE and
    // BTN_EXTRA, which mice use for back and forward, as AppKit's buttons
    // 3 and 4 are.
    match code {
        0x110 => Button::Left,
        0x111 => Button::Right,
        0x112 => Button::Other(2),
        0x113 | 0x116 => Button::Other(3),
        0x114 | 0x115 => Button::Other(4),
        c => Button::Other(c.saturating_sub(0x110).min(31) as u8),
    }
}

impl Dispatch<WlPointer, u32> for State {
    fn event(
        state: &mut State,
        pointer: &WlPointer,
        event: wl_pointer::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let name = *name;
        // Before version 5 there are no frames: every event stands alone.
        let framed = pointer.version() >= 5;
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                let role = state.role(&surface);
                let Some(p) = pointer_of(state, name) else { return };
                p.enter_serial = serial;
                p.focus = role.map(|r| (surface, r));
                (p.x, p.y) = (surface_x, surface_y);
                (p.shown, p.hidden) = (None, false);
                if !framed {
                    settle(state, name);
                }
                if let Some(Role::Decor(window, part)) = role {
                    decor::pointer_moved(state, window, part, surface_x, surface_y);
                }
                update_cursor(state, name);
            }
            wl_pointer::Event::Leave { surface, .. } => {
                let Some(p) = pointer_of(state, name) else { return };
                if p.focus.as_ref().is_some_and(|(s, _)| *s == surface) {
                    let left = p.focus.take();
                    if let Some((_, Role::Decor(window, part))) = left {
                        decor::pointer_left(state, window, part);
                    }
                }
                if !framed {
                    settle(state, name);
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                let Some(p) = pointer_of(state, name) else { return };
                (p.x, p.y) = (surface_x, surface_y);
                settle(state, name);
                if let Some(window) = pointer_of(state, name).and_then(|p| p.inside) {
                    state.seats.pointer_event(window);
                }
                moved(state, name);
            }
            wl_pointer::Event::Button { serial, time, button: code, state: WEnum::Value(pressed) } => {
                settle(state, name);
                let pressed = pressed == ButtonState::Pressed;
                if pressed {
                    let seat = state.seats.by_name(name).map(|s| s.seat.clone());
                    state.seats.latest = seat.map(|s| (s, serial));
                }
                pressed_or_released(state, name, serial, time, code, pressed);
            }
            wl_pointer::Event::Axis { time, axis: WEnum::Value(axis), value } => {
                let Some(p) = pointer_of(state, name) else { return };
                p.axis.value[axis_index(axis)] += value;
                p.axis.time = time;
                if !framed {
                    scrolled(state, name);
                }
            }
            wl_pointer::Event::AxisSource { axis_source: WEnum::Value(source) } => {
                if let Some(p) = pointer_of(state, name) {
                    p.axis.source = Some(source);
                }
            }
            wl_pointer::Event::AxisDiscrete { axis: WEnum::Value(axis), discrete } => {
                if let Some(p) = pointer_of(state, name) {
                    p.axis.discrete[axis_index(axis)] += discrete;
                }
            }
            wl_pointer::Event::AxisValue120 { axis: WEnum::Value(axis), value120 } => {
                if let Some(p) = pointer_of(state, name) {
                    p.axis.value120[axis_index(axis)] += value120;
                }
            }
            wl_pointer::Event::AxisStop { time, .. } => {
                if let Some(p) = pointer_of(state, name) {
                    p.axis.stop = true;
                    p.axis.time = time;
                }
                if !framed {
                    scrolled(state, name);
                }
            }
            wl_pointer::Event::AxisRelativeDirection {
                axis: WEnum::Value(axis),
                direction: WEnum::Value(direction),
            } => {
                if let Some(p) = pointer_of(state, name) {
                    p.inverted[axis_index(axis)] = direction == wl_pointer::AxisRelativeDirection::Inverted;
                }
            }
            wl_pointer::Event::Frame => {
                settle(state, name);
                scrolled(state, name);
            }
            _ => {}
        }
    }
}

fn axis_index(axis: wl_pointer::Axis) -> usize {
    match axis {
        wl_pointer::Axis::HorizontalScroll => 0,
        _ => 1,
    }
}

/// Where the pointer is in its window's content, if it's over content.
fn content_position(state: &State, p: &Pointer) -> Option<(WindowId, f64, f64)> {
    let (_, role) = p.focus.as_ref()?;
    match *role {
        Role::Root(window) | Role::Bar(window) | Role::Frame(window) => {
            let (ox, oy) = state.content_offset(*role);
            Some((window, p.x + ox, p.y + oy))
        }
        Role::Decor(..) => None,
    }
}

/// Tell the main thread which window's content the pointer is in, if that
/// changed.
fn settle(state: &mut State, name: u32) {
    let Some(p) = state.seats.list.iter().find(|s| s.name == name).and_then(|s| s.pointer.as_ref()) else {
        return;
    };
    let now = content_position(state, p);
    let before = p.inside;
    if now.map(|n| n.0) == before {
        return;
    }
    if let Some(window) = before {
        state.send(FromRender::Leave { window });
    }
    if let Some((window, x, y)) = now {
        state.send(FromRender::Enter { window, x, y });
    }
    if let Some(p) = pointer_of(state, name) {
        p.inside = now.map(|n| n.0);
    }
}

fn moved(state: &mut State, name: u32) {
    let flags = state.seats.flags();
    let Some(p) = state.seats.list.iter().find(|s| s.name == name).and_then(|s| s.pointer.as_ref()) else {
        return;
    };
    let (x, y) = (p.x, p.y);
    match p.focus.as_ref().map(|(_, r)| *r) {
        Some(Role::Decor(window, part)) => {
            decor::pointer_moved(state, window, part, x, y);
            update_cursor(state, name);
        }
        Some(_) => {
            if let Some((window, x, y)) = content_position(state, p) {
                state.send(FromRender::Motion { window, x, y, modifiers: flags });
            }
            if state.seats.hidden == Some(true) {
                hide_cursor(state, false, false);
            }
        }
        None => {}
    }
}

fn pressed_or_released(state: &mut State, name: u32, serial: u32, time: u32, code: u32, pressed: bool) {
    let flags = state.seats.flags();
    let Some(seat) = state.seats.list.iter().find(|s| s.name == name) else { return };
    let Some(p) = seat.pointer.as_ref() else { return };
    let wl_seat = seat.seat.clone();
    let Some((_, role)) = p.focus.clone() else { return };
    let (window, x, y) = match role {
        Role::Decor(window, _) => (window, p.x, p.y),
        _ => match content_position(state, p) {
            Some(at) => at,
            None => return,
        },
    };
    let Some(p) = pointer_of(state, name) else { return };
    let target = (window, matches!(role, Role::Decor(..)));
    let clicks = if pressed { p.clicks.press(time, x, y, code, target) } else { p.clicks.count.max(1) };
    match role {
        Role::Decor(window, part) => {
            decor::pointer_button(state, window, part, (x, y), code, pressed, clicks, &wl_seat, serial);
        }
        _ => {
            let activating = state.seats.pointer_event(window) && pressed;
            let button = button(code);
            state.send(FromRender::Button { window, x, y, button, pressed, clicks, modifiers: flags, activating });
        }
    }
}

impl Clicks {
    /// Count a press: the next in a series if it's soon enough after and
    /// close enough to the last, with the same button; else the first.
    fn press(&mut self, time: u32, x: f64, y: f64, button: u32, target: (WindowId, bool)) -> u32 {
        let near = (x - self.x).hypot(y - self.y) <= DOUBLE_CLICK_DISTANCE;
        let soon = time.wrapping_sub(self.time) <= DOUBLE_CLICK_MS;
        let same = self.count > 0 && button == self.button && target == self.target;
        self.count = if same && near && soon { self.count + 1 } else { 1 };
        (self.time, self.x, self.y, self.button, self.target) = (time, x, y, button, target);
        self.count
    }
}

fn scrolled(state: &mut State, name: u32) {
    let flags = state.seats.flags();
    let Some(p) = pointer_of(state, name) else { return };
    let axis = std::mem::take(&mut p.axis);
    let moved = axis.value != [0.0; 2] || axis.value120 != [0; 2] || axis.discrete != [0; 2];
    if !moved && !axis.stop {
        return;
    }
    let wheel = matches!(axis.source, Some(AxisSource::Wheel | AxisSource::WheelTilt))
        || (axis.source.is_none() && (axis.value120 != [0; 2] || axis.discrete != [0; 2]));
    // Touchpad scrolls are gestures the fingers' lifting ends.
    let finger = axis.source == Some(AxisSource::Finger) || (p.gesture.is_some() && !wheel);
    let delta = |i: usize| {
        if !wheel {
            axis.value[i]
        } else if axis.value120[i] != 0 {
            axis.value120[i] as f64 / 120.0
        } else if axis.discrete[i] != 0 {
            axis.discrete[i] as f64
        } else {
            // A wheel that reports only distances: compositors move 10 to
            // 15 points per detent.
            axis.value[i] / 15.0
        }
    };
    let (dx, dy) = (delta(0), delta(1));
    // One flag for the event: the axis that moves, the vertical one first.
    let inverted = if dy == 0.0 && dx != 0.0 { p.inverted[0] } else { p.inverted[1] };
    let mut sends = Vec::with_capacity(2);
    if moved {
        let phase = if !finger {
            ScrollPhase::None
        } else if p.gesture.is_none() {
            ScrollPhase::Began
        } else {
            ScrollPhase::Changed
        };
        if finger {
            let recent = p.gesture.get_or_insert_with(Vec::new);
            recent.retain(|(t, _, _)| axis.time.wrapping_sub(*t) <= FLICK_WINDOW_MS);
            recent.push((axis.time, dx, dy));
        }
        sends.push((dx, dy, phase, (0.0, 0.0)));
    }
    if axis.stop
        && let Some(recent) = p.gesture.take()
    {
        sends.push((0.0, 0.0, ScrollPhase::Ended, flick(&recent, axis.time)));
    }
    let Some(p) = state.seats.list.iter().find(|s| s.name == name).and_then(|s| s.pointer.as_ref()) else {
        return;
    };
    if let Some((window, x, y)) = content_position(state, p) {
        for (dx, dy, phase, velocity) in sends {
            let modifiers = flags;
            state.send(FromRender::Scroll { window, x, y, dx, dy, wheel, modifiers, phase, velocity, inverted });
        }
    }
}

impl Dispatch<ZwpPointerGesturesV1, ()> for State {
    fn event(
        _: &mut State,
        _: &ZwpPointerGesturesV1,
        _: <ZwpPointerGesturesV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, u32> for State {
    fn event(
        state: &mut State,
        _: &ZwpPointerGesturePinchV1,
        event: zwp_pointer_gesture_pinch_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let flags = state.seats.flags();
        let Some(p) = pointer_of(state, *name) else { return };
        let (phase, magnification, rotation) = match event {
            zwp_pointer_gesture_pinch_v1::Event::Begin { .. } => {
                p.pinch_scale = Some(1.0);
                (ScrollPhase::Began, 0.0, 0.0)
            }
            zwp_pointer_gesture_pinch_v1::Event::Update { scale, rotation, .. } => {
                let Some(before) = p.pinch_scale.replace(scale) else { return };
                (ScrollPhase::Changed, pinch_step(before, scale), -rotation)
            }
            zwp_pointer_gesture_pinch_v1::Event::End { .. } => {
                if p.pinch_scale.take().is_none() {
                    return;
                }
                (ScrollPhase::Ended, 0.0, 0.0)
            }
            _ => return,
        };
        let Some(p) = state.seats.list.iter().find(|s| s.name == *name).and_then(|s| s.pointer.as_ref()) else {
            return;
        };
        if let Some((window, x, y)) = content_position(state, p) {
            state.send(FromRender::Pinch { window, x, y, phase, magnification, rotation, modifiers: flags });
        }
    }
}

/// AppKit's magnification for a pinch going from scale `before` to `after`:
/// the factor less one, so the factors of a gesture multiply to its scale.
fn pinch_step(before: f64, after: f64) -> f64 {
    if before > 0.0 { after / before - 1.0 } else { 0.0 }
}

/// The speed, in points per second, of the moves in the last
/// [`FLICK_WINDOW_MS`] before `end`.
fn flick(recent: &[(u32, f64, f64)], end: u32) -> (f64, f64) {
    let recent: Vec<_> = recent.iter().filter(|(t, _, _)| end.wrapping_sub(*t) <= FLICK_WINDOW_MS).collect();
    let Some(first) = recent.first() else { return (0.0, 0.0) };
    // At least a frame's time, so one quick move doesn't read as infinite.
    let span = f64::from(end.wrapping_sub(first.0).max(16)) / 1000.0;
    let (sx, sy) = recent.iter().fold((0.0, 0.0), |(x, y), (_, dx, dy)| (x + dx, y + dy));
    (sx / span, sy / span)
}

// Cursors.

/// Show the cursor for what the pointer of seat `name` is over.
fn update_cursor(state: &mut State, name: u32) {
    let hidden = state.seats.hidden.is_some();
    let Some(p) = state.seats.list.iter_mut().find(|s| s.name == name).and_then(|s| s.pointer.as_mut()) else {
        return;
    };
    let Some((_, role)) = p.focus.clone() else { return };
    if hidden && !matches!(role, Role::Decor(..)) {
        if p.shown.is_some() || !p.hidden {
            p.pointer.set_cursor(p.enter_serial, None, 0, 0);
            (p.shown, p.hidden) = (None, true);
        }
        return;
    }
    p.hidden = false;
    let (x, y) = (p.x, p.y);
    let Some(win) = state.windows.get(&role.window()) else { return };
    let cursor = match role {
        Role::Decor(_, part) => decor::cursor(win, part, x, y),
        _ => win.cursor,
    };
    let scale = win.scale.ceil().max(1.0) as u32;
    set_cursor(state, name, cursor, scale);
}

/// The program hides the pointer over its windows' content, or shows it.
pub(super) fn hide_cursor(state: &mut State, hidden: bool, until_moved: bool) {
    state.seats.hidden = hidden.then_some(until_moved);
    let names: Vec<u32> = state.seats.list.iter().filter(|s| s.pointer.is_some()).map(|s| s.name).collect();
    for name in names {
        update_cursor(state, name);
    }
}

fn set_cursor(state: &mut State, name: u32, cursor: Cursor, scale: u32) {
    let seats = &mut state.seats;
    let Some(p) = seats.list.iter_mut().find(|s| s.name == name).and_then(|s| s.pointer.as_mut()) else { return };
    if p.shown == Some((cursor, scale)) {
        return;
    }
    p.shown = Some((cursor, scale));
    if let Some(shape) = &p.shape {
        shape.set_shape(p.enter_serial, to_shape(cursor));
        return;
    }
    // A themed cursor: an image from the theme, at the window's scale.
    let theme = match seats.themes.entry(scale) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => {
            let name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
            let size = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).unwrap_or(24u32);
            match CursorTheme::load_from_name(&state.conn, state.shm.wl_shm().clone(), &name, size * scale) {
                Ok(theme) => e.insert(theme),
                Err(_) => return,
            }
        }
    };
    let mut names = std::iter::once(cursor.name()).chain(cursor.alt_names().iter().copied()).chain(["default"]);
    let Some(name) = names.find(|n| theme.get_cursor(n).is_some()) else { return };
    let Some(image) = theme.get_cursor(name).map(|c| &c[0]) else { return };
    let (w, h) = image.dimensions();
    let (hx, hy) = image.hotspot();
    let surface = &p.cursor_surface;
    surface.set_buffer_scale(scale as i32);
    surface.attach(Some(image), 0, 0);
    surface.damage_buffer(0, 0, w as i32, h as i32);
    surface.commit();
    p.pointer.set_cursor(p.enter_serial, Some(surface), (hx / scale) as i32, (hy / scale) as i32);
}

/// The main thread changed a window's cursor: show it where the pointer
/// is over that window's content.
pub(super) fn cursor_changed(state: &mut State, window: WindowId) {
    let names: Vec<u32> = state
        .seats
        .list
        .iter()
        .filter(|s| s.pointer.as_ref().is_some_and(|p| p.inside == Some(window)))
        .map(|s| s.name)
        .collect();
    for name in names {
        update_cursor(state, name);
    }
}

/// A window's scale changed: its cursor is drawn again at the new one.
pub(super) fn scale_changed(state: &mut State, window: WindowId) {
    let names: Vec<u32> = state
        .seats
        .list
        .iter()
        .filter(|s| s.pointer.as_ref().and_then(|p| p.focus.as_ref()).is_some_and(|(_, r)| r.window() == window))
        .map(|s| s.name)
        .collect();
    for name in names {
        update_cursor(state, name);
    }
}

/// Forget a window that closed.
pub(super) fn window_closed(state: &mut State, window: WindowId) {
    state.seats.focus_lost(window);
    for seat in &mut state.seats.list {
        if let Some(p) = &mut seat.pointer {
            if p.focus.as_ref().is_some_and(|(_, r)| r.window() == window) {
                p.focus = None;
            }
            if p.inside == Some(window) {
                p.inside = None;
            }
        }
        if let Some(k) = &mut seat.keyboard
            && k.focus == Some(window)
        {
            k.focus = None;
            if let Some((_, token)) = k.repeating.take() {
                state.loop_handle.remove(token);
            }
        }
    }
}

fn to_shape(cursor: Cursor) -> Shape {
    match cursor {
        Cursor::ContextMenu => Shape::ContextMenu,
        Cursor::Help => Shape::Help,
        Cursor::Pointer => Shape::Pointer,
        Cursor::Progress => Shape::Progress,
        Cursor::Wait => Shape::Wait,
        Cursor::Cell => Shape::Cell,
        Cursor::Crosshair => Shape::Crosshair,
        Cursor::Text => Shape::Text,
        Cursor::VerticalText => Shape::VerticalText,
        Cursor::Alias => Shape::Alias,
        Cursor::Copy => Shape::Copy,
        Cursor::Move | Cursor::AllResize => Shape::Move,
        Cursor::NoDrop => Shape::NoDrop,
        Cursor::NotAllowed => Shape::NotAllowed,
        Cursor::Grab => Shape::Grab,
        Cursor::Grabbing => Shape::Grabbing,
        Cursor::EResize => Shape::EResize,
        Cursor::NResize => Shape::NResize,
        Cursor::NeResize => Shape::NeResize,
        Cursor::NwResize => Shape::NwResize,
        Cursor::SResize => Shape::SResize,
        Cursor::SeResize => Shape::SeResize,
        Cursor::SwResize => Shape::SwResize,
        Cursor::WResize => Shape::WResize,
        Cursor::EwResize => Shape::EwResize,
        Cursor::NsResize => Shape::NsResize,
        Cursor::NeswResize => Shape::NeswResize,
        Cursor::NwseResize => Shape::NwseResize,
        Cursor::ColResize => Shape::ColResize,
        Cursor::RowResize => Shape::RowResize,
        Cursor::AllScroll => Shape::AllScroll,
        Cursor::ZoomIn => Shape::ZoomIn,
        Cursor::ZoomOut => Shape::ZoomOut,
        _ => Shape::Default,
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &WpCursorShapeManagerV1,
        _: <WpCursorShapeManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WpCursorShapeDeviceV1, ()> for State {
    fn event(
        _: &mut State,
        _: &WpCursorShapeDeviceV1,
        _: <WpCursorShapeDeviceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

// Keyboards.

fn keyboard_of(state: &mut State, name: u32) -> Option<&mut Keyboard> {
    state.seats.by_name(name)?.keyboard.as_mut()
}

impl Dispatch<WlKeyboard, u32> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let name = *name;
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(KeymapFormat::XkbV1), fd, size } => {
                // Read, not mapped: the file is the compositor's and may be
                // shared with other clients, and positioned reads leave its
                // offset alone.
                let file = std::fs::File::from(fd);
                let mut text = vec![0; size as usize];
                let keymap = file.read_exact_at(&mut text, 0).ok().and_then(|()| Keymap::parse(&text));
                if let Some(k) = keyboard_of(state, name) {
                    k.keymap = keymap;
                    k.compose.reset();
                }
            }
            wl_keyboard::Event::Enter { serial, surface, .. } => {
                let Some(window) = state.role(&surface).map(Role::window) else { return };
                let seat = state.seats.by_name(name).map(|s| s.seat.clone());
                state.seats.latest = seat.map(|s| (s, serial));
                if let Some(k) = keyboard_of(state, name) {
                    k.focus = Some(window);
                }
                state.seats.focus_gained(window);
                state.send(FromRender::Focus { window, focused: true });
                selection::focus_changed(state);
            }
            wl_keyboard::Event::Leave { .. } => {
                stop_repeat(state, name);
                let Some(k) = keyboard_of(state, name) else { return };
                k.compose.reset();
                if let Some(window) = k.focus.take() {
                    state.seats.focus_lost(window);
                    state.send(FromRender::Focus { window, focused: false });
                }
            }
            wl_keyboard::Event::Key { serial, key, state: WEnum::Value(key_state), .. } => {
                let down = key_state == KeyState::Pressed;
                if down {
                    let seat = state.seats.by_name(name).map(|s| s.seat.clone());
                    state.seats.latest = seat.map(|s| (s, serial));
                }
                key_event(state, name, key + 8, down);
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                let Some(k) = keyboard_of(state, name) else { return };
                k.mods.mods_pressed = ModifierMask(mods_depressed);
                k.mods.mods_latched = ModifierMask(mods_latched);
                k.mods.mods_locked = ModifierMask(mods_locked);
                k.mods.group_locked = GroupIndex(group);
                k.mods.update_effective();
                let flags = k.keymap.as_ref().map_or(0, |m| m.flags(&k.mods));
                if flags == k.flags {
                    return;
                }
                k.flags = flags;
                let (window, code) = (k.focus, k.modifier_key);
                if let Some(window) = window {
                    state.send(FromRender::Modifiers { window, modifiers: flags, code });
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                REPEAT_RATE.store(rate.max(0) as u32, std::sync::atomic::Ordering::Relaxed);
                REPEAT_DELAY_MS.store(delay.max(0) as u32, std::sync::atomic::Ordering::Relaxed);
                if let Some(k) = keyboard_of(state, name) {
                    (k.rate, k.delay) = (rate, delay);
                }
            }
            _ => {}
        }
    }
}

/// A key pressed or released: tell the main thread, and start or stop
/// repeating it.
fn key_event(state: &mut State, name: u32, code: u32, down: bool) {
    let Some(k) = keyboard_of(state, name) else { return };
    let (Some(window), Some(keymap)) = (k.focus, k.keymap.as_ref()) else { return };
    let compose = if down { Some(&mut k.compose) } else { None };
    let t = keymap.translate(&k.mods, code, compose);
    if t.modifier {
        k.modifier_key = code as u16;
        return;
    }
    let key = Key {
        down,
        repeat: false,
        code: code as u16,
        characters: t.characters,
        unmodified: t.unmodified,
        modifiers: k.flags | t.key_flags,
        composing: t.composing,
    };
    let repeating = k.repeating.as_ref().map(|(c, _)| *c);
    state.send(FromRender::Key { window, key });
    if down {
        if t.repeats {
            start_repeat(state, name, code);
        } else {
            stop_repeat(state, name);
        }
    } else if repeating == Some(code) {
        stop_repeat(state, name);
    }
}

fn start_repeat(state: &mut State, name: u32, code: u32) {
    stop_repeat(state, name);
    let Some(k) = keyboard_of(state, name) else { return };
    if k.rate <= 0 {
        return;
    }
    let interval = Duration::from_micros(1_000_000 / k.rate as u64);
    let timer = Timer::from_duration(Duration::from_millis(k.delay.max(0) as u64));
    let token = state.loop_handle.insert_source(timer, move |deadline, _, state| {
        if !repeat(state, name, code) {
            return TimeoutAction::Drop;
        }
        // Don't catch up after a stall: that would type a burst.
        let next = deadline + interval;
        let now = Instant::now();
        TimeoutAction::ToInstant(if next < now { now + interval } else { next })
    });
    if let (Ok(token), Some(k)) = (token, keyboard_of(state, name)) {
        k.repeating = Some((code, token));
    }
}

fn stop_repeat(state: &mut State, name: u32) {
    if let Some((_, token)) = keyboard_of(state, name).and_then(|k| k.repeating.take()) {
        state.loop_handle.remove(token);
    }
}

/// One repeat of a held key, translated with the modifiers held now.
/// False when the key should stop repeating.
fn repeat(state: &mut State, name: u32, code: u32) -> bool {
    let Some(k) = keyboard_of(state, name) else { return false };
    let (Some(window), Some(keymap)) = (k.focus, k.keymap.as_ref()) else {
        k.repeating = None;
        return false;
    };
    let t = keymap.translate(&k.mods, code, None);
    let key = Key {
        down: true,
        repeat: true,
        code: code as u16,
        characters: t.characters,
        unmodified: t.unmodified,
        modifiers: k.flags | t.key_flags,
        composing: None,
    };
    state.send(FromRender::Key { window, key });
    true
}

#[cfg(test)]
mod tests {

    #[test]
    fn pinches_multiply_to_their_scale() {
        use super::pinch_step;
        let scales = [1.0, 1.1, 1.3, 1.25, 2.0];
        let product: f64 = scales.windows(2).map(|w| 1.0 + pinch_step(w[0], w[1])).product();
        assert!((product - 2.0).abs() < 1e-12);
        assert_eq!(pinch_step(0.0, 1.0), 0.0);
    }

    #[test]
    fn flicks_read_the_last_moves() {
        use super::flick;
        // 30 points down in 30 ms: a second's worth is 1000.
        let moves = [(1000, 0.0, 10.0), (1010, 0.0, 10.0), (1020, 0.0, 10.0)];
        let (vx, vy) = flick(&moves, 1030);
        assert_eq!(vx, 0.0);
        assert!((vy - 1000.0).abs() < 1e-9);
        // Moves older than the window don't count: fingers that rested
        // before lifting leave no speed.
        assert_eq!(flick(&moves, 1300), (0.0, 0.0));
        // One quick move counts over a frame at least.
        let (_, vy) = flick(&[(500, 0.0, 8.0)], 500);
        assert!((vy - 500.0).abs() < 1e-9);
    }

    use super::*;

    #[test]
    fn clicks_count_up_when_quick_and_close() {
        let mut c = Clicks::default();
        let left = 0x110;
        assert_eq!(c.press(1000, 10.0, 10.0, left, (1, false)), 1);
        assert_eq!(c.press(1200, 12.0, 11.0, left, (1, false)), 2);
        assert_eq!(c.press(1500, 12.0, 11.0, left, (1, false)), 3);
        // Too slow, too far, another button, the decorations: a new series.
        assert_eq!(c.press(2000, 12.0, 11.0, left, (1, false)), 1);
        assert_eq!(c.press(2100, 40.0, 11.0, left, (1, false)), 1);
        assert_eq!(c.press(2200, 40.0, 11.0, 0x111, (1, false)), 1);
        assert_eq!(c.press(2300, 40.0, 11.0, 0x111, (1, true)), 1);
        // The compositor's millisecond clock wraps.
        c.press(u32::MAX - 100, 0.0, 0.0, left, (1, false));
        assert_eq!(c.press(100, 0.0, 0.0, left, (1, false)), 2);
    }

    #[test]
    fn buttons_are_numbered_as_appkit_numbers_them() {
        assert_eq!(button(0x110), Button::Left);
        assert_eq!(button(0x111), Button::Right);
        assert_eq!(button(0x112), Button::Other(2));
        assert_eq!(button(0x113), Button::Other(3));
        assert_eq!(button(0x114), Button::Other(4));
    }
}
