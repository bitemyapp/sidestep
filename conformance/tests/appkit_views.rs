//! Probe (temporary): what AppKit does for the NSView contract.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSResponder, NSScrollView, NSView, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_app_kit::NSUserInterfaceItemIdentification;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

fn log(s: String) {
    LOG.with(|l| l.borrow_mut().push(s));
}

fn take() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

struct Ivars {
    name: String,
    flipped: bool,
    super_will_draw: bool,
}

fn name_of(v: Option<&NSView>) -> String {
    match v {
        None => "nil".into(),
        Some(v) => {
            let id: Option<Retained<NSString>> = unsafe { msg_send![v, identifier] };
            id.map(|s| s.to_string()).unwrap_or("?".into())
        }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ProbeRecorder"]
    #[ivars = Ivars]
    struct Rec;

    impl Rec {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            self.ivars().flipped
        }
        #[unsafe(method(viewWillMoveToSuperview:))]
        fn will_super(&self, s: Option<&NSView>) {
            log(format!("{} willMoveToSuperview {}", self.ivars().name, name_of(s)));
        }
        #[unsafe(method(viewDidMoveToSuperview))]
        fn did_super(&self) {
            log(format!("{} didMoveToSuperview", self.ivars().name));
        }
        #[unsafe(method(viewWillMoveToWindow:))]
        fn will_window(&self, w: Option<&NSWindow>) {
            log(format!("{} willMoveToWindow {}", self.ivars().name, w.is_some()));
        }
        #[unsafe(method(viewDidMoveToWindow))]
        fn did_window(&self) {
            log(format!("{} didMoveToWindow", self.ivars().name));
        }
        #[unsafe(method(didAddSubview:))]
        fn did_add(&self, v: &NSView) {
            log(format!("{} didAddSubview {}", self.ivars().name, name_of(Some(v))));
        }
        #[unsafe(method(willRemoveSubview:))]
        fn will_remove(&self, v: &NSView) {
            log(format!("{} willRemoveSubview {}", self.ivars().name, name_of(Some(v))));
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
            unsafe { msg_send![super(self), layout] }
        }
        #[unsafe(method(viewWillDraw))]
        fn view_will_draw(&self) {
            log(format!("{} viewWillDraw", self.ivars().name));
            if self.ivars().super_will_draw {
                unsafe { msg_send![super(self), viewWillDraw] }
            }
        }
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, r: NSRect) {
            log(format!("{} drawRect {:?}", self.ivars().name, r));
        }
        #[unsafe(method(updateConstraints))]
        fn update_constraints(&self) {
            log(format!("{} updateConstraints", self.ivars().name));
            unsafe { msg_send![super(self), updateConstraints] }
        }
        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews(&self, s: NSSize) {
            log(format!("{} resizeSubviewsWithOldSize {:?}", self.ivars().name, s));
            unsafe { msg_send![super(self), resizeSubviewsWithOldSize: s] }
        }
        #[unsafe(method(resizeWithOldSuperviewSize:))]
        fn resize_with(&self, s: NSSize) {
            log(format!("{} resizeWithOldSuperviewSize {:?}", self.ivars().name, s));
            unsafe { msg_send![super(self), resizeWithOldSuperviewSize: s] }
        }
    }

    unsafe impl NSObjectProtocol for Rec {}
);

fn rec(mtm: MainThreadMarker, name: &str, frame: NSRect) -> Retained<NSView> {
    rec2(mtm, name, frame, false, true)
}

fn rec2(mtm: MainThreadMarker, name: &str, frame: NSRect, flipped: bool, sup: bool) -> Retained<NSView> {
    let this = Rec::alloc(mtm).set_ivars(Ivars { name: name.into(), flipped, super_will_draw: sup });
    let v: Retained<Rec> = unsafe { msg_send![super(this), initWithFrame: frame] };
    let v = Retained::into_super(v);
    let _: () = unsafe { msg_send![&*v, setIdentifier: &*NSString::from_str(name)] };
    v
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn names(v: &NSView) -> Vec<String> {
    v.subviews().iter().map(|s| name_of(Some(&s))).collect()
}

fn window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

extern "C-unwind" fn by_name_desc(
    a: std::ptr::NonNull<NSView>,
    b: std::ptr::NonNull<NSView>,
    _ctx: *mut std::ffi::c_void,
) -> objc2_foundation::NSComparisonResult {
    let (a, b) = unsafe { (name_of(Some(a.as_ref())), name_of(Some(b.as_ref()))) };
    match b.cmp(&a) {
        std::cmp::Ordering::Less => objc2_foundation::NSComparisonResult::Ascending,
        std::cmp::Ordering::Equal => objc2_foundation::NSComparisonResult::Same,
        std::cmp::Ordering::Greater => objc2_foundation::NSComparisonResult::Descending,
    }
}

fn window2(mtm: MainThreadMarker, defer: bool) -> Retained<NSWindow> {
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(100.0, 100.0, 400.0, 300.0),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            defer,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    w
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();
    objc2::rc::autoreleasepool(|_| {
        println!("--- windowless layout propagation");
        let lone = rec(mtm, "lone", rect(0.0, 0.0, 10.0, 10.0));
        let lone1 = rec(mtm, "lone1", rect(0.0, 0.0, 10.0, 10.0));
        lone.addSubview(&lone1);
        lone.layoutSubtreeIfNeeded();
        take();
        lone.layoutSubtreeIfNeeded();
        println!("second {:?}", take());
        lone1.setNeedsLayout(true);
        println!("after lone1 flag: lone {} lone1 {}", lone.needsLayout(), lone1.needsLayout());
        lone.layoutSubtreeIfNeeded();
        println!("{:?}", take());
        let x = rec(mtm, "x", rect(0.0, 0.0, 10.0, 10.0));
        lone.addSubview(&x);
        println!("after addSubview: lone {} x {}", lone.needsLayout(), x.needsLayout());
        lone.layoutSubtreeIfNeeded();
        take();
        x.removeFromSuperview();
        println!("after remove: lone {}", lone.needsLayout());
        lone.layoutSubtreeIfNeeded();
        take();
        lone1.setHidden(true);
        println!("after hide: lone {} lone1 {}", lone.needsLayout(), lone1.needsLayout());
        lone.layoutSubtreeIfNeeded();
        println!("hidden layout {:?}", take());
        lone1.setNeedsLayout(true);
        lone.layoutSubtreeIfNeeded();
        println!("hidden flagged layout {:?}", take());
        lone1.setHidden(false);
        lone.layoutSubtreeIfNeeded();
        take();
        lone1.setFrameSize(NSSize::new(30.0, 30.0));
        println!("after lone1 resize: lone {} lone1 {}", lone.needsLayout(), lone1.needsLayout());
        lone.layoutSubtreeIfNeeded();
        println!("{:?}", take());
        lone.setFrameSize(NSSize::new(30.0, 30.0));
        println!("after lone resize: lone {} lone1 {}", lone.needsLayout(), lone1.needsLayout());
        lone.layoutSubtreeIfNeeded();
        println!("{:?}", take());
        lone.setNeedsLayout(false);
        println!("setNeedsLayout NO -> {}", lone.needsLayout());
        println!("needsUpdateConstraints lone {}", lone.needsUpdateConstraints());

        println!("--- hidden ancestor + hiding descendants");
        let h = rec(mtm, "h", rect(0.0, 0.0, 10.0, 10.0));
        let h1 = rec(mtm, "h1", rect(0.0, 0.0, 10.0, 10.0));
        h.addSubview(&h1);
        h.setHidden(true);
        take();
        h1.setHidden(true);
        println!("hide h1 under hidden h {:?} h1.isHiddenOrHas {}", take(), h1.isHiddenOrHasHiddenAncestor());
        h1.setHidden(false);
        println!("unhide h1 under hidden h {:?}", take());
        let h2 = rec(mtm, "h2", rect(0.0, 0.0, 10.0, 10.0));
        let g = rec(mtm, "g", rect(0.0, 0.0, 10.0, 10.0));
        g.setHidden(true);
        take();
        h2.removeFromSuperview();
        g.addSubview(&h2);
        take();
        h.addSubview(&h2);
        println!("move from hidden g to hidden h {:?}", take());
        println!("plain policy {:?} subviews {}", NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO).layerContentsRedrawPolicy(), NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO).subviews().count());
        h1.removeFromSuperview();
        take();
        h1.removeFromSuperview();
        println!("remove w/o superview {:?}", take());

        println!("--- replace edge cases");
        let r = rec(mtm, "r", rect(0.0, 0.0, 10.0, 10.0));
        let r1 = rec(mtm, "r1", rect(0.0, 0.0, 10.0, 10.0));
        let r2 = rec(mtm, "r2", rect(0.0, 0.0, 10.0, 10.0));
        let r3 = rec(mtm, "r3", rect(0.0, 0.0, 10.0, 10.0));
        let other = rec(mtm, "other", rect(0.0, 0.0, 10.0, 10.0));
        r.addSubview(&r1);
        r.addSubview(&r2);
        other.addSubview(&r3);
        take();
        r.replaceSubview_with(&r1, &r3);
        println!("replace r1 with r3 from other {:?} {:?} other {:?}", take(), names(&r), names(&other));
        r.replaceSubview_with(&r3, &r2);
        println!("replace r3 with sibling r2 {:?} {:?}", take(), names(&r));

        println!("--- scrollRectToVisible unflipped");
        let sv = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
        let doc = rec2(mtm, "doc", rect(0.0, 0.0, 400.0, 1000.0), false, true);
        sv.setDocumentView(Some(&doc));
        let clip = sv.contentView();
        println!("unflipped initial {:?}", clip.bounds());
        let ok = doc.scrollRectToVisible(rect(0.0, 500.0, 10.0, 10.0));
        println!("unflipped (0,500) -> {ok} {:?}", clip.bounds());
        let ok = doc.scrollRectToVisible(rect(0.0, 100.0, 10.0, 10.0));
        println!("unflipped (0,100) -> {ok} {:?}", clip.bounds());
        let ok = doc.scrollRectToVisible(rect(0.0, 300.0, 10.0, 300.0));
        println!("unflipped taller below? -> {ok} {:?}", clip.bounds());
        clip.scrollToPoint(NSPoint::new(0.0, 0.0));
        let ok = doc.scrollRectToVisible(rect(0.0, 300.0, 10.0, 300.0));
        println!("unflipped taller above -> {ok} {:?}", clip.bounds());
        clip.scrollToPoint(NSPoint::new(0.0, 900.0));
        let ok = doc.scrollRectToVisible(rect(0.0, 300.0, 10.0, 300.0));
        println!("unflipped taller from top -> {ok} {:?}", clip.bounds());
        let docf = rec2(mtm, "docf", rect(0.0, 0.0, 400.0, 1000.0), true, true);
        sv.setDocumentView(Some(&docf));
        clip.scrollToPoint(NSPoint::new(0.0, 900.0));
        let ok = docf.scrollRectToVisible(rect(0.0, 300.0, 10.0, 300.0));
        println!("flipped taller from bottom -> {ok} {:?}", clip.bounds());
        let ok = docf.scrollRectToVisible(rect(-50.0, 300.0, 500.0, 50.0));
        println!("flipped wider -> {ok} {:?}", clip.bounds());
        clip.scrollToPoint(NSPoint::new(100.0, 300.0));
        let ok = docf.scrollRectToVisible(rect(-50.0, 300.0, 500.0, 50.0));
        println!("flipped wider from right -> {ok} {:?}", clip.bounds());
        let inner = rec2(mtm, "inner", rect(50.0, 400.0, 20.0, 20.0), false, true);
        docf.addSubview(&inner);
        clip.scrollToPoint(NSPoint::new(0.0, 0.0));
        let ok = inner.scrollRectToVisible(rect(0.0, 0.0, 20.0, 20.0));
        println!("subview of doc -> {ok} {:?}", clip.bounds());
        clip.scrollToPoint(NSPoint::new(0.0, 0.0));
        inner.scrollPoint(NSPoint::new(0.0, 20.0));
        println!("scrollPoint in unflipped subview -> {:?}", clip.bounds());
        let ok = clip.scrollRectToVisible(rect(0.0, 700.0, 10.0, 10.0));
        println!("clip scrollRectToVisible -> {ok} {:?}", clip.bounds());
        clip.scrollPoint(NSPoint::new(0.0, 100.0));
        println!("clip scrollPoint -> {:?}", clip.bounds());
        sv.scrollPoint(NSPoint::new(0.0, 5.0));
        println!("sv scrollPoint -> {:?}", clip.bounds());

        println!("--- display with non-deferred window");
        let w = window2(mtm, false);
        let cv = rec(mtm, "cv", rect(0.0, 0.0, 10.0, 10.0));
        let cv1 = rec(mtm, "cv1", rect(10.0, 10.0, 50.0, 50.0));
        let nosup = rec2(mtm, "nosup", rect(100.0, 100.0, 50.0, 50.0), false, false);
        let nosup1 = rec(mtm, "nosup1", rect(0.0, 0.0, 10.0, 10.0));
        let hid = rec(mtm, "hid", rect(0.0, 0.0, 10.0, 10.0));
        hid.setHidden(true);
        cv.addSubview(&cv1);
        cv.addSubview(&nosup);
        cv.addSubview(&hid);
        nosup.addSubview(&nosup1);
        w.setContentView(Some(&cv));
        take();
        cv.display();
        println!("cv.display {:?}", take());
        cv1.setNeedsDisplay(true);
        w.displayIfNeeded();
        println!("displayIfNeeded cv1 {:?}", take());
        cv1.setNeedsLayout(true);
        w.displayIfNeeded();
        println!("displayIfNeeded after layout flag {:?}", take());
        cv1.setNeedsDisplay(true);
        cv1.setNeedsLayout(true);
        w.displayIfNeeded();
        println!("displayIfNeeded with both {:?}", take());
        cv1.displayRect(rect(0.0, 0.0, 5.0, 5.0));
        println!("cv1.displayRect {:?}", take());
        w.display();
        println!("w.display {:?}", take());
        println!("scale {}", w.backingScaleFactor());
        let r = rect(0.3, 0.7, 10.4, 10.6);
        println!("centerScan in window {:?}", cv1.centerScanRect(r));
        println!("visibleRect cv1 {:?} nosup1 {:?} hid {:?}", cv1.visibleRect(), nosup1.visibleRect(), hid.visibleRect());
        println!("preparedContentRect cv1 {:?}", cv1.preparedContentRect());
        w.setContentView(None);
    });
}
