//! The render thread. It owns the Wayland connection and each window's
//! surfaces:
//!
//! - the root layer is the content's surface, its paints rasterized at the
//!   present straight into one of a few shared-memory buffers (a large one
//!   in bands on several threads, `raster::bands`), which first catches up
//!   with the one shown last where it missed frames, damaging only what
//!   changed (see `State::draw_frame`). A popup's, a sheet's or a layer
//!   surface's ([`layer`]) is its own surface; a toplevel's is a
//!   subsurface of its frame ([`Frame`]);
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
//! for a callback on the content's surface and on one tile or overlay, and
//! the first to fire counts: compositors send none to a surface they
//! consider hidden, and tiles can cover the content's surface completely.

mod decor;
mod dnd;
mod drag_out;
mod keyboard;
mod layer;
mod lock;
mod menubar;
pub mod null;
mod outputs;
mod seat;
mod selection;
mod sheet;
mod textinput;
mod tiles;

pub use decor::{HEADER, TITLE_SIZE, explicit_theme};

/// Milliseconds a run of `f` takes, the median of seven, for the timing
/// tests (`cargo test --release -p sidestep-appkit timing -- --ignored
/// --nocapture`, and the engine's own).
#[doc(hidden)]
pub fn median(mut f: impl FnMut()) -> f64 {
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
use smithay_client_toolkit::reexports::client::protocol::wl_region::WlRegion;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
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
use smithay_client_toolkit::session_lock::{SessionLock, SessionLockState, SessionLockSurface};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{LayerShell, LayerSurface};
use smithay_client_toolkit::shell::xdg::popup::{Popup, PopupConfigure, PopupHandler};
use smithay_client_toolkit::shell::xdg::window::{
    DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::{XdgPositioner, XdgShell, XdgSurface};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::{
    delegate_activation, delegate_compositor, delegate_layer, delegate_output, delegate_registry,
    delegate_session_lock, delegate_shm, delegate_simple, delegate_subcompositor, delegate_xdg_popup,
    delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};

use crate::protocol::{
    Cursor, FromRender, LayerPlacement, Op, PopupPlacement, Rect, SizeLimits, Style, Target, TitleText, ToRender,
    WindowId, WindowRequest, WindowState,
};
use crate::raster::{self, Canvas, Glyphs};

/// The main thread's end of the render thread.
pub struct Backend {
    pub tx: channel::Sender<ToRender>,
    pub rx: mpsc::Receiver<FromRender>,
}

/// How the render thread wakes the main thread's loop: called after every
/// message it sends there, and once more when it stops (its sender gone,
/// the channel then reads as closed). It runs on the render thread, so it
/// only signals (a run loop source, a condition variable) and returns.
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;

/// The render thread's sender to the main thread: each message wakes the
/// main loop through the [`Waker`]. When the render thread stops (a
/// Wayland protocol error ends its connection) the sender goes away, which
/// also wakes the loop, to see the channel closed.
pub struct MainSender {
    tx: Option<mpsc::Sender<FromRender>>,
    wake: Waker,
}

impl MainSender {
    pub fn new(tx: mpsc::Sender<FromRender>, wake: Waker) -> MainSender {
        MainSender { tx: Some(tx), wake }
    }

    pub fn send(&self, msg: FromRender) -> Result<(), mpsc::SendError<FromRender>> {
        self.tx.as_ref().expect("the sender is only taken when dropped").send(msg)?;
        (self.wake)();
        Ok(())
    }
}

impl Drop for MainSender {
    fn drop(&mut self) {
        // Closed first, so the woken loop finds it closed.
        drop(self.tx.take());
        (self.wake)();
    }
}

/// Start the render thread; `wake` wakes the main thread's loop after each
/// message to it (see [`Waker`]). A process has one render thread at a
/// time: the toolkit running starts it and keeps the [`Backend`], and when
/// it drops the `Backend` the render thread ends (its channel closes), so
/// another may start after it.
pub fn start(wake: Waker) -> Backend {
    let backend = if null::chosen() {
        null::start(wake)
    } else {
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
    };
    *ANY_THREAD.write().unwrap_or_else(|e| e.into_inner()) = Some(backend.tx.clone());
    backend
}

/// The latest render thread's inbox, for threads other than the main one.
static ANY_THREAD: std::sync::RwLock<Option<channel::Sender<ToRender>>> = std::sync::RwLock::new(None);

/// Send to the render thread from any thread, if it's running; if it
/// isn't (no window was ever shown, or the toolkit that ran it stopped),
/// the message is dropped: without a window there's no Wayland focus to
/// act with.
pub fn send_if_running(msg: ToRender) {
    if let Some(tx) = ANY_THREAD.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        null::sending();
        let _ = tx.send(msg);
    }
}

/// The compositor's key repeat settings, for `+[NSEvent keyRepeatDelay]`
/// and `keyRepeatInterval`: milliseconds before the first repeat, and
/// repeats per second (0 when keys don't repeat).
static REPEAT_DELAY_MS: AtomicU32 = AtomicU32::new(600);
static REPEAT_RATE: AtomicU32 = AtomicU32::new(25);

/// Seconds before a held key repeats, and between repeats (0 if keys don't
/// repeat).
pub fn key_repeat() -> (f64, f64) {
    let delay = REPEAT_DELAY_MS.load(Ordering::Relaxed) as f64 / 1000.0;
    let rate = REPEAT_RATE.load(Ordering::Relaxed);
    (delay, if rate == 0 { 0.0 } else { 1.0 / rate as f64 })
}

/// How long after a click the next one still counts toward a double
/// click, and how far away (points) it may land: GTK's defaults, which
/// GNOME keeps.
pub const DOUBLE_CLICK_MS: u32 = 400;
pub const DOUBLE_CLICK_DISTANCE: f64 = 5.0;

/// `SIDESTEP_DECORATIONS=client` draws decorations even where the
/// compositor would (as sway does), to see them anywhere.
pub fn force_client_decorations() -> bool {
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
            ChannelEvent::Msg(msg) if crate::trace::tracing() => {
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
        layer_shell: LayerShell::bind(&globals, &qh).ok(),
        session_locks: SessionLockState::new(&globals, &qh),
        session_lock: None,
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
        ca: crate::ca::tree::Compositor::default(),
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
    // A program that stops while it holds the session lock leaves the
    // session locked: the lock goes with the connection, not destroyed
    // (a protocol error while locked).
    if let Some(lock) = state.session_lock.take().filter(SessionLock::is_locked) {
        std::mem::forget(lock);
    }
}

/// How long the render thread rasterizes tiles between looks for events:
/// well inside a frame at 120 Hz, so a frame callback or a present waits
/// on it little.
pub const RASTER_SLICE: std::time::Duration = std::time::Duration::from_micros(2_000);

pub struct State {
    registry: RegistryState,
    outputs: OutputState,
    seats: seat::Seats,
    selection: selection::Selection,
    text_inputs: textinput::TextInputs,
    activation: Option<ActivationState>,
    /// wlr-layer-shell, where the compositor offers it (see `layer`).
    layer_shell: Option<LayerShell>,
    /// ext-session-lock (see `lock`), and the lock asked for or held.
    session_locks: SessionLockState,
    session_lock: Option<SessionLock>,
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
    /// Core Animation's layer trees, animated here (see `ca::tree`).
    ca: crate::ca::tree::Compositor,
    exit: bool,
}

/// What a surface is to input. Scroll layers' surfaces take no input
/// (see `tiles`): what's over them goes to the content's surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    /// A window's content.
    Root(WindowId),
    /// A toplevel's frame (see `Frame`). It takes no pointer input, but
    /// has the keyboard and text input.
    Frame(WindowId),
    /// Part of the decorations.
    Decor(WindowId, decor::Part),
    /// The main menu's bar above the content (see `menubar`).
    Bar(WindowId),
}

impl Role {
    fn window(self) -> WindowId {
        match self {
            Role::Root(w) | Role::Frame(w) | Role::Decor(w, _) | Role::Bar(w) => w,
        }
    }
}

enum Shell {
    Toplevel(Frame),
    Popup(Popup),
    /// Part of another window (see `sheet`).
    Sheet(sheet::Sheet),
    /// A desktop surface (see `layer`).
    Layer(LayerSurface),
    /// The session lock's surface on an output, by its id (see `lock`).
    Lock(SessionLockSurface, u32),
}

/// A toplevel's own surface, its frame: exactly the window geometry, the
/// title bar and the content. The content's surface, its scroll tiles, the
/// decorations, the menu bar and sheets are all subsurfaces of the frame,
/// side by side: nested subsurfaces stack wrongly in some compositors
/// (Hyprland draws a tree a level at a time). The window geometry starts at
/// the frame's origin, as compositors that place a window by its surface
/// rather than by its geometry (Hyprland) need: with the content as the
/// toplevel's surface, the title bar hangs above it, outside the place they
/// give the window. The frame shows one transparent pixel stretched over
/// the geometry and takes no pointer input, so what's seen and clicked is
/// what's on it. Everything but sheets is synchronized with it, so one
/// commit of the frame, after the others', shows the content, its tiles
/// and decorations, the frame's size and the content's place in it
/// together.
struct Frame {
    window: Window,
    /// The content's subsurface of the frame, and its surface.
    subsurface: WlSubsurface,
    content: WlSurface,
    viewport: WpViewport,
    /// The pixel the frame shows, attached at its first placing.
    pixel: Option<Buffer>,
    /// The content's size and its top in the frame, in points, as last
    /// placed.
    placed: Option<(u32, u32, u32)>,
}

impl Frame {
    /// Size the frame for `width` × `height` points of content under `top`
    /// points of title bar, as of its next commit. Returns whether the
    /// content moved in the frame.
    fn place(&mut self, width: u32, height: u32, top: u32, pool: &mut SlotPool) -> bool {
        if self.pixel.is_none()
            && let Ok((buffer, bytes)) = pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888)
        {
            bytes[..4].fill(0);
            let surface = self.window.wl_surface();
            let _ = buffer.attach_to(surface);
            surface.damage_buffer(0, 0, 1, 1);
            self.pixel = Some(buffer);
        }
        let placed = (width, height, top);
        let before = self.placed.replace(placed);
        if before != Some(placed) {
            self.viewport.set_destination(width as i32, (height + top) as i32);
        }
        let moved = before.map(|(_, _, t)| t) != Some(top);
        if moved {
            self.subsurface.set_position(0, top as i32);
        }
        moved
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // The content goes first; the frame's surface goes with `window`.
        self.subsurface.destroy();
        self.content.destroy();
        self.viewport.destroy();
    }
}

pub struct Win {
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
    /// The paints and moves since the last present, drawn into a buffer
    /// when it comes.
    pending: Vec<Pending>,
    /// Their damage in device pixels.
    damage: Vec<Rect>,
    buffers: Vec<RootBuffer>,
    /// See-through where it draws nothing: its buffers carry alpha.
    transparent: bool,
    /// The buffer presented last, which holds the window as it shows.
    front: Option<usize>,
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
    /// Where the content takes pointer input, if not everywhere.
    input_region: Option<Vec<Rect>>,
    /// The window a popup is placed against (0 for none).
    popup_parent: WindowId,
    /// For `stats`: the time the frame's paints took to rasterize, and
    /// the device pixels they redrew.
    raster_time: std::time::Duration,
    raster_pixels: f64,
}

impl Win {
    /// The content's surface, which the root layer is shown on.
    fn surface(&self) -> &WlSurface {
        match &self.shell {
            Shell::Toplevel(f) => &f.content,
            Shell::Popup(p) => p.wl_surface(),
            Shell::Sheet(s) => s.surface(),
            Shell::Layer(l) => l.wl_surface(),
            Shell::Lock(l, _) => l.wl_surface(),
        }
    }

    /// The surface the shell knows the window by: a toplevel's frame, or
    /// the content's surface. The content starts `titlebar` points down it.
    fn main_surface(&self) -> &WlSurface {
        match &self.shell {
            Shell::Toplevel(f) => f.window.wl_surface(),
            Shell::Popup(_) | Shell::Sheet(_) | Shell::Layer(_) | Shell::Lock(..) => self.surface(),
        }
    }

    fn toplevel(&self) -> Option<&Window> {
        match &self.shell {
            Shell::Toplevel(f) => Some(&f.window),
            Shell::Popup(_) | Shell::Sheet(_) | Shell::Layer(_) | Shell::Lock(..) => None,
        }
    }

    /// The window geometry for the window's size: the title bar and the
    /// content, from the main surface's origin.
    fn expected_geometry(&self) -> (i32, i32, i32, i32) {
        (0, 0, self.width as i32, (self.height + self.titlebar()) as i32)
    }

    /// Commit the content's surface, then the frame it is a synchronized
    /// subsurface of, which shows what the content committed.
    fn commit(&self) {
        self.surface().commit();
        if let Shell::Toplevel(f) = &self.shell {
            f.window.wl_surface().commit();
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

/// What a window's surface was asked to draw before its next present.
enum Pending {
    /// Ops over rectangles, in points.
    Paint(Vec<Rect>, Vec<Op>),
    /// A rectangle of device pixels moved by `dx`, `dy` of them.
    Scroll { rect: Rect, dx: i32, dy: i32 },
}

/// Pixels for `points` at `scale`, as wp_fractional_scale_v1 asks buffers
/// to be sized: rounded half up.
pub fn px(points: u32, scale: f64) -> u32 {
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

/// A buffer format for a window: with alpha for a see-through one (its
/// pixels are premultiplied, as wl_shm's alpha formats take them), else
/// without, so the compositor needn't draw what's under it.
fn window_format(rgba: bool, transparent: bool) -> wl_shm::Format {
    match (rgba, transparent) {
        (true, true) => wl_shm::Format::Abgr8888,
        (false, true) => wl_shm::Format::Argb8888,
        (rgba, false) => format(rgba),
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn see_through_windows_get_alpha_and_the_rest_dont() {
        assert_eq!(window_format(true, false), wl_shm::Format::Xbgr8888);
        assert_eq!(window_format(false, false), wl_shm::Format::Xrgb8888);
        assert_eq!(window_format(true, true), wl_shm::Format::Abgr8888);
        assert_eq!(window_format(false, true), wl_shm::Format::Argb8888);
    }
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

/// A rectangle of device pixels, clamped to a `width` × `height` buffer:
/// x0, y0, x1, y1.
fn clamped(r: &Rect, width: u32, height: u32) -> (usize, usize, usize, usize) {
    let r = r.round_out();
    let x0 = r.x0.max(0.0) as usize;
    let y0 = r.y0.max(0.0) as usize;
    let x1 = (r.x1.min(width as f32).max(0.0) as usize).max(x0);
    let y1 = (r.y1.min(height as f32).max(0.0) as usize).max(y0);
    (x0, y0, x1, y1)
}

/// Fill a rectangle of device pixels with `pixel`.
fn fill_rows(px: &mut [u32], width: u32, height: u32, r: &Rect, pixel: u32) {
    let (x0, y0, x1, y1) = clamped(r, width, height);
    let w = width as usize;
    for y in y0..y1 {
        px[y * w + x0..y * w + x1].fill(pixel);
    }
}

/// Swap red and blue in rectangles of device pixels: canvas pixels to
/// XRGB8888's, and back.
fn swizzle(px: &mut [u32], width: u32, height: u32, rects: &[Rect]) {
    let w = width as usize;
    for r in rects {
        let (x0, y0, x1, y1) = clamped(r, width, height);
        for y in y0..y1 {
            px[y * w + x0..y * w + x1].iter_mut().for_each(|p| *p = raster::to_xrgb(*p));
        }
    }
}

/// Where a rectangle of device pixels moved by `dx`, `dy` lands within
/// itself: what a move changes, but for the strip it uncovers.
fn moved(r: &Rect, dx: i32, dy: i32) -> Rect {
    let (dx, dy) = (dx as f32, dy as f32);
    r.intersect(&Rect::new(r.x0 + dx, r.y0 + dy, r.x1 + dx, r.y1 + dy))
}

/// Rectangles covering what `rects` cover, without overlapping.
fn disjoint(rects: &[Rect]) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for r in rects.iter().map(Rect::round_out) {
        let mut pieces = vec![r];
        for o in &out {
            pieces = pieces.iter().flat_map(|p| subtract(p, o)).collect();
        }
        out.extend(pieces);
    }
    out
}

/// `a` less `b`: up to four rectangles.
fn subtract(a: &Rect, b: &Rect) -> Vec<Rect> {
    let i = a.intersect(b);
    if i.is_empty() {
        return vec![*a];
    }
    [
        Rect::new(a.x0, a.y0, a.x1, i.y0),
        Rect::new(a.x0, i.y1, a.x1, a.y1),
        Rect::new(a.x0, i.y0, i.x0, i.y1),
        Rect::new(i.x1, i.y0, a.x1, i.y1),
    ]
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// A buffer's pixels, for workers each moving their own rows or columns
/// of it.
#[derive(Clone, Copy)]
struct Pixels(*mut u32);

// SAFETY: workers given a buffer's pixels touch only their own rows or
// columns of it (see `move_pixels`).
unsafe impl Send for Pixels {}
unsafe impl Sync for Pixels {}

impl Pixels {
    /// The pixel `i` pixels on.
    ///
    /// # Safety
    ///
    /// It is within the buffer.
    unsafe fn at(self, i: usize) -> *mut u32 {
        // SAFETY: as the caller promised.
        unsafe { self.0.add(i) }
    }
}

/// Copy `dest` (device pixels: x0, y0, x1, y1) of the pixels at `to` from
/// those at `from` `dx`, `dy` back: from another buffer of the same size,
/// or from the same one (a scroll). A large one is shared with the raster
/// workers, in bands of rows, or of columns where a vertical move within
/// one buffer would have one band read rows another writes (a move both
/// ways within one buffer stays on one thread).
///
/// # Safety
///
/// `from` and `to` each point to a buffer of `width` pixels a row holding
/// `dest`, and `dest` moved back by `dx`, `dy`; nothing else touches the
/// pixels of `dest` at `to` meanwhile, nor writes those at `from`.
unsafe fn move_pixels(from: Pixels, to: Pixels, width: usize, dest: (usize, usize, usize, usize), dx: i32, dy: i32) {
    let (x0, y0, x1, y1) = dest;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let within = std::ptr::eq(from.0, to.0);
    // Rows in the order that reads each before it's written, within one.
    let part = move |(x0, y0, x1, y1): (usize, usize, usize, usize)| {
        let row = |y: usize| {
            let src = (y as i64 - i64::from(dy)) as usize * width + (x0 as i64 - i64::from(dx)) as usize;
            // SAFETY: as the caller promised; `copy` allows the overlap of
            // a horizontal move within a row.
            unsafe { std::ptr::copy(from.at(src), to.at(y * width + x0), x1 - x0) };
        };
        if within && dy > 0 { (y0..y1).rev().for_each(row) } else { (y0..y1).for_each(row) }
    };
    let area = (x1 - x0) * (y1 - y0);
    let n =
        if area >= raster::bands::BANDED && !(within && dx != 0 && dy != 0) { raster::bands::workers() + 1 } else { 1 };
    let by_columns = within && dy != 0;
    let piece = |i: usize| {
        if by_columns {
            (x0 + (x1 - x0) * i / n, y0, x0 + (x1 - x0) * (i + 1) / n, y1)
        } else {
            (x0, y0 + (y1 - y0) * i / n, x1, y0 + (y1 - y0) * (i + 1) / n)
        }
    };
    let others: Vec<Box<dyn FnOnce() + Send>> =
        (1..n).map(|i| -> Box<dyn FnOnce() + Send> { Box::new(move || part(piece(i))) }).collect();
    raster::bands::scoped(others, || part(piece(0)));
}

/// Copy rectangles of device pixels from buffer `from` (which the
/// compositor may still be showing: it's only read) into buffer `to`, the
/// same size, `dx`, `dy` back.
fn copy_between(pool: &mut SlotPool, from: &Buffer, to: &Buffer, size: (u32, u32), rects: &[Rect], dx: i32, dy: i32) {
    let (width, height) = size;
    let source = Pixels(as_pixels(pool.raw_data_mut(&from.slot())).as_mut_ptr());
    let Some(bytes) = to.canvas(pool) else { return };
    let target = Pixels(as_pixels(bytes).as_mut_ptr());
    for r in rects {
        // SAFETY: two slots of the pool, the same size, which stays mapped
        // where it is while nothing makes buffers; `dest` and its source
        // are within them (`moved`, `clamped`), and the target is ours.
        unsafe { move_pixels(source, target, width as usize, clamped(r, width, height), dx, dy) };
    }
}

/// Move a rectangle of device pixels within `px` by `dx`, `dy` (whole
/// pixels): what lands within it is what was `dx`, `dy` back; the rest of
/// it stays as it was.
pub(super) fn scroll_in(px: &mut [u32], width: u32, height: u32, r: &Rect, dx: i32, dy: i32) {
    let r = r.round_out().intersect(&Rect::new(0.0, 0.0, width as f32, height as f32));
    let dest = clamped(&moved(&r, dx, dy), width, height);
    let all = Pixels(px.as_mut_ptr());
    // SAFETY: `px` is borrowed whole meanwhile; `dest` and its source are
    // within `r`, which is within it.
    unsafe { move_pixels(all, all, width as usize, dest, dx, dy) };
}

/// The device pixels a frame's first paint (`rects`, `ops`) replaces before
/// it draws over what's there: those its leading ops fill replacing what's
/// there, or with an opaque color. (Later paints may draw over pixels an
/// earlier one blended with.)
fn replaced(rects: &[Rect], ops: &[Op], width: u32, height: u32, scale: f64) -> Vec<Rect> {
    let probe = Canvas::new(&mut [], width, height, 0.0, scale as f32);
    let mut replaced = Vec::new();
    for op in ops {
        let rect = match op {
            Op::FillWith { rect, blend: crate::protocol::Blend::Copy | crate::protocol::Blend::Clear, .. } => rect,
            Op::Fill { rect, color } if raster::opaque(*color) => rect,
            _ => break,
        };
        for paint in rects {
            if let Some((x0, y0, x1, y1)) = probe.pixels(&rect.intersect(paint)) {
                replaced.push(Rect::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32));
            }
        }
    }
    replaced
}

/// A shared-memory buffer's bytes as pixels. Pool slots are page-aligned.
pub fn as_pixels(bytes: &mut [u8]) -> &mut [u32] {
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
            ToRender::CreateWindow {
                window,
                width,
                height,
                title,
                style,
                limits,
                popup,
                sheet_of,
                layer,
                lock,
                transparent,
            } => {
                self.create_window(window, width, height, title, style, limits, popup, sheet_of, layer, lock);
                if let Some(w) = self.windows.get_mut(&window) {
                    w.transparent = transparent;
                }
            }
            ToRender::LockSession => lock::lock(self),
            ToRender::UnlockSession => lock::unlock(self),
            ToRender::RequestActivationToken { request, app_id } => self.request_token(request, app_id),
            ToRender::StartDrag { window, drag, contents, actions, icon } => {
                drag_out::start(self, window, drag, contents, actions, icon)
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
            ToRender::Scroll { window, rect, dx, dy } => self.scroll(window, rect, dx, dy),
            ToRender::Present { window } => {
                if let Some(win) = self.windows.get_mut(&window) {
                    win.main_waiting = true;
                    win.held = false;
                }
                // Its paints were rasterized as they came; layer trees drawn
                // now are rasterized as part of presenting.
                let started = crate::stats::on().then(std::time::Instant::now);
                let before = self.windows.get(&window).map(|w| w.raster_time).unwrap_or_default();
                self.ca_frame(window);
                self.present(window);
                if let Some(started) = started {
                    self.frame_measured(window, started.elapsed(), before);
                }
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
            ToRender::CaptureWindow { reply, .. } => {
                // Pixel capture is a null-backend diagnostic only.
                let _ = reply.send(None);
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
            ToRender::SetInputRegion { window, rects } => {
                if let Some(win) = self.windows.get_mut(&window) {
                    win.input_region = rects;
                }
                self.content_input(window);
            }
            ToRender::SetOpaqueRegion { window, rects } => {
                let region = rects.and_then(|rects| self.region(&rects));
                let Some(win) = self.windows.get(&window) else { return };
                win.surface().set_opaque_region(region.as_ref().map(Region::wl_region));
                if win.configured {
                    win.commit();
                }
            }
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
        layer: Option<LayerPlacement>,
        lock: Option<u32>,
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
        let (shell, surface) = match (popup, sheet, layer, lock) {
            (_, Some(sheet), ..) => {
                let surface = sheet.surface().clone();
                (Shell::Sheet(sheet), surface)
            }
            (Some(placement), None, ..) => {
                let surface = self.compositor.create_surface(&self.qh);
                let Some(popup) = self.create_popup(&surface, width, height, placement) else {
                    surface.destroy();
                    return;
                };
                (Shell::Popup(popup), surface)
            }
            (None, None, Some(placement), _) => {
                let surface = self.compositor.create_surface(&self.qh);
                let Some(l) = layer::create(self, &surface, width, height, &placement) else {
                    // Without a layer shell it can't show: it is closed at once.
                    eprintln!("sidestep: the compositor has no wlr-layer-shell for layer surface {window}");
                    surface.destroy();
                    self.send(FromRender::CloseRequested { window });
                    return;
                };
                scale = layer::initial_scale(self, placement.output).unwrap_or(scale);
                (Shell::Layer(l), surface)
            }
            (None, None, None, Some(output)) => {
                let surface = self.compositor.create_surface(&self.qh);
                let Some(l) = lock::create(self, &surface, output) else {
                    // No lock asked for, no such output, or one there already.
                    eprintln!("sidestep: no session lock surface for window {window} on output {output}");
                    surface.destroy();
                    self.send(FromRender::CloseRequested { window });
                    return;
                };
                scale = layer::initial_scale(self, Some(output)).unwrap_or(scale);
                (Shell::Lock(l, output), surface)
            }
            (None, None, None, None) => {
                let decorations = if !style.titled || force_client_decorations() {
                    WindowDecorations::ClientOnly
                } else {
                    WindowDecorations::RequestServer
                };
                let frame = self.compositor.create_surface(&self.qh);
                let w = self.xdg.create_window(frame.clone(), decorations, &self.qh);
                w.set_title(title);
                w.set_app_id(app_id());
                if let Some(token) = self.startup_token.take()
                    && let Some(activation) = &self.activation
                {
                    activation.activate::<State>(&frame, token);
                }
                let (subsurface, content) = self.subcompositor.create_subsurface(frame.clone(), &self.qh);
                frame.set_input_region(Some(&self.empty_region()));
                let viewport = self.viewporter.get().expect("viewporter").get_viewport(&frame, &self.qh, ());
                self.roles.insert(frame.id(), Role::Frame(window));
                w.commit();
                let surface = content.clone();
                (
                    Shell::Toplevel(Frame { window: w, subsurface, content, viewport, pixel: None, placed: None }),
                    surface,
                )
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
                pending: Vec::new(),
                damage: Vec::new(),
                buffers: Vec::new(),
                transparent: false,
                front: None,
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
                raster_time: std::time::Duration::ZERO,
                raster_pixels: 0.0,
                input_region: None,
                popup_parent: popup.map_or(0, |p| p.parent),
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
    /// Where a popup `width` × `height` points goes against `parent`, as
    /// `placement` says.
    fn positioner(&self, parent: &Win, width: u32, height: u32, placement: PopupPlacement) -> Option<XdgPositioner> {
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
        Some(positioner)
    }

    fn create_popup(&self, surface: &WlSurface, width: u32, height: u32, placement: PopupPlacement) -> Option<Popup> {
        let parent = self.windows.get(&placement.parent)?;
        let positioner = self.positioner(parent, width, height, placement)?;
        let parent_xdg = match &parent.shell {
            Shell::Toplevel(f) => Some(f.window.xdg_surface().clone()),
            Shell::Popup(p) => Some(p.xdg_surface().clone()),
            // A layer surface's popup has no xdg parent: it is given to
            // the layer surface before its first commit.
            Shell::Layer(_) => None,
            // Popups of sheets aren't placed yet; a lock surface has none.
            Shell::Sheet(_) | Shell::Lock(..) => return None,
        };
        let popup = Popup::from_surface(parent_xdg.as_ref(), &positioner, &self.qh, surface.clone(), &self.xdg).ok()?;
        if let Shell::Layer(l) = &parent.shell {
            l.get_popup(popup.xdg_popup());
        }
        if placement.grab
            && let Some((seat, serial)) = self.grab_serial(placement.parent)
        {
            popup.xdg_popup().grab(&seat, serial);
        }
        popup.wl_surface().commit();
        Some(popup)
    }

    /// The seat and serial a popup of `parent` grabs with: the latest
    /// press's or keyboard entry's. With none (a shell's menu opened by a
    /// key binding that reached it over IPC, its panel never pressed nor
    /// given the keyboard), a popup of a layer surface grabs with serial
    /// 0, which Aizome's compositor and sway take. Other popups don't
    /// grab then, as before: a compositor that checks serials dismisses a
    /// popup whose grab it refuses.
    fn grab_serial(&self, parent: WindowId) -> Option<(WlSeat, u32)> {
        self.seats.latest_serial().or_else(|| {
            // Up the popups to the window they all come from.
            let mut id = parent;
            for _ in 0..64 {
                let win = self.windows.get(&id)?;
                match win.shell {
                    Shell::Popup(_) => id = win.popup_parent,
                    Shell::Layer(_) => return self.seats.first_seat().map(|seat| (seat, 0)),
                    _ => return None,
                }
            }
            None
        })
    }

    fn close_window(&mut self, window: WindowId) {
        let Some(win) = self.windows.remove(&window) else { return };
        outputs::window_closed(window);
        crate::stats::closed(window);
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
        if crate::trace::tracing() {
            eprintln!(
                "sidestep @{:.1} ms limits: window {window} min {:?} max {:?}",
                crate::trace::trace_ms(),
                limits.min,
                limits.max
            );
        }
        let Some(w) = win.toplevel() else { return };
        w.set_min_size((min != (0, 0)).then_some(limits.min));
        w.set_max_size((max != (0, 0)).then_some(limits.max));
    }

    fn request(&mut self, window: WindowId, request: WindowRequest) {
        if layer::request(self, window, request) {
            return;
        }
        if let WindowRequest::Reposition(placement, width, height) = request {
            self.reposition(window, placement, width, height);
            return;
        }
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
            // Activation is asked above; the rest, of layer surfaces only.
            WindowRequest::Activate
            | WindowRequest::Layer(_)
            | WindowRequest::LayerAnchors(_)
            | WindowRequest::LayerExclusiveZone(_)
            | WindowRequest::LayerMargins(_)
            | WindowRequest::LayerKeyboard(_)
            | WindowRequest::Reposition(..) => {}
        }
    }

    /// Place popup `window` anew, `width` × `height` points, as `placement`
    /// says: the compositor configures it where it goes. Only with
    /// xdg-shell 3 or later (xdg_popup.reposition); otherwise it stays.
    fn reposition(&mut self, window: WindowId, placement: PopupPlacement, width: u32, height: u32) {
        let Some(win) = self.windows.get(&window) else { return };
        let Shell::Popup(popup) = &win.shell else { return };
        let Some(parent) = self.windows.get(&win.popup_parent) else { return };
        if popup.xdg_popup().version() < 3 {
            return;
        }
        let Some(positioner) = self.positioner(parent, width, height, placement) else { return };
        static TOKEN: AtomicU32 = AtomicU32::new(1);
        popup.reposition(&positioner, TOKEN.fetch_add(1, Ordering::Relaxed));
        positioner.destroy();
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
                app_id: Some(app_id()),
                seat_and_serial: self.seats.latest_serial(),
                requester: Some(focused.unwrap_or(win).main_surface().clone()),
                target: Some(win.main_surface().clone()),
                request: None,
            },
        );
    }

    /// Ask for an activation token for another program (`app_id`'s, if
    /// known), as the result of the latest input, with the surface that has
    /// the keyboard, if one of ours has it: the main thread hears it under
    /// `request`, or that there's none.
    fn request_token(&mut self, request: u64, app_id: Option<String>) {
        let Some(activation) = &self.activation else {
            self.send(FromRender::ActivationToken { request, token: None });
            return;
        };
        let focused = self.seats.keyboard_window().and_then(|w| self.windows.get(&w));
        activation.request_token_with_data(
            &self.qh,
            Activation {
                app_id,
                seat_and_serial: self.seats.latest_serial(),
                requester: focused.map(|w| w.main_surface().clone()),
                target: None,
                request: Some(request),
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
            // What was painted at the old size goes with its buffers.
            win.pending.clear();
            win.buffers.clear();
            win.front = None;
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

    /// Paint `rects` of the window's own surface: drawn into a buffer at
    /// the next present (see `present`).
    fn paint(&mut self, window: WindowId, rects: Vec<Rect>, ops: Vec<Op>) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        if !win.configured {
            return;
        }
        let scale = win.scale;
        win.damage.extend(rects.iter().map(|r| to_px(r, scale)));
        win.pending.push(Pending::Paint(rects, ops));
    }

    /// Move `rect` (points) of the window's surface by `dx`, `dy` points, at
    /// the next present: all of it changes.
    fn scroll(&mut self, window: WindowId, rect: Rect, dx: f32, dy: f32) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        if !win.configured {
            return;
        }
        let s = win.scale as f32;
        let bounds = Rect::new(0.0, 0.0, win.px_width as f32, win.px_height as f32);
        let rect = to_px(&rect, win.scale).intersect(&bounds);
        if rect.is_empty() {
            return;
        }
        win.damage.push(rect);
        win.pending.push(Pending::Scroll { rect, dx: (dx * s).round() as i32, dy: (dy * s).round() as i32 });
    }

    /// A frame the main thread sent was presented, taking `took`, of
    /// which what rasterizing took past `before` was rasterizing: to
    /// `stats`, with what its paints took to rasterize.
    fn frame_measured(&mut self, window: WindowId, took: std::time::Duration, before: std::time::Duration) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let took = took.saturating_sub(win.raster_time.saturating_sub(before));
        let raster = std::mem::take(&mut win.raster_time);
        let pixels = std::mem::take(&mut win.raster_pixels);
        let all = f64::from(win.px_width) * f64::from(win.px_height);
        let damage = if all > 0.0 { (pixels / all) as f32 } else { 0.0 };
        crate::stats::frame(window, raster, took, damage);
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
        if crate::trace::tracing() && count > 0 {
            eprintln!(
                "sidestep @{:.1} ms ca frame: window {window} render: {:.3} ms, {count} redraws, {area:.0} points squared",
                crate::trace::trace_ms(),
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
        win.surface().frame(&self.qh, FrameTag { window, seq });
        win.commit();
    }

    /// Whether buffers take canvas pixels as they are. Canvases are RGBA
    /// in memory, which wl_shm calls XBGR8888: shown as they are where the
    /// compositor takes it, else swizzled to XRGB8888. The formats arrive
    /// as events after wl_shm is bound, so this is decided when the first
    /// buffer is made (a window is configured by then, so they're in), and
    /// kept: every buffer has the same format. `SIDESTEP_SHM_FORMAT=xrgb8888`
    /// takes XRGB8888 regardless, to try the swizzling.
    fn rgba(&mut self) -> bool {
        let shm = &self.shm;
        *self.rgba.get_or_insert_with(|| {
            let xrgb = std::env::var("SIDESTEP_SHM_FORMAT").is_ok_and(|f| f.eq_ignore_ascii_case("xrgb8888"));
            !xrgb && shm.formats().contains(&wl_shm::Format::Xbgr8888)
        })
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
        if crate::trace::tracing() {
            tiles::trace(window, &layers, &win.damage);
        }
        if !win.damage.is_empty() {
            self.draw_frame(window, rgba);
        }
        self.place_decorations(window);
        let Some(win) = self.windows.get(&window) else { return };
        win.surface().frame(&self.qh, FrameTag { window, seq });
        win.commit();
    }

    /// Draw the paints since the last present straight into a buffer, and
    /// attach it with their damage. The buffer is the one presented last
    /// if the compositor is done with it (as compositors that copy shared
    /// memory into textures are at once), else another it's done with,
    /// else a new one; one other than the last must first catch up with
    /// it where it missed frames (each buffer keeps where it does), except
    /// where the paints replace what's there outright before drawing over
    /// it (a canvas clears what it redraws first). Without XBGR8888 the
    /// buffers hold XRGB8888, and the damage is turned to canvas pixels for
    /// drawing and back.
    fn draw_frame(&mut self, window: WindowId, rgba: bool) {
        let State { windows, pool, glyphs, .. } = self;
        let Some(win) = windows.get_mut(&window) else { return };
        let format = window_format(rgba, win.transparent);
        let damage = std::mem::take(&mut win.damage);
        let pending = std::mem::take(&mut win.pending);
        let (width, height, scale) = (win.px_width, win.px_height, win.scale);
        let free = |b: &RootBuffer, pool: &mut SlotPool| b.buffer.canvas(pool).is_some();
        let front = win.front.filter(|&f| free(&win.buffers[f], pool));
        let index = match front.or_else(|| win.buffers.iter().position(|b| free(b, pool))) {
            Some(i) => i,
            None => {
                let (buffer, _) = pool
                    .create_buffer(width as i32, height as i32, width as i32 * 4, format)
                    .expect("sidestep: window buffer");
                let full = Rect::new(0.0, 0.0, width as f32, height as f32);
                win.buffers.push(RootBuffer { buffer, stale: vec![full] });
                win.buffers.len() - 1
            }
        };
        let mut stale: Vec<&mut Vec<Rect>> = win.buffers.iter_mut().map(|b| &mut b.stale).collect();
        let stale = tiles::pass_on(&mut stale, index, &damage);
        let replaced = match pending.first() {
            Some(Pending::Paint(rects, ops)) => replaced(rects, ops, width, height, scale),
            _ => Vec::new(),
        };
        let mut repair: Vec<Rect> =
            stale.into_iter().filter(|s| !replaced.iter().any(|r| r.intersect(s) == *s)).collect();
        // A frame that starts by moving pixels, drawn into a buffer behind
        // the last, takes what it moves from the last, moving it.
        let mut moved_first = false;
        match win.front {
            Some(last) if last != index => {
                let (from, to) = (&win.buffers[last].buffer, &win.buffers[index].buffer);
                if let Some(&Pending::Scroll { rect, dx, dy }) = pending.first() {
                    let dest = moved(&rect, dx, dy);
                    repair = repair.iter().flat_map(|r| subtract(r, &dest)).collect();
                    copy_between(pool, from, to, (width, height), &repair, 0, 0);
                    copy_between(pool, from, to, (width, height), &[dest], dx, dy);
                    moved_first = true;
                } else {
                    copy_between(pool, from, to, (width, height), &repair, 0, 0);
                }
            }
            Some(_) => {}
            // The first frame at this size: what it doesn't draw is the
            // background.
            None => {
                let px = as_pixels(win.buffers[index].buffer.canvas(pool).expect("a free buffer"));
                let ground = if win.transparent {
                    0
                } else if rgba {
                    BACKGROUND
                } else {
                    raster::to_xrgb(BACKGROUND)
                };
                for r in &repair {
                    fill_rows(px, width, height, r, ground);
                }
            }
        }
        let started = crate::stats::on().then(std::time::Instant::now);
        let px = as_pixels(win.buffers[index].buffer.canvas(pool).expect("a free buffer"));
        // Each pixel turned once, however the damage overlaps.
        let turned = if rgba { Vec::new() } else { disjoint(&damage) };
        swizzle(px, width, height, &turned);
        let px = &mut px[..(width * height) as usize];
        for (i, item) in pending.iter().enumerate() {
            match item {
                Pending::Paint(rects, ops) => {
                    let mut canvas = Canvas::new(px, width, height, 0.0, scale as f32);
                    if crate::trace::tracing_tiles() {
                        raster::paint_timed(&mut canvas, glyphs, rects, ops, window);
                    } else {
                        // A large paint on several threads.
                        raster::bands::paint_banded(&mut canvas, glyphs, rects, ops);
                    }
                }
                Pending::Scroll { .. } if i == 0 && moved_first => {}
                &Pending::Scroll { rect, dx, dy } => scroll_in(px, width, height, &rect, dx, dy),
            }
        }
        swizzle(px, width, height, &turned);
        if let Some(started) = started {
            win.raster_time += started.elapsed();
            let area = |r: &Rect| f64::from((r.x1 - r.x0).max(0.0)) * f64::from((r.y1 - r.y0).max(0.0));
            let bounds = Rect::new(0.0, 0.0, width as f32, height as f32);
            win.raster_pixels += damage.iter().map(|r| area(&r.intersect(&bounds))).sum::<f64>();
        }
        win.front = Some(index);
        let surface = win.surface().clone();
        win.buffers[index].buffer.attach_to(&surface).expect("sidestep: attach window buffer");
        for r in &damage {
            let r = r.round_out();
            surface.damage_buffer(r.x0 as i32, r.y0 as i32, (r.x1 - r.x0) as i32, (r.y1 - r.y0) as i32);
        }
        win.viewport.set_destination(win.width as i32, win.height as i32);
    }

    /// Set the window geometry, size the frame and place the content in it,
    /// and draw and place the decorations for the window's current size,
    /// ahead of the window's commit.
    fn place_decorations(&mut self, window: WindowId) {
        let rgba = self.rgba();
        let Some(win) = self.windows.get_mut(&window) else { return };
        let geometry = win.expected_geometry();
        if win.geometry != geometry {
            win.geometry = geometry;
            match &win.shell {
                Shell::Toplevel(f) => {
                    f.window.xdg_surface().set_window_geometry(geometry.0, geometry.1, geometry.2, geometry.3)
                }
                Shell::Popup(p) => p.xdg_surface().set_window_geometry(0, 0, geometry.2, geometry.3),
                Shell::Sheet(_) | Shell::Layer(_) | Shell::Lock(..) => {}
            }
        }
        let (width, height, top) = (win.width, win.height, win.titlebar());
        let moved = match &mut win.shell {
            Shell::Toplevel(f) => f.place(width, height, top, &mut self.pool),
            Shell::Popup(_) | Shell::Sheet(_) | Shell::Layer(_) | Shell::Lock(..) => false,
        };
        let bar = win.bar_height();
        if let Some(menubar) = &mut win.menubar {
            menubar.draw(win.width, bar, top as i32, win.scale, rgba, &mut self.pool, &mut self.glyphs);
        }
        if let Some(mut decor) = win.decor.take() {
            decor.draw(&frame_info(win), &mut self.pool, &mut self.glyphs);
            win.decor = Some(decor);
        }
        if moved {
            // Scroll tiles, sheets and carets are placed in the frame, under
            // the title bar.
            tiles::content_moved(self, window, top as i32);
            sheet::parent_resized(self, window);
            textinput::content_moved(self, window);
        }
    }

    /// Draw decorations that changed (hover, focus, style) between presents,
    /// and show them. Only for changes no present brings: after a resize
    /// the present does it, with the content drawn for the new size.
    fn refresh_decor(&mut self, window: WindowId) {
        let Some(win) = self.windows.get(&window) else { return };
        if !win.configured {
            return;
        }
        let redraw = win.decor.as_ref().is_some_and(|d| d.needs_draw(&frame_info(win)))
            || win.menubar.as_ref().is_some_and(menubar::Bar::needs_draw);
        if !redraw && win.geometry == win.expected_geometry() {
            return;
        }
        self.place_decorations(window);
        // Decorations are synchronized subsurfaces: what they show changes
        // with the window's next commit.
        if let Some(win) = self.windows.get(&window) {
            win.commit();
        }
    }

    /// Create or remove decorations after the decoration mode, style or
    /// state changed.
    fn update_decorations(&mut self, window: WindowId) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let wanted = win.toplevel().is_some() && win.style.titled && (win.client_side || force_client_decorations());
        if wanted && win.decor.is_none() {
            let mut d = decor::Decor::new(
                win.main_surface(),
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
        let empty = wanted.then(|| self.empty_region());
        let Some(win) = self.windows.get_mut(&window) else { return };
        win.passthrough = empty.is_some();
        if let Some(d) = &mut win.decor {
            d.set_passthrough(empty.as_ref());
        }
        // The window's commit applies the content's region and, as the
        // others are synchronized subsurfaces, theirs. A frame never takes
        // pointer input.
        self.content_input(window);
    }

    /// Give the content's surface its input region: none if pointer input
    /// goes through, else the one the main thread set, or all of it, and
    /// commit it.
    fn content_input(&mut self, window: WindowId) {
        let Some(win) = self.windows.get(&window) else { return };
        let set = win.input_region.clone();
        let empty = win.passthrough.then(|| self.empty_region());
        let region = if empty.is_some() { None } else { set.and_then(|rects| self.region(&rects)) };
        let Some(win) = self.windows.get(&window) else { return };
        win.surface().set_input_region(empty.as_ref().or(region.as_ref().map(Region::wl_region)));
        if win.configured {
            win.commit();
        }
    }

    /// A region of `rects` (points of a surface), for the moment it's set:
    /// a surface copies it.
    fn region(&self, rects: &[Rect]) -> Option<Region> {
        let region = Region::new(&self.compositor).ok()?;
        for r in rects.iter().map(Rect::round_out).filter(|r| !r.is_empty()) {
            region.add(r.x0 as i32, r.y0 as i32, (r.x1 - r.x0) as i32, (r.y1 - r.y0) as i32);
        }
        Some(region)
    }

    /// The input region with nothing in it, made the first time it's
    /// wanted.
    fn empty_region(&mut self) -> WlRegion {
        let compositor = &self.compositor;
        let region = self.empty_region.get_or_insert_with(|| Region::new(compositor).expect("sidestep: wl_region"));
        region.wl_region().clone()
    }

    fn role(&self, surface: &WlSurface) -> Option<Role> {
        self.roles.get(&surface.id()).copied()
    }

    /// Where a surface's origin is in its window's content, in points: the
    /// content's surface takes the content's input (tiles take none), the
    /// menu bar sits above it, and a frame holds it under the title bar.
    fn content_offset(&self, role: Role) -> (f64, f64) {
        let above = |window, height: fn(&Win) -> u32| -(self.windows.get(&window).map_or(0, height) as f64);
        match role {
            Role::Bar(window) => (0.0, above(window, Win::bar_height)),
            Role::Frame(window) => (0.0, above(window, Win::titlebar)),
            Role::Root(_) | Role::Decor(..) => (0.0, 0.0),
        }
    }
}

/// The application id a toolkit set ([`set_app_id`]).
static APP_ID: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Name the application to the compositor from now on: the app id its
/// toplevels are created with (xdg_toplevel's, which compositors match
/// windows to their programs' icons and desktop entries by) and that its
/// activation tokens are asked for with. `None`, the default, is the
/// program's name.
pub fn set_app_id(id: Option<String>) {
    *APP_ID.write().unwrap_or_else(|e| e.into_inner()) = id.filter(|id| !id.is_empty());
}

/// The application id compositors group windows by: what was set, else
/// the program's name.
fn app_id() -> String {
    if let Some(id) = APP_ID.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return id;
    }
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
        if let Some(Role::Frame(id)) = self.role(window.wl_surface()) {
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
        let Some(Role::Frame(id)) = self.role(window.wl_surface()) else { return };
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
        // the content. A window that can't be resized keeps its own size,
        // as xdg-shell lets a client outside full screen and maximized:
        // a tiling compositor offers its tile before the window maps, then
        // floats it at its own size, as its fixed limits ask. Taking the
        // tile would make that the window's fixed size.
        let bar = win.titlebar();
        let keep = !win.style.resizable && !win.state.fullscreen && !win.state.maximized;
        let size = if keep { (None, None) } else { configure.new_size };
        let width = size.0.map_or(win.width, |w| w.get());
        let height = size.1.map_or(win.height, |h| h.get().saturating_sub(bar));
        let before = win.configured.then_some((win.width, win.height));
        if crate::trace::tracing() {
            eprintln!(
                "sidestep @{:.1} ms configure: window {id} offered {:?} {:?}, took {width}x{height} under a {bar} point title bar",
                crate::trace::trace_ms(),
                configure.new_size,
                configure.state
            );
        }
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
        (layout.resize_x, ConstraintAdjustment::ResizeX),
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
/// token activates, or the main thread's request it answers.
pub struct Activation {
    app_id: Option<String>,
    seat_and_serial: Option<(WlSeat, u32)>,
    requester: Option<WlSurface>,
    target: Option<WlSurface>,
    request: Option<u64>,
}

impl RequestDataExt for Activation {
    fn app_id(&self) -> Option<&str> {
        self.app_id.as_deref()
    }

    fn seat_and_serial(&self) -> Option<(&WlSeat, u32)> {
        self.seat_and_serial.as_ref().map(|(seat, serial)| (seat, *serial))
    }

    fn surface(&self) -> Option<&WlSurface> {
        self.requester.as_ref()
    }
}

impl ActivationHandler for State {
    type RequestData = Activation;

    fn new_token(&mut self, token: String, data: &Activation) {
        if let Some(request) = data.request {
            self.send(FromRender::ActivationToken { request, token: Some(token) });
        } else if let (Some(activation), Some(target)) = (&self.activation, &data.target) {
            activation.activate::<State>(target, token);
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
                let time = crate::ca::math::media_now();
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
delegate_layer!(State);
delegate_session_lock!(State);
delegate_activation!(State, Activation);
delegate_simple!(State, WpViewporter, 1);
delegate_registry!(State);
