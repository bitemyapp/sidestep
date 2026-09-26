//! Scroll layers on the main thread: which clip views get a layer of their
//! own, where the render thread shows each, and what the display pass
//! records into the window's surface, the layers' tiles and their
//! overlays. `backend::tiles` keeps the pixels and the surfaces.
//!
//! **Promotion.** A clip view gets a layer when it is in a window and
//! shown (no hidden or faded view above it), its visible part is at least
//! 64 device pixels each way, its document overflows it along some axis,
//! and the window has fewer than 24 layers. A clip view inside another
//! promoted one's document gets a layer too, nested in that one's; a clip
//! view without a layer draws its document inline, into whatever layer it
//! is in, and scrolling it redraws its visible part. The decision is made
//! again at every display pass; a clip view gaining or losing its layer
//! has the layer it sits in redraw its visible part (without its document,
//! or with it).
//!
//! **Coordinates.** A layer's points are its clip view's bounds
//! coordinates, with y turned downward when the clip view (like its
//! document) isn't flipped. So a flipped document grows downward from row
//! 0 and an unflipped one into negative rows, and a document that grows
//! never moves the pixels already drawn; scrolling only moves where the
//! layer's origin is in the window. Tiles are a grid of device pixels
//! over that ([`TileGrid`]): 512 tall and as wide as the layer, up to
//! 2048, so horizontal scrolling moves tiles too.
//!
//! **Tiles.** Each pass records the tiles that come into the viewport, the
//! damaged parts of the tiles the render thread keeps, and, while the pass
//! has spent less than [`PREFETCH_BUDGET`], one tile ahead in the
//! direction the layer is scrolling (after telling the document with
//! `prepareContentInRect:`). Tiles more than two tiles from the viewport
//! are dropped, and the farthest ones too while a window's tiles hold more
//! than [`MEMORY_CAP`]; so is a tile out of view that is damaged over half
//! of it or more (a document drawn whole again), rather than drawn before
//! it is needed. A layer whose clip view draws an opaque background
//! has tiles cleared to it; any other layer's tiles are transparent, and
//! what the window's surface drew below them (the clip view's own
//! background, what's behind the scroll view) shows through.
//!
//! **Overlays.** Tiles are subsurfaces above the window's surface, so what
//! is painted after a clip view and over its viewport (its scroll view's
//! overlay scrollers, a view floating over the scroll view) would be
//! hidden under them. Such views are drawn into the layer's overlay
//! instead: a transparent surface above the layer's tiles and the layers
//! nested in it. They are found by walking the views after the clip view
//! in paint order, only through those that reach the viewport or the
//! overlay so far, and going inside views that draw nothing themselves;
//! the layer they belong to leaves them out, so a translucent one isn't
//! drawn twice. The overlay is the size of their frames in the layer they
//! are drawn in (the one this layer is nested in, or the window's
//! surface), in that layer's points, so scrolling that layer moves the
//! overlay with it and redraws nothing; only one far bigger than the
//! window is cut to what that layer shows. The overlay is drawn again
//! where the layer it sits over is damaged, and whole when its rectangle
//! or its views change.
//!
//! **Passes.** A pass records the tiles in view, the damaged parts of the
//! layers and the overlays, and has the window present; then, while it
//! has spent less than [`PREFETCH_BUDGET`], it records tiles ahead, which
//! the render thread draws while the compositor shows the frame. Drawing
//! runs program code, which may order the window out, see its scale
//! change, remove clip views or display another window: the layers are
//! out of the window during a pass, and what happens to them meanwhile is
//! noted and applied when they go back. Each window runs one pass at a
//! time; a window asked to display during its own pass does so at the
//! next.
//!
//! `SIDESTEP_TRACE_FRAMES=1` prints, for each pass, the time the main
//! thread took and what it recorded; the render thread prints what it
//! uploaded and committed.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, Imp, Sel};
use objc2::{ClassType, Message, msg_send, sel};
use objc2_app_kit::NSView;
use objc2_foundation::NSRect;

use crate::app;
use crate::graphics::{self, Xf};
use crate::protocol::{LayerId, LayerPlace, Op, ROOT_LAYER, Rect, Target, TileGrid, TileKey, ToRender};
use crate::views::{self, NSViewImpl};
use crate::window::NSWindowImpl;

/// At most this many layers in a window, its own surface included.
const MAX_LAYERS: usize = 24;

/// A clip view smaller than this (device pixels) either way draws inline.
const MIN_VIEWPORT: f64 = 64.0;

/// Tiles are dropped beyond this many tiles from the viewport.
const KEEP: i32 = 2;

/// The most a window's tiles hold, in bytes, before the farthest go.
const MEMORY_CAP: usize = 96 << 20;

/// How long a pass may have spent before it stops drawing tiles ahead.
const PREFETCH_BUDGET: Duration = Duration::from_millis(4);

/// Whether `SIDESTEP_TRACE_FRAMES` asks for counters (read once).
pub(crate) fn tracing() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("SIDESTEP_TRACE_FRAMES").is_some_and(|v| v != "0"))
}

/// Where damage goes: the layer's own, or, for a view drawn in an overlay,
/// only the overlays over that layer (keyed by the layer's id with its low
/// bit set: ids are view addresses, and the root's is 0).
pub(crate) fn damage_key(layer: LayerId, overlay: bool) -> LayerId {
    layer | LayerId::from(overlay)
}

/// A clip view's part: whether it has a layer now. Kept in the clip
/// view, so placing a view (`views::placement`) needs no window.
#[derive(Default)]
pub(crate) struct ClipLayer {
    promoted: Cell<bool>,
}

/// Whether `clip`, a clip view, draws its document into a layer of its
/// own.
pub(crate) fn promoted(clip: &NSViewImpl) -> bool {
    crate::scroll::clip_layer(clip).is_some_and(|l| l.promoted.get())
}

fn set_promoted(clip: &NSViewImpl, on: bool) {
    if let Some(l) = crate::scroll::clip_layer(clip) {
        l.promoted.set(on);
    }
}

/// The map from a clip view's bounds to its layer's points.
pub(crate) fn layer_xf(clip: &NSViewImpl) -> Xf {
    Xf { tx: 0.0, a: if views::is_flipped(clip) { 1.0 } else { -1.0 }, ty: 0.0 }
}

/// A window's layers, kept in its ivars.
#[derive(Default)]
pub(crate) struct Layers {
    layers: HashMap<LayerId, Layer>,
    /// Set while a pass has the layers out (this is its stand-in): what
    /// happened to them meanwhile, for the pass to apply.
    out: Option<Meanwhile>,
}

/// What happened to a window's layers while a pass had them out.
#[derive(Default)]
struct Meanwhile {
    /// The window went off screen, or its scale changed.
    reset: bool,
    rescaled: bool,
    /// Clip views that left the window.
    left: Vec<LayerId>,
}

impl Meanwhile {
    fn is_empty(&self) -> bool {
        !self.reset && !self.rescaled && self.left.is_empty()
    }
}

struct Layer {
    clip: Retained<NSView>,
    /// The layer it is nested in.
    parent: LayerId,
    /// What the render thread was last told.
    sent: Option<LayerPlace>,
    /// The tiles the render thread holds, drawn and kept up to date.
    valid: HashSet<TileKey>,
    /// The views drawn into the overlay, in paint order.
    overlay_views: Vec<Retained<NSView>>,
    /// The overlay holds its views' drawing (since its rectangle last
    /// changed).
    overlay_drawn: bool,
    /// The part of the layer shown at the last pass (layer points), to
    /// tell which way it scrolls.
    visible: Rect,
    /// What passes have drawn, for tests: paints of tiles and of the
    /// overlay, and the area painted into tiles (points squared).
    painted: (u32, u32, f64),
}

impl Layers {
    /// The window went off screen, and its layers with it.
    pub(crate) fn reset(&mut self) {
        if let Some(meanwhile) = &mut self.out {
            meanwhile.reset = true;
        }
        for layer in std::mem::take(&mut self.layers).into_values() {
            forget(&layer);
        }
    }

    /// The window's scale changed: the render thread dropped every tile and
    /// overlay, and needs every placement again.
    pub(crate) fn rescaled(&mut self) {
        if let Some(meanwhile) = &mut self.out {
            meanwhile.rescaled = true;
        }
        for layer in self.layers.values_mut() {
            layer.valid.clear();
            layer.sent = None;
            layer.overlay_drawn = false;
        }
    }

    /// Apply what happened to the layers while a pass had them out.
    fn catch_up(&mut self, window: &NSWindowImpl, meanwhile: Meanwhile) {
        for id in meanwhile.left {
            if let Some(layer) = self.layers.remove(&id) {
                forget(&layer);
                if window.on_screen() {
                    app::send(ToRender::DropLayer { window: window.id(), layer: id });
                }
            }
        }
        if meanwhile.reset {
            self.reset();
        } else if meanwhile.rescaled {
            self.rescaled();
        }
    }
}

/// A layer is gone: its clip view and its overlay's views lose their flags.
fn forget(layer: &Layer) {
    set_promoted(views::imp(&layer.clip), false);
    for v in &layer.overlay_views {
        crate::view_layout::set_in_overlay(views::imp(v), false);
    }
}

/// Whether a display pass of `window` is running.
pub(crate) fn in_pass(window: &NSWindowImpl) -> bool {
    window.layers().try_borrow().map_or(true, |l| l.out.is_some())
}

/// `clip` left the window: its layer goes (when the pass running has the
/// layers, as it puts them back).
pub(crate) fn clip_left(window: &NSWindowImpl, clip: &NSViewImpl) {
    set_promoted(clip, false);
    let id = views::layer_id(clip);
    let gone = window.layers().try_borrow_mut().ok().and_then(|mut l| match &mut l.out {
        Some(meanwhile) => {
            meanwhile.left.push(id);
            None
        }
        None => l.layers.remove(&id),
    });
    if let Some(layer) = gone {
        forget(&layer);
        if window.on_screen() {
            app::send(ToRender::DropLayer { window: window.id(), layer: id });
        }
    }
}

/// A clip view the pass gives a layer: where it shows and what's over it.
struct Plan {
    clip: Retained<NSView>,
    id: LayerId,
    /// The clip view's place in paint order: its index among its siblings
    /// at each depth, from the content view down.
    path: Vec<usize>,
    parent: LayerId,
    place: LayerPlace,
    overlay_views: Vec<Retained<NSView>>,
}

thread_local! {
    /// What `record` is drawing (see [`Mode`]).
    static MODE: Cell<Mode> = const { Cell::new(Mode::Inline) };
    /// The most a window's tiles may hold (tests lower it).
    static CAP: Cell<usize> = const { Cell::new(MEMORY_CAP) };
}

/// Lower the tile memory cap, or restore it (`testing`).
pub(crate) fn set_memory_cap(bytes: Option<usize>) {
    CAP.with(|c| c.set(bytes.unwrap_or(MEMORY_CAP)));
}

/// What [`record`] draws.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Everything inline: a snapshot (`cacheDisplayInRect:`).
    Inline,
    /// A layer in a window's display pass: promoted clip views' documents
    /// and views drawn in overlays are left out.
    Layer,
    /// An overlay: promoted clip views' documents are left out.
    Overlay,
}

/// Set a recording mode for as long as the guard lives.
struct ModeGuard(Mode);

impl ModeGuard {
    fn set(mode: Mode) -> ModeGuard {
        ModeGuard(MODE.with(|m| m.replace(mode)))
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        MODE.with(|m| m.set(self.0));
    }
}

/// What a pass did, for `SIDESTEP_TRACE_FRAMES`, and whether it changed
/// anything the render thread shows.
#[derive(Default)]
struct Counts {
    tiles: usize,
    paints: usize,
    /// Layers placed, dropped or given fewer tiles.
    placed: usize,
}

/// Record everything that changed in `window` for the render thread: place
/// its layers, draw new and damaged tiles, the window's surface and the
/// overlays; then have `present` present the frame (told whether anything
/// changed), and draw tiles ahead. Does nothing while a pass of the window
/// is running.
pub(crate) fn display(window: &NSWindowImpl, present: impl FnOnce(bool)) {
    let start = Instant::now();
    let mut counts = Counts::default();
    // Out of the window while the pass runs, with a stand-in noting what
    // happens meanwhile: drawing runs program code.
    let mut layers = {
        let mut kept = window.layers().borrow_mut();
        if kept.out.is_some() {
            return;
        }
        std::mem::replace(&mut *kept, Layers { layers: HashMap::new(), out: Some(Meanwhile::default()) })
    };
    let plans = plan(window);
    counts.placed += apply_promotions(window, &mut layers, &plans);
    let mut damage = window.take_damage();
    let scale = window.scale();
    let ring = crate::controls::focus::ring_view(window);
    for plan in &plans {
        let layer = layers.layers.get_mut(&plan.id).expect("a planned layer");
        let layer_damage = damage.get(&plan.id).map(Vec::as_slice).unwrap_or_default();
        draw_tiles(window, plan, layer, layer_damage, scale, ring.as_deref(), &mut counts);
    }
    let root_damage = damage.remove(&ROOT_LAYER).unwrap_or_default();
    draw_root(window, &root_damage, scale, ring.as_deref(), &mut counts);
    let height = window.content_height();
    for plan in plans.iter().filter(|p| p.place.overlay.is_some()) {
        let layer = layers.layers.get_mut(&plan.id).expect("a planned layer");
        // The overlay's views are in the layer this one is nested in, in its
        // points: that layer's damage, and the damage only its overlays take.
        let own = if plan.parent == ROOT_LAYER { Some(&root_damage) } else { damage.get(&plan.parent) };
        let above = damage.get(&damage_key(plan.parent, true));
        let under: Vec<Rect> = own.into_iter().chain(above).flatten().copied().collect();
        let base = match plans.iter().find(|p| p.id == plan.parent) {
            Some(parent) => layer_xf(views::imp(&parent.clip)),
            None => Xf { tx: 0.0, a: -1.0, ty: height },
        };
        draw_overlay(window, plan, layer, &under, base, scale, ring.as_deref(), &mut counts);
    }
    present(counts.paints + counts.placed > 0);
    // Tiles ahead, after the frame: the render thread draws them while the
    // compositor shows it.
    let budget = start + PREFETCH_BUDGET;
    for plan in &plans {
        if Instant::now() >= budget {
            break;
        }
        let layer = layers.layers.get_mut(&plan.id).expect("a planned layer");
        prefetch(window, plan, layer, scale, ring.as_deref(), &mut counts);
    }
    evict(window, &mut layers, &plans, scale);
    for plan in &plans {
        if let Some(layer) = layers.layers.get_mut(&plan.id) {
            layer.visible = visible_part(&plan.place);
        }
    }
    // Put back, with what happened meanwhile (catching up releases views,
    // which runs program code: the stand-in stays until it is done).
    loop {
        let meanwhile = window.layers().borrow_mut().out.as_mut().map(std::mem::take).unwrap_or_default();
        if meanwhile.is_empty() {
            break;
        }
        layers.catch_up(window, meanwhile);
    }
    let stand_in = std::mem::replace(&mut *window.layers().borrow_mut(), layers);
    drop(stand_in);
    if tracing() {
        eprintln!(
            "sidestep frame: window {} main: {:.2} ms, {} tiles recorded, {} paints, {} layers",
            window.id(),
            start.elapsed().as_secs_f64() * 1000.0,
            counts.tiles,
            counts.paints,
            plans.len()
        );
    }
}

/// Clip views that gained or lost their layers: flag them, tell the render
/// thread, and redraw where they are in the layer they sit in. Returns how
/// many layers went.
fn apply_promotions(window: &NSWindowImpl, layers: &mut Layers, plans: &[Plan]) -> usize {
    let planned: HashSet<LayerId> = plans.iter().map(|p| p.id).collect();
    let gone: Vec<LayerId> = layers.layers.keys().copied().filter(|id| !planned.contains(id)).collect();
    let dropped = gone.len();
    for id in gone {
        let layer = layers.layers.remove(&id).expect("a layer");
        let clip = views::imp(&layer.clip);
        set_promoted(clip, false);
        for v in &layer.overlay_views {
            let v = views::imp(v);
            crate::view_layout::set_in_overlay(v, false);
            views::invalidate(v, views::bounds(v));
        }
        app::send(ToRender::DropLayer { window: window.id(), layer: id });
        window.discard_damage(id);
        views::invalidate(clip, views::bounds(clip));
    }
    for plan in plans {
        let clip = views::imp(&plan.clip);
        if let std::collections::hash_map::Entry::Vacant(entry) = layers.layers.entry(plan.id) {
            set_promoted(clip, true);
            entry.insert(Layer {
                clip: plan.clip.clone(),
                parent: plan.parent,
                sent: None,
                valid: HashSet::new(),
                overlay_views: Vec::new(),
                overlay_drawn: false,
                visible: Rect::default(),
                painted: (0, 0, 0.0),
            });
            // Its document was drawn inline; now it's in the tiles.
            views::invalidate(clip, views::bounds(clip));
        }
    }
    // Views moving into overlays or out of them: the layers below redraw
    // where they are.
    for plan in plans {
        let layer = layers.layers.get_mut(&plan.id).expect("a layer");
        layer.parent = plan.parent;
        let same = |a: &Retained<NSView>, b: &Retained<NSView>| std::ptr::eq(&**a, &**b);
        for v in &layer.overlay_views {
            if !plan.overlay_views.iter().any(|n| same(n, v)) {
                crate::view_layout::set_in_overlay(views::imp(v), false);
                views::invalidate(views::imp(v), views::bounds(views::imp(v)));
            }
        }
        for v in &plan.overlay_views {
            if !layer.overlay_views.iter().any(|o| same(o, v)) {
                views::invalidate(views::imp(v), views::bounds(views::imp(v)));
            }
        }
        for v in &plan.overlay_views {
            crate::view_layout::set_in_overlay(views::imp(v), true);
        }
        if layer.overlay_views.len() != plan.overlay_views.len()
            || layer.overlay_views.iter().zip(&plan.overlay_views).any(|(a, b)| !same(a, b))
        {
            layer.overlay_drawn = false;
        }
        layer.overlay_views = plan.overlay_views.clone();
    }
    dropped
}

/// The part of a layer its viewport shows, in its points.
fn visible_part(place: &LayerPlace) -> Rect {
    let o = place.origin;
    Rect::new(place.viewport.x0 - o[0], place.viewport.y0 - o[1], place.viewport.x1 - o[0], place.viewport.y1 - o[1])
        .intersect(&place.extent)
}

// Planning.

/// The map from a view's coordinates to window points, top-left origin.
fn to_window_top(view: &NSViewImpl, height: f64) -> Xf {
    views::to_window(view).then(&Xf { tx: 0.0, a: -1.0, ty: height })
}

/// Whether a view or one above it is hidden or not fully opaque: its
/// clip view then draws inline, where groups and hiding apply.
fn hidden_or_faded(view: &NSViewImpl) -> bool {
    let mut cur = Some(view);
    while let Some(v) = cur {
        if views::is_hidden(v) || views::alpha_of(v) < 1.0 {
            return true;
        }
        cur = views::superview_of(v);
    }
    false
}

/// Where a view is in paint order.
fn paint_path(view: &NSViewImpl) -> Vec<usize> {
    let mut path = Vec::with_capacity(8);
    let mut cur = view;
    while let Some(sup) = views::superview_of(cur) {
        path.push(views::index_of(sup, cur).unwrap_or(0));
        cur = sup;
    }
    path.reverse();
    path
}

/// A rectangle with its edges at the nearest whole points.
fn round(r: Rect) -> Rect {
    Rect::new(r.x0.round(), r.y0.round(), r.x1.round(), r.y1.round())
}

/// Decide which clip views get layers and where each shows.
fn plan(window: &NSWindowImpl) -> Vec<Plan> {
    let clips = window.clips();
    if clips.is_empty() {
        return Vec::new();
    }
    let scale = window.scale();
    let size = window.content_size();
    let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    let mut candidates: Vec<Plan> = Vec::new();
    for clip in clips {
        let c = views::imp(&clip);
        if !crate::scroll::has_document(c) || hidden_or_faded(c) {
            continue;
        }
        let top = to_window_top(c, size.height);
        let viewport = round(top.rect(crate::view_layout::visible_rect(c))).intersect(&bounds);
        let wide = |len: f32| len as f64 * scale >= MIN_VIEWPORT;
        if viewport.is_empty() || !wide(viewport.x1 - viewport.x0) || !wide(viewport.y1 - viewport.y0) {
            continue;
        }
        let lxf = layer_xf(c);
        let flipped = views::is_flipped(c);
        let mut extent: Option<Rect> = None;
        for sub in views::subviews(c).iter() {
            let s = views::imp(sub);
            if !views::is_hidden(s) {
                let r = views::step(s, flipped, views::frame(s)).then(&lxf).rect(views::bounds(s));
                extent = Some(extent.map_or(r, |e| e.union(&r)));
            }
        }
        let Some(extent) = extent.filter(|e| !e.is_empty()) else { continue };
        let b = views::bounds(c);
        let overflows =
            (extent.x1 - extent.x0) as f64 > b.size.width + 0.5 || (extent.y1 - extent.y0) as f64 > b.size.height + 0.5;
        if !overflows {
            continue;
        }
        // The layer's origin, on the device pixel grid.
        let (ox, oy) = top.point(0.0, 0.0);
        let snap = |v: f64| ((v * scale).round() / scale) as f32;
        let place = LayerPlace {
            z: [0, 0],
            viewport,
            origin: [snap(ox), snap(oy)],
            extent,
            grid: TileGrid::for_layer(&extent, scale),
            opaque: crate::scroll::opaque_background(c),
            parent: ROOT_LAYER,
            overlay: None,
        };
        candidates.push(Plan {
            id: views::layer_id(c),
            path: paint_path(c),
            parent: ROOT_LAYER,
            place,
            overlay_views: Vec::new(),
            clip,
        });
    }
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    // Parents come before what's nested in them.
    let mut plans: Vec<Plan> = Vec::with_capacity(candidates.len());
    for mut candidate in candidates {
        if plans.len() + 1 >= MAX_LAYERS {
            break;
        }
        let mut cur = views::superview_of(views::imp(&candidate.clip));
        while let Some(v) = cur {
            if views::is_clip(v) && plans.iter().any(|p| p.id == views::layer_id(v)) {
                candidate.parent = views::layer_id(v);
                break;
            }
            cur = views::superview_of(v);
        }
        candidate.place.parent = candidate.parent;
        plans.push(candidate);
    }
    stack(&mut plans);
    for i in 0..plans.len() {
        let (views, rect) = overlay_of(&plans[i], &plans, window);
        plans[i].overlay_views = views;
        plans[i].place.overlay = rect;
    }
    plans
}

/// Give each layer its place in the stacking: its tiles, the layers
/// nested in it, then its overlay.
fn stack(plans: &mut [Plan]) {
    fn visit(plans: &mut [Plan], i: usize, next: &mut u32) {
        plans[i].place.z[0] = *next;
        *next += 1;
        let id = plans[i].id;
        let children: Vec<usize> = (0..plans.len()).filter(|&j| plans[j].parent == id).collect();
        for j in children {
            visit(plans, j, next);
        }
        plans[i].place.z[1] = *next;
        *next += 1;
    }
    let mut next = 0;
    let roots: Vec<usize> = (0..plans.len()).filter(|&i| plans[i].parent == ROOT_LAYER).collect();
    for i in roots {
        visit(plans, i, &mut next);
    }
}

/// NSView's own `drawRect:`, which draws nothing.
fn plain_draw_rect() -> Option<Imp> {
    thread_local!(static PLAIN: Cell<Option<Imp>> = const { Cell::new(None) });
    PLAIN.with(|p| {
        if p.get().is_none() {
            p.set(draw_rect_of(NSView::class()));
        }
        p.get()
    })
}

fn draw_rect_of(class: &AnyClass) -> Option<Imp> {
    let sel: Sel = sel!(drawRect:);
    class.instance_method(sel).map(|m| m.implementation())
}

/// Whether a view's class draws with a `drawRect:` of its own rather than
/// NSView's, which draws nothing.
pub(crate) fn overrides_draw_rect(view: &NSViewImpl) -> bool {
    let class = views::as_view(view).class();
    !draw_rect_of(class).zip(plain_draw_rect()).is_some_and(|(a, b)| std::ptr::fn_addr_eq(a, b))
}

/// Whether a view draws nothing of its own and is fully opaque, so what it
/// holds can be looked at view by view.
fn draws_nothing(view: &NSViewImpl) -> bool {
    views::alpha_of(view) >= 1.0 && !overrides_draw_rect(view)
}

/// The views painted after a layer's clip view, in the layer it sits in,
/// that reach its viewport (or those already found): what its overlay
/// draws, and the overlay's rectangle, in the points of that layer.
fn overlay_of(plan: &Plan, plans: &[Plan], window: &NSWindowImpl) -> (Vec<Retained<NSView>>, Option<Rect>) {
    let height = window.content_height();
    let clip = views::imp(&plan.clip);
    let parent = plans.iter().find(|p| p.id == plan.parent);
    let stop = parent.map(|p| views::imp(&p.clip));
    let mut found = Vec::new();
    let mut region = plan.place.viewport;
    let mut child = clip;
    while let Some(sup) = views::superview_of(child) {
        let subviews = views::subviews(sup);
        let after = views::index_of(sup, child).map_or(subviews.len(), |i| i + 1);
        for view in &subviews[after..] {
            visit(views::imp(view), height, &mut region, &mut found);
        }
        if stop.is_some_and(|s| std::ptr::eq(s, sup)) {
            break;
        }
        child = sup;
    }
    // Their frames in that layer, whole, not cut by its viewport: the
    // overlay moves with the layer and keeps its pixels.
    let rect = found
        .iter()
        .filter_map(|v| layer_rect(views::imp(v), stop, height))
        .filter(|r| !r.is_empty())
        .map(|r| r.round_out())
        .reduce(|a, b| a.union(&b));
    // One too big to keep whole is cut to what the layer shows.
    let size = window.content_size();
    let too_big = |r: &Rect| f64::from((r.x1 - r.x0) * (r.y1 - r.y0)) > size.width * size.height;
    let rect = rect.map(|r| match parent {
        Some(p) if too_big(&r) => r.intersect(&visible_part(&p.place)).round_out(),
        _ => r,
    });
    (found, rect.filter(|r| !r.is_empty()))
}

/// Where `view`'s bounds are in the layer of `clip` (the window's surface
/// for none), cut only by its ancestors inside that layer: its frame there.
fn layer_rect(view: &NSViewImpl, clip: Option<&NSViewImpl>, height: f64) -> Option<Rect> {
    let mut chain = vec![view];
    loop {
        let cur = *chain.last().expect("chain");
        match views::superview_of(cur) {
            Some(sup) if clip.is_some_and(|c| std::ptr::eq(c, sup)) => break,
            Some(sup) => chain.push(sup),
            None if clip.is_some() => return None,
            None => break,
        }
    }
    let root = *chain.last().expect("chain");
    let mut xf = match clip {
        Some(c) => views::step(root, views::is_flipped(c), views::frame(root)).then(&layer_xf(c)),
        None => views::root_xf(root, height),
    };
    let mut r = xf.rect(views::bounds(root));
    for pair in chain.windows(2).rev() {
        let (child, parent) = (pair[0], pair[1]);
        xf = views::step(child, views::is_flipped(parent), views::frame(child)).then(&xf);
        r = r.intersect(&xf.rect(views::bounds(child)));
    }
    Some(r)
}

fn visit(view: &NSViewImpl, height: f64, region: &mut Rect, found: &mut Vec<Retained<NSView>>) {
    if views::is_hidden(view) {
        return;
    }
    let shown = to_window_top(view, height).rect(crate::view_layout::visible_rect(view)).round_out();
    if shown.is_empty() || shown.intersect(region).is_empty() {
        return;
    }
    if draws_nothing(view) && !views::is_clip(view) {
        for sub in views::subviews(view).iter() {
            visit(views::imp(sub), height, region, found);
        }
        return;
    }
    found.push(views::as_view(view).retain());
    *region = region.union(&shown);
}

// Drawing.

/// Merge damage rectangles that overlap or nearly touch, so each area is
/// drawn once.
pub(crate) fn coalesce(mut rects: Vec<Rect>) -> Vec<Rect> {
    rects.retain(|r| !r.is_empty());
    let area = |r: &Rect| (r.x1 - r.x0) * (r.y1 - r.y0);
    let mut merged = true;
    while merged && rects.len() > 1 {
        merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let u = rects[i].union(&rects[j]);
                // Merge when the union wastes little over drawing both.
                if area(&u) <= (area(&rects[i]) + area(&rects[j])) * 1.25 + 64.0 {
                    rects[i] = u;
                    rects.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    if rects.len() > 16 {
        let all = rects.iter().skip(1).fold(rects[0], |a, r| a.union(r));
        rects = vec![all];
    }
    rects
}

/// Record `view` and its subviews for `area` of a layer. `xf` maps the view
/// to the layer; `clip` is where its ancestors let it draw. `ring` is the
/// view whose focus ring shows (see `controls::focus`), drawn after its
/// subtree and clipped only by its ancestors.
pub(crate) fn record(view: &NSViewImpl, xf: Xf, clip: Rect, area: Rect, ring: Option<&NSView>) {
    let mode = MODE.with(Cell::get);
    if mode == Mode::Layer && crate::view_layout::in_overlay(view) {
        // Its overlay draws it.
        return;
    }
    let visible = clip.intersect(&xf.rect(views::bounds(view)));
    let target = visible.intersect(&area);
    let is_ring = ring.is_some_and(|r| std::ptr::eq(views::imp(r), view));
    if !target.is_empty() {
        // Its opacity, over its subviews too: nothing at 0, a group below 1.
        let Some(_opacity) = crate::context::opacity(view, target) else { return };
        // A fresh graphics state and the view's appearance for its drawRect:.
        let mark = crate::context::begin_view(view, xf, target);
        // SAFETY: drawRect: takes an NSRect.
        unsafe { msg_send![view, drawRect: xf.inverse_rect(target)] }
        crate::context::end_view(mark);
        // A promoted clip view's document is in its layer.
        if mode == Mode::Inline || !(views::is_clip(view) && promoted(view)) {
            let flipped = views::is_flipped(view);
            for sub in views::subviews(view) {
                let sub = views::imp(&sub);
                if views::is_hidden(sub) {
                    continue;
                }
                let sub_xf = views::step(sub, flipped, views::frame(sub)).then(&xf);
                record(sub, sub_xf, visible, area, ring);
            }
        }
    }
    if is_ring {
        let mark = crate::context::begin_view(view, xf, clip.intersect(&area));
        crate::controls::focus::draw_ring(views::as_view(view));
        crate::context::end_view(mark);
    }
}

/// Draw the damaged parts of the window's own surface.
fn draw_root(window: &NSWindowImpl, damage: &[Rect], scale: f64, ring: Option<&NSView>, counts: &mut Counts) {
    if damage.is_empty() {
        return;
    }
    let _mode = ModeGuard::set(Mode::Layer);
    let content = window.content();
    let color = window.background();
    let height = window.content_height();
    // User space's base is the window's content, origin at the bottom left.
    let base = Xf { tx: 0.0, a: -1.0, ty: height };
    let size = window.content_size();
    let all = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
    for area in coalesce(damage.to_vec()) {
        crate::context::begin_recording(base, scale);
        graphics::push(Op::Fill { rect: area, color });
        if let Some(content) = &content {
            let root = views::imp(content);
            let xf = views::root_xf(root, height);
            record(root, xf, all, area, ring);
        }
        let ops = graphics::end_recording();
        app::send(ToRender::Paint { window: window.id(), target: Target::Root, rects: vec![area], ops });
        counts.paints += 1;
    }
}

/// Record `area` (layer points) of a layer's tiles: cleared to its
/// background (or to nothing), then its clip view's subviews.
fn record_tiles(window: &NSWindowImpl, plan: &Plan, area: Rect, scale: f64, ring: Option<&NSView>) {
    let _mode = ModeGuard::set(Mode::Layer);
    let clip = views::imp(&plan.clip);
    let lxf = layer_xf(clip);
    crate::context::begin_recording(lxf, scale);
    let clear = match plan.place.opaque {
        Some(color) => Op::FillWith { rect: area, color, blend: crate::protocol::Blend::Copy },
        None => Op::FillWith { rect: area, color: [0.0; 4], blend: crate::protocol::Blend::Clear },
    };
    graphics::push(clear);
    let flipped = views::is_flipped(clip);
    let everywhere = Rect::new(f32::MIN, f32::MIN, f32::MAX, f32::MAX);
    for sub in views::subviews(clip) {
        let s = views::imp(&sub);
        if !views::is_hidden(s) {
            record(s, views::step(s, flipped, views::frame(s)).then(&lxf), everywhere, area, ring);
        }
    }
    let ops = graphics::end_recording();
    app::send(ToRender::Paint { window: window.id(), target: Target::Tiles(plan.id), rects: vec![area], ops });
}

/// Place a layer and draw what its viewport needs: tiles coming into view,
/// and the damaged parts of the tiles the render thread keeps.
fn draw_tiles(
    window: &NSWindowImpl,
    plan: &Plan,
    layer: &mut Layer,
    damage: &[Rect],
    scale: f64,
    ring: Option<&NSView>,
    counts: &mut Counts,
) {
    let place = plan.place;
    if layer.sent != Some(place) {
        if layer.sent.is_none_or(|s| s.grid != place.grid || s.opaque != place.opaque) {
            // The render thread starts this layer's tiles over.
            layer.valid.clear();
        }
        if layer.sent.is_none_or(|s| s.overlay != place.overlay || s.parent != place.parent) {
            layer.overlay_drawn = false;
        }
        app::send(ToRender::PlaceLayer { window: window.id(), layer: plan.id, place });
        layer.sent = Some(place);
        counts.placed += 1;
    }
    let grid = place.grid;
    let visible = visible_part(&place);
    let Some((columns, rows)) = grid.keys(&visible, scale) else { return };
    // Tiles well away from the viewport, or outside the layer, go.
    let within = grid.keys(&place.extent, scale);
    let keep = |k: &TileKey| {
        (columns.start() - KEEP..=columns.end() + KEEP).contains(&k[0])
            && (rows.start() - KEEP..=rows.end() + KEEP).contains(&k[1])
            && within.as_ref().is_some_and(|(c, r)| c.contains(&k[0]) && r.contains(&k[1]))
    };
    let mut far: Vec<TileKey> = layer.valid.iter().copied().filter(|k| !keep(k)).collect();
    // The damaged parts of the tiles kept. A tile out of view that is
    // damaged over half of it or more (a view drawn whole again) goes
    // instead, to be drawn if it comes into view.
    let in_view = |k: &TileKey| columns.contains(&k[0]) && rows.contains(&k[1]);
    let size = |r: &Rect| f64::from(r.x1 - r.x0) * f64::from(r.y1 - r.y0);
    let mut parts: Vec<(TileKey, Rect)> = Vec::new();
    for r in coalesce(damage.to_vec()) {
        for key in layer.valid.iter().filter(|k| keep(k)) {
            let whole = grid.padded(*key, scale);
            let part = r.intersect(&whole);
            if part.is_empty() {
                continue;
            }
            if !in_view(key) && size(&part) * 2.0 >= size(&whole) {
                far.push(*key);
            } else {
                parts.push((*key, part));
            }
        }
    }
    if !far.is_empty() {
        far.sort_unstable();
        far.dedup();
        for k in &far {
            layer.valid.remove(k);
        }
        parts.retain(|(key, _)| layer.valid.contains(key));
        app::send(ToRender::DropTiles { window: window.id(), layer: plan.id, tiles: far });
        counts.placed += 1;
    }
    let mut areas: Vec<Rect> = Vec::new();
    for row in rows.clone() {
        for column in columns.clone() {
            let key = [column, row];
            if layer.valid.insert(key) {
                areas.push(grid.padded(key, scale));
                counts.tiles += 1;
            }
        }
    }
    for (_, part) in parts {
        if !areas.iter().any(|a| a.intersect(&part) == part) {
            areas.push(part);
        }
    }
    for area in areas {
        record_tiles(window, plan, area, scale, ring);
        counts.paints += 1;
        note_tiles(layer, &area);
    }
}

/// Count a paint of a layer's tiles.
fn note_tiles(layer: &mut Layer, area: &Rect) {
    layer.painted.0 += 1;
    layer.painted.2 += f64::from((area.x1 - area.x0) * (area.y1 - area.y0));
}

/// Draw one tile ahead of the viewport, in the direction the layer is
/// scrolling (down, when it hasn't moved), after telling the document.
fn prefetch(
    window: &NSWindowImpl,
    plan: &Plan,
    layer: &mut Layer,
    scale: f64,
    ring: Option<&NSView>,
    counts: &mut Counts,
) {
    let place = plan.place;
    let grid = place.grid;
    let visible = visible_part(&place);
    let Some((columns, rows)) = grid.keys(&visible, scale) else { return };
    let Some((all_columns, all_rows)) = grid.keys(&place.extent, scale) else { return };
    let (dx, dy) = (visible.x0 - layer.visible.x0, visible.y0 - layer.visible.y0);
    let ahead: Vec<TileKey> = if dx.abs() > dy.abs() {
        let column = if dx < 0.0 { columns.start() - 1 } else { columns.end() + 1 };
        rows.map(|row| [column, row]).collect()
    } else {
        let row = if dy < 0.0 { rows.start() - 1 } else { rows.end() + 1 };
        columns.map(|column| [column, row]).collect()
    };
    let ahead: Vec<TileKey> = ahead
        .into_iter()
        .filter(|k| all_columns.contains(&k[0]) && all_rows.contains(&k[1]) && !layer.valid.contains(k))
        .collect();
    if ahead.is_empty() {
        return;
    }
    // The document gets ready to draw what comes next.
    let clip = views::imp(&plan.clip);
    if let Some(document) = crate::scroll::document_of(clip) {
        let lxf = layer_xf(clip);
        let area = ahead.iter().fold(visible, |a, k| a.union(&grid.rect(*k, scale)));
        let in_clip = lxf.inverse_rect(area);
        let rect: NSRect = document.convertRect_fromView(in_clip, Some(views::as_view(clip)));
        document.prepareContentInRect(rect);
    }
    for key in ahead {
        layer.valid.insert(key);
        let area = grid.padded(key, scale);
        record_tiles(window, plan, area, scale, ring);
        counts.tiles += 1;
        counts.paints += 1;
        note_tiles(layer, &area);
    }
}

/// Keep the window's tiles under [`MEMORY_CAP`]: drop the farthest from
/// their viewports first, never one in view.
fn evict(window: &NSWindowImpl, layers: &mut Layers, plans: &[Plan], scale: f64) {
    let bytes = |grid: &TileGrid| {
        let (.., w, h) = grid.pixels([0, 0]);
        w as usize * h as usize * 4
    };
    let mut total: usize = 0;
    let mut candidates: Vec<(f32, LayerId, TileKey, usize)> = Vec::new();
    for plan in plans {
        let Some(layer) = layers.layers.get(&plan.id) else { continue };
        let grid = plan.place.grid;
        let size = bytes(&grid);
        total += size * layer.valid.len();
        let visible = visible_part(&plan.place);
        let (cx, cy) = ((visible.x0 + visible.x1) / 2.0, (visible.y0 + visible.y1) / 2.0);
        for key in &layer.valid {
            let r = grid.rect(*key, scale);
            if !r.intersect(&visible).is_empty() {
                continue;
            }
            let d = ((r.x0 + r.x1) / 2.0 - cx).hypot((r.y0 + r.y1) / 2.0 - cy);
            candidates.push((d, plan.id, *key, size));
        }
    }
    let cap = CAP.with(Cell::get);
    if total <= cap {
        return;
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut dropped: HashMap<LayerId, Vec<TileKey>> = HashMap::new();
    for (_, id, key, size) in candidates {
        if total <= cap {
            break;
        }
        if let Some(layer) = layers.layers.get_mut(&id) {
            layer.valid.remove(&key);
            dropped.entry(id).or_default().push(key);
            total -= size;
        }
    }
    for (layer, tiles) in dropped {
        app::send(ToRender::DropTiles { window: window.id(), layer, tiles });
    }
}

/// Draw a layer's overlay: whole when its rectangle or its views changed,
/// else where the layer it sits in was damaged (`under`, that layer's
/// points). `base` maps user space's base to that layer.
#[allow(clippy::too_many_arguments)]
fn draw_overlay(
    window: &NSWindowImpl,
    plan: &Plan,
    layer: &mut Layer,
    under: &[Rect],
    base: Xf,
    scale: f64,
    ring: Option<&NSView>,
    counts: &mut Counts,
) {
    let Some(rect) = plan.place.overlay else { return };
    let areas = if layer.overlay_drawn {
        coalesce(under.iter().map(|r| r.intersect(&rect)).filter(|r| !r.is_empty()).collect())
    } else {
        vec![rect]
    };
    layer.overlay_drawn = true;
    if areas.is_empty() {
        return;
    }
    let _mode = ModeGuard::set(Mode::Overlay);
    for area in areas {
        crate::context::begin_recording(base, scale);
        graphics::push(Op::FillWith { rect: area, color: [0.0; 4], blend: crate::protocol::Blend::Clear });
        for view in &layer.overlay_views {
            let v = views::imp(view);
            match views::placement(v) {
                Some(p) if p.layer == plan.parent => record(v, p.xf, p.clip, area, ring),
                _ => {}
            }
        }
        let ops = graphics::end_recording();
        app::send(ToRender::Paint { window: window.id(), target: Target::Overlay(plan.id), rects: vec![area], ops });
        counts.paints += 1;
        layer.painted.1 += 1;
    }
}

/// The window's layers as the last pass left them, stacked bottom first
/// (see `testing::scroll_layers`).
pub(crate) fn describe(window: &NSWindowImpl) -> Vec<crate::testing::LayerInfo> {
    let layers = window.layers().borrow();
    let mut all: Vec<crate::testing::LayerInfo> = layers
        .layers
        .values()
        .filter_map(|layer| {
            let place = layer.sent?;
            let r = |r: Rect| [r.x0, r.y0, r.x1, r.y1];
            let mut tiles: Vec<TileKey> = layer.valid.iter().copied().collect();
            tiles.sort_unstable();
            Some(crate::testing::LayerInfo {
                clip: Retained::as_ptr(&layer.clip) as usize,
                parent: layer.parent as usize,
                z: place.z,
                viewport: r(place.viewport),
                origin: place.origin,
                extent: r(place.extent),
                tile_size: [place.grid.width, place.grid.height],
                opaque: place.opaque.is_some(),
                overlay: place.overlay.map(r),
                overlay_views: layer.overlay_views.iter().map(|v| Retained::as_ptr(v) as usize).collect(),
                tiles,
                tile_paints: layer.painted.0,
                overlay_paints: layer.painted.1,
                tile_area: layer.painted.2,
            })
        })
        .collect();
    all.sort_by_key(|l| l.z);
    all
}
