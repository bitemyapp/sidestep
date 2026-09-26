//! The NSView contract beyond geometry: the hierarchy methods and the
//! messages AppKit sends as views move, hide and show; per-view state
//! (layer flags, the identifier, scaled bounds); the scrolling
//! helpers; and the layout pass.
//!
//! **Callbacks.** Moving a view sends, in AppKit's order (which
//! `conformance/tests/appkit_views.rs` pins):
//!
//! - `viewWillMoveToSuperview:` to the view;
//! - `willRemoveSubview:` to its old superview, then `didAddSubview:` to
//!   the new one;
//! - `viewDidMoveToSuperview`;
//! - and, when either superview is in a window (the same one included),
//!   `viewWillMoveToWindow:` and `viewDidMoveToWindow` to the view and each
//!   of its subviews, a view hearing before and after its subviews.
//!
//! A view that joins or leaves a hidden ancestor, or whose own `hidden`
//! changes while no ancestor is hidden, hears `viewDidHide` or
//! `viewDidUnhide`, and so do its subviews down to the first hidden one.
//!
//! Program code runs in every callback and can move views itself, so what
//! a move does next is read again after each one: the view leaves whatever
//! superview it has by then, and joins the window its place then puts it
//! in.
//!
//! **The layout pass.** Every view carries two flags for its own work
//! (`needsLayout`, `needsUpdateConstraints`, both set on a new view, as in
//! AppKit) and two saying some view below it has work, which setting a flag
//! raises on each ancestor until one already has it. A round walks only
//! flagged subtrees, once each: `updateConstraints` children first, then
//! Auto Layout solves, then `layout` parents first; a view's own request
//! made while it runs is dropped, as on macOS, so a view asking for layout
//! in its `layout` is laid out once. Rounds repeat while one leaves work
//! behind (a `layout` flagging its superview, or adding views that need
//! their constraints), at most `MAX_ROUNDS` times, so a pass costs at most
//! that many walks of the flagged views. The window runs it before each
//! frame (`run`, from the display pass), then sends `viewWillDraw` down the
//! tree when there is something to draw. `layoutSubtreeIfNeeded` and
//! NSWindow's `layoutIfNeeded` and `updateConstraintsIfNeeded` run it on
//! demand.
//!
//! Frames stay the source of truth. Autoresizing happens as a frame changes,
//! as it always has here; macOS defers it to the layout pass once Auto
//! Layout has run in a window, which only changes when a program sees it.
//!
//! The methods reach NSView through a link-time category
//! (`SidestepViewContract`), so the classes that define views don't list
//! them.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_app_kit::{
    NSClipView, NSEvent, NSScrollView, NSView, NSViewLayerContentsRedrawPolicy, NSWindow, NSWindowOrderingMode,
};
use objc2_foundation::{
    NSAlignmentOptions, NSArray, NSComparisonResult, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::views::{self, NSViewImpl};
use crate::window::NSWindowImpl;

/// The view needs `layout`.
const NEEDS_LAYOUT: u16 = 1 << 0;
/// Some view below this one needs `layout`.
const SUBTREE_LAYOUT: u16 = 1 << 1;
/// The view needs `updateConstraints`.
const NEEDS_UPDATE: u16 = 1 << 2;
/// Some view below this one needs `updateConstraints`.
const SUBTREE_UPDATE: u16 = 1 << 3;
/// `autoresizesSubviews` is NO.
const NO_AUTORESIZE: u16 = 1 << 4;
const WANTS_LAYER: u16 = 1 << 5;
/// `translatesAutoresizingMaskIntoConstraints` is NO.
const NO_TRANSLATE: u16 = 1 << 6;
/// The view has been an item of its tree's Auto Layout engine (a hint: it
/// may have left since).
const IN_ENGINE: u16 = 1 << 7;
/// The view needs layout when its clip view scrolls or resizes (a table
/// realizing its visible rows).
const FOLLOWS_CLIP: u16 = 1 << 8;
/// Something in the subtree has taken part in Auto Layout (constraints,
/// guides, a stack's arranged views). Raised on the ancestors like the
/// work flags and never cleared, so trees that never used Auto Layout
/// skip its bookkeeping when views move.
const SUBTREE_AUTO: u16 = 1 << 9;

const ANY_WORK: u16 = NEEDS_LAYOUT | SUBTREE_LAYOUT | NEEDS_UPDATE | SUBTREE_UPDATE;

/// How many times a pass goes round before giving up on views that keep
/// asking for layout (a layout cycle).
const MAX_ROUNDS: usize = 16;

/// A view's part of the contract, kept in its ivars.
pub(crate) struct ViewState {
    flags: Cell<u16>,
    redraw_policy: Cell<NSViewLayerContentsRedrawPolicy>,
    /// The bounds size when it differs from the frame's (`setBoundsSize:`),
    /// kept in proportion as the frame resizes. Stored and reported by
    /// `bounds` only: drawing, hit testing and conversion don't scale yet.
    bounds_size: Cell<Option<NSSize>>,
    identifier: RefCell<Option<Retained<NSString>>>,
    prepared: Cell<NSRect>,
    /// Auto Layout's state for a view that takes part, made when it first
    /// does (see `autolayout`).
    pub(crate) auto: RefCell<Option<Box<crate::autolayout::ViewAuto>>>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            flags: Cell::new(NEEDS_LAYOUT | NEEDS_UPDATE),
            redraw_policy: Cell::new(NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay),
            bounds_size: Cell::new(None),
            identifier: RefCell::new(None),
            prepared: Cell::new(NSRect::ZERO),
            auto: RefCell::new(None),
        }
    }
}

fn state(view: &NSViewImpl) -> &ViewState {
    &view.ivars().state
}

fn has(view: &NSViewImpl, flag: u16) -> bool {
    state(view).flags.get() & flag != 0
}

fn set_flag(view: &NSViewImpl, flag: u16, on: bool) {
    let flags = &state(view).flags;
    flags.set(if on { flags.get() | flag } else { flags.get() & !flag });
}

pub(crate) fn translates_mask(view: &NSViewImpl) -> bool {
    !has(view, NO_TRANSLATE)
}

pub(crate) fn set_translates_mask(view: &NSViewImpl, on: bool) {
    set_flag(view, NO_TRANSLATE, !on);
}

pub(crate) fn autoresizes_subviews(view: &NSViewImpl) -> bool {
    !has(view, NO_AUTORESIZE)
}

pub(crate) fn engine_hint(view: &NSViewImpl) -> bool {
    has(view, IN_ENGINE)
}

pub(crate) fn set_engine_hint(view: &NSViewImpl) {
    set_flag(view, IN_ENGINE, true);
    mark_auto(view);
}

/// Note that `view` takes part in Auto Layout, on it and its ancestors.
pub(crate) fn mark_auto(view: &NSViewImpl) {
    let mut cur = Some(view);
    while let Some(v) = cur {
        if has(v, SUBTREE_AUTO) {
            break;
        }
        set_flag(v, SUBTREE_AUTO, true);
        cur = views::superview_of(v);
    }
}

/// Whether anything in the subtree at `view` may take part in Auto Layout.
pub(crate) fn has_auto(view: &NSViewImpl) -> bool {
    has(view, SUBTREE_AUTO)
}

/// Lay `view` out again whenever the clip view it is the document of
/// scrolls or resizes.
pub(crate) fn follow_clip_view(view: &NSViewImpl) {
    set_flag(view, FOLLOWS_CLIP, true);
}

/// A clip view scrolled or resized: documents that follow it need layout.
pub(crate) fn clip_moved(clip: &NSViewImpl) {
    for sub in views::subviews(clip).iter() {
        let s = views::imp(sub);
        if has(s, FOLLOWS_CLIP) {
            needs_layout(s);
        }
    }
}

// Hierarchy.

/// Where `add_subview` puts a view among its new siblings.
#[derive(Clone, Copy)]
pub(crate) enum Place<'a> {
    /// `addSubview:`.
    Top,
    /// Above all the others, from `addSubview:positioned:relativeTo:`.
    Front,
    Bottom,
    Above(&'a NSView),
    Below(&'a NSView),
    /// In the place of this subview, which then leaves (`replaceSubview:with:`).
    Instead(&'a NSView),
}

/// `addSubview:` and its relatives. A view that is already a subview of
/// `this` only changes places, silently, except that `addSubview:` moves it
/// to the top by taking it out and putting it back, with the messages.
pub(crate) fn add_subview(this: &NSViewImpl, view: &NSView, place: Place<'_>) {
    let v = views::imp(view);
    assert!(!std::ptr::eq(v, this), "sidestep: -[NSView addSubview:] can't add a view to itself");
    let count = views::subviews(this).len();
    let index = |other: &NSView| views::index_of(this, views::imp(other));
    let own = views::superview_of(v).is_some_and(|s| std::ptr::eq(s, this));
    if own {
        let from = views::index_of(this, v).expect("a subview");
        let to = match place {
            // Already on top: nothing happens.
            Place::Top if from + 1 == count => return,
            Place::Top => None,
            Place::Front => Some(count),
            Place::Bottom => Some(0),
            Place::Above(other) => index(other).map(|i| i + 1),
            Place::Below(other) => index(other),
            Place::Instead(other) => index(other),
        };
        if let Some(to) = to {
            let mut order: Vec<Retained<NSView>> = views::subviews(this).to_vec();
            let moving = order.remove(from);
            let to = if to > from { to - 1 } else { to };
            order.insert(to.min(order.len()), moving);
            views::reorder(this, order);
            return;
        }
        // addSubview: of a subview below the top: out and back in.
    }
    let at = match place {
        Place::Top | Place::Front => usize::MAX,
        Place::Bottom => 0,
        Place::Above(other) => index(other).map_or(usize::MAX, |i| i + 1),
        Place::Below(other) => index(other).unwrap_or(0),
        Place::Instead(other) => index(other).unwrap_or(usize::MAX),
    };
    move_view(this, view, at);
}

/// Move `view` from wherever it is to `this`'s subviews at `at` (past the
/// end means on top), with every message.
fn move_view(this: &NSViewImpl, view: &NSView, at: usize) {
    let view = view.retain();
    let v = views::imp(&view);
    let this_view = views::as_view(this).retain();
    let cycle = "sidestep: -[NSView addSubview:] can't add a view to its own subview";
    assert!(!is_descendant(this, v), "{cycle}");
    view.viewWillMoveToSuperview(Some(&this_view));
    // Where the view is now, after program code ran.
    let old = views::superview_of(v).map(|s| views::as_view(s).retain());
    let old_window = views::window_of(v).is_some();
    let was_hidden = old.as_deref().is_some_and(|o| views::is_hidden_or_has_hidden_ancestor(views::imp(o)));
    if let Some(old) = &old {
        old.willRemoveSubview(&view);
    }
    leave_superview(v, true, Some(this));
    assert!(!is_descendant(this, v), "{cycle}");
    let now_hidden = views::is_hidden_or_has_hidden_ancestor(this);
    // Sibling indexes after the move out.
    views::link(this, &view, at);
    raise_work(this, state(v).flags.get());
    if has(v, SUBTREE_AUTO) {
        mark_auto(this);
    }
    crate::autolayout::joined(v);
    this_view.didAddSubview(&view);
    if !views::is_hidden(v) && now_hidden && !was_hidden {
        tell_hidden(v, true);
    }
    view.viewDidMoveToSuperview();
    // Still here: into the window it is in now (a callback may have moved
    // it on, and the move that did told it).
    let here = views::superview_of(v).is_some_and(|s| std::ptr::eq(s, this));
    let window = views::window_of(this).map(|w| NonNull::from(w.as_window()));
    if here && (old_window || window.is_some()) {
        views::set_window(v, window);
    }
    // Its effective appearance may have changed with its superview.
    crate::appearance::refresh(&view);
    // Drawn where it now is, in the window it is now in.
    views::invalidate(v, views::bounds(v));
}

/// Take `view` out of whatever superview it has, on its way to `to`, with
/// Auto Layout's bookkeeping but no messages.
fn leave_superview(view: &NSViewImpl, display: bool, to: Option<&NSViewImpl>) {
    let Some(sup) = views::superview_of(view) else { return };
    let sup = views::as_view(sup).retain();
    crate::autolayout::leaving(view, views::imp(&sup), to);
    views::unlink(view, display);
}

/// `removeFromSuperview`, and without redrawing where the view was when
/// `display` is false.
pub(crate) fn remove_from_superview(view: &NSViewImpl, display: bool) {
    let Some(sup) = views::superview_of(view) else { return };
    let sup = views::as_view(sup).retain();
    let this = views::as_view(view).retain();
    this.viewWillMoveToSuperview(None);
    if views::superview_of(view).is_some_and(views::is_hidden_or_has_hidden_ancestor) && !views::is_hidden(view) {
        tell_hidden(view, false);
    }
    // The superview it has now, after program code ran.
    if let Some(now) = views::superview_of(view) {
        views::as_view(now).willRemoveSubview(&this);
    }
    leave_superview(view, display, None);
    crate::autolayout::became_root(view);
    this.viewDidMoveToSuperview();
    if views::superview_of(view).is_none() && views::window_of(view).is_some() {
        views::set_window(view, None);
    }
    crate::appearance::refresh(&this);
    drop(sup);
}

/// `setSubviews:`: the views not in `new` leave (in their old order), the
/// new ones join (in theirs), and the result is in `new`'s order.
fn set_subviews(this: &NSViewImpl, new: &NSArray<NSView>) {
    let new: Vec<Retained<NSView>> = new.to_vec();
    let key = |v: &NSView| (v as *const NSView).addr();
    let wanted: HashSet<usize> = new.iter().map(|v| key(v)).collect();
    for old in views::subviews(this) {
        if !wanted.contains(&key(&old)) {
            remove_from_superview(views::imp(&old), true);
        }
    }
    for view in &new {
        let own = views::superview_of(views::imp(view)).is_some_and(|s| std::ptr::eq(s, this));
        if !own {
            move_view(this, view, usize::MAX);
        }
    }
    // Only subviews remain; put them in the order asked for, each once.
    let mut seen = HashSet::with_capacity(new.len());
    let order: Vec<Retained<NSView>> = new
        .into_iter()
        .filter(|v| views::superview_of(views::imp(v)).is_some_and(|s| std::ptr::eq(s, this)) && seen.insert(key(v)))
        .collect();
    if order.len() == views::subviews(this).len() {
        views::reorder(this, order);
    }
}

/// `sortSubviewsUsingFunction:context:`, a stable sort.
type Compare = unsafe extern "C-unwind" fn(NonNull<NSView>, NonNull<NSView>, *mut c_void) -> NSComparisonResult;

/// Sorted with a merge sort of its own: the program's function need not
/// be a consistent order (std's sorts may panic when it isn't), and it
/// may change the subviews, in which case they are left as they are.
fn sort_subviews(this: &NSViewImpl, compare: Compare, context: *mut c_void) {
    let before = views::subviews(this);
    let mut order: Vec<Retained<NSView>> = before.to_vec();
    let mut after = |a: &Retained<NSView>, b: &Retained<NSView>| {
        // SAFETY: the caller's function compares two views with its
        // context, as sortSubviewsUsingFunction:context: promises it.
        let result = unsafe { compare(NonNull::from(&**a), NonNull::from(&**b), context) };
        result == NSComparisonResult::Descending
    };
    merge_sort(&mut order, &mut after);
    let now = views::subviews(this);
    let same = now.len() == before.len() && now.iter().zip(before.iter()).all(|(a, b)| std::ptr::eq(&**a, &**b));
    if same {
        views::reorder(this, order);
    }
}

/// A stable merge sort putting `b` before `a` only when `after(a, b)`,
/// whatever `after` says.
fn merge_sort<T: Clone>(list: &mut [T], after: &mut impl FnMut(&T, &T) -> bool) {
    if list.len() < 2 {
        return;
    }
    let mid = list.len() / 2;
    merge_sort(&mut list[..mid], after);
    merge_sort(&mut list[mid..], after);
    let (left, right) = (list[..mid].to_vec(), list[mid..].to_vec());
    let (mut i, mut j) = (0, 0);
    for slot in list.iter_mut() {
        let take_right = i == left.len() || (j < right.len() && after(&left[i], &right[j]));
        *slot = if take_right {
            j += 1;
            right[j - 1].clone()
        } else {
            i += 1;
            left[i - 1].clone()
        };
    }
}

/// Whether `view` is `ancestor` or below it.
pub(crate) fn is_descendant(view: &NSViewImpl, ancestor: &NSViewImpl) -> bool {
    let mut cur = Some(view);
    while let Some(v) = cur {
        if std::ptr::eq(v, ancestor) {
            return true;
        }
        cur = views::superview_of(v);
    }
    false
}

/// The nearest view both are, or are below.
pub(crate) fn common_ancestor<'a>(a: &'a NSViewImpl, b: &'a NSViewImpl) -> Option<&'a NSViewImpl> {
    let (da, db) = (views::depth(a), views::depth(b));
    let (mut a, mut b) = (a, b);
    for _ in db..da {
        a = views::superview_of(a)?;
    }
    for _ in da..db {
        b = views::superview_of(b)?;
    }
    loop {
        if std::ptr::eq(a, b) {
            return Some(a);
        }
        a = views::superview_of(a)?;
        b = views::superview_of(b)?;
    }
}

fn enclosing_scroll_view(view: &NSViewImpl) -> Option<Retained<NSScrollView>> {
    let mut cur = views::superview_of(view);
    while let Some(v) = cur {
        let view = views::as_view(v);
        if view.isKindOfClass(NSScrollView::class()) {
            // SAFETY: an instance of NSScrollView or a subclass.
            return Some(unsafe { Retained::cast_unchecked(view.retain()) });
        }
        cur = views::superview_of(v);
    }
    None
}

// Hiding.

/// `hidden` changed: tell the view and its shown subviews, unless an
/// ancestor hides them all anyway.
pub(crate) fn hidden_changed(view: &NSViewImpl, hidden: bool) {
    if !views::superview_of(view).is_some_and(views::is_hidden_or_has_hidden_ancestor) {
        tell_hidden(view, hidden);
    }
    if let Some(sup) = views::superview_of(view) {
        crate::autolayout::subview_hidden(sup, view);
    }
}

/// Tell a view, and its subviews down to hidden ones, that it is now shown
/// or hidden.
fn tell_hidden(view: &NSViewImpl, hidden: bool) {
    let v = views::as_view(view);
    if hidden {
        v.viewDidHide()
    } else {
        v.viewDidUnhide()
    }
    for sub in views::subviews(view) {
        let s = views::imp(&sub);
        if !views::is_hidden(s) {
            tell_hidden(s, hidden);
        }
    }
}

// Geometry.

/// `bounds`: the origin and, unless `setBoundsSize:` scaled it, the frame's
/// size.
pub(crate) fn bounds(view: &NSViewImpl) -> NSRect {
    let b = views::bounds(view);
    match state(view).bounds_size.get() {
        Some(size) => NSRect::new(b.origin, size),
        None => b,
    }
}

fn set_bounds_size(view: &NSViewImpl, size: NSSize) {
    let frame = views::frame(view).size;
    state(view).bounds_size.set((size != frame).then_some(size));
}

/// The frame changed from `old`; the new one is set.
pub(crate) fn frame_changed(view: &NSViewImpl, old: NSRect) {
    let new = views::frame(view);
    if old.size != new.size {
        if let Some(b) = state(view).bounds_size.get() {
            // Scaled bounds keep their scale.
            let scale = |b: f64, new: f64, old: f64| if old == 0.0 { b } else { b * new / old };
            let size = NSSize::new(
                scale(b.width, new.size.width, old.size.width),
                scale(b.height, new.size.height, old.size.height),
            );
            state(view).bounds_size.set((size != new.size).then_some(size));
        }
        needs_layout(view);
        if views::is_clip(view) {
            clip_moved(view);
        }
        // As on macOS, a view without subviews isn't asked.
        if !has(view, NO_AUTORESIZE) && !views::subviews(view).is_empty() {
            // SAFETY: resizeSubviewsWithOldSize: takes an NSSize.
            unsafe { msg_send![view, resizeSubviewsWithOldSize: old.size] }
        }
    }
    crate::autolayout::frame_changed(view);
}

fn intersect(a: NSRect, b: NSRect) -> NSRect {
    let x0 = a.origin.x.max(b.origin.x);
    let y0 = a.origin.y.max(b.origin.y);
    let x1 = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    if x1 <= x0 || y1 <= y0 {
        return NSRect::ZERO;
    }
    NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0))
}

fn map_rect(xf: &crate::graphics::Xf, r: NSRect) -> NSRect {
    let (x0, y0) = xf.point(r.origin.x, r.origin.y);
    let (x1, y1) = xf.point(r.origin.x + r.size.width, r.origin.y + r.size.height);
    NSRect::new(NSPoint::new(x0.min(x1), y0.min(y1)), NSSize::new((x1 - x0).abs(), (y1 - y0).abs()))
}

/// `visibleRect`: the part of the view its ancestors' bounds leave, in its
/// coordinates, window or not. Hidden views show nothing.
pub(crate) fn visible_rect(view: &NSViewImpl) -> NSRect {
    let mut to_cur = crate::graphics::Xf::IDENTITY;
    let mut rect = views::bounds(view);
    let mut cur = view;
    loop {
        if views::is_hidden(cur) {
            return NSRect::ZERO;
        }
        let Some(sup) = views::superview_of(cur) else { break };
        let step = views::step(cur, views::is_flipped(sup), views::frame(cur));
        to_cur = to_cur.then(&step);
        rect = intersect(map_rect(&step, rect), views::bounds(sup));
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            return NSRect::ZERO;
        }
        cur = sup;
    }
    map_rect(&to_cur.inverse(), rect)
}

/// The scale of the window's backing store, or 1 outside a window.
fn backing_scale(view: &NSViewImpl) -> f64 {
    views::window_of(view).map_or(1.0, |w| w.as_window().backingScaleFactor())
}

/// `backingAlignedRect:options:`: align each edge or size the options name
/// to the backing store's pixels, in window coordinates.
fn backing_aligned(view: &NSViewImpl, rect: NSRect, options: NSAlignmentOptions) -> NSRect {
    let to_window = views::to_window(view);
    let scale = backing_scale(view);
    let r = map_rect(&to_window, rect);
    // A flipped view's minimum y is the window's maximum.
    let options = if to_window.a < 0.0 { swap_y(options) } else { options };
    let (x, w) = align_axis(r.origin.x * scale, r.size.width * scale, options.0, 0);
    let (y, h) = align_axis(r.origin.y * scale, r.size.height * scale, options.0, 1);
    let aligned = NSRect::new(NSPoint::new(x / scale, y / scale), NSSize::new(w / scale, h / scale));
    map_rect(&to_window.inverse(), aligned)
}

fn swap_y(options: NSAlignmentOptions) -> NSAlignmentOptions {
    // Minimum y is bit 1 of each group of options, maximum y bit 3.
    let mut bits = options.0 & !(0b1010 | 0b1010 << 8 | 0b1010 << 16);
    for group in [0, 8, 16] {
        if options.0 & (0b10 << group) != 0 {
            bits |= 0b1000 << group;
        }
        if options.0 & (0b1000 << group) != 0 {
            bits |= 0b10 << group;
        }
    }
    NSAlignmentOptions(bits)
}

/// One axis of an alignment, in device pixels: `axis` 0 is x, 1 is y. Of
/// the minimum edge, the maximum edge and the size, the options name two;
/// the third follows from them.
fn align_axis(min: f64, size: f64, options: u64, axis: u32) -> (f64, f64) {
    // Bits for this axis's minimum, maximum and size in each group.
    let bit = |group: u32, which: u32| options & (1u64 << (group + which * 2 + axis)) != 0;
    // (inward, outward, nearest) rounding for an edge that grows the rect
    // outward by going down (a minimum) or up (a maximum and a size).
    let round = |v: f64, which: u32| {
        let down = which == 0;
        if bit(0, which) {
            if down { v.ceil() } else { v.floor() }
        } else if bit(8, which) {
            if down { v.floor() } else { v.ceil() }
        } else if bit(16, which) {
            (v + 0.5).floor()
        } else {
            v
        }
    };
    let named = |which: u32| bit(0, which) || bit(8, which) || bit(16, which);
    let max = min + size;
    match (named(0), named(1), named(2)) {
        (true, _, true) => {
            let lo = round(min, 0);
            (lo, round(size, 2))
        }
        (_, true, true) => {
            let hi = round(max, 1);
            let s = round(size, 2);
            (hi - s, s)
        }
        (true, true, false) => {
            let lo = round(min, 0);
            (lo, round(max, 1) - lo)
        }
        _ => (min, size),
    }
}

// Scrolling.

/// The nearest clip view at or above `view`.
fn clip_of(view: &NSViewImpl) -> Option<&NSViewImpl> {
    let mut cur = Some(view);
    while let Some(v) = cur {
        if views::is_clip(v) {
            return Some(v);
        }
        cur = views::superview_of(v);
    }
    None
}

fn as_clip(view: &NSViewImpl) -> &NSClipView {
    // SAFETY: every view marked as a clip view is an NSClipView.
    unsafe { &*(view as *const NSViewImpl).cast::<NSClipView>() }
}

/// Scroll `clip` to `origin`, kept within its document, and tell its scroll
/// view. Returns whether it moved.
fn scroll_clip_to(clip: &NSViewImpl, origin: NSPoint) -> bool {
    let c = as_clip(clip);
    let bounds = c.bounds();
    let target = c.constrainBoundsRect(NSRect::new(origin, bounds.size)).origin;
    if target == bounds.origin {
        return false;
    }
    c.scrollToPoint(target);
    if let Some(sup) = views::superview_of(clip) {
        let sup = views::as_view(sup);
        if sup.isKindOfClass(NSScrollView::class()) {
            // SAFETY: an NSScrollView, which takes the clip view.
            let _: () = unsafe { msg_send![sup, reflectScrolledClipView: c] };
        }
    }
    true
}

fn scroll_point(view: &NSViewImpl, point: NSPoint) {
    let Some(clip) = clip_of(view) else { return };
    let p = views::as_view(clip).convertPoint_fromView(point, Some(views::as_view(view)));
    scroll_clip_to(clip, p);
}

/// Where a viewport spanning `vis` along one axis moves to show as much of
/// `want` as it can, moving as little as it can.
fn reveal(vis_min: f64, vis_size: f64, want_min: f64, want_size: f64) -> f64 {
    let (vis_max, want_max) = (vis_min + vis_size, want_min + want_size);
    if want_min >= vis_min && want_max <= vis_max || want_min < vis_min && want_max > vis_max {
        vis_min
    } else if want_max > vis_max {
        want_min.min(want_max - vis_size)
    } else {
        want_min.max(want_max - vis_size)
    }
}

fn scroll_rect_to_visible(view: &NSViewImpl, rect: NSRect) -> bool {
    let Some(clip) = clip_of(view) else { return false };
    let c = views::as_view(clip);
    let r = c.convertRect_fromView(rect, Some(views::as_view(view)));
    let b = views::bounds(clip);
    let x = reveal(b.origin.x, b.size.width, r.origin.x, r.size.width);
    let y = reveal(b.origin.y, b.size.height, r.origin.y, r.size.height);
    let scrolled = scroll_clip_to(clip, NSPoint::new(x, y));
    // Enclosing scroll views show the part this one does.
    let outer = views::superview_of(clip).is_some_and(|sup| {
        let shown = intersect(r, views::bounds(clip));
        let shown = if shown.size.width > 0.0 { shown } else { r };
        let sup = views::as_view(sup);
        sup.scrollRectToVisible(sup.convertRect_fromView(shown, Some(c)))
    });
    scrolled || outer
}

fn autoscroll(view: &NSViewImpl, event: &NSEvent) -> bool {
    let Some(clip) = clip_of(view) else { return false };
    let c = views::as_view(clip);
    let p = c.convertPoint_fromView(event.locationInWindow(), None);
    let b = views::bounds(clip);
    let inside =
        p.x >= b.origin.x && p.y >= b.origin.y && p.x < b.origin.x + b.size.width && p.y < b.origin.y + b.size.height;
    !inside && scroll_rect_to_visible(clip, NSRect::new(p, NSSize::new(1.0, 1.0)))
}

// The layout pass.

/// Raise `work`'s flags for the subtree on `view` and every ancestor, and
/// ask the window for a pass.
fn raise_work(view: &NSViewImpl, work: u16) {
    let mut bits = 0;
    if work & (NEEDS_LAYOUT | SUBTREE_LAYOUT) != 0 {
        bits |= SUBTREE_LAYOUT;
    }
    if work & (NEEDS_UPDATE | SUBTREE_UPDATE) != 0 {
        bits |= SUBTREE_UPDATE;
    }
    if bits == 0 {
        return;
    }
    let mut cur = Some(view);
    while let Some(v) = cur {
        if state(v).flags.get() & bits == bits {
            break;
        }
        set_flag(v, bits, true);
        cur = views::superview_of(v);
    }
    // The walk may stop early; ask the window whatever it found.
    let mut top = view;
    while let Some(sup) = views::superview_of(top) {
        top = sup;
    }
    if let Some(window) = views::window_of(top) {
        window.needs_layout_pass();
    }
}

/// `setNeedsLayout:YES`.
pub(crate) fn needs_layout(view: &NSViewImpl) {
    if !has(view, NEEDS_LAYOUT) {
        set_flag(view, NEEDS_LAYOUT, true);
        if let Some(sup) = views::superview_of(view) {
            raise_work(sup, NEEDS_LAYOUT);
        } else if let Some(window) = views::window_of(view) {
            window.needs_layout_pass();
        }
    }
}

/// `setNeedsUpdateConstraints:YES`.
pub(crate) fn needs_update_constraints(view: &NSViewImpl) {
    if !has(view, NEEDS_UPDATE) {
        set_flag(view, NEEDS_UPDATE, true);
        if let Some(sup) = views::superview_of(view) {
            raise_work(sup, NEEDS_UPDATE);
        } else if let Some(window) = views::window_of(view) {
            window.needs_layout_pass();
        }
    }
}

/// Send `updateConstraints` to the flagged views under `view`, subviews
/// before their superview, once each. A view's own request made while it
/// updates is dropped (AppKit raises); requests for others stay for the
/// next round.
fn update_under(view: &NSViewImpl) {
    if has(view, SUBTREE_UPDATE) {
        set_flag(view, SUBTREE_UPDATE, false);
        for sub in views::subviews(view) {
            update_under(views::imp(&sub));
        }
    }
    if has(view, NEEDS_UPDATE) {
        views::as_view(view).updateConstraints();
        set_flag(view, NEEDS_UPDATE, false);
    }
}

/// Send `layout` to the flagged views under `view`, superviews before
/// their subviews, once each. A view's own request made while it lays out
/// is dropped, as on macOS; requests for others stay for the next round.
fn layout_under(view: &NSViewImpl) {
    if has(view, NEEDS_LAYOUT) {
        views::as_view(view).layout();
        set_flag(view, NEEDS_LAYOUT, false);
    }
    if has(view, SUBTREE_LAYOUT) {
        set_flag(view, SUBTREE_LAYOUT, false);
        for sub in views::subviews(view) {
            layout_under(views::imp(&sub));
        }
    }
}

/// Bring the subtree at `view` up to date: constraints, the solver, then
/// layout, until no view asks for more.
fn lay_out(view: &NSViewImpl, layout: bool) {
    for _ in 0..MAX_ROUNDS {
        let flags = state(view).flags.get();
        let pending = if layout { ANY_WORK } else { NEEDS_UPDATE | SUBTREE_UPDATE };
        if flags & pending == 0 && !(layout && crate::autolayout::needs_solve(view)) {
            return;
        }
        update_under(view);
        if !layout {
            return;
        }
        crate::autolayout::solve(view);
        layout_under(view);
    }
}

/// The window's pass before a frame: layout, then `viewWillDraw` if
/// anything will be drawn, and layout again for anything that asked for it
/// there.
pub(crate) fn run(window: &NSWindowImpl) {
    let Some(content) = window.content() else { return };
    let root = views::imp(&content);
    lay_out(root, true);
    if window.has_damage() {
        content.viewWillDraw();
        lay_out(root, true);
    }
}

/// `display`, `displayIfNeeded` and the like: draw now if the window can,
/// else lay out and prepare as AppKit does for a window off screen.
fn display_now(view: &NSViewImpl) {
    // Held: the pass runs program code, which may let the window go.
    let Some(window) = views::window_of(view).map(|w| w.as_window().retain()) else { return };
    let window_imp = crate::window::imp(&window);
    if window.isVisible() {
        crate::window::display_if_needed(window_imp);
    } else if let Some(content) = window_imp.content() {
        lay_out(views::imp(&content), true);
        views::as_view(view).viewWillDraw();
    }
}

// NSView's methods, added by a category.

define_class!(
    /// The contract's NSView methods. Receivers are NSViews (the category
    /// adds them to NSView only), never instances of this helper.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepViewContract"]
    struct ViewContract;

    impl ViewContract {
        #[unsafe(method_id(subviews))]
        fn subviews(&self) -> Retained<NSArray<NSView>> {
            NSArray::from_retained_slice(&views::subviews(me(self)))
        }

        #[unsafe(method(setSubviews:))]
        fn set_subviews(&self, subviews: &NSArray<NSView>) {
            set_subviews(me(self), subviews);
        }

        #[unsafe(method(addSubview:positioned:relativeTo:))]
        fn add_subview_positioned(&self, view: &NSView, place: NSWindowOrderingMode, other: Option<&NSView>) {
            let place = match (place, other) {
                (NSWindowOrderingMode::Below, Some(other)) => Place::Below(other),
                (NSWindowOrderingMode::Below, None) => Place::Bottom,
                (_, Some(other)) => Place::Above(other),
                (_, None) => Place::Front,
            };
            add_subview(me(self), view, place);
        }

        #[unsafe(method(replaceSubview:with:))]
        fn replace_subview(&self, old: &NSView, new: &NSView) {
            let this = me(self);
            if views::index_of(this, views::imp(old)).is_none() || std::ptr::eq(old, new) {
                return;
            }
            let old = old.retain();
            add_subview(this, new, Place::Instead(&old));
            remove_from_superview(views::imp(&old), true);
        }

        #[unsafe(method(sortSubviewsUsingFunction:context:))]
        fn sort_subviews_using_function(&self, compare: Compare, context: *mut c_void) {
            sort_subviews(me(self), compare, context);
        }

        #[unsafe(method(isDescendantOf:))]
        fn is_descendant_of(&self, view: &NSView) -> bool {
            is_descendant(me(self), views::imp(view))
        }

        #[unsafe(method_id(ancestorSharedWithView:))]
        fn ancestor_shared_with_view(&self, view: &NSView) -> Option<Retained<NSView>> {
            common_ancestor(me(self), views::imp(view)).map(|v| views::as_view(v).retain())
        }

        #[unsafe(method_id(enclosingScrollView))]
        fn enclosing_scroll_view(&self) -> Option<Retained<NSScrollView>> {
            enclosing_scroll_view(me(self))
        }

        #[unsafe(method(removeFromSuperviewWithoutNeedingDisplay))]
        fn remove_from_superview_without_needing_display(&self) {
            remove_from_superview(me(self), false);
        }

        #[unsafe(method_id(identifier))]
        fn identifier(&self) -> Option<Retained<NSString>> {
            state(me(self)).identifier.borrow().clone()
        }

        #[unsafe(method(setIdentifier:))]
        fn set_identifier(&self, identifier: Option<&NSString>) {
            let old = state(me(self)).identifier.replace(identifier.map(objc2_foundation::NSCopying::copy));
            drop(old);
        }

        // Callbacks: nothing by default.

        #[unsafe(method(viewWillMoveToSuperview:))]
        fn view_will_move_to_superview(&self, _superview: Option<&NSView>) {}

        #[unsafe(method(viewDidMoveToSuperview))]
        fn view_did_move_to_superview(&self) {}

        #[unsafe(method(viewWillMoveToWindow:))]
        fn view_will_move_to_window(&self, _window: Option<&NSWindow>) {}

        #[unsafe(method(viewDidMoveToWindow))]
        fn view_did_move_to_window(&self) {}

        #[unsafe(method(didAddSubview:))]
        fn did_add_subview(&self, _subview: &NSView) {}

        #[unsafe(method(willRemoveSubview:))]
        fn will_remove_subview(&self, _subview: &NSView) {}

        #[unsafe(method(viewDidHide))]
        fn view_did_hide(&self) {}

        #[unsafe(method(viewDidUnhide))]
        fn view_did_unhide(&self) {}

        // State.

        #[unsafe(method(wantsLayer))]
        fn wants_layer(&self) -> bool {
            has(me(self), WANTS_LAYER)
        }

        #[unsafe(method(setWantsLayer:))]
        fn set_wants_layer(&self, flag: bool) {
            set_flag(me(self), WANTS_LAYER, flag);
        }

        #[unsafe(method(layerContentsRedrawPolicy))]
        fn layer_contents_redraw_policy(&self) -> NSViewLayerContentsRedrawPolicy {
            state(me(self)).redraw_policy.get()
        }

        #[unsafe(method(setLayerContentsRedrawPolicy:))]
        fn set_layer_contents_redraw_policy(&self, policy: NSViewLayerContentsRedrawPolicy) {
            state(me(self)).redraw_policy.set(policy);
        }

        #[unsafe(method(autoresizesSubviews))]
        fn autoresizes_subviews(&self) -> bool {
            !has(me(self), NO_AUTORESIZE)
        }

        #[unsafe(method(setAutoresizesSubviews:))]
        fn set_autoresizes_subviews(&self, flag: bool) {
            set_flag(me(self), NO_AUTORESIZE, !flag);
        }

        #[unsafe(method(setBounds:))]
        fn set_bounds(&self, bounds: NSRect) {
            let view = me(self);
            views::as_view(view).setBoundsOrigin(bounds.origin);
            set_bounds_size(view, bounds.size);
        }

        #[unsafe(method(setBoundsSize:))]
        fn set_bounds_size(&self, size: NSSize) {
            set_bounds_size(me(self), size);
        }

        // Sizes don't scale yet, and have no direction.

        #[unsafe(method(convertSize:toView:))]
        fn convert_size_to_view(&self, size: NSSize, _view: Option<&NSView>) -> NSSize {
            NSSize::new(size.width.abs(), size.height.abs())
        }

        #[unsafe(method(convertSize:fromView:))]
        fn convert_size_from_view(&self, size: NSSize, _view: Option<&NSView>) -> NSSize {
            NSSize::new(size.width.abs(), size.height.abs())
        }

        #[unsafe(method(centerScanRect:))]
        fn center_scan_rect(&self, rect: NSRect) -> NSRect {
            let nearest = NSAlignmentOptions::AlignMinXNearest
                | NSAlignmentOptions::AlignMinYNearest
                | NSAlignmentOptions::AlignWidthNearest
                | NSAlignmentOptions::AlignHeightNearest;
            backing_aligned(me(self), rect, nearest)
        }

        #[unsafe(method(backingAlignedRect:options:))]
        fn backing_aligned_rect(&self, rect: NSRect, options: NSAlignmentOptions) -> NSRect {
            backing_aligned(me(self), rect, options)
        }

        // Scrolling.

        #[unsafe(method(scrollPoint:))]
        fn scroll_point(&self, point: NSPoint) {
            scroll_point(me(self), point);
        }

        #[unsafe(method(scrollRectToVisible:))]
        fn scroll_rect_to_visible(&self, rect: NSRect) -> bool {
            scroll_rect_to_visible(me(self), rect)
        }

        #[unsafe(method(autoscroll:))]
        fn autoscroll(&self, event: &NSEvent) -> bool {
            autoscroll(me(self), event)
        }

        #[unsafe(method(adjustScroll:))]
        fn adjust_scroll(&self, proposed: NSRect) -> NSRect {
            proposed
        }

        #[unsafe(method(prepareForReuse))]
        fn prepare_for_reuse(&self) {}

        #[unsafe(method(prepareContentInRect:))]
        fn prepare_content_in_rect(&self, rect: NSRect) {
            state(me(self)).prepared.set(rect);
        }

        #[unsafe(method(preparedContentRect))]
        fn prepared_content_rect(&self) -> NSRect {
            state(me(self)).prepared.get()
        }

        #[unsafe(method(setPreparedContentRect:))]
        fn set_prepared_content_rect(&self, rect: NSRect) {
            state(me(self)).prepared.set(rect);
        }

        // Layout.

        #[unsafe(method(needsLayout))]
        fn needs_layout(&self) -> bool {
            has(me(self), NEEDS_LAYOUT)
        }

        /// NO takes nothing back, as on macOS.
        #[unsafe(method(setNeedsLayout:))]
        fn set_needs_layout(&self, flag: bool) {
            if flag {
                needs_layout(me(self));
            }
        }

        #[unsafe(method(layout))]
        fn layout(&self) {
            crate::autolayout::layout_subviews(me(self));
        }

        #[unsafe(method(layoutSubtreeIfNeeded))]
        fn layout_subtree_if_needed(&self) {
            lay_out(me(self), true);
        }

        #[unsafe(method(needsUpdateConstraints))]
        fn needs_update_constraints(&self) -> bool {
            has(me(self), NEEDS_UPDATE)
        }

        #[unsafe(method(setNeedsUpdateConstraints:))]
        fn set_needs_update_constraints(&self, flag: bool) {
            if flag {
                needs_update_constraints(me(self));
            }
        }

        #[unsafe(method(updateConstraints))]
        fn update_constraints(&self) {}

        #[unsafe(method(updateConstraintsForSubtreeIfNeeded))]
        fn update_constraints_for_subtree_if_needed(&self) {
            lay_out(me(self), false);
        }

        // Drawing.

        #[unsafe(method(viewWillDraw))]
        fn view_will_draw(&self) {
            for sub in views::subviews(me(self)) {
                if !views::is_hidden(views::imp(&sub)) {
                    sub.viewWillDraw();
                }
            }
        }

        #[unsafe(method(display))]
        fn display(&self) {
            let view = me(self);
            views::invalidate(view, views::bounds(view));
            display_now(view);
        }

        #[unsafe(method(displayIfNeeded))]
        fn display_if_needed(&self) {
            display_now(me(self));
        }

        #[unsafe(method(displayRect:))]
        fn display_rect(&self, rect: NSRect) {
            let view = me(self);
            views::invalidate(view, rect);
            display_now(view);
        }

        #[unsafe(method(displayIfNeededInRect:))]
        fn display_if_needed_in_rect(&self, _rect: NSRect) {
            display_now(me(self));
        }
    }
);

/// The NSView a category method was sent to.
fn me(this: &ViewContract) -> &NSViewImpl {
    // SAFETY: the category adds these methods to NSView only, so the
    // receiver is an NSView, whose layout NSViewImpl describes.
    unsafe { &*(this as *const ViewContract).cast::<NSViewImpl>() }
}

sidestep_runtime::category!("NSView"(SidestepViewContract), |category| {
    // SAFETY: the helper's methods treat their receiver as an NSView.
    unsafe { category.add_methods_of(ViewContract::class()) };
});

define_class!(
    /// NSWindow's layout methods. Receivers are NSWindows.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepWindowLayout"]
    struct WindowLayout;

    impl WindowLayout {
        #[unsafe(method(layoutIfNeeded))]
        fn layout_if_needed(&self) {
            if let Some(content) = window(self).content() {
                lay_out(views::imp(&content), true);
            }
        }

        #[unsafe(method(updateConstraintsIfNeeded))]
        fn update_constraints_if_needed(&self) {
            if let Some(content) = window(self).content() {
                lay_out(views::imp(&content), false);
            }
        }
    }
);

fn window(this: &WindowLayout) -> &NSWindowImpl {
    // SAFETY: the category adds these methods to NSWindow only.
    crate::window::imp(unsafe { &*(this as *const WindowLayout).cast::<NSWindow>() })
}

sidestep_runtime::category!("NSWindow"(SidestepWindowLayout), |category| {
    // SAFETY: the helper's methods treat their receiver as an NSWindow.
    unsafe { category.add_methods_of(WindowLayout::class()) };
});
