//! The render thread. It owns the Wayland connection and each window's
//! surfaces:
//!
//! - the root layer is the window's own surface, rasterized into a canvas and
//!   presented through a few shared-memory buffers, copying and damaging only
//!   what changed;
//! - a scroll layer is a stack of tiles, each on its own subsurface, cropped
//!   to the scroll view with the viewporter. Scrolling moves tiles; a tile is
//!   uploaded again only when its content changes;
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
//! keymaps ([`keyboard`]) and cursors. [`selection`] carries the clipboard.
//! All of it reaches the main thread as [`FromRender`] messages.
//!
//! Frames are paced by the compositor's frame callbacks, which are passed on
//! to the main thread as permission to send the next frame. Each present asks
//! for a callback on every surface it shows, and the first to fire counts:
//! compositors send none to a surface they consider hidden, and tiles can
//! cover a window's own surface completely.

mod decor;
mod keyboard;
mod seat;
mod selection;

pub(crate) use decor::{HEADER, TITLE_SIZE};

/// Milliseconds a run of `f` takes, the median of seven, for the timing
/// tests (`cargo test --release -p sidestep-appkit timing -- --ignored
/// --nocapture`).
#[cfg(test)]
fn median(mut f: impl FnMut()) -> f64 {
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

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;

use smithay_client_toolkit::activation::{ActivationHandler, ActivationState, RequestData};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::channel::{self, Channel, Event as ChannelEvent};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
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

use crate::protocol::{
    Cursor, FromRender, LayerId, Op, PopupPlacement, ROOT_LAYER, Rect, SizeLimits, Style, TILE_HEIGHT, TitleText,
    ToRender, WindowId, WindowRequest, WindowState,
};
use crate::raster::{self, Canvas, Glyphs};

/// The main thread's end of the render thread.
pub(crate) struct Backend {
    pub tx: channel::Sender<ToRender>,
    pub rx: mpsc::Receiver<FromRender>,
}

pub(crate) fn start() -> Backend {
    let (tx, channel) = channel::channel();
    let (to_main, rx) = mpsc::channel();
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
fn force_client_decorations() -> bool {
    static FORCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCE.get_or_init(|| std::env::var("SIDESTEP_DECORATIONS").is_ok_and(|v| v == "client"))
}

fn run(channel: Channel<ToRender>, to_main: mpsc::Sender<FromRender>) {
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
    let fractional = globals.bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ()).ok();
    let mut state = State {
        registry: RegistryState::new(&globals),
        outputs: OutputState::new(&globals, &qh),
        seats: seat::Seats::new(&globals, &qh),
        selection: selection::Selection::new(&globals, &qh),
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
        to_main,
        // The environment isn't changed: other threads may be reading it.
        startup_token: std::env::var("XDG_ACTIVATION_TOKEN").ok().filter(|t| !t.is_empty()),
        exit: false,
    };
    seat::bind_existing(&mut state, &globals);
    while !state.exit {
        if event_loop.dispatch(None, &mut state).is_err() {
            break;
        }
    }
}

pub(crate) struct State {
    registry: RegistryState,
    outputs: OutputState,
    seats: seat::Seats,
    selection: selection::Selection,
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
    to_main: mpsc::Sender<FromRender>,
    /// The activation token we were started with, for the first window.
    startup_token: Option<String>,
    exit: bool,
}

/// What a surface is to input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Role {
    /// A window's own surface.
    Root(WindowId),
    /// A tile of a scroll layer.
    Tile(WindowId, LayerId, u32),
    /// Part of the decorations.
    Decor(WindowId, decor::Part),
}

impl Role {
    fn window(self) -> WindowId {
        match self {
            Role::Root(w) | Role::Tile(w, _, _) | Role::Decor(w, _) => w,
        }
    }
}

enum Shell {
    Toplevel(Window),
    Popup(Popup),
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
    /// Damage in pixels.
    damage: Vec<Rect>,
    buffers: Vec<RootBuffer>,
    layers: HashMap<LayerId, ScrollLayer>,
    /// Counts presents; a frame callback names the present it belongs to.
    frame_seq: u64,
    frame_signalled: bool,
    style: Style,
    limits: SizeLimits,
    state: WindowState,
    title: TitleText,
    /// The compositor asked us to draw decorations.
    client_side: bool,
    decor: Option<decor::Decor>,
    cursor: Cursor,
    /// The window geometry last set, to set it again only on changes.
    geometry: (i32, i32, i32, i32),
    /// What the main thread was last told, to tell it only changes.
    reported: Option<(u32, u32, u64, u32, WindowState)>,
    /// The size limits last given the compositor, title bar included.
    sent_limits: Option<SizeLimits>,
}

impl Win {
    fn surface(&self) -> &WlSurface {
        match &self.shell {
            Shell::Toplevel(w) => w.wl_surface(),
            Shell::Popup(p) => p.wl_surface(),
        }
    }

    fn toplevel(&self) -> Option<&Window> {
        match &self.shell {
            Shell::Toplevel(w) => Some(w),
            Shell::Popup(_) => None,
        }
    }

    /// Height of the title bar drawn above the content, in points.
    fn titlebar(&self) -> u32 {
        self.decor.as_ref().map_or(0, |d| d.titlebar(&self.state))
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
    /// Areas (pixels) that changed since this buffer was last written.
    stale: Vec<Rect>,
}

struct ScrollLayer {
    viewport: Rect,
    offset: f32,
    doc_width: u32,
    tiles: BTreeMap<u32, Tile>,
}

struct Tile {
    canvas: Vec<u32>,
    /// Canvas size in pixels.
    px_width: u32,
    px_height: u32,
    dirty: bool,
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    buffer: Option<Buffer>,
    mapped: bool,
    /// The window area this tile covered at the last present.
    placed: Rect,
}

impl Drop for Tile {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

const BACKGROUND: u32 = 0x00ececec;

fn copy_rows(dst: &mut [u32], src: &[u32], width: u32, height: u32, r: &Rect) {
    let r = r.round_out();
    let x0 = r.x0.max(0.0) as usize;
    let y0 = r.y0.max(0.0) as usize;
    let x1 = r.x1.min(width as f32).max(0.0) as usize;
    let y1 = r.y1.min(height as f32).max(0.0) as usize;
    let w = width as usize;
    for y in y0..y1.max(y0) {
        if x0 < x1 {
            dst[y * w + x0..y * w + x1].copy_from_slice(&src[y * w + x0..y * w + x1]);
        }
    }
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

/// Tiles as `Role`s, to forget when they go.
fn forget_tiles(roles: &mut HashMap<ObjectId, Role>, tiles: impl IntoIterator<Item = Tile>) {
    for tile in tiles {
        roles.remove(&tile.surface.id());
    }
}

impl State {
    fn send(&self, msg: FromRender) {
        let _ = self.to_main.send(msg);
    }

    fn handle(&mut self, msg: ToRender) {
        match msg {
            ToRender::CreateWindow { window, width, height, title, style, limits, popup } => {
                self.create_window(window, width, height, title, style, limits, popup)
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
                self.refresh_decor(window);
            }
            ToRender::SetStyle { window, style } => {
                let Some(win) = self.windows.get_mut(&window) else { return };
                win.style = style;
                self.apply_limits(window);
                self.update_decorations(window);
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
            ToRender::Paint { window, layer, rects, ops } => self.paint(window, layer, rects, ops),
            ToRender::ScrollLayer { window, layer, viewport, offset, doc_width } => {
                let Some(win) = self.windows.get_mut(&window) else { return };
                let scroll = win.layers.entry(layer).or_insert_with(|| ScrollLayer {
                    viewport,
                    offset,
                    doc_width,
                    tiles: BTreeMap::new(),
                });
                if scroll.doc_width != doc_width {
                    forget_tiles(&mut self.roles, std::mem::take(&mut scroll.tiles).into_values());
                    scroll.doc_width = doc_width;
                }
                scroll.viewport = viewport;
                scroll.offset = offset;
            }
            ToRender::DropTiles { window, layer, tiles } => {
                if let Some(scroll) = self.windows.get_mut(&window).and_then(|w| w.layers.get_mut(&layer)) {
                    forget_tiles(&mut self.roles, tiles.iter().filter_map(|i| scroll.tiles.remove(i)));
                }
            }
            ToRender::Present { window } => self.present(window),
            ToRender::CloseWindow { window } => self.close_window(window),
            ToRender::SetSelection { contents } => selection::set(self, contents),
            ToRender::ReadSelection { mime, token } => selection::read(self, mime, token),
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
    ) {
        let surface = self.compositor.create_surface(&self.qh);
        let shell = match popup {
            Some(placement) => {
                let Some(popup) = self.create_popup(&surface, width, height, placement) else {
                    surface.destroy();
                    return;
                };
                Shell::Popup(popup)
            }
            None => {
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
                scale: 1.0,
                px_width: 0,
                px_height: 0,
                configured: false,
                canvas: Vec::new(),
                damage: Vec::new(),
                buffers: Vec::new(),
                layers: HashMap::new(),
                frame_seq: 0,
                frame_signalled: false,
                style,
                limits,
                state: WindowState::default(),
                title: TitleText::default(),
                client_side: false,
                decor: None,
                cursor: Cursor::Default,
                geometry: (0, 0, 0, 0),
                reported: None,
                sent_limits: None,
            },
        );
        self.apply_limits(window);
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
        positioner.set_anchor(if placement.below { Anchor::BottomLeft } else { Anchor::TopLeft });
        positioner.set_gravity(Gravity::BottomRight);
        positioner.set_size(width.max(1) as i32, height.max(1) as i32);
        positioner.set_constraint_adjustment(
            ConstraintAdjustment::FlipY | ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY,
        );
        let parent_xdg = match &parent.shell {
            Shell::Toplevel(w) => w.xdg_surface().clone(),
            Shell::Popup(p) => p.xdg_surface().clone(),
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
        self.roles.retain(|_, role| role.window() != window);
        seat::window_closed(self, window);
        if let Some(f) = &win.fractional {
            f.destroy();
        }
        win.viewport.destroy();
        // Tiles and decorations go first: they're subsurfaces of the root.
        drop(win.layers);
        drop(win.decor);
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
        let Some(win) = self.windows.get_mut(&window) else { return };
        let Some(w) = win.toplevel() else { return };
        match request {
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
        let (Some(activation), Some(win)) = (&self.activation, self.windows.get(&window)) else { return };
        activation.request_token_with_data(
            &self.qh,
            RequestData {
                app_id: Some(app_id()),
                seat_and_serial: self.seats.latest_serial(),
                surface: Some(win.surface().clone()),
            },
        );
    }

    /// Size the window's canvas for its size and scale, as a configure or a
    /// scale change asks, and tell the main thread.
    fn resize(&mut self, window: WindowId, width: u32, height: u32) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let (width, height) = (width.max(1), height.max(1));
        let (pw, ph) = (px(width, win.scale), px(height, win.scale));
        if !win.configured || pw != win.px_width || ph != win.px_height || width != win.width || height != win.height {
            win.configured = true;
            win.width = width;
            win.height = height;
            win.px_width = pw;
            win.px_height = ph;
            win.canvas = vec![BACKGROUND; (pw * ph) as usize];
            win.buffers.clear();
            win.damage.clear();
        }
        self.report(window);
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

    fn set_scale(&mut self, window: WindowId, scale: f64) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        if (win.scale - scale).abs() < 1e-9 || scale <= 0.0 {
            return;
        }
        win.scale = scale;
        // Tiles were drawn at the old scale; the main thread draws them again.
        for layer in win.layers.values_mut() {
            forget_tiles(&mut self.roles, std::mem::take(&mut layer.tiles).into_values());
        }
        if let Some(d) = &mut win.decor {
            d.scale_changed();
        }
        seat::scale_changed(self, window);
        let Some(win) = self.windows.get(&window) else { return };
        if win.configured {
            let (w, h) = (win.width, win.height);
            self.resize(window, w, h);
            self.refresh_decor(window);
        }
    }

    fn paint(&mut self, window: WindowId, layer: LayerId, rects: Vec<Rect>, ops: Vec<Op>) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        let scale = win.scale;
        if layer == ROOT_LAYER {
            if !win.configured {
                return;
            }
            let mut canvas = Canvas {
                px: &mut win.canvas,
                width: win.px_width,
                height: win.px_height,
                origin_y: 0.0,
                scale: scale as f32,
            };
            raster::paint(&mut canvas, &mut self.glyphs, &rects, &ops);
            win.damage.extend(rects.iter().map(|r| to_px(r, scale)));
            return;
        }
        let parent = win.surface().clone();
        let Some(scroll) = win.layers.get_mut(&layer) else { return };
        let (tile_w, tile_h) = (px(scroll.doc_width, scale), px(TILE_HEIGHT, scale));
        for rect in &rects {
            let first = (rect.y0.max(0.0) as u32) / TILE_HEIGHT;
            let last = ((rect.y1 - 1.0).max(0.0) as u32) / TILE_HEIGHT;
            for index in first..=last {
                let tile = scroll.tiles.entry(index).or_insert_with(|| {
                    let (subsurface, surface) = self.subcompositor.create_subsurface(parent.clone(), &self.qh);
                    let viewport = self.viewporter.get().expect("viewporter").get_viewport(&surface, &self.qh, ());
                    self.roles.insert(surface.id(), Role::Tile(window, layer, index));
                    Tile {
                        canvas: vec![BACKGROUND; (tile_w * tile_h) as usize],
                        px_width: tile_w,
                        px_height: tile_h,
                        dirty: true,
                        surface,
                        subsurface,
                        viewport,
                        buffer: None,
                        mapped: false,
                        placed: Rect::default(),
                    }
                });
                let mut canvas = Canvas {
                    px: &mut tile.canvas,
                    width: tile.px_width,
                    height: tile.px_height,
                    origin_y: (index * TILE_HEIGHT) as f32,
                    scale: scale as f32,
                };
                raster::paint(&mut canvas, &mut self.glyphs, std::slice::from_ref(rect), &ops);
                tile.dirty = true;
            }
        }
    }

    fn present(&mut self, window: WindowId) {
        let Some(win) = self.windows.get_mut(&window) else { return };
        if !win.configured {
            return;
        }
        win.frame_seq += 1;
        win.frame_signalled = false;
        let seq = win.frame_seq;
        let s = win.scale;
        for scroll in win.layers.values_mut() {
            let vp = scroll.viewport;
            let width = scroll.doc_width;
            for (&index, tile) in scroll.tiles.iter_mut() {
                let top = vp.y0 + (index * TILE_HEIGHT) as f32 - scroll.offset;
                let shown =
                    Rect::new(vp.x0, top, vp.x0 + width as f32, top + TILE_HEIGHT as f32).intersect(&vp).round_out();
                if shown.is_empty() {
                    if tile.mapped {
                        tile.surface.attach(None, 0, 0);
                        tile.surface.commit();
                        tile.mapped = false;
                    }
                    continue;
                }
                if tile.dirty || !tile.mapped {
                    let (w, h) = (tile.px_width, tile.px_height);
                    let (buffer, bytes) = self
                        .pool
                        .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Xrgb8888)
                        .expect("sidestep: tile buffer");
                    as_pixels(bytes).copy_from_slice(&tile.canvas);
                    buffer.attach_to(&tile.surface).expect("sidestep: attach tile");
                    tile.surface.damage_buffer(0, 0, w as i32, h as i32);
                    tile.buffer = Some(buffer);
                    tile.dirty = false;
                    tile.mapped = true;
                }
                let (w, h) = (shown.x1 - shown.x0, shown.y1 - shown.y0);
                tile.subsurface.set_position(shown.x0 as i32, shown.y0 as i32);
                // The source counts buffer pixels, the destination points.
                let src_w = (w as f64 * s).min(tile.px_width as f64);
                let src_h = (h as f64 * s).min(tile.px_height as f64);
                tile.viewport.set_source((shown.x0 - vp.x0) as f64 * s, (shown.y0 - top) as f64 * s, src_w, src_h);
                tile.viewport.set_destination(w as i32, h as i32);
                tile.surface.frame(&self.qh, FrameTag { window, seq });
                tile.surface.commit();
                tile.placed = shown;
            }
        }

        if !win.damage.is_empty() {
            let damage = std::mem::take(&mut win.damage);
            let free = win.buffers.iter().position(|b| b.buffer.canvas(&mut self.pool).is_some());
            let index = match free {
                Some(i) => i,
                None => {
                    let (buffer, _) = self
                        .pool
                        .create_buffer(
                            win.px_width as i32,
                            win.px_height as i32,
                            win.px_width as i32 * 4,
                            wl_shm::Format::Xrgb8888,
                        )
                        .expect("sidestep: window buffer");
                    let full = Rect::new(0.0, 0.0, win.px_width as f32, win.px_height as f32);
                    win.buffers.push(RootBuffer { buffer, stale: vec![full] });
                    win.buffers.len() - 1
                }
            };
            for (i, b) in win.buffers.iter_mut().enumerate() {
                if i != index {
                    b.stale.extend_from_slice(&damage);
                }
            }
            let surface = win.surface().clone();
            let target = &mut win.buffers[index];
            let stale = std::mem::take(&mut target.stale);
            let pixels = as_pixels(target.buffer.canvas(&mut self.pool).expect("free buffer"));
            for r in stale.iter().chain(&damage) {
                copy_rows(pixels, &win.canvas, win.px_width, win.px_height, r);
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
            }
        }
        let Some(decor) = &mut win.decor else { return };
        let frame = decor::FrameInfo {
            width: win.width,
            height: win.height,
            scale: win.scale,
            state: win.state,
            style: win.style,
            title: &win.title,
        };
        decor.draw(&frame, &mut self.pool, &mut self.glyphs);
    }

    /// Draw decorations that changed (hover, title, focus) between presents,
    /// and show them.
    fn refresh_decor(&mut self, window: WindowId) {
        let Some(win) = self.windows.get(&window) else { return };
        if !win.configured || win.decor.as_ref().is_none_or(|d| !d.needs_draw()) {
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
            let d = decor::Decor::new(
                win.surface(),
                &self.compositor,
                &self.subcompositor,
                self.viewporter.get().expect("viewporter"),
                &self.qh,
            );
            for (id, part) in d.surfaces() {
                self.roles.insert(id, Role::Decor(window, part));
            }
            win.decor = Some(d);
        } else if !wanted && let Some(d) = win.decor.take() {
            for (id, _) in d.surfaces() {
                self.roles.remove(&id);
            }
        }
        if let Some(d) = &mut win.decor {
            d.state_changed();
        }
    }

    fn role(&self, surface: &WlSurface) -> Option<Role> {
        self.roles.get(&surface.id()).copied()
    }

    /// Where a content surface's origin is in its window, in points.
    fn content_offset(&self, role: Role) -> (f64, f64) {
        match role {
            Role::Tile(window, layer, index) => self
                .windows
                .get(&window)
                .and_then(|w| w.layers.get(&layer))
                .and_then(|l| l.tiles.get(&index))
                .map_or((0.0, 0.0), |t| (t.placed.x0 as f64, t.placed.y0 as f64)),
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
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
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
        };
        win.client_side = configure.decoration_mode == DecorationMode::Client;
        if !keyboard && win.state.activated != was_active {
            // No keyboard, so no keyboard focus: activation stands for it.
            let focused = win.state.activated;
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
        self.apply_limits(id);
        self.resize(id, width, height);
        self.refresh_decor(id);
    }
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

impl ActivationHandler for State {
    type RequestData = RequestData;

    fn new_token(&mut self, token: String, data: &RequestData) {
        if let (Some(activation), Some(surface)) = (&self.activation, &data.surface) {
            activation.activate::<State>(surface, token);
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
            let _ = state.to_main.send(FromRender::Frame { window: tag.window });
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
delegate_activation!(State);
delegate_simple!(State, WpViewporter, 1);
delegate_registry!(State);
