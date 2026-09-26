//! Tracking areas and cursor rectangles: which views hear of the pointer
//! entering, leaving and moving over parts of them, and which cursor shows
//! where.
//!
//! A view keeps its tracking areas and cursor rectangles, and a window the
//! list of its views that have any (a flag on the view makes adding one
//! constant time). After each pointer event the window works out which
//! areas hold the pointer and tells their owners what changed:
//! `mouseExited:` first, then `mouseEntered:`, `cursorUpdate:` and
//! `mouseMoved:`. When views move, resize, scroll or come and go, the
//! window has the views in the subtrees that changed `updateTrackingAreas`
//! and `resetCursorRects` before it next looks (the whole window when many
//! subtrees changed), and looks again at the end of the turn even if the
//! pointer stayed still; while the pointer is elsewhere that waits until it
//! comes back. `invalidateCursorRectsForView:` has only that view reset its
//! rectangles. Where areas or rectangles overlap, the deepest view's win.

use std::cell::Cell;
use std::ffi::c_void;

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, MessageReceiver, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSCursor, NSEvent, NSEventType, NSTrackingArea, NSTrackingAreaOptions, NSView};
use objc2_foundation::{NSArray, NSDictionary, NSPoint, NSRect, NSZone};

use crate::views::{self, NSViewImpl};
use crate::window::NSWindowImpl;

type Options = NSTrackingAreaOptions;

pub(crate) struct AreaIvars {
    rect: NSRect,
    options: Options,
    /// Not retained, as owners usually hold their areas.
    owner: Option<Weak<AnyObject>>,
    user_info: Option<Retained<NSDictionary<AnyObject, AnyObject>>>,
    /// For an area made by `addTrackingRect:owner:userData:assumeInside:`:
    /// its tag and the pointer its events carry.
    legacy: Cell<Option<(isize, *mut c_void)>>,
    /// The pointer was inside when the window last looked.
    inside: Cell<bool>,
}

impl AreaIvars {
    fn new(
        rect: NSRect,
        options: Options,
        owner: Option<&AnyObject>,
        user_info: Option<&NSDictionary<AnyObject, AnyObject>>,
    ) -> Self {
        AreaIvars {
            rect,
            options,
            owner: owner.map(Weak::new),
            user_info: user_info.map(|d| d.retain()),
            legacy: Cell::new(None),
            inside: Cell::new(options.contains(Options::AssumeInside)),
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSTrackingArea"]
    #[ivars = AreaIvars]
    pub(crate) struct NSTrackingAreaImpl;

    impl NSTrackingAreaImpl {
        #[unsafe(method_id(initWithRect:options:owner:userInfo:))]
        fn init_with_rect(
            this: Allocated<Self>,
            rect: NSRect,
            options: Options,
            owner: Option<&AnyObject>,
            user_info: Option<&NSDictionary<AnyObject, AnyObject>>,
        ) -> Retained<Self> {
            let this = this.set_ivars(AreaIvars::new(rect, options, owner, user_info));
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(AreaIvars::new(NSRect::ZERO, NSTrackingAreaOptions(0), None, None));
            // SAFETY: as above.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method(rect))]
        fn rect(&self) -> NSRect {
            self.ivars().rect
        }

        #[unsafe(method(options))]
        fn options(&self) -> Options {
            self.ivars().options
        }

        #[unsafe(method_id(owner))]
        fn owner(&self) -> Option<Retained<AnyObject>> {
            self.ivars().owner.as_ref().and_then(Weak::load)
        }

        #[unsafe(method_id(userInfo))]
        fn user_info(&self) -> Option<Retained<NSDictionary<AnyObject, AnyObject>>> {
            self.ivars().user_info.clone()
        }

        // Areas don't change: a copy is the same area.
        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSTrackingAreaImpl {}
);

fn imp(area: &NSTrackingArea) -> &NSTrackingAreaImpl {
    // SAFETY: every NSTrackingArea is an NSTrackingAreaImpl.
    unsafe { &*(area as *const NSTrackingArea).cast::<NSTrackingAreaImpl>() }
}

fn as_area(area: &NSTrackingAreaImpl) -> &NSTrackingArea {
    // SAFETY: NSTrackingAreaImpl is the class NSTrackingArea names.
    unsafe { &*(area as *const NSTrackingAreaImpl).cast::<NSTrackingArea>() }
}

/// What the tracking-area events carry: the area, a number for it and the
/// user data.
pub(crate) fn event_fields(area: &NSTrackingArea) -> (isize, *mut c_void) {
    let ivars = imp(area).ivars();
    match ivars.legacy.get() {
        Some(legacy) => legacy,
        None => {
            let data = ivars.user_info.as_ref().map_or(std::ptr::null_mut(), |d| Retained::as_ptr(d) as *mut c_void);
            (area as *const NSTrackingArea as isize, data)
        }
    }
}

// A view's side.

/// A view's tracking areas and cursor rectangles.
#[derive(Default)]
pub(crate) struct ViewTracking {
    areas: Vec<Retained<NSTrackingArea>>,
    cursor_rects: Vec<(NSRect, Retained<NSCursor>)>,
    /// The view is in its window's list.
    registered: bool,
}

impl ViewTracking {
    fn is_empty(&self) -> bool {
        self.areas.is_empty() && self.cursor_rects.is_empty()
    }
}

pub(crate) fn add_area(view: &NSViewImpl, area: &NSTrackingArea) {
    {
        let mut tracking = views::tracking(view).borrow_mut();
        if tracking.areas.iter().any(|a| std::ptr::eq(&**a, area)) {
            return;
        }
        tracking.areas.push(area.retain());
    }
    changed(view);
}

pub(crate) fn remove_area(view: &NSViewImpl, area: &NSTrackingArea) {
    let removed = {
        let mut tracking = views::tracking(view).borrow_mut();
        let at = tracking.areas.iter().position(|a| std::ptr::eq(&**a, area));
        at.map(|i| tracking.areas.remove(i))
    };
    // Released outside the borrow.
    drop(removed);
}

/// The view's areas as an `NSArray`.
pub(crate) fn areas_array(view: &NSViewImpl) -> Retained<AnyObject> {
    let areas = views::tracking(view).borrow().areas.clone();
    NSArray::from_retained_slice(&areas).into()
}

thread_local!(static NEXT_TAG: Cell<isize> = const { Cell::new(1) });

pub(crate) fn add_tracking_rect(
    view: &NSViewImpl,
    rect: NSRect,
    owner: &AnyObject,
    data: *mut c_void,
    assume_inside: bool,
) -> isize {
    let mut options = Options::MouseEnteredAndExited | Options::ActiveAlways;
    if assume_inside {
        options |= Options::AssumeInside;
    }
    crate::load_shell::<objc2_app_kit::NSTrackingArea>();
    let this = NSTrackingAreaImpl::alloc().set_ivars(AreaIvars::new(rect, options, Some(owner), None));
    // SAFETY: NSObject's designated initializer.
    let area: Retained<NSTrackingAreaImpl> = unsafe { msg_send![super(this), init] };
    let tag = NEXT_TAG.with(|t| t.replace(t.get() + 1));
    area.ivars().legacy.set(Some((tag, data)));
    add_area(view, as_area(&area));
    tag
}

pub(crate) fn remove_tracking_rect(view: &NSViewImpl, tag: isize) {
    let found = views::tracking(view)
        .borrow()
        .areas
        .iter()
        .find(|a| imp(a).ivars().legacy.get().is_some_and(|(t, _)| t == tag))
        .cloned();
    if let Some(area) = found {
        remove_area(view, &area);
    }
}

pub(crate) fn add_cursor_rect(view: &NSViewImpl, rect: NSRect, cursor: &NSCursor) {
    views::tracking(view).borrow_mut().cursor_rects.push((rect, cursor.retain()));
    changed(view);
}

pub(crate) fn remove_cursor_rect(view: &NSViewImpl, rect: NSRect, cursor: &NSCursor) {
    let removed = {
        let mut tracking = views::tracking(view).borrow_mut();
        let at = tracking.cursor_rects.iter().position(|(r, c)| *r == rect && std::ptr::eq(&**c, cursor));
        at.map(|i| tracking.cursor_rects.remove(i))
    };
    drop(removed);
}

pub(crate) fn discard_cursor_rects(view: &NSViewImpl) {
    let removed = std::mem::take(&mut views::tracking(view).borrow_mut().cursor_rects);
    drop(removed);
}

/// The view's areas or rectangles changed: its window has it looked at.
fn changed(view: &NSViewImpl) {
    if let Some(window) = views::window_of(view) {
        let mut tracking = window.tracking().borrow_mut();
        tracking.register(view);
        tracking.recheck = true;
    }
}

/// Subtrees whose views update their areas or rectangles before the
/// window next looks: each root with its subviews, or every view.
#[derive(Default)]
struct Pending {
    all: bool,
    roots: Vec<Retained<NSView>>,
}

/// More subtrees than this and the whole window is walked instead.
const MOST_ROOTS: usize = 16;

impl Pending {
    fn add(&mut self, view: Option<&NSViewImpl>) {
        if self.all {
            return;
        }
        match view {
            Some(view) if self.roots.len() < MOST_ROOTS => {
                if !self.roots.iter().any(|r| std::ptr::eq(views::imp(r), view)) {
                    self.roots.push(views::as_view(view).retain());
                }
            }
            _ => self.everything(),
        }
    }

    fn everything(&mut self) {
        self.all = true;
        self.roots.clear();
    }

    fn is_empty(&self) -> bool {
        !self.all && self.roots.is_empty()
    }

    /// The subtrees to walk, `content` standing for the whole window;
    /// roots inside other roots are left out.
    fn take(&mut self, content: &NSView) -> Vec<Retained<NSView>> {
        let roots = std::mem::take(&mut self.roots);
        if std::mem::take(&mut self.all) {
            return vec![content.retain()];
        }
        let inside = |view: &NSView, other: &NSView| {
            let mut up = views::superview_of(views::imp(view));
            while let Some(v) = up {
                if std::ptr::eq(v, views::imp(other)) {
                    return true;
                }
                up = views::superview_of(v);
            }
            false
        };
        roots.iter().filter(|r| !roots.iter().any(|o| inside(r, o))).cloned().collect()
    }
}

// A window's side.

/// A window's views with tracking areas or cursor rectangles, and what it
/// has to do before it next looks at them.
pub(crate) struct WindowTracking {
    views: Vec<Retained<NSView>>,
    /// Views that moved: they update their areas.
    moved: Pending,
    /// Views whose cursor rectangles are out of date.
    rects: Pending,
    /// Areas or rectangles changed: the window looks again.
    recheck: bool,
    cursor_rects_enabled: bool,
    /// The cursor of the cursor rectangle the pointer is in.
    rect_cursor: Option<Retained<NSCursor>>,
    /// Where the pointer last was, in window coordinates.
    last: NSPoint,
}

impl Default for WindowTracking {
    fn default() -> Self {
        WindowTracking {
            views: Vec::new(),
            moved: Pending { all: true, roots: Vec::new() },
            rects: Pending { all: true, roots: Vec::new() },
            recheck: false,
            cursor_rects_enabled: true,
            rect_cursor: None,
            last: NSPoint::ZERO,
        }
    }
}

impl WindowTracking {
    fn register(&mut self, view: &NSViewImpl) {
        let mut tracking = views::tracking(view).borrow_mut();
        if !tracking.registered {
            tracking.registered = true;
            self.views.push(views::as_view(view).retain());
        }
    }

    /// Something the areas depend on changed: look again.
    pub(crate) fn recheck(&mut self) {
        self.recheck = true;
    }

    pub(crate) fn cursor_rects_enabled(&self) -> bool {
        self.cursor_rects_enabled
    }

    pub(crate) fn enable_cursor_rects(&mut self, enabled: bool) {
        self.cursor_rects_enabled = enabled;
        self.recheck = true;
    }

    /// A view's cursor rectangles, or every view's (`None`), are out of
    /// date.
    pub(crate) fn invalidate_cursor_rects(&mut self, view: Option<&NSViewImpl>) {
        self.rects.add(view);
        self.recheck = true;
    }
}

/// A view came into the window: it and its subviews update their areas.
/// (Called for each view of a subtree that joins, the root first.)
pub(crate) fn view_joined(window: &NSWindowImpl, view: &NSViewImpl) {
    let mut tracking = window.tracking().borrow_mut();
    if !views::tracking(view).borrow().is_empty() {
        tracking.register(view);
    }
    let covered = views::superview_of(view)
        .is_some_and(|s| views::window_of(s).is_some_and(|w| std::ptr::eq(w, window)))
        && tracking.moved.roots.iter().any(|r| is_within(view, views::imp(r)));
    if !covered {
        tracking.moved.add(Some(view));
        tracking.rects.add(Some(view));
    }
}

/// `view` is `root` or inside it.
fn is_within(view: &NSViewImpl, root: &NSViewImpl) -> bool {
    let mut at = Some(view);
    while let Some(v) = at {
        if std::ptr::eq(v, root) {
            return true;
        }
        at = views::superview_of(v);
    }
    false
}

/// A view left the window: it's forgotten, and its areas start outside.
pub(crate) fn view_left(window: &NSWindowImpl, view: &NSViewImpl) {
    let registered = std::mem::take(&mut views::tracking(view).borrow_mut().registered);
    let gone = {
        let mut tracking = window.tracking().borrow_mut();
        tracking.recheck = true;
        let at = if registered { tracking.views.iter().position(|v| std::ptr::eq(views::imp(v), view)) } else { None };
        at.map(|i| tracking.views.remove(i))
    };
    for area in &views::tracking(view).borrow().areas {
        area_ivars(area).inside.set(false);
    }
    drop(gone);
}

/// A view moved, resized, scrolled, or showed or hid in the window.
pub(crate) fn views_moved(window: &NSWindowImpl, view: &NSViewImpl) {
    let mut tracking = window.tracking().borrow_mut();
    tracking.moved.add(Some(view));
    tracking.rects.add(Some(view));
}

fn area_ivars(area: &NSTrackingArea) -> &AreaIvars {
    imp(area).ivars()
}

/// At the end of a turn: look again if anything moved, with the pointer
/// where it is. Without the pointer over the window nothing needs looking
/// at, and views update their areas once it comes back.
pub(crate) fn refresh(window: &NSWindowImpl) {
    if window.pointer().is_none() {
        return;
    }
    let pending = {
        let t = window.tracking().borrow();
        // Rectangles out of date wait while they're disabled.
        !t.moved.is_empty() || t.recheck || (!t.rects.is_empty() && t.cursor_rects_enabled)
    };
    if pending {
        update(window, None);
    }
}

/// Have the views that moved update their areas, and those whose cursor
/// rectangles are out of date reset them. Views left with neither areas
/// nor rectangles leave the window's list.
pub(crate) fn rebuild(window: &NSWindowImpl) {
    let Some(content) = window.content() else { return };
    let (mut moved, mut reset) = {
        let mut t = window.tracking().borrow_mut();
        let moved = if t.moved.is_empty() { Vec::new() } else { t.moved.take(&content) };
        let reset = if t.cursor_rects_enabled && !t.rects.is_empty() { t.rects.take(&content) } else { Vec::new() };
        (moved, reset)
    };
    // Subtrees that left the window since then are no longer its to walk.
    let here = |r: &Retained<NSView>| views::window_of(views::imp(r)).is_some_and(|w| std::ptr::eq(w, window));
    moved.retain(here);
    reset.retain(here);
    if moved.is_empty() && reset.is_empty() {
        return;
    }
    for root in &moved {
        walk(root, &mut |view| view.updateTrackingAreas());
    }
    for root in &reset {
        walk(root, &mut |view| {
            discard_cursor_rects(views::imp(view));
            view.resetCursorRects();
        });
    }
    let dropped: Vec<_> = {
        let mut t = window.tracking().borrow_mut();
        let (kept, dropped) = std::mem::take(&mut t.views).into_iter().partition(|v| {
            let mut tracking = views::tracking(views::imp(v)).borrow_mut();
            tracking.registered = !tracking.is_empty();
            tracking.registered
        });
        t.views = kept;
        dropped
    };
    drop(dropped);
}

/// Call `f` on `view` and every view inside it, depth first.
fn walk(view: &NSView, f: &mut impl FnMut(&NSView)) {
    f(view);
    // By index, not over a copy of the list: `f` may add or remove views.
    let mut i = 0;
    while let Some(sub) = views::subview_at(views::imp(view), i) {
        walk(&sub, f);
        i += 1;
    }
}

/// Look at the areas and cursor rectangles against where the pointer is,
/// after `moved` (a mouse-moved event) or anything else.
pub(crate) fn update(window: &NSWindowImpl, moved: Option<&NSEvent>) {
    rebuild(window);
    let pointer = window.pointer();
    let dragging = window.dragging();
    let key = window.is_key();
    let app_active = crate::app::is_active();
    let rects_enabled = window.tracking().borrow().cursor_rects_enabled;

    let mut exits = Vec::new();
    let mut enters = Vec::new();
    let mut moves = Vec::new();
    let mut cursor_changed = false;
    // The deepest cursor-update area and cursor rectangle the pointer is in.
    let mut cursor_area: Option<(usize, Retained<NSTrackingArea>)> = None;
    let mut rect_cursor: Option<(usize, Retained<NSCursor>)> = None;

    let mut i = 0;
    loop {
        let Some(view) = window.tracking().borrow().views.get(i).cloned() else { break };
        i += 1;
        let v = views::imp(&view);
        if views::tracking(v).borrow().is_empty() {
            continue;
        }
        let shown = !views::is_hidden_or_has_hidden_ancestor(v);
        let local = pointer.filter(|_| shown).map(|p| view.convertPoint_fromView(p, None));
        let depth = views::depth(v);
        let first = window.is_first_responder(v);
        // Asked before the areas are borrowed, as views may override it.
        let needs_visible = {
            let t = views::tracking(v).borrow();
            t.areas.iter().any(|a| area_ivars(a).options.contains(Options::InVisibleRect))
                || (local.is_some() && !t.cursor_rects.is_empty())
        };
        let visible = if needs_visible { view.visibleRect() } else { NSRect::ZERO };
        let tracking = views::tracking(v).borrow();
        for area in &tracking.areas {
            let a = area_ivars(area);
            let active = if a.options.contains(Options::ActiveAlways) {
                true
            } else if a.options.contains(Options::ActiveInActiveApp) {
                app_active
            } else if a.options.contains(Options::ActiveInKeyWindow) {
                key
            } else {
                a.options.contains(Options::ActiveWhenFirstResponder) && key && first
            };
            let rect = if a.options.contains(Options::InVisibleRect) { visible } else { a.rect };
            let inside = active && local.is_some_and(|p| contains(rect, p));
            if inside != a.inside.get() {
                if dragging && !a.options.contains(Options::EnabledDuringMouseDrag) {
                    continue;
                }
                a.inside.set(inside);
                if a.options.contains(Options::MouseEnteredAndExited) {
                    if inside { enters.push(area.clone()) } else { exits.push(area.clone()) }
                }
                cursor_changed |= a.options.contains(Options::CursorUpdate);
            }
            if inside
                && a.options.contains(Options::CursorUpdate)
                && cursor_area.as_ref().is_none_or(|(d, _)| depth >= *d)
            {
                cursor_area = Some((depth, area.clone()));
            }
            if inside && moved.is_some() && a.options.contains(Options::MouseMoved) {
                moves.push(area.clone());
            }
        }
        if rects_enabled && let Some(p) = local {
            for (rect, cursor) in &tracking.cursor_rects {
                let clipped = intersect(*rect, visible);
                if contains(clipped, p) && rect_cursor.as_ref().is_none_or(|(d, _)| depth >= *d) {
                    rect_cursor = Some((depth, cursor.clone()));
                }
            }
        }
    }

    let location = {
        let mut t = window.tracking().borrow_mut();
        if let Some(p) = pointer {
            t.last = p;
        }
        t.recheck = false;
        t.last
    };
    let make = |kind, area: &NSTrackingArea| crate::event::tracking_event(kind, location, window.as_window(), area);
    for area in &exits {
        tell(area, sel!(mouseExited:), &make(NSEventType::MouseExited, area));
    }
    for area in &enters {
        tell(area, sel!(mouseEntered:), &make(NSEventType::MouseEntered, area));
    }
    if cursor_changed {
        match (cursor_area, &rect_cursor) {
            (Some((_, area)), _) => tell(&area, sel!(cursorUpdate:), &make(NSEventType::CursorUpdate, &area)),
            (None, Some((_, cursor))) if rects_enabled => cursor.set(),
            (None, _) => NSCursor::arrowCursor().set(),
        }
    }
    if let Some(event) = moved {
        for area in &moves {
            tell(area, sel!(mouseMoved:), event);
        }
    }
    if rects_enabled {
        let wanted = rect_cursor.map(|(_, c)| c);
        let previous = {
            let mut t = window.tracking().borrow_mut();
            let same = match (&wanted, &t.rect_cursor) {
                (Some(a), Some(b)) => std::ptr::eq(&**a, &**b),
                (None, None) => true,
                _ => false,
            };
            (!same).then(|| std::mem::replace(&mut t.rect_cursor, wanted.clone()))
        };
        match (previous, wanted) {
            (Some(_), Some(cursor)) => cursor.set(),
            (Some(Some(_)), None) => NSCursor::arrowCursor().set(),
            _ => {}
        }
    }
}

/// Send a tracking message to an area's owner, if it has the method.
fn tell(area: &NSTrackingArea, selector: Sel, event: &NSEvent) {
    let Some(owner) = imp(area).ivars().owner.as_ref().and_then(Weak::load) else { return };
    // SAFETY: respondsToSelector: takes a selector and returns BOOL.
    let responds: bool = unsafe { msg_send![&*owner, respondsToSelector: selector] };
    if responds {
        // SAFETY: the tracking messages take an event and return nothing.
        let _: () = unsafe { (&*owner).send_message(selector, (event,)) };
    }
}

fn contains(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x && p.x < r.origin.x + r.size.width && p.y >= r.origin.y && p.y < r.origin.y + r.size.height
}

fn intersect(a: NSRect, b: NSRect) -> NSRect {
    let x0 = a.origin.x.max(b.origin.x);
    let y0 = a.origin.y.max(b.origin.y);
    let x1 = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    NSRect::new(NSPoint::new(x0, y0), objc2_foundation::NSSize::new((x1 - x0).max(0.0), (y1 - y0).max(0.0)))
}
