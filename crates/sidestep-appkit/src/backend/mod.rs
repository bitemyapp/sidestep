//! The render thread. It owns the Wayland connection and each window's
//! surfaces:
//!
//! - the root layer is the window's own surface, rasterized into a canvas and
//!   presented through a few shared-memory buffers, copying and damaging only
//!   what changed;
//! - a scroll layer is a grid of tiles, each on its own subsurface, cropped
//!   to the scroll view with the viewporter, with an overlay above for the
//!   views drawn over it ([`tiles`]). Scrolling moves tiles; a tile uploads
//!   only the pixels that changed;
//! - when the compositor leaves decorations to the client, a title bar and a
//!   resize border drawn here ([`decor`]), on subsurfaces around the root
//!   surface so the content keeps its coordinates.
//!
//! Drawing ops arrive in points. Each window renders at its output's scale:
//! the fractional scale (wp_fractional_scale_v1) when the compositor offers
//! it, else the integer one. Every surface's buffer is `points × scale`
//! pixels, shown at its size in points through a viewport, so the compositor
//! doesn't resample.
//!
//! Input comes through [`seat`]: pointers, keyboards translated with their
//! keymaps ([`keyboard`]) and cursors. [`selection`] carries the clipboard
//! and [`textinput`] input methods. All of it reaches the main thread as
//! [`FromRender`] messages.
//!
//! Frames are paced by the compositor's frame callbacks, which are passed on
//! to the main thread as permission to send the next frame. Each present asks
//! for a callback on the window's surface and on one tile or overlay, and
//! the first to fire counts: compositors send none to a surface they
//! consider hidden, and tiles can cover a window's own surface completely.

mod decor;
mod dnd;
mod keyboard;
mod menubar;
pub(crate) mod null;
mod outputs;
mod seat;
mod selection;
mod sheet;
mod textinput;
mod tiles;

pub(crate) use decor::{HEADER, TITLE_SIZE, explicit_theme};

/// Milliseconds a run of `f` takes, the median of seven, for the timing
/// tests (`cargo test --release -p sidestep-appkit timing -- --ignored
/// --nocapture`).
#[cfg(test)]
pub(crate) fn median(mut f: impl FnMut()) -> f64 {
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = std::time::Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;

use smithay_client_toolkit::activation::{ActivationHandler, ActivationState, RequestDataExt};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::channel::{self, Channel, Event as ChannelEvent};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_shm};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::{self, WpViewport};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState, SimpleGlobal};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::popup::{Popup, PopupConfigure, PopupHandler};
use smithay_client_toolkit::shell::xdg::window::{
    DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::{XdgPositioner, XdgShell, XdgSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::{
    delegate_activation, delegate_compositor, delegate_output, delegate_registry, delegate_shm, delegate_simple,
    delegate_subcompositor, delegate_xdg_popup, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};

use crate::event_loop::MainSender;
use crate::protocol::{
    Cursor, FromRender, Op, PopupPlacement, Rect, SizeLimits, Style, Target, TitleText, ToRender, WindowId,
    WindowRequest, WindowState,
};
use crate::raster::{self, Canvas, Glyphs};
use sidestep_foundation::runloop::SourceSignal;

/// The main thread's end of the render thread.
pub(crate) struct Backend {
    pub tx: channel::Sender<ToRender>,
    pub rx: mpsc::Receiver<FromRender>,
}

/// Start the render thread; `wake` is the main loop's event source, which
/// each message to the main thread signals.
pub(crate) fn start(wake: SourceSignal) -> Backend {
    if null::chosen() {
        return null::start(wake);
    }
    let (tx, channel) = channel::channel();
    let (to_main, rx) = mpsc::channel();
    let to_main = MainSender::new(to_main, wake);
    // Decorations follow the desktop's light or dark preference, unless
    // the environment picked one; the settings thread tells both threads.
    crate::settings::connect(tx.clone());
    std::thread::Builder::new()
        .name("sidestep-render".into())
        .spawn(move || run(channel, to_main))
        .expect("sidestep: couldn't start the render thread");
    Backend { tx, rx }
}

/// The compositor's key repeat settings, for `+[NSEvent keyRepeatDelay]`
/// and `keyRepeatInterval`: milliseconds before the first repeat, and
/// repeats per second (0 when keys don't repeat).
static REPEAT_DELAY_MS: AtomicU32 = AtomicU32::new(600);
static REPEAT_RATE: AtomicU32 = AtomicU32::new(25);

/// Seconds before a held key repeats, and between repeats (0 if keys don't
/// repeat).
pub(crate) fn key_repeat() -> (f64, f64) {
    let delay = REPEAT_DELAY_MS.load(Ordering::Relaxed) as f64 / 1000.0;
    let rate = REPEAT_RATE.load(Ordering::Relaxed);
    (delay, if rate == 0 { 0.0 } else { 1.0 / rate as f64 })
}

/// How long after a click the next one still counts toward a double
/// click, and how far away (points) it may land: GTK's defaults, which
/// GNOME keeps.
pub(crate) const DOUBLE_CLICK_MS: u32 = 400;
pub(crate) const DOUBLE_CLICK_DISTANCE: f64 = 5.0;

/// `SIDESTEP_DECORATIONS=client` draws decorations even where the
/// compositor would (as sway does), to see them anywhere.
pub(crate) fn force_client_decorations() -> bool {
    static FORCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCE.get_or_init(|| std::env::var("SIDESTEP_DECORATIONS").is_ok_and(|v| v == "client"))
}

fn run(channel: Channel<ToRender>, to_main: MainSender) {
    keyboard::load_compose_table_early();
    let conn =
        Connection::connect_to_env().expect("sidestep: can't reach a Wayland compositor (is WAYLAND_DISPLAY set?)");
    let (globals, queue) = registry_queue_init(&conn).expect("sidestep: Wayland registry");
    let qh = queue.handle();
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("sidestep: event loop");
    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle()).expect("sidestep: Wayland event source");
    event_loop
        .handle()
        .insert_source(channel, |event, _, state: &mut State| match event {
            ChannelEvent::Msg(msg) if crate::layers::tracing() => {
                let kind = match &msg {
                    ToRender::Paint { target: Target::Tiles(_), .. } => "a tiles paint",
                    ToRender::Paint { target: Target::Overlay(_), .. } => "an overlay paint",
                    ToRender::Paint { target: Target::Root, .. } => "a window paint",
                    ToRender::Paint { target: Target::Content(_), .. } => "a layer canvas paint",
                    ToRender::Present { .. } => "a present",
                    ToRender::Commit(_) => "a layer commit",
                    _ => "another message",
                };
                let started = std::time::Instant::now();
                state.handle(msg);
                tiles::note_busy(started.elapsed(), kind);
            }
            ChannelEvent::Msg(msg) => state.handle(msg),
            ChannelEvent::Closed => state.exit = true,
        })
        .expect("sidestep: channel event source");

    let compositor = CompositorState::bind(&globals, &qh).expect("sidestep: wl_compositor is required");
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .expect("sidestep: wl_subcompositor is required");
    let viewporter = SimpleGlobal::<WpViewporter, 1>::bind(&globals, &qh).expect("sidestep: wp_viewporter is required");
    let xdg = XdgShell::bind(&globals, &qh).expect("sidestep: xdg_wm_base is required");
    let shm = Shm::bind(&globals, &qh).expect("sidestep: wl_shm is required");
    let pool = SlotPool::new(4 << 20, &shm).expect("sidestep: shared memory pool");
    // SIDESTEP_NO_FRACTIONAL_SCALE=1 renders at the integer scale even
    // where fractional scales are offered.
    let fractional = if std::env::var_os("SIDESTEP_NO_FRACTIONAL_SCALE").is_some() {
        None
    } else {
        globals.bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ()).ok()
    };
    let mut state = State {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: seat::Seats::new(&globals, &qh),
        selection: selection::Selection::new(&globals, &qh),
        text_inputs: textinput::TextInputs::new(&globals, &qh),
        activation: ActivationState::bind(&globals, &qh).ok(),
        compositor,
        subcompositor,
        viewporter,
        fractional,
        xdg,
        shm,
        pool,
        conn,
        qh,
        loop_handle: event_loop.handle(),
        windows: HashMap::new(),
        roles: HashMap::new(),
        glyphs: Glyphs::default(),
        rgba: None,
        to_main,
        // The environment isn't changed: other threads may be reading it.
        startup_token: std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| !t.is_empty()),
        empty_region: None,
        ca: crate::quartzcore::tree::Compositor::default(),
        exit: false,
    };
    seat::bind_existing(&mut state, &globals);
    outputs::started(&state, &globals);
    while !state.exit {
        // While tiles have paints to rasterize, look for events without
        // waiting, and rasterize a slice after handling them: frame
        // callbacks and the main thread's presents come first.
        let owed = state.windows.values().any(|w| tiles::owed(&w.layers));
        if event_loop.dispatch(owed.then_some(std::time::Duration::ZERO), &mut state).is_err() {
            break;
        }
        // The Wayland source reports only I/O errors. After a protocol error
        // the connection is dead but its socket stays readable, which would
        // spin this loop: stop, and the main thread hears the channel close.
        if let Some(error) = state.conn.protocol_error() {
            eprintln!("sidestep: Wayland protocol error: {error}");
            break;
        }
        state.rasterize_tiles();
    }
}

/// How long the render thread rasterizes tiles between looks for events:
/// well inside a frame at 120 Hz, so a frame callback or a present waits
/// on it little.
pub(crate) const RASTER_SLICE: std::time::Duration = std::time::Duration::from_micros(2_000);

pub(crate) struct State {
    registry: RegistryState,
    outputs: OutputState,
    seats: seat::Seats,
    selection: selection::Selection,
    text_inputs: textinput::TextInputs,
    activation: Option<ActivationState>,
    compositor: CompositorState,
    subcompositor: SubcompositorState,
    viewporter: SimpleGlobal<WpViewporter, 1>,
    fractional: Option<WpFractionalScaleManagerV1>,
    xdg: XdgShell,
    shm: Shm,
    pool: SlotPool,
    conn: Connection,
    qh: QueueHandle<State>,
    loop_handle: LoopHandle<'static, State>,
    windows: HashMap<WindowId, Win>,
    /// What each of our surfaces is, for input.
    roles: HashMap<ObjectId, Role>,
    glyphs: Glyphs,
    /// Buffers take canvas pixels as they are (XBGR8888), decided at the
    /// first buffer ([`State::rgba`]).
    rgba: Option<bool>,
    to_main: MainSender,
    /// The activation token we were started with, for the first window.
    startup_token: Option<String>,
    /// An input region with nothing in it, made the first time a window
    /// lets pointer input through.
    empty_region: Option<Region>,
    /// Core Animation's layer trees, animated here (see `quartzcore::tree`).
    ca: crate::quartzcore::tree::Compositor,
    exit: bool,
}

/// What a surface is to input. Scroll layers' surfaces take no input
/// (see `tiles`): what's over them goes to the window's own surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Role {
    /// A window's own surface.
    Root(WindowId),
    /// Part of the decorations.
    Decor(WindowId, decor::Part),
    /// The main menu's bar above the content (see `menubar`).
    Bar(WindowId),
}

impl Role {
    fn window(self) -> WindowId {
        match self {
            Role::Root(w) | Role::Decor(w, _) | Role::Bar(w) => w,
        }
    }
}

enum Shell {
    Toplevel(Window),
    Popup(Popup),
    /// Part of another window (see `sheet`).
    Sheet(sheet::Sheet),
}

pub(crate) struct Win {
    shell: Shell,
    viewport: WpViewport,
    fractional: Option<WpFractionalScaleV1>,
    /// Content size in points.
    width: u32,
    height: u32,
    scale: f64,
    /// Canvas size in pixels.
    px_width: u32,
    px_height: u32,
    configured: bool,
    canvas: Vec<u32>,
    /// Damage in device pixels.
    damage: Vec<Rect>,
    buffers: Vec<RootBuffer>,
    layers: tiles::Layers,
    /// Counts presents; a frame callback names the present it belongs to.
    frame_seq: u64,
    frame_signalled: bool,
    /// The main thread presented and waits to hear the frame showed.
    main_waiting: bool,
    /// Display links want to hear of every frame (`FromRender::Tick`).
    ticks: bool,
    /// Layer trees changed or animate while a frame was on its way: draw
    /// them when it shows.
    ca_pending: bool,
    /// The main thread owes the window a frame (it resized, or a display
    /// pass is sending its paints): layer trees wait for its `Present`
    /// rather than showing half of what it draws.
    held: bool,
    style: Style,
    limits: SizeLimits,
    state: WindowState,
    title: TitleText,
    /// The compositor asked us to draw decorations.
    client_side: bool,
    decor: Option<decor::Decor>,
    /// The main menu's bar, when the window shows one.
    menubar: Option<menubar::Bar>,
    cursor: Cursor,
    /// The window geometry last set, to set it again only on changes.
    geometry: (i32, i32, i32, i32),
    /// What the main thread was last told, to tell it only changes.
    reported: Option<(u32, u32, u64, u32, WindowState)>,
    /// The size limits last given the compositor, title bar included.
    sent_limits: Option<SizeLimits>,
    /// The surfaces have empty input regions.
    passthrough: bool,
    /// The compositor said which scale it prefers.
    scale_given: bool,
    /// The first responder takes text from input methods.
    text_input: bool,
    /// Where its caret is, in points from the top left of the content.
    caret: Option<Rect>,
}

impl Win {
    fn surface(&self) -> &WlSurface {
        match &self.shell {
            Shell::Toplevel(w) => w.wl_surface(),
            Shell::Popup(p) => p.wl_surface(),
            Shell::Sheet(s) => s.surface(),
        }
    }

    fn toplevel(&self) -> Option<&Window> {
        match &self.shell {
            Shell::Toplevel(w) => Some(w),
            Shell::Popup(_) | Shell::Sheet(_) => None,
        }
    }

    /// Height of the title bar drawn above the content, in points: the
    /// header, and the menu bar under it.
    fn titlebar(&self) -> u32 {
        self.decor.as_ref().map_or(0, |d| d.titlebar(&self.state)) + self.bar_height()
    }

    /// Height of the menu bar, in points: none in full screen.
    pub(super) fn bar_height(&self) -> u32 {
        if self.state.fullscreen { 0 } else { self.menubar.as_ref().map_or(0, |b| b.height) }
    }
}

/// Pixels for `points` at `scale`, as wp_fractional_scale_v1 asks buffers
/// to be sized: rounded half up.
pub(crate) fn px(points: u32, scale: f64) -> u32 {
    (points as f64 * scale + 0.5).floor() as u32
}

/// What a frame callback belongs to.
struct FrameTag {
    window: WindowId,
    seq: u64,
}

struct RootBuffer {
    buffer: Buffer,
    /// Areas (device pixels) that changed since this buffer was last
    /// written.
    stale: Vec<Rect>,
}

/// What canvases start as, before the first paint: opaque light gray.
const BACKGROUND: u32 = u32::from_ne_bytes([0xec, 0xec, 0xec, 0xff]);

/// The buffer format for canvas pixels.
fn format(rgba: bool) -> wl_shm::Format {
    if rgba { wl_shm::Format::Xbgr8888 } else { wl_shm::Format::Xrgb8888 }
}

/// Copy canvas pixels into a buffer, swizzling them in the same pass
/// unless the buffer takes them as they are.
fn copy_pixels(dst: &mut [u32], src: &[u32], rgba: bool) {
    if rgba {
        dst.copy_from_slice(src);
    } else {
        dst.iter_mut().zip(src).for_each(|(d, s)| *d = raster::to_xrgb(*s));
    }
}

/// Copy a rectangle of canvas pixels into a buffer of the same size (the
/// window's or a tile's). Returns the bytes copied.
fn copy_rows(dst: &mut [u32], src: &[u32], width: u32, height: u32, r: &Rect, rgba: bool) -> usize {
    let r = r.round_out();
    let x0 = r.x0.max(0.0) as usize;
    let y0 = r.y0.max(0.0) as usize;
    let x1 = (r.x1.min(width as f32).max(0.0) as usize).max(x0);
    let y1 = (r.y1.min(height as f32).max(0.0) as usize).max(y0);
    let w = width as usize;
    for y in y0..y1 {
        if x0 < x1 {
            copy_pixels(&mut dst[y * w + x0..y * w + x1], &src[y * w + x0..y * w + x1], rgba);
        }
    }
    (x1 - x0) * (y1 - y0) * 4
}

/// A shared-memory buffer's bytes as pixels. Pool slots are page-aligned.
pub(crate) fn as_pixels(bytes: &mut [u8]) -> &mut [u32] {
    // SAFETY: the pool maps whole pages, so the bytes are 4-byte aligned, and
    // any bit pattern is a valid u32.
    unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<u32>(), bytes.len() / 4) }
}

/// `r` (points) in pixels at `scale`, grown to whole pixels.
fn to_px(r: &Rect, scale: f64) -> Rect {
    let s = scale as f32;
    Rect::new(r.x0 * s, r.y0 * s, r.x1 * s, r.y1 * s).round_out()
}

/// What a window's decorations are drawn around.
fn frame_info(win: &Win) -> decor::FrameInfo<'_> {
    decor::FrameInfo {
        width: win.width,
        height: win.height,
        scale: win.scale,
        // Suspension changes nothing drawn.
        state: WindowState { suspended: false, resizing: false, ..win.state },
        style: win.style,
        title: &win.title,
        bar: win.bar_height(),
    }
}

impl State {
    fn send(&self, msg: FromRender) {
        let _ = self.to_main.send(msg);
    }

    fn handle(&mut self, msg: ToRender) {
        match msg {
            ToRender::CreateWindow { window, width, height, title, style, limits, popup, sheet_of } => {
                self.create_window(window, width, height, title, style, limits, popup, sheet_of)
            }
            ToRender::SetTitle { window, title, text } => {
                let Some(win) = self.windows.get_mut(&window) else { return };
                if let Some(w) = win.toplevel() {
                    w.set_title(title);
                }
                win.title = text;
                if let Some(d) = &mut win.decor {
                    d.title_changed();
                }
                // Sent by a display pass, whose present draws the header.
            }
            ToRender::SetStyle { window, style } => {
                let Some(win) = self.windows.get_mut(&window) else { return };
                win.style = style;
                self.apply_limits(window);
                self.update_decorations(window);
                self.apply_input(window);
                self.refresh_decor(window);
            }
            ToRender::SetSizeLimits { window, limits } => {
                if let Some(win) = self.windows.get_mut(&window) {
                    win.limits = limits;
                    self.apply_limits(window);
                }
            }
            ToRender::Request { window, request } => self.request(window, request),
            ToRender::SetCursor { window, cursor } => {
                if let Some(win) = self.windows.get_mut(&window) {
                    win.cursor = cursor;
                }
                seat::cursor_changed(self, window);
            }
            ToRender::HideCursor { hidden, until_moved } => seat::hide_cursor(self, hidden, until_moved),
            ToRender::Paint { window, target, rects, ops } => match target {
                Target::Content(layer) => {
                    // Canvases are painted in a display pass, whose Present
                    // shows them.
                    self.ca.paint_content(layer, &rects, &ops, &mut self.glyphs);
                    self.hold(window);
                }
                target => {
                    let ops = self.ca.paint(window, target, &rects, ops);
                    self.paint_target(window, target, rects, ops);
                }
            },
            ToRender::PlaceLayer { window, layer, place } => tiles::place(self, window, layer, place),
            ToRender::DropLayer { window, layer } => {
                self.ca.drop_target(window, Target::Tiles(layer));
                self.ca.drop_target(window, Target::Overlay(layer));
                tiles::drop_layer(self, window, layer)
            }
            ToRender::DropTiles { window, layer, tiles } => {
                // The kept paints no longer draw into the dropped tiles.
                let rects = self
                    .windows
                    .get(&window)
                    .map(|w| tiles::tile_rects(&w.layers, layer, &tiles, w.scale))
                    .unwrap_or_default();
                self.ca.drop_rects(window, Target::Tiles(layer), &rects);
                tiles::drop_tiles(self, window, layer, &tiles)
            }
            ToRender::Present { window } => {
                if let Some(win) = self.windows.get_mut(&window) {
                    win.main_waiting = true;
                    win.held = false;
                }
                self.ca_frame(window);
                self.present(window)
            }
            ToRender::CloseWindow { window } => {
                self.ca.drop_window(window);
                self.close_window(window)
            }
            ToRender::Commit(commit) => {
                for w in &commit.hold {
                    self.hold(*w);
                }
                self.ca.apply(*commit);
                self.animate_all();
            }
            ToRender::FrameTicks { window, on } => {
                let Some(win) = self.windows.get_mut(&window) else { return };
                win.ticks = on;
                if on && win.configured && win.frame_signalled {
                    self.request_frame(window);
                }
            }
            ToRender::Composite { window, at } => {
                let clock = std::mem::replace(&mut self.ca.clock, at);
                self.ca_frame(window);
                self.ca.clock = clock;
                self.present(window);
            }
            ToRender::SetSelection { contents } => selection::set(self, contents),
            ToRender::ReadSelection { mime, token, source, drag } => selection::read(self, mime, token, source, drag),
            ToRender::SelectionData { token, data } => selection::provided(self, token, data),
            ToRender::DndStatus { drag, mime, actions, preferred, periodic } => {
                dnd::status(self, drag, mime, actions, preferred, periodic);
            }
            ToRender::DndFinish { drag, performed } => dnd::finish(self, drag, performed),
            ToRender::PublishOutputs => outputs::requested(self),
            ToRender::TextInput { window, wanted, caret } => textinput::set_wanted(self, window, wanted, caret),
            ToRender::ResetTextInput { window } => textinput::reset(self, window),
            ToRender::SetParent { window, parent } => {
                let parent = parent.and_then(|p| self.windows.get(&p)).and_then(|w| w.toplevel().cloned());
                if let Some(w) = self.windows.get(&window).and_then(|w| w.toplevel()) {
                    w.set_parent(parent.as_ref());
                }
            }
            ToRender::ForgetImages { keys } => raster::images::forget(&keys),
            ToRender::MenuBar { window, height, ops } => menubar::set(self, window, height, ops),
            ToRender::ColorScheme { dark } => {
                decor::set_dark(dark);
                let windows: Vec<WindowId> = self.windows.keys().copied().collect();
                for window in windows {
                    if let Some(d) = self.windows.get_mut(&window).and_then(|w| w.decor.as_mut()) {
                        d.restyle();
                    }
                    self.refresh_decor(window);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn create_window(
        &mut self,
        window: WindowId,
        width: u32,
        height: u32,
        title: String,
        style: Style,
        limits: SizeLimits,
        popup: Option<PopupPlacement>,
        sheet_of: Option<WindowId>,
    ) {
        let mut scale = self.initial_scale();
        let sheet = match sheet_of {
            Some(parent) => {
                let Some(sheet) = sheet::create(self, parent) else { return };
                scale = self.windows.get(&parent).map_or(scale, |p| p.scale);
                Some(sheet)
            }
            None => None,
        };
        let surface = match &sheet {
            Some(sheet) => sheet.surface().clone(),
            None => self.compositor.create_surface(&self.qh),
        };
        let shell = match (popup, sheet) {
            (_, Some(sheet)) => Shell::Sheet(sheet),
            (Some(placement), None) => {
                let Some(popup) = self.create_popup(&surface, width, height, placement) else {
                    surface.destroy();
                    return;
                };
                Shell::Popup(popup)
            }
            (None, None) => {
                let decorations = if !style.titled || force_client_decorations() {
                    WindowDecorations::ClientOnly
                } else {
                    WindowDecorations::RequestServer
                };
                let w = self.xdg.create_window(surface.clone(), decorations, &self.qh);
                w.set_title(title);
                w.set_app_id(app_id());
                if let Some(token) = self.startup_token.take()
                    && let Some(activation) = &self.activation
                {
                    activation.activate::<State>(&surface, token);
                }
                w.commit();
                Shell::Toplevel(w)
            }
        };
        let viewport = self.viewporter.get().expect("viewporter").get_viewport(&surface, &self.qh, ());
        let fractional = self.fractional.as_ref().map(|f| f.get_fractional_scale(&surface, &self.qh, window));
        self.roles.insert(surface.id(), Role::Root(window));
        self.windows.insert(
            window,
            Win {
                shell,
                viewport,
                fractional,
                width,
                height,
                scale,
                px_width: 0,
                px_height: 0,
                configured: false,
                canvas: Vec::new(),
                damage: Vec::new(),
                buffers: Vec::new(),
                layers: tiles::Layers::default(),
                frame_seq: 0,
                frame_signalled: false,
                main_waiting: false,
                ticks: false,
                ca_pending: false,
                held: false,
                style,
                limits,
                state: WindowState::default(),
                title: TitleText::default(),
                client_side: false,
                decor: None,
                menubar: None,
                cursor: Cursor::Default,
                geometry: (0, 0, 0, 0),
                reported: None,
                sent_limits: None,
                passthrough: false,
                scale_given: false,
                text_input: false,
                caret: None,
            },
        );
        self.apply_limits(window);
        self.apply_input(window);
        if sheet_of.is_some() {
            // No compositor configures a subsurface: it is ready now.
            self.resize(window, width, height);
            sheet::place(self, window);
            sheet::commit_parent(self, window);
        }
    }

    /// An xdg_popup below `placement.anchor` in its parent, grabbing input
    /// if asked (as menus do).
    fn create_popup(&self, surface: &WlSurface, width: u32, height: u32, placement: PopupPlacement) -> Option<Popup> {
        let parent = self.windows.get(&placement.parent)?;
        let positioner = XdgPositioner::new(&self.xdg).ok()?;
        let a = placement.anchor;
        // Positioners count from the parent's window geometry, which starts
        // at the title bar when we draw one.
        let top = parent.titlebar() as f32;
        positioner.set_anchor_rect(
            a.x0.floor() as i32,
            (a.y0 + top).floor() as i32,
            ((a.x1 - a.x0).ceil() as i32).max(1),
            ((a.y1 - a.y0).ceil() as i32).max(1),
        );
        positioner.set_size(width.max(1) as i32, height.max(1) as i32);
        match placement.layout {
            Some(layout) => menu_layout(&positioner, layout),
            None => {
                positioner.set_anchor(if placement.below { Anchor::BottomLeft } else { Anchor::TopLeft });
                positioner.set_gravity(Gravity::BottomRight);
                positioner.set_constraint_adjustment(
                    ConstraintAdjustment::FlipY | ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY,
                );
            }
        }
        let parent_xdg = match &parent.shell {
            Shell::Toplevel(w) => w.xdg_surface().clone(),
            Shell::Popup(p) => p.xdg_surface().clone(),
            // Popups of sheets aren't placed yet.
            Shell::Sheet(_) => return None,
        };
        let popup = Popup::from_surface(Some(&parent_xdg), &positioner, &self.qh, surface.clone(), &self.xdg).ok()?;
        if placement.grab
            && let Some((seat, serial)) = self.seats.latest_serial()
        {
            popup.xdg_popup().grab(&seat, serial);
        }
        popup.wl_surface().commit();
        Some(popup)
    }

    fn close_window(&mut self, window: WindowId) {
        let Some(win) = self.windows.remove(&window) else { return };
        outputs::window_closed(window);
        self.roles.retain(|_, role| role.window() != window);
        seat::window_closed(self, window);
        if let Some(f) = &win.fractional {
            f.destroy();
        }
        win.viewport.destroy();
        // Tiles and decorations go first: they're subsurfaces of the root.
        drop(win.layers);
        drop(win.decor);
        drop(win.menubar);
        drop(win.shell);
    }

    /// Size limits in window geometry terms, title bar included.
    fn apply_limits(&mut self, window: WindowId) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let bar = win.titlebar();
        let SizeLimits { mut min, mut max } = win.limits;
        if !win.style.resizable {
            // A fixed size: the current one.
            min = (win.width, win.height);
            max = min;
        }
        let with_bar = |(w, h): (u32, u32)| (w, if h == 0 { 0 } else { h + bar });
        let limits = SizeLimits { min: with_bar(min), max: with_bar(max) };
        if win.sent_limits == Some(limits) {
            return;
        }
        win.sent_limits = Some(limits);
        let Some(w) = win.toplevel() else { return };
        w.set_min_size((min != (0, 0)).then_some(limits.min));
        w.set_max_size((max != (0, 0)).then_some(limits.max));
    }

    fn request(&mut self, window: WindowId, request: WindowRequest) {
        if let WindowRequest::Activate = request {
            self.activate(window);
            return;
        }
        let latest = self.seats.latest_serial();
        let Some(win) = self.windows.get_mut(&window) else { return };
        if let (Shell::Sheet(_), WindowRequest::Resize(width, height)) = (&win.shell, request) {
            self.resize(window, width, height);
            sheet::place(self, window);
            sheet::commit_parent(self, window);
            return;
        }
        let Some(w) = win.toplevel() else { return };
        match request {
            WindowRequest::Move => {
                if let Some((seat, serial)) = latest {
                    w.move_(&seat, serial);
                }
            }
            WindowRequest::Minimize => w.set_minimized(),
            WindowRequest::Maximize(true) => w.set_maximized(),
            WindowRequest::Maximize(false) => w.unset_maximized(),
            WindowRequest::Fullscreen(true) => w.set_fullscreen(None),
            WindowRequest::Fullscreen(false) => w.unset_fullscreen(),
            WindowRequest::Resize(width, height) => {
                // Between configures a client picks its own size; the
                // compositor adjusts it with the next one if it must.
                let fixed = win.state.maximized || win.state.fullscreen;
                if win.configured && !fixed {
                    self.resize(window, width, height);
                } else if !win.configured {
                    win.width = width;
                    win.height = height;
                }
                self.apply_limits(window);
            }
            WindowRequest::Activate => {}
        }
    }

    /// Ask for keyboard focus with xdg-activation, as the result of the
    /// latest input event.
    fn activate(&mut self, window: WindowId) {
        // A sheet's keyboard is its parent's.
        let window = sheet::toplevel_of(self, window);
        let (Some(activation), Some(win)) = (&self.activation, self.windows.get(&window)) else { return };
        // Compositors give tokens to the surface with the keyboard: ask with
        // ours if one has it, else with the window itself.
        let focused = self.seats.keyboard_window().and_then(|w| self.windows.get(&w));
        activation.request_token_with_data(
            &self.qh,
            Activation {
                app_id: app_id(),
                seat_and_serial: self.seats.latest_serial(),
                requester: focused.unwrap_or(win).surface().clone(),
                target: win.surface().clone(),
            },
        );
    }

    /// Size the window's canvas for its size and scale, as a configure or a
    /// scale change asks, and tell the main thread.
    fn resize(&mut self, window: WindowId, width: u32, height: u32) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let (width, height) = (width.max(1), height.max(1));
        let (pw, ph) = (px(width, win.scale), px(height, win.scale));
        let resized = width != win.width || height != win.height;
        if !win.configured || pw != win.px_width || ph != win.px_height || resized {
            win.configured = true;
            win.width = width;
            win.height = height;
            win.px_width = pw;
            win.px_height = ph;
            win.canvas = vec![BACKGROUND; (pw * ph) as usize];
            win.buffers.clear();
            win.damage.clear();
            // The main thread draws it all again; layer trees wait for that.
            win.held = true;
        }
        self.report(window);
        if resized {
            sheet::parent_resized(self, window);
        }
    }

    /// Tell the main thread the window's size, scale and state if they
    /// changed since it was last told.
    fn report(&mut self, window: WindowId) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let now = (win.width, win.height, win.scale.to_bits(), win.titlebar(), win.state);
        if win.reported == Some(now) {
            return;
        }
        win.reported = Some(now);
        let _ = self.to_main.send(FromRender::Configure {
            window,
            width: win.width,
            height: win.height,
            scale: win.scale,
            titlebar: now.3,
            state: win.state,
        });
    }

    /// The scale a new window starts at, before the compositor says which
    /// it prefers: the outputs' when they agree, so the first frame is drawn
    /// at the right scale on integer-scaled outputs. wl_output reports a
    /// fractional scale rounded up (2 for 1.5), and the fractional one comes
    /// only once the window is mapped, so there the window draws its first
    /// frame at the integer scale and draws again at the fractional one.
    fn initial_scale(&self) -> f64 {
        let mut scales = self.outputs.outputs().filter_map(|o| self.outputs.info(&o)).map(|i| i.scale_factor);
        match scales.next() {
            Some(first) if first > 0 && scales.all(|s| s == first) => first as f64,
            _ => 1.0,
        }
    }

    /// Windows the compositor hasn't configured yet start at the outputs'
    /// scale, now known.
    fn outputs_changed(&mut self) {
        let scale = self.initial_scale();
        for win in self.windows.values_mut().filter(|w| !w.configured && !w.scale_given) {
            win.scale = scale;
        }
    }

    fn set_scale(&mut self, window: WindowId, scale: f64) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        win.scale_given = true;
        if (win.scale - scale).abs() < 1e-9 || scale <= 0.0 {
            return;
        }
        win.scale = scale;
        // Tiles were drawn at the old scale; the main thread draws them again.
        tiles::rescaled(&mut win.layers);
        if let Some(d) = &mut win.decor {
            d.scale_changed();
        }
        seat::scale_changed(self, window);
        let Some(win) = self.windows.get(&window) else { return };
        if win.configured {
            // The main thread draws everything at the new scale and presents,
            // which draws the decorations too.
            let (w, h) = (win.width, win.height);
            self.resize(window, w, h);
        }
    }

    /// Paint `rects` of the window's own canvas.
    fn paint(&mut self, window: WindowId, rects: Vec<Rect>, ops: Vec<Op>) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        if !win.configured {
            return;
        }
        let scale = win.scale;
        let mut canvas = Canvas::new(&mut win.canvas, win.px_width, win.px_height, 0.0, scale as f32);
        raster::paint(&mut canvas, &mut self.glyphs, &rects, &ops);
        win.damage.extend(rects.iter().map(|r| to_px(r, scale)));
    }

    /// Draw a paint into its target.
    fn paint_target(&mut self, window: WindowId, target: Target, rects: Vec<Rect>, ops: Vec<Op>) {
        match target {
            Target::Root => self.paint(window, rects, ops),
            Target::Tiles(layer) => tiles::paint_tiles(self, window, layer, &rects, ops),
            Target::Overlay(layer) => tiles::paint_overlay(self, window, layer, &rects, &ops),
            Target::Content(_) => {}
        }
    }

    /// Rasterize tiles' pending paints for a slice of time (see `tiles`).
    fn rasterize_tiles(&mut self) {
        let until = std::time::Instant::now() + RASTER_SLICE;
        let State { windows, glyphs, .. } = self;
        for win in windows.values_mut() {
            if tiles::owed(&win.layers) && tiles::rasterize_pending(&mut win.layers, glyphs, win.scale, until) {
                // The slice is over.
                return;
            }
        }
    }

    /// The main thread owes the window a frame: its layer trees wait for
    /// its `Present`.
    fn hold(&mut self, window: WindowId) {
        if let Some(win) = self.windows.get_mut(&window) {
            win.held = true;
        }
    }

    /// Draw what changed of a window's layer trees now, and tell the main
    /// thread what the render thread can't draw again itself. Returns how
    /// many redraws it made.
    fn ca_frame(&mut self, window: WindowId) -> usize {
        let start = std::time::Instant::now();
        let now = self.ca.now();
        let (redraws, uncovered) = self.ca.frame(window, now);
        let mut area = 0.0;
        let count = redraws.len();
        for r in redraws {
            area += r.rects.iter().map(|q| f64::from((q.x1 - q.x0) * (q.y1 - q.y0))).sum::<f64>();
            self.paint_target(window, r.target, r.rects, r.ops);
        }
        if crate::layers::tracing() && count > 0 {
            eprintln!(
                "sidestep @{:.1} ms ca frame: window {window} render: {:.3} ms, {count} redraws, {area:.0} points squared",
                crate::layers::trace_ms(),
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
        if !uncovered.is_empty() {
            let mut by_target: Vec<(Target, Vec<Rect>)> = Vec::new();
            for (t, r) in uncovered {
                match by_target.iter_mut().find(|(bt, _)| *bt == t) {
                    Some((_, rs)) => rs.push(r),
                    None => by_target.push((t, vec![r])),
                }
            }
            for (target, rects) in by_target {
                self.send(FromRender::Repaint { window, target, rects });
            }
        }
        if let Some(win) = self.windows.get_mut(&window) {
            win.ca_pending = false;
        }
        count
    }

    /// Windows whose layer trees changed or animate draw a frame, now if
    /// their last one showed, else when it does.
    fn animate_all(&mut self) {
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for window in windows {
            if self.ca.wants_frame(window) {
                self.animate(window);
            }
        }
    }

    fn animate(&mut self, window: WindowId) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        if !win.configured {
            return;
        }
        if win.held || (!win.frame_signalled && win.frame_seq > 0) {
            win.ca_pending = true;
            return;
        }
        if self.ca_frame(window) > 0 {
            self.present(window);
        } else if self.ca.wants_frame(window) {
            // Animating but nothing looked different: the next frame's
            // time, without presenting.
            self.request_frame(window);
        }
    }

    /// Ask for a frame callback without new content (display links tick
    /// with nothing to draw).
    fn request_frame(&mut self, window: WindowId) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        win.frame_seq += 1;
        win.frame_signalled = false;
        let seq = win.frame_seq;
        let surface = win.surface().clone();
        surface.frame(&self.qh, FrameTag { window, seq });
        surface.commit();
    }

    /// Whether buffers take canvas pixels as they are. Canvases are RGBA
    /// in memory, which wl_shm calls XBGR8888: shown as they are where the
    /// compositor takes it, else swizzled to XRGB8888. The formats arrive
    /// as events after wl_shm is bound, so this is decided when the first
    /// buffer is made (a window is configured by then, so they're in), and
    /// kept: every buffer has the same format.
    fn rgba(&mut self) -> bool {
        let shm = &self.shm;
        *self.rgba.get_or_insert_with(|| shm.formats().contains(&wl_shm::Format::Xbgr8888))
    }

    fn present(&mut self, window: WindowId) {
        if !self.windows.get(&window).is_some_and(|w| w.configured) {
            return;
        }
        let rgba = self.rgba();
        let Some(win) = self.windows.get_mut(&window) else { return };
        win.frame_seq += 1;
        win.frame_signalled = false;
        let seq = win.frame_seq;
        let layers = tiles::present(self, window, seq);
        let Some(win) = self.windows.get_mut(&window) else { return };
        if crate::layers::tracing() {
            tiles::trace(window, &layers, &win.damage);
        }
        if !win.damage.is_empty() {
            let damage = std::mem::take(&mut win.damage);
            let free = win.buffers.iter().position(|b| b.buffer.canvas(&mut self.pool).is_some());
            let index = match free {
                Some(i) => i,
                None => {
                    let (buffer, _) = self
                        .pool
                        .create_buffer(win.px_width as i32, win.px_height as i32, win.px_width as i32 * 4, format(rgba))
                        .expect("sidestep: window buffer");
                    let full = Rect::new(0.0, 0.0, win.px_width as f32, win.px_height as f32);
                    win.buffers.push(RootBuffer { buffer, stale: vec![full] });
                    win.buffers.len() - 1
                }
            };
            // As tiles' buffers do (see `tiles`).
            let mut stale: Vec<&mut Vec<Rect>> = win.buffers.iter_mut().map(|b| &mut b.stale).collect();
            let stale = tiles::pass_on(&mut stale, index, &damage);
            let surface = win.surface().clone();
            let target = &mut win.buffers[index];
            let pixels = as_pixels(target.buffer.canvas(&mut self.pool).expect("free buffer"));
            for r in tiles::missed(&stale, &damage) {
                copy_rows(pixels, &win.canvas, win.px_width, win.px_height, r, rgba);
            }
            target.buffer.attach_to(&surface).expect("sidestep: attach window buffer");
            for r in &damage {
                let r = r.round_out();
                surface.damage_buffer(r.x0 as i32, r.y0 as i32, (r.x1 - r.x0) as i32, (r.y1 - r.y0) as i32);
            }
            win.viewport.set_destination(win.width as i32, win.height as i32);
        }
        self.place_decorations(window);
        let Some(win) = self.windows.get(&window) else { return };
        let surface = win.surface();
        surface.frame(&self.qh, FrameTag { window, seq });
        surface.commit();
    }

    /// Set the window geometry, and draw and place the decorations for the
    /// window's current size, ahead of a commit of the root surface.
    fn place_decorations(&mut self, window: WindowId) {
        let rgba = self.rgba();
        let Some(win) = self.windows.get_mut(&window) else { return };
        let bar = win.titlebar() as i32;
        let geometry = (0, -bar, win.width as i32, win.height as i32 + bar);
        if win.geometry != geometry {
            win.geometry = geometry;
            match &win.shell {
                Shell::Toplevel(w) => {
                    w.xdg_surface().set_window_geometry(geometry.0, geometry.1, geometry.2, geometry.3)
                }
                Shell::Popup(p) => p.xdg_surface().set_window_geometry(0, 0, geometry.2, geometry.3),
                Shell::Sheet(_) => {}
            }
        }
        let bar = win.bar_height();
        if let Some(menubar) = &mut win.menubar {
            menubar.draw(win.width, bar, win.scale, rgba, &mut self.pool, &mut self.glyphs);
        }
        let Some(mut decor) = win.decor.take() else { return };
        decor.draw(&frame_info(win), &mut self.pool, &mut self.glyphs);
        win.decor = Some(decor);
    }

    /// Draw decorations that changed (hover, focus, style) between presents,
    /// and show them. Only for changes no present brings: after a resize
    /// the present does it, with the content drawn for the new size.
    fn refresh_decor(&mut self, window: WindowId) {
        let Some(win) = self.windows.get(&window) else { return };
        if !win.configured {
            return;
        }
        let bar = win.titlebar() as i32;
        let geometry = (0, -bar, win.width as i32, win.height as i32 + bar);
        let redraw = win.decor.as_ref().is_some_and(|d| d.needs_draw(&frame_info(win)))
            || win.menubar.as_ref().is_some_and(menubar::Bar::needs_draw);
        if !redraw && win.geometry == geometry {
            return;
        }
        self.place_decorations(window);
        // Decorations are synchronized subsurfaces: what they show changes
        // with the root surface's next commit.
        if let Some(win) = self.windows.get(&window) {
            win.surface().commit();
        }
    }

    /// Create or remove decorations after the decoration mode, style or
    /// state changed.
    fn update_decorations(&mut self, window: WindowId) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let wanted = win.toplevel().is_some() && win.style.titled && (win.client_side || force_client_decorations());
        if wanted && win.decor.is_none() {
            let mut d = decor::Decor::new(
                win.surface(),
                &self.compositor,
                &self.subcompositor,
                self.viewporter.get().expect("viewporter"),
                &self.qh,
            );
            for (id, part) in d.surfaces() {
                self.roles.insert(id, Role::Decor(window, part));
            }
            if win.passthrough {
                d.set_passthrough(self.empty_region.as_ref().map(Region::wl_region));
            }
            win.decor = Some(d);
        } else if !wanted && let Some(d) = win.decor.take() {
            for (id, _) in d.surfaces() {
                self.roles.remove(&id);
            }
        }
    }

    /// Give the window's surfaces the input regions its style asks for:
    /// empty ones when pointer input goes through to what's behind.
    fn apply_input(&mut self, window: WindowId) {
        let wanted = match self.windows.get(&window) {
            Some(w) if w.passthrough != w.style.passthrough => w.style.passthrough,
            _ => return,
        };
        if wanted && self.empty_region.is_none() {
            self.empty_region = Region::new(&self.compositor).ok();
        }
        let empty = if wanted { self.empty_region.as_ref().map(Region::wl_region) } else { None };
        let Some(win) = self.windows.get_mut(&window) else { return };
        win.passthrough = empty.is_some();
        if let Some(d) = &mut win.decor {
            d.set_passthrough(empty);
        }
        // The root surface's commit applies its own region and, as the
        // others are synchronized subsurfaces, theirs.
        win.surface().set_input_region(empty);
        if win.configured {
            win.surface().commit();
        }
    }

    fn role(&self, surface: &WlSurface) -> Option<Role> {
        self.roles.get(&surface.id()).copied()
    }

    /// Where a content surface's origin is in its window, in points: the
    /// window's own surface takes the content's input (tiles take none),
    /// and the menu bar sits above it.
    fn content_offset(&self, role: Role) -> (f64, f64) {
        match role {
            Role::Bar(window) => (0.0, -(self.windows.get(&window).map_or(0, Win::bar_height) as f64)),
            _ => (0.0, 0.0),
        }
    }
}

/// The application id compositors group windows by: the program's name.
fn app_id() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "sidestep".into())
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, factor: i32) {
        // The integer scale, used when fractional scales aren't offered.
        if self.fractional.is_some() {
            return;
        }
        if let Some(Role::Root(window)) = self.role(surface) {
            self.set_scale(window, factor.max(1) as f64);
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: wl_output::Transform) {}
    // Frame callbacks are requested with a FrameTag instead; see below.
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        output: &wl_output::WlOutput,
    ) {
        outputs::surface_moved(self, surface, output, true);
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        output: &wl_output::WlOutput,
    ) {
        outputs::surface_moved(self, surface, output, false);
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed();
        outputs::changed(self);
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed();
        outputs::changed(self);
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, output: wl_output::WlOutput) {
        outputs::destroyed(self, &output);
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        if let Some(Role::Root(id)) = self.role(window.wl_surface()) {
            self.send(FromRender::CloseRequested { window: id });
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        window: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let Some(Role::Root(id)) = self.role(window.wl_surface()) else { return };
        let keyboard = seat::has_keyboard(self);
        let Some(win) = self.windows.get_mut(&id) else { return };
        let was_active = win.state.activated;
        win.state = WindowState {
            maximized: configure.is_maximized(),
            fullscreen: configure.is_fullscreen(),
            activated: configure.is_activated(),
            tiled: configure.is_tiled()
                || configure.is_tiled_left()
                || configure.is_tiled_right()
                || configure.is_tiled_top()
                || configure.is_tiled_bottom(),
            suspended: configure.state.contains(smithay_client_toolkit::reexports::csd_frame::WindowState::SUSPENDED),
            resizing: configure.state.contains(smithay_client_toolkit::reexports::csd_frame::WindowState::RESIZING),
        };
        win.client_side = configure.decoration_mode == DecorationMode::Client;
        if !keyboard && win.state.activated != was_active {
            // No keyboard, so no keyboard focus: activation stands for it.
            let focused = win.state.activated;
            if focused {
                self.seats.focus_gained(id);
            } else {
                self.seats.focus_lost(id);
            }
            self.send(FromRender::Focus { window: id, focused });
        }
        self.update_decorations(id);
        let Some(win) = self.windows.get_mut(&id) else { return };
        if let Some(d) = &mut win.decor {
            d.set_capabilities(configure.capabilities);
        }
        // The configured size is the window geometry: the title bar and
        // the content.
        let bar = win.titlebar();
        let width = configure.new_size.0.map_or(win.width, |w| w.get());
        let height = configure.new_size.1.map_or(win.height, |h| h.get().saturating_sub(bar));
        let before = win.configured.then_some((win.width, win.height));
        outputs::learned(self, id, None, configure.suggested_bounds);
        self.apply_limits(id);
        self.resize(id, width, height);
        // A first configure or a new size has the main thread draw and
        // present, and the present draws the decorations for the new size
        // with the content. Anything else (focus, tiling, maximized at the
        // same size) shows now.
        let after = self.windows.get(&id).map(|w| (w.width, w.height));
        if before.is_some() && before == after {
            self.refresh_decor(id);
        }
    }
}

/// A menu's placement on its positioner (see `PopupLayout`).
fn menu_layout(positioner: &XdgPositioner, layout: crate::protocol::PopupLayout) {
    use crate::protocol::Corner;
    let anchor = match layout.corner {
        Corner::TopLeft => Anchor::TopLeft,
        Corner::TopRight => Anchor::TopRight,
        Corner::BottomLeft => Anchor::BottomLeft,
        Corner::BottomRight => Anchor::BottomRight,
    };
    let gravity = match layout.gravity {
        Corner::TopLeft => Gravity::TopLeft,
        Corner::TopRight => Gravity::TopRight,
        Corner::BottomLeft => Gravity::BottomLeft,
        Corner::BottomRight => Gravity::BottomRight,
    };
    positioner.set_anchor(anchor);
    positioner.set_gravity(gravity);
    positioner.set_offset(layout.offset.0, layout.offset.1);
    let mut adjust = ConstraintAdjustment::empty();
    for (on, bit) in [
        (layout.flip_x, ConstraintAdjustment::FlipX),
        (layout.flip_y, ConstraintAdjustment::FlipY),
        (layout.slide_x, ConstraintAdjustment::SlideX),
        (layout.slide_y, ConstraintAdjustment::SlideY),
        (layout.resize_y, ConstraintAdjustment::ResizeY),
    ] {
        if on {
            adjust |= bit;
        }
    }
    positioner.set_constraint_adjustment(adjust);
}

impl PopupHandler for State {
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup, config: PopupConfigure) {
        let Some(Role::Root(id)) = self.role(popup.wl_surface()) else { return };
        self.resize(id, config.width.max(1) as u32, config.height.max(1) as u32);
    }

    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        if let Some(Role::Root(id)) = self.role(popup.wl_surface()) {
            self.send(FromRender::PopupDone { window: id });
        }
    }
}

/// An activation token request: the surface that asks, and the one the
/// token activates.
pub(crate) struct Activation {
    app_id: String,
    seat_and_serial: Option<(WlSeat, u32)>,
    requester: WlSurface,
    target: WlSurface,
}

impl RequestDataExt for Activation {
    fn app_id(&self) -> Option<&str> {
        Some(&self.app_id)
    }

    fn seat_and_serial(&self) -> Option<(&WlSeat, u32)> {
        self.seat_and_serial.as_ref().map(|(seat, serial)| (seat, *serial))
    }

    fn surface(&self) -> Option<&WlSurface> {
        Some(&self.requester)
    }
}

impl ActivationHandler for State {
    type RequestData = Activation;

    fn new_token(&mut self, token: String, data: &Activation) {
        if let Some(activation) = &self.activation {
            activation.activate::<State>(&data.target, token);
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, seat::Seats];
}

impl AsMut<SimpleGlobal<WpViewporter, 1>> for State {
    fn as_mut(&mut self) -> &mut SimpleGlobal<WpViewporter, 1> {
        &mut self.viewporter
    }
}

impl Dispatch<WlCallback, FrameTag> for State {
    fn event(
        state: &mut State,
        _: &WlCallback,
        event: wl_callback::Event,
        tag: &FrameTag,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let wl_callback::Event::Done { .. } = event else { return };
        let Some(win) = state.windows.get_mut(&tag.window) else { return };
        if win.frame_seq == tag.seq && !win.frame_signalled {
            win.frame_signalled = true;
            // The main thread hears only of frames it waits for, so an
            // animation the render thread draws alone costs it nothing.
            if std::mem::replace(&mut win.main_waiting, false) {
                let _ = state.to_main.send(FromRender::Frame { window: tag.window });
            }
            let ticks = win.ticks;
            let pending = win.ca_pending;
            if ticks {
                let time = crate::quartzcore::math::media_now();
                let _ = state.to_main.send(FromRender::Tick { window: tag.window, time });
            }
            if pending || state.ca.wants_frame(tag.window) {
                state.animate(tag.window);
            } else if ticks {
                state.request_frame(tag.window);
            }
        }
    }
}

impl Dispatch<WpViewport, ()> for State {
    fn event(_: &mut State, _: &WpViewport, _: wp_viewport::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WpFractionalScaleManagerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<WpFractionalScaleV1, WindowId> for State {
    fn event(
        state: &mut State,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        window: &WindowId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            // In 120ths.
            state.set_scale(*window, scale as f64 / 120.0);
            outputs::learned(state, *window, Some(scale as f64 / 120.0), None);
        }
    }
}

delegate_compositor!(State);
delegate_subcompositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);
delegate_xdg_popup!(State);
delegate_activation!(State, Activation);
delegate_simple!(State, WpViewporter, 1);
delegate_registry!(State);
