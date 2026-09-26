//! The render thread. It owns the Wayland connection and each window's
//! layers:
//!
//! - the root layer is the window's own surface, rasterized into a canvas and
//!   presented through a few shared-memory buffers, copying and damaging only
//!   what changed;
//! - a scroll layer is a stack of tiles, each on its own subsurface, cropped
//!   to the scroll view with the viewporter. Scrolling moves tiles; a tile is
//!   uploaded again only when its content changes.
//!
//! Frames are paced by the compositor's frame callbacks, which are passed on
//! to the main thread as permission to send the next frame. Each present asks
//! for a callback on every surface it shows, and the first to fire counts:
//! compositors send none to a surface they consider hidden, and tiles can
//! cover a window's own surface completely.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop::channel::{self, Channel, Event as ChannelEvent};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::{self, WpViewport};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState, SimpleGlobal};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    delegate_simple, delegate_subcompositor, delegate_xdg_shell, delegate_xdg_window, registry_handlers,
};

use crate::protocol::{FromRender, LayerId, ROOT_LAYER, Rect, TILE_HEIGHT, ToRender, WindowId};
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

fn run(channel: Channel<ToRender>, to_main: mpsc::Sender<FromRender>) {
    let conn =
        Connection::connect_to_env().expect("sidestep: can't reach a Wayland compositor (is WAYLAND_DISPLAY set?)");
    let (globals, queue) = registry_queue_init(&conn).expect("sidestep: Wayland registry");
    let qh = queue.handle();
    let mut event_loop: EventLoop<State> = EventLoop::try_new().expect("sidestep: event loop");
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
    let mut state = State {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        compositor,
        subcompositor,
        viewporter,
        xdg,
        shm,
        pool,
        qh,
        pointer: None,
        windows: HashMap::new(),
        glyphs: Glyphs::default(),
        to_main,
        exit: false,
    };
    while !state.exit {
        if event_loop.dispatch(None, &mut state).is_err() {
            break;
        }
    }
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    subcompositor: SubcompositorState,
    viewporter: SimpleGlobal<WpViewporter, 1>,
    xdg: XdgShell,
    shm: Shm,
    pool: SlotPool,
    qh: QueueHandle<State>,
    pointer: Option<wl_pointer::WlPointer>,
    windows: HashMap<WindowId, Win>,
    glyphs: Glyphs,
    to_main: mpsc::Sender<FromRender>,
    exit: bool,
}

struct Win {
    window: Window,
    width: u32,
    height: u32,
    configured: bool,
    canvas: Vec<u32>,
    damage: Vec<Rect>,
    buffers: Vec<RootBuffer>,
    layers: HashMap<LayerId, ScrollLayer>,
    /// Counts presents; a frame callback names the present it belongs to.
    frame_seq: u64,
    frame_signalled: bool,
}

/// What a frame callback belongs to.
struct FrameTag {
    window: WindowId,
    seq: u64,
}

struct RootBuffer {
    buffer: Buffer,
    /// Areas that changed since this buffer was last written.
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
fn as_pixels(bytes: &mut [u8]) -> &mut [u32] {
    // SAFETY: the pool maps whole pages, so the bytes are 4-byte aligned, and
    // any bit pattern is a valid u32.
    unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<u32>(), bytes.len() / 4) }
}

impl State {
    fn handle(&mut self, msg: ToRender) {
        match msg {
            ToRender::CreateWindow { window, width, height, title } => {
                let surface = self.compositor.create_surface(&self.qh);
                let xdg_window = self.xdg.create_window(surface, WindowDecorations::RequestServer, &self.qh);
                xdg_window.set_title(title);
                xdg_window.set_app_id("sidestep");
                xdg_window.commit();
                self.windows.insert(
                    window,
                    Win {
                        window: xdg_window,
                        width,
                        height,
                        configured: false,
                        canvas: Vec::new(),
                        damage: Vec::new(),
                        buffers: Vec::new(),
                        layers: HashMap::new(),
                        frame_seq: 0,
                        frame_signalled: false,
                    },
                );
            }
            ToRender::SetTitle { window, title } => {
                if let Some(win) = self.windows.get(&window) {
                    win.window.set_title(title);
                }
            }
            ToRender::Paint { window, layer, rects, ops } => {
                let Some(win) = self.windows.get_mut(&window) else { return };
                if layer == ROOT_LAYER {
                    if !win.configured {
                        return;
                    }
                    let mut canvas =
                        Canvas { px: &mut win.canvas, width: win.width, height: win.height, origin_y: 0.0 };
                    raster::paint(&mut canvas, &mut self.glyphs, &rects, &ops);
                    win.damage.extend(rects);
                    return;
                }
                let Some(scroll) = win.layers.get_mut(&layer) else { return };
                let width = scroll.doc_width;
                for rect in &rects {
                    let first = (rect.y0.max(0.0) as u32) / TILE_HEIGHT;
                    let last = ((rect.y1 - 1.0).max(0.0) as u32) / TILE_HEIGHT;
                    for index in first..=last {
                        let tile = scroll.tiles.entry(index).or_insert_with(|| {
                            let (subsurface, surface) =
                                self.subcompositor.create_subsurface(win.window.wl_surface().clone(), &self.qh);
                            let viewport =
                                self.viewporter.get().expect("viewporter").get_viewport(&surface, &self.qh, ());
                            Tile {
                                canvas: vec![BACKGROUND; (width * TILE_HEIGHT) as usize],
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
                            width,
                            height: TILE_HEIGHT,
                            origin_y: (index * TILE_HEIGHT) as f32,
                        };
                        raster::paint(&mut canvas, &mut self.glyphs, std::slice::from_ref(rect), &ops);
                        tile.dirty = true;
                    }
                }
            }
            ToRender::ScrollLayer { window, layer, viewport, offset, doc_width } => {
                let Some(win) = self.windows.get_mut(&window) else { return };
                let scroll = win.layers.entry(layer).or_insert_with(|| ScrollLayer {
                    viewport,
                    offset,
                    doc_width,
                    tiles: BTreeMap::new(),
                });
                if scroll.doc_width != doc_width {
                    scroll.tiles.clear();
                    scroll.doc_width = doc_width;
                }
                scroll.viewport = viewport;
                scroll.offset = offset;
            }
            ToRender::DropTiles { window, layer, tiles } => {
                if let Some(scroll) = self.windows.get_mut(&window).and_then(|w| w.layers.get_mut(&layer)) {
                    for index in tiles {
                        scroll.tiles.remove(&index);
                    }
                }
            }
            ToRender::Present { window } => self.present(window),
            ToRender::CloseWindow { window } => {
                self.windows.remove(&window);
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
                    let (buffer, bytes) = self
                        .pool
                        .create_buffer(width as i32, TILE_HEIGHT as i32, width as i32 * 4, wl_shm::Format::Xrgb8888)
                        .expect("sidestep: tile buffer");
                    as_pixels(bytes).copy_from_slice(&tile.canvas);
                    buffer.attach_to(&tile.surface).expect("sidestep: attach tile");
                    tile.surface.damage_buffer(0, 0, width as i32, TILE_HEIGHT as i32);
                    tile.buffer = Some(buffer);
                    tile.dirty = false;
                    tile.mapped = true;
                }
                let (w, h) = (shown.x1 - shown.x0, shown.y1 - shown.y0);
                tile.subsurface.set_position(shown.x0 as i32, shown.y0 as i32);
                tile.viewport.set_source((shown.x0 - vp.x0) as f64, (shown.y0 - top) as f64, w as f64, h as f64);
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
                            win.width as i32,
                            win.height as i32,
                            win.width as i32 * 4,
                            wl_shm::Format::Xrgb8888,
                        )
                        .expect("sidestep: window buffer");
                    let full = Rect::new(0.0, 0.0, win.width as f32, win.height as f32);
                    win.buffers.push(RootBuffer { buffer, stale: vec![full] });
                    win.buffers.len() - 1
                }
            };
            for (i, b) in win.buffers.iter_mut().enumerate() {
                if i != index {
                    b.stale.extend_from_slice(&damage);
                }
            }
            let target = &mut win.buffers[index];
            let stale = std::mem::take(&mut target.stale);
            let pixels = as_pixels(target.buffer.canvas(&mut self.pool).expect("free buffer"));
            for r in stale.iter().chain(&damage) {
                copy_rows(pixels, &win.canvas, win.width, win.height, r);
            }
            let surface = win.window.wl_surface();
            target.buffer.attach_to(surface).expect("sidestep: attach window buffer");
            for r in &damage {
                let r = r.round_out();
                surface.damage_buffer(r.x0 as i32, r.y0 as i32, (r.x1 - r.x0) as i32, (r.y1 - r.y0) as i32);
            }
        }
        let surface = win.window.wl_surface();
        surface.frame(&self.qh, FrameTag { window, seq });
        surface.commit();
    }

    fn window_for_surface(&self, surface: &WlSurface) -> Option<(WindowId, f64, f64)> {
        for (&id, win) in &self.windows {
            if win.window.wl_surface() == surface {
                return Some((id, 0.0, 0.0));
            }
            for scroll in win.layers.values() {
                for tile in scroll.tiles.values() {
                    if &tile.surface == surface {
                        return Some((id, tile.placed.x0 as f64, tile.placed.y0 as f64));
                    }
                }
            }
        }
        None
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: i32) {}
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
        if let Some((id, _, _)) = self.window_for_surface(window.wl_surface()) {
            let _ = self.to_main.send(FromRender::CloseRequested { window: id });
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
        let Some((id, _, _)) = self.window_for_surface(window.wl_surface()) else { return };
        let win = self.windows.get_mut(&id).expect("window");
        let width = configure.new_size.0.map_or(win.width, |w| w.get());
        let height = configure.new_size.1.map_or(win.height, |h| h.get());
        if !win.configured || width != win.width || height != win.height {
            win.configured = true;
            win.width = width;
            win.height = height;
            win.canvas = vec![BACKGROUND; (width * height) as usize];
            win.buffers.clear();
            win.damage.clear();
            let _ = self.to_main.send(FromRender::Configure { window: id, width, height });
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seats.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some((window, ox, oy)) = self.window_for_surface(&event.surface) else { continue };
            let (x, y) = (event.position.0 + ox, event.position.1 + oy);
            let msg = match event.kind {
                PointerEventKind::Press { button, .. } => FromRender::Button { window, x, y, button, pressed: true },
                PointerEventKind::Release { button, .. } => FromRender::Button { window, x, y, button, pressed: false },
                PointerEventKind::Motion { .. } => FromRender::Motion { window, x, y },
                PointerEventKind::Axis { vertical, .. } => FromRender::Scroll { window, x, y, dy: vertical.absolute },
                _ => continue,
            };
            let _ = self.to_main.send(msg);
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
    registry_handlers![OutputState, SeatState];
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

delegate_compositor!(State);
delegate_subcompositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_pointer!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);
delegate_simple!(State, WpViewporter, 1);
delegate_registry!(State);
