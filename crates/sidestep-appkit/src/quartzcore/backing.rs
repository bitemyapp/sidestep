//! Views and their layers.
//!
//! As on macOS, a view is layer-backed when it wants a layer
//! (`setWantsLayer:`), or its superview is: then it has a backing layer
//! (`makeBackingLayer`, a plain `CALayer` unless overridden) whose delegate
//! is the view, and the view keeps the layer's geometry to its own: the
//! anchor point at (0, 0), the position the frame's origin, the bounds its
//! bounds (so the layer's frame is the view's frame, measured; a bounds
//! size other than the frame's is the layer's transform, a scale), the
//! geometry flipped where the view's flippedness differs from its
//! superview's, hidden, opacity and clipping (`masksToBounds`) its own. A
//! view given a layer with `setLayer:` hosts it: the view keeps its
//! geometry the same way. A layer-backed view's layer holds, in order, the
//! sublayers the program added and then its subviews' layers in their
//! order, as AppKit orders them whenever the subviews change. A view that
//! leaves its superview keeps its layer until it joins a tree that isn't
//! layer-backed (measured). The view answers `NSNull` for every action (so
//! its layer's changes don't animate implicitly), unless an animation
//! context allows implicit animation: then the changes of the view's own
//! frame and alpha animate its layer too.
//!
//! **Drawing.** In a window's display pass a layer-backed view is
//! composited rather than drawn in place: the topmost one of a branch
//! records an `Op::Composite` where its tree belongs (`layers::record`),
//! and each view's own drawing (`drawRect:`) goes into its layer's canvas
//! on the render thread (`Target::Content` paints, in the layer's points
//! from the view's top left), redrawn where the view was invalidated. A
//! view that `wantsUpdateLayer` (as a plain view does, measured) gets
//! `updateLayer` instead, when its layer displays at a commit (as on
//! macOS: the first commit that finds it in a window, and each after its
//! view or layer needed display), and shows its layer's contents. What the
//! render thread composites is the layer tree (`quartzcore::tree`), so a
//! view's layer animates there. A scroll layer's document (`layers`) isn't
//! composited: its tiles are its backing store, as the document's layer is
//! tiled on macOS; its layer-backed subviews are, each where it is in the
//! tiles.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use objc2::rc::{Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, Message, msg_send};
use objc2_app_kit::{NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::CALayer;

use super::layer::{self, LayerId, imp};
use super::props::{Key, Props};
use super::render::NodeContent;
use super::tree::ContentUpdate;
use crate::protocol::{Op, Rect, Target, ToRender, WindowId};
use crate::views::{self, NSViewImpl};

/// The most pixels a view's canvas holds either way; bigger views keep
/// the part of their bounds near what shows.
const CANVAS_MAX: f64 = 4096.0;

/// The grid (points) a big view's canvas is placed on, so scrolling moves
/// it only now and then.
const CANVAS_GRID: f64 = 512.0;

/// A view's layer state, kept in its ivars.
#[derive(Default)]
pub(crate) struct ViewLayer {
    layer: RefCell<Option<Retained<CALayer>>>,
    /// The layer was given with `setLayer:` (the view hosts it).
    hosted: Cell<bool>,
    /// Where the view's drawing is out of date in its canvas (view
    /// coordinates).
    damage: RefCell<Vec<NSRect>>,
    /// Waiting in the list of views whose canvases are out of date.
    pending: Cell<bool>,
    /// The canvas's rectangle when last sent (layer points).
    canvas: Cell<Option<[f64; 4]>>,
    /// The layer's transform is a scale this view set (its bounds size
    /// differs from its frame's).
    scaled: Cell<bool>,
    /// `canDrawSubviewsIntoLayer` and `layerUsesCoreImageFilters`, kept as
    /// given (each subview draws into its own layer's canvas regardless,
    /// and there are no Core Image filters).
    pub(crate) draws_subviews: Cell<bool>,
    pub(crate) uses_filters: Cell<bool>,
}

fn state(view: &NSViewImpl) -> &ViewLayer {
    &view.ivars().layer
}

/// The view's layer, if it has one.
pub(crate) fn layer_of(view: &NSViewImpl) -> Option<Retained<CALayer>> {
    state(view).layer.borrow().clone()
}

/// Whether `view` is layer-backed: it wants a layer, hosts one, or its
/// superview is layer-backed.
pub(crate) fn backed(view: &NSViewImpl) -> bool {
    let mut cur = Some(view);
    while let Some(v) = cur {
        if crate::view_layout::wants_layer(v) || state(v).hosted.get() {
            return true;
        }
        cur = views::superview_of(v);
    }
    false
}

// NSView's methods.

/// `-[NSView layer]`.
pub(crate) fn layer(view: &NSViewImpl) -> Option<Retained<CALayer>> {
    if let Some(l) = layer_of(view) {
        return Some(l);
    }
    if backed(view) {
        ensure(view);
        return layer_of(view);
    }
    None
}

/// `-[NSView setLayer:]`: the view hosts `layer` (or drops its own).
pub(crate) fn set_layer(view: &NSViewImpl, new: Option<&CALayer>) {
    let old = state(view).layer.borrow_mut().take();
    if let Some(old) = &old {
        imp(old).set_view(None);
        layer::remove_from_superlayer(imp(old));
    }
    state(view).hosted.set(new.is_some());
    state(view).scaled.set(false);
    if let Some(l) = new {
        *state(view).layer.borrow_mut() = Some(l.retain());
        imp(l).set_view(Some(views::as_view(view)));
        sync_geometry(view);
        attach(view);
        sync_sublayers(view);
    }
    drop(old);
    views::invalidate_reach(view);
}

/// `setWantsLayer:` changed.
pub(crate) fn wants_layer_changed(view: &NSViewImpl) {
    refresh(view);
    views::invalidate_reach(view);
}

/// Make `view`'s backing layer (and its subviews'), or drop them, as its
/// being layer-backed says.
fn refresh(view: &NSViewImpl) {
    if backed(view) {
        ensure(view);
    } else if !state(view).hosted.get() {
        let old = state(view).layer.borrow_mut().take();
        if let Some(old) = &old {
            imp(old).set_view(None);
            layer::remove_from_superlayer(imp(old));
        }
        drop(old);
    }
    for sub in views::subviews(view) {
        refresh(views::imp(&sub));
    }
}

/// Give a layer-backed view its backing layer, set up, if it has none.
fn ensure(view: &NSViewImpl) {
    if layer_of(view).is_some() {
        return;
    }
    // SAFETY: makeBackingLayer returns a new layer.
    let l: Retained<CALayer> = unsafe { msg_send![views::as_view(view), makeBackingLayer] };
    let li = imp(&l);
    li.set_view(Some(views::as_view(view)));
    li.write(|m| {
        m.props.anchor = [0.0, 0.0];
        m.props.contents_scale = views::window_of(view).map_or(1.0, |w| w.scale());
        m.delegate = super::objects::weak_of(Some::<&AnyObject>(views::as_view(view)));
        m.has_delegate = true;
        // It displays at the first commit that finds it in a window.
        m.needs_display = true;
    });
    *state(view).layer.borrow_mut() = Some(l);
    sync_geometry(view);
    attach(view);
    for sub in views::subviews(view) {
        ensure(views::imp(&sub));
    }
    sync_sublayers(view);
    set_needs_display(view, crate::view_layout::bounds(view));
}

/// Put the view's layer into its superview's layer, where AppKit orders it.
fn attach(view: &NSViewImpl) {
    if let Some(sup) = views::superview_of(view)
        && layer_of(sup).is_some()
    {
        sync_sublayers(sup);
    }
}

/// Order a layer-backed view's layer's sublayers as AppKit does: those
/// the program added, as they are, then its subviews' layers in their
/// order.
pub(crate) fn sync_sublayers(view: &NSViewImpl) {
    let Some(l) = layer_of(view) else { return };
    let li = imp(&l);
    let subs: Vec<Retained<CALayer>> = views::subviews(view).iter().filter_map(|s| layer_of(views::imp(s))).collect();
    let current = li.read(|m| m.sublayers.clone());
    let own: Vec<Retained<CALayer>> = current.iter().filter(|c| !imp(c).has_view()).cloned().collect();
    let mut wanted = own;
    wanted.extend(subs.iter().cloned());
    let same = wanted.len() == current.len() && wanted.iter().zip(&current).all(|(a, b)| std::ptr::eq(&**a, &**b));
    if same {
        return;
    }
    // Sublayers of view layers leave their old places first.
    for s in &subs {
        let sup = imp(s).read(|m| m.superlayer);
        if sup.is_some_and(|p| !std::ptr::eq(p.as_ptr(), &*l)) {
            layer::remove_from_superlayer(imp(s));
        }
    }
    let gone: Vec<Retained<CALayer>> = current
        .iter()
        .filter(|c| imp(c).has_view() && !subs.iter().any(|s| std::ptr::eq(&**s, &***c)))
        .cloned()
        .collect();
    li.write(|m| m.sublayers = wanted.clone());
    for s in &gone {
        imp(s).write(|m| m.superlayer = None);
        super::transaction::detached(imp(s));
    }
    // Only the layers that joined go whole to the next commit.
    let mut joined = Vec::new();
    for s in &wanted {
        let was = imp(s).write(|m| m.superlayer.replace(NonNull::from(&*l)));
        if was.is_none_or(|p| !std::ptr::eq(p.as_ptr(), &*l)) {
            joined.push(s.clone());
        }
    }
    super::transaction::structure_changed(li);
    for s in &joined {
        super::transaction::attached(imp(s));
    }
    drop(gone);
}

/// Keep the layer's geometry the view's. Inside an animation context that
/// allows implicit animation, the changes of its position, bounds and
/// opacity go through the layer's actions (so they animate, as macOS's
/// do); otherwise they apply at once.
pub(crate) fn sync_geometry(view: &NSViewImpl) {
    let Some(l) = layer_of(view) else { return };
    let frame = views::frame(view);
    let bounds = crate::view_layout::bounds(view);
    let flipped = views::is_flipped(view);
    let super_flipped = views::superview_of(view).is_some_and(views::is_flipped);
    let hidden = views::is_hidden(view);
    let alpha = views::alpha_of(view).clamp(0.0, 1.0);
    let clips = views::clips_to_bounds(view);
    let hosted = state(view).hosted.get();
    let li = imp(&l);
    let position = [frame.origin.x, frame.origin.y];
    // The layer's bounds are the view's; a bounds size other than the
    // frame's is a scale (measured).
    let size = |b: f64, f: f64| if b.abs() > 1e-12 { b } else { f };
    let lb = [
        bounds.origin.x,
        bounds.origin.y,
        size(bounds.size.width, frame.size.width),
        size(bounds.size.height, frame.size.height),
    ];
    let scale = [frame.size.width / lb[2], frame.size.height / lb[3]];
    let scaled = (scale[0] - 1.0).abs() > 1e-12 || (scale[1] - 1.0).abs() > 1e-12;
    let transform = if scaled {
        Some(super::math::scale(scale[0], scale[1], 1.0))
    } else if state(view).scaled.get() {
        Some(super::math::IDENTITY)
    } else {
        None
    };
    state(view).scaled.set(scaled);
    let implicit = crate::animation::allows_implicit() && li.is_live() && !super::transaction::disable_actions();
    let (old_position, old_bounds, old_opacity) = li.read(|m| (m.props.position, m.props.bounds, m.props.opacity));
    if implicit {
        // As their setters would, one key at a time (the view answers nil
        // for actions now, so the layer's defaults apply).
        if old_position != position {
            layer::change(li, Key::Position, |m| m.props.position = position);
        }
        if old_bounds != lb {
            layer::change(li, Key::Bounds, |m| m.props.bounds = lb);
        }
        if old_opacity != alpha {
            layer::change(li, Key::Opacity, |m| m.props.opacity = alpha);
        }
    }
    let changed = li.write(|m| {
        let before = (
            m.props.position,
            m.props.bounds,
            m.props.geometry_flipped,
            m.props.hidden,
            m.props.opacity,
            m.props.masks_to_bounds,
            m.props.transform,
        );
        m.props.anchor = [0.0, 0.0];
        m.props.position = position;
        m.props.bounds = lb;
        m.props.geometry_flipped = flipped != super_flipped;
        m.props.hidden = hidden;
        m.props.opacity = alpha;
        if let Some(t) = transform {
            m.props.transform = t;
        }
        if !hosted {
            m.props.masks_to_bounds = clips;
        }
        before
            != (
                m.props.position,
                m.props.bounds,
                m.props.geometry_flipped,
                m.props.hidden,
                m.props.opacity,
                m.props.masks_to_bounds,
                m.props.transform,
            )
    });
    if changed {
        super::transaction::mark_dirty(li);
    }
}

pub(crate) fn geometry_changed(view: &NSViewImpl) {
    if layer_of(view).is_some() {
        sync_geometry(view);
        // Subviews' flipped geometry follows this one's.
        for sub in views::subviews(view) {
            sync_geometry(views::imp(&sub));
        }
        // A view that redraws when resized draws again (or updates its
        // layer again).
        if !crate::view_layout::keeps_content_on_resize(view) {
            match layer_of(view).filter(|_| updates_layer(views::as_view(view))) {
                Some(l) => layer::set_needs_display(imp(&l)),
                None => set_needs_display(view, crate::view_layout::bounds(view)),
            }
        }
    }
}

/// A subview joined `view`: its layer follows (made, or dropped when the
/// tree it joined isn't layer-backed).
pub(crate) fn subview_linked(view: &NSViewImpl, sub: &NSViewImpl) {
    refresh(sub);
    if layer_of(view).is_some() {
        sync_sublayers(view);
    }
}

/// A view left its superview: its layer leaves the superview's layer. The
/// view keeps it (measured) until it joins a tree again.
pub(crate) fn removed(view: &NSViewImpl) {
    if let Some(l) = layer_of(view) {
        layer::remove_from_superlayer(imp(&l));
    }
}

// Drawing.

/// Whether `view` draws into a canvas of its own in a window's display
/// pass: it's layer-backed, in a window, and isn't a scroll layer's
/// document (whose tiles are its backing store).
pub(crate) fn composited(view: &NSViewImpl) -> bool {
    if layer_of(view).is_none() || views::window_of(view).is_none() {
        return false;
    }
    !views::superview_of(view).is_some_and(|s| views::is_clip(s) && crate::layers::promoted(s))
}

/// Part of a composited view's drawing is out of date: its canvas redraws
/// there at the next display pass (or, for a view that updates its layer
/// itself, its layer displays at the next commit). Returns false for a
/// view whose drawing isn't composited.
pub(crate) fn invalidate(view: &NSViewImpl, rect: NSRect) -> bool {
    if !composited(view) {
        return false;
    }
    if updates_layer(views::as_view(view))
        && let Some(l) = layer_of(view)
    {
        layer::set_needs_display(imp(&l));
        return true;
    }
    set_needs_display(view, rect);
    true
}

fn set_needs_display(view: &NSViewImpl, rect: NSRect) {
    let Some(window) = views::window_of(view) else { return };
    state(view).damage.borrow_mut().push(rect);
    if !state(view).pending.replace(true) {
        let w = super::objects::weak_of(Some(views::as_view(view)));
        PENDING.with(|p| p.borrow_mut().push(w));
    }
    window.needs_display_pass();
}

/// A view's layer asked to display (`-[CALayer setNeedsDisplay]`): a view
/// that updates its layer itself does at the next commit; another draws
/// its canvas again.
pub(crate) fn layer_needs_display(view: &NSView) {
    if updates_layer(view) {
        return;
    }
    let v = views::imp(view);
    set_needs_display(v, crate::view_layout::bounds(v));
}

/// A view's layer displays (`-[CALayer display]`): `updateLayer` for a
/// view that wants it, else the view's drawing again into its canvas.
pub(crate) fn display_view_layer(view: &NSView) {
    if updates_layer(view) {
        // SAFETY: updateLayer takes nothing.
        let _: () = unsafe { msg_send![view, updateLayer] };
        return;
    }
    let v = views::imp(view);
    set_needs_display(v, crate::view_layout::bounds(v));
}

thread_local! {
    /// Views whose canvases are out of date.
    static PENDING: RefCell<Vec<Weak<NSView>>> = const { RefCell::new(Vec::new()) };
    /// The layers the display pass leaves out of the trees it composites
    /// (scroll layers' documents and views drawn in overlays), per window.
    static SKIP: RefCell<Vec<LayerId>> = const { RefCell::new(Vec::new()) };
    /// The window whose display pass runs, and whether a commit went out
    /// in it.
    static PASS: Cell<(Option<WindowId>, bool)> = const { Cell::new((None, false)) };
    /// Windows that will draw a view's new canvas at their next display
    /// pass: the render thread holds their layer trees until it presents.
    static HOLDS: RefCell<Vec<WindowId>> = const { RefCell::new(Vec::new()) };
}

/// A display pass of `window` starts: commits until [`end_pass`] have the
/// render thread hold its layer trees until the pass presents.
pub(crate) fn begin_pass(window: &crate::window::NSWindowImpl) {
    PASS.with(|p| p.set((Some(window.id()), false)));
}

/// The pass's commits are done; returns whether one went out (then the
/// pass must present).
pub(crate) fn end_pass() -> bool {
    PASS.with(|p| p.replace((None, false)).1)
}

/// The windows a commit going out now has the render thread hold.
pub(crate) fn take_holds() -> Vec<WindowId> {
    let mut holds = HOLDS.with(|h| std::mem::take(&mut *h.borrow_mut()));
    PASS.with(|p| {
        let (window, _) = p.get();
        if let Some(w) = window {
            p.set((window, true));
            if !holds.contains(&w) {
                holds.push(w);
            }
        }
    });
    holds
}

/// The canvas of a view's layer: the part of its bounds it covers (the
/// whole of them, unless they're too big, then a part around what shows,
/// on a coarse grid, kept while it covers what shows), and its size in
/// pixels.
fn canvas_rect(view: &NSViewImpl, props: &Props, scale: f64) -> Option<([f64; 4], u32, u32)> {
    let [bx, by, w, h] = props.bounds;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let mut r = [bx, by, w, h];
    if w * scale > CANVAS_MAX || h * scale > CANVAS_MAX {
        let vis = crate::view_layout::visible_rect(view);
        if vis.size.width <= 0.0 || vis.size.height <= 0.0 {
            return None;
        }
        let (sw, sh) = ((CANVAS_MAX / scale).floor().min(w), (CANVAS_MAX / scale).floor().min(h));
        // Along one axis: where a span of `span` starts to show the visible
        // range, snapped down to the grid, within the bounds.
        let start = |lo: f64, len: f64, v0: f64, vlen: f64, span: f64| -> f64 {
            let ideal = v0 + vlen / 2.0 - span / 2.0;
            ((ideal / CANVAS_GRID).floor() * CANVAS_GRID).clamp(lo, (lo + len - span).max(lo))
        };
        let covers = |c: [f64; 4]| {
            c[0] <= vis.origin.x.max(bx)
                && c[1] <= vis.origin.y.max(by)
                && c[0] + c[2] >= (vis.origin.x + vis.size.width).min(bx + w)
                && c[1] + c[3] >= (vis.origin.y + vis.size.height).min(by + h)
                && c[2] == sw
                && c[3] == sh
                && c[0] >= bx
                && c[1] >= by
        };
        r = match state(view).canvas.get() {
            Some(c) if covers(c) => c,
            _ => [
                start(bx, w, vis.origin.x, vis.size.width, sw),
                start(by, h, vis.origin.y, vis.size.height, sh),
                sw,
                sh,
            ],
        };
    }
    Some((r, (r[2] * scale).ceil() as u32, (r[3] * scale).ceil() as u32))
}

/// A viewport moved without changing its descendants' model geometry.
/// Keep canvases that cover the new viewport, and commit a replacement
/// for those that do not. The commit holds presentation until the new
/// canvas is painted, as it does for any other canvas change.
pub(crate) fn prepare_viewports(view: &NSViewImpl) {
    if views::is_hidden(view) {
        return;
    }
    if let Some(old) = state(view).canvas.get()
        && composited(view)
        && let Some(layer) = layer_of(view)
    {
        let li = imp(&layer);
        let props = li.read(|m| m.props.clone());
        let scale = views::window_of(view).map_or(1.0, |w| w.scale());
        if let Some((rect, _, _)) = canvas_rect(view, &props, scale)
            && old != rect
        {
            super::transaction::mark_dirty(li);
        }
    }
    for sub in views::subviews(view) {
        prepare_viewports(views::imp(&sub));
    }
}

/// Whether a view updates its layer itself (`wantsUpdateLayer`).
pub(crate) fn updates_layer(view: &NSView) -> bool {
    // SAFETY: wantsUpdateLayer takes nothing and returns BOOL.
    unsafe { msg_send![view, wantsUpdateLayer] }
}

/// Whether a view-backed layer shows the view's drawing (a canvas): the
/// view draws (`drawRect:`), doesn't update its layer itself, and its
/// layer wasn't given contents.
pub(crate) fn draws_canvas(view: &NSView) -> bool {
    let v = views::imp(view);
    let given = layer_of(v).is_some_and(|l| imp(&l).read(|m| m.objs.contents.is_some()));
    !given && crate::layers::overrides_draw_rect(v) && !updates_layer(view)
}

/// The contents given to a view-backed layer that shows them (see
/// [`draws_canvas`]), as pixels and their size in points.
pub(crate) fn given_contents(
    view: &NSView,
    props: &Props,
) -> Option<(std::sync::Arc<crate::raster::images::ImageData>, [f64; 2])> {
    let given = layer_of(views::imp(view)).and_then(|l| imp(&l).read(|m| m.objs.contents.clone()))?;
    super::render::image_of_object(&given, props.contents_scale.max(0.01))
}

/// What a commit sends of a view-backed layer's canvas (for a view that
/// [`draws_canvas`]).
pub(crate) fn canvas_update(view: &NSView, props: &Props) -> ContentUpdate {
    let v = views::imp(view);
    let scale = views::window_of(v).map_or(1.0, |w| w.scale());
    match canvas_rect(v, props, scale) {
        Some((rect, width, height)) => {
            if state(v).canvas.replace(Some(rect)) != Some(rect) {
                // A new canvas: drawn whole at the next display pass, which
                // the render thread waits for before showing it.
                set_needs_display(v, crate::view_layout::bounds(v));
                if let Some(w) = views::window_of(v) {
                    let id = w.id();
                    HOLDS.with(|h| {
                        let mut h = h.borrow_mut();
                        if !h.contains(&id) {
                            h.push(id);
                        }
                    });
                }
            }
            ContentUpdate::Canvas { rect, width, height, scale }
        }
        None => ContentUpdate::None,
    }
}

/// The display pass is about to record a window: bring the canvases of
/// its composited views up to date. Returns whether anything was painted.
pub(crate) fn display_canvases(window: &crate::window::NSWindowImpl) -> bool {
    let views: Vec<Retained<NSView>> = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        let mut mine = Vec::new();
        p.retain(|w| {
            let Some(v) = w.load() else { return false };
            let here = views::window_of(views::imp(&v)).is_some_and(|w| std::ptr::eq(w, window));
            if here {
                mine.push(v);
            }
            !here
        });
        mine
    });
    let mut painted = false;
    for v in views {
        let vi = views::imp(&v);
        state(vi).pending.set(false);
        let damage = std::mem::take(&mut *state(vi).damage.borrow_mut());
        if !composited(vi) || views::is_hidden_or_has_hidden_ancestor(vi) || !draws_canvas(&v) {
            continue;
        }
        let Some(l) = layer_of(vi) else { continue };
        let li = imp(&l);
        li.write(|m| m.needs_display = false);
        let props = li.read(|m| m.props.clone());
        let scale = window.scale();
        let Some((rect, _, _)) = canvas_rect(vi, &props, scale) else { continue };
        if state(vi).canvas.get() != Some(rect) {
            // The next commit makes the canvas; draw it whole then.
            super::transaction::mark_dirty(li);
            super::transaction::commit_now();
        }
        painted |= record_canvas(vi, li.id(), rect, scale, &damage);
    }
    // Layer changes made meanwhile go to the render thread now.
    super::transaction::commit_now();
    painted
}

/// Record a view's drawing (and a text view's TextKit 2 content) for `area`
/// (canvas points; `xf` maps the view's coordinates there), first clearing
/// it when `clear`.
fn record_view(view: &NSViewImpl, xf: crate::graphics::Xf, area: Rect, scale: f64, clear: bool) -> Vec<Op> {
    crate::context::begin_recording(xf, scale);
    if clear {
        crate::graphics::push(Op::FillWith { rect: area, color: [0.0; 4], blend: crate::protocol::Blend::Clear });
    }
    let mark = crate::context::begin_view(view, xf, area);
    // SAFETY: drawRect: takes an NSRect.
    let _: () = unsafe { msg_send![views::as_view(view), drawRect: xf.inverse_rect(area)] };
    crate::context::end_view(mark);
    if crate::textkit::text_view::draws_text_kit_2(views::as_view(view)) {
        let mark = crate::context::begin_view(view, xf, area);
        crate::textkit::text_view::draw_content(views::as_view(view), xf.inverse_rect(area));
        crate::context::end_view(mark);
    }
    crate::graphics::end_recording()
}

/// Record the view's drawing for `damage` (view coordinates) of its
/// canvas covering `rect` (layer points), and send it. Returns whether
/// anything was.
fn record_canvas(view: &NSViewImpl, id: LayerId, rect: [f64; 4], scale: f64, damage: &[NSRect]) -> bool {
    let Some(window) = views::window_of(view) else { return false };
    let window = window.id();
    // View coordinates to canvas points, from the canvas's top left.
    let xf = super::render::top_left(rect, views::is_flipped(view));
    let whole = Rect::new(0.0, 0.0, rect[2] as f32, rect[3] as f32);
    let areas: Vec<Rect> = crate::layers::coalesce(
        damage.iter().map(|d| xf.rect(*d).intersect(&whole).round_out()).filter(|r| !r.is_empty()).collect(),
    );
    if areas.is_empty() {
        return false;
    }
    for area in areas {
        let ops = record_view(view, xf, area, scale, true);
        crate::app::send(ToRender::Paint { window, target: Target::Content(id), rects: vec![area], ops });
    }
    true
}

/// A view-backed layer's contents for `renderInContext:`: the view's
/// drawing, recorded now.
pub(crate) fn view_content(view: &NSView, props: &Props) -> Option<NodeContent> {
    let v = views::imp(view);
    if !draws_canvas(view) {
        return given_contents(view, props).map(|(image, size)| NodeContent::Image { image, size });
    }
    let [_, _, w, h] = props.bounds;
    let scale = props.contents_scale.max(1.0);
    let xf = super::render::top_left(props.bounds, views::is_flipped(v));
    let ops = record_view(v, xf, Rect::new(0.0, 0.0, w as f32, h as f32), scale, false);
    let image = super::render::rasterize(&ops, (w * scale).ceil() as u32, (h * scale).ceil() as u32, scale)?;
    Some(NodeContent::Canvas { image, rect: props.bounds })
}

// The display pass's side.

/// The layers the display pass leaves out of the trees it composites.
pub(crate) fn set_skipped(skip: Vec<LayerId>) {
    SKIP.with(|s| *s.borrow_mut() = skip);
}

/// The composite a host view's tree draws as, in a display pass: `xf`
/// maps the view to the layer being recorded, `clip` is where its
/// ancestors let it draw (layer points).
pub(crate) fn composite_op(view: &NSViewImpl, xf: crate::graphics::Xf, clip: Rect) -> Option<Op> {
    let l = layer_of(view)?;
    let super_flipped = views::superview_of(view).is_some_and(views::is_flipped);
    let sup = views::step(view, super_flipped, views::frame(view)).inverse().then(&xf);
    let base = [1.0, 0.0, 0.0, sup.a, sup.tx, sup.ty];
    let skip: std::sync::Arc<[LayerId]> = SKIP.with(|s| std::sync::Arc::from(s.borrow().as_slice()));
    let mask = crate::context::with_state(|st| st.gs.mask.clone()).flatten();
    Some(Op::Composite(std::sync::Arc::new(crate::protocol::CompositeOp {
        root: imp(&l).id(),
        base,
        down: sup.a > 0.0,
        clip,
        mask,
        skip,
    })))
}

/// How far a host view's layer tree draws, in its coordinates: its
/// layers' frames, shadows and sublayers, and where their animations take
/// them.
pub(crate) fn reach(view: &NSViewImpl) -> Option<NSRect> {
    let l = layer_of(view)?;
    let li = imp(&l);
    let props = li.read(|m| m.props.clone());
    let own = props.to_superlayer();
    let inv = super::props::invert_affine(&own)?;
    let mut acc: Option<Rect> = None;
    tree_reach(&l, &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0], &mut acc, 0);
    let r = acc?;
    // In the layer's own space: the view's coordinates.
    let b = super::render::map_bounds(&inv, [r.x0 as f64, r.y0 as f64, (r.x1 - r.x0) as f64, (r.y1 - r.y0) as f64]);
    Some(NSRect::new(NSPoint::new(b.x0 as f64, b.y0 as f64), NSSize::new((b.x1 - b.x0) as f64, (b.y1 - b.y0) as f64)))
}

/// The box `layer`'s tree covers in its superlayer's space (mapped by
/// `parent`), over its animations' reach.
fn tree_reach(layer: &CALayer, parent: &[f64; 6], acc: &mut Option<Rect>, depth: usize) {
    if depth > 64 {
        return;
    }
    let li = imp(layer);
    let (props, subs, anims) =
        li.read(|m| (m.props.clone(), m.sublayers.clone(), m.anims.iter().map(|a| a.spec.clone()).collect::<Vec<_>>()));
    // The model, and samples along each animation.
    let mut states = vec![props.clone()];
    for spec in &anims {
        let (begin, end) =
            (spec.timing.begin, spec.end().unwrap_or(spec.timing.begin + 1.0).min(spec.timing.begin + 60.0));
        for i in 0..=16 {
            let t = begin + (end - begin) * i as f64 / 16.0;
            let mut p = props.clone();
            super::spec::present(&mut p, std::slice::from_ref(spec), t);
            states.push(p);
        }
    }
    for p in &states {
        let m = super::props::compose(&p.to_superlayer(), parent);
        let mut r = super::render::map_bounds(&m, p.bounds);
        if p.shadow_opacity > 0.0 {
            let pad = (3.0 * p.shadow_radius + p.shadow_offset[0].abs().max(p.shadow_offset[1].abs())) as f32 + 2.0;
            r = Rect::new(r.x0 - pad, r.y0 - pad, r.x1 + pad, r.y1 + pad);
        }
        *acc = Some(acc.map_or(r, |a| a.union(&r)));
    }
    if props.masks_to_bounds {
        return;
    }
    let m = super::render::sublayer_map(&props, &super::props::compose(&props.to_superlayer(), parent));
    for s in subs {
        if imp(&s).has_view() {
            // A subview's own reach counts through the view tree.
            continue;
        }
        tree_reach(&s, &m, acc, depth + 1);
    }
}

// Commits.

/// The window of a layer tree's root, if a view in one backs it.
pub(crate) fn window_of_root(root: &CALayer) -> Option<Retained<NSWindow>> {
    let view = imp(root).view()?;
    views::window_of(views::imp(&view)).map(|w| w.as_window().retain())
}

pub(crate) fn window_on_screen(window: &NSWindow) -> bool {
    crate::window::imp(window).on_screen()
}

/// Whether a window has a backing, so the layers in its trees can be live
/// (a deferred window has none until it is shown, measured).
pub(crate) fn window_backed(window: &NSWindow) -> bool {
    crate::window::imp(window).backed()
}

/// Whether the host view of a layer tree whose root `root` is sits in a
/// flipped superview (the layers' space then runs down the window).
pub(crate) fn host_down(root: &CALayer) -> bool {
    imp(root).view().is_some_and(|v| views::superview_of(views::imp(&v)).is_some_and(views::is_flipped))
}

/// A commit went out: windows whose composited views need drawing draw.
pub(crate) fn committed() {
    let windows: Vec<Retained<NSWindow>> = PENDING.with(|p| {
        p.borrow()
            .iter()
            .filter_map(|w| w.load())
            .filter_map(|v| views::window_of(views::imp(&v)).map(|w| w.as_window().retain()))
            .collect()
    });
    for w in windows {
        crate::window::imp(&w).needs_display_pass();
    }
}

/// A window came on screen (or its scale changed): every layer in its
/// trees goes to the render thread again, whole.
pub(crate) fn window_shown(window: &crate::window::NSWindowImpl) {
    let Some(content) = window.content() else { return };
    let mut stack = vec![content];
    while let Some(v) = stack.pop() {
        let vi = views::imp(&v);
        if let Some(l) = layer_of(vi) {
            state(vi).canvas.set(None);
            imp(&l).write(|m| m.props.contents_scale = window.scale());
            for sub in layer::tree(&l) {
                super::transaction::mark_dirty(imp(&sub));
            }
            // Canvases are drawn again (a view updating its layer itself
            // keeps what it set).
            if draws_canvas(&v) {
                set_needs_display(vi, crate::view_layout::bounds(vi));
            }
        }
        stack.extend(views::subviews(vi).iter().cloned());
    }
}

/// NSView's `actionForLayer:forKey:`: `NSNull` (no implicit animation of
/// a view's layer), unless the current animation context allows implicit
/// animation, then nil (the layer's own default).
pub(crate) fn action_for_layer(_view: &NSViewImpl) -> Option<Retained<AnyObject>> {
    if crate::animation::allows_implicit() {
        return None;
    }
    Some(super::objects::any(objc2_foundation::NSNull::null()))
}
