//! The render thread's side of Core Animation: a copy of the committed
//! layer trees, animated and composited without the main thread.
//!
//! **Commits.** Each `ToRender::Commit` replaces the changed layers whole
//! (properties, sublayers, animations, contents). A layer a view backs
//! has a canvas here, the view's drawing, painted by `Target::Content`
//! paints as a window's surface is. A transition animation new in a
//! commit first takes a copy of how its layer looked, which it fades
//! (or pushes, moves in, reveals) from.
//!
//! **Hosts.** A window's display pass records a layer tree where it
//! belongs in paint order as an `Op::Composite` naming its root layer (a
//! host: the topmost layer-backed view of a branch). Paints with
//! composites are retained, per target (the window's surface, a scroll
//! layer's tiles or overlay): each keeps the rectangles no later paint
//! has covered, so any part of them can be drawn again here. A composite
//! draws as the host's tree at the frame's time (`render::emit`).
//!
//! **Frames.** After a commit, a canvas paint, or a frame callback while
//! something animates, each host whose tree changed or animates is walked
//! as it shows now; a layer that looks different from last time damages
//! the box it and its sublayers covered and cover now, and the retained
//! paints are drawn again there. Damage no retained paint covers (a layer
//! animated outside where its host was recorded) goes to the main thread
//! to repaint. Nothing animating, no frames: an animation doesn't count
//! while its layer's time stands still (a speed of 0 on the way down),
//! before it begins (the main thread wakes the render thread then), or
//! where it can't show (under a hidden or transparent layer whose
//! visibility doesn't animate itself).

use std::collections::HashMap;
use std::sync::Arc;

use super::layer::{CALayerImpl, LayerId};
use super::props::{Kind, Props, compose};
use super::render::{self, Clip, Node, NodeContent};
use super::spec::{AnimSpec, Direction, TransitionKind, present, transition_progress};
use crate::protocol::{CompositeOp, Op, Rect, Target, ToRender, WindowId};
use crate::raster::images::{ImageData, Pixels};

/// What a commit says of a layer's contents.
#[derive(Clone)]
pub(crate) enum ContentUpdate {
    /// Unchanged since the last commit.
    Keep,
    None,
    /// An image: its pixels and size in points.
    Image(Arc<ImageData>, [f64; 2]),
    /// A view's canvas: the part of the layer's space it covers, its
    /// pixels' size, the scale. Its pixels come by `Target::Content`
    /// paints (kept where the new canvas overlaps the old).
    Canvas {
        rect: [f64; 4],
        width: u32,
        height: u32,
        scale: f64,
    },
}

/// A layer, whole, as a commit sends it.
pub(crate) struct LayerUpdate {
    pub id: LayerId,
    pub props: Props,
    pub sublayers: Vec<LayerId>,
    pub mask: Option<LayerId>,
    pub anims: Vec<Arc<AnimSpec>>,
    pub content: ContentUpdate,
}

pub(crate) struct Commit {
    pub updates: Vec<LayerUpdate>,
    /// Layers gone from the program.
    pub gone: Vec<LayerId>,
    /// The media time of the commit.
    pub time: f64,
    /// Windows whose layer trees wait for the main thread's next `Present`
    /// (it is in a display pass, or will draw a view's new canvas in the
    /// next).
    pub hold: Vec<WindowId>,
}

// The main thread's side.

thread_local! {
    /// The contents version each layer last sent (and whether a view backs
    /// it).
    static SENT_CONTENT: std::cell::RefCell<HashMap<LayerId, (u64, bool)>> = std::cell::RefCell::default();
}

/// A changed layer as the render thread takes it.
pub(crate) fn update_of(layer: &CALayerImpl) -> LayerUpdate {
    let id = layer.id();
    let (props, sublayers, mask, anims, version) = layer.read(|m| {
        (
            m.props.clone(),
            m.sublayers.iter().map(|s| super::layer::imp(s).id()).collect(),
            m.mask.as_ref().map(|s| super::layer::imp(s).id()),
            m.anims.iter().filter(|a| a.started).map(|a| a.spec.clone()).collect(),
            m.content_version,
        )
    });
    // Contents go again only when they changed: a view's canvas is kept by
    // its rectangle (the render thread keeps a canvas that stays the same),
    // images and nothing by the layer's contents version.
    let view = layer.view();
    let canvas = view.as_ref().is_some_and(|v| super::backing::draws_canvas(v));
    let content = match &view {
        Some(view) if canvas => super::backing::canvas_update(view, &props),
        _ => {
            let tag = (version, view.is_some());
            let changed = SENT_CONTENT.with(|s| s.borrow_mut().insert(id, tag) != Some(tag));
            if !changed {
                ContentUpdate::Keep
            } else {
                let image = match &view {
                    Some(view) => super::backing::given_contents(view, &props),
                    None => super::render::contents_image(layer),
                };
                match image {
                    Some((image, size)) => ContentUpdate::Image(image, size),
                    None => ContentUpdate::None,
                }
            }
        }
    };
    if canvas {
        SENT_CONTENT.with(|s| s.borrow_mut().remove(&id));
    }
    LayerUpdate { id, props, sublayers, mask, anims, content }
}

/// Wake the render thread (an animation begins now): an empty commit has
/// it draw what animates.
pub(crate) fn wake() {
    crate::app::send_if_running(ToRender::Commit(Box::new(Commit {
        updates: Vec::new(),
        gone: Vec::new(),
        time: super::math::media_now(),
        hold: Vec::new(),
    })));
}

/// Send a commit to the render thread (if it runs).
pub(crate) fn send_commit(updates: Vec<LayerUpdate>, gone: Vec<LayerId>, time: f64) {
    SENT_CONTENT.with(|s| {
        let mut s = s.borrow_mut();
        for id in &gone {
            s.remove(id);
        }
    });
    let hold = super::backing::take_holds();
    crate::app::send_if_running(ToRender::Commit(Box::new(Commit { updates, gone, time, hold })));
}

// The render thread's side.

/// A layer's canvas: premultiplied RGBA covering `rect` of its space.
struct Canvas {
    rect: [f64; 4],
    width: u32,
    height: u32,
    scale: f64,
    px: Arc<[u8]>,
    key: u64,
    generation: u64,
}

impl Canvas {
    fn image(&self) -> Arc<ImageData> {
        Arc::new(ImageData {
            key: self.key,
            generation: self.generation,
            width: self.width,
            height: self.height,
            pixels: Pixels::Rgba(self.px.clone()),
        })
    }
}

enum Content {
    None,
    Image(Arc<ImageData>, [f64; 2]),
    Canvas(Canvas),
}

struct RLayer {
    props: Props,
    sublayers: Vec<LayerId>,
    parent: Option<LayerId>,
    mask: Option<LayerId>,
    anims: Vec<Arc<AnimSpec>>,
    content: Content,
    /// Transitions running: the animation's id and how the layer looked
    /// when it began.
    transitions: Vec<(u64, Box<Node>)>,
    /// Counts changes of its contents.
    content_gen: u64,
}

/// A paint drawn again where it isn't covered by a later one.
struct Kept {
    target: Target,
    rects: Vec<Rect>,
    ops: Arc<Vec<Op>>,
}

/// What a host looked like at the last frame, by layer: its presented
/// properties, contents generation, and the box its tree covered.
#[derive(Default)]
struct HostState {
    last: HashMap<LayerId, (Props, u64, Rect)>,
    /// Changed by a commit or a paint since the last frame.
    dirty: bool,
    /// Something in it animated at the last frame: the next draws where
    /// it ended up, even with nothing animating any more.
    moving: bool,
}

#[derive(Default)]
struct WindowState {
    kept: Vec<Kept>,
    hosts: HashMap<(Target, LayerId), HostState>,
    /// Each host's composite, from the latest kept paint of its target
    /// that has one: the hosts with none are dropped.
    composites: HashMap<(Target, LayerId), Arc<CompositeOp>>,
}

impl WindowState {
    /// The kept paints changed: find each host's composite again, and
    /// forget the hosts no kept paint composites any more.
    fn reindex(&mut self) {
        self.composites.clear();
        for k in &self.kept {
            for op in k.ops.iter() {
                if let Op::Composite(c) = op {
                    self.composites.insert((k.target, c.root), c.clone());
                }
            }
        }
        let composites = &self.composites;
        self.hosts.retain(|key, _| composites.contains_key(key));
    }
}

/// Something to draw again: into `target`, `rects`, from `ops` (composites
/// already drawn out).
pub(crate) struct Redraw {
    pub target: Target,
    pub rects: Vec<Rect>,
    pub ops: Vec<Op>,
}

#[derive(Default)]
pub(crate) struct Compositor {
    layers: HashMap<LayerId, RLayer>,
    windows: HashMap<WindowId, WindowState>,
    /// Sublayers (and masks) named by a layer before they arrived, and the
    /// layer that named them.
    waiting: HashMap<LayerId, LayerId>,
    /// A fixed time to composite at (tests), else the clock.
    pub clock: Option<f64>,
}

impl Compositor {
    pub fn now(&self) -> f64 {
        self.clock.unwrap_or_else(super::math::media_now)
    }

    /// Take a commit in.
    pub fn apply(&mut self, commit: Commit) {
        let now = commit.time.max(self.now());
        // Transitions new in this commit copy how their layers look first.
        let mut before: Vec<(LayerId, u64, Node)> = Vec::new();
        for u in &commit.updates {
            let Some(old) = self.layers.get(&u.id) else { continue };
            for spec in &u.anims {
                if spec.is_transition()
                    && !old.anims.iter().any(|a| a.id == spec.id)
                    && let Some(node) = self.node(u.id, now, &[])
                {
                    before.push((u.id, spec.id, node));
                }
            }
        }
        let mut changed: Vec<LayerId> = Vec::with_capacity(commit.updates.len());
        for id in &commit.gone {
            if let Some(l) = self.layers.remove(id) {
                if let Content::Canvas(c) = &l.content {
                    crate::raster::images::forget(&[c.key]);
                }
                for sub in l.sublayers.iter().chain(l.mask.iter()) {
                    if let Some(s) = self.layers.get_mut(sub)
                        && s.parent == Some(*id)
                    {
                        s.parent = None;
                    }
                }
                changed.extend(l.parent);
            }
            self.waiting.remove(id);
        }
        for u in commit.updates {
            let fresh = !self.layers.contains_key(&u.id);
            let layer = self.layers.entry(u.id).or_insert_with(|| RLayer {
                props: Props::default(),
                sublayers: Vec::new(),
                parent: None,
                mask: None,
                anims: Vec::new(),
                content: Content::None,
                transitions: Vec::new(),
                content_gen: 0,
            });
            layer.props = u.props;
            let old_mask = std::mem::replace(&mut layer.mask, u.mask);
            // Transitions end with their animations.
            layer.transitions.retain(|(id, _)| u.anims.iter().any(|a| a.id == *id));
            layer.anims = u.anims;
            let old_subs = std::mem::replace(&mut layer.sublayers, u.sublayers.clone());
            match u.content {
                ContentUpdate::Keep => {}
                ContentUpdate::None => {
                    layer.content = Content::None;
                    layer.content_gen += 1;
                }
                ContentUpdate::Image(image, size) => {
                    layer.content = Content::Image(image, size);
                    layer.content_gen += 1;
                }
                ContentUpdate::Canvas { rect, width, height, scale } => {
                    let same = matches!(&layer.content, Content::Canvas(c) if c.rect == rect && c.width == width && c.height == height && c.scale == scale);
                    if !same {
                        let old = match std::mem::replace(&mut layer.content, Content::None) {
                            Content::Canvas(c) => Some(c),
                            _ => None,
                        };
                        layer.content = Content::Canvas(new_canvas(rect, width, height, scale, old.as_ref()));
                        layer.content_gen += 1;
                    }
                }
            }
            // A layer named by its parent before it arrived.
            if fresh && let Some(parent) = self.waiting.remove(&u.id) {
                layer.parent = Some(parent);
            }
            let now_subs: Vec<LayerId> = u.sublayers.iter().chain(u.mask.iter()).copied().collect();
            for sub in old_subs.iter().chain(old_mask.iter()) {
                if now_subs.contains(sub) {
                    continue;
                }
                if let Some(s) = self.layers.get_mut(sub)
                    && s.parent == Some(u.id)
                {
                    s.parent = None;
                }
                if self.waiting.get(sub) == Some(&u.id) {
                    self.waiting.remove(sub);
                }
            }
            for sub in now_subs {
                match self.layers.get_mut(&sub) {
                    Some(s) => s.parent = Some(u.id),
                    None => {
                        self.waiting.insert(sub, u.id);
                    }
                }
            }
            changed.push(u.id);
        }
        self.touched(&changed);
        for (id, anim, node) in before {
            if let Some(l) = self.layers.get_mut(&id) {
                l.transitions.push((anim, Box::new(node)));
            }
        }
    }

    /// `ids` changed: the hosts they're under redraw at the next frame.
    fn touched(&mut self, ids: &[LayerId]) {
        if ids.is_empty() {
            return;
        }
        // Each changed layer and its ancestors, each walked up once.
        let mut above: std::collections::HashSet<LayerId> = std::collections::HashSet::new();
        for id in ids {
            let mut cur = Some(*id);
            while let Some(c) = cur {
                if !above.insert(c) {
                    break;
                }
                cur = self.layers.get(&c).and_then(|l| l.parent);
            }
        }
        for w in self.windows.values_mut() {
            for ((_, host), state) in w.hosts.iter_mut() {
                if above.contains(host) {
                    state.dirty = true;
                }
            }
        }
    }

    /// A paint of a layer's canvas.
    pub fn paint_content(&mut self, id: LayerId, rects: &[Rect], ops: &[Op], glyphs: &mut crate::raster::Glyphs) {
        let Some(layer) = self.layers.get_mut(&id) else { return };
        let Content::Canvas(c) = &mut layer.content else { return };
        crate::raster::images::forget(&[c.key]);
        if Arc::get_mut(&mut c.px).is_none() {
            c.px = Arc::from(&c.px[..]);
        }
        let Some(bytes) = Arc::get_mut(&mut c.px) else { return };
        let Some(px) = words(bytes) else { return };
        // Canvas points: from the canvas's top left.
        let mut canvas = crate::raster::Canvas::new(px, c.width, c.height, 0.0, c.scale as f32);
        crate::raster::paint(&mut canvas, glyphs, rects, ops);
        c.generation += 1;
        layer.content_gen += 1;
        self.touched(&[id]);
    }

    /// A paint for `target` of a window: kept if it composites a layer
    /// tree (and trimming what earlier kept paints it covers), and
    /// returned with its composites drawn out at the frame's time.
    pub fn paint(&mut self, window: WindowId, target: Target, rects: &[Rect], ops: Vec<Op>) -> Vec<Op> {
        let hosts: Vec<(LayerId, Arc<CompositeOp>)> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Composite(c) => Some((c.root, c.clone())),
                _ => None,
            })
            .collect();
        let w = self.windows.entry(window).or_default();
        // Later paints cover earlier ones where they draw.
        let before = w.kept.len();
        for kept in w.kept.iter_mut().filter(|k| k.target == target) {
            kept.rects = kept.rects.iter().flat_map(|k| subtract(*k, rects)).collect();
        }
        w.kept.retain(|k| !k.rects.is_empty());
        if hosts.is_empty() {
            if w.kept.len() != before {
                w.reindex();
            }
            return ops;
        }
        let ops = Arc::new(ops);
        w.kept.push(Kept { target, rects: rects.to_vec(), ops: ops.clone() });
        w.reindex();
        for (host, _) in &hosts {
            w.hosts.entry((target, *host)).or_default().dirty = true;
        }
        let now = self.now();
        let expanded = self.expand(&ops, now);
        // What was drawn is what the host looks like now.
        for (host, c) in &hosts {
            let snapshot = self.snapshot(*host, c, now);
            if let Some(w) = self.windows.get_mut(&window)
                && let Some(state) = w.hosts.get_mut(&(target, *host))
            {
                state.last = snapshot;
                state.dirty = false;
            }
        }
        expanded
    }

    /// A scroll layer (and its paints) is gone.
    pub fn drop_target(&mut self, window: WindowId, target: Target) {
        if let Some(w) = self.windows.get_mut(&window) {
            w.kept.retain(|k| k.target != target);
            w.reindex();
        }
    }

    /// Parts of a target are gone (a scroll layer's tiles the main thread
    /// dropped): the kept paints no longer draw there.
    pub fn drop_rects(&mut self, window: WindowId, target: Target, rects: &[Rect]) {
        if let Some(w) = self.windows.get_mut(&window) {
            for kept in w.kept.iter_mut().filter(|k| k.target == target) {
                kept.rects = kept.rects.iter().flat_map(|k| subtract(*k, rects)).collect();
            }
            let before = w.kept.len();
            w.kept.retain(|k| !k.rects.is_empty());
            if w.kept.len() != before {
                w.reindex();
            }
        }
    }

    pub fn drop_window(&mut self, window: WindowId) {
        self.windows.remove(&window);
    }

    /// `ops` with each composite drawn out at time `t`.
    fn expand(&self, ops: &[Op], t: f64) -> Vec<Op> {
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            match op {
                Op::Composite(c) => self.composite(c, t, &mut out),
                op => out.push(op.clone()),
            }
        }
        out
    }

    fn composite(&self, c: &CompositeOp, t: f64, out: &mut Vec<Op>) {
        let Some(node) = self.node(c.root, t, &c.skip) else { return };
        let clip = Clip { rect: c.clip, paths: c.mask.as_deref().map(<[_]>::to_vec).unwrap_or_default() };
        render::emit(&node, &c.base, c.down, &clip, out);
    }

    /// The layer's time for the media time `t`.
    fn local(&self, id: LayerId, t: f64) -> f64 {
        let mut chain = Vec::new();
        let mut cur = Some(id);
        while let Some(i) = cur {
            let Some(l) = self.layers.get(&i) else { break };
            chain.push(l.props.time);
            cur = l.parent;
            if chain.len() > 4096 {
                break;
            }
        }
        chain.iter().rev().fold(t, |t, time| time.local(t))
    }

    /// The layer and its sublayers as they show at `t`, leaving out `skip`.
    fn node(&self, id: LayerId, t: f64, skip: &[LayerId]) -> Option<Node> {
        let local = self.local(id, t);
        self.node_at(id, local, skip, 0)
    }

    fn node_at(&self, id: LayerId, local: f64, skip: &[LayerId], depth: usize) -> Option<Node> {
        if depth > 256 {
            return None;
        }
        let l = self.layers.get(&id)?;
        let mut props = l.props.clone();
        present(&mut props, &l.anims, local);
        let content = match &l.content {
            Content::None => NodeContent::None,
            Content::Image(image, size) => NodeContent::Image { image: image.clone(), size: *size },
            Content::Canvas(c) => NodeContent::Canvas { image: c.image(), rect: c.rect },
        };
        let children = l
            .sublayers
            .iter()
            .filter(|s| !skip.contains(s))
            .filter_map(|s| {
                let sl = self.layers.get(s)?;
                self.node_at(*s, sl.props.time.local(local), skip, depth + 1)
            })
            .collect();
        let mask = l.mask.and_then(|m| {
            let ml = self.layers.get(&m)?;
            self.node_at(m, ml.props.time.local(local), skip, depth + 1)
        });
        // The latest transition running shows.
        let transition = l.transitions.iter().rev().find_map(|(anim, old)| {
            let spec = l.anims.iter().find(|a| a.id == *anim)?;
            let (p, kind, dir) = transition_progress(spec, local)?;
            Some((old.clone(), p, kind, dir))
        });
        Some(Node { props, content, children, mask: mask.map(Box::new), transition })
    }

    /// Whether anything under `id` animates at the time its superlayer's
    /// is `parent` (`frozen`: an ancestor's time stands still): an
    /// animation that has begun and not ended, where it can show.
    fn animating(&self, id: LayerId, parent: f64, frozen: bool, skip: &[LayerId], depth: usize) -> bool {
        let Some(l) = self.layers.get(&id) else { return false };
        if depth > 256 {
            return false;
        }
        let local = l.props.time.local(parent);
        let frozen = frozen || l.props.time.speed == 0.0;
        // A hidden or transparent layer shows nothing of its tree, unless
        // that animates itself (a transition runs, or its visibility).
        let shows = |key| l.anims.iter().any(|a| a.is_transition() || a.touches(key));
        if (l.props.hidden && !shows(super::props::Key::Hidden))
            || (l.props.opacity <= 0.0 && !shows(super::props::Key::Opacity))
        {
            return false;
        }
        if !frozen
            && l.anims
                .iter()
                .any(|a| a.timing.speed != 0.0 && local >= a.timing.begin && a.end().is_none_or(|end| local <= end))
        {
            return true;
        }
        l.sublayers
            .iter()
            .chain(l.mask.iter())
            .filter(|s| !skip.contains(s))
            .any(|s| self.animating(*s, local, frozen, skip, depth + 1))
    }

    /// Whether a host's tree animates at the media time `t`.
    fn host_animating(&self, host: LayerId, t: f64, skip: &[LayerId]) -> bool {
        // The time and stillness of the host's superlayer.
        let mut chain = Vec::new();
        let mut cur = self.layers.get(&host).and_then(|l| l.parent);
        while let Some(i) = cur {
            let Some(l) = self.layers.get(&i) else { break };
            chain.push(l.props.time);
            cur = l.parent;
            if chain.len() > 4096 {
                break;
            }
        }
        let frozen = chain.iter().any(|time| time.speed == 0.0);
        let parent = chain.iter().rev().fold(t, |t, time| time.local(t));
        self.animating(host, parent, frozen, skip, 0)
    }

    /// Whether a window has something to draw at the next frame.
    pub fn wants_frame(&self, window: WindowId) -> bool {
        let t = self.now();
        let Some(w) = self.windows.get(&window) else { return false };
        w.hosts.iter().any(|(key, state)| {
            let Some(c) = w.composites.get(key) else { return false };
            state.dirty || state.moving || self.host_animating(key.1, t, &c.skip)
        })
    }

    /// Draw a window's frame at `t`: the damage of each host that changed
    /// or animates, redrawn from the kept paints. Returns what to draw
    /// again, and the damage no kept paint covers (for the main thread).
    pub fn frame(&mut self, window: WindowId, t: f64) -> (Vec<Redraw>, Vec<(Target, Rect)>) {
        let Some(w) = self.windows.get(&window) else { return (Vec::new(), Vec::new()) };
        let keys: Vec<((Target, LayerId), Arc<CompositeOp>)> =
            w.hosts.keys().filter_map(|k| w.composites.get(k).map(|c| (*k, c.clone()))).collect();
        let mut damage: Vec<(Target, Rect)> = Vec::new();
        for ((target, host), c) in keys {
            let (dirty, moving) = self
                .windows
                .get(&window)
                .and_then(|w| w.hosts.get(&(target, host)))
                .map_or((false, false), |s| (s.dirty, s.moving));
            let animating = self.host_animating(host, t, &c.skip);
            if !dirty && !animating && !moving {
                continue;
            }
            let now = self.snapshot(host, &c, t);
            let Some(state) = self.windows.get_mut(&window).and_then(|w| w.hosts.get_mut(&(target, host))) else {
                continue;
            };
            let last = std::mem::take(&mut state.last);
            state.dirty = false;
            state.moving = animating;
            for (id, (props, generation, bx)) in &now {
                match last.get(id) {
                    Some((p, g, b)) if p == props && g == generation && b == bx => {}
                    Some((_, _, b)) => {
                        damage.push((target, *b));
                        damage.push((target, *bx));
                    }
                    None => damage.push((target, *bx)),
                }
            }
            for (id, (_, _, b)) in &last {
                if !now.contains_key(id) {
                    damage.push((target, *b));
                }
            }
            // Clipped to where the host may draw.
            damage.iter_mut().filter(|(t, _)| *t == target).for_each(|(_, r)| *r = r.intersect(&c.clip).round_out());
            if let Some(state) = self.windows.get_mut(&window).and_then(|w| w.hosts.get_mut(&(target, host))) {
                state.last = now;
            }
        }
        damage.retain(|(_, r)| !r.is_empty());
        if damage.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut redraws = Vec::new();
        let mut uncovered = Vec::new();
        let Some(w) = self.windows.get(&window) else { return (Vec::new(), Vec::new()) };
        let mut targets: Vec<Target> = Vec::new();
        for (t, _) in &damage {
            if !targets.contains(t) {
                targets.push(*t);
            }
        }
        for target in targets {
            let rects: Vec<Rect> = coalesce(damage.iter().filter(|(t, _)| *t == target).map(|(_, r)| *r).collect());
            // What the kept paints cover of the damage, and what they don't.
            let mut left: Vec<Rect> = rects.clone();
            for kept in w.kept.iter().filter(|k| k.target == target) {
                let parts: Vec<Rect> = kept
                    .rects
                    .iter()
                    .flat_map(|k| rects.iter().map(move |r| k.intersect(r)))
                    .filter(|r| !r.is_empty())
                    .collect();
                if parts.is_empty() {
                    continue;
                }
                left = left.iter().flat_map(|l| subtract(*l, &kept.rects)).collect();
                redraws.push(Redraw { target, rects: parts, ops: self.expand(&kept.ops, t) });
            }
            uncovered.extend(left.into_iter().filter(|r| !r.is_empty()).map(|r| (target, r)));
        }
        (redraws, uncovered)
    }

    /// The presented properties, contents generation and tree box of each
    /// layer under `host`, as its composite `c` draws them at `t`.
    fn snapshot(&self, host: LayerId, c: &CompositeOp, t: f64) -> HashMap<LayerId, (Props, u64, Rect)> {
        let mut out = HashMap::new();
        let local = self.local(host, t);
        self.walk(host, local, &c.base, &c.skip, &mut out, 0);
        out
    }

    /// Record `id`'s state and return the box its tree covers (target
    /// points).
    fn walk(
        &self,
        id: LayerId,
        local: f64,
        parent: &[f64; 6],
        skip: &[LayerId],
        out: &mut HashMap<LayerId, (Props, u64, Rect)>,
        depth: usize,
    ) -> Option<Rect> {
        if depth > 256 {
            return None;
        }
        let l = self.layers.get(&id)?;
        let mut props = l.props.clone();
        present(&mut props, &l.anims, local);
        let m = compose(&props.to_superlayer(), parent);
        let mut bx = own_box(&props, &m, parent);
        let transition = l.transitions.iter().rev().find_map(|(anim, _)| {
            l.anims.iter().find(|a| a.id == *anim).and_then(|spec| transition_progress(spec, local))
        });
        let transitioning = transition.is_some();
        if !props.hidden || transitioning {
            let sub = render::sublayer_map(&props, &m);
            for s in l.sublayers.iter().chain(l.mask.iter()).filter(|s| !skip.contains(s)) {
                let Some(sl) = self.layers.get(s) else { continue };
                if let Some(b) = self.walk(*s, sl.props.time.local(local), &sub, skip, out, depth + 1)
                    && !props.masks_to_bounds
                {
                    bx = bx.union(&b);
                }
            }
        }
        // A transition fades or moves the whole of it, so it changes every
        // frame; a move reaches a width (or height) beyond, along its axis.
        let mut generation = l.content_gen;
        if let Some((_, kind, direction)) = transition {
            generation = generation.wrapping_add((local * 1e6) as u64);
            let (w, h) = ((bx.x1 - bx.x0), (bx.y1 - bx.y0));
            let turned = m[1].abs() > 1e-9 || m[2].abs() > 1e-9;
            let (w, h) = match (kind, direction) {
                (TransitionKind::Fade, _) => (0.0, 0.0),
                _ if turned => (w, h),
                (_, Direction::Left | Direction::Right) => (w, 0.0),
                (_, Direction::Top | Direction::Bottom) => (0.0, h),
            };
            bx = Rect::new(bx.x0 - w, bx.y0 - h, bx.x1 + w, bx.y1 + h);
        }
        out.insert(id, (props, generation, bx));
        Some(bx)
    }

    /// For tests: a layer's presented properties at `t`.
    pub fn presented(&self, id: LayerId, t: f64) -> Option<Props> {
        let l = self.layers.get(&id)?;
        let mut props = l.props.clone();
        present(&mut props, &l.anims, self.local(id, t));
        Some(props)
    }

    /// For tests: how many layers the render thread holds.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

/// The box a layer draws in itself (target points): its bounds, a shape
/// layer's path and stroke, and its shadow.
fn own_box(p: &Props, m: &[f64; 6], parent: &[f64; 6]) -> Rect {
    let mut r = p.bounds;
    if let Kind::Shape(s) = &p.kind
        && let Some(shape) = &s.path
        && let Some(b) = crate::path::tight_bounds(&shape.path)
    {
        // A stroke reaches half its width out, a square cap's corner
        // further, and a miter join up to the miter limit.
        let reach = match (s.line_join, s.line_cap) {
            (0, _) => s.miter_limit.max(std::f64::consts::SQRT_2),
            (_, 2) => std::f64::consts::SQRT_2,
            _ => 1.0,
        };
        let pad = if s.stroke_color.is_some() { s.line_width.max(0.0) * reach / 2.0 } else { 0.0 };
        let (x0, y0) = (r[0].min(b.x0 - pad), r[1].min(b.y0 - pad));
        let (x1, y1) = ((r[0] + r[2]).max(b.x1 + pad), (r[1] + r[3]).max(b.y1 + pad));
        r = [x0, y0, x1 - x0, y1 - y0];
    }
    let mut bx = render::map_bounds(m, r);
    if p.shadow_opacity > 0.0 && p.shadow_color.is_some_and(|c| c[3] > 0.0) {
        let [ox, oy] = p.shadow_offset;
        let dx = (ox * parent[0] + oy * parent[2]) as f32;
        let dy = (ox * parent[1] + oy * parent[3]) as f32;
        let scale = (parent[0] * parent[3] - parent[1] * parent[2]).abs().sqrt();
        let blur = (3.0 * p.shadow_radius.max(0.0) * scale) as f32 + 2.0;
        let s = Rect::new(bx.x0 + dx - blur, bx.y0 + dy - blur, bx.x1 + dx + blur, bx.y1 + dy + blur);
        bx = bx.union(&s);
    }
    // Antialiased edges reach a pixel out.
    Rect::new(bx.x0 - 1.0, bx.y0 - 1.0, bx.x1 + 1.0, bx.y1 + 1.0)
}

/// A new canvas, keeping the pixels of `old` where the two overlap (a
/// view resized keeps what it drew, as its layer's contents would).
fn new_canvas(rect: [f64; 4], width: u32, height: u32, scale: f64, old: Option<&Canvas>) -> Canvas {
    let mut px = vec![0u8; width as usize * height as usize * 4];
    if let Some(old) = old
        && old.scale == scale
    {
        // Rows and columns in common, by the canvases' top-left corners,
        // a row at a time.
        let dx = ((old.rect[0] - rect[0]) * scale).round() as i64;
        let dy = ((old.rect[1] - rect[1]) * scale).round() as i64;
        let x0 = 0.max(-dx);
        let x1 = (old.width as i64).min(width as i64 - dx);
        if x1 > x0 {
            let len = (x1 - x0) as usize * 4;
            for y in 0.max(-dy)..(old.height as i64).min(height as i64 - dy) {
                let s = (y as usize * old.width as usize + x0 as usize) * 4;
                let d = ((y + dy) as usize * width as usize + (x0 + dx) as usize) * 4;
                px[d..d + len].copy_from_slice(&old.px[s..s + len]);
            }
        }
    }
    Canvas { rect, width, height, scale, px: Arc::from(px), key: crate::raster::images::next_key(), generation: 0 }
}

/// Bytes as pixels, when they're aligned as pixels (allocations are).
fn words(bytes: &mut [u8]) -> Option<&mut [u32]> {
    if !bytes.as_ptr().cast::<u32>().is_aligned() {
        return None;
    }
    // SAFETY: aligned, as checked; four bytes to a u32, any bit pattern
    // valid.
    Some(unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<u32>(), bytes.len() / 4) })
}

/// `r` less every rectangle of `cut`, as rectangles.
pub(crate) fn subtract(r: Rect, cut: &[Rect]) -> Vec<Rect> {
    let mut parts = vec![r];
    for c in cut {
        let mut next = Vec::with_capacity(parts.len() + 3);
        for p in parts {
            let i = p.intersect(c);
            if i.is_empty() {
                next.push(p);
                continue;
            }
            // Above, below, left and right of the cut.
            let pieces = [
                Rect::new(p.x0, p.y0, p.x1, i.y0),
                Rect::new(p.x0, i.y1, p.x1, p.y1),
                Rect::new(p.x0, i.y0, i.x0, i.y1),
                Rect::new(i.x1, i.y0, p.x1, i.y1),
            ];
            next.extend(pieces.into_iter().filter(|q| !q.is_empty()));
        }
        parts = next;
    }
    parts
}

/// Damage rectangles, the overlapping ones merged, at most 16 (their
/// union past that). Unlike `layers::coalesce`, which also merges nearby
/// rectangles that don't overlap and may leave overlapping ones apart, no
/// two of these overlap: a redraw paints its ops into each rectangle over
/// what is there, so an overlap would be drawn twice.
fn coalesce(mut rects: Vec<Rect>) -> Vec<Rect> {
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if !rects[i].intersect(&rects[j]).is_empty() {
                    rects[i] = rects[i].union(&rects[j]);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtracting_leaves_what_is_outside() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        let left = subtract(r, &[Rect::new(2.0, 2.0, 8.0, 8.0)]);
        let area: f32 = left.iter().map(|q| (q.x1 - q.x0) * (q.y1 - q.y0)).sum();
        assert_eq!(area, 100.0 - 36.0);
        assert!(subtract(r, &[Rect::new(-1.0, -1.0, 11.0, 11.0)]).is_empty());
        assert_eq!(subtract(r, &[Rect::new(20.0, 20.0, 30.0, 30.0)]), vec![r]);
    }
}
