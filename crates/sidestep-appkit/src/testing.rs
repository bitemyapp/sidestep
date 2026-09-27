//! Driving AppKit from tests without a display (not a stable API).
//!
//! [`use_null_backend`] (or `SIDESTEP_BACKEND=null`) replaces the Wayland
//! render thread with one that answers at once and draws nothing (see
//! `backend::null`). Tests then play the render thread's part for input:
//! each `inject_…` function adds a message to the main thread's inbox as if
//! the render thread had sent it, after whatever it did send, and
//! [`settle`] runs the main loop, sending events as the application's main
//! loop does, until everything sent both ways has been handled. Windows are named by the id the render thread knows them by
//! while they are on screen ([`showing_id`]); positions are in points from
//! the top left of the window's content, as the render thread reports
//! them.
//!
//! Everything here belongs to the main thread.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use objc2_app_kit::NSWindow;
use sidestep_foundation::runloop::Mode;

use crate::backend::null;
use crate::event_loop;
use crate::protocol::{Button, FromRender, Key, ScrollPhase, WindowState};

/// What the main thread asked the null render thread for, besides drawing.
#[derive(Clone, Debug, PartialEq)]
pub enum Seen {
    /// A window was shown; a popup or a sheet names its parent.
    Created {
        window: u32,
        width: u32,
        height: u32,
        popup_of: Option<u32>,
        sheet_of: Option<u32>,
    },
    Closed {
        window: u32,
    },
    /// The pointer's shape over a window's content, by its cursor-theme name.
    Cursor {
        window: u32,
        name: String,
    },
    CursorHidden {
        hidden: bool,
        until_moved: bool,
    },
    /// A request (activation, resizing, moving, …), as its debug form.
    Request {
        window: u32,
        request: String,
    },
    /// The window a window belongs over.
    Parent {
        window: u32,
        parent: Option<u32>,
    },
}

/// A run of text the main thread asked the null render thread to paint.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintedText {
    pub window: u32,
    /// Straight sRGB red, green, blue and alpha, 0 to 1.
    pub color: [f32; 4],
    /// Where the run starts on its baseline, in the points of what it was
    /// painted into: the window's content, or the document of a scroll
    /// view (whose content is painted into tiles of its own).
    pub x: f32,
    pub y: f32,
}

/// Start (or stop) writing down the text painted, for
/// [`take_painted_text`].
pub fn note_painted_text(on: bool) {
    *null::TEXT.lock().unwrap_or_else(|e| e.into_inner()) = on.then(Vec::new);
}

/// The text painted since the last call, while noting it.
pub fn take_painted_text() -> Vec<PaintedText> {
    null::TEXT.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(std::mem::take).unwrap_or_default()
}

/// Use the null render thread. Call before the first window is shown.
pub fn use_null_backend() {
    null::request();
}

/// What the main thread asked for since the last call.
pub fn take_render_log() -> Vec<Seen> {
    std::mem::take(&mut *null::LOG.lock().unwrap_or_else(|e| e.into_inner()))
}

/// The id the render thread knows `window` by while it is on screen.
pub fn showing_id(window: &NSWindow) -> u32 {
    crate::window::imp(window).id()
}

/// Run the main loop until the null render thread has answered everything
/// sent to it and the main thread has handled the answers, the input
/// injected and whatever those led to (displays, more answers).
pub fn settle() {
    event_loop::install();
    let give_up = Instant::now() + Duration::from_secs(5);
    let mut quiet = 0;
    // A window that draws on every frame never goes quiet; a few rounds
    // with nothing new are enough.
    for _ in 0..64 {
        let sent = null::SENT.load(Ordering::Acquire);
        while null::HANDLED.load(Ordering::Acquire) < sent {
            assert!(Instant::now() < give_up, "sidestep: the null render thread stopped answering");
            std::thread::yield_now();
        }
        event_loop::take_from_render();
        // As the application's main loop runs: events are sent.
        event_loop::run_once(Mode::DEFAULT);
        if null::SENT.load(Ordering::Acquire) == sent && !event_loop::has_inbox() {
            quiet += 1;
            if quiet == 2 {
                return;
            }
        } else {
            quiet = 0;
        }
    }
}

/// Run the main loop in the default mode for `ms` milliseconds, sending
/// events as the application's main loop does.
pub fn run_for(ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        event_loop::run_until(Mode::DEFAULT, until);
    }
}

/// The pointer came over the window's content.
pub fn inject_enter(window: u32, x: f64, y: f64) {
    event_loop::inject(FromRender::Enter { window, x, y });
}

pub fn inject_motion(window: u32, x: f64, y: f64, modifiers: usize) {
    event_loop::inject(FromRender::Motion { window, x, y, modifiers });
}

pub fn inject_leave(window: u32) {
    event_loop::inject(FromRender::Leave { window });
}

/// A mouse button (AppKit's numbering: 0 left, 1 right, 2 and on the
/// others) pressed or released.
pub fn inject_button(window: u32, x: f64, y: f64, button: u8, pressed: bool, clicks: u32, modifiers: usize) {
    inject_press(window, x, y, button, pressed, clicks, modifiers, false);
}

/// A left button press on a window the press gave the keyboard: the click
/// that activates an inactive window.
pub fn inject_activating_press(window: u32, x: f64, y: f64) {
    inject_press(window, x, y, 0, true, 1, 0, true);
}

#[allow(clippy::too_many_arguments)]
fn inject_press(
    window: u32,
    x: f64,
    y: f64,
    button: u8,
    pressed: bool,
    clicks: u32,
    modifiers: usize,
    activating: bool,
) {
    let button = match button {
        0 => Button::Left,
        1 => Button::Right,
        n => Button::Other(n),
    };
    event_loop::inject(FromRender::Button { window, x, y, button, pressed, clicks, modifiers, activating });
}

/// A key typed on the window: `code` is the XKB keycode (the evdev code
/// plus 8), `characters` what it types with `modifiers` and `unmodified`
/// what it types with Shift at most.
pub fn inject_key(window: u32, code: u16, characters: &str, unmodified: &str, down: bool, modifiers: usize) {
    let key = Key {
        down,
        repeat: false,
        code,
        characters: characters.into(),
        unmodified: unmodified.into(),
        modifiers,
        composing: None,
    };
    event_loop::inject(FromRender::Key { window, key });
}

/// The modifier keys changed.
pub fn inject_modifiers(window: u32, modifiers: usize, code: u16) {
    event_loop::inject(FromRender::Modifiers { window, modifiers, code });
}

/// The window got or lost the keyboard.
pub fn inject_focus(window: u32, focused: bool) {
    event_loop::inject(FromRender::Focus { window, focused });
}

/// A wheel scrolled `dy` detents (positive toward the bottom, as Wayland
/// counts).
pub fn inject_wheel(window: u32, x: f64, y: f64, dx: f64, dy: f64) {
    event_loop::inject(FromRender::Scroll {
        window,
        x,
        y,
        dx,
        dy,
        wheel: true,
        modifiers: 0,
        phase: ScrollPhase::None,
        velocity: (0.0, 0.0),
        inverted: false,
    });
}

/// A touchpad scroll by `dx`, `dy` points (positive toward the bottom
/// right, as Wayland counts), in a gesture's phase (0 none, 1 began, 2
/// changed, 3 ended); an ending gesture's `velocity` (points a second)
/// makes it coast.
pub fn inject_scroll(window: u32, x: f64, y: f64, delta: (f64, f64), phase: u8, velocity: (f64, f64)) {
    let (dx, dy) = delta;
    let phase = match phase {
        1 => ScrollPhase::Began,
        2 => ScrollPhase::Changed,
        3 => ScrollPhase::Ended,
        _ => ScrollPhase::None,
    };
    event_loop::inject(FromRender::Scroll {
        window,
        x,
        y,
        dx,
        dy,
        wheel: false,
        modifiers: 0,
        phase,
        velocity,
        inverted: false,
    });
}

/// A scroll layer as the main thread last placed it (see `layers`):
/// rectangles are `[x0, y0, x1, y1]` in window points (the extent in the
/// layer's, the overlay in those of the layer it is nested in), views are
/// named by their addresses.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerInfo {
    /// The clip view whose layer it is.
    pub clip: usize,
    /// The clip view of the layer it is nested in, or 0.
    pub parent: usize,
    /// Stacking: the tiles', the overlay's.
    pub z: [u32; 2],
    pub viewport: [f32; 4],
    pub origin: [f32; 2],
    pub extent: [f32; 4],
    /// Tiles' size in device pixels.
    pub tile_size: [u32; 2],
    pub opaque: bool,
    pub overlay: Option<[f32; 4]>,
    pub overlay_views: Vec<usize>,
    /// The tiles drawn and kept, by column and row.
    pub tiles: Vec<[i32; 2]>,
    /// Paints of the tiles and of the overlay so far, and the area painted
    /// into tiles (points squared, margins included).
    pub tile_paints: u32,
    pub overlay_paints: u32,
    pub tile_area: f64,
}

/// The window's scroll layers, bottom first.
pub fn scroll_layers(window: &NSWindow) -> Vec<LayerInfo> {
    crate::layers::describe(crate::window::imp(window))
}

/// Lower the memory a window's scroll layers' tiles may hold, or with
/// `None` restore it (96 MB).
pub fn set_tile_memory_cap(bytes: Option<usize>) {
    crate::layers::set_memory_cap(bytes);
}

/// Change how long a display pass may spend before it puts the tiles
/// ahead of its scroll layers off to the next, or with `None` restore it
/// (4 ms).
pub fn set_prefetch_budget(budget: Option<Duration>) {
    crate::layers::set_prefetch_budget(budget);
}

/// The compositor asked to close the window (its close button).
pub fn inject_close_request(window: u32) {
    event_loop::inject(FromRender::CloseRequested { window });
}

/// The compositor configured the window: its content size in points, its
/// scale, and whether it is being resized interactively or is suspended.
pub fn inject_configure(window: u32, width: u32, height: u32, scale: f64, resizing: bool, suspended: bool) {
    let state = WindowState { suspended, resizing, ..WindowState::default() };
    event_loop::inject(FromRender::Configure { window, width, height, scale, titlebar: 0, state });
}
