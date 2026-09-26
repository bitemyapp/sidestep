//! NSStackView, checked on macOS and on Linux alike: its defaults, the
//! arranged subviews and gravities, each distribution and alignment, edge
//! insets and spacing, hidden views dropping out, visibility priorities,
//! hugging and compression, a stack sized by its content, and the delegate.
//!
//! Sizes are chosen so no frame needs rounding. Windows are never shown.
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSLayoutAttribute, NSLayoutConstraint, NSLayoutConstraintOrientation, NSResponder, NSStackView,
    NSStackViewDelegate, NSStackViewDistribution, NSStackViewGravity, NSUserInterfaceLayoutOrientation, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize};

use sidestep as _;

struct Ivars {
    size: Cell<NSSize>,
}

define_class!(
    /// A view with an intrinsic size.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceStackItem"]
    #[ivars = Ivars]
    struct Item;

    impl Item {
        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            self.ivars().size.get()
        }
    }

    unsafe impl NSObjectProtocol for Item {}
);

fn item(mtm: MainThreadMarker, w: f64, h: f64) -> Retained<NSView> {
    let this = Item::alloc(mtm).set_ivars(Ivars { size: Cell::new(NSSize::new(w, h)) });
    // SAFETY: the superclass's designated initializer.
    let view: Retained<Item> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
    Retained::into_super(view)
}

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceStackDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSStackViewDelegate for Delegate {
        #[unsafe(method(stackView:willDetachViews:))]
        fn will_detach(&self, _stack: &NSStackView, views: &NSArray<NSView>) {
            LOG.with(|l| l.borrow_mut().push(format!("detach {}", views.count())));
        }

        #[unsafe(method(stackView:didReattachViews:))]
        fn did_reattach(&self, _stack: &NSStackView, views: &NSArray<NSView>) {
            LOG.with(|l| l.borrow_mut().push(format!("reattach {}", views.count())));
        }
    }
);

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn window(mtm: MainThreadMarker) -> (Retained<NSWindow>, Retained<NSView>) {
    // SAFETY: a plain window, never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 400.0, 300.0));
    w.setContentView(Some(&content));
    (w, content)
}

/// A stack of three views: 40×20, 60×30 and 50×10.
fn three(mtm: MainThreadMarker) -> (Retained<NSStackView>, [Retained<NSView>; 3]) {
    let s = NSStackView::new(mtm);
    let views = [item(mtm, 40.0, 20.0), item(mtm, 60.0, 30.0), item(mtm, 50.0, 10.0)];
    for v in &views {
        s.addArrangedSubview(v);
    }
    (s, views)
}

fn frames(views: &[Retained<NSView>]) -> Vec<NSRect> {
    views.iter().map(|v| v.frame()).collect()
}

fn same(a: &NSView, b: &NSView) -> bool {
    std::ptr::eq(a, b)
}

fn defaults(mtm: MainThreadMarker) {
    use NSLayoutConstraintOrientation as O;
    let s = NSStackView::new(mtm);
    assert_eq!(s.frame(), NSRect::ZERO);
    assert_eq!(s.orientation(), NSUserInterfaceLayoutOrientation::Horizontal);
    assert_eq!(s.alignment(), NSLayoutAttribute::CenterY);
    assert_eq!(s.distribution(), NSStackViewDistribution::GravityAreas);
    assert_eq!(s.spacing(), 8.0);
    let i = s.edgeInsets();
    assert_eq!((i.top, i.left, i.bottom, i.right), (0.0, 0.0, 0.0, 0.0));
    assert!(s.detachesHiddenViews());
    assert!(s.translatesAutoresizingMaskIntoConstraints());
    assert!(!s.isFlipped());
    // Just below a view's default hugging.
    let below = f32::from_bits(250f32.to_bits() - 1);
    assert_eq!(s.huggingPriorityForOrientation(O::Horizontal), below);
    assert_eq!(s.huggingPriorityForOrientation(O::Vertical), below);
    assert_eq!(s.clippingResistancePriorityForOrientation(O::Horizontal), 1000.0);
    assert_eq!(s.clippingResistancePriorityForOrientation(O::Vertical), 1000.0);
    assert_eq!(s.contentHuggingPriorityForOrientation(O::Horizontal), 250.0);
    // A centered alignment turns with the orientation.
    s.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    assert_eq!(s.alignment(), NSLayoutAttribute::CenterX);
    s.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    assert_eq!(s.alignment(), NSLayoutAttribute::CenterY);
    s.setHuggingPriority_forOrientation(300.0, O::Horizontal);
    s.setClippingResistancePriority_forOrientation(700.0, O::Vertical);
    assert_eq!(s.huggingPriorityForOrientation(O::Horizontal), 300.0);
    assert_eq!(s.clippingResistancePriorityForOrientation(O::Vertical), 700.0);

    let made = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[item(mtm, 1.0, 1.0)]), mtm);
    assert!(!made.translatesAutoresizingMaskIntoConstraints());
    assert_eq!(made.arrangedSubviews().count(), 1);
    assert_eq!(made.distribution(), NSStackViewDistribution::GravityAreas);
}

fn arranging(mtm: MainThreadMarker) {
    let s = NSStackView::new(mtm);
    let (a, b, c) = (item(mtm, 40.0, 20.0), item(mtm, 60.0, 30.0), item(mtm, 50.0, 10.0));
    assert!(a.translatesAutoresizingMaskIntoConstraints());
    s.addArrangedSubview(&a);
    // Arranged views are subviews placed by constraints, in the leading
    // gravity; the stack's own constraints aren't listed.
    assert!(!a.translatesAutoresizingMaskIntoConstraints());
    assert_eq!((s.subviews().count(), s.arrangedSubviews().count(), s.views().count()), (1, 1, 1));
    assert_eq!(s.viewsInGravity(NSStackViewGravity::Leading).count(), 1);
    assert_eq!(s.constraints().count(), 0);
    assert_eq!(s.customSpacingAfterView(&a), f32::MAX as f64);
    assert_eq!(s.visibilityPriorityForView(&a), 1000.0);
    s.addArrangedSubview(&b);
    s.insertArrangedSubview_atIndex(&c, 0);
    let order: Vec<bool> = s.arrangedSubviews().iter().zip([&c, &a, &b]).map(|(x, y)| same(&x, y)).collect();
    assert_eq!(order, [true, true, true]);
    // Removing an arranged view keeps it as a subview.
    s.removeArrangedSubview(&c);
    assert_eq!((s.arrangedSubviews().count(), s.subviews().count()), (2, 3));
    // SAFETY: the superview is alive while the view is in it.
    assert!(unsafe { c.superview() }.is_some_and(|x| same(&x, &s)));
    // Leaving the stack stops arranging it.
    b.removeFromSuperview();
    assert_eq!(s.arrangedSubviews().count(), 1);
}

fn fitting(mtm: MainThreadMarker) {
    let (s, _) = three(mtm);
    assert_eq!(s.fittingSize(), NSSize::new(166.0, 30.0));
    s.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    assert_eq!(s.fittingSize(), NSSize::new(60.0, 76.0));
}

fn distributions(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (s, views) = three(mtm);
    cv.addSubview(&s);
    let lay = |dist, width: f64| {
        s.setDistribution(dist);
        s.setFrame(rect(10.0, 10.0, width, 50.0));
        w.layoutIfNeeded();
        frames(&views)
    };
    // Packed at the leading edge, centered across.
    assert_eq!(
        lay(NSStackViewDistribution::GravityAreas, 316.0),
        [rect(0.0, 15.0, 40.0, 20.0), rect(48.0, 10.0, 60.0, 30.0), rect(116.0, 20.0, 50.0, 10.0)]
    );
    // Filling: the first of views hugging alike takes the room.
    assert_eq!(
        lay(NSStackViewDistribution::Fill, 316.0),
        [rect(0.0, 15.0, 190.0, 20.0), rect(198.0, 10.0, 60.0, 30.0), rect(266.0, 20.0, 50.0, 10.0)]
    );
    assert_eq!(
        lay(NSStackViewDistribution::FillEqually, 316.0),
        [rect(0.0, 15.0, 100.0, 20.0), rect(108.0, 10.0, 100.0, 30.0), rect(216.0, 20.0, 100.0, 10.0)]
    );
    // In proportion to their intrinsic widths (40:60:50 of 300).
    assert_eq!(
        lay(NSStackViewDistribution::FillProportionally, 316.0),
        [rect(0.0, 15.0, 80.0, 20.0), rect(88.0, 10.0, 120.0, 30.0), rect(216.0, 20.0, 100.0, 10.0)]
    );
    assert_eq!(
        lay(NSStackViewDistribution::EqualSpacing, 316.0),
        [rect(0.0, 15.0, 40.0, 20.0), rect(123.0, 10.0, 60.0, 30.0), rect(266.0, 20.0, 50.0, 10.0)]
    );
    // Centers 20, 155, 290 apart alike.
    assert_eq!(
        lay(NSStackViewDistribution::EqualCentering, 315.0),
        [rect(0.0, 15.0, 40.0, 20.0), rect(125.0, 10.0, 60.0, 30.0), rect(265.0, 20.0, 50.0, 10.0)]
    );
    // Too small: equal widths, or proportional ones, all shrink.
    assert_eq!(
        lay(NSStackViewDistribution::FillEqually, 100.0),
        [rect(0.0, 15.0, 28.0, 20.0), rect(36.0, 10.0, 28.0, 30.0), rect(72.0, 20.0, 28.0, 10.0)]
    );
    w.setContentView(None);
}

fn alignments(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (s, views) = three(mtm);
    cv.addSubview(&s);
    s.setFrame(rect(0.0, 0.0, 300.0, 50.0));
    let lay = |alignment| {
        s.setAlignment(alignment);
        w.layoutIfNeeded();
        frames(&views).iter().map(|f| f.origin.y).collect::<Vec<_>>()
    };
    assert_eq!(lay(NSLayoutAttribute::Top), [30.0, 20.0, 40.0]);
    assert_eq!(lay(NSLayoutAttribute::Bottom), [0.0, 0.0, 0.0]);
    assert_eq!(lay(NSLayoutAttribute::CenterY), [15.0, 10.0, 20.0]);
    // Baselines at the top of views without text.
    assert_eq!(lay(NSLayoutAttribute::FirstBaseline), [30.0, 20.0, 40.0]);

    // Down a column: from the top, across by x.
    s.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    s.setAlignment(NSLayoutAttribute::CenterX);
    s.setFrame(rect(0.0, 0.0, 100.0, 200.0));
    w.layoutIfNeeded();
    assert_eq!(
        frames(&views),
        [rect(30.0, 180.0, 40.0, 20.0), rect(20.0, 142.0, 60.0, 30.0), rect(25.0, 124.0, 50.0, 10.0)]
    );
    let lay = |alignment| {
        s.setAlignment(alignment);
        w.layoutIfNeeded();
        frames(&views).iter().map(|f| f.origin.x).collect::<Vec<_>>()
    };
    assert_eq!(lay(NSLayoutAttribute::Leading), [0.0, 0.0, 0.0]);
    assert_eq!(lay(NSLayoutAttribute::Trailing), [60.0, 40.0, 50.0]);
    assert_eq!(lay(NSLayoutAttribute::CenterX), [30.0, 20.0, 25.0]);
    w.setContentView(None);
}

fn insets_and_spacing(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (s, views) = three(mtm);
    cv.addSubview(&s);
    s.setFrame(rect(0.0, 0.0, 300.0, 50.0));
    s.setEdgeInsets(NSEdgeInsets { top: 5.0, left: 20.0, bottom: 7.0, right: 30.0 });
    w.layoutIfNeeded();
    assert_eq!(
        frames(&views),
        [rect(20.0, 15.0, 40.0, 20.0), rect(68.0, 10.0, 60.0, 30.0), rect(136.0, 20.0, 50.0, 10.0)]
    );
    s.setDistribution(NSStackViewDistribution::Fill);
    w.layoutIfNeeded();
    assert_eq!(
        frames(&views),
        [rect(20.0, 15.0, 124.0, 20.0), rect(152.0, 10.0, 60.0, 30.0), rect(220.0, 20.0, 50.0, 10.0)]
    );
    // The top inset moves views aligned to the top.
    s.setAlignment(NSLayoutAttribute::Top);
    w.layoutIfNeeded();
    assert_eq!(views[0].frame(), rect(20.0, 25.0, 124.0, 20.0));
    s.setAlignment(NSLayoutAttribute::CenterY);
    s.setDistribution(NSStackViewDistribution::GravityAreas);
    s.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 });
    s.setCustomSpacing_afterView(20.0, &views[0]);
    assert_eq!(s.customSpacingAfterView(&views[0]), 20.0);
    w.layoutIfNeeded();
    assert_eq!(frames(&views).iter().map(|f| f.origin.x).collect::<Vec<_>>(), [0.0, 60.0, 128.0]);
    s.setCustomSpacing_afterView(f32::MAX as f64, &views[0]);
    s.setSpacing(4.0);
    w.layoutIfNeeded();
    assert_eq!(frames(&views).iter().map(|f| f.origin.x).collect::<Vec<_>>(), [0.0, 44.0, 108.0]);
    w.setContentView(None);
}

fn hidden_views(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (s, [a, b, c]) = three(mtm);
    cv.addSubview(&s);
    s.setFrame(rect(0.0, 0.0, 300.0, 50.0));
    // SAFETY: the class's initializer.
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    s.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    w.layoutIfNeeded();
    let log = || LOG.with(|l| std::mem::take(&mut *l.borrow_mut()));
    log();
    // A hidden view drops out; the others close up.
    b.setHidden(true);
    w.layoutIfNeeded();
    assert_eq!((a.frame().origin.x, c.frame().origin.x), (0.0, 48.0));
    assert_eq!((s.detachedViews().count(), s.arrangedSubviews().count()), (1, 3));
    assert_eq!(log(), ["detach 1"]);
    b.setHidden(false);
    w.layoutIfNeeded();
    assert_eq!((b.frame().origin.x, c.frame().origin.x), (48.0, 116.0));
    assert_eq!(s.detachedViews().count(), 0);
    assert_eq!(log(), ["reattach 1"]);
    // Unless the stack keeps hidden views.
    s.setDetachesHiddenViews(false);
    b.setHidden(true);
    w.layoutIfNeeded();
    assert_eq!(c.frame().origin.x, 116.0);
    b.setHidden(false);
    s.setDetachesHiddenViews(true);

    // Not visible: hidden and dropped.
    s.setDistribution(NSStackViewDistribution::Fill);
    s.setVisibilityPriority_forView(0.0, &b);
    assert!(b.isHidden());
    w.layoutIfNeeded();
    assert_eq!(s.detachedViews().count(), 1);
    assert_eq!((a.frame().size.width, c.frame().origin.x), (242.0, 250.0));
    s.setVisibilityPriority_forView(1000.0, &b);
    assert!(!b.isHidden());
    w.layoutIfNeeded();
    assert_eq!(s.detachedViews().count(), 0);
    log();
    w.setContentView(None);
}

fn gravities(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (s, [a, b, c]) = three(mtm);
    cv.addSubview(&s);
    s.setFrame(rect(0.0, 0.0, 300.0, 50.0));
    s.removeArrangedSubview(&b);
    b.removeFromSuperview();
    s.addView_inGravity(&b, NSStackViewGravity::Trailing);
    let d = item(mtm, 30.0, 30.0);
    s.addView_inGravity(&d, NSStackViewGravity::Center);
    w.layoutIfNeeded();
    assert_eq!(
        frames(&[a.clone(), c.clone(), d.clone(), b.clone()]).iter().map(|f| f.origin.x).collect::<Vec<_>>(),
        [0.0, 48.0, 135.0, 240.0]
    );
    let count = |g| s.viewsInGravity(g).count();
    assert_eq!(
        (count(NSStackViewGravity::Leading), count(NSStackViewGravity::Center), count(NSStackViewGravity::Trailing)),
        (2, 1, 1)
    );
    // Arranged in gravity order.
    let order: Vec<bool> = s.arrangedSubviews().iter().zip([&a, &c, &d, &b]).map(|(x, y)| same(&x, y)).collect();
    assert_eq!(order, [true, true, true, true]);
    // Other distributions take the gravities in that order.
    s.setDistribution(NSStackViewDistribution::Fill);
    w.layoutIfNeeded();
    assert_eq!(
        frames(&[a.clone(), c.clone(), d.clone(), b.clone()]),
        [
            rect(0.0, 15.0, 136.0, 20.0),
            rect(144.0, 20.0, 50.0, 10.0),
            rect(202.0, 10.0, 30.0, 30.0),
            rect(240.0, 10.0, 60.0, 30.0)
        ]
    );
    w.setContentView(None);
}

fn priorities(mtm: MainThreadMarker) {
    const H: NSLayoutConstraintOrientation = NSLayoutConstraintOrientation::Horizontal;
    let (w, cv) = window(mtm);
    let (s, [a, b, c]) = three(mtm);
    cv.addSubview(&s);
    s.setDistribution(NSStackViewDistribution::Fill);
    s.setFrame(rect(0.0, 0.0, 300.0, 50.0));
    // The view that hugs least takes the room.
    b.setContentHuggingPriority_forOrientation(200.0, H);
    w.layoutIfNeeded();
    assert_eq!(
        frames(&[a.clone(), b.clone(), c.clone()]),
        [rect(0.0, 15.0, 40.0, 20.0), rect(48.0, 10.0, 194.0, 30.0), rect(250.0, 20.0, 50.0, 10.0)]
    );
    b.setContentHuggingPriority_forOrientation(250.0, H);
    // Too small: the view that resists least gives up its room first.
    s.setFrame(rect(0.0, 0.0, 100.0, 50.0));
    c.setContentCompressionResistancePriority_forOrientation(700.0, H);
    w.layoutIfNeeded();
    assert_eq!(c.frame().size.width, 0.0);
    assert_eq!(a.frame().size.width + b.frame().size.width, 84.0);
    w.setContentView(None);
}

fn sized_by_content(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let g = NSStackView::stackViewWithViews(
        &NSArray::from_retained_slice(&[item(mtm, 40.0, 20.0), item(mtm, 60.0, 30.0)]),
        mtm,
    );
    cv.addSubview(&g);
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&[
        g.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        g.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
    ]));
    w.layoutIfNeeded();
    assert_eq!(g.frame(), rect(0.0, 270.0, 108.0, 30.0));
    let arranged = g.arrangedSubviews();
    assert_eq!(arranged.objectAtIndex(0).frame(), rect(0.0, 5.0, 40.0, 20.0));
    assert_eq!(arranged.objectAtIndex(1).frame(), rect(48.0, 0.0, 60.0, 30.0));
    // A top-aligned stack's height takes in its top inset; a
    // bottom-aligned one's not its bottom one.
    let h = NSStackView::stackViewWithViews(
        &NSArray::from_retained_slice(&[item(mtm, 40.0, 20.0), item(mtm, 60.0, 30.0)]),
        mtm,
    );
    h.setEdgeInsets(NSEdgeInsets { top: 5.0, left: 20.0, bottom: 7.0, right: 30.0 });
    h.setAlignment(NSLayoutAttribute::Top);
    assert_eq!(h.fittingSize(), NSSize::new(158.0, 35.0));
    h.setAlignment(NSLayoutAttribute::Bottom);
    assert_eq!(h.fittingSize(), NSSize::new(158.0, 30.0));
    w.setContentView(None);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("defaults", defaults),
        ("arranging", arranging),
        ("fitting", fitting),
        ("distributions", distributions),
        ("alignments", alignments),
        ("insets_and_spacing", insets_and_spacing),
        ("hidden_views", hidden_views),
        ("gravities", gravities),
        ("priorities", priorities),
        ("sized_by_content", sized_by_content),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
