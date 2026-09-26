//! AppKit view geometry, checked on macOS and on Linux alike: frames and
//! bounds, converting between flipped and unflipped views, hit testing,
//! autoresizing and clip view scrolling. No window is needed.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSResponder, NSScrollView, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use sidestep as _;

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ConformanceFlippedView"]
    struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn pt(x: f64, y: f64) -> NSPoint {
    NSPoint::new(x, y)
}

fn view(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    NSView::initWithFrame(NSView::alloc(mtm), frame)
}

fn flipped(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    let this = Flipped::alloc(mtm).set_ivars(());
    let view: Retained<Flipped> = unsafe { msg_send![super(this), initWithFrame: frame] };
    Retained::into_super(view)
}

fn same(a: &NSView, b: &NSView) -> bool {
    std::ptr::eq(a, b)
}

fn frames_and_bounds(mtm: MainThreadMarker) {
    let v = view(mtm, rect(10.0, 20.0, 100.0, 50.0));
    assert_eq!(v.frame(), rect(10.0, 20.0, 100.0, 50.0));
    assert_eq!(v.bounds(), rect(0.0, 0.0, 100.0, 50.0));
    assert!(!v.isFlipped());
    v.setFrameSize(NSSize::new(200.0, 60.0));
    assert_eq!(v.bounds(), rect(0.0, 0.0, 200.0, 60.0));
    v.setBoundsOrigin(pt(5.0, 7.0));
    assert_eq!(v.bounds(), rect(5.0, 7.0, 200.0, 60.0));
    assert_eq!(v.frame(), rect(10.0, 20.0, 200.0, 60.0));
    assert!(flipped(mtm, rect(0.0, 0.0, 1.0, 1.0)).isFlipped());
}

fn converts_points(mtm: MainThreadMarker) {
    let root = view(mtm, rect(0.0, 0.0, 400.0, 300.0));
    let plain = view(mtm, rect(10.0, 20.0, 100.0, 50.0));
    let flip = flipped(mtm, rect(10.0, 20.0, 100.0, 50.0));
    let inner = view(mtm, rect(5.0, 5.0, 20.0, 20.0));
    root.addSubview(&plain);
    root.addSubview(&flip);
    flip.addSubview(&inner);
    assert!(same(&unsafe { plain.superview() }.unwrap(), &root));

    assert_eq!(plain.convertPoint_toView(pt(0.0, 0.0), Some(&root)), pt(10.0, 20.0));
    // A flipped view's origin is its top left corner.
    assert_eq!(flip.convertPoint_toView(pt(0.0, 0.0), Some(&root)), pt(10.0, 70.0));
    assert_eq!(flip.convertPoint_toView(pt(0.0, 10.0), Some(&root)), pt(10.0, 60.0));
    assert_eq!(flip.convertPoint_fromView(pt(10.0, 60.0), Some(&root)), pt(0.0, 10.0));
    // An unflipped view in a flipped one: its origin is its bottom left.
    assert_eq!(inner.convertPoint_toView(pt(0.0, 0.0), Some(&flip)), pt(5.0, 25.0));
    assert_eq!(inner.convertPoint_toView(pt(0.0, 0.0), Some(&root)), pt(15.0, 45.0));
    assert_eq!(root.convertPoint_fromView(pt(0.0, 0.0), Some(&inner)), pt(15.0, 45.0));
    // Between siblings.
    assert_eq!(plain.convertPoint_toView(pt(0.0, 0.0), Some(&flip)), pt(0.0, 50.0));
    assert_eq!(flip.convertRect_toView(rect(0.0, 0.0, 10.0, 10.0), Some(&root)), rect(10.0, 60.0, 10.0, 10.0));

    // The bounds origin shifts what a view shows.
    plain.setBoundsOrigin(pt(5.0, 5.0));
    assert_eq!(plain.convertPoint_toView(pt(5.0, 5.0), Some(&root)), pt(10.0, 20.0));
}

fn hit_tests(mtm: MainThreadMarker) {
    let root = view(mtm, rect(0.0, 0.0, 400.0, 300.0));
    let a = view(mtm, rect(10.0, 20.0, 100.0, 50.0));
    let b = flipped(mtm, rect(50.0, 40.0, 100.0, 50.0));
    root.addSubview(&a);
    root.addSubview(&b);
    let hit = |p: NSPoint| root.hitTest(p);
    assert!(same(&hit(pt(15.0, 25.0)).unwrap(), &a));
    // Where they overlap, the later sibling is on top.
    assert!(same(&hit(pt(60.0, 45.0)).unwrap(), &b));
    assert!(same(&hit(pt(300.0, 200.0)).unwrap(), &root));
    assert!(hit(pt(500.0, 500.0)).is_none());
    b.setHidden(true);
    assert!(same(&hit(pt(60.0, 45.0)).unwrap(), &a));
    b.removeFromSuperview();
    assert!(unsafe { b.superview() }.is_none());
    assert!(same(&hit(pt(120.0, 80.0)).unwrap(), &root));
}

fn autoresizes(mtm: MainThreadMarker) {
    let parent = view(mtm, rect(0.0, 0.0, 200.0, 100.0));
    let wide = view(mtm, rect(10.0, 10.0, 50.0, 20.0));
    let right = view(mtm, rect(10.0, 10.0, 50.0, 20.0));
    let fixed = view(mtm, rect(10.0, 10.0, 50.0, 20.0));
    let both = view(mtm, rect(10.0, 10.0, 50.0, 20.0));
    let spread = view(mtm, rect(10.0, 10.0, 50.0, 20.0));
    spread.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewMinXMargin
            | NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMaxXMargin,
    );
    wide.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    right.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMinYMargin);
    both.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    for v in [&wide, &right, &fixed, &both, &spread] {
        parent.addSubview(v);
    }
    parent.setFrameSize(NSSize::new(300.0, 150.0));
    assert_eq!(wide.frame(), rect(10.0, 10.0, 150.0, 20.0));
    assert_eq!(right.frame(), rect(110.0, 60.0, 50.0, 20.0));
    assert_eq!(fixed.frame(), rect(10.0, 10.0, 50.0, 20.0));
    assert_eq!(both.frame(), rect(10.0, 10.0, 150.0, 70.0));
    // Several flexible parts share the change in proportion to their sizes
    // (10, 50 and 140 of the old 200).
    assert_eq!(spread.frame(), rect(15.0, 10.0, 75.0, 20.0));
}

fn clip_view_scrolls(mtm: MainThreadMarker) {
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    let doc = flipped(mtm, rect(0.0, 0.0, 200.0, 1000.0));
    scroll.setDocumentView(Some(&doc));
    let clip = scroll.contentView();
    assert!(same(&scroll.documentView().unwrap(), &doc));
    assert!(same(&clip.documentView().unwrap(), &doc));
    assert!(clip.isFlipped(), "a clip view is flipped like its document");
    assert!(scroll.isFlipped());
    assert_eq!(clip.bounds(), rect(0.0, 0.0, 200.0, 100.0));

    clip.scrollToPoint(pt(0.0, 300.0));
    assert_eq!(clip.bounds().origin, pt(0.0, 300.0));
    assert_eq!(scroll.documentVisibleRect(), rect(0.0, 300.0, 200.0, 100.0));
    assert_eq!(doc.convertPoint_toView(pt(0.0, 300.0), Some(&clip)), pt(0.0, 300.0));
    assert_eq!(doc.convertPoint_toView(pt(0.0, 300.0), Some(&scroll)), pt(0.0, 0.0));

    // scrollToPoint: goes where it's told; constrainBoundsRect: keeps a
    // proposed position within the document.
    clip.scrollToPoint(pt(0.0, 5000.0));
    assert_eq!(clip.bounds().origin, pt(0.0, 5000.0));
    assert_eq!(clip.constrainBoundsRect(rect(0.0, 5000.0, 200.0, 100.0)), rect(0.0, 900.0, 200.0, 100.0));
    assert_eq!(clip.constrainBoundsRect(rect(0.0, -50.0, 200.0, 100.0)), rect(0.0, 0.0, 200.0, 100.0));
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: [Test; 5] = [
        ("frames_and_bounds", frames_and_bounds),
        ("converts_points", converts_points),
        ("hit_tests", hit_tests),
        ("autoresizes", autoresizes),
        ("clip_view_scrolls", clip_view_scrolls),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
