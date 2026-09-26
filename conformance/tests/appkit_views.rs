//! The NSView contract, checked on macOS and on Linux alike: the subview
//! list and its methods, the messages views get as they move between
//! superviews and windows and as they hide and show, per-view state,
//! scaled bounds, alignment to the backing store, the scrolling helpers,
//! the layout pass (`updateConstraints`, `layout`, `viewWillDraw`), views
//! that ask for layout while they lay out, and callbacks that move views.
//!
//! Windows are never shown. AppKit belongs to the main thread, so this
//! file has its own `main`.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSResponder, NSScrollView, NSUserInterfaceItemIdentification,
    NSView, NSViewLayerContentsRedrawPolicy, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSAlignmentOptions, NSArray, NSComparisonResult, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

/// What recording views heard since the last call.
fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// The log's entries that mention one of `words`.
fn only(log: Vec<String>, words: &[&str]) -> Vec<String> {
    log.into_iter().filter(|e| words.iter().any(|w| e.contains(w))).collect()
}

const NONE: [&str; 0] = [];

struct Ivars {
    name: String,
    flipped: bool,
    /// Pass viewWillDraw on to the subviews (NSView's own does).
    will_draw_super: bool,
}

/// A view's identifier, which recording views use as their name.
fn name_of(view: Option<&NSView>) -> String {
    match view {
        None => "nil".into(),
        Some(v) => v.identifier().map(|s| s.to_string()).unwrap_or_else(|| "?".into()),
    }
}

define_class!(
    /// Logs the messages views get.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceViewsRecorder"]
    #[ivars = Ivars]
    struct Recorder;

    impl Recorder {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().flipped
        }

        #[unsafe(method(viewWillMoveToSuperview:))]
        fn will_move_to_superview(&self, superview: Option<&NSView>) {
            log(format!("{} willMoveToSuperview {}", self.ivars().name, name_of(superview)));
        }

        #[unsafe(method(viewDidMoveToSuperview))]
        fn did_move_to_superview(&self) {
            log(format!("{} didMoveToSuperview", self.ivars().name));
        }

        #[unsafe(method(viewWillMoveToWindow:))]
        fn will_move_to_window(&self, window: Option<&NSWindow>) {
            let to = if window.is_some() { "window" } else { "nil" };
            log(format!("{} willMoveToWindow {to}", self.ivars().name));
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn did_move_to_window(&self) {
            log(format!("{} didMoveToWindow", self.ivars().name));
        }

        #[unsafe(method(didAddSubview:))]
        fn did_add_subview(&self, view: &NSView) {
            log(format!("{} didAddSubview {}", self.ivars().name, name_of(Some(view))));
        }

        #[unsafe(method(willRemoveSubview:))]
        fn will_remove_subview(&self, view: &NSView) {
            log(format!("{} willRemoveSubview {}", self.ivars().name, name_of(Some(view))));
        }

        #[unsafe(method(viewDidHide))]
        fn did_hide(&self) {
            log(format!("{} viewDidHide", self.ivars().name));
        }

        #[unsafe(method(viewDidUnhide))]
        fn did_unhide(&self) {
            log(format!("{} viewDidUnhide", self.ivars().name));
        }

        #[unsafe(method(layout))]
        fn layout(&self) {
            log(format!("{} layout", self.ivars().name));
            // SAFETY: the superclass's method, with the arguments it was given.
            unsafe { msg_send![super(self), layout] }
        }

        #[unsafe(method(updateConstraints))]
        fn update_constraints(&self) {
            log(format!("{} updateConstraints", self.ivars().name));
            // SAFETY: the superclass's method, with the arguments it was given.
            unsafe { msg_send![super(self), updateConstraints] }
        }

        #[unsafe(method(viewWillDraw))]
        fn view_will_draw(&self) {
            log(format!("{} viewWillDraw", self.ivars().name));
            if self.ivars().will_draw_super {
                // SAFETY: the superclass's method, with the arguments it was given.
                unsafe { msg_send![super(self), viewWillDraw] }
            }
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews(&self, old: NSSize) {
            log(format!("{} resizeSubviewsWithOldSize {}x{}", self.ivars().name, old.width, old.height));
            // SAFETY: the superclass's method, with the arguments it was given.
            unsafe { msg_send![super(self), resizeSubviewsWithOldSize: old] }
        }

        #[unsafe(method(resizeWithOldSuperviewSize:))]
        fn resize_with_old_superview_size(&self, old: NSSize) {
            log(format!("{} resizeWithOldSuperviewSize {}x{}", self.ivars().name, old.width, old.height));
            // SAFETY: the superclass's method, with the arguments it was given.
            unsafe { msg_send![super(self), resizeWithOldSuperviewSize: old] }
        }
    }

    unsafe impl NSObjectProtocol for Recorder {}
);

/// What a mover does in `layout`.
#[derive(Clone, Copy, PartialEq)]
enum InLayout {
    Nothing,
    /// Asks for its own layout again, every time.
    FlagsItself,
    /// Asks for its superview's layout, once.
    FlagsSuperview,
    /// Adds its `child`, once.
    AddsChild,
}

struct MoverIvars {
    name: String,
    in_layout: Cell<InLayout>,
    child: RefCell<Option<Retained<NSView>>>,
    /// Taken out of its superview when this view joins a window.
    victim: RefCell<Option<Retained<NSView>>>,
}

define_class!(
    /// A view that does things while it's laid out or moved.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceViewsMover"]
    #[ivars = MoverIvars]
    struct Mover;

    impl Mover {
        #[unsafe(method(layout))]
        fn layout(&self) {
            log(format!("{} layout", self.ivars().name));
            // SAFETY: the superclass's method, with the arguments it was given.
            let _: () = unsafe { msg_send![super(self), layout] };
            let me = self.as_view();
            match self.ivars().in_layout.get() {
                InLayout::Nothing => {}
                InLayout::FlagsItself => me.setNeedsLayout(true),
                InLayout::FlagsSuperview => {
                    self.ivars().in_layout.set(InLayout::Nothing);
                    // SAFETY: the superview is alive while the view is in it.
                    if let Some(sup) = unsafe { me.superview() } {
                        sup.setNeedsLayout(true);
                    }
                }
                InLayout::AddsChild => {
                    self.ivars().in_layout.set(InLayout::Nothing);
                    if let Some(child) = self.ivars().child.take() {
                        me.addSubview(&child);
                    }
                }
            }
        }

        #[unsafe(method(updateConstraints))]
        fn update_constraints(&self) {
            log(format!("{} updateConstraints", self.ivars().name));
            // SAFETY: the superclass's method, with the arguments it was given.
            unsafe { msg_send![super(self), updateConstraints] }
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn did_move_to_window(&self) {
            if self.as_view().window().is_some()
                && let Some(victim) = self.ivars().victim.take()
            {
                victim.removeFromSuperview();
            }
        }
    }

    unsafe impl NSObjectProtocol for Mover {}
);

impl Mover {
    fn as_view(&self) -> &NSView {
        // SAFETY: a Mover is an NSView.
        unsafe { &*(self as *const Mover).cast::<NSView>() }
    }
}

fn mover(mtm: MainThreadMarker, name: &str, in_layout: InLayout) -> Retained<NSView> {
    let this = Mover::alloc(mtm).set_ivars(MoverIvars {
        name: name.into(),
        in_layout: Cell::new(in_layout),
        child: RefCell::new(None),
        victim: RefCell::new(None),
    });
    // SAFETY: the superclass's designated initializer.
    let view: Retained<Mover> = unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, 10.0, 10.0)] };
    Retained::into_super(view)
}

fn mover_ivars(view: &NSView) -> &MoverIvars {
    // SAFETY: only called on views `mover` made.
    unsafe { &*(view as *const NSView).cast::<Mover>() }.ivars()
}

fn make(mtm: MainThreadMarker, name: &str, frame: NSRect, flipped: bool, will_draw_super: bool) -> Retained<NSView> {
    let this = Recorder::alloc(mtm).set_ivars(Ivars { name: name.into(), flipped, will_draw_super });
    // SAFETY: the superclass's designated initializer.
    let view: Retained<Recorder> = unsafe { msg_send![super(this), initWithFrame: frame] };
    let view = Retained::into_super(view);
    view.setIdentifier(Some(&NSString::from_str(name)));
    view
}

/// A recording view named `name`.
fn rec(mtm: MainThreadMarker, name: &str) -> Retained<NSView> {
    make(mtm, name, rect(0.0, 0.0, 10.0, 10.0), false, true)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn pt(x: f64, y: f64) -> NSPoint {
    NSPoint::new(x, y)
}

fn names(view: &NSView) -> Vec<String> {
    view.subviews().iter().map(|s| name_of(Some(&s))).collect()
}

fn window(mtm: MainThreadMarker, defer: bool) -> Retained<NSWindow> {
    // SAFETY: a plain window, never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            defer,
        )
    };
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

fn same(a: &NSView, b: &NSView) -> bool {
    std::ptr::eq(a, b)
}

fn subviews_are_snapshots(mtm: MainThreadMarker) {
    let p = rec(mtm, "p");
    assert_eq!(p.subviews().count(), 0);
    p.addSubview(&rec(mtm, "a"));
    let before = p.subviews();
    p.addSubview(&rec(mtm, "b"));
    assert_eq!(before.count(), 1);
    assert_eq!(names(&p), ["a", "b"]);
    take();
}

fn adding_and_removing(mtm: MainThreadMarker) {
    let (p, q) = (rec(mtm, "p"), rec(mtm, "q"));
    let (a, b) = (rec(mtm, "a"), rec(mtm, "b"));
    p.addSubview(&a);
    assert_eq!(take(), ["a willMoveToSuperview p", "p didAddSubview a", "a didMoveToSuperview"]);
    // SAFETY: the superview is alive while the view is in it.
    assert!(same(&unsafe { a.superview() }.unwrap(), &p));
    p.addSubview(&b);
    take();

    // Adding the top subview again does nothing; adding a lower one moves
    // it to the top, out and back in.
    p.addSubview(&b);
    assert_eq!(take(), NONE);
    p.addSubview(&a);
    assert_eq!(
        take(),
        ["a willMoveToSuperview p", "p willRemoveSubview a", "p didAddSubview a", "a didMoveToSuperview"]
    );
    assert_eq!(names(&p), ["b", "a"]);

    // From one superview to another: one will and one did.
    q.addSubview(&a);
    assert_eq!(
        take(),
        ["a willMoveToSuperview q", "p willRemoveSubview a", "q didAddSubview a", "a didMoveToSuperview"]
    );
    assert_eq!(names(&p), ["b"]);

    a.removeFromSuperview();
    assert_eq!(take(), ["a willMoveToSuperview nil", "q willRemoveSubview a", "a didMoveToSuperview"]);
    // SAFETY: the superview is alive while the view is in it.
    assert!(unsafe { a.superview() }.is_none());
    a.removeFromSuperview();
    assert_eq!(take(), NONE);

    b.removeFromSuperviewWithoutNeedingDisplay();
    assert_eq!(take(), ["b willMoveToSuperview nil", "p willRemoveSubview b", "b didMoveToSuperview"]);
    assert_eq!(p.subviews().count(), 0);
}

fn positioned_subviews(mtm: MainThreadMarker) {
    let p = rec(mtm, "p");
    let (a, b, c) = (rec(mtm, "a"), rec(mtm, "b"), rec(mtm, "c"));
    p.addSubview(&a);
    p.addSubview(&b);
    take();
    p.addSubview_positioned_relativeTo(&c, NSWindowOrderingMode::Below, None);
    assert_eq!(take(), ["c willMoveToSuperview p", "p didAddSubview c", "c didMoveToSuperview"]);
    assert_eq!(names(&p), ["c", "a", "b"]);
    // A subview changes places silently.
    p.addSubview_positioned_relativeTo(&c, NSWindowOrderingMode::Above, Some(&a));
    assert_eq!(names(&p), ["a", "c", "b"]);
    p.addSubview_positioned_relativeTo(&c, NSWindowOrderingMode::Below, Some(&b));
    assert_eq!(names(&p), ["a", "c", "b"]);
    p.addSubview_positioned_relativeTo(&a, NSWindowOrderingMode::Above, None);
    assert_eq!(names(&p), ["c", "b", "a"]);
    p.addSubview_positioned_relativeTo(&a, NSWindowOrderingMode::Below, None);
    assert_eq!(names(&p), ["a", "c", "b"]);
    p.addSubview_positioned_relativeTo(&b, NSWindowOrderingMode::Above, Some(&a));
    assert_eq!(names(&p), ["a", "b", "c"]);
    assert_eq!(take(), NONE);
    let d = rec(mtm, "d");
    p.addSubview_positioned_relativeTo(&d, NSWindowOrderingMode::Above, Some(&a));
    assert_eq!(names(&p), ["a", "d", "b", "c"]);
    take();
}

fn replacing_subviews(mtm: MainThreadMarker) {
    let (p, other) = (rec(mtm, "p"), rec(mtm, "other"));
    let (a, b, c, d) = (rec(mtm, "a"), rec(mtm, "b"), rec(mtm, "c"), rec(mtm, "d"));
    p.addSubview(&a);
    p.addSubview(&b);
    other.addSubview(&d);
    take();
    // The new view takes the old one's place, then the old one leaves.
    p.replaceSubview_with(&a, &c);
    assert_eq!(
        take(),
        [
            "c willMoveToSuperview p",
            "p didAddSubview c",
            "c didMoveToSuperview",
            "a willMoveToSuperview nil",
            "p willRemoveSubview a",
            "a didMoveToSuperview",
        ]
    );
    assert_eq!(names(&p), ["c", "b"]);
    // SAFETY: the superview is alive while the view is in it.
    assert!(unsafe { a.superview() }.is_none());
    // A view from another superview moves.
    p.replaceSubview_with(&b, &d);
    assert_eq!(
        take(),
        [
            "d willMoveToSuperview p",
            "other willRemoveSubview d",
            "p didAddSubview d",
            "d didMoveToSuperview",
            "b willMoveToSuperview nil",
            "p willRemoveSubview b",
            "b didMoveToSuperview",
        ]
    );
    assert_eq!(names(&p), ["c", "d"]);
    assert_eq!(other.subviews().count(), 0);
    // A sibling only changes places.
    p.replaceSubview_with(&c, &d);
    assert_eq!(take(), ["c willMoveToSuperview nil", "p willRemoveSubview c", "c didMoveToSuperview"]);
    assert_eq!(names(&p), ["d"]);
}

fn setting_subviews(mtm: MainThreadMarker) {
    let p = rec(mtm, "p");
    let (a, b, c, d, e) = (rec(mtm, "a"), rec(mtm, "b"), rec(mtm, "c"), rec(mtm, "d"), rec(mtm, "e"));
    for v in [&d, &c, &b] {
        p.addSubview(v);
    }
    take();
    // Those not kept leave in their old order, then new ones join in theirs.
    p.setSubviews(&NSArray::from_retained_slice(&[a.clone(), c.clone(), e.clone()]));
    assert_eq!(
        take(),
        [
            "d willMoveToSuperview nil",
            "p willRemoveSubview d",
            "d didMoveToSuperview",
            "b willMoveToSuperview nil",
            "p willRemoveSubview b",
            "b didMoveToSuperview",
            "a willMoveToSuperview p",
            "p didAddSubview a",
            "a didMoveToSuperview",
            "e willMoveToSuperview p",
            "p didAddSubview e",
            "e didMoveToSuperview",
        ]
    );
    assert_eq!(names(&p), ["a", "c", "e"]);
    // Reordering sends nothing.
    p.setSubviews(&NSArray::from_retained_slice(&[e.clone(), a.clone()]));
    assert_eq!(take(), ["c willMoveToSuperview nil", "p willRemoveSubview c", "c didMoveToSuperview"]);
    assert_eq!(names(&p), ["e", "a"]);
}

extern "C-unwind" fn by_name_descending(
    a: std::ptr::NonNull<NSView>,
    b: std::ptr::NonNull<NSView>,
    context: *mut std::ffi::c_void,
) -> NSComparisonResult {
    // The context counts the comparisons.
    // SAFETY: the context is the test's counter, a usize.
    unsafe { *context.cast::<usize>() += 1 };
    // SAFETY: AppKit passes the views being sorted, which are alive.
    let (a, b) = unsafe { (name_of(Some(a.as_ref())), name_of(Some(b.as_ref()))) };
    match b.cmp(&a) {
        std::cmp::Ordering::Less => NSComparisonResult::Ascending,
        std::cmp::Ordering::Equal => NSComparisonResult::Same,
        std::cmp::Ordering::Greater => NSComparisonResult::Descending,
    }
}

fn sorting_subviews(mtm: MainThreadMarker) {
    let p = rec(mtm, "p");
    for name in ["b", "d", "a", "c"] {
        p.addSubview(&rec(mtm, name));
    }
    take();
    let mut compared = 0usize;
    // SAFETY: the function only reads views and counts in `compared`, which outlives the call.
    unsafe { p.sortSubviewsUsingFunction_context(by_name_descending, (&raw mut compared).cast()) };
    assert_eq!(names(&p), ["d", "c", "b", "a"]);
    assert!(compared > 0);
    assert_eq!(take(), NONE);

    // A function that isn't an order gives some order of the same views.
    let many = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    let kids: Vec<Retained<NSView>> =
        (0..60).map(|_| NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO)).collect();
    for k in &kids {
        many.addSubview(k);
    }
    let mut state = 12345u64;
    for _ in 0..20 {
        // SAFETY: the function only reads its context, the test's `state`, which outlives the call.
        unsafe { many.sortSubviewsUsingFunction_context(at_random, (&raw mut state).cast()) };
    }
    let after = many.subviews();
    assert_eq!(after.count(), kids.len());
    assert!(kids.iter().all(|k| after.iter().any(|v| same(&v, k))));
}

extern "C-unwind" fn at_random(
    _a: std::ptr::NonNull<NSView>,
    _b: std::ptr::NonNull<NSView>,
    context: *mut std::ffi::c_void,
) -> NSComparisonResult {
    // SAFETY: the context is the test's state, a u64.
    let state = unsafe { &mut *context.cast::<u64>() };
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    match (*state >> 33) % 3 {
        0 => NSComparisonResult::Ascending,
        1 => NSComparisonResult::Same,
        _ => NSComparisonResult::Descending,
    }
}

fn ancestry(mtm: MainThreadMarker) {
    let (p, a, e, gc, lone) = (rec(mtm, "p"), rec(mtm, "a"), rec(mtm, "e"), rec(mtm, "gc"), rec(mtm, "lone"));
    p.addSubview(&a);
    p.addSubview(&e);
    a.addSubview(&gc);
    take();
    assert!(p.isDescendantOf(&p), "a view counts as its own descendant");
    assert!(gc.isDescendantOf(&a) && gc.isDescendantOf(&p));
    assert!(!gc.isDescendantOf(&e) && !p.isDescendantOf(&gc));
    let shared = |x: &NSView, y: &NSView| name_of(x.ancestorSharedWithView(y).as_deref());
    assert_eq!(shared(&gc, &e), "p");
    assert_eq!(shared(&gc, &gc), "gc");
    assert_eq!(shared(&p, &gc), "p");
    assert_eq!(shared(&gc, &lone), "nil");

    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    let doc = rec(mtm, "doc");
    scroll.setDocumentView(Some(&doc));
    doc.addSubview(&lone);
    let encloses = |v: &NSView| v.enclosingScrollView().is_some_and(|s| same(&s, &scroll));
    assert!(encloses(&doc) && encloses(&lone) && encloses(&scroll.contentView()));
    assert!(scroll.enclosingScrollView().is_none(), "a scroll view doesn't enclose itself");
    assert!(p.enclosingScrollView().is_none());
    take();
}

fn hiding_tells_shown_subviews(mtm: MainThreadMarker) {
    let (h, h1, h2) = (rec(mtm, "h"), rec(mtm, "h1"), rec(mtm, "h2"));
    h.addSubview(&h1);
    h1.addSubview(&h2);
    take();
    h2.setHidden(true);
    assert_eq!(take(), ["h2 viewDidHide"]);
    // Down to hidden subviews, not past them.
    h.setHidden(true);
    assert_eq!(take(), ["h viewDidHide", "h1 viewDidHide"]);
    assert!(h1.isHiddenOrHasHiddenAncestor() && !h1.isHidden());
    h.setHidden(true);
    assert_eq!(take(), NONE);
    // Under a hidden ancestor, a view's own flag tells it nothing.
    h1.setHidden(true);
    h1.setHidden(false);
    assert_eq!(take(), NONE);
    h.setHidden(false);
    assert_eq!(take(), ["h viewDidUnhide", "h1 viewDidUnhide"]);

    // Joining and leaving a hidden superview.
    let h3 = rec(mtm, "h3");
    h.setHidden(true);
    take();
    h.addSubview(&h3);
    assert_eq!(take(), ["h3 willMoveToSuperview h", "h didAddSubview h3", "h3 viewDidHide", "h3 didMoveToSuperview"]);
    h3.removeFromSuperview();
    assert_eq!(
        take(),
        ["h3 willMoveToSuperview nil", "h3 viewDidUnhide", "h willRemoveSubview h3", "h3 didMoveToSuperview"]
    );
    // From one hidden superview to another: still hidden, nothing said.
    let g = rec(mtm, "g");
    g.setHidden(true);
    g.addSubview(&h3);
    take();
    h.addSubview(&h3);
    assert_eq!(
        take(),
        ["h3 willMoveToSuperview h", "g willRemoveSubview h3", "h didAddSubview h3", "h3 didMoveToSuperview"]
    );
}

fn view_state(mtm: MainThreadMarker) {
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert!(v.identifier().is_none());
    v.setIdentifier(Some(&NSString::from_str("name")));
    assert_eq!(v.identifier().unwrap().to_string(), "name");
    v.setIdentifier(None);
    assert!(v.identifier().is_none());

    assert_eq!(v.alphaValue(), 1.0);
    v.setAlphaValue(0.25);
    assert_eq!(v.alphaValue(), 0.25);
    // Not clamped.
    v.setAlphaValue(2.0);
    assert_eq!(v.alphaValue(), 2.0);

    assert!(!v.wantsLayer());
    v.setWantsLayer(true);
    assert!(v.wantsLayer());
    v.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::Never);
    assert_eq!(v.layerContentsRedrawPolicy(), NSViewLayerContentsRedrawPolicy::Never);
    v.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::BeforeViewResize);
    assert_eq!(v.layerContentsRedrawPolicy(), NSViewLayerContentsRedrawPolicy::BeforeViewResize);
    assert!(v.autoresizesSubviews());
    assert_eq!(v.preparedContentRect(), NSRect::ZERO);
    v.prepareContentInRect(rect(0.0, 0.0, 50.0, 50.0));
    assert_eq!(v.preparedContentRect(), rect(0.0, 0.0, 50.0, 50.0));
    assert_eq!(v.adjustScroll(rect(1.5, 2.5, 3.0, 4.0)), rect(1.5, 2.5, 3.0, 4.0));
}

fn autoresizing_subviews_is_optional(mtm: MainThreadMarker) {
    let q = make(mtm, "q", rect(0.0, 0.0, 100.0, 100.0), false, true);
    let q1 = rec(mtm, "q1");
    q1.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    q.addSubview(&q1);
    take();
    q.setAutoresizesSubviews(false);
    assert!(!q.autoresizesSubviews());
    q.setFrameSize(NSSize::new(200.0, 100.0));
    assert_eq!(take(), NONE);
    assert_eq!(q1.frame(), rect(0.0, 0.0, 10.0, 10.0));
    q.setAutoresizesSubviews(true);
    q.setFrameSize(NSSize::new(300.0, 100.0));
    assert_eq!(take(), ["q resizeSubviewsWithOldSize 200x100", "q1 resizeWithOldSuperviewSize 200x100"]);
    assert_eq!(q1.frame(), rect(0.0, 0.0, 110.0, 10.0));
    // Only a change of size resizes subviews.
    q.setFrameSize(NSSize::new(300.0, 100.0));
    q.setFrameOrigin(pt(3.0, 3.0));
    assert_eq!(take(), NONE);
}

fn scaled_bounds(mtm: MainThreadMarker) {
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 50.0));
    v.setBoundsSize(NSSize::new(200.0, 100.0));
    assert_eq!(v.bounds(), rect(0.0, 0.0, 200.0, 100.0));
    assert_eq!(v.frame(), rect(0.0, 0.0, 100.0, 50.0));
    // Resizing keeps the scale.
    v.setFrameSize(NSSize::new(50.0, 50.0));
    assert_eq!(v.bounds(), rect(0.0, 0.0, 100.0, 100.0));
    v.setBounds(rect(10.0, 20.0, 25.0, 25.0));
    assert_eq!(v.bounds(), rect(10.0, 20.0, 25.0, 25.0));
    assert_eq!(v.frame(), rect(0.0, 0.0, 50.0, 50.0));
    v.setBoundsOrigin(pt(1.0, 2.0));
    assert_eq!(v.bounds(), rect(1.0, 2.0, 25.0, 25.0));
    // Unscaled bounds follow the frame.
    let w = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 50.0));
    w.setBounds(rect(5.0, 5.0, 100.0, 50.0));
    w.setFrameSize(NSSize::new(120.0, 60.0));
    assert_eq!(w.bounds(), rect(5.0, 5.0, 120.0, 60.0));
}

fn converting_sizes(mtm: MainThreadMarker) {
    let root = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    let flipped = make(mtm, "fl", rect(10.0, 20.0, 100.0, 50.0), true, true);
    let plain = make(mtm, "pl", rect(10.0, 20.0, 100.0, 50.0), false, true);
    root.addSubview(&flipped);
    root.addSubview(&plain);
    take();
    let s = NSSize::new(3.0, 4.0);
    // Sizes have no direction: flipping doesn't make them negative, and
    // negative ones come back positive.
    for (from, to) in [(&flipped, Some(&*root)), (&root, Some(&*flipped)), (&flipped, Some(&*plain)), (&plain, None)] {
        assert_eq!(from.convertSize_toView(s, to), s);
        assert_eq!(from.convertSize_fromView(s, to), s);
    }
    assert_eq!(flipped.convertSize_toView(NSSize::new(-3.0, -4.0), Some(&root)), s);
}

fn backing_alignment(mtm: MainThreadMarker) {
    use NSAlignmentOptions as A;
    // Values chosen to align alike at a scale of 1 or 2.
    let w = window(mtm, true);
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    w.setContentView(Some(&content));
    let v = NSView::initWithFrame(NSView::alloc(mtm), rect(10.0, 20.0, 100.0, 100.0));
    content.addSubview(&v);
    let aligned = |r: NSRect, options: A| v.backingAlignedRect_options(r, options);
    // Origin and size to the nearest pixel.
    assert_eq!(v.centerScanRect(rect(0.1, 0.9, 10.2, 9.8)), rect(0.0, 1.0, 10.0, 10.0));
    assert_eq!(aligned(rect(0.9, 1.9, 10.2, 9.3), A::AlignAllEdgesInward), rect(1.0, 2.0, 10.0, 9.0));
    assert_eq!(aligned(rect(0.1, 1.1, 10.8, 9.8), A::AlignAllEdgesOutward), rect(0.0, 1.0, 11.0, 10.0));
    assert_eq!(aligned(rect(0.1, 0.9, 10.0, 10.0), A::AlignAllEdgesNearest), rect(0.0, 1.0, 10.0, 10.0));
    // An edge and a size.
    let mixed = A::AlignMinXInward | A::AlignWidthOutward | A::AlignMinYNearest | A::AlignHeightNearest;
    assert_eq!(aligned(rect(0.9, 0.1, 10.9, 9.9), mixed), rect(1.0, 0.0, 11.0, 10.0));
    w.setContentView(None);
}

fn scrolling_helpers(mtm: MainThreadMarker) {
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    let doc = make(mtm, "doc", rect(0.0, 0.0, 400.0, 1000.0), true, true);
    scroll.setDocumentView(Some(&doc));
    let clip = scroll.contentView();
    let origin = || clip.bounds().origin;

    // scrollPoint: stays within the document.
    doc.scrollPoint(pt(10.0, 300.0));
    assert_eq!(origin(), pt(10.0, 300.0));
    doc.scrollPoint(pt(1000.0, 5000.0));
    assert_eq!(origin(), pt(200.0, 900.0));
    doc.scrollPoint(pt(-10.0, -10.0));
    assert_eq!(origin(), pt(0.0, 0.0));
    // Pure geometry, no window needed.
    doc.scrollPoint(pt(0.0, 900.0));
    assert_eq!(doc.visibleRect(), rect(0.0, 900.0, 200.0, 100.0));
    doc.scrollPoint(pt(0.0, 0.0));

    // scrollRectToVisible: moves as little as it can and says whether it
    // moved.
    assert!(!doc.scrollRectToVisible(rect(0.0, 50.0, 10.0, 10.0)));
    assert!(doc.scrollRectToVisible(rect(0.0, 500.0, 10.0, 10.0)));
    assert_eq!(origin(), pt(0.0, 410.0));
    assert!(doc.scrollRectToVisible(rect(300.0, 100.0, 10.0, 10.0)));
    assert_eq!(origin(), pt(110.0, 100.0));
    // Taller than the view: the near edge.
    assert!(doc.scrollRectToVisible(rect(0.0, 200.0, 50.0, 300.0)));
    assert_eq!(origin(), pt(0.0, 200.0));
    clip.scrollToPoint(pt(0.0, 900.0));
    assert!(doc.scrollRectToVisible(rect(0.0, 300.0, 10.0, 300.0)));
    assert_eq!(origin(), pt(0.0, 500.0));
    // Past the document: as far as it goes.
    assert!(doc.scrollRectToVisible(rect(0.0, 5000.0, 50.0, 10.0)));
    assert_eq!(origin(), pt(0.0, 900.0));
    // Wider than the view on both sides: stays.
    clip.scrollToPoint(pt(100.0, 300.0));
    assert!(!doc.scrollRectToVisible(rect(-50.0, 300.0, 500.0, 50.0)));
    assert_eq!(origin(), pt(100.0, 300.0));
    // From a subview, in its coordinates.
    let inner = make(mtm, "inner", rect(50.0, 400.0, 20.0, 20.0), false, true);
    doc.addSubview(&inner);
    clip.scrollToPoint(pt(0.0, 0.0));
    assert!(inner.scrollRectToVisible(rect(0.0, 0.0, 20.0, 20.0)));
    assert_eq!(origin(), pt(0.0, 320.0));
    inner.scrollPoint(pt(0.0, 20.0));
    assert_eq!(origin(), pt(50.0, 400.0));
    // A clip view scrolls itself; a view outside one can't scroll.
    assert!(clip.scrollRectToVisible(rect(0.0, 700.0, 10.0, 10.0)));
    assert_eq!(origin(), pt(0.0, 610.0));
    clip.scrollPoint(pt(0.0, 100.0));
    assert_eq!(origin(), pt(0.0, 100.0));
    scroll.scrollPoint(pt(0.0, 5.0));
    assert_eq!(origin(), pt(0.0, 100.0));
    let outside = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    assert!(!outside.scrollRectToVisible(rect(0.0, 0.0, 5.0, 5.0)));

    // The same in an unflipped document.
    let up = make(mtm, "up", rect(0.0, 0.0, 400.0, 1000.0), false, true);
    scroll.setDocumentView(Some(&up));
    let clip = scroll.contentView();
    let origin = || clip.bounds().origin;
    assert_eq!(origin(), pt(0.0, 0.0));
    assert!(up.scrollRectToVisible(rect(0.0, 500.0, 10.0, 10.0)));
    assert_eq!(origin(), pt(0.0, 410.0));
    assert!(up.scrollRectToVisible(rect(0.0, 100.0, 10.0, 10.0)));
    assert_eq!(origin(), pt(0.0, 100.0));
    assert!(up.scrollRectToVisible(rect(0.0, 300.0, 10.0, 300.0)));
    assert_eq!(origin(), pt(0.0, 300.0));
    clip.scrollToPoint(pt(0.0, 900.0));
    assert!(up.scrollRectToVisible(rect(0.0, 300.0, 10.0, 300.0)));
    assert_eq!(origin(), pt(0.0, 500.0));
    take();
}

fn moving_between_windows(mtm: MainThreadMarker) {
    let w = window(mtm, true);
    let (cv, cv1) = (rec(mtm, "cv"), rec(mtm, "cv1"));
    cv.addSubview(&cv1);
    take();
    // A content view's superview is the window's own business; the window
    // messages go to it and then its subviews.
    w.setContentView(Some(&cv));
    let moves = ["Window"];
    assert_eq!(
        only(take(), &moves),
        ["cv willMoveToWindow window", "cv1 willMoveToWindow window", "cv1 didMoveToWindow", "cv didMoveToWindow"]
    );
    assert!(cv1.window().is_some_and(|x| std::ptr::eq(&*x, &*w)));

    let (late, late1) = (rec(mtm, "late"), rec(mtm, "late1"));
    late.addSubview(&late1);
    take();
    cv.addSubview(&late);
    assert_eq!(
        take(),
        [
            "late willMoveToSuperview cv",
            "cv didAddSubview late",
            "late didMoveToSuperview",
            "late willMoveToWindow window",
            "late1 willMoveToWindow window",
            "late1 didMoveToWindow",
            "late didMoveToWindow",
        ]
    );
    assert!(late1.window().is_some());
    late.removeFromSuperview();
    assert_eq!(
        take(),
        [
            "late willMoveToSuperview nil",
            "cv willRemoveSubview late",
            "late didMoveToSuperview",
            "late willMoveToWindow nil",
            "late1 willMoveToWindow nil",
            "late1 didMoveToWindow",
            "late didMoveToWindow",
        ]
    );
    assert!(late1.window().is_none());
    // Within the window: the window messages all the same.
    cv.addSubview(&late);
    take();
    cv1.addSubview(&late);
    assert_eq!(
        take(),
        [
            "late willMoveToSuperview cv1",
            "cv willRemoveSubview late",
            "cv1 didAddSubview late",
            "late didMoveToSuperview",
            "late willMoveToWindow window",
            "late1 willMoveToWindow window",
            "late1 didMoveToWindow",
            "late didMoveToWindow",
        ]
    );
    // Out to a view without a window.
    let other = rec(mtm, "other");
    other.addSubview(&late);
    assert_eq!(
        take(),
        [
            "late willMoveToSuperview other",
            "cv1 willRemoveSubview late",
            "other didAddSubview late",
            "late didMoveToSuperview",
            "late willMoveToWindow nil",
            "late1 willMoveToWindow nil",
            "late1 didMoveToWindow",
            "late didMoveToWindow",
        ]
    );
    // Neither superview in a window: no window messages.
    let lone = rec(mtm, "lone");
    lone.addSubview(&late);
    assert_eq!(only(take(), &moves), NONE);

    w.setContentView(None);
    assert_eq!(
        only(take(), &moves),
        ["cv willMoveToWindow nil", "cv1 willMoveToWindow nil", "cv1 didMoveToWindow", "cv didMoveToWindow"]
    );
    assert!(cv1.window().is_none());
}

fn the_layout_pass(mtm: MainThreadMarker) {
    let w = window(mtm, true);
    let (cv, cv1, late, late1) = (rec(mtm, "cv"), rec(mtm, "cv1"), rec(mtm, "late"), rec(mtm, "late1"));
    cv.addSubview(&cv1);
    cv1.addSubview(&late);
    late.addSubview(&late1);
    // New views need both.
    assert!(cv.needsLayout() && late1.needsLayout());
    assert!(cv.needsUpdateConstraints() && late1.needsUpdateConstraints());
    w.setContentView(Some(&cv));
    take();
    let pass = ["layout", "updateConstraints"];
    // Constraints from the leaves up, then layout from the top down.
    w.layoutIfNeeded();
    assert_eq!(
        only(take(), &pass),
        [
            "late1 updateConstraints",
            "late updateConstraints",
            "cv1 updateConstraints",
            "cv updateConstraints",
            "cv layout",
            "cv1 layout",
            "late layout",
            "late1 layout",
        ]
    );
    assert!(!cv.needsLayout() && !cv1.needsLayout() && !cv.needsUpdateConstraints());
    w.layoutIfNeeded();
    assert_eq!(take(), NONE);

    // Only flagged views; flagging a view doesn't flag its superview.
    cv1.setNeedsLayout(true);
    assert!(cv1.needsLayout() && !cv.needsLayout());
    w.layoutIfNeeded();
    assert_eq!(only(take(), &pass), ["cv1 layout"]);
    late.setNeedsLayout(true);
    cv.setNeedsLayout(true);
    w.layoutIfNeeded();
    assert_eq!(only(take(), &pass), ["cv layout", "late layout"]);
    // Asking for no layout doesn't take back a request.
    cv1.setNeedsLayout(true);
    cv1.setNeedsLayout(false);
    assert!(cv1.needsLayout());
    cv1.setNeedsUpdateConstraints(true);
    cv1.setNeedsUpdateConstraints(false);
    assert!(cv1.needsUpdateConstraints());
    w.layoutIfNeeded();
    assert_eq!(only(take(), &pass), ["cv1 updateConstraints", "cv1 layout"]);

    // A subtree lays out what is flagged in it, not above it.
    late1.setNeedsLayout(true);
    cv1.layoutSubtreeIfNeeded();
    assert_eq!(only(take(), &pass), ["late1 layout"]);
    cv.setNeedsLayout(true);
    cv1.layoutSubtreeIfNeeded();
    assert_eq!(only(take(), &pass), NONE);
    assert!(cv.needsLayout());
    cv.layoutSubtreeIfNeeded();
    assert_eq!(only(take(), &pass), ["cv layout"]);

    // Constraints alone.
    w.updateConstraintsIfNeeded();
    assert_eq!(take(), NONE);
    cv1.setNeedsUpdateConstraints(true);
    assert!(cv1.needsUpdateConstraints() && !cv.needsUpdateConstraints());
    cv1.setNeedsLayout(true);
    w.updateConstraintsIfNeeded();
    assert_eq!(only(take(), &pass), ["cv1 updateConstraints"]);
    assert!(cv1.needsLayout());
    w.layoutIfNeeded();
    assert_eq!(only(take(), &pass), ["cv1 layout"]);
    late.setNeedsUpdateConstraints(true);
    late.updateConstraintsForSubtreeIfNeeded();
    assert_eq!(only(take(), &pass), ["late updateConstraints"]);

    // A new size needs layout; a new origin doesn't.
    late.setFrameSize(NSSize::new(20.0, 20.0));
    assert!(late.needsLayout() && !cv1.needsLayout());
    w.layoutIfNeeded();
    assert_eq!(only(take(), &["layout"]), ["late layout"]);
    late.setFrameOrigin(pt(2.0, 2.0));
    assert!(!late.needsLayout());
    // A view joining the window brings its flags along.
    let x = rec(mtm, "x");
    late1.addSubview(&x);
    take();
    w.layoutIfNeeded();
    assert_eq!(only(take(), &pass), ["x updateConstraints", "x layout"]);
    w.setContentView(None);
    take();
}

fn layout_cycles(mtm: MainThreadMarker) {
    // A view asking for layout in its own layout is laid out once a pass,
    // however deep it is, and is left done.
    for depth in 1..=4 {
        let root = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
        let mut at = root.clone();
        for _ in 1..depth {
            let v = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 50.0, 50.0));
            at.addSubview(&v);
            at = v;
        }
        let leaf = mover(mtm, "leaf", InLayout::FlagsItself);
        at.addSubview(&leaf);
        take();
        root.layoutSubtreeIfNeeded();
        assert_eq!(only(take(), &["layout"]), ["leaf layout"], "depth {depth}");
        assert!(!leaf.needsLayout());
        root.layoutSubtreeIfNeeded();
        assert_eq!(only(take(), &["layout"]), NONE);
    }

    // A view asking for its superview's layout gets it in the same pass.
    let (p, c) = (mover(mtm, "p", InLayout::Nothing), mover(mtm, "c", InLayout::FlagsSuperview));
    p.addSubview(&c);
    take();
    p.layoutSubtreeIfNeeded();
    assert_eq!(take(), ["c updateConstraints", "p updateConstraints", "p layout", "c layout", "p layout"]);
    assert!(!p.needsLayout());

    // A view added during layout is laid out, then has its constraints
    // updated, in the same pass.
    let w = window(mtm, true);
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    w.setContentView(Some(&content));
    let (p, new) = (mover(mtm, "p", InLayout::AddsChild), mover(mtm, "new", InLayout::Nothing));
    mover_ivars(&p).child.replace(Some(new.clone()));
    content.addSubview(&p);
    take();
    w.layoutIfNeeded();
    assert_eq!(take(), ["p updateConstraints", "p layout", "new layout", "new updateConstraints"]);
    assert!(!new.needsLayout() && !new.needsUpdateConstraints());
    w.setContentView(None);
    take();
}

fn callbacks_that_move_views(mtm: MainThreadMarker) {
    // A view that, joining a window, takes a sibling out: the sibling
    // doesn't join the window.
    let w = window(mtm, true);
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    w.setContentView(Some(&content));
    let p = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 100.0, 100.0));
    let (s1, s2) = (mover(mtm, "s1", InLayout::Nothing), NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO));
    mover_ivars(&s1).victim.replace(Some(s2.clone()));
    p.addSubview(&s1);
    p.addSubview(&s2);
    content.addSubview(&p);
    // SAFETY: plain property reads.
    assert!(unsafe { s2.superview() }.is_none() && s2.window().is_none());
    assert!(s1.window().is_some() && p.subviews().count() == 1);
    w.setContentView(None);
    assert!(s1.window().is_none());
    take();
}

fn display_lays_out_and_prepares(mtm: MainThreadMarker) {
    // A window with its backing store, never shown: display lays out and
    // sends viewWillDraw, though nothing is drawn.
    let w = window(mtm, false);
    let cv = rec(mtm, "cv");
    let cv1 = make(mtm, "cv1", rect(10.0, 10.0, 50.0, 50.0), false, true);
    // It doesn't pass viewWillDraw on.
    let quiet = make(mtm, "quiet", rect(100.0, 100.0, 50.0, 50.0), false, false);
    let quiet1 = rec(mtm, "quiet1");
    cv.addSubview(&cv1);
    cv.addSubview(&quiet);
    quiet.addSubview(&quiet1);
    w.setContentView(Some(&cv));
    take();
    cv.display();
    assert_eq!(
        only(take(), &["layout", "updateConstraints", "viewWillDraw"]),
        [
            "cv1 updateConstraints",
            "quiet1 updateConstraints",
            "quiet updateConstraints",
            "cv updateConstraints",
            "cv layout",
            "cv1 layout",
            "quiet layout",
            "quiet1 layout",
            "cv viewWillDraw",
            "cv1 viewWillDraw",
            "quiet viewWillDraw",
        ]
    );
    w.setContentView(None);
    take();
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("subviews_are_snapshots", subviews_are_snapshots),
        ("adding_and_removing", adding_and_removing),
        ("positioned_subviews", positioned_subviews),
        ("replacing_subviews", replacing_subviews),
        ("setting_subviews", setting_subviews),
        ("sorting_subviews", sorting_subviews),
        ("ancestry", ancestry),
        ("hiding_tells_shown_subviews", hiding_tells_shown_subviews),
        ("view_state", view_state),
        ("autoresizing_subviews_is_optional", autoresizing_subviews_is_optional),
        ("scaled_bounds", scaled_bounds),
        ("converting_sizes", converting_sizes),
        ("backing_alignment", backing_alignment),
        ("scrolling_helpers", scrolling_helpers),
        ("moving_between_windows", moving_between_windows),
        ("the_layout_pass", the_layout_pass),
        ("layout_cycles", layout_cycles),
        ("callbacks_that_move_views", callbacks_that_move_views),
        ("display_lays_out_and_prepares", display_lays_out_and_prepares),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        // What views heard as they went away.
        take();
        println!("test {name} ... ok");
    }
}
