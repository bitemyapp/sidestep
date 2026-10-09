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
pub use crate::backend::null::{PaintedText, Seen};
use crate::event_loop;
use crate::protocol::{Button, FromRender, Key, ScrollPhase, WindowState};

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

/// The damage `window`'s own surface (not its scroll layers) will redraw
/// at the next pass, in points from its content's top left: x0, y0, x1,
/// y1.
pub fn root_damage(window: &NSWindow) -> Vec<[f32; 4]> {
    let w = crate::window::imp(window);
    let damage = w.take_damage();
    let rects = damage
        .get(&crate::protocol::ROOT_LAYER)
        .map_or_else(Vec::new, |rs| rs.iter().map(|r| [r.x0, r.y0, r.x1, r.y1]).collect());
    for (layer, rs) in damage {
        for r in rs {
            w.invalidate(layer, r);
        }
    }
    rects
}

/// Run `window`'s display pass now, as the main loop would once its last
/// frame is shown (nothing, if it isn't shown or has nothing to draw).
pub fn display_now(window: &NSWindow) {
    crate::window::display_if_needed(crate::window::imp(window));
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

/// [`settle`], and before that keep running the main loop until first
/// frames stop waiting for the desktop's light or dark, which the null
/// render thread never tells (they give up after a moment): after this, a
/// window shown before it has drawn its first frame, however slow the
/// machine.
pub fn settle_first_frames() {
    settle();
    let give_up = Instant::now() + Duration::from_secs(10);
    while !crate::settings::ready() {
        assert!(Instant::now() < give_up, "sidestep: first frames kept waiting for the desktop's appearance");
        let until = crate::settings::deadline().unwrap_or_else(Instant::now).min(give_up);
        event_loop::run_until(Mode::DEFAULT, until);
    }
    settle();
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

/// The accessibility notifications posted since the last call (with
/// `NSAccessibilityPostNotificationWithUserInfo`), oldest first: the
/// element while it lives, the name, and the user info.
#[allow(clippy::type_complexity)]
pub fn take_accessibility_notifications() -> Vec<(
    Option<objc2::rc::Retained<objc2::runtime::AnyObject>>,
    String,
    Option<objc2::rc::Retained<objc2_foundation::NSDictionary<objc2_foundation::NSString, objc2::runtime::AnyObject>>>,
)> {
    crate::accessibility::take_posted()
        .into_iter()
        .map(|p| (p.element.load(), p.name.to_string(), p.user_info))
        .collect()
}

// Core Animation (see `quartzcore`).

/// A layer as the render thread shows it at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct PresentedLayer {
    pub opacity: f64,
    pub position: [f64; 2],
    pub bounds: [f64; 4],
    pub transform: [f64; 16],
    pub corner_radius: f64,
    pub background: Option<[f64; 4]>,
    pub hidden: bool,
}

/// `layer` as the null render thread would show it at the media time `t`
/// (none if it never got there).
pub fn presented_layer(layer: &objc2_quartz_core::CALayer, t: f64) -> Option<PresentedLayer> {
    let id = crate::quartzcore::layer::imp(layer).id();
    let ca = null::CA.lock().unwrap_or_else(|e| e.into_inner());
    let p = ca.as_ref()?.presented(id, t)?;
    Some(PresentedLayer {
        opacity: p.opacity,
        position: p.position,
        bounds: p.bounds,
        transform: p.transform,
        corner_radius: p.corner_radius,
        background: p.background_color,
        hidden: p.hidden,
    })
}

/// How many layers the null render thread holds.
pub fn render_layer_count() -> usize {
    null::CA.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |ca| ca.layer_count())
}

/// Have the null render thread show windows at `scale` (whole: 1, 2, 3),
/// so the main thread draws at it and captured pixels have it. Call before
/// the first window is shown.
pub fn use_null_backend_scale(scale: u32) {
    null::SCALE.store(scale.max(1), std::sync::atomic::Ordering::Relaxed);
}

/// Keep the pixels of windows shown from now on (at the null render
/// thread's scale, 1 unless asked) as it would draw them, composites
/// included, or stop.
pub fn capture_pixels(on: bool) {
    *null::PIXELS.lock().unwrap_or_else(|e| e.into_inner()) = on.then(Default::default);
}

/// A shown window's captured pixels: width, height and premultiplied RGBA
/// bytes, as canvases hold them, row by row from the top left.
pub fn window_pixels(window: &NSWindow) -> Option<(u32, u32, Vec<[u8; 4]>)> {
    let id = showing_id(window);
    let pixels = null::PIXELS.lock().unwrap_or_else(|e| e.into_inner());
    let (w, h, px) = pixels.as_ref()?.get(&id)?;
    Some((*w, *h, px.iter().map(|p| p.to_ne_bytes()).collect()))
}

/// Ask the null render thread to composite at `t` and copy a crop, without
/// waiting or locking the pixel buffer on the calling thread. `crop` is
/// [x, y, width, height] in device pixels from the content's top left.
/// The reply holds width, height and premultiplied RGBA pixels, or `None`
/// if capture is disabled, the window/crop is invalid, or this isn't the
/// null backend. Move the receiver to a worker for encoding or file I/O;
/// never wait for its reply on the GUI thread. Enable `capture_pixels`
/// before showing the window, and bound the number of requests in flight.
pub fn request_window_pixels(
    window: &NSWindow,
    t: f64,
    crop: [u32; 4],
) -> std::sync::mpsc::Receiver<Option<crate::protocol::Capture>> {
    let (reply, receiver) = std::sync::mpsc::channel();
    crate::app::send(crate::protocol::ToRender::CaptureWindow { window: showing_id(window), at: t, crop, reply });
    receiver
}

/// Have the null render thread draw `window`'s layer trees as they show at
/// the media time `t` (as a frame would), and wait for it.
pub fn composite_at(window: &NSWindow, t: f64) {
    crate::app::send(crate::protocol::ToRender::Composite { window: showing_id(window), at: Some(t) });
    settle();
}

/// Ask the null render thread to draw `window`'s layer trees as they show
/// at `t`, without waiting: the pixels have it once it gets to it. For
/// code that can't run the loop to wait, as a timer inside a tracking
/// loop can't.
pub fn request_composite_at(window: &NSWindow, t: f64) {
    crate::app::send(crate::protocol::ToRender::Composite { window: showing_id(window), at: Some(t) });
}

/// `CACurrentMediaTime()`.
pub fn media_time() -> f64 {
    crate::quartzcore::math::media_now()
}

/// Play the render thread's part: a frame of `window` showed at the media
/// time `time` (what display links hear).
pub fn inject_tick(window: u32, time: f64) {
    event_loop::inject(FromRender::Tick { window, time });
}

/// Commit Core Animation's changes now (`+[CATransaction flush]` from
/// outside any transaction).
pub fn commit_layers() {
    crate::quartzcore::transaction::commit_now();
}

/// Whether Core Animation's main-thread timer waits to end (or begin) an
/// animation.
pub fn layer_endings_scheduled() -> bool {
    crate::quartzcore::transaction::ending_scheduled()
}

/// Whether the null render thread would draw a frame of `window`'s layer
/// trees now (something in them changed, or animates where it shows).
pub fn layers_want_frame(window: &NSWindow) -> bool {
    let id = showing_id(window);
    null::CA.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|ca| ca.wants_frame(id))
}
