//! Scroll layers on the render thread: each layer's tiles and its overlay,
//! on subsurfaces of the window's surface.
//!
//! The main thread (`layers`) decides which clip views get layers, where
//! they show and what is drawn into them; this module only keeps pixels
//! and surfaces. A layer arrives as a [`LayerPlace`]: its viewport and
//! origin in the window, its extent, its tile grid, whether it is opaque,
//! the layer it is nested in, and its overlay's rectangle.
//!
//! **Tiles.** A tile holds a rectangle of the layer's device pixels (see
//! [`TileGrid`]) plus a margin as wide as a point on every side, so that a
//! tile placed at whole points (subsurfaces can't be placed anywhere else)
//! always has the pixels its crop starts at: at integer scales a tile
//! then shows exactly the pixels it should, wherever the layer is
//! scrolled to. Only the main thread decides which tiles exist: a paint
//! makes a tile only when it covers the whole tile, margin included,
//! which is how the main thread draws a tile it starts keeping; a paint
//! that only reaches into a tile's margin changes the tile if it is kept
//! and is otherwise left out. So a tile's canvas lives exactly as long as
//! the main thread keeps the tile, and the main thread's memory cap counts
//! every canvas. Surfaces and buffers exist only while a tile is on
//! screen. Opaque layers' tiles are cleared to their color and shown
//! without alpha; transparent layers' tiles are cleared to nothing and
//! shown with it, over what the window's surface drew below them.
//!
//! **Overlays** hold the views painted over a layer (its overlay
//! scrollers). They live in the points of the layer the views are drawn
//! in, the one this layer is nested in (or the window's surface), with a
//! margin like a tile's; a present places and crops an overlay from that
//! layer's origin and viewport. So scrolling the layer an overlay sits in
//! moves it and redraws nothing.
//!
//! **Surfaces.** Every surface set (surface, subsurface, viewport) has an
//! empty input region, so pointer input always lands on the window's own
//! surface, in its coordinates. Sets taken off screen go to a pool for
//! the next tile. A present commits a tile only when its content or its
//! crop changed (a position is the window surface's state, applied by
//! its commit), uploads only the pixels that changed, through two
//! buffers per tile that each remember what they miss (as the window's
//! own buffers do), and restacks only when the surfaces on screen or
//! their order changed. Besides the window's surface, one surface per
//! present asks for a frame callback: the first tile or overlay to commit
//! anyway, else the biggest tile on screen of a layer nested in no other,
//! committed for it, since tiles may cover the window's surface completely
//! and a compositor sends no callbacks to a surface it doesn't show.
//!
//! **Stacking** is by the main thread's paint order: a layer's tiles, the
//! layers nested in it, then its overlay, all directly above the window's
//! surface and so below sheets.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};

use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};

use super::{FrameTag, State, as_pixels, copy_rows};
use crate::protocol::{LayerId, LayerPlace, Op, ROOT_LAYER, Rect, TileKey, WindowId};
use crate::raster::{self, Canvas, Glyphs};

/// A window's scroll layers, and the surfaces they're shown on.
#[derive(Default)]
pub(crate) struct Layers {
    layers: HashMap<LayerId, Layer>,
    /// Surface sets off screen, for tiles coming on screen.
    pool: Vec<Surfaces>,
    /// The surfaces on screen, bottom first, as last stacked.
    stack: Vec<ObjectId>,
    /// What a present works through, kept so presents don't allocate: the
    /// layers' tiles and overlays in paint order, and the surfaces on
    /// screen.
    order: Vec<(u32, LayerId, bool)>,
    on_screen: Vec<(ObjectId, WlSubsurface, WlSurface)>,
    /// Canvases of tiles dropped, for the next tiles (a layer's tiles are
    /// all one size), a few at most.
    spare: Vec<Vec<u32>>,
}

/// How many dropped tiles' canvases a window keeps for new ones.
const SPARE_CANVASES: usize = 4;

struct Layer {
    place: LayerPlace,
    tiles: BTreeMap<TileKey, Tile>,
    overlay: Option<Overlay>,
}

/// A tile: its pixels, what changed since they were last uploaded, and
/// its surfaces while it is on screen.
struct Tile {
    canvas: Vec<u32>,
    dirty: Vec<Rect>,
    shown: Option<Shown>,
}

/// The views drawn above a layer, on a surface of their own.
struct Overlay {
    /// In the points of the layer the views are drawn in.
    rect: Rect,
    canvas: Vec<u32>,
    /// The canvas's top left on that layer's pixel grid (a margin outside
    /// `rect`), and its size.
    x0: i32,
    y0: i32,
    width: u32,
    height: u32,
    dirty: Vec<Rect>,
    shown: Option<Shown>,
}

impl Overlay {
    /// An overlay for `rect`, cleared, with a point's margin of pixels.
    fn new(rect: Rect, scale: f64) -> Overlay {
        let m = scale.ceil().max(1.0) as i32;
        let px = |v: f32| (v as f64 * scale).round() as i32;
        let (x0, y0) = (px(rect.x0) - m, px(rect.y0) - m);
        let width = (px(rect.x1) + m - x0).max(1) as u32;
        let height = (px(rect.y1) + m - y0).max(1) as u32;
        let canvas = vec![0; width as usize * height as usize];
        Overlay { rect, canvas, x0, y0, width, height, dirty: Vec::new(), shown: None }
    }
}

/// A surface set on screen: the surfaces, their buffers, and the crop and
/// place last given them.
struct Shown {
    surfaces: Surfaces,
    buffers: Vec<Upload>,
    crop: Option<Crop>,
    at: (i32, i32),
}

#[derive(Clone, Copy, PartialEq)]
struct Crop {
    /// wp_viewport's source rectangle, in buffer pixels.
    source: (f64, f64, f64, f64),
    /// And its destination size, in points.
    size: (i32, i32),
}

/// A buffer, and the pixels (canvas rectangles) it misses: what changed
/// while another buffer was being written.
struct Upload {
    buffer: Buffer,
    stale: Vec<Rect>,
}

/// A surface for a tile or an overlay: a subsurface of the window's
/// surface, cropped and scaled with a viewport.
struct Surfaces {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
}

impl Drop for Surfaces {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

/// What a present did, for `SIDESTEP_TRACE_FRAMES`.
#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct Stats {
    /// Copied into tiles' buffers, and into overlays'.
    pub bytes: usize,
    pub overlay_bytes: usize,
    pub commits: usize,
    /// Tiles on screen, and tiles kept.
    pub tiles: usize,
    pub held: usize,
}

/// Print what a present of `window` did (`SIDESTEP_TRACE_FRAMES`): its
/// layers' `stats`, and the window surface's `damage` (device pixels),
/// which it commits too.
pub(crate) fn trace(window: WindowId, stats: &Stats, damage: &[Rect]) {
    let window_bytes: f32 = damage.iter().map(|r| (r.x1 - r.x0) * (r.y1 - r.y0) * 4.0).sum();
    eprintln!(
        "sidestep frame: window {window} render: {} bytes uploaded to tiles, {} to overlays, {} to the window; \
         {} surfaces committed, {} tiles on screen, {} kept",
        stats.bytes,
        stats.overlay_bytes,
        window_bytes as usize,
        stats.commits + 1,
        stats.tiles,
        stats.held
    );
}

/// A layer moved, appeared or changed its grid, opacity or overlay.
pub(crate) fn place(state: &mut State, window: WindowId, id: LayerId, place: LayerPlace) {
    let Some(win) = state.windows.get_mut(&window) else { return };
    let scale = win.scale;
    let layers = &mut win.layers;
    let layer = layers.layers.entry(id).or_insert_with(|| Layer { place, tiles: BTreeMap::new(), overlay: None });
    let old = std::mem::replace(&mut layer.place, place);
    if old.grid != place.grid || old.opaque != place.opaque {
        let tiles = std::mem::take(&mut layer.tiles);
        layers.pool.extend(tiles.into_values().filter_map(|t| t.shown).map(|s| off_screen(s.surfaces)));
    }
    // An overlay whose rectangle is the same keeps its pixels, wherever
    // the layer it sits in shows now.
    if layer.overlay.as_ref().map(|o| o.rect) != place.overlay || old.parent != place.parent {
        let gone = layer.overlay.take();
        layers.pool.extend(gone.and_then(|o| o.shown).map(|s| off_screen(s.surfaces)));
        layer.overlay = place.overlay.filter(|r| !r.is_empty()).map(|rect| Overlay::new(rect, scale));
    }
}

/// The layer went away.
pub(crate) fn drop_layer(state: &mut State, window: WindowId, id: LayerId) {
    let Some(win) = state.windows.get_mut(&window) else { return };
    let Some(layer) = win.layers.layers.remove(&id) else { return };
    let pool = &mut win.layers.pool;
    pool.extend(layer.tiles.into_values().filter_map(|t| t.shown).map(|s| off_screen(s.surfaces)));
    pool.extend(layer.overlay.and_then(|o| o.shown).map(|s| off_screen(s.surfaces)));
}

/// Tiles the main thread forgot.
pub(crate) fn drop_tiles(state: &mut State, window: WindowId, id: LayerId, keys: &[TileKey]) {
    let Some(win) = state.windows.get_mut(&window) else { return };
    let layers = &mut win.layers;
    let Some(layer) = layers.layers.get_mut(&id) else { return };
    for key in keys {
        let Some(tile) = layer.tiles.remove(key) else { continue };
        if let Some(shown) = tile.shown {
            layers.pool.push(off_screen(shown.surfaces));
        }
        if layers.spare.len() < SPARE_CANVASES {
            layers.spare.push(tile.canvas);
        }
    }
}

/// The rectangles (layer points, margins included) of a scroll layer's
/// tiles.
pub(crate) fn tile_rects(layers: &Layers, id: LayerId, keys: &[TileKey], scale: f64) -> Vec<Rect> {
    let Some(layer) = layers.layers.get(&id) else { return Vec::new() };
    keys.iter().map(|k| layer.place.grid.padded(*k, scale)).collect()
}

/// The window's scale changed: every tile and overlay was drawn at the
/// old one, and the main thread draws them again.
pub(crate) fn rescaled(layers: &mut Layers) {
    for layer in layers.layers.values_mut() {
        let tiles = std::mem::take(&mut layer.tiles);
        layers.pool.extend(tiles.into_values().filter_map(|t| t.shown).map(|s| off_screen(s.surfaces)));
        if let Some(o) = layer.overlay.take() {
            layers.pool.extend(o.shown.map(|s| off_screen(s.surfaces)));
        }
        // Made again at the new scale by the next placement.
        layer.place.overlay = None;
    }
}

/// Paint `rects` of a layer's tiles (layer points) or of its overlay (the
/// points of the layer it sits in).
pub(crate) fn paint(state: &mut State, window: WindowId, id: LayerId, overlay: bool, rects: &[Rect], ops: &[Op]) {
    let State { windows, glyphs, .. } = state;
    let Some(win) = windows.get_mut(&window) else { return };
    let scale = win.scale;
    let Layers { layers, spare, .. } = &mut win.layers;
    let Some(layer) = layers.get_mut(&id) else { return };
    if overlay {
        layer.paint_overlay(glyphs, scale, rects, ops);
    } else {
        layer.paint_tiles(spare, glyphs, scale, rects, ops);
    }
}

impl Layer {
    /// Paint `rects` into the tiles they reach: the tiles kept, and a tile
    /// a rectangle covers whole (the main thread drawing a new one).
    fn paint_tiles(&mut self, spare: &mut Vec<Vec<u32>>, glyphs: &mut Glyphs, scale: f64, rects: &[Rect], ops: &[Op]) {
        let grid = self.place.grid;
        let clear = self.place.opaque.map_or(0, raster::premultiplied);
        for rect in rects {
            let Some((columns, rows)) = grid.padded_keys(rect, scale) else { continue };
            for row in rows {
                for column in columns.clone() {
                    let key = [column, row];
                    let (x0, y0, w, h) = grid.pixels(key);
                    let tile = match self.tiles.entry(key) {
                        Entry::Occupied(tile) => tile.into_mut(),
                        Entry::Vacant(new) => {
                            // Both threads compute a tile's rectangle the
                            // same way, so covering it is exact.
                            let whole = grid.padded(key, scale);
                            if rect.intersect(&whole) != whole {
                                continue;
                            }
                            // A dropped tile's pixels, when there are some.
                            let mut canvas = spare.pop().unwrap_or_default();
                            canvas.clear();
                            canvas.resize((w * h) as usize, clear);
                            new.insert(Tile { canvas, dirty: Vec::new(), shown: None })
                        }
                    };
                    let mut canvas = Canvas { x0, y0, ..Canvas::new(&mut tile.canvas, w, h, 0.0, scale as f32) };
                    raster::paint(&mut canvas, glyphs, std::slice::from_ref(rect), ops);
                    if let Some(r) = canvas.pixels(rect) {
                        add_dirty(&mut tile.dirty, px_rect(r));
                    }
                }
            }
        }
    }

    fn paint_overlay(&mut self, glyphs: &mut Glyphs, scale: f64, rects: &[Rect], ops: &[Op]) {
        let Some(o) = self.overlay.as_mut() else { return };
        let mut canvas =
            Canvas { x0: o.x0, y0: o.y0, ..Canvas::new(&mut o.canvas, o.width, o.height, 0.0, scale as f32) };
        for rect in rects {
            raster::paint(&mut canvas, glyphs, std::slice::from_ref(rect), ops);
            if let Some(r) = canvas.pixels(rect) {
                add_dirty(&mut o.dirty, px_rect(r));
            }
        }
    }
}

/// Where a canvas's pixel rectangle is, as a `Rect` of pixels.
fn px_rect((x0, y0, x1, y1): (usize, usize, usize, usize)) -> Rect {
    Rect::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32)
}

/// Note a changed rectangle; many become their bounds.
pub(super) fn add_dirty(dirty: &mut Vec<Rect>, r: Rect) {
    if r.is_empty() || dirty.iter().any(|d| d.intersect(&r) == r) {
        return;
    }
    dirty.push(r);
    if dirty.len() > 8 {
        let all = dirty.iter().skip(1).fold(dirty[0], |a, r| a.union(r));
        dirty.clear();
        dirty.push(all);
    }
}

/// Take a surface set off screen, for the pool.
fn off_screen(surfaces: Surfaces) -> Surfaces {
    surfaces.surface.attach(None, 0, 0);
    surfaces.surface.commit();
    surfaces
}

/// What a present needs from the render thread's state.
struct Ctx<'a> {
    pool: &'a mut SlotPool,
    make: &'a mut dyn FnMut() -> Surfaces,
    /// Buffers take canvas pixels as they are (ABGR8888 and XBGR8888)
    /// rather than swizzled (ARGB8888 and XRGB8888), with alpha and without.
    as_is: (bool, bool),
    window: WindowId,
    seq: u64,
    qh: &'a smithay_client_toolkit::reexports::client::QueueHandle<State>,
    stats: Stats,
}

/// Show the window's layers as they now are, before its surface commits:
/// place, crop, upload and stack the surfaces on screen.
pub(crate) fn present(state: &mut State, window: WindowId, seq: u64) -> Stats {
    if state.windows.get(&window).is_none_or(|w| w.layers.layers.is_empty() && w.layers.stack.is_empty()) {
        return Stats::default();
    }
    let rgba = state.rgba();
    let argb = state.shm.formats().contains(&wl_shm::Format::Abgr8888);
    let empty = match &state.empty_region {
        Some(r) => r.wl_region().clone(),
        None => {
            let region = Region::new(&state.compositor).expect("sidestep: wl_region");
            let r = region.wl_region().clone();
            state.empty_region = Some(region);
            r
        }
    };
    let State { windows, pool, subcompositor, viewporter, qh, .. } = state;
    let Some(win) = windows.get_mut(&window) else { return Stats::default() };
    let parent = win.surface().clone();
    let (scale, bounds) = (win.scale, Rect::new(0.0, 0.0, win.width as f32, win.height as f32));
    let viewporter = viewporter.get().expect("viewporter").clone();
    let mut make = || {
        let (subsurface, surface) = subcompositor.create_subsurface(parent.clone(), qh);
        surface.set_input_region(Some(&empty));
        let viewport = viewporter.get_viewport(&surface, qh, ());
        Surfaces { surface, subsurface, viewport }
    };
    let layers = &mut win.layers;
    let mut ctx = Ctx { pool, make: &mut make, as_is: (argb, rgba), window, seq, qh, stats: Stats::default() };

    // Paint order: each layer's tiles, and its overlay.
    let mut order = std::mem::take(&mut layers.order);
    order.clear();
    for (&id, layer) in &layers.layers {
        order.push((layer.place.z[0], id, false));
        if layer.overlay.is_some() {
            order.push((layer.place.z[1], id, true));
        }
    }
    order.sort_unstable();
    // The surfaces on screen, bottom first.
    let mut on_screen = std::mem::take(&mut layers.on_screen);
    on_screen.clear();
    let pool_sets = &mut layers.pool;
    // Whether a commit asked for the frame callback, and else the biggest
    // tile on screen of a layer nested in no other, to ask with. Tiles come
    // before the overlay above them, so they ask first.
    let mut frame_asked = false;
    let mut fallback: Option<(f32, LayerId, TileKey)> = None;
    for &(_, id, is_overlay) in &order {
        if is_overlay {
            // Placed where the layer it sits in shows.
            let under = layers.layers.get(&id).map_or(ROOT_LAYER, |l| l.place.parent);
            let (origin, clip) = if under == ROOT_LAYER {
                ([0.0; 2], bounds)
            } else {
                match layers.layers.get(&under) {
                    Some(l) => (l.place.origin, l.place.viewport.intersect(&bounds)),
                    None => continue,
                }
            };
            let layer = layers.layers.get_mut(&id).expect("a layer in order");
            let Some(o) = layer.overlay.as_mut() else { continue };
            let Some((shown, crop)) = overlay_crop(o, origin, &clip, scale) else {
                if let Some(s) = o.shown.take() {
                    pool_sets.push(off_screen(s.surfaces));
                }
                continue;
            };
            let at = (shown.x0 as i32, shown.y0 as i32);
            let canvas = Pixels { px: &o.canvas, size: (o.width, o.height), alpha: true };
            let tile_bytes = ctx.stats.bytes;
            frame_asked |= show(&mut ctx, pool_sets, &mut o.shown, &mut o.dirty, canvas, crop, at, !frame_asked);
            ctx.stats.overlay_bytes += ctx.stats.bytes - tile_bytes;
            ctx.stats.bytes = tile_bytes;
            let s = &o.shown.as_ref().expect("on screen").surfaces;
            on_screen.push((s.surface.id(), s.subsurface.clone(), s.surface.clone()));
            continue;
        }
        let layer = layers.layers.get_mut(&id).expect("a layer in order");
        let place = layer.place;
        ctx.stats.held += layer.tiles.len();
        for (&key, tile) in layer.tiles.iter_mut() {
            let Some((shown, crop)) = tile_crop(&place, key, scale) else {
                if let Some(s) = tile.shown.take() {
                    pool_sets.push(off_screen(s.surfaces));
                }
                continue;
            };
            let (.., w, h) = place.grid.pixels(key);
            let at = (shown.x0 as i32, shown.y0 as i32);
            let canvas = Pixels { px: &tile.canvas, size: (w, h), alpha: place.opaque.is_none() };
            frame_asked |= show(&mut ctx, pool_sets, &mut tile.shown, &mut tile.dirty, canvas, crop, at, !frame_asked);
            if place.parent == ROOT_LAYER {
                let area = (shown.x1 - shown.x0) * (shown.y1 - shown.y0);
                if fallback.is_none_or(|(most, ..)| area > most) {
                    fallback = Some((area, id, key));
                }
            }
            let s = &tile.shown.as_ref().expect("on screen").surfaces;
            on_screen.push((s.surface.id(), s.subsurface.clone(), s.surface.clone()));
            ctx.stats.tiles += 1;
        }
    }
    // Nothing committed: a tile asks for the frame callback anyway, as a
    // compositor may send none to a window surface its tiles cover.
    if !frame_asked
        && let Some((_, id, key)) = fallback
        && let Some(s) = layers.layers.get(&id).and_then(|l| l.tiles.get(&key)).and_then(|t| t.shown.as_ref())
    {
        s.surfaces.surface.frame(ctx.qh, FrameTag { window: ctx.window, seq: ctx.seq });
        s.surfaces.surface.commit();
        ctx.stats.commits += 1;
    }
    // Restack only when what's on screen changed: each surface directly
    // above the one before, the first directly above the window's surface
    // (so below sheets and anything else above it).
    if on_screen.len() != layers.stack.len() || on_screen.iter().zip(&layers.stack).any(|(s, id)| s.0 != *id) {
        let mut below = &parent;
        for (_, subsurface, surface) in &on_screen {
            subsurface.place_above(below);
            below = surface;
        }
        layers.stack.clear();
        layers.stack.extend(on_screen.iter().map(|(id, ..)| id.clone()));
    }
    on_screen.clear();
    layers.on_screen = on_screen;
    layers.order = order;
    ctx.stats
}

/// A canvas to show: its pixels, its size, and whether its alpha counts.
#[derive(Clone, Copy)]
struct Pixels<'a> {
    px: &'a [u32],
    size: (u32, u32),
    alpha: bool,
}

/// Where a tile shows (whole window points) and its crop, if it shows:
/// its own rectangle in the window, within the viewport and the layer's
/// extent, grown to whole points, which its margin covers.
fn tile_crop(place: &LayerPlace, key: TileKey, scale: f64) -> Option<(Rect, Crop)> {
    let origin = place.origin;
    let moved = |r: Rect| Rect::new(r.x0 + origin[0], r.y0 + origin[1], r.x1 + origin[0], r.y1 + origin[1]);
    let area = place.viewport.intersect(&moved(place.extent).round_out());
    let shown = moved(place.grid.rect(key, scale)).round_out().intersect(&area);
    if shown.is_empty() {
        return None;
    }
    let (x0, y0, w, h) = place.grid.pixels(key);
    // The canvas's top left, in window points.
    let corner = (origin[0] as f64 + x0 as f64 / scale, origin[1] as f64 + y0 as f64 / scale);
    Some((shown, crop_from(&shown, corner, (w, h), scale)))
}

/// Where an overlay shows and its crop, if it shows: its rectangle placed
/// at `origin` (the origin of the layer it sits in) grown to whole points,
/// which its margin covers, within `clip` (where that layer shows).
fn overlay_crop(o: &Overlay, origin: [f32; 2], clip: &Rect, scale: f64) -> Option<(Rect, Crop)> {
    let r = o.rect;
    let shown =
        Rect::new(r.x0 + origin[0], r.y0 + origin[1], r.x1 + origin[0], r.y1 + origin[1]).round_out().intersect(clip);
    if shown.is_empty() {
        return None;
    }
    let corner = (origin[0] as f64 + o.x0 as f64 / scale, origin[1] as f64 + o.y0 as f64 / scale);
    Some((shown, crop_from(&shown, corner, (o.width, o.height), scale)))
}

/// The crop of a canvas `w` × `h` pixels whose top left is at `corner`
/// (window points) that shows `shown` (whole window points).
fn crop_from(shown: &Rect, corner: (f64, f64), size: (u32, u32), scale: f64) -> Crop {
    let source = (
        ((shown.x0 as f64 - corner.0) * scale).round().clamp(0.0, size.0 as f64 - 1.0),
        ((shown.y0 as f64 - corner.1) * scale).round().clamp(0.0, size.1 as f64 - 1.0),
    );
    crop_of(shown, source, size, scale)
}

/// The crop of a canvas `w` × `h` pixels that shows `shown` (whole window
/// points) from the pixel `(x, y)` on: whole pixels, so the compositor
/// copies rather than filters, and inside the buffer, as the protocol
/// requires.
fn crop_of(shown: &Rect, (x, y): (f64, f64), (w, h): (u32, u32), scale: f64) -> Crop {
    let sw = (((shown.x1 - shown.x0) as f64) * scale).round().clamp(1.0, (w as f64 - x).max(1.0));
    let sh = (((shown.y1 - shown.y0) as f64) * scale).round().clamp(1.0, (h as f64 - y).max(1.0));
    Crop { source: (x, y, sw, sh), size: ((shown.x1 - shown.x0) as i32, (shown.y1 - shown.y0) as i32) }
}

/// Put a canvas on screen, or keep it there: take surfaces if it has none,
/// upload what changed, crop and place it, and commit if its content or
/// crop changed, asking for a frame callback if `frame`. Returns whether
/// it committed with a frame request.
#[allow(clippy::too_many_arguments)]
fn show(
    ctx: &mut Ctx<'_>,
    pool_sets: &mut Vec<Surfaces>,
    slot: &mut Option<Shown>,
    dirty: &mut Vec<Rect>,
    canvas: Pixels<'_>,
    crop: Crop,
    at: (i32, i32),
    frame: bool,
) -> bool {
    let (w, h) = canvas.size;
    if slot.is_none() {
        let surfaces = pool_sets.pop().unwrap_or_else(|| (ctx.make)());
        *slot = Some(Shown { surfaces, buffers: Vec::new(), crop: None, at: (i32::MIN, i32::MIN) });
        // Everything shows for the first time.
        dirty.clear();
        dirty.push(Rect::new(0.0, 0.0, w as f32, h as f32));
    }
    let shown = slot.as_mut().expect("on screen");
    let mut commit = false;
    if !dirty.is_empty() {
        ctx.stats.bytes += upload(ctx.pool, shown, dirty, canvas, ctx.as_is);
        dirty.clear();
        commit = true;
    }
    if shown.at != at {
        shown.at = at;
        shown.surfaces.subsurface.set_position(at.0, at.1);
    }
    if shown.crop != Some(crop) {
        let (x, y, sw, sh) = crop.source;
        shown.surfaces.viewport.set_source(x, y, sw, sh);
        shown.surfaces.viewport.set_destination(crop.size.0, crop.size.1);
        shown.crop = Some(crop);
        commit = true;
    }
    if commit {
        if frame {
            shown.surfaces.surface.frame(ctx.qh, FrameTag { window: ctx.window, seq: ctx.seq });
        }
        shown.surfaces.surface.commit();
        ctx.stats.commits += 1;
    }
    commit && frame
}

/// Copy what changed of a canvas into a buffer the compositor isn't
/// reading (the one it misses least, made if both are busy), attach it,
/// and damage only what changed. Returns the bytes copied.
fn upload(pool: &mut SlotPool, shown: &mut Shown, dirty: &[Rect], canvas: Pixels<'_>, as_is: (bool, bool)) -> usize {
    let (w, h) = canvas.size;
    let as_is = if canvas.alpha { as_is.0 } else { as_is.1 };
    let format = match (canvas.alpha, as_is) {
        (true, true) => wl_shm::Format::Abgr8888,
        (true, false) => wl_shm::Format::Argb8888,
        (false, true) => wl_shm::Format::Xbgr8888,
        (false, false) => wl_shm::Format::Xrgb8888,
    };
    let free = shown.buffers.iter().position(|b| b.buffer.canvas(pool).is_some());
    let index = match free {
        Some(i) => i,
        None => {
            let (buffer, _) =
                pool.create_buffer(w as i32, h as i32, w as i32 * 4, format).expect("sidestep: tile buffer");
            shown.buffers.push(Upload { buffer, stale: vec![Rect::new(0.0, 0.0, w as f32, h as f32)] });
            shown.buffers.len() - 1
        }
    };
    let mut stale: Vec<&mut Vec<Rect>> = shown.buffers.iter_mut().map(|b| &mut b.stale).collect();
    let stale = pass_on(&mut stale, index, dirty);
    let target = &mut shown.buffers[index];
    let bytes = target.buffer.canvas(pool).expect("a free buffer");
    // The pool rounds slots up: the buffer is the start of its slot.
    let pixels = as_pixels(&mut bytes[..(w * h * 4) as usize]);
    let mut copied = 0;
    for r in missed(&stale, dirty) {
        copied += copy_rows(pixels, canvas.px, w, h, r, as_is);
    }
    target.buffer.attach_to(&shown.surfaces.surface).expect("sidestep: attach tile buffer");
    for r in dirty {
        let r = r.round_out();
        shown.surfaces.surface.damage_buffer(r.x0 as i32, r.y0 as i32, (r.x1 - r.x0) as i32, (r.y1 - r.y0) as i32);
    }
    copied
}

/// Two buffers' bookkeeping, for tiles and the window's own buffers: buffer
/// `index` is about to be written with `dirty`; every other buffer learns
/// it now misses `dirty`, and what `index` missed is taken from it, to be
/// copied along with `dirty`.
pub(super) fn pass_on(stale: &mut [&mut Vec<Rect>], index: usize, dirty: &[Rect]) -> Vec<Rect> {
    for (i, s) in stale.iter_mut().enumerate() {
        if i != index {
            for r in dirty {
                add_dirty(s, *r);
            }
        }
    }
    std::mem::take(&mut *stale[index])
}

/// What a buffer copies: what it missed, and what changed that it doesn't
/// already take.
pub(super) fn missed<'a>(stale: &'a [Rect], dirty: &'a [Rect]) -> impl Iterator<Item = &'a Rect> {
    stale.iter().chain(dirty.iter().filter(|d| !stale.iter().any(|s| s.intersect(d) == **d)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TileGrid;

    /// A layer shown at `origin`, its viewport the window's top left 300 by
    /// 200 points, its extent `extent`.
    fn place(origin: [f32; 2], extent: Rect, scale: f64) -> LayerPlace {
        LayerPlace {
            z: [0, 1],
            viewport: Rect::new(0.0, 0.0, 300.0, 200.0),
            origin,
            extent,
            grid: TileGrid::for_layer(&extent, scale),
            opaque: None,
            parent: ROOT_LAYER,
            overlay: None,
        }
    }

    /// Tiles cut by the viewport at every scroll offset show whole pixels
    /// from inside their buffers, the pixels they should at integer scales
    /// (thanks to their margins) and within half a pixel at fractional
    /// ones; the tiles in view cover the viewport without gaps.
    #[test]
    fn tiles_show_the_right_pixels() {
        let extent = Rect::new(0.0, 0.0, 1283.0, 3000.0);
        for scale in [1.0, 1.25, 1.5, 5.0 / 3.0, 2.0, 3.0] {
            for step in 0..600 {
                // Scrolled to whole device pixels, as the main thread does.
                let offset = ((step as f64 * 1.37 * scale).round() / scale) as f32;
                let p = place([0.0, -offset], extent, scale);
                let mut covered = [false; 200];
                for row in 0..8 {
                    let key = [0, row];
                    let Some((shown, crop)) = tile_crop(&p, key, scale) else { continue };
                    let (_, y0, w, h) = p.grid.pixels(key);
                    let (x, y, sw, sh) = crop.source;
                    let at = format!("scale {scale}, offset {offset}, row {row}: {x} {y} {sw} {sh}");
                    assert!([x, y, sw, sh].iter().all(|v| v.fract() == 0.0), "{at}");
                    assert!(x >= 0.0 && y >= 0.0 && sw >= 1.0 && sh >= 1.0, "{at}");
                    assert!(x + sw <= w as f64 && y + sh <= h as f64, "{at}");
                    // The pixel shown at the top is the layer's pixel there.
                    let wanted = (shown.y0 as f64 + offset as f64) * scale - y0 as f64;
                    let tolerance = if scale.fract() == 0.0 { 1e-3 } else { 0.5 + 1e-3 };
                    assert!((y - wanted).abs() <= tolerance, "{at}: wanted {wanted}");
                    covered[shown.y0 as usize..shown.y1 as usize].fill(true);
                }
                assert!(covered.iter().all(|c| *c), "scale {scale}, offset {offset}: a gap");
            }
        }
    }

    /// An overlay in the points of a layer scrolled to any device pixel
    /// shows whole pixels from inside its canvas, the ones it should at
    /// integer scales, and all of itself that the layer's viewport shows.
    #[test]
    fn overlays_show_the_right_pixels() {
        let rect = Rect::new(183.0, 40.0, 200.0, 190.0);
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let o = Overlay::new(rect, scale);
            for step in 0..400 {
                let offset = ((step as f64 * 0.73 * scale).round() / scale) as f32;
                let origin = [10.0, 5.0 - offset];
                let clip = Rect::new(10.0, 5.0, 210.0, 155.0);
                let Some((shown, crop)) = overlay_crop(&o, origin, &clip, scale) else {
                    // Scrolled out of the viewport.
                    assert!(rect.y1 + origin[1] <= clip.y0 + 1.0, "scale {scale}, offset {offset}");
                    continue;
                };
                let (x, y, sw, sh) = crop.source;
                let at = format!("scale {scale}, offset {offset}: {x} {y} {sw} {sh}");
                assert!([x, y, sw, sh].iter().all(|v| v.fract() == 0.0), "{at}");
                assert!(x + sw <= o.width as f64 && y + sh <= o.height as f64, "{at}");
                let wanted = (shown.y0 as f64 - origin[1] as f64) * scale - o.y0 as f64;
                let tolerance = if scale.fract() == 0.0 { 1e-3 } else { 0.5 + 1e-3 };
                assert!((y - wanted).abs() <= tolerance, "{at}: wanted {wanted}");
                // What the viewport shows of the overlay is shown.
                let visible =
                    Rect::new(rect.x0 + origin[0], rect.y0 + origin[1], rect.x1 + origin[0], rect.y1 + origin[1])
                        .intersect(&clip);
                assert!(shown.intersect(&visible) == visible, "{at}: {shown:?} {visible:?}");
            }
        }
    }

    /// A paint makes only a tile it covers whole, as the main thread
    /// records a tile it starts keeping; paints reaching into a margin
    /// change the tiles kept and make none. Dropping the tiles the main
    /// thread kept leaves nothing.
    #[test]
    fn paints_make_only_the_tiles_recorded() {
        let scale = 1.0;
        let extent = Rect::new(0.0, 0.0, 900.0, 5000.0);
        let mut layer = Layer { place: place([0.0, 0.0], extent, scale), tiles: BTreeMap::new(), overlay: None };
        let grid = layer.place.grid;
        let (mut spare, mut glyphs) = (Vec::new(), Glyphs::default());
        let fill = |r: Rect| vec![Op::Fill { rect: r, color: [1.0, 0.0, 0.0, 1.0] }];
        let keys = |l: &Layer| l.tiles.keys().copied().collect::<Vec<_>>();
        let padded = grid.padded([0, 5], scale);
        layer.paint_tiles(&mut spare, &mut glyphs, scale, &[padded], &fill(padded));
        assert_eq!(keys(&layer), [[0, 5]]);
        // Damage across the edge between rows 5 and 6: row 5 only.
        let edge = Rect::new(10.0, 3071.0, 50.0, 3073.0);
        layer.tiles.values_mut().for_each(|t| t.dirty.clear());
        layer.paint_tiles(&mut spare, &mut glyphs, scale, &[edge], &fill(edge));
        assert_eq!(keys(&layer), [[0, 5]]);
        assert!(!layer.tiles[&[0, 5]].dirty.is_empty());
        // Row 6 recorded: both hold what's painted across the edge.
        let next = grid.padded([0, 6], scale);
        layer.paint_tiles(&mut spare, &mut glyphs, scale, &[next], &fill(next));
        layer.tiles.values_mut().for_each(|t| t.dirty.clear());
        layer.paint_tiles(&mut spare, &mut glyphs, scale, &[edge], &fill(edge));
        assert_eq!(keys(&layer), [[0, 5], [0, 6]]);
        assert!(layer.tiles.values().all(|t| !t.dirty.is_empty()));
        // At a fractional scale too.
        let scale = 1.25;
        let mut layer = Layer { place: place([0.0, 0.0], extent, scale), tiles: BTreeMap::new(), overlay: None };
        let grid = layer.place.grid;
        for key in [[0, 3], [0, 4]] {
            let r = grid.padded(key, scale);
            layer.paint_tiles(&mut spare, &mut glyphs, scale, &[r], &fill(r));
        }
        assert_eq!(keys(&layer), [[0, 3], [0, 4]]);
        for key in [[0, 3], [0, 4]] {
            layer.tiles.remove(&key);
        }
        assert!(layer.tiles.is_empty());
    }

    /// A buffer written misses nothing; the other learns what it now
    /// misses, and copies it when its turn comes.
    #[test]
    fn buffers_copy_what_they_miss() {
        let full = Rect::new(0.0, 0.0, 100.0, 100.0);
        let (mut a, mut b) = (vec![full], vec![full]);
        let d1 = Rect::new(0.0, 0.0, 10.0, 10.0);
        // A new buffer copies everything, once.
        assert_eq!(pass_on(&mut [&mut a, &mut b], 0, &[d1]), [full]);
        assert!(a.is_empty());
        assert_eq!(b, [full]);
        assert_eq!(pass_on(&mut [&mut a, &mut b], 1, &[d1]), [full]);
        assert_eq!(a, [d1]);
        // Then only what changed while the other was written.
        let d2 = Rect::new(50.0, 50.0, 60.0, 60.0);
        assert_eq!(pass_on(&mut [&mut a, &mut b], 0, &[d2]), [d1]);
        assert_eq!(b, [d2]);
        assert_eq!(pass_on(&mut [&mut a, &mut b], 1, &[]), [d2]);
        assert!(a.is_empty() && b.is_empty());
        // What a buffer copies: what it missed, and what changed that isn't
        // already in it.
        let stale = [Rect::new(0.0, 0.0, 50.0, 50.0)];
        let dirty = [Rect::new(10.0, 10.0, 20.0, 20.0), d2];
        assert_eq!(missed(&stale, &dirty).copied().collect::<Vec<_>>(), [stale[0], d2]);
    }

    #[test]
    fn changed_rectangles_merge() {
        let mut dirty = Vec::new();
        add_dirty(&mut dirty, Rect::new(0.0, 0.0, 10.0, 10.0));
        // Inside one already there: nothing new.
        add_dirty(&mut dirty, Rect::new(2.0, 2.0, 5.0, 5.0));
        assert_eq!(dirty.len(), 1);
        for i in 1..9 {
            let x = (i * 20) as f32;
            add_dirty(&mut dirty, Rect::new(x, 0.0, x + 5.0, 5.0));
        }
        // Past eight, their bounds.
        assert_eq!(dirty, [Rect::new(0.0, 0.0, 165.0, 10.0)]);
    }
}
