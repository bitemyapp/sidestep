//! Scroll views on Linux, through the null render thread: which clip
//! views get layers, where the layers are placed and stacked, their
//! tiles (anchored rows, columns, the memory cap), transparency and
//! overlays, what a document redraws as it grows, passes that meet
//! program code, and scrolling by wheel, touchpad gesture, scroller and
//! action. What Apple's AppKit does is pinned by
//! `conformance/tests/appkit_scroll.rs`; these cover what only Sidestep's
//! display pass and input show.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

#[cfg(target_vendor = "apple")]
fn main() {}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    linux::main();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::RefCell;
    use std::time::Duration;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSApplication, NSBackingStoreType, NSCursor, NSResponder, NSScrollView, NSScrollerPart, NSScrollerStyle,
        NSView, NSViewLayerContentsRedrawPolicy, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSPoint, NSRect, NSSize};
    use sidestep_appkit::testing::{self, LayerInfo, Seen};

    thread_local!(static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) });

    fn log(line: String) {
        LOG.with(|l| l.borrow_mut().push(line));
    }

    fn take_log() -> Vec<String> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    }

    fn pt(x: f64, y: f64) -> NSPoint {
        NSPoint::new(x, y)
    }

    pub(crate) struct DocIvars {
        flipped: bool,
    }

    define_class!(
        /// A document or a container, flipped or not, taking the focus.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxScrollDocument"]
        #[ivars = DocIvars]
        pub(crate) struct Document;

        impl Document {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                self.ivars().flipped
            }

            #[unsafe(method(acceptsFirstResponder))]
            fn accepts_first_responder(&self) -> bool {
                true
            }
        }
    );

    fn view(mtm: MainThreadMarker, frame: NSRect, flipped: bool) -> Retained<NSView> {
        let this = Document::alloc(mtm).set_ivars(DocIvars { flipped });
        // SAFETY: NSView's designated initializer.
        let view: Retained<Document> = unsafe { msg_send![super(this), initWithFrame: frame] };
        Retained::into_super(view)
    }

    thread_local! {
        /// What a `Drawing` view does when it draws, once.
        static ON_DRAW: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    define_class!(
        /// A flipped view with a `drawRect:` of its own, which logs its
        /// name and runs what `ON_DRAW` holds.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxScrollDrawing"]
        #[ivars = &'static str]
        pub(crate) struct Drawing;

        impl Drawing {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                true
            }

            #[unsafe(method(drawRect:))]
            fn draw_rect(&self, _dirty: NSRect) {
                log(format!("drew {}", self.ivars()));
                if let Some(then) = ON_DRAW.with(|d| d.borrow_mut().take()) {
                    then();
                }
            }
        }
    );

    fn drawing(mtm: MainThreadMarker, frame: NSRect, name: &'static str) -> Retained<NSView> {
        let this = Drawing::alloc(mtm).set_ivars(name);
        // SAFETY: NSView's designated initializer.
        let view: Retained<Drawing> = unsafe { msg_send![super(this), initWithFrame: frame] };
        Retained::into_super(view)
    }

    define_class!(
        /// Logs the notifications it observes.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LinuxScrollObserver"]
        pub(crate) struct Observer;

        impl Observer {
            #[unsafe(method(seen:))]
            fn seen(&self, note: &NSNotification) {
                log(note.name().to_string());
            }
        }

        unsafe impl NSObjectProtocol for Observer {}
    );

    /// A scroll view at `frame` with a vertical scroller of `style` and a
    /// document `doc` big.
    fn scroll_view(
        mtm: MainThreadMarker,
        frame: NSRect,
        style: NSScrollerStyle,
        doc: NSSize,
        flipped: bool,
    ) -> (Retained<NSScrollView>, Retained<NSView>) {
        let sv = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        sv.setScrollerStyle(style);
        sv.setHasVerticalScroller(true);
        sv.tile();
        let document = view(mtm, NSRect::new(NSPoint::ZERO, doc), flipped);
        sv.setDocumentView(Some(&document));
        (sv, document)
    }

    /// A window of 400 by 300 whose content (flipped) holds `views`,
    /// shown.
    fn shown(mtm: MainThreadMarker, views: &[&NSView]) -> (Retained<NSWindow>, u32, Retained<NSView>) {
        shown_sized(mtm, NSSize::new(400.0, 300.0), views)
    }

    fn shown_sized(
        mtm: MainThreadMarker,
        size: NSSize,
        views: &[&NSView],
    ) -> (Retained<NSWindow>, u32, Retained<NSView>) {
        let content = view(mtm, NSRect::new(NSPoint::ZERO, size), true);
        for v in views {
            content.addSubview(v);
        }
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable;
        // SAFETY: a plain window, shown by the null render thread.
        let w = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, size),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: Rust owns the window, so closing it mustn't release it.
        unsafe { w.setReleasedWhenClosed(false) };
        w.setContentView(Some(&content));
        w.makeKeyAndOrderFront(None);
        testing::settle();
        // The first frame waits a moment for the desktop's light or dark,
        // which the null render thread never tells.
        thread_local!(static WAITED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
        if !WAITED.with(|w| w.replace(true)) {
            testing::run_for(200);
            testing::settle();
        }
        let id = testing::showing_id(&w);
        (w, id, content)
    }

    fn close(w: &NSWindow) {
        w.orderOut(None);
        testing::settle();
        w.setContentView(None);
    }

    fn addr(v: &NSView) -> usize {
        v as *const NSView as usize
    }

    fn layer_of<'a>(layers: &'a [LayerInfo], sv: &NSScrollView) -> Option<&'a LayerInfo> {
        layers.iter().find(|l| l.clip == addr(&sv.contentView()))
    }

    fn promotion(mtm: MainThreadMarker) {
        let (big, _) = scroll_view(
            mtm,
            rect(10.0, 20.0, 200.0, 150.0),
            NSScrollerStyle::Overlay,
            NSSize::new(400.0, 1000.0),
            true,
        );
        // Too small, fits its document, hidden: drawn inline.
        let (small, _) =
            scroll_view(mtm, rect(220.0, 20.0, 50.0, 50.0), NSScrollerStyle::Overlay, NSSize::new(100.0, 500.0), true);
        let (fits, _) =
            scroll_view(mtm, rect(220.0, 80.0, 150.0, 100.0), NSScrollerStyle::Overlay, NSSize::new(100.0, 80.0), true);
        let (hidden, _) = scroll_view(
            mtm,
            rect(220.0, 190.0, 150.0, 100.0),
            NSScrollerStyle::Overlay,
            NSSize::new(100.0, 800.0),
            true,
        );
        hidden.setHidden(true);
        let (w, _, _) = shown(mtm, &[&big, &small, &fits, &hidden]);
        let layers = testing::scroll_layers(&w);
        assert_eq!(layers.len(), 1, "{layers:?}");
        let l = layer_of(&layers, &big).expect("a layer");
        assert_eq!(l.viewport, [10.0, 20.0, 210.0, 170.0]);
        assert_eq!(l.origin, [10.0, 20.0]);
        assert_eq!(l.extent, [0.0, 0.0, 400.0, 1000.0]);
        assert_eq!(l.tile_size, [448, 512]);
        assert!(l.opaque, "the clip view draws an opaque background");
        assert_eq!(l.parent, 0);
        // The row in view and the one below it, drawn ahead.
        assert_eq!(l.tiles, [[0, 0], [0, 1]]);
        // Shown, it gets a layer; hidden, it loses its.
        hidden.setHidden(false);
        testing::settle();
        assert_eq!(testing::scroll_layers(&w).len(), 2);
        big.setHidden(true);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        assert_eq!(layers.len(), 1);
        assert!(layer_of(&layers, &hidden).is_some());
        close(&w);
    }

    fn scrolling_moves_layers(mtm: MainThreadMarker) {
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(400.0, 5000.0), true);
        let (w, _, _) = shown(mtm, &[&sv]);
        let clip = sv.contentView();
        clip.scrollToPoint(pt(0.0, 3000.0));
        sv.reflectScrolledClipView(&clip);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        // The layer moved; the document's pixels didn't.
        assert_eq!(l.origin, [0.0, -3000.0]);
        assert_eq!(l.viewport, [0.0, 0.0, 200.0, 150.0]);
        // Rows 5 and 6 are in view, row 7 drawn ahead (downward); the rows
        // more than two tiles up were dropped.
        assert!(l.tiles.contains(&[0, 5]) && l.tiles.contains(&[0, 6]) && l.tiles.contains(&[0, 7]), "{:?}", l.tiles);
        assert!(!l.tiles.contains(&[0, 0]) && !l.tiles.contains(&[0, 1]), "{:?}", l.tiles);
        // Scrolling up draws ahead upward.
        clip.scrollToPoint(pt(0.0, 2600.0));
        sv.reflectScrolledClipView(&clip);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        assert!(l.tiles.contains(&[0, 4]), "{:?}", l.tiles);
        close(&w);
    }

    /// With no time for tiles ahead, as on a slow machine, each pass puts
    /// them off to the next, which draws them the way the layer went.
    fn tiles_ahead_come_a_pass_late(mtm: MainThreadMarker) {
        testing::set_prefetch_budget(Some(Duration::ZERO));
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(400.0, 5000.0), true);
        let (w, _, _) = shown(mtm, &[&sv]);
        let layers = testing::scroll_layers(&w);
        assert_eq!(layer_of(&layers, &sv).expect("a layer").tiles, [[0, 0], [0, 1]]);
        let clip = sv.contentView();
        clip.scrollToPoint(pt(0.0, 3000.0));
        sv.reflectScrolledClipView(&clip);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        assert!(l.tiles.contains(&[0, 7]), "{:?}", l.tiles);
        // Only row 5 is in view here, and row 6 is still held: the pass
        // late goes up, as the scroll did, not down as a still layer does.
        clip.scrollToPoint(pt(0.0, 2600.0));
        sv.reflectScrolledClipView(&clip);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        assert!(l.tiles.contains(&[0, 4]), "{:?}", l.tiles);
        testing::set_prefetch_budget(None);
        close(&w);
    }

    fn anchored_rows_and_columns(mtm: MainThreadMarker) {
        // An unflipped document: its rows are negative, counted up from its
        // bottom, so growing it moves nothing already drawn.
        let (sv, doc) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(3000.0, 400.0), false);
        sv.setHasHorizontalScroller(true);
        let (w, _, _) = shown(mtm, &[&sv]);
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        assert_eq!(l.extent, [0.0, -400.0, 3000.0, 0.0]);
        // Columns as wide as the widest a tile gets.
        assert_eq!(l.tile_size, [2048, 512]);
        assert_eq!(l.tiles, [[0, -1]]);
        // Scrolling sideways reaches the next column.
        let clip = sv.contentView();
        clip.scrollToPoint(pt(2500.0, 0.0));
        sv.reflectScrolledClipView(&clip);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        assert_eq!(l.origin, [-2500.0, 150.0]);
        assert!(l.tiles.contains(&[1, -1]), "{:?}", l.tiles);
        // Grown taller, it keeps its tiles (and its bottom in view).
        doc.setFrameSize(NSSize::new(3000.0, 800.0));
        testing::settle();
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        assert_eq!(l.extent, [0.0, -800.0, 3000.0, 0.0]);
        assert!(l.tiles.contains(&[1, -1]) && l.tiles.contains(&[0, -1]), "{:?}", l.tiles);
        assert_eq!(clip.bounds().origin, pt(2500.0, 0.0));
        close(&w);
    }

    fn nested_layers_stack_in_paint_order(mtm: MainThreadMarker) {
        let (outer, outer_doc) =
            scroll_view(mtm, rect(0.0, 0.0, 300.0, 250.0), NSScrollerStyle::Overlay, NSSize::new(300.0, 2000.0), true);
        let (inner, _) =
            scroll_view(mtm, rect(20.0, 50.0, 200.0, 100.0), NSScrollerStyle::Overlay, NSSize::new(200.0, 800.0), true);
        outer_doc.addSubview(&inner);
        let (w, _, _) = shown(mtm, &[&outer]);
        let clip = outer.contentView();
        clip.scrollToPoint(pt(0.0, 80.0));
        outer.reflectScrolledClipView(&clip);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        assert_eq!(layers.len(), 2, "{layers:?}");
        let (o, i) = (layer_of(&layers, &outer).expect("outer"), layer_of(&layers, &inner).expect("inner"));
        assert_eq!(i.parent, o.clip);
        // The outer layer's tiles, the inner layer (tiles, overlay), then
        // the outer overlay.
        assert!(o.z[0] < i.z[0] && i.z[0] < i.z[1] && i.z[1] < o.z[1], "{:?} {:?}", o.z, i.z);
        // The inner viewport is cut by the outer one.
        assert_eq!(i.viewport, [20.0, 0.0, 220.0, 70.0]);
        assert_eq!(i.origin, [20.0, -30.0]);
        // Each overlay holds its own scroller.
        let scroller = |sv: &NSScrollView| addr(&sv.verticalScroller().expect("a scroller"));
        assert_eq!(o.overlay_views, [scroller(&outer)]);
        assert_eq!(i.overlay_views, [scroller(&inner)]);
        assert_eq!(o.overlay, Some([283.0, 0.0, 300.0, 250.0]));
        // The inner one is in the outer layer's points, and whole: the
        // outer layer scrolling moves it and draws nothing in it.
        assert_eq!(i.overlay, Some([203.0, 50.0, 220.0, 150.0]));
        let paints = i.overlay_paints;
        for y in [70.0, 30.0, 0.0, 50.0] {
            clip.scrollToPoint(pt(0.0, y));
            outer.reflectScrolledClipView(&clip);
            testing::settle();
            let layers = testing::scroll_layers(&w);
            let i = layer_of(&layers, &inner).expect("inner");
            assert_eq!(i.overlay, Some([203.0, 50.0, 220.0, 150.0]));
            assert_eq!(i.overlay_paints, paints, "scrolled to {y}");
        }
        close(&w);
    }

    fn transparency_and_overlays(mtm: MainThreadMarker) {
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy, NSSize::new(400.0, 1000.0), true);
        sv.setDrawsBackground(false);
        // A view floating over the scroll view, painted after it.
        let badge = view(mtm, rect(150.0, 100.0, 100.0, 30.0), true);
        let (w, _, _) = shown(mtm, &[&sv, &badge]);
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        assert!(!l.opaque, "no background: what's behind shows through");
        // Legacy scrollers sit beside the viewport; the badge draws nothing
        // of its own, so it isn't in the overlay either.
        assert!(l.overlay.is_none() && l.overlay_views.is_empty(), "{l:?}");
        close(&w);
        // An overlay scroller, and a view that draws, over the viewport.
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(400.0, 1000.0), true);
        let label =
            objc2_app_kit::NSBox::initWithFrame(objc2_app_kit::NSBox::alloc(mtm), rect(150.0, 100.0, 100.0, 30.0));
        let (w, _, _) = shown(mtm, &[&sv, &label]);
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer");
        let scroller = addr(&sv.verticalScroller().expect("a scroller"));
        assert_eq!(l.overlay_views, [scroller, addr(&label)]);
        assert_eq!(l.overlay, Some([150.0, 0.0, 250.0, 150.0]));
        close(&w);
    }

    fn wheels_scroll_by_lines(mtm: MainThreadMarker) {
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(400.0, 1000.0), true);
        let (w, id, _) = shown(mtm, &[&sv]);
        let clip = sv.contentView();
        // One detent is three lines of verticalLineScroll.
        testing::inject_wheel(id, 50.0, 50.0, 0.0, 1.0);
        testing::settle();
        assert_eq!(clip.bounds().origin, pt(0.0, 30.0));
        sv.setVerticalLineScroll(20.0);
        testing::inject_wheel(id, 50.0, 50.0, 0.0, -1.0);
        testing::settle();
        assert_eq!(clip.bounds().origin, pt(0.0, 0.0));
        // Sideways.
        testing::inject_wheel(id, 50.0, 50.0, 1.0, 0.0);
        testing::settle();
        assert_eq!(clip.bounds().origin, pt(30.0, 0.0));
        // The scroller follows.
        let v = sv.verticalScroller().expect("a scroller");
        testing::inject_wheel(id, 50.0, 50.0, 0.0, 2.0);
        testing::settle();
        assert_eq!(v.doubleValue(), 120.0 / 850.0);
        close(&w);
    }

    fn what_a_scroll_view_cant_use_goes_up(mtm: MainThreadMarker) {
        let (outer, outer_doc) =
            scroll_view(mtm, rect(0.0, 0.0, 300.0, 250.0), NSScrollerStyle::Overlay, NSSize::new(300.0, 2000.0), true);
        // Only sideways.
        let (inner, _) =
            scroll_view(mtm, rect(0.0, 100.0, 150.0, 50.0), NSScrollerStyle::Overlay, NSSize::new(600.0, 50.0), true);
        outer_doc.addSubview(&inner);
        let (w, id, _) = shown(mtm, &[&outer]);
        testing::inject_wheel(id, 50.0, 120.0, 0.0, 1.0);
        testing::settle();
        assert_eq!(inner.contentView().bounds().origin, pt(0.0, 0.0));
        assert_eq!(outer.contentView().bounds().origin, pt(0.0, 30.0));
        testing::inject_wheel(id, 50.0, 90.0, 1.0, 0.0);
        testing::settle();
        assert_eq!(inner.contentView().bounds().origin, pt(30.0, 0.0));
        assert_eq!(outer.contentView().bounds().origin, pt(0.0, 30.0));
        close(&w);
    }

    fn gestures_scroll_live(mtm: MainThreadMarker) {
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(400.0, 1000.0), true);
        let (w, id, _) = shown(mtm, &[&sv]);
        // SAFETY: the observer outlives its registration, removed below.
        let observer: Retained<Observer> = unsafe { msg_send![Observer::alloc(mtm), init] };
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: seen: takes the notification.
        unsafe { center.addObserver_selector_name_object(&observer, sel!(seen:), None, Some(&sv)) };
        take_log();
        testing::inject_scroll(id, 50.0, 50.0, (0.0, 12.5), 1, (0.0, 0.0));
        testing::inject_scroll(id, 50.0, 50.0, (0.0, 20.0), 2, (0.0, 0.0));
        testing::inject_scroll(id, 50.0, 50.0, (0.0, 0.0), 3, (0.0, 0.0));
        testing::settle();
        testing::run_for(20);
        assert_eq!(sv.contentView().bounds().origin, pt(0.0, 32.5));
        assert_eq!(
            take_log(),
            [
                "NSScrollViewWillStartLiveScrollNotification",
                "NSScrollViewDidLiveScrollNotification",
                "NSScrollViewDidLiveScrollNotification",
                "NSScrollViewDidEndLiveScrollNotification",
            ]
        );
        // A wheel scrolls live, without a start or an end (as on macOS
        // once a scroll view has scrolled once).
        testing::inject_wheel(id, 50.0, 50.0, 0.0, 1.0);
        testing::settle();
        assert_eq!(take_log(), ["NSScrollViewDidLiveScrollNotification"]);
        // A flick coasts on: the live scroll ends when the coasting does.
        testing::inject_scroll(id, 50.0, 50.0, (0.0, 10.0), 1, (0.0, 0.0));
        testing::inject_scroll(id, 50.0, 50.0, (0.0, 30.0), 2, (0.0, 0.0));
        testing::inject_scroll(id, 50.0, 50.0, (0.0, 0.0), 3, (0.0, 1500.0));
        testing::settle();
        testing::run_for(60);
        let started = take_log();
        assert_eq!(started[0], "NSScrollViewWillStartLiveScrollNotification");
        assert!(!started.iter().any(|n| n.contains("DidEnd")), "{started:?}");
        let before = sv.contentView().bounds().origin.y;
        testing::run_for(3000);
        let coasted = take_log();
        assert!(sv.contentView().bounds().origin.y > before + 50.0);
        assert_eq!(coasted.last().map(String::as_str), Some("NSScrollViewDidEndLiveScrollNotification"), "{coasted:?}");
        assert_eq!(coasted.iter().filter(|n| n.contains("DidEnd")).count(), 1);
        // SAFETY: removing the observer's registrations.
        unsafe { center.removeObserver(&observer) };
        close(&w);
    }

    fn scrollers_drag_and_page(mtm: MainThreadMarker) {
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Legacy, NSSize::new(183.0, 1500.0), true);
        let (w, id, _) = shown(mtm, &[&sv]);
        // SAFETY: the observer outlives its registration, removed below.
        let observer: Retained<Observer> = unsafe { msg_send![Observer::alloc(mtm), init] };
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: seen: takes the notification.
        unsafe { center.addObserver_selector_name_object(&observer, sel!(seen:), None, Some(&sv)) };
        let live = || take_log().into_iter().filter(|n| n.contains("LiveScroll")).collect::<Vec<_>>();
        live();
        let clip = sv.contentView();
        let v = sv.verticalScroller().expect("a scroller");
        assert_eq!(v.frame(), rect(183.0, 0.0, 17.0, 150.0));
        // The knob: 20 points (a tenth of 144 is less), at the top.
        assert_eq!(v.rectForPart(NSScrollerPart::Knob), rect(3.0, 3.0, 11.0, 20.0));
        // Dragging the knob is a live scroll, as on macOS.
        testing::inject_button(id, 191.0, 13.0, 0, true, 1, 0);
        testing::settle();
        assert_eq!(v.hitPart(), NSScrollerPart::Knob);
        assert_eq!(live(), ["NSScrollViewWillStartLiveScrollNotification"]);
        // Halfway along the knob's travel of 124 points.
        testing::inject_motion(id, 191.0, 75.0, 0);
        testing::settle();
        assert_eq!(v.doubleValue(), 0.5);
        assert_eq!(clip.bounds().origin, pt(0.0, 675.0));
        assert_eq!(live(), ["NSScrollViewDidLiveScrollNotification"]);
        testing::inject_button(id, 191.0, 75.0, 0, false, 1, 0);
        testing::settle();
        assert_eq!(v.hitPart(), NSScrollerPart::NoPart);
        assert_eq!(live(), ["NSScrollViewDidEndLiveScrollNotification"]);
        // A click below the knob pages down: the visible height less the
        // page overlap, live too.
        testing::inject_button(id, 191.0, 140.0, 0, true, 1, 0);
        testing::inject_button(id, 191.0, 140.0, 0, false, 1, 0);
        testing::settle();
        assert_eq!(clip.bounds().origin, pt(0.0, 815.0));
        assert_eq!(
            live(),
            [
                "NSScrollViewWillStartLiveScrollNotification",
                "NSScrollViewDidLiveScrollNotification",
                "NSScrollViewDidEndLiveScrollNotification"
            ]
        );
        // SAFETY: removing the observer's registrations.
        unsafe { center.removeObserver(&observer) };
        close(&w);
    }

    fn hidden_overlay_scrollers_let_clicks_through(mtm: MainThreadMarker) {
        let (sv, doc) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(200.0, 1000.0), true);
        let (w, _, content) = shown(mtm, &[&sv]);
        let v = sv.verticalScroller().expect("a scroller");
        // hitTest: takes the point in the window's coordinates here.
        let over = content.convertPoint_toView(pt(191.0, 75.0), None);
        // Idle: the document takes the click.
        let hit = content.hitTest(over).expect("a view");
        assert!(std::ptr::eq(&*hit, &*doc), "{hit:?}");
        // Scrolled, it shows, and takes it.
        let clip = sv.contentView();
        clip.scrollToPoint(pt(0.0, 100.0));
        sv.reflectScrolledClipView(&clip);
        let hit = content.hitTest(over).expect("a view");
        assert!(std::ptr::eq(&*hit, addr(&v) as *const NSView), "{hit:?}");
        // A second after the last scroll it fades, and lets clicks through
        // again.
        testing::run_for(1400);
        let hit = content.hitTest(over).expect("a view");
        assert!(std::ptr::eq(&*hit, &*doc), "{hit:?}");
        close(&w);
    }

    fn page_actions_reach_the_scroll_view(mtm: MainThreadMarker) {
        let (sv, doc) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(200.0, 1000.0), true);
        let (w, _, _) = shown(mtm, &[&sv]);
        w.makeFirstResponder(Some(&doc));
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: pageDown: takes a sender.
        let sent = unsafe { app.sendAction_to_from(sel!(pageDown:), None, None::<&AnyObject>) };
        assert!(sent);
        assert_eq!(sv.contentView().bounds().origin, pt(0.0, 140.0));
        // SAFETY: as above.
        let _ = unsafe { app.sendAction_to_from(sel!(pageUp:), None, None::<&AnyObject>) };
        assert_eq!(sv.contentView().bounds().origin, pt(0.0, 0.0));
        close(&w);
    }

    fn damage_goes_where_it_is_drawn(mtm: MainThreadMarker) {
        let (sv, doc) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(200.0, 1000.0), true);
        let (w, _, _) = shown(mtm, &[&sv]);
        let before = layer_of(&testing::scroll_layers(&w), &sv).expect("a layer").clone();
        // A scroller is drawn in the overlay only.
        let scroller: Retained<NSView> =
            Retained::into_super(Retained::into_super(sv.verticalScroller().expect("a scroller")));
        scroller.setNeedsDisplay(true);
        testing::settle();
        let after = layer_of(&testing::scroll_layers(&w), &sv).expect("a layer").clone();
        assert_eq!(after.overlay_paints, before.overlay_paints + 1);
        assert_eq!(after.tile_paints, before.tile_paints);
        // The document in its tiles only, where the overlay isn't.
        doc.setNeedsDisplayInRect(rect(10.0, 10.0, 20.0, 20.0));
        testing::settle();
        let now = layer_of(&testing::scroll_layers(&w), &sv).expect("a layer").clone();
        assert_eq!(now.tile_paints, after.tile_paints + 1);
        assert_eq!(now.overlay_paints, after.overlay_paints);
        assert_eq!(now.tile_area - after.tile_area, 400.0);
        // Scrolling among the tiles drawn draws nothing in them.
        let clip = sv.contentView();
        for y in [40.0, 80.0, 120.0, 60.0] {
            clip.scrollToPoint(pt(0.0, y));
            sv.reflectScrolledClipView(&clip);
            testing::settle();
        }
        let scrolled = layer_of(&testing::scroll_layers(&w), &sv).expect("a layer").clone();
        assert_eq!(scrolled.tile_paints, now.tile_paints);
        assert_eq!(scrolled.tiles, now.tiles);
        close(&w);
    }

    fn growing_documents_draw_what_they_gain(mtm: MainThreadMarker) {
        // A document that draws nothing itself, one that draws (which
        // macOS has redraw whole on every resize), and one that draws but
        // asks to redraw only on setNeedsDisplay:.
        for (name, strip_only) in [("plain", true), ("drawing", false), ("opted in", true)] {
            let sv = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 200.0, 150.0));
            sv.setScrollerStyle(NSScrollerStyle::Overlay);
            let frame = rect(0.0, 0.0, 200.0, 1000.0);
            let doc = if name == "plain" { view(mtm, frame, true) } else { drawing(mtm, frame, "document") };
            let expected = match name {
                "drawing" => NSViewLayerContentsRedrawPolicy::DuringViewResize,
                _ => NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay,
            };
            if name == "opted in" {
                doc.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay);
            }
            assert_eq!(doc.layerContentsRedrawPolicy(), expected, "{name}");
            sv.setDocumentView(Some(&doc));
            let (w, _, _) = shown(mtm, &[&sv]);
            let clip = sv.contentView();
            clip.scrollToPoint(pt(0.0, 850.0));
            sv.reflectScrolledClipView(&clip);
            testing::settle();
            let before = layer_of(&testing::scroll_layers(&w), &sv).expect("a layer").clone();
            // A line more, kept in view at the end.
            doc.setFrameSize(NSSize::new(200.0, 1022.0));
            clip.scrollToPoint(pt(0.0, 872.0));
            sv.reflectScrolledClipView(&clip);
            testing::settle();
            let after = layer_of(&testing::scroll_layers(&w), &sv).expect("a layer").clone();
            assert_eq!(after.extent, [0.0, 0.0, 200.0, 1022.0]);
            let drawn = after.tile_area - before.tile_area;
            if strip_only {
                assert_eq!(after.tile_paints, before.tile_paints + 1, "{name}");
                assert_eq!(drawn, 200.0 * 22.0, "{name}");
            } else {
                // The tile in view, whole; the one above it, out of view,
                // is dropped rather than drawn.
                assert!(before.tiles.contains(&[0, 0]), "{:?}", before.tiles);
                assert_eq!(after.tiles, [[0, 1]], "{name}");
                assert_eq!(drawn, 200.0 * (1022.0 - 511.0), "{name}");
            }
            close(&w);
        }
        take_log();
    }

    fn layers_are_capped(mtm: MainThreadMarker) {
        // 25 scroll views that would each get a layer: the window's surface
        // and 23 layers, the first 23 in paint order.
        let views: Vec<Retained<NSScrollView>> = (0..25)
            .map(|i| {
                let at = rect(f64::from(i % 5) * 100.0, f64::from(i / 5) * 100.0, 90.0, 90.0);
                scroll_view(mtm, at, NSScrollerStyle::Overlay, NSSize::new(90.0, 500.0), true).0
            })
            .collect();
        let refs: Vec<&NSView> = views.iter().map(|v| &***v).collect();
        let (w, _, _) = shown_sized(mtm, NSSize::new(500.0, 500.0), &refs);
        let layers = testing::scroll_layers(&w);
        assert_eq!(layers.len(), 23);
        for sv in &views[..23] {
            assert!(layer_of(&layers, sv).is_some());
        }
        // One going makes room for the next.
        views[0].setHidden(true);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        assert_eq!(layers.len(), 23);
        assert!(layer_of(&layers, &views[0]).is_none() && layer_of(&layers, &views[23]).is_some());
        close(&w);
    }

    fn tile_memory_is_capped(mtm: MainThreadMarker) {
        // Tiles of 448 by 512 pixels with their margins: 450 × 514 × 4
        // bytes each. A cap of three tiles, where the viewport would keep
        // four (two rows back, the one in view, one drawn ahead).
        let tile = 450 * 514 * 4;
        testing::set_tile_memory_cap(Some(3 * tile));
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 400.0, 300.0), NSScrollerStyle::Overlay, NSSize::new(400.0, 20000.0), true);
        let (w, _, _) = shown(mtm, &[&sv]);
        let clip = sv.contentView();
        // Scrolled down a row at a time, the rows passed stay kept (two
        // tiles back), until the cap drops the farthest.
        for row in 1..8 {
            clip.scrollToPoint(pt(0.0, f64::from(row) * 512.0 + 100.0));
            sv.reflectScrolledClipView(&clip);
            testing::settle();
            let layers = testing::scroll_layers(&w);
            let l = layer_of(&layers, &sv).expect("a layer");
            assert!(l.tiles.len() <= 3, "row {row}: {:?}", l.tiles);
            // Never one in view.
            assert!(l.tiles.contains(&[0, row]), "row {row}: {:?}", l.tiles);
            // The farthest go first: what's kept is next to the viewport.
            assert!(l.tiles.iter().all(|k| (k[1] - row).abs() <= 1), "row {row}: {:?}", l.tiles);
        }
        testing::set_tile_memory_cap(None);
        close(&w);
    }

    fn document_cursor(mtm: MainThreadMarker) {
        let (sv, _) =
            scroll_view(mtm, rect(0.0, 0.0, 200.0, 150.0), NSScrollerStyle::Overlay, NSSize::new(200.0, 1000.0), true);
        sv.setDocumentCursor(Some(&NSCursor::pointingHandCursor()));
        let (w, id, _) = shown(mtm, &[&sv]);
        testing::take_render_log();
        // Over the clip view: its document cursor; beside it: the arrow.
        testing::inject_enter(id, 50.0, 50.0);
        testing::inject_motion(id, 60.0, 60.0, 0);
        testing::settle();
        let shapes = |log: Vec<Seen>| {
            log.into_iter()
                .filter_map(|s| match s {
                    Seen::Cursor { name, .. } => Some(name),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(shapes(testing::take_render_log()).last().map(String::as_str), Some("pointer"));
        testing::inject_motion(id, 300.0, 60.0, 0);
        testing::settle();
        assert_eq!(shapes(testing::take_render_log()).last().map(String::as_str), Some("default"));
        close(&w);
    }

    fn passes_meet_program_code(mtm: MainThreadMarker) {
        // A window ordered out by drawing in its own pass: its layers go
        // with it, and come back when it is shown again.
        let sv = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 200.0, 150.0));
        sv.setScrollerStyle(NSScrollerStyle::Overlay);
        let doc = drawing(mtm, rect(0.0, 0.0, 200.0, 1000.0), "document");
        sv.setDocumentView(Some(&doc));
        let (w, _, _) = shown(mtm, &[&sv]);
        assert_eq!(testing::scroll_layers(&w).len(), 1);
        let window = w.clone();
        ON_DRAW.with(|d| d.replace(Some(Box::new(move || window.orderOut(None)))));
        doc.setNeedsDisplayInRect(rect(0.0, 0.0, 10.0, 10.0));
        testing::settle();
        assert!(!w.isVisible());
        assert!(testing::scroll_layers(&w).is_empty());
        w.makeKeyAndOrderFront(None);
        testing::settle();
        let layers = testing::scroll_layers(&w);
        let l = layer_of(&layers, &sv).expect("a layer again");
        assert!(l.tile_paints > 0 && !l.tiles.is_empty(), "{l:?}");
        close(&w);
        // Another window displayed from a view's drawing draws then.
        let other = drawing(mtm, rect(0.0, 0.0, 100.0, 100.0), "other");
        let (b, _, _) = shown(mtm, &[&other]);
        let first = drawing(mtm, rect(0.0, 0.0, 100.0, 100.0), "first");
        let (a, _, _) = shown(mtm, &[&first]);
        take_log();
        let shown_other = other.clone();
        ON_DRAW.with(|d| d.replace(Some(Box::new(move || shown_other.display()))));
        first.setNeedsDisplay(true);
        testing::settle();
        let log = take_log();
        assert!(log.contains(&"drew first".to_string()) && log.contains(&"drew other".to_string()), "{log:?}");
        close(&a);
        close(&b);
    }

    type Test = (&'static str, fn(MainThreadMarker));

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        testing::use_null_backend();
        let tests: &[Test] = &[
            ("promotion", promotion),
            ("scrolling_moves_layers", scrolling_moves_layers),
            ("tiles_ahead_come_a_pass_late", tiles_ahead_come_a_pass_late),
            ("anchored_rows_and_columns", anchored_rows_and_columns),
            ("nested_layers_stack_in_paint_order", nested_layers_stack_in_paint_order),
            ("transparency_and_overlays", transparency_and_overlays),
            ("wheels_scroll_by_lines", wheels_scroll_by_lines),
            ("what_a_scroll_view_cant_use_goes_up", what_a_scroll_view_cant_use_goes_up),
            ("gestures_scroll_live", gestures_scroll_live),
            ("scrollers_drag_and_page", scrollers_drag_and_page),
            ("hidden_overlay_scrollers_let_clicks_through", hidden_overlay_scrollers_let_clicks_through),
            ("page_actions_reach_the_scroll_view", page_actions_reach_the_scroll_view),
            ("damage_goes_where_it_is_drawn", damage_goes_where_it_is_drawn),
            ("growing_documents_draw_what_they_gain", growing_documents_draw_what_they_gain),
            ("layers_are_capped", layers_are_capped),
            ("tile_memory_is_capped", tile_memory_is_capped),
            ("document_cursor", document_cursor),
            ("passes_meet_program_code", passes_meet_program_code),
        ];
        let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
        for (name, test) in tests {
            if only.as_deref().is_some_and(|o| !name.contains(o)) {
                continue;
            }
            objc2::rc::autoreleasepool(|_| test(mtm));
            testing::take_render_log();
            println!("test {name} ... ok");
        }
    }
}
