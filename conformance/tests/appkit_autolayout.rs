//! Auto Layout, checked on macOS and on Linux alike: constraints and their
//! defaults, anchors, where activation installs constraints, solving in and
//! out of windows (frames rounded to whole pixels), views that translate
//! their autoresizing masks, intrinsic sizes with hugging and compression
//! resistance, `fittingSize`, layout guides and ambiguity; priorities as
//! tiers, content that resizes its window, anchors' identity, constraints
//! and guides that outlive their views, and constrained views moving
//! between superviews.
//!
//! Windows are never shown. AppKit belongs to the main thread, so this
//! file has its own `main`.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSLayoutAttribute, NSLayoutConstraint,
    NSLayoutConstraintOrientation, NSLayoutGuide, NSLayoutRelation, NSResponder, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

struct Ivars {
    size: Cell<NSSize>,
    flipped: bool,
}

define_class!(
    /// A view with an intrinsic size, or a flipped one.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceLayoutView"]
    #[ivars = Ivars]
    struct Sized;

    impl Sized {
        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> NSSize {
            self.ivars().size.get()
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().flipped
        }
    }

    unsafe impl NSObjectProtocol for Sized {}
);

fn make(mtm: MainThreadMarker, frame: NSRect, size: NSSize, flipped: bool) -> Retained<NSView> {
    let this = Sized::alloc(mtm).set_ivars(Ivars { size: Cell::new(size), flipped });
    // SAFETY: the superclass's designated initializer.
    let view: Retained<Sized> = unsafe { msg_send![super(this), initWithFrame: frame] };
    Retained::into_super(view)
}

/// A view with an intrinsic size.
fn intrinsic(mtm: MainThreadMarker, w: f64, h: f64) -> Retained<NSView> {
    make(mtm, NSRect::ZERO, NSSize::new(w, h), false)
}

fn flipped(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    make(mtm, frame, NSSize::new(-1.0, -1.0), true)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn view(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    NSView::initWithFrame(NSView::alloc(mtm), frame)
}

/// A view placed by constraints alone.
fn constrained(mtm: MainThreadMarker) -> Retained<NSView> {
    let v = view(mtm, NSRect::ZERO);
    v.setTranslatesAutoresizingMaskIntoConstraints(false);
    v
}

fn window(mtm: MainThreadMarker) -> (Retained<NSWindow>, Retained<NSView>) {
    // SAFETY: a plain window, never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: Rust owns the window, so closing it mustn't release it.
    unsafe { w.setReleasedWhenClosed(false) };
    let content = view(mtm, rect(0.0, 0.0, 400.0, 300.0));
    w.setContentView(Some(&content));
    (w, content)
}

fn activate(constraints: &[Retained<NSLayoutConstraint>]) {
    NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(constraints));
}

fn is(object: Option<Retained<AnyObject>>, view: &NSView) -> bool {
    object.is_some_and(|o| std::ptr::eq(Retained::as_ptr(&o), (view as *const NSView).cast()))
}

fn defaults(mtm: MainThreadMarker) {
    let (horizontal, vertical) = (NSLayoutConstraintOrientation::Horizontal, NSLayoutConstraintOrientation::Vertical);
    let v = view(mtm, rect(0.0, 0.0, 10.0, 10.0));
    assert_eq!(v.contentHuggingPriorityForOrientation(horizontal), 250.0);
    assert_eq!(v.contentHuggingPriorityForOrientation(vertical), 250.0);
    assert_eq!(v.contentCompressionResistancePriorityForOrientation(horizontal), 750.0);
    assert_eq!(v.contentCompressionResistancePriorityForOrientation(vertical), 750.0);
    v.setContentHuggingPriority_forOrientation(300.0, vertical);
    v.setContentCompressionResistancePriority_forOrientation(900.0, horizontal);
    assert_eq!(v.contentHuggingPriorityForOrientation(vertical), 300.0);
    assert_eq!(v.contentCompressionResistancePriorityForOrientation(horizontal), 900.0);
    assert!(v.translatesAutoresizingMaskIntoConstraints());
    let insets = v.alignmentRectInsets();
    assert_eq!((insets.top, insets.left, insets.bottom, insets.right), (0.0, 0.0, 0.0, 0.0));
    assert_eq!(v.alignmentRectForFrame(rect(1.0, 2.0, 3.0, 4.0)), rect(1.0, 2.0, 3.0, 4.0));
    assert_eq!(v.frameForAlignmentRect(rect(1.0, 2.0, 3.0, 4.0)), rect(1.0, 2.0, 3.0, 4.0));
    assert_eq!(v.firstBaselineOffsetFromTop(), 0.0);
    assert_eq!(v.lastBaselineOffsetFromBottom(), 0.0);
    assert_eq!(v.baselineOffsetFromBottom(), 0.0);
    assert_eq!(v.fittingSize(), NSSize::ZERO);
    assert_eq!(v.constraints().count(), 0);
    assert!(!NSView::requiresConstraintBasedLayout(mtm));

    let c = v.widthAnchor().constraintEqualToConstant(50.0);
    assert_eq!(c.priority(), 1000.0);
    assert_eq!((c.constant(), c.multiplier()), (50.0, 1.0));
    assert!(!c.isActive());
    assert_eq!(c.relation(), NSLayoutRelation::Equal);
    assert_eq!(c.firstAttribute(), NSLayoutAttribute::Width);
    assert_eq!(c.secondAttribute(), NSLayoutAttribute::NotAnAttribute);
    // SAFETY: the items are alive for the whole test.
    assert!(is(unsafe { c.firstItem() }, &v));
    // SAFETY: the items are alive for the whole test.
    assert!(unsafe { c.secondItem() }.is_none() && c.secondAnchor().is_none());
    assert!(c.identifier().is_none());
    c.setIdentifier(Some(&NSString::from_str("width")));
    assert_eq!(c.identifier().unwrap().to_string(), "width");
    assert!(!c.shouldBeArchived());
    c.setPriority(700.0);
    c.setConstant(60.0);
    assert_eq!((c.priority(), c.constant()), (700.0, 60.0));
}

fn anchors(mtm: MainThreadMarker) {
    let v = view(mtm, rect(0.0, 0.0, 10.0, 10.0));
    let names = [
        v.leadingAnchor().name(),
        v.trailingAnchor().name(),
        v.leftAnchor().name(),
        v.rightAnchor().name(),
        v.topAnchor().name(),
        v.bottomAnchor().name(),
        v.widthAnchor().name(),
        v.heightAnchor().name(),
        v.centerXAnchor().name(),
        v.centerYAnchor().name(),
        v.firstBaselineAnchor().name(),
        v.lastBaselineAnchor().name(),
    ];
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    assert_eq!(
        names,
        [
            "leading",
            "trailing",
            "left",
            "right",
            "top",
            "bottom",
            "width",
            "height",
            "centerX",
            "centerY",
            "firstBaseline",
            "lastBaseline"
        ]
    );
    assert!(is(v.leadingAnchor().item(), &v));
    let class = |a: &AnyObject| a.class().name().to_str().unwrap().to_owned();
    assert_eq!(class(&v.leadingAnchor()), "NSLayoutXAxisAnchor");
    assert_eq!(class(&v.topAnchor()), "NSLayoutYAxisAnchor");
    assert_eq!(class(&v.widthAnchor()), "NSLayoutDimension");

    let x = view(mtm, NSRect::ZERO);
    let c = v.widthAnchor().constraintEqualToAnchor_multiplier_constant(&x.heightAnchor(), 2.0, 3.0);
    assert_eq!((c.multiplier(), c.constant()), (2.0, 3.0));
    assert_eq!((c.firstAttribute(), c.secondAttribute()), (NSLayoutAttribute::Width, NSLayoutAttribute::Height));
    // SAFETY: the items are alive for the whole test.
    assert!(is(unsafe { c.secondItem() }, &x));
    assert_eq!(c.firstAnchor().name().to_string(), "width");
    assert_eq!(c.secondAnchor().unwrap().name().to_string(), "height");
    let c = v.leadingAnchor().constraintGreaterThanOrEqualToAnchor_constant(&x.trailingAnchor(), 5.0);
    assert_eq!(c.relation(), NSLayoutRelation::GreaterThanOrEqual);
    let c = v.topAnchor().constraintLessThanOrEqualToAnchor(&x.bottomAnchor());
    assert_eq!((c.relation(), c.constant()), (NSLayoutRelation::LessThanOrEqual, 0.0));

    // The distance between two anchors has no item or name.
    let distance = x.leadingAnchor().anchorWithOffsetToAnchor(&x.trailingAnchor());
    assert_eq!(distance.name().to_string(), "");
    assert!(distance.item().is_none());
    let c = distance.constraintEqualToConstant(10.0);
    assert_eq!(c.firstAttribute(), NSLayoutAttribute::NotAnAttribute);
    // SAFETY: the items are alive for the whole test.
    assert!(unsafe { c.firstItem() }.is_none());

    // The item form.
    // SAFETY: the items are views, alive for the whole test.
    let c = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &x,
            NSLayoutAttribute::Width,
            NSLayoutRelation::GreaterThanOrEqual,
            None,
            NSLayoutAttribute::NotAnAttribute,
            1.0,
            42.0,
        )
    };
    assert_eq!((c.relation(), c.constant(), c.multiplier()), (NSLayoutRelation::GreaterThanOrEqual, 42.0, 1.0));
    assert_eq!(c.secondAttribute(), NSLayoutAttribute::NotAnAttribute);
    // SAFETY: the items are alive for the whole test.
    assert!(unsafe { c.secondItem() }.is_none() && c.secondAnchor().is_none());
    assert_eq!(c.firstAnchor().name().to_string(), "width");
    // SAFETY: the items are views, alive for the whole test.
    let c = unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            &x,
            NSLayoutAttribute::Leading,
            NSLayoutRelation::Equal,
            Some(&v),
            NSLayoutAttribute::CenterX,
            0.5,
            3.0,
        )
    };
    assert_eq!((c.firstAttribute(), c.secondAttribute()), (NSLayoutAttribute::Leading, NSLayoutAttribute::CenterX));
    assert_eq!(c.secondAnchor().unwrap().name().to_string(), "centerX");
}

fn installing(mtm: MainThreadMarker) {
    let v = view(mtm, rect(0.0, 0.0, 10.0, 10.0));
    let c = v.widthAnchor().constraintEqualToConstant(50.0);
    c.setActive(true);
    assert!(c.isActive());
    // One item: on the item itself.
    assert_eq!(v.constraints().count(), 1);
    c.setActive(false);
    assert_eq!(v.constraints().count(), 0);

    // Two: on the nearest view holding both.
    let (_w, cv) = window(mtm);
    let (a, b) = (constrained(mtm), constrained(mtm));
    let inner = constrained(mtm);
    cv.addSubview(&a);
    cv.addSubview(&b);
    a.addSubview(&inner);
    let cs = [
        a.widthAnchor().constraintEqualToConstant(100.0),
        a.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        b.leadingAnchor().constraintEqualToAnchor(&inner.trailingAnchor()),
        inner.widthAnchor().constraintEqualToAnchor(&a.widthAnchor()),
    ];
    activate(&cs);
    assert_eq!((a.constraints().count(), b.constraints().count(), cv.constraints().count()), (2, 0, 2));
    assert!(cs.iter().all(|c| c.isActive()));
    // Leaving the superview takes the constraints between the view and the
    // rest; those inside it stay.
    a.removeFromSuperview();
    assert!(!cs[1].isActive() && !cs[2].isActive());
    assert!(cs[0].isActive() && cs[3].isActive());
    assert_eq!((a.constraints().count(), cv.constraints().count()), (2, 0));
    NSLayoutConstraint::deactivateConstraints(&NSArray::from_retained_slice(&cs));
    assert_eq!(a.constraints().count(), 0);

    // addConstraint: installs on the receiver.
    cv.addSubview(&a);
    let c = a.heightAnchor().constraintEqualToConstant(5.0);
    cv.addConstraint(&c);
    assert!(c.isActive());
    assert_eq!((cv.constraints().count(), a.constraints().count()), (1, 0));
    a.removeConstraint(&c);
    assert!(c.isActive(), "only the view it's installed on removes it");
    cv.removeConstraint(&c);
    assert!(!c.isActive());
}

fn solving_in_a_window(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (a, b) = (constrained(mtm), constrained(mtm));
    cv.addSubview(&a);
    cv.addSubview(&b);
    let cs = [
        a.leadingAnchor().constraintEqualToAnchor_constant(&cv.leadingAnchor(), 20.0),
        a.topAnchor().constraintEqualToAnchor_constant(&cv.topAnchor(), 10.0),
        a.widthAnchor().constraintEqualToConstant(100.0),
        a.heightAnchor().constraintEqualToConstant(60.0),
        b.leadingAnchor().constraintEqualToAnchor_constant(&a.trailingAnchor(), 8.0),
        b.trailingAnchor().constraintEqualToAnchor_constant(&cv.trailingAnchor(), -20.0),
        b.centerYAnchor().constraintEqualToAnchor(&a.centerYAnchor()),
        b.heightAnchor().constraintEqualToAnchor_multiplier(&a.heightAnchor(), 0.5),
    ];
    activate(&cs);
    // Nothing moves until layout.
    assert_eq!(a.frame(), NSRect::ZERO);
    w.layoutIfNeeded();
    // The content view isn't flipped: top 10 is y = 300 − 10 − 60.
    assert_eq!(a.frame(), rect(20.0, 230.0, 100.0, 60.0));
    assert_eq!(b.frame(), rect(128.0, 245.0, 252.0, 30.0));
    // A new constant takes effect at the next layout.
    cs[0].setConstant(30.0);
    assert_eq!(a.frame(), rect(20.0, 230.0, 100.0, 60.0));
    w.layoutIfNeeded();
    assert_eq!(a.frame(), rect(30.0, 230.0, 100.0, 60.0));
    assert_eq!(b.frame(), rect(138.0, 245.0, 242.0, 30.0));
    // A weaker constraint gives way.
    let narrow = a.widthAnchor().constraintEqualToConstant(10.0);
    narrow.setPriority(999.0);
    narrow.setActive(true);
    w.layoutIfNeeded();
    assert_eq!(a.frame().size.width, 100.0);
    cs[2].setActive(false);
    w.layoutIfNeeded();
    assert_eq!(a.frame().size.width, 10.0);

    // Edges round to the window's pixels.
    let thin = constrained(mtm);
    cv.addSubview(&thin);
    activate(&[
        thin.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        thin.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
        thin.widthAnchor().constraintEqualToConstant(10.3),
        thin.heightAnchor().constraintEqualToConstant(10.3),
    ]);
    w.layoutIfNeeded();
    let scale = w.backingScaleFactor();
    let edge = |v: f64| (v * scale).round() / scale;
    assert_eq!(thin.frame(), rect(0.0, edge(289.7), edge(10.3), 300.0 - edge(289.7)));
    w.setContentView(None);
}

fn solving_outside_a_window(mtm: MainThreadMarker) {
    // Flipped: top is y.
    let root = flipped(mtm, rect(0.0, 0.0, 400.0, 300.0));
    let a = constrained(mtm);
    root.addSubview(&a);
    activate(&[
        a.leadingAnchor().constraintEqualToAnchor_constant(&root.leadingAnchor(), 20.0),
        a.topAnchor().constraintEqualToAnchor_constant(&root.topAnchor(), 10.0),
        a.widthAnchor().constraintEqualToConstant(100.0),
        a.heightAnchor().constraintEqualToConstant(50.0),
    ]);
    root.layoutSubtreeIfNeeded();
    assert_eq!(a.frame(), rect(20.0, 10.0, 100.0, 50.0));

    // Outside a window, edges round to whole points.
    let t = view(mtm, rect(0.0, 0.0, 100.0, 100.0));
    let parts: Vec<_> = (0..3)
        .map(|_| {
            let x = constrained(mtm);
            t.addSubview(&x);
            x
        })
        .collect();
    let mut cs = vec![
        parts[0].leadingAnchor().constraintEqualToAnchor(&t.leadingAnchor()),
        parts[1].leadingAnchor().constraintEqualToAnchor(&parts[0].trailingAnchor()),
        parts[2].leadingAnchor().constraintEqualToAnchor(&parts[1].trailingAnchor()),
        parts[2].trailingAnchor().constraintEqualToAnchor(&t.trailingAnchor()),
        parts[1].widthAnchor().constraintEqualToAnchor(&parts[0].widthAnchor()),
        parts[2].widthAnchor().constraintEqualToAnchor(&parts[0].widthAnchor()),
    ];
    for p in &parts {
        cs.push(p.topAnchor().constraintEqualToAnchor(&t.topAnchor()));
        cs.push(p.heightAnchor().constraintEqualToConstant(10.3));
    }
    activate(&cs);
    t.layoutSubtreeIfNeeded();
    let frames: Vec<NSRect> = parts.iter().map(|p| p.frame()).collect();
    assert_eq!(frames, [rect(0.0, 90.0, 33.0, 10.0), rect(33.0, 90.0, 34.0, 10.0), rect(67.0, 90.0, 33.0, 10.0)]);
}

fn translated_masks(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    // A view placed by its frame, and one constrained to it.
    let fixed = view(mtm, rect(10.0, 10.0, 50.0, 50.0));
    cv.addSubview(&fixed);
    let c2 = constrained(mtm);
    cv.addSubview(&c2);
    activate(&[
        c2.leadingAnchor().constraintEqualToAnchor_constant(&fixed.trailingAnchor(), 8.0),
        c2.bottomAnchor().constraintEqualToAnchor(&fixed.bottomAnchor()),
        c2.widthAnchor().constraintEqualToConstant(20.0),
        c2.heightAnchor().constraintEqualToConstant(20.0),
    ]);
    w.layoutIfNeeded();
    assert_eq!(c2.frame(), rect(68.0, 10.0, 20.0, 20.0));
    fixed.setFrame(rect(100.0, 100.0, 50.0, 50.0));
    w.layoutIfNeeded();
    assert_eq!(c2.frame(), rect(158.0, 100.0, 20.0, 20.0));

    // An autoresizing view inside a constrained one follows it, and views
    // constrained to it follow too.
    let holder = constrained(mtm);
    cv.addSubview(&holder);
    let inner = view(mtm, rect(10.0, 10.0, 80.0, 30.0));
    inner.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin);
    holder.addSubview(&inner);
    let width = holder.widthAnchor().constraintEqualToConstant(100.0);
    activate(&[
        holder.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        holder.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
        width.clone(),
        holder.heightAnchor().constraintEqualToConstant(50.0),
    ]);
    w.layoutIfNeeded();
    assert_eq!(holder.frame(), rect(0.0, 250.0, 100.0, 50.0));
    assert_eq!(inner.frame(), rect(10.0, 60.0, 180.0, 30.0));
    width.setConstant(200.0);
    w.layoutIfNeeded();
    assert_eq!(inner.frame(), rect(10.0, 60.0, 280.0, 30.0));
    let tail = constrained(mtm);
    cv.addSubview(&tail);
    activate(&[
        tail.leadingAnchor().constraintEqualToAnchor(&inner.trailingAnchor()),
        tail.topAnchor().constraintEqualToAnchor(&inner.topAnchor()),
        tail.widthAnchor().constraintEqualToConstant(5.0),
        tail.heightAnchor().constraintEqualToConstant(5.0),
    ]);
    w.layoutIfNeeded();
    assert_eq!(tail.frame(), rect(290.0, 335.0, 5.0, 5.0));
    width.setConstant(300.0);
    w.layoutIfNeeded();
    assert_eq!(holder.frame(), rect(0.0, 250.0, 300.0, 50.0));
    assert_eq!(inner.frame(), rect(10.0, 60.0, 380.0, 30.0));
    assert_eq!(tail.frame(), rect(390.0, 335.0, 5.0, 5.0));
    w.setContentView(None);
}

fn intrinsic_sizes(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let i = intrinsic(mtm, 40.0, 20.0);
    assert_eq!(i.fittingSize(), NSSize::new(40.0, 20.0));
    i.setTranslatesAutoresizingMaskIntoConstraints(false);
    cv.addSubview(&i);
    activate(&[
        i.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        i.bottomAnchor().constraintEqualToAnchor(&cv.bottomAnchor()),
    ]);
    w.layoutIfNeeded();
    assert_eq!(i.frame(), rect(0.0, 0.0, 40.0, 20.0));
    // Growing it beats hugging (250) only when stronger.
    let wide = i.widthAnchor().constraintEqualToConstant(100.0);
    wide.setPriority(200.0);
    wide.setActive(true);
    w.layoutIfNeeded();
    assert_eq!(i.frame().size.width, 40.0);
    wide.setPriority(300.0);
    w.layoutIfNeeded();
    assert_eq!(i.frame().size.width, 100.0);
    wide.setActive(false);
    // Shrinking it beats compression resistance (750) only when stronger.
    let narrow = i.widthAnchor().constraintEqualToConstant(10.0);
    narrow.setPriority(700.0);
    narrow.setActive(true);
    w.layoutIfNeeded();
    assert_eq!(i.frame().size.width, 40.0);
    narrow.setPriority(800.0);
    w.layoutIfNeeded();
    assert_eq!(i.frame().size.width, 10.0);
    i.setContentCompressionResistancePriority_forOrientation(900.0, NSLayoutConstraintOrientation::Horizontal);
    w.layoutIfNeeded();
    assert_eq!(i.frame().size.width, 40.0);
    w.setContentView(None);
}

fn fitting_sizes(mtm: MainThreadMarker) {
    let v = view(mtm, rect(0.0, 0.0, 10.0, 10.0));
    v.widthAnchor().constraintEqualToConstant(50.0).setActive(true);
    assert_eq!(v.fittingSize(), NSSize::new(50.0, 0.0));

    let holder = view(mtm, rect(0.0, 0.0, 10.0, 10.0));
    let (k1, k2) = (intrinsic(mtm, 30.0, 10.0), intrinsic(mtm, 50.0, 20.0));
    for k in [&k1, &k2] {
        k.setTranslatesAutoresizingMaskIntoConstraints(false);
        holder.addSubview(k);
    }
    activate(&[
        k1.leadingAnchor().constraintEqualToAnchor_constant(&holder.leadingAnchor(), 5.0),
        k2.leadingAnchor().constraintEqualToAnchor_constant(&k1.trailingAnchor(), 5.0),
        k2.trailingAnchor().constraintEqualToAnchor_constant(&holder.trailingAnchor(), -5.0),
        k1.topAnchor().constraintEqualToAnchor_constant(&holder.topAnchor(), 5.0),
        k2.topAnchor().constraintEqualToAnchor_constant(&holder.topAnchor(), 5.0),
        k1.bottomAnchor().constraintLessThanOrEqualToAnchor_constant(&holder.bottomAnchor(), -5.0),
        k2.bottomAnchor().constraintLessThanOrEqualToAnchor_constant(&holder.bottomAnchor(), -5.0),
    ]);
    // Its own constraints, whether or not it translates its mask; its
    // frame doesn't count.
    assert_eq!(holder.fittingSize(), NSSize::new(95.0, 30.0));
    assert_eq!(holder.frame(), rect(0.0, 0.0, 10.0, 10.0));
    holder.setTranslatesAutoresizingMaskIntoConstraints(false);
    assert_eq!(holder.fittingSize(), NSSize::new(95.0, 30.0));
    // Laid out by itself, it takes that size.
    holder.layoutSubtreeIfNeeded();
    assert_eq!(holder.frame(), rect(0.0, 0.0, 95.0, 30.0));
    assert_eq!(k1.frame(), rect(5.0, 15.0, 30.0, 10.0));
    assert_eq!(k2.frame(), rect(40.0, 5.0, 50.0, 20.0));
}

fn layout_guides(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let g = NSLayoutGuide::new();
    assert_eq!(g.frame(), NSRect::ZERO);
    assert!(g.owningView(mtm).is_none());
    assert_eq!(g.identifier().to_string(), "");
    cv.addLayoutGuide(&g);
    assert!(g.owningView(mtm).is_some_and(|o| std::ptr::eq(&*o, &*cv)));
    assert_eq!(cv.layoutGuides().count(), 1);
    activate(&[
        g.leadingAnchor().constraintEqualToAnchor_constant(&cv.leadingAnchor(), 7.0),
        g.topAnchor().constraintEqualToAnchor_constant(&cv.topAnchor(), 9.0),
        g.widthAnchor().constraintEqualToConstant(30.0),
        g.heightAnchor().constraintEqualToConstant(40.0),
    ]);
    w.layoutIfNeeded();
    // In the owning view's coordinates.
    assert_eq!(g.frame(), rect(7.0, 251.0, 30.0, 40.0));
    assert!(!g.hasAmbiguousLayout());
    let v = constrained(mtm);
    cv.addSubview(&v);
    activate(&[
        v.leadingAnchor().constraintEqualToAnchor(&g.trailingAnchor()),
        v.topAnchor().constraintEqualToAnchor(&g.bottomAnchor()),
        v.widthAnchor().constraintEqualToAnchor(&g.widthAnchor()),
        v.heightAnchor().constraintEqualToConstant(1.0),
    ]);
    w.layoutIfNeeded();
    assert_eq!(v.frame(), rect(37.0, 250.0, 30.0, 1.0));
    cv.removeLayoutGuide(&g);
    assert!(g.owningView(mtm).is_none());
    assert_eq!(cv.layoutGuides().count(), 0);
    w.setContentView(None);
}

fn ambiguity(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (whole, partial) = (constrained(mtm), constrained(mtm));
    cv.addSubview(&whole);
    cv.addSubview(&partial);
    activate(&[
        whole.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        whole.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
        whole.widthAnchor().constraintEqualToConstant(10.0),
        whole.heightAnchor().constraintEqualToConstant(10.0),
        partial.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
    ]);
    w.layoutIfNeeded();
    assert!(!whole.hasAmbiguousLayout());
    assert!(partial.hasAmbiguousLayout());
    // Asking moves nothing.
    assert_eq!(whole.frame(), rect(0.0, 290.0, 10.0, 10.0));
    let affecting = whole.constraintsAffectingLayoutForOrientation(NSLayoutConstraintOrientation::Horizontal);
    assert!(affecting.count() >= 2);
    w.setContentView(None);
}

fn moving_constrained_views(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let left = constrained(mtm);
    cv.addSubview(&left);
    let a = constrained(mtm);
    left.addSubview(&a);
    let cross = a.leadingAnchor().constraintEqualToAnchor_constant(&cv.leadingAnchor(), 30.0);
    let inner = a.leadingAnchor().constraintEqualToAnchor_constant(&left.leadingAnchor(), 5.0);
    inner.setPriority(500.0);
    let own = a.widthAnchor().constraintEqualToConstant(10.0);
    activate(&[
        left.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        left.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
        left.widthAnchor().constraintEqualToConstant(100.0),
        left.heightAnchor().constraintEqualToConstant(100.0),
        cross.clone(),
        inner.clone(),
        a.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
        own.clone(),
        a.heightAnchor().constraintEqualToConstant(10.0),
    ]);
    w.layoutIfNeeded();
    assert_eq!(a.frame(), rect(30.0, 90.0, 10.0, 10.0));
    // Straight to another superview: the constraints crossing the edge of
    // the tree it leaves go, and those on itself stay.
    let other = view(mtm, rect(0.0, 0.0, 50.0, 50.0));
    other.addSubview(&a);
    assert!(!cross.isActive() && !inner.isActive() && own.isActive());
    let holds = |v: &NSView, c: &NSLayoutConstraint| v.constraints().iter().any(|x| std::ptr::eq(&*x, c));
    assert!(!holds(&cv, &cross) && !holds(&left, &inner) && holds(&a, &own));

    // A view built with constraints inside it, then added, lays out by
    // them, and keeps them as it moves.
    let card = constrained(mtm);
    let label = constrained(mtm);
    card.addSubview(&label);
    let size =
        [card.widthAnchor().constraintEqualToConstant(50.0), card.heightAnchor().constraintEqualToConstant(40.0)];
    activate(&[
        label.leadingAnchor().constraintEqualToAnchor_constant(&card.leadingAnchor(), 4.0),
        label.topAnchor().constraintEqualToAnchor_constant(&card.topAnchor(), 3.0),
        label.widthAnchor().constraintEqualToConstant(20.0),
        label.heightAnchor().constraintEqualToConstant(10.0),
    ]);
    activate(&size);
    cv.addSubview(&card);
    let to_cv = [
        card.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        card.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
    ];
    activate(&to_cv);
    w.layoutIfNeeded();
    assert_eq!((card.frame(), label.frame()), (rect(0.0, 260.0, 50.0, 40.0), rect(4.0, 27.0, 20.0, 10.0)));
    // Moved further into the view its constraints are installed on, it
    // keeps them.
    let holder = view(mtm, rect(100.0, 100.0, 200.0, 100.0));
    cv.addSubview(&holder);
    holder.addSubview(&card);
    assert!(to_cv.iter().all(|c| c.isActive()));
    NSLayoutConstraint::deactivateConstraints(&NSArray::from_retained_slice(&to_cv));
    let to_holder = [
        card.leadingAnchor().constraintEqualToAnchor_constant(&holder.leadingAnchor(), 10.0),
        card.topAnchor().constraintEqualToAnchor(&holder.topAnchor()),
    ];
    activate(&to_holder);
    w.layoutIfNeeded();
    assert_eq!((card.frame(), label.frame()), (rect(10.0, 60.0, 50.0, 40.0), rect(4.0, 27.0, 20.0, 10.0)));
    // Moved out of it, it loses them.
    let other = view(mtm, rect(0.0, 0.0, 200.0, 100.0));
    cv.addSubview(&other);
    other.addSubview(&card);
    assert!(to_holder.iter().all(|c| !c.isActive()) && size.iter().all(|c| c.isActive()));
    activate(&[
        card.leadingAnchor().constraintEqualToAnchor_constant(&other.leadingAnchor(), 20.0),
        card.topAnchor().constraintEqualToAnchor(&other.topAnchor()),
    ]);
    w.layoutIfNeeded();
    assert_eq!((card.frame(), label.frame()), (rect(20.0, 60.0, 50.0, 40.0), rect(4.0, 27.0, 20.0, 10.0)));
    w.setContentView(None);
}

fn constraints_outlive_their_views(mtm: MainThreadMarker) {
    // Installed on the view that goes.
    let c = objc2::rc::autoreleasepool(|_| {
        let v = view(mtm, rect(0.0, 0.0, 10.0, 10.0));
        let c = v.widthAnchor().constraintEqualToConstant(50.0);
        c.setActive(true);
        assert!(c.isActive());
        c
    });
    assert!(!c.isActive());
    c.setConstant(7.0);
    c.setPriority(700.0);
    c.setActive(false);
    assert!(!c.isActive());

    // Installed on a superview that goes, while the view it places stays.
    let x = constrained(mtm);
    let c = objc2::rc::autoreleasepool(|_| {
        let p = view(mtm, rect(0.0, 0.0, 200.0, 200.0));
        p.addSubview(&x);
        let c = x.leadingAnchor().constraintEqualToAnchor_constant(&p.leadingAnchor(), 10.0);
        c.setActive(true);
        p.layoutSubtreeIfNeeded();
        c
    });
    // SAFETY: a plain property read.
    assert!(unsafe { x.superview() }.is_none());
    assert!(!c.isActive());
    c.setConstant(20.0);
    c.setActive(false);
    assert!(x.constraints().count() == 0);

    // A guide whose owning view goes has none.
    let g = NSLayoutGuide::new();
    objc2::rc::autoreleasepool(|_| {
        let v = view(mtm, rect(0.0, 0.0, 200.0, 200.0));
        v.addLayoutGuide(&g);
        g.widthAnchor().constraintEqualToConstant(5.0).setActive(true);
    });
    assert!(g.owningView(mtm).is_none());
}

/// Activating a constraint whose view has gone leaves it inactive (AppKit
/// crashes).
#[cfg(not(target_vendor = "apple"))]
fn activating_without_an_item(mtm: MainThreadMarker) {
    let p = view(mtm, rect(0.0, 0.0, 200.0, 200.0));
    let c = objc2::rc::autoreleasepool(|_| {
        let x = view(mtm, NSRect::ZERO);
        p.addSubview(&x);
        let c = x.leadingAnchor().constraintEqualToAnchor(&p.leadingAnchor());
        x.removeFromSuperview();
        c
    });
    c.setActive(true);
    assert!(!c.isActive());
    p.addSubview(&view(mtm, NSRect::ZERO));
    p.layoutSubtreeIfNeeded();
}

fn priority_tiers(mtm: MainThreadMarker) {
    // Any number of constraints give way to one a priority stronger, as
    // AppKit satisfies priorities strongest first.
    for (n, weak, strong) in [(3, 250.0, 251.0), (30, 250.0, 251.0), (3, 750.0, 751.0), (3, 249.99998, 250.0)] {
        let (w, cv) = window(mtm);
        let views: Vec<Retained<NSView>> = (0..n).map(|_| constrained(mtm)).collect();
        let mut cs = Vec::new();
        for (i, v) in views.iter().enumerate() {
            cv.addSubview(v);
            cs.push(v.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()));
            cs.push(v.topAnchor().constraintEqualToAnchor_constant(&cv.topAnchor(), 5.0 * i as f64));
            cs.push(v.heightAnchor().constraintEqualToConstant(5.0));
            let narrow = v.widthAnchor().constraintLessThanOrEqualToConstant(100.0);
            narrow.setPriority(weak);
            cs.push(narrow);
            if i > 0 {
                cs.push(v.widthAnchor().constraintEqualToAnchor(&views[0].widthAnchor()));
            }
        }
        let wide = views[0].widthAnchor().constraintEqualToConstant(300.0);
        wide.setPriority(strong);
        cs.push(wide);
        activate(&cs);
        w.layoutIfNeeded();
        assert_eq!(views[0].frame().size.width, 300.0, "{n} at {weak} against one at {strong}");
        w.setContentView(None);
    }
}

fn content_sizes_its_window(mtm: MainThreadMarker) {
    // The window holds its size at 500: constraints stronger than that,
    // and required ones, resize it; weaker ones give way.
    for (priority, width) in [(1000.0, 600.0), (750.0, 600.0), (510.0, 600.0), (499.0, 400.0)] {
        let (w, cv) = window(mtm);
        let wide = constrained(mtm);
        cv.addSubview(&wide);
        let min = wide.widthAnchor().constraintGreaterThanOrEqualToConstant(600.0);
        min.setPriority(priority);
        activate(&[
            wide.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
            wide.trailingAnchor().constraintEqualToAnchor(&cv.trailingAnchor()),
            wide.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
            wide.bottomAnchor().constraintEqualToAnchor(&cv.bottomAnchor()),
            min,
        ]);
        w.layoutIfNeeded();
        assert_eq!(w.contentLayoutRect().size, NSSize::new(width, 300.0), "at {priority}");
        assert_eq!((cv.frame(), wide.frame()), (rect(0.0, 0.0, width, 300.0), rect(0.0, 0.0, width, 300.0)));
        w.setContentView(None);
    }
    // Smaller too.
    let (w, cv) = window(mtm);
    let narrow = constrained(mtm);
    cv.addSubview(&narrow);
    activate(&[
        narrow.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        narrow.trailingAnchor().constraintEqualToAnchor(&cv.trailingAnchor()),
        narrow.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
        narrow.bottomAnchor().constraintEqualToAnchor(&cv.bottomAnchor()),
        narrow.widthAnchor().constraintLessThanOrEqualToConstant(300.0),
    ]);
    w.layoutIfNeeded();
    assert_eq!((w.contentLayoutRect().size, narrow.frame()), (NSSize::new(300.0, 300.0), rect(0.0, 0.0, 300.0, 300.0)));
    w.setContentView(None);
}

fn anchor_identity(mtm: MainThreadMarker) {
    let same = |a: &AnyObject, b: &AnyObject| std::ptr::eq(a, b);
    let v = view(mtm, rect(0.0, 0.0, 10.0, 10.0));
    // One object per attribute, the one constraints hand back.
    assert!(same(&v.topAnchor(), &v.topAnchor()));
    assert!(v.topAnchor().isEqual(Some(&v.topAnchor())));
    assert!(!v.topAnchor().isEqual(Some(&v.bottomAnchor())));
    let c = v.widthAnchor().constraintEqualToConstant(5.0);
    assert!(same(&c.firstAnchor(), &v.widthAnchor()));
    let x = view(mtm, NSRect::ZERO);
    let c = v.leadingAnchor().constraintEqualToAnchor(&x.trailingAnchor());
    assert!(same(&c.secondAnchor().unwrap(), &x.trailingAnchor()));
    // Distances are made each time.
    let d = || x.leadingAnchor().anchorWithOffsetToAnchor(&x.trailingAnchor());
    assert!(!same(&d(), &d()));
    let g = NSLayoutGuide::new();
    assert!(same(&g.topAnchor(), &g.topAnchor()));
}

fn system_spacing(mtm: MainThreadMarker) {
    let (w, cv) = window(mtm);
    let (a, b) = (constrained(mtm), constrained(mtm));
    cv.addSubview(&a);
    cv.addSubview(&b);
    activate(&[
        a.leadingAnchor().constraintEqualToAnchor(&cv.leadingAnchor()),
        a.topAnchor().constraintEqualToAnchor(&cv.topAnchor()),
        a.widthAnchor().constraintEqualToConstant(10.0),
        a.heightAnchor().constraintEqualToConstant(10.0),
        b.leadingAnchor().constraintEqualToSystemSpacingAfterAnchor_multiplier(&a.trailingAnchor(), 1.0),
        b.topAnchor().constraintEqualToSystemSpacingBelowAnchor_multiplier(&a.bottomAnchor(), 2.0),
        b.widthAnchor().constraintEqualToConstant(10.0),
        b.heightAnchor().constraintEqualToConstant(10.0),
    ]);
    w.layoutIfNeeded();
    // Eight points, times the multiplier.
    assert_eq!(b.frame(), rect(18.0, 264.0, 10.0, 10.0));
    w.setContentView(None);
}

/// Microseconds a run of `f` takes, the median of seven.
fn median_us(mut f: impl FnMut()) -> f64 {
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = std::time::Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

/// What layout costs, for comparing AppKit with Sidestep: `cargo test
/// --release -p sidestep-conformance --test appkit_autolayout -- timing`.
fn timing(mtm: MainThreadMarker) {
    const N: usize = 300;
    // A window of views without constraints, none of them touched.
    let (w, cv) = window(mtm);
    for i in 0..N {
        cv.addSubview(&view(mtm, rect(i as f64, 0.0, 10.0, 10.0)));
    }
    w.layoutIfNeeded();
    let idle = median_us(|| w.layoutIfNeeded());
    w.setContentView(None);

    // A row of views, each after the last: four constraints apiece.
    let solve = || {
        let (w, cv) = window(mtm);
        let views: Vec<Retained<NSView>> = (0..N).map(|_| constrained(mtm)).collect();
        let mut constraints = Vec::new();
        for (i, v) in views.iter().enumerate() {
            cv.addSubview(v);
            let after = match i {
                0 => cv.leadingAnchor(),
                _ => views[i - 1].trailingAnchor(),
            };
            constraints.push(v.leadingAnchor().constraintEqualToAnchor_constant(&after, 1.0));
            constraints.push(v.topAnchor().constraintEqualToAnchor_constant(&cv.topAnchor(), (i % 7) as f64));
            constraints.push(v.widthAnchor().constraintEqualToConstant(1.0 + (i % 3) as f64));
            constraints.push(v.heightAnchor().constraintEqualToConstant(10.0));
        }
        (w, cv, views, constraints)
    };
    let first = median_us(|| {
        let (w, _cv, _views, constraints) = solve();
        activate(&constraints);
        w.layoutIfNeeded();
        w.setContentView(None);
    });
    let (w, _cv, views, constraints) = solve();
    activate(&constraints);
    w.layoutIfNeeded();
    let mut k = 0.0;
    let change = median_us(|| {
        k += 1.0;
        constraints[0].setConstant(k);
        w.layoutIfNeeded();
    });
    assert_eq!(views[N - 1].frame().origin.x.fract(), 0.0);
    w.setContentView(None);

    // A window resized, holding 200 autoresizing views each with a view
    // pinned inside by four constraints.
    let (w, cv) = window(mtm);
    let mut kids = Vec::new();
    for i in 0..200 {
        let holder = view(mtm, rect(0.0, i as f64, 400.0, 1.0));
        holder.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        cv.addSubview(&holder);
        let kid = constrained(mtm);
        holder.addSubview(&kid);
        activate(&[
            kid.leadingAnchor().constraintEqualToAnchor_constant(&holder.leadingAnchor(), 2.0),
            kid.trailingAnchor().constraintEqualToAnchor_constant(&holder.trailingAnchor(), -2.0),
            kid.topAnchor().constraintEqualToAnchor(&holder.topAnchor()),
            kid.heightAnchor().constraintEqualToConstant(1.0),
        ]);
        kids.push(kid);
    }
    w.layoutIfNeeded();
    let mut wide = false;
    let resize = median_us(|| {
        wide = !wide;
        w.setContentSize(NSSize::new(if wide { 500.0 } else { 400.0 }, 300.0));
        w.layoutIfNeeded();
    });
    assert!([396.0, 496.0].contains(&kids[199].frame().size.width));
    w.setContentView(None);
    println!(
        "{N} views, no constraints, nothing to do: {idle:.1} µs; {} constraints, first solve: {first:.0} µs; \
         one constant changed: {change:.0} µs; window resized, 200 pinned views: {resize:.0} µs",
        4 * N
    );
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    if std::env::args().any(|a| a == "timing") {
        return objc2::rc::autoreleasepool(|_| timing(mtm));
    }
    let tests: &[Test] = &[
        ("defaults", defaults),
        ("anchors", anchors),
        ("installing", installing),
        ("solving_in_a_window", solving_in_a_window),
        ("solving_outside_a_window", solving_outside_a_window),
        ("translated_masks", translated_masks),
        ("intrinsic_sizes", intrinsic_sizes),
        ("fitting_sizes", fitting_sizes),
        ("layout_guides", layout_guides),
        ("ambiguity", ambiguity),
        ("system_spacing", system_spacing),
        ("moving_constrained_views", moving_constrained_views),
        ("constraints_outlive_their_views", constraints_outlive_their_views),
        ("priority_tiers", priority_tiers),
        ("content_sizes_its_window", content_sizes_its_window),
        ("anchor_identity", anchor_identity),
    ];
    #[cfg(not(target_vendor = "apple"))]
    let tests =
        &[tests, &[("activating_without_an_item", activating_without_an_item as fn(MainThreadMarker))]].concat();
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
