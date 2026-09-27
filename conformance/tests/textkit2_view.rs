//! NSTextView and TextKit 2: which mode a text view starts in, the switch
//! to TextKit 1 that asking for its layout manager makes, its viewport
//! (bounds, layout, the fragments it draws), sizing to the text, the text
//! layout manager's selections following the view's, and editing.
//! Expected values are what macOS does. No window is shown.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

mod common;

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSClipView, NSFont, NSResponder, NSScrollView, NSSecureTextField, NSText, NSTextContainer,
    NSTextContentManager, NSTextContentStorage, NSTextElement, NSTextElementProvider, NSTextField, NSTextInputClient,
    NSTextLayoutFragment, NSTextLayoutManager, NSTextLayoutManagerDelegate, NSTextLocation, NSTextStorageObserving,
    NSTextView, NSTextViewportLayoutController, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_graphics::CGContext;
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString};

use sidestep as _;

type Test = (&'static str, fn(MainThreadMarker));

const NOT_FOUND: usize = isize::MAX as usize;

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn same<A: ?Sized, B: ?Sized>(a: &A, b: &B) -> bool {
    std::ptr::eq(a as *const A as *const u8, b as *const B as *const u8)
}

fn name(o: &AnyObject) -> String {
    o.class().name().to_str().unwrap_or("").to_owned()
}

fn tlm_of(tv: &NSTextView) -> Option<Retained<NSTextLayoutManager>> {
    tv.textLayoutManager()
}

fn cs_of(tv: &NSTextView) -> Option<Retained<NSTextContentStorage>> {
    tv.textContentStorage()
}

fn vp_of(tlm: &NSTextLayoutManager) -> Retained<NSTextViewportLayoutController> {
    tlm.textViewportLayoutController()
}

/// A text view of the text layout manager's own.
fn network(width: f64) -> (Retained<NSTextContentStorage>, Retained<NSTextLayoutManager>, Retained<NSTextContainer>) {
    let cs = NSTextContentStorage::new();
    let tlm = NSTextLayoutManager::new();
    let c = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(width, 1.0e7));
    tlm.setTextContainer(Some(&c));
    cs.addTextLayoutManager(&tlm);
    (cs, tlm, c)
}

define_class!(
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextKit2ViewDrawsItself"]
    struct DrawsItself;

    impl DrawsItself {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, r: NSRect) {
            let _: () = unsafe { msg_send![super(self), drawRect: r] };
        }
    }
);

define_class!(
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TextKit2ViewLogging"]
    #[ivars = RefCell<Vec<String>>]
    struct Logging;

    impl Logging {
        #[unsafe(method(viewportBoundsForTextViewportLayoutController:))]
        fn bounds(&self, c: &NSTextViewportLayoutController) -> NSRect {
            let r: NSRect = unsafe { msg_send![super(self), viewportBoundsForTextViewportLayoutController: c] };
            self.ivars().borrow_mut().push("bounds".into());
            r
        }

        #[unsafe(method(textViewportLayoutControllerWillLayout:))]
        fn will(&self, c: &NSTextViewportLayoutController) {
            self.ivars().borrow_mut().push("will".into());
            let _: () = unsafe { msg_send![super(self), textViewportLayoutControllerWillLayout: c] };
        }

        #[unsafe(method(textViewportLayoutController:configureRenderingSurfaceForTextLayoutFragment:))]
        fn configure(&self, c: &NSTextViewportLayoutController, f: &NSTextLayoutFragment) {
            let frame = f.layoutFragmentFrame();
            let line = format!("configure {} {} {}", f.state().0, frame.origin.y, frame.size.height);
            self.ivars().borrow_mut().push(line);
            let _: () = unsafe {
                msg_send![super(self), textViewportLayoutController: c, configureRenderingSurfaceForTextLayoutFragment: f]
            };
        }

        #[unsafe(method(textViewportLayoutControllerDidLayout:))]
        fn did(&self, c: &NSTextViewportLayoutController) {
            self.ivars().borrow_mut().push("did".into());
            let _: () = unsafe { msg_send![super(self), textViewportLayoutControllerDidLayout: c] };
        }
    }
);

fn logging(mtm: MainThreadMarker, frame: NSRect) -> Retained<Logging> {
    let this = Logging::alloc(mtm).set_ivars(RefCell::new(Vec::new()));
    unsafe { msg_send![super(this), initWithFrame: frame] }
}

thread_local! {
    /// The fragments drawn (where each starts) and the points they were
    /// drawn at.
    static DRAWN: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    static POINTS: RefCell<Vec<NSPoint>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    #[unsafe(super(NSTextLayoutFragment))]
    #[name = "TextKit2ViewRecordingFragment"]
    struct Recording;

    impl Recording {
        #[unsafe(method(drawAtPoint:inContext:))]
        fn draw(&self, p: NSPoint, cg: &CGContext) {
            let cm = self.textLayoutManager().and_then(|m| m.textContentManager()).expect("a content manager");
            let start = cm.documentRange().location();
            let at = cm.offsetFromLocation_toLocation(&start, &self.rangeInElement().location());
            DRAWN.with(|d| d.borrow_mut().push(at));
            POINTS.with(|d| d.borrow_mut().push(p));
            let _: () = unsafe { msg_send![super(self), drawAtPoint: p, inContext: cg] };
        }
    }
);

define_class!(
    /// A fragment drawing, as it says, far below its frame.
    #[unsafe(super(NSTextLayoutFragment))]
    #[name = "TextKit2ViewTallSurface"]
    struct TallSurface;

    impl TallSurface {
        #[unsafe(method(renderingSurfaceBounds))]
        fn surface(&self) -> NSRect {
            let mut r: NSRect = unsafe { msg_send![super(self), renderingSurfaceBounds] };
            r.size.height += 200.0;
            r
        }

        #[unsafe(method(drawAtPoint:inContext:))]
        fn draw(&self, p: NSPoint, cg: &CGContext) {
            DRAWN.with(|d| d.borrow_mut().push(-1));
            let _: () = unsafe { msg_send![super(self), drawAtPoint: p, inContext: cg] };
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2ViewTallSurfaces"]
    struct TallSurfaces;

    unsafe impl NSObjectProtocol for TallSurfaces {}
    unsafe impl NSTextLayoutManagerDelegate for TallSurfaces {
        #[unsafe(method_id(textLayoutManager:textLayoutFragmentForLocation:inTextElement:))]
        fn fragment(
            &self,
            _tlm: &NSTextLayoutManager,
            _location: &ProtocolObject<dyn NSTextLocation>,
            element: &NSTextElement,
        ) -> Retained<NSTextLayoutFragment> {
            let range = element.elementRange();
            let this = TallSurface::alloc().set_ivars(());
            let f: Retained<TallSurface> =
                unsafe { msg_send![super(this), initWithTextElement: element, range: range.as_deref()] };
            Retained::into_super(f)
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TextKit2ViewFragments"]
    struct RecordingFragments;

    unsafe impl NSObjectProtocol for RecordingFragments {}
    unsafe impl NSTextLayoutManagerDelegate for RecordingFragments {
        #[unsafe(method_id(textLayoutManager:textLayoutFragmentForLocation:inTextElement:))]
        fn fragment(
            &self,
            _tlm: &NSTextLayoutManager,
            _location: &ProtocolObject<dyn NSTextLocation>,
            element: &NSTextElement,
        ) -> Retained<NSTextLayoutFragment> {
            let range = element.elementRange();
            let this = Recording::alloc().set_ivars(());
            let f: Retained<Recording> =
                unsafe { msg_send![super(this), initWithTextElement: element, range: range.as_deref()] };
            Retained::into_super(f)
        }
    }
);

fn test_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let frame = rect(100.0, 100.0, 300.0, 200.0);
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { w.setReleasedWhenClosed(false) };
    let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, frame.size));
    w.setContentView(Some(&content));
    w
}

/// Which mode a text view starts in.
fn modes(mtm: MainThreadMarker) {
    let frame = rect(0.0, 0.0, 200.0, 100.0);
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), frame);
    let tlm = tlm_of(&tv).expect("TextKit 2");
    let cs = cs_of(&tv).expect("a content storage");
    let c = unsafe { tv.textContainer() }.expect("a container");
    assert!(c.textLayoutManager().is_some_and(|m| same(&*m, &*tlm)));
    assert!(tlm.textContainer().is_some_and(|x| same(&*x, &*c)));
    assert!(tlm.textContentManager().is_some_and(|m| same(&*m, &*cs)));
    assert!(unsafe { tv.textStorage() }.is_some_and(|s| cs.textStorage().is_some_and(|t| same(&*s, &*t))));
    assert!(vp_of(&tlm).delegate().is_some_and(|d| same(&*d, &*tv)));
    assert!(tlm.delegate().is_none());
    assert_eq!(c.size().width, 200.0);

    // A subclass drawing itself starts in TextKit 1, unless given a TextKit
    // 2 container.
    let d: Retained<DrawsItself> =
        unsafe { msg_send![super(DrawsItself::alloc(mtm).set_ivars(())), initWithFrame: frame] };
    assert!(tlm_of(&d).is_none());
    let (cs2, tlm2, c2) = network(200.0);
    let d: Retained<DrawsItself> =
        unsafe { msg_send![super(DrawsItself::alloc(mtm).set_ivars(())), initWithFrame: frame, textContainer: &*c2] };
    assert!(tlm_of(&d).is_some_and(|m| same(&*m, &*tlm2)));
    assert!(cs_of(&d).is_some_and(|m| same(&*m, &*cs2)));
    assert!(vp_of(&tlm2).delegate().is_some_and(|x| same(&*x, &*d)));
    assert!(c2.textView(mtm).is_some_and(|v| same(&*v, &*d)));

    assert!(tlm_of(&NSTextView::initUsingTextLayoutManager(NSTextView::alloc(mtm), false)).is_none());
    let on = NSTextView::initUsingTextLayoutManager(NSTextView::alloc(mtm), true);
    assert!(tlm_of(&on).is_some());
    assert_eq!(on.frame(), NSRect::ZERO);
    assert!(on.isVerticallyResizable());
    assert!(tlm_of(&NSTextView::textViewUsingTextLayoutManager(true, mtm)).is_some());
    assert!(tlm_of(&NSTextView::textViewUsingTextLayoutManager(false, mtm)).is_none());
    assert!(tlm_of(&NSTextView::new(mtm)).is_some());
    let scroll = NSTextView::scrollableTextView(mtm);
    let doc: Retained<NSTextView> = scroll.documentView().and_then(|v| v.downcast().ok()).expect("a text view");
    assert!(tlm_of(&doc).is_some());
    assert!(tlm_of(&NSTextView::fieldEditor(mtm)).is_some());

    // Given a text layout manager with no content manager: TextKit 2, with
    // nothing to show.
    let bare = NSTextLayoutManager::new();
    let c3 = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(100.0, 100.0));
    bare.setTextContainer(Some(&c3));
    let tv3 = NSTextView::initWithFrame_textContainer(NSTextView::alloc(mtm), frame, Some(&c3));
    assert!(tlm_of(&tv3).is_some() && cs_of(&tv3).is_none() && unsafe { tv3.textStorage() }.is_none());
    // A container of no layout manager at all: neither.
    let c4 = NSTextContainer::initWithSize(NSTextContainer::alloc(), NSSize::new(100.0, 100.0));
    let tv4 = NSTextView::initWithFrame_textContainer(NSTextView::alloc(mtm), frame, Some(&c4));
    assert!(tlm_of(&tv4).is_none() && unsafe { tv4.textStorage() }.is_none());

    // A window's field editor is TextKit 2, a secure field's TextKit 1.
    let w = test_window(mtm);
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(0.0, 0.0, 80.0, 22.0));
    w.contentView().expect("content").addSubview(&field);
    let editor: Retained<NSTextView> =
        unsafe { w.fieldEditor_forObject(true, Some(&field)) }.and_then(|e| e.downcast().ok()).expect("an editor");
    assert!(tlm_of(&editor).is_some());
    let secure = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), rect(0.0, 30.0, 80.0, 22.0));
    w.contentView().expect("content").addSubview(&secure);
    let editor: Retained<NSTextView> =
        unsafe { w.fieldEditor_forObject(true, Some(&secure)) }.and_then(|e| e.downcast().ok()).expect("an editor");
    assert!(tlm_of(&editor).is_none());
    w.close();
}

/// Asking a TextKit 2 view for its layout manager makes it TextKit 1.
fn switch_to_text_kit_1(mtm: MainThreadMarker) {
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    tv.setString(&NSString::from_str("hello\nworld"));
    let tlm = tlm_of(&tv).expect("TextKit 2");
    let storage = unsafe { tv.textStorage() }.expect("a storage");
    let c = unsafe { tv.textContainer() }.expect("a container");
    let lm = unsafe { tv.layoutManager() }.expect("a layout manager");
    let lm_obj: &AnyObject = &lm;
    assert_eq!(name(lm_obj), "NSLayoutManager");
    assert!(tlm_of(&tv).is_none() && cs_of(&tv).is_none());
    let c_now = unsafe { tv.textContainer() }.expect("a container");
    assert!(same(&*c_now, &*c));
    assert!(c.textLayoutManager().is_none());
    assert!(unsafe { c.layoutManager() }.is_some_and(|m| same(&*m, &*lm)));
    assert!(unsafe { tv.textStorage() }.is_some_and(|s| same(&*s, &*storage)));
    assert_eq!(tv.string().to_string(), "hello\nworld");
    // The text layout manager keeps its container and content.
    assert!(tlm.textContainer().is_some_and(|x| same(&*x, &*c)));
    assert!(tlm.textContentManager().is_some());
}

/// The viewport's bounds: what shows of the view, across its whole width,
/// in container coordinates; everything below its top when nothing clips
/// it.
fn viewport_bounds(mtm: MainThreadMarker) {
    let frame = rect(0.0, 0.0, 200.0, 100.0);
    let bounds = |tv: &NSTextView| -> NSRect {
        let tlm = tlm_of(tv).expect("TextKit 2");
        unsafe { msg_send![tv, viewportBoundsForTextViewportLayoutController: &*vp_of(&tlm)] }
    };
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), frame);
    tv.setTextContainerInset(NSSize::new(10.0, 20.0));
    let b = bounds(&tv);
    assert_eq!((b.origin.x, b.origin.y, b.size.width), (-10.0, -20.0, 200.0));
    assert!(b.size.height > 1.0e300, "{b:?}");
    let clip = NSClipView::initWithFrame(NSClipView::alloc(mtm), rect(0.0, 0.0, 150.0, 50.0));
    let tv2 = NSTextView::initWithFrame(NSTextView::alloc(mtm), frame);
    clip.setDocumentView(Some(&tv2));
    assert_eq!(bounds(&tv2), rect(0.0, 0.0, 200.0, 50.0));
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    let tv3 = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 600.0));
    scroll.setDocumentView(Some(&tv3));
    let visible = scroll.contentView().bounds();
    assert_eq!(bounds(&tv3), rect(0.0, 0.0, 200.0, visible.size.height));
    scroll.contentView().scrollToPoint(NSPoint::new(0.0, 200.0));
    scroll.reflectScrolledClipView(&scroll.contentView());
    tv3.setTextContainerInset(NSSize::new(10.0, 20.0));
    assert_eq!(bounds(&tv3), rect(-10.0, 180.0, 200.0, visible.size.height));
}

fn lines(n: usize) -> String {
    (0..n).map(|i| format!("line number {i}\n")).collect()
}

/// Laying the viewport out: the view is the delegate; the fragments that
/// show are configured, laid out; the view sizes to the text as estimated.
fn viewport_layout(mtm: MainThreadMarker) {
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    let tv = logging(mtm, rect(0.0, 0.0, 200.0, 100.0));
    tv.setVerticallyResizable(true);
    scroll.setDocumentView(Some(&tv));
    let tlm = tlm_of(&tv).expect("TextKit 2");
    tv.setString(&NSString::from_str(&lines(2000)));
    // Setting the text lays nothing out and leaves the view alone.
    assert!(!tv.ivars().borrow().iter().any(|s| s == "will"));
    assert_eq!(tv.frame().size.height, 100.0);
    assert_eq!(tlm.usageBoundsForTextContainer(), NSRect::ZERO);
    tv.ivars().borrow_mut().clear();
    vp_of(&tlm).layoutViewport();
    let log = tv.ivars().borrow().clone();
    let will = log.iter().position(|s| s == "will").expect("will");
    let did = log.iter().position(|s| s == "did").expect("did");
    assert!(will < did);
    assert!(log[will..did].iter().any(|s| s == "bounds"), "{log:?}");
    let configured: Vec<(f64, f64)> = log
        .iter()
        .filter_map(|s| s.strip_prefix("configure "))
        .map(|s| {
            let v: Vec<&str> = s.split(' ').collect();
            assert_eq!(v[0], "3", "laid out");
            (v[1].parse().expect("y"), v[2].parse().expect("height"))
        })
        .collect();
    assert!(configured.len() >= 5 && configured.len() < 20, "{configured:?}");
    // They cover what shows, and no more than a fragment past it.
    assert!(configured[0].0 <= 0.0);
    let bottom = configured.last().map(|(y, h)| y + h).expect("some");
    assert!(bottom >= 100.0 && configured.last().expect("some").0 < 100.0, "{configured:?}");
    // The view grows to the text as estimated.
    let usage = tlm.usageBoundsForTextContainer();
    assert!(usage.origin.y + usage.size.height > 1000.0, "{usage:?}");
    assert!(tv.frame().size.height >= usage.origin.y + usage.size.height - 1.0, "{:?}", tv.frame());
    assert_eq!(
        vp_of(&tlm).viewportRange().map(|r| {
            let cm = tlm.textContentManager().expect("content");
            cm.offsetFromLocation_toLocation(&cm.documentRange().location(), &r.location())
        }),
        Some(0)
    );
}

/// A TextKit 2 view draws through its fragments' `drawAtPoint:inContext:`,
/// at the point zero: those that show, and the text is there.
fn drawing(mtm: MainThreadMarker) {
    let (cs, tlm, c) = network(180.0);
    let delegate: Retained<RecordingFragments> =
        unsafe { msg_send![super(RecordingFragments::alloc().set_ivars(())), init] };
    tlm.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    cs.textStorage()
        .expect("a storage")
        .setAttributedString(&objc2_foundation::NSAttributedString::from_nsstring(&NSString::from_str(&lines(50))));
    let tv = NSTextView::initWithFrame_textContainer(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 60.0), Some(&c));
    tv.setTextContainerInset(NSSize::new(10.0, 5.0));
    tv.setBackgroundColor(&objc2_app_kit::NSColor::whiteColor());
    tv.setTextColor(Some(&objc2_app_kit::NSColor::blackColor()));
    tv.setFont(Some(&NSFont::systemFontOfSize(14.0)));
    let clip = NSClipView::initWithFrame(NSClipView::alloc(mtm), rect(0.0, 0.0, 200.0, 60.0));
    clip.setDocumentView(Some(&tv));
    DRAWN.with(|d| d.borrow_mut().clear());
    POINTS.with(|d| d.borrow_mut().clear());
    let rep = common::snapshot(&tv, 1.0);
    let drawn = DRAWN.with(|d| d.borrow().clone());
    assert!(drawn.contains(&0), "the first fragment drew: {drawn:?}");
    assert!(drawn.iter().all(|&o| o < 400), "only what shows: {drawn:?}");
    let points = POINTS.with(|d| d.borrow().clone());
    assert!(points.iter().all(|&p| p == NSPoint::ZERO), "{points:?}");
    // Dark pixels where the first line's text is.
    let dark = (0..40)
        .flat_map(|x| (5..25).map(move |y| (x + 15, y)))
        .filter(|&(x, y)| {
            let p = common::pixel(&rep, x, y);
            p[3] > 0 && p[0] < 128 && p[1] < 128 && p[2] < 128
        })
        .count();
    assert!(dark > 5, "text drawn: {dark} dark pixels");
}

/// A fragment whose rendering surface reaches below its frame draws for
/// a rect that meets only that part of it.
fn rendering_surface(mtm: MainThreadMarker) {
    let (cs, tlm, c) = network(180.0);
    let delegate: Retained<TallSurfaces> = unsafe { msg_send![super(TallSurfaces::alloc().set_ivars(())), init] };
    tlm.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    cs.textStorage()
        .expect("a storage")
        .setAttributedString(&objc2_foundation::NSAttributedString::from_nsstring(&NSString::from_str("ab")));
    let tv = NSTextView::initWithFrame_textContainer(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 300.0), Some(&c));
    DRAWN.with(|d| d.borrow_mut().clear());
    let rep = common::bitmap(200, 20);
    rep.setSize(NSSize::new(200.0, 20.0));
    tv.cacheDisplayInRect_toBitmapImageRep(rect(0.0, 150.0, 200.0, 20.0), &rep);
    assert!(DRAWN.with(|d| d.borrow().contains(&-1)), "the fragment drew");
}

/// The viewport of a view scrolled in a clip view taller than what shows
/// of it: the clip view's height, down past the view's bottom.
fn viewport_past_the_view(mtm: MainThreadMarker) {
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 60.0));
    tv.setTextContainerInset(NSSize::new(10.0, 5.0));
    let clip = NSClipView::initWithFrame(NSClipView::alloc(mtm), rect(0.0, 0.0, 200.0, 60.0));
    clip.setDocumentView(Some(&tv));
    clip.scrollToPoint(NSPoint::new(0.0, 40.0));
    let tlm = tlm_of(&tv).expect("TextKit 2");
    let b: NSRect = unsafe { msg_send![&*tv, viewportBoundsForTextViewportLayoutController: &*vp_of(&tlm)] };
    assert_eq!(b, rect(-10.0, 35.0, 200.0, 60.0));
}

/// `sizeToFit` lays the text out first: a short text's height is its
/// fragments', an empty one's its extra line's, which is a caret's; a long
/// text's is estimated past what is laid out.
fn size_to_fit(mtm: MainThreadMarker) {
    let sized = |text: &str| {
        let tv = NSTextView::initUsingTextLayoutManager(NSTextView::alloc(mtm), true);
        tv.setFrame(rect(0.0, 0.0, 200.0, 10.0));
        tv.setMinSize(NSSize::ZERO);
        tv.setVerticallyResizable(true);
        tv.setFont(Some(&NSFont::systemFontOfSize(20.0)));
        tv.setString(&NSString::from_str(text));
        tv.sizeToFit();
        tv
    };
    let tv = sized("x");
    let tlm = tlm_of(&tv).expect("TextKit 2");
    let cm = tlm.textContentManager().expect("content");
    let f = tlm.textLayoutFragmentForLocation(&cm.documentRange().location()).expect("a fragment");
    assert_eq!(f.state().0, 3);
    let h = f.layoutFragmentFrame().size.height;
    // Within a point: a view's height may be rounded up to whole points.
    let about = |a: f64, b: f64| (a - b).abs() < 1.0;
    assert!(h > 20.0 && about(tv.frame().size.height, h), "{:?} {h}", tv.frame());
    assert!(about(sized("a\nb\nc").frame().size.height, 3.0 * h));
    assert!(about(sized("").frame().size.height, h));
    let long = sized(&lines(3000));
    assert!(long.frame().size.height > 3000.0 * h * 0.5, "{:?}", long.frame());
}

/// Asking for the layout manager switches the view, between the two
/// notifications AppKit posts.
fn switch_notifications(mtm: MainThreadMarker) {
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    let seen = std::rc::Rc::new(RefCell::new(Vec::<(String, bool)>::new()));
    let center = objc2_foundation::NSNotificationCenter::defaultCenter();
    let mut tokens = Vec::new();
    for name in unsafe {
        [
            objc2_app_kit::NSTextViewWillSwitchToNSLayoutManagerNotification,
            objc2_app_kit::NSTextViewDidSwitchToNSLayoutManagerNotification,
        ]
    } {
        let seen = seen.clone();
        let block = block2::RcBlock::new(move |n: std::ptr::NonNull<objc2_foundation::NSNotification>| {
            let n = unsafe { n.as_ref() };
            let still = n
                .object()
                .and_then(|o| o.downcast::<NSTextView>().ok())
                .is_some_and(|v| v.textLayoutManager().is_some());
            seen.borrow_mut().push((n.name().to_string(), still));
        });
        tokens.push(unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), Some(&tv), None, &block) });
    }
    let _ = unsafe { tv.layoutManager() };
    for t in tokens {
        let o: &AnyObject = unsafe { &*(Retained::as_ptr(&t) as *const AnyObject) };
        unsafe { center.removeObserver(o) };
    }
    assert_eq!(
        *seen.borrow(),
        [
            ("NSTextViewWillSwitchToNSLayoutManagerNotification".to_owned(), true),
            ("NSTextViewDidSwitchToNSLayoutManagerNotification".to_owned(), false)
        ]
    );
}

/// An empty field's editor puts its caret on a line of the field's font,
/// as tall as after typing.
fn empty_field_editor(mtm: MainThreadMarker) {
    let w = test_window(mtm);
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(10.0, 10.0, 200.0, 50.0));
    field.setFont(Some(&NSFont::systemFontOfSize(30.0)));
    w.contentView().expect("content").addSubview(&field);
    w.makeFirstResponder(Some(&field));
    let editor: Retained<NSTextView> = field.currentEditor().and_then(|e| e.downcast().ok()).expect("an editor");
    assert!(tlm_of(&editor).is_some());
    let mut actual = NSRange::new(0, 0);
    let empty = unsafe { editor.firstRectForCharacterRange_actualRange(NSRange::new(0, 0), &mut actual) };
    unsafe { editor.insertText_replacementRange(&NSString::from_str("ab"), NSRange::new(NOT_FOUND, 0)) };
    let typed = unsafe { editor.firstRectForCharacterRange_actualRange(NSRange::new(2, 0), &mut actual) };
    assert!(empty.size.height > 30.0 && empty.size.height == typed.size.height, "{empty:?} {typed:?}");
    w.close();
}

/// The text layout manager's selections follow the view's.
fn selections(mtm: MainThreadMarker) {
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    tv.setString(&NSString::from_str("hello world\nsecond"));
    let tlm = tlm_of(&tv).expect("TextKit 2");
    tv.setSelectedRange(NSRange::new(5, 3));
    let sels = tlm.textSelections();
    assert_eq!(sels.count(), 1);
    let cm = tlm.textContentManager().expect("content");
    let start = cm.documentRange().location();
    let ranges: Vec<(isize, isize)> = sels
        .objectAtIndex(0)
        .textRanges()
        .iter()
        .map(|r| {
            (
                cm.offsetFromLocation_toLocation(&start, &r.location()),
                cm.offsetFromLocation_toLocation(&start, &r.endLocation()),
            )
        })
        .collect();
    assert_eq!(ranges, [(5, 8)]);
}

/// Editing a TextKit 2 view: the content storage hears of it through the
/// storage, the edited paragraph gets a new fragment, and the one after it
/// moves along.
fn editing(mtm: MainThreadMarker) {
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 200.0, 100.0));
    tv.setString(&NSString::from_str("abc\ndef"));
    let tlm = tlm_of(&tv).expect("TextKit 2");
    let cm: Retained<NSTextContentManager> = tlm.textContentManager().expect("content");
    let at = |i: isize| cm.locationFromLocation_withOffset(&cm.documentRange().location(), i).expect("a location");
    tlm.ensureLayoutForRange(&cm.documentRange());
    let first_before = tlm.textLayoutFragmentForLocation(&at(0)).expect("a fragment");
    let second = tlm.textLayoutFragmentForLocation(&at(5)).expect("a fragment");
    tv.setSelectedRange(NSRange::new(0, 0));
    unsafe { tv.insertText_replacementRange(&NSString::from_str("X"), NSRange::new(NOT_FOUND, 0)) };
    assert_eq!(tv.string().to_string(), "Xabc\ndef");
    let first = tlm.textLayoutFragmentForLocation(&at(0)).expect("a fragment");
    let r = first.rangeInElement();
    let start = cm.documentRange().location();
    assert_eq!(
        (
            cm.offsetFromLocation_toLocation(&start, &r.location()),
            cm.offsetFromLocation_toLocation(&start, &r.endLocation())
        ),
        (0, 5)
    );
    assert!(!same(&*first, &*first_before), "the edited paragraph has a new fragment");
    // The paragraph after the edit moved along. (Whether its fragment is
    // the same object isn't pinned: through a text view macOS makes a new
    // one, through the storage alone it keeps it, as Sidestep does both
    // ways; see docs/text.md.)
    let again = tlm.textLayoutFragmentForLocation(&at(6)).expect("a fragment");
    let r = again.rangeInElement();
    assert_eq!(cm.offsetFromLocation_toLocation(&start, &r.location()), 5);
    drop(second);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("the test's main runs on the main thread");
    let tests: &[Test] = &[
        ("modes", modes),
        ("switch_to_text_kit_1", switch_to_text_kit_1),
        ("viewport_bounds", viewport_bounds),
        ("viewport_layout", viewport_layout),
        ("drawing", drawing),
        ("rendering_surface", rendering_surface),
        ("viewport_past_the_view", viewport_past_the_view),
        ("size_to_fit", size_to_fit),
        ("switch_notifications", switch_notifications),
        ("empty_field_editor", empty_field_editor),
        ("selections", selections),
        ("editing", editing),
    ];
    for (name, test) in tests {
        test(mtm);
        println!("test {name} ... ok");
    }
}
