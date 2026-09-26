//! Auto Layout: `NSLayoutConstraint`, the anchors, `NSLayoutGuide`,
//! `NSStackView`, and the NSView methods that use them, on the kasuari
//! solver (see `engine` for how views become variables).
//!
//! **Installing.** Activating a constraint installs it on the nearest view
//! that holds all its items (a guide counts as its owning view), which
//! keeps it (`constraints`); `addConstraint:` installs on the receiver.
//! Each item's view also notes the constraints naming it, so a view
//! leaving its superview finds, in its own subtree, the constraints between
//! it and the rest. As in AppKit, those go unless the view they are
//! installed on still holds the view where it goes; those among its own
//! subviews stay. A view that goes away takes its constraints and guides
//! with it: they are inactive, and ownerless.
//!
//! **Engines.** A tree of views with active constraints has one engine, kept
//! by the tree's root (a window's content view, or the top view of a tree
//! outside a window) and made when its first constraint is activated;
//! trees without constraints never make one. Adding and removing
//! constraints, changes to their constants and priorities, and views
//! moving in and out of the tree update the solver as they happen (see
//! `engine`). A view that stops being a root gives up its engine; one that
//! becomes a root gets a new one, built when first used. Only subtrees
//! flagged as taking part (`view_layout::has_auto`) are looked at, so
//! views that never used Auto Layout cost it nothing as they move.
//!
//! **Layout.** The layout pass (`view_layout`) solves after
//! `updateConstraints` and before `layout`, and asks for layout of the
//! superview of each view whose variables moved. NSView's default `layout`
//! gives its subviews that don't translate their masks the frames the
//! solver found, edges rounded to the window's pixels (whole points outside
//! a window). Views that translate their masks keep being placed by
//! autoresizing, which their generated constraints reproduce.
//!
//! No RefCell borrow is held across a message: the engine is taken out of
//! its root while it works, and messages that might reach program code
//! (`intrinsicContentSize`, `alignmentRectInsets`) find it gone.

mod anchor;
mod constraint;
mod engine;
mod guide;
mod stack;

use std::cell::Cell;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, DefinedClass, Message, define_class};
use objc2_app_kit::{
    NSLayoutAttribute, NSLayoutConstraint, NSLayoutConstraintOrientation, NSLayoutDimension, NSLayoutGuide,
    NSLayoutXAxisAnchor, NSLayoutYAxisAnchor, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSObjectProtocol, NSPoint, NSRect, NSSize};

use engine::{DEFAULT_COMPRESSION, DEFAULT_HUGGING, Engine, Mode, NO_INTRINSIC};

use crate::view_layout;
use crate::views::{self, NSViewImpl};

sidestep_runtime::static_class!(pub NSLAYOUTCONSTRAINT, NSLAYOUTCONSTRAINT_META = "NSLayoutConstraint", || {
    let _ = constraint::NSLayoutConstraintImpl::class();
});

sidestep_runtime::static_class!(pub NSLAYOUTANCHOR, NSLAYOUTANCHOR_META = "NSLayoutAnchor", || {
    let _ = anchor::NSLayoutAnchorImpl::class();
});

sidestep_runtime::static_class!(pub NSLAYOUTXAXISANCHOR, NSLAYOUTXAXISANCHOR_META = "NSLayoutXAxisAnchor", || {
    let _ = anchor::NSLayoutXAxisAnchorImpl::class();
});

sidestep_runtime::static_class!(pub NSLAYOUTYAXISANCHOR, NSLAYOUTYAXISANCHOR_META = "NSLayoutYAxisAnchor", || {
    let _ = anchor::NSLayoutYAxisAnchorImpl::class();
});

sidestep_runtime::static_class!(pub NSLAYOUTDIMENSION, NSLAYOUTDIMENSION_META = "NSLayoutDimension", || {
    let _ = anchor::NSLayoutDimensionImpl::class();
});

sidestep_runtime::static_class!(pub NSLAYOUTGUIDE, NSLAYOUTGUIDE_META = "NSLayoutGuide", || {
    let _ = guide::NSLayoutGuideImpl::class();
});

sidestep_runtime::static_class!(pub NSSTACKVIEW, NSSTACKVIEW_META = "NSStackView", || {
    let _ = stack::NSStackViewImpl::class();
});

// Items.

/// Something constraints can name: a view or a layout guide. Only valid
/// while the object lives; see `constraint` for why active constraints'
/// items do.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ItemRef {
    ptr: NonNull<AnyObject>,
    guide: bool,
}

impl ItemRef {
    pub(crate) fn view(view: &NSViewImpl) -> ItemRef {
        ItemRef { ptr: NonNull::from(view).cast(), guide: false }
    }

    pub(crate) fn guide(guide: &NSLayoutGuide) -> ItemRef {
        ItemRef { ptr: NonNull::from(guide).cast(), guide: true }
    }

    pub(crate) fn key(self) -> usize {
        self.ptr.as_ptr().addr()
    }

    /// # Safety
    /// The item must be alive, for `'a`.
    pub(crate) unsafe fn as_view<'a>(self) -> Option<&'a NSViewImpl> {
        // SAFETY: a view item is an NSView, alive by the caller's promise.
        (!self.guide).then(|| views::imp(unsafe { self.ptr.cast::<NSView>().as_ref() }))
    }

    /// # Safety
    /// As for `as_view`.
    pub(crate) unsafe fn as_guide<'a>(self) -> Option<&'a NSLayoutGuide> {
        // SAFETY: a guide item is an NSLayoutGuide, alive by the caller's
        // promise.
        self.guide.then(|| unsafe { self.ptr.cast::<NSLayoutGuide>().as_ref() })
    }

    /// The view whose coordinates the item's frame is in: its superview,
    /// or a guide's owning view.
    ///
    /// # Safety
    /// As for `as_view`.
    pub(crate) unsafe fn parent<'a>(self) -> Option<&'a NSViewImpl> {
        // SAFETY: the caller's promise; a superview or owning view outlives
        // its link.
        unsafe {
            match self.as_view() {
                Some(view) => views::superview_of(view),
                None => guide::owner(self.as_guide()?).map(|v| views::imp(v.as_ref())),
            }
        }
    }

    /// The view that stands for the item in the tree: the view itself, or
    /// a guide's owning view.
    ///
    /// # Safety
    /// As for `as_view`.
    pub(crate) unsafe fn host<'a>(self) -> Option<&'a NSViewImpl> {
        // SAFETY: as for `parent`.
        unsafe {
            match self.as_view() {
                Some(view) => Some(view),
                None => guide::owner(self.as_guide()?).map(|v| views::imp(v.as_ref())),
            }
        }
    }
}

/// The item a constraint names by object: a view or a guide.
pub(crate) fn item_ref(object: &AnyObject) -> ItemRef {
    if let Some(guide) = object.downcast_ref::<NSLayoutGuide>() {
        ItemRef::guide(guide)
    } else if let Some(view) = object.downcast_ref::<NSView>() {
        ItemRef::view(views::imp(view))
    } else {
        panic!(
            "sidestep: a layout constraint's items must be views or layout guides, not {}",
            object.class().name().to_string_lossy()
        );
    }
}

/// An item's attribute, weighed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Term {
    pub item: ItemRef,
    pub attr: NSLayoutAttribute,
    pub coeff: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Axis {
    X,
    Y,
}

impl Axis {
    pub(crate) fn of(orientation: NSLayoutConstraintOrientation) -> Axis {
        if orientation == NSLayoutConstraintOrientation::Vertical { Axis::Y } else { Axis::X }
    }
}

/// The axis an anchor for `attr` lies along; none for the dimensions.
pub(crate) fn axis_of(attr: NSLayoutAttribute) -> Option<Axis> {
    match attr {
        NSLayoutAttribute::Left
        | NSLayoutAttribute::Right
        | NSLayoutAttribute::Leading
        | NSLayoutAttribute::Trailing
        | NSLayoutAttribute::CenterX => Some(Axis::X),
        NSLayoutAttribute::Top
        | NSLayoutAttribute::Bottom
        | NSLayoutAttribute::CenterY
        | NSLayoutAttribute::FirstBaseline
        | NSLayoutAttribute::LastBaseline => Some(Axis::Y),
        _ => None,
    }
}

/// The axis `attr` constrains, dimensions included.
pub(crate) fn layout_axis(attr: NSLayoutAttribute) -> Axis {
    match attr {
        NSLayoutAttribute::Height => Axis::Y,
        other => axis_of(other).unwrap_or(Axis::X),
    }
}

/// An anchor's name, as AppKit gives it.
pub(crate) fn attribute_name(attr: NSLayoutAttribute) -> &'static str {
    match attr {
        NSLayoutAttribute::Left => "left",
        NSLayoutAttribute::Right => "right",
        NSLayoutAttribute::Top => "top",
        NSLayoutAttribute::Bottom => "bottom",
        NSLayoutAttribute::Leading => "leading",
        NSLayoutAttribute::Trailing => "trailing",
        NSLayoutAttribute::Width => "width",
        NSLayoutAttribute::Height => "height",
        NSLayoutAttribute::CenterX => "centerX",
        NSLayoutAttribute::CenterY => "centerY",
        NSLayoutAttribute::LastBaseline => "lastBaseline",
        NSLayoutAttribute::FirstBaseline => "firstBaseline",
        _ => "",
    }
}

// Per-view state.

/// A view's Auto Layout state, made when it first takes part.
pub(crate) struct ViewAuto {
    /// Constraints installed here.
    installed: Vec<Retained<NSLayoutConstraint>>,
    /// Active constraints naming this view, or a guide it owns, as an item.
    naming: Vec<Retained<NSLayoutConstraint>>,
    guides: Vec<Retained<NSLayoutGuide>>,
    anchors: anchor::Anchors,
    /// Hugging (horizontal, vertical), then compression resistance.
    priorities: [f32; 4],
    /// The engine of the tree this view is the root of.
    engine: Option<Box<Engine>>,
}

const DEFAULT_PRIORITIES: [f32; 4] = [DEFAULT_HUGGING, DEFAULT_HUGGING, DEFAULT_COMPRESSION, DEFAULT_COMPRESSION];

impl Default for ViewAuto {
    fn default() -> Self {
        ViewAuto {
            installed: Vec::new(),
            naming: Vec::new(),
            guides: Vec::new(),
            anchors: Default::default(),
            priorities: DEFAULT_PRIORITIES,
            engine: None,
        }
    }
}

impl Drop for ViewAuto {
    /// The view is going away. It has no superview (which would keep it),
    /// so the constraints installed on it name only views going with it:
    /// they become inactive. Its guides have no owner any more.
    fn drop(&mut self) {
        for g in &self.guides {
            guide::set_owner(g, None);
        }
        for c in &self.installed {
            constraint::set_installed(c, None);
            // Views it names that outlive this one forget it. This view's
            // weak references load as nothing now, so its own state, being
            // dropped, isn't touched.
            for (_object, item) in constraint::items(c) {
                // SAFETY: `_object` keeps the item alive.
                if let Some(host) = unsafe { item.host() } {
                    let at = read_auto(host, |a| a.naming.iter().position(|x| std::ptr::eq(&**x, &**c))).flatten();
                    let removed = at.map(|i| with_auto(host, |a| a.naming.remove(i)));
                    drop(removed);
                }
            }
        }
    }
}

fn with_auto<R>(view: &NSViewImpl, f: impl FnOnce(&mut ViewAuto) -> R) -> R {
    let mut slot = view.ivars().state.auto.borrow_mut();
    f(slot.get_or_insert_with(Box::default))
}

fn read_auto<R>(view: &NSViewImpl, f: impl FnOnce(&ViewAuto) -> R) -> Option<R> {
    view.ivars().state.auto.borrow().as_deref().map(f)
}

/// The constraints installed on `view`.
pub(crate) fn installed_on(view: &NSViewImpl) -> Vec<Retained<NSLayoutConstraint>> {
    read_auto(view, |a| a.installed.clone()).unwrap_or_default()
}

/// The guides `view` owns.
pub(crate) fn guides_of(view: &NSViewImpl) -> Vec<Retained<NSLayoutGuide>> {
    read_auto(view, |a| a.guides.clone()).unwrap_or_default()
}

/// Work with the anchors `item` keeps.
///
/// # Safety
/// The item must be alive.
unsafe fn with_anchors<R>(item: ItemRef, f: impl FnOnce(&mut anchor::Anchors) -> R) -> R {
    // SAFETY: the caller's promise.
    unsafe {
        match item.as_guide() {
            Some(g) => guide::with_anchors(g, f),
            None => with_auto(item.as_view().expect("a view or a guide"), |a| f(&mut a.anchors)),
        }
    }
}

pub(crate) fn priorities(view: &NSViewImpl) -> [f32; 4] {
    read_auto(view, |a| a.priorities).unwrap_or(DEFAULT_PRIORITIES)
}

// Engines.

fn root_of(view: &NSViewImpl) -> &NSViewImpl {
    let mut top = view;
    while let Some(sup) = views::superview_of(top) {
        top = sup;
    }
    top
}

fn take_engine(view: &NSViewImpl) -> Option<Box<Engine>> {
    view.ivars().state.auto.borrow_mut().as_mut().and_then(|a| a.engine.take())
}

/// Work on the engine of the tree at `root`, if it has one: taken out
/// while `f` runs, so messages `f` sends can't reach it.
fn with_engine<R>(root: &NSViewImpl, f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    let mut engine = take_engine(root)?;
    let r = f(&mut engine);
    with_auto(root, |a| {
        if a.engine.is_some() {
            // One was made while this one was out: this one is out of date.
            engine.stale = true;
        }
        a.engine = Some(engine);
    });
    Some(r)
}

/// Make sure the tree at `root` has an engine; a new one is built at its
/// first use.
fn ensure_engine(root: &NSViewImpl) {
    with_auto(root, |a| {
        if a.engine.is_none() {
            a.engine = Some(Box::new(Engine::new(root, Mode::Tree)));
        }
    });
}

/// The tree's engine needs rebuilding before its next use.
fn mark_stale(root: &NSViewImpl) {
    if let Some(a) = root.ivars().state.auto.borrow_mut().as_mut()
        && let Some(e) = a.engine.as_mut()
    {
        e.stale = true;
    }
    ask_for_pass(root);
}

/// Ask the root's window for a layout pass.
fn ask_for_pass(root: &NSViewImpl) {
    if let Some(window) = views::window_of(root) {
        window.needs_layout_pass();
    }
}

/// Bring the engine up to date: rebuilt if stale, its constraints weighed
/// by the priorities it has seen.
fn fresh(root: &NSViewImpl, e: &mut Engine) {
    if e.stale {
        e.rebuild(root);
    }
    e.settle();
}

// Activation.

/// Install `c` on the nearest view holding its items, or on `on`
/// (`addConstraint:`).
pub(crate) fn activate(c: &NSLayoutConstraint, on: Option<&NSViewImpl>) {
    if constraint::installed(c).is_some() {
        return;
    }
    if !constraint::items_alive(c) {
        // AppKit crashes here; this leaves the constraint inactive.
        eprintln!("sidestep: can't activate a layout constraint whose view or layout guide is gone");
        return;
    }
    let items = constraint::items(c);
    let mut top: Option<&NSViewImpl> = None;
    let mut hosts: Vec<&NSViewImpl> = Vec::with_capacity(items.len());
    for (_object, item) in &items {
        // SAFETY: `_object` keeps the item alive.
        let Some(host) = (unsafe { item.host() }) else {
            panic!("sidestep: can't activate a constraint on a layout guide that no view owns");
        };
        if !hosts.iter().any(|h| std::ptr::eq(*h, host)) {
            hosts.push(host);
        }
        top = Some(match top {
            None => host,
            Some(t) => view_layout::common_ancestor(t, host).unwrap_or_else(|| {
                panic!("sidestep: can't activate a constraint between items in different view hierarchies")
            }),
        });
    }
    let Some(mut top) = top else { panic!("sidestep: can't activate a constraint without items") };
    if let Some(on) = on {
        assert!(view_layout::is_descendant(top, on), "sidestep: addConstraint: on a view that doesn't hold its items");
        top = on;
    }
    let c = c.retain();
    with_auto(top, |a| a.installed.push(c.clone()));
    // Each item's view takes part now, and knows the constraint names it.
    for host in &hosts {
        with_auto(host, |a| a.naming.push(c.clone()));
        view_layout::mark_auto(host);
    }
    view_layout::mark_auto(top);
    constraint::set_installed(&c, Some(NonNull::from(views::as_view(top))));
    let root = root_of(top);
    ensure_engine(root);
    with_engine(root, |e| {
        if !e.stale {
            e.add(&c);
        }
        e.dirty = true;
    });
    ask_for_pass(root);
}

pub(crate) fn deactivate(c: &NSLayoutConstraint) {
    let Some(on) = constraint::installed(c) else { return };
    let c = c.retain();
    // SAFETY: the view it's installed on holds it, and clears the link when
    // it goes.
    let view = views::imp(unsafe { on.as_ref() });
    constraint::set_installed(&c, None);
    let take = |list: &mut Vec<Retained<NSLayoutConstraint>>| {
        let at = list.iter().position(|x| std::ptr::eq(&**x, &*c));
        at.map(|i| list.remove(i))
    };
    let removed = with_auto(view, |a| take(&mut a.installed));
    let mut named = Vec::new();
    for (_object, item) in constraint::items(&c) {
        // SAFETY: `_object` keeps the item alive.
        if let Some(host) = unsafe { item.host() } {
            named.extend(with_auto(host, |a| take(&mut a.naming)));
        }
    }
    drop(removed);
    drop(named);
    let root = root_of(view);
    with_engine(root, |e| {
        if !e.stale {
            e.remove(&c);
        }
        e.dirty = true;
    });
    ask_for_pass(root);
}

/// A constraint's constant or priority changed.
pub(crate) fn constraint_changed(c: &NSLayoutConstraint) {
    let Some(on) = constraint::installed(c) else { return };
    // SAFETY: as in `deactivate`.
    let root = root_of(views::imp(unsafe { on.as_ref() }));
    // The same constraint, reweighed.
    with_engine(root, |e| {
        if !e.stale && e.has_constraint(c) {
            e.remove(c);
            e.add(c);
        }
        e.dirty = true;
    });
    ask_for_pass(root);
}

// Hooks from the view contract.

/// A stack view's arrangement changed.
fn stack_changed(view: &NSViewImpl) {
    view_layout::mark_auto(view);
    let root = root_of(view);
    ensure_engine(root);
    with_engine(root, |e| {
        if !e.stale {
            e.include(ItemRef::view(view));
        }
    });
    view_layout::needs_layout(view);
    ask_for_pass(root);
}

/// `view`, a subview of `superview`, was hidden or shown.
pub(crate) fn subview_hidden(superview: &NSViewImpl, view: &NSViewImpl) {
    if let Some(stack) = stack::as_stack(superview) {
        stack::subview_hidden(stack, view);
    }
}

/// `view` is about to leave its superview `from` for `to`: the
/// constraints between its subtree and the rest go, unless the view they
/// are installed on holds `to`, and the tree's engine forgets the subtree.
/// Nothing to do for a subtree that never took part.
pub(crate) fn leaving(view: &NSViewImpl, from: &NSViewImpl, to: Option<&NSViewImpl>) {
    if !view_layout::has_auto(view) {
        return;
    }
    let mut crossing = Vec::new();
    naming_outside(view, view, &mut crossing);
    for c in &crossing {
        let kept = to.is_some_and(|to| {
            // SAFETY: the view it's installed on holds it.
            constraint::installed(c)
                .is_some_and(|on| view_layout::is_descendant(to, views::imp(unsafe { on.as_ref() })))
        });
        if !kept {
            deactivate(c);
        }
    }
    let root = root_of(from);
    with_engine(root, |e| {
        if !e.stale {
            e.forget_subtree(view);
        }
    });
    ask_for_pass(root);
}

/// The constraints naming views of the subtree at `view` that are
/// installed outside the subtree at `top`.
fn naming_outside(view: &NSViewImpl, top: &NSViewImpl, out: &mut Vec<Retained<NSLayoutConstraint>>) {
    if !view_layout::has_auto(view) {
        return;
    }
    let naming = read_auto(view, |a| a.naming.clone()).unwrap_or_default();
    for c in naming {
        let inside = constraint::installed(&c).is_some_and(|on| {
            // SAFETY: the view it's installed on holds it.
            view_layout::is_descendant(views::imp(unsafe { on.as_ref() }), top)
        });
        if !inside && !out.iter().any(|x| std::ptr::eq(&**x, &*c)) {
            out.push(c);
        }
    }
    for sub in views::subviews(view) {
        naming_outside(views::imp(&sub), top, out);
    }
}

/// `view` joined a superview: it isn't a root any more, and its subtree's
/// constraints join its new tree's engine, with those kept between it and
/// the rest.
pub(crate) fn joined(view: &NSViewImpl) {
    drop(take_engine(view));
    if !view_layout::has_auto(view) {
        return;
    }
    let mut kept = Vec::new();
    naming_outside(view, view, &mut kept);
    let root = root_of(view);
    ensure_engine(root);
    with_engine(root, |e| {
        if !e.stale {
            e.learn_subtree(view);
            for c in &kept {
                e.add(c);
            }
        }
    });
    ask_for_pass(root);
}

/// `view` left its superview and is the root of its own tree now: one with
/// constraints gets an engine, built when first used.
pub(crate) fn became_root(view: &NSViewImpl) {
    if view_layout::has_auto(view) {
        ensure_engine(view);
        mark_stale(view);
    }
}

thread_local!(static AUTORESIZING: Cell<Option<(usize, NSRect)>> = const { Cell::new(None) });

/// Set `view`'s frame to `frame`, where autoresizing puts it. A view that
/// translates its mask has constraints that already give that frame for
/// its parent's new size, so `frame_changed` doesn't make them again.
pub(crate) fn autoresize(view: &NSViewImpl, frame: NSRect, set: impl FnOnce()) {
    let before = AUTORESIZING.replace(Some((ItemRef::view(view).key(), frame)));
    set();
    AUTORESIZING.set(before);
}

/// A view's frame changed. Outside the solver's own changes, a view that
/// translates its mask, or the root, has its generated constraints made
/// again from its new frame.
pub(crate) fn frame_changed(view: &NSViewImpl) {
    if APPLYING.get() || !view_layout::engine_hint(view) {
        return;
    }
    let root = root_of(view);
    let item = ItemRef::view(view);
    let autoresized = AUTORESIZING.get().is_some_and(|(key, frame)| key == item.key() && frame == views::frame(view));
    if autoresized && !std::ptr::eq(root, view) {
        return;
    }
    let changed = with_engine(root, |e| {
        let own = std::ptr::eq(root, view) || view_layout::translates_mask(view);
        if e.stale || !own || !e.contains(item) {
            return false;
        }
        e.regenerate(item);
        true
    });
    if changed == Some(true) {
        ask_for_pass(root);
    }
}

/// A view's intrinsic content size, or its priorities, changed.
/// `invalidateIntrinsicContentSize` calls this.
pub(crate) fn intrinsic_size_changed(view: &NSViewImpl) {
    if !view_layout::engine_hint(view) {
        return;
    }
    let root = root_of(view);
    with_engine(root, |e| {
        if !e.stale {
            e.regenerate(ItemRef::view(view));
        }
    });
    ask_for_pass(root);
}

thread_local!(static APPLYING: Cell<bool> = const { Cell::new(false) });

/// Set frames the solver found, telling `frame_changed` they are its own.
fn apply(f: impl FnOnce()) {
    let before = APPLYING.replace(true);
    f();
    APPLYING.set(before);
}

/// Whether the tree at `view` has solving to do.
pub(crate) fn needs_solve(view: &NSViewImpl) -> bool {
    let root = root_of(view);
    read_auto(root, |a| a.engine.as_ref().is_some_and(|e| e.stale || e.dirty)).unwrap_or(false)
}

/// Solve the tree at `view`, and ask for layout of the superviews of the
/// views that moved.
pub(crate) fn solve(view: &NSViewImpl) {
    let root = root_of(view);
    let window = views::window_of(root);
    let Some((changed, size, content)) = with_engine(root, |e| {
        fresh(root, e);
        let changed = e.changes();
        // A root outside a window may be sized by its constraints.
        let free = window.is_none() && !view_layout::translates_mask(root);
        // Content that needs another size asks its window for it, once.
        let wanted = engine::round_frame(NSRect::new(NSPoint::ZERO, e.root_size()), 1.0).size;
        let content = if window.is_none() || wanted == views::frame(root).size {
            e.asked = None;
            None
        } else if e.asked != Some(wanted) {
            e.asked = Some(wanted);
            Some(wanted)
        } else {
            None
        };
        (changed, free.then(|| e.root_size()), content)
    }) else {
        return;
    };
    if let (Some(window), Some(size)) = (window, content) {
        window.as_window().setContentSize(size);
    }
    for item in changed {
        // SAFETY: the engine is fresh, so its items are in the tree.
        if let Some(v) = unsafe { item.as_view() }
            && !std::ptr::eq(v, root)
            && !view_layout::translates_mask(v)
            && let Some(sup) = views::superview_of(v)
        {
            view_layout::needs_layout(sup);
        }
    }
    if let Some(size) = size {
        let size = engine::round_frame(NSRect::new(NSPoint::ZERO, size), 1.0).size;
        if views::frame(root).size != size {
            apply(|| views::as_view(root).setFrameSize(size));
        }
    }
}

/// NSView's `layout`: the solver's frames for the subviews that don't
/// translate their masks.
pub(crate) fn layout_subviews(view: &NSViewImpl) {
    if !view_layout::engine_hint(view) {
        return;
    }
    let root = root_of(view);
    let scale = views::window_of(root).map_or(1.0, |w| w.as_window().backingScaleFactor());
    let Some(frames) = with_engine(root, |e| {
        fresh(root, e);
        let mut frames = Vec::new();
        for sub in views::subviews(view) {
            let s = views::imp(&sub);
            if view_layout::translates_mask(s) {
                continue;
            }
            if let Some(frame) = e.frame(ItemRef::view(s)) {
                frames.push((sub, engine::round_frame(frame, scale)));
            }
        }
        frames
    }) else {
        return;
    };
    apply(|| {
        for (sub, frame) in frames {
            if sub.frame() != frame {
                sub.setFrame(frame);
            }
        }
    });
}

// Guides.

pub(crate) fn add_guide(view: &NSView, guide: &NSLayoutGuide) {
    if let Some(old) = guide::owner(guide) {
        if std::ptr::eq(old.as_ptr(), view) {
            return;
        }
        // SAFETY: the owning view holds the guide, so is alive.
        remove_guide(unsafe { old.as_ref() }, guide);
    }
    with_auto(views::imp(view), |a| a.guides.push(guide.retain()));
    view_layout::mark_auto(views::imp(view));
    guide::set_owner(guide, Some(NonNull::from(view)));
}

pub(crate) fn remove_guide(view: &NSView, guide: &NSLayoutGuide) {
    let v = views::imp(view);
    let item = ItemRef::guide(guide);
    // Constraints naming the guide go with it; its owner knows them.
    let naming = read_auto(v, |a| a.naming.clone()).unwrap_or_default();
    let gone: Vec<_> = naming.into_iter().filter(|c| constraint::items(c).iter().any(|(_, i)| *i == item)).collect();
    for c in &gone {
        deactivate(c);
    }
    let root = root_of(v);
    with_engine(root, |e| {
        if !e.stale {
            e.forget_item(item);
        }
    });
    guide::set_owner(guide, None);
    let removed = with_auto(v, |a| {
        let at = a.guides.iter().position(|g| std::ptr::eq(&**g, guide));
        at.map(|i| a.guides.remove(i))
    });
    ask_for_pass(root);
    drop(removed);
}

pub(crate) fn guide_frame(guide: &NSLayoutGuide) -> NSRect {
    let item = ItemRef::guide(guide);
    // SAFETY: the guide is alive: it was sent a message.
    let Some(owner) = (unsafe { item.host() }) else { return NSRect::ZERO };
    let root = root_of(owner);
    with_engine(root, |e| {
        fresh(root, e);
        e.frame(item)
    })
    .flatten()
    .unwrap_or(NSRect::ZERO)
}

// Questions.

pub(crate) fn item_is_ambiguous(item: ItemRef) -> bool {
    // SAFETY: the item is alive: it, or its anchor, was sent a message.
    let Some(host) = (unsafe { item.host() }) else { return false };
    let root = root_of(host);
    let answer = with_engine(root, |e| {
        fresh(root, e);
        e.contains(item).then(|| e.is_ambiguous(item))
    })
    .flatten();
    // A view the solver doesn't know is placed by its frame, unless it
    // gave that up.
    // SAFETY: as above.
    answer.unwrap_or_else(|| unsafe { item.as_view() }.is_some_and(|v| !view_layout::translates_mask(v)))
}

/// The constraints naming `item` along `axis` (all if none), from the
/// views that could hold them.
pub(crate) fn constraints_affecting(item: ItemRef, axis: Option<Axis>) -> Retained<NSArray<NSLayoutConstraint>> {
    let mut out = Vec::new();
    // SAFETY: as in `item_is_ambiguous`.
    let mut cur = unsafe { item.host() };
    while let Some(v) = cur {
        for c in installed_on(v) {
            let Some(parts) = constraint::parts(&c) else { continue };
            let names = parts
                .first
                .iter()
                .chain(parts.second.iter())
                .any(|t| t.item == item && axis.is_none_or(|a| layout_axis(t.attr) == a));
            if names {
                out.push(c);
            }
        }
        cur = views::superview_of(v);
    }
    NSArray::from_retained_slice(&out)
}

/// Whether the subtree at `view` has constraints of its own, a stack's
/// included.
fn subtree_has_constraints(view: &NSViewImpl) -> bool {
    view_layout::has_auto(view)
        && (read_auto(view, |a| !a.installed.is_empty()).unwrap_or(false)
            || stack::arranges(view)
            || views::subviews(view).iter().any(|s| subtree_has_constraints(views::imp(s))))
}

/// `fittingSize`: the smallest size the view's own constraints allow, or
/// its intrinsic size without any.
pub(crate) fn fitting_size(view: &NSViewImpl) -> NSSize {
    if !subtree_has_constraints(view) {
        let v = views::as_view(view);
        if !v.respondsToSelector(objc2::sel!(intrinsicContentSize)) {
            return NSSize::ZERO;
        }
        // SAFETY: intrinsicContentSize takes nothing and returns an NSSize.
        let size: NSSize = unsafe { objc2::msg_send![v, intrinsicContentSize] };
        let known = |v: f64| if v == NO_INTRINSIC { 0.0 } else { v };
        return NSSize::new(known(size.width), known(size.height));
    }
    let mut e = Engine::new(view, Mode::Fitting);
    e.rebuild(view);
    e.settle();
    let size = e.root_size();
    // What the solver leaves as tiny rounding errors.
    let clean = |v: f64| (v * 1e6).round() / 1e6 + 0.0;
    NSSize::new(clean(size.width), clean(size.height))
}

// NSView's methods, added by a category.

fn anchor<T: Message>(view: &AutoLayoutView, attr: NSLayoutAttribute) -> Retained<T> {
    let v = me(view);
    let object: &AnyObject = views::as_view(v);
    // SAFETY: `of` returns the class the attribute's axis takes, which
    // the caller asked for.
    unsafe { Retained::cast_unchecked(anchor::of(object, ItemRef::view(v), attr)) }
}

fn set_priority(view: &NSViewImpl, index: usize, priority: f32) {
    let changed = with_auto(view, |a| std::mem::replace(&mut a.priorities[index], priority) != priority);
    if changed {
        intrinsic_size_changed(view);
    }
}

fn vertical(orientation: NSLayoutConstraintOrientation) -> usize {
    usize::from(orientation == NSLayoutConstraintOrientation::Vertical)
}

fn installed_here(c: &NSLayoutConstraint, view: &NSViewImpl) -> bool {
    constraint::installed(c).is_some_and(|v| std::ptr::eq(v.as_ptr(), views::as_view(view)))
}

define_class!(
    /// NSView's Auto Layout methods. Receivers are NSViews (the category
    /// adds them to NSView only), never instances of this helper.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepAutoLayoutView"]
    struct AutoLayoutView;

    impl AutoLayoutView {
        #[unsafe(method_id(leadingAnchor))]
        fn leading_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Leading)
        }

        #[unsafe(method_id(trailingAnchor))]
        fn trailing_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Trailing)
        }

        #[unsafe(method_id(leftAnchor))]
        fn left_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Left)
        }

        #[unsafe(method_id(rightAnchor))]
        fn right_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::Right)
        }

        #[unsafe(method_id(topAnchor))]
        fn top_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::Top)
        }

        #[unsafe(method_id(bottomAnchor))]
        fn bottom_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::Bottom)
        }

        #[unsafe(method_id(widthAnchor))]
        fn width_anchor(&self) -> Retained<NSLayoutDimension> {
            anchor(self, NSLayoutAttribute::Width)
        }

        #[unsafe(method_id(heightAnchor))]
        fn height_anchor(&self) -> Retained<NSLayoutDimension> {
            anchor(self, NSLayoutAttribute::Height)
        }

        #[unsafe(method_id(centerXAnchor))]
        fn center_x_anchor(&self) -> Retained<NSLayoutXAxisAnchor> {
            anchor(self, NSLayoutAttribute::CenterX)
        }

        #[unsafe(method_id(centerYAnchor))]
        fn center_y_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::CenterY)
        }

        #[unsafe(method_id(firstBaselineAnchor))]
        fn first_baseline_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::FirstBaseline)
        }

        #[unsafe(method_id(lastBaselineAnchor))]
        fn last_baseline_anchor(&self) -> Retained<NSLayoutYAxisAnchor> {
            anchor(self, NSLayoutAttribute::LastBaseline)
        }

        #[unsafe(method_id(constraints))]
        fn constraints(&self) -> Retained<NSArray<NSLayoutConstraint>> {
            NSArray::from_retained_slice(&installed_on(me(self)))
        }

        #[unsafe(method(addConstraint:))]
        fn add_constraint(&self, constraint: &NSLayoutConstraint) {
            activate(constraint, Some(me(self)));
        }

        #[unsafe(method(addConstraints:))]
        fn add_constraints(&self, constraints: &NSArray<NSLayoutConstraint>) {
            for c in constraints.iter() {
                activate(&c, Some(me(self)));
            }
        }

        #[unsafe(method(removeConstraint:))]
        fn remove_constraint(&self, constraint: &NSLayoutConstraint) {
            if installed_here(constraint, me(self)) {
                deactivate(constraint);
            }
        }

        #[unsafe(method(removeConstraints:))]
        fn remove_constraints(&self, constraints: &NSArray<NSLayoutConstraint>) {
            for c in constraints.iter() {
                if installed_here(&c, me(self)) {
                    deactivate(&c);
                }
            }
        }

        #[unsafe(method(translatesAutoresizingMaskIntoConstraints))]
        fn translates_autoresizing_mask_into_constraints(&self) -> bool {
            view_layout::translates_mask(me(self))
        }

        #[unsafe(method(setTranslatesAutoresizingMaskIntoConstraints:))]
        fn set_translates_autoresizing_mask_into_constraints(&self, flag: bool) {
            let view = me(self);
            if view_layout::translates_mask(view) != flag {
                view_layout::set_translates_mask(view, flag);
                view_layout::needs_update_constraints(view);
                if view_layout::engine_hint(view) {
                    // Its own constraints: a mask's, or an intrinsic size's.
                    let root = root_of(view);
                    with_engine(root, |e| {
                        if !e.stale {
                            e.regenerate(ItemRef::view(view));
                        }
                    });
                    if let Some(sup) = views::superview_of(view) {
                        view_layout::needs_layout(sup);
                    }
                    ask_for_pass(root);
                }
            }
        }

        #[unsafe(method(requiresConstraintBasedLayout))]
        fn requires_constraint_based_layout() -> bool {
            false
        }

        #[unsafe(method(fittingSize))]
        fn fitting_size(&self) -> NSSize {
            fitting_size(me(self))
        }

        #[unsafe(method(alignmentRectInsets))]
        fn alignment_rect_insets(&self) -> NSEdgeInsets {
            NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }
        }

        #[unsafe(method(alignmentRectForFrame:))]
        fn alignment_rect_for_frame(&self, frame: NSRect) -> NSRect {
            let i = views::as_view(me(self)).alignmentRectInsets();
            let flipped = views::superview_of(me(self)).is_some_and(views::is_flipped);
            let y = frame.origin.y + if flipped { i.top } else { i.bottom };
            NSRect::new(
                NSPoint::new(frame.origin.x + i.left, y),
                NSSize::new(frame.size.width - i.left - i.right, frame.size.height - i.top - i.bottom),
            )
        }

        #[unsafe(method(frameForAlignmentRect:))]
        fn frame_for_alignment_rect(&self, rect: NSRect) -> NSRect {
            let i = views::as_view(me(self)).alignmentRectInsets();
            let flipped = views::superview_of(me(self)).is_some_and(views::is_flipped);
            let y = rect.origin.y - if flipped { i.top } else { i.bottom };
            NSRect::new(
                NSPoint::new(rect.origin.x - i.left, y),
                NSSize::new(rect.size.width + i.left + i.right, rect.size.height + i.top + i.bottom),
            )
        }

        #[unsafe(method(firstBaselineOffsetFromTop))]
        fn first_baseline_offset_from_top(&self) -> f64 {
            0.0
        }

        #[unsafe(method(lastBaselineOffsetFromBottom))]
        fn last_baseline_offset_from_bottom(&self) -> f64 {
            0.0
        }

        #[unsafe(method(baselineOffsetFromBottom))]
        fn baseline_offset_from_bottom(&self) -> f64 {
            0.0
        }

        #[unsafe(method(contentHuggingPriorityForOrientation:))]
        fn content_hugging_priority(&self, orientation: NSLayoutConstraintOrientation) -> f32 {
            priorities(me(self))[vertical(orientation)]
        }

        #[unsafe(method(setContentHuggingPriority:forOrientation:))]
        fn set_content_hugging_priority(&self, priority: f32, orientation: NSLayoutConstraintOrientation) {
            set_priority(me(self), vertical(orientation), priority);
        }

        #[unsafe(method(contentCompressionResistancePriorityForOrientation:))]
        fn content_compression_resistance_priority(&self, orientation: NSLayoutConstraintOrientation) -> f32 {
            priorities(me(self))[2 + vertical(orientation)]
        }

        #[unsafe(method(setContentCompressionResistancePriority:forOrientation:))]
        fn set_content_compression_resistance_priority(
            &self,
            priority: f32,
            orientation: NSLayoutConstraintOrientation,
        ) {
            set_priority(me(self), 2 + vertical(orientation), priority);
        }

        #[unsafe(method(addLayoutGuide:))]
        fn add_layout_guide(&self, guide: &NSLayoutGuide) {
            add_guide(views::as_view(me(self)), guide);
        }

        #[unsafe(method(removeLayoutGuide:))]
        fn remove_layout_guide(&self, guide: &NSLayoutGuide) {
            let view = views::as_view(me(self));
            if guide::owner(guide).is_some_and(|o| std::ptr::eq(o.as_ptr(), view)) {
                remove_guide(view, guide);
            }
        }

        #[unsafe(method_id(layoutGuides))]
        fn layout_guides(&self) -> Retained<NSArray<NSLayoutGuide>> {
            let guides = read_auto(me(self), |a| a.guides.clone()).unwrap_or_default();
            NSArray::from_retained_slice(&guides)
        }

        #[unsafe(method(hasAmbiguousLayout))]
        fn has_ambiguous_layout(&self) -> bool {
            item_is_ambiguous(ItemRef::view(me(self)))
        }

        #[unsafe(method(exerciseAmbiguityInLayout))]
        fn exercise_ambiguity_in_layout(&self) {}

        #[unsafe(method_id(constraintsAffectingLayoutForOrientation:))]
        fn constraints_affecting_layout_for_orientation(
            &self,
            orientation: NSLayoutConstraintOrientation,
        ) -> Retained<NSArray<NSLayoutConstraint>> {
            constraints_affecting(ItemRef::view(me(self)), Some(Axis::of(orientation)))
        }
    }
);

/// The NSView a category method was sent to.
fn me(this: &AutoLayoutView) -> &NSViewImpl {
    // SAFETY: the category adds these methods to NSView only, so the
    // receiver is an NSView, whose layout NSViewImpl describes.
    unsafe { &*(this as *const AutoLayoutView).cast::<NSViewImpl>() }
}

sidestep_runtime::category!("NSView"(SidestepAutoLayout), |category| {
    // SAFETY: the helper's methods treat their receiver as an NSView.
    unsafe { category.add_methods_of(AutoLayoutView::class()) };
});
