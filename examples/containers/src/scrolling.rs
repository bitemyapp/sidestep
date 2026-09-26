//! The scroll view scenarios: overlay, transparent, nested, hscroll,
//! fling, stream and idle (see the crate's documentation).

use std::cell::Cell;
use std::ptr::NonNull;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSResponder, NSScrollView,
    NSScrollerStyle, NSStringDrawing, NSView, NSViewLayerContentsRedrawPolicy,
};
use objc2_foundation::{NSDictionary, NSObject, NSPoint, NSRect, NSSize, NSString, NSTimer};

use crate::{color, rect, sizable};

/// Label text in `color`.
fn text(fill: &NSColor, size: f64) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = NSFont::systemFontOfSize(size);
    // SAFETY: the constants are defined by AppKit.
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [&font, fill];
    NSDictionary::from_slices(&keys, &values)
}

fn draw_text(s: &str, at: NSPoint, attributes: &NSDictionary<NSString, AnyObject>) {
    // SAFETY: the attributes hold a font and a color.
    unsafe { NSString::from_str(s).drawAtPoint_withAttributes(at, Some(attributes)) };
}

pub(crate) struct RowsIvars {
    count: Cell<usize>,
    height: f64,
    /// Rows are drawn over a background (else only their text and marks).
    opaque: bool,
    hue: f64,
}

define_class!(
    /// A list of rows, drawing only the rows asked for.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ContainersRows"]
    #[ivars = RowsIvars]
    pub(crate) struct Rows;

    impl Rows {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            let ivars = self.ivars();
            let h = ivars.height;
            let width = self.bounds().size.width;
            let first = (dirty.origin.y / h).floor().max(0.0) as usize;
            let last = (((dirty.origin.y + dirty.size.height) / h).ceil() as usize).min(ivars.count.get());
            let ink = text(&color(0.1, 0.1, 0.12), 13.0);
            for row in first..last {
                let y = row as f64 * h;
                if ivars.opaque {
                    let shade = if row % 2 == 0 { 1.0 } else { 0.95 };
                    color(shade, shade, shade).setFill();
                    NSBezierPath::fillRect(rect(0.0, y, width, h));
                }
                let t = (row % 12) as f64 / 12.0;
                color(0.4 + 0.5 * t, 0.55 + ivars.hue * 0.3, 0.95 - 0.5 * t).setFill();
                NSBezierPath::fillRect(rect(8.0, y + 4.0, 12.0, h - 8.0));
                draw_text(&format!("Row {row}: the quick brown fox jumps over the lazy dog"), NSPoint::new(28.0, y + 3.0), &ink);
            }
        }
    }
);

fn rows(mtm: MainThreadMarker, count: usize, width: f64, opaque: bool, hue: f64) -> Retained<Rows> {
    let height = 22.0;
    let this = Rows::alloc(mtm).set_ivars(RowsIvars { count: Cell::new(count), height, opaque, hue });
    // SAFETY: the superclass's designated initializer.
    unsafe { msg_send![super(this), initWithFrame: rect(0.0, 0.0, width, count as f64 * height)] }
}

define_class!(
    /// A wide timeline: a tick every 50 points, a label every 200.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ContainersTimeline"]
    pub(crate) struct Timeline;

    impl Timeline {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            let height = self.bounds().size.height;
            color(0.98, 0.97, 0.94).setFill();
            NSBezierPath::fillRect(dirty);
            let ink = text(&color(0.2, 0.2, 0.25), 12.0);
            let first = (dirty.origin.x / 50.0).floor().max(0.0) as usize;
            let last = ((dirty.origin.x + dirty.size.width) / 50.0).ceil() as usize;
            for i in first..=last {
                let x = i as f64 * 50.0;
                let major = i % 4 == 0;
                color(0.3, 0.3, 0.35).setFill();
                NSBezierPath::fillRect(rect(x, 0.0, 1.0, if major { 24.0 } else { 12.0 }));
                if major {
                    draw_text(&format!("{} s", i / 4), NSPoint::new(x + 4.0, 6.0), &ink);
                }
            }
            // Lanes of events.
            for lane in 0..8 {
                let y = 40.0 + lane as f64 * 44.0;
                if y > height {
                    break;
                }
                let step = 170.0 + lane as f64 * 37.0;
                let first = ((dirty.origin.x - 160.0) / step).floor().max(0.0) as usize;
                let last = ((dirty.origin.x + dirty.size.width) / step).ceil() as usize;
                for k in first..=last {
                    let x = k as f64 * step + lane as f64 * 13.0;
                    let t = ((k + lane) % 7) as f64 / 7.0;
                    color(0.35 + 0.5 * t, 0.6, 0.9 - 0.4 * t).setFill();
                    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                        rect(x, y, 140.0, 32.0),
                        6.0,
                        6.0,
                    );
                    path.fill();
                }
            }
        }
    }
);

define_class!(
    /// Diagonal stripes, to see through things over them.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ContainersStripes"]
    pub(crate) struct Stripes;

    impl Stripes {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            color(0.97, 0.93, 0.84).setFill();
            NSBezierPath::fillRect(dirty);
            color(0.93, 0.78, 0.55).setFill();
            let b = self.bounds();
            let mut x = -b.size.height;
            while x < b.size.width {
                let path = NSBezierPath::bezierPath();
                path.moveToPoint(NSPoint::new(x, 0.0));
                path.lineToPoint(NSPoint::new(x + 18.0, 0.0));
                path.lineToPoint(NSPoint::new(x + 18.0 + b.size.height, b.size.height));
                path.lineToPoint(NSPoint::new(x + b.size.height, b.size.height));
                path.closePath();
                path.fill();
                x += 40.0;
            }
        }
    }
);

fn scroll_view(mtm: MainThreadMarker, frame: NSRect, document: &NSView) -> Retained<NSScrollView> {
    let sv = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    sv.setHasVerticalScroller(true);
    sv.setDocumentView(Some(document));
    sv
}

/// Run `f` every frame (60 times a second) until the app ends.
fn every_frame(timers: &mut Vec<Retained<NSTimer>>, f: impl Fn(f64) + 'static) {
    let start = Instant::now();
    let block = RcBlock::new(move |_: NonNull<NSTimer>| f(start.elapsed().as_secs_f64()));
    // SAFETY: the block takes the timer and returns nothing.
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 60.0, true, &block) };
    timers.push(timer);
}

/// Scroll `sv` to `y` (and `x`), as a program scrolling it would.
fn scroll_to(sv: &NSScrollView, x: f64, y: f64) {
    let clip = sv.contentView();
    let bounds = clip.bounds();
    let target = clip.constrainBoundsRect(NSRect::new(NSPoint::new(x, y), bounds.size)).origin;
    clip.scrollToPoint(target);
    sv.reflectScrolledClipView(&clip);
}

pub(crate) fn overlay(mtm: MainThreadMarker, frame: NSRect, timers: &mut Vec<Retained<NSTimer>>) -> Retained<NSView> {
    let root = crate::pane(mtm, "", color(0.9, 0.9, 0.92));
    root.setFrame(frame);
    root.setAutoresizingMask(sizable());
    let list = rows(mtm, 2000, frame.size.width - 40.0, true, 0.2);
    let sv = scroll_view(mtm, rect(20.0, 20.0, frame.size.width - 40.0, frame.size.height - 40.0), &list);
    sv.setScrollerStyle(NSScrollerStyle::Overlay);
    sv.setHasHorizontalScroller(true);
    sv.setAutoresizingMask(sizable());
    root.addSubview(&sv);
    // A badge floating over the list, painted after it.
    let badge = crate::swatch(mtm, "Floating over the list", color(1.0, 0.85, 0.4), NSSize::new(210.0, 28.0));
    badge.setFrame(rect(frame.size.width - 280.0, 40.0, 210.0, 28.0));
    root.addSubview(&badge);
    scroll_to(&sv, 0.0, 700.0);
    // Scrolling a little each second keeps the scrollers shown.
    every_frame(timers, move |t| {
        let y = 700.0 + (t * 0.8).sin() * 60.0;
        scroll_to(&sv, 0.0, y);
    });
    root
}

pub(crate) fn transparent(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    // SAFETY: NSView's designated initializer.
    let backdrop: Retained<Stripes> = unsafe { msg_send![Stripes::alloc(mtm), initWithFrame: frame] };
    backdrop.setAutoresizingMask(sizable());
    let list = rows(mtm, 2000, frame.size.width - 120.0, false, 0.6);
    let sv = scroll_view(mtm, rect(60.0, 40.0, frame.size.width - 120.0, frame.size.height - 80.0), &list);
    sv.setScrollerStyle(NSScrollerStyle::Legacy);
    sv.setDrawsBackground(false);
    sv.setAutoresizingMask(sizable());
    backdrop.addSubview(&sv);
    scroll_to(&sv, 0.0, 300.0);
    Retained::into_super(backdrop)
}

pub(crate) fn nested(mtm: MainThreadMarker, frame: NSRect, timers: &mut Vec<Retained<NSTimer>>) -> Retained<NSView> {
    let shelves = 12;
    let shelf = 150.0;
    // A document of no rows: only the shelves in it draw.
    let column = rows(mtm, 0, frame.size.width, false, 0.0);
    column.setFrameSize(NSSize::new(frame.size.width, shelves as f64 * shelf));
    for i in 0..shelves {
        let y = i as f64 * shelf;
        let title = crate::swatch(mtm, &format!("Shelf {i}"), color(0.95, 0.95, 0.97), NSSize::new(-1.0, -1.0));
        title.setFrame(rect(10.0, y + 6.0, frame.size.width - 40.0, 24.0));
        column.addSubview(&title);
        // A row of cards, wider than the window.
        let cards = crate::pane(mtm, "", color(0.99, 0.99, 1.0));
        cards.setFrame(rect(0.0, 0.0, 30.0 * 150.0, 110.0));
        for k in 0..30 {
            let t = ((i + k) % 9) as f64 / 9.0;
            let card = crate::swatch(
                mtm,
                &format!("Card {i}.{k}"),
                color(0.5 + 0.4 * t, 0.8, 1.0 - 0.4 * t),
                NSSize::new(-1.0, -1.0),
            );
            card.setFrame(rect(10.0 + k as f64 * 150.0, 8.0, 140.0, 94.0));
            cards.addSubview(&card);
        }
        let row =
            NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(10.0, y + 34.0, frame.size.width - 40.0, 110.0));
        row.setHasHorizontalScroller(true);
        row.setScrollerStyle(NSScrollerStyle::Overlay);
        row.setDocumentView(Some(&cards));
        scroll_to(&row, (i as f64 * 97.0) % 1800.0, 0.0);
        row.flashScrollers();
        column.addSubview(&row);
    }
    let sv = scroll_view(mtm, frame, &column);
    sv.setScrollerStyle(NSScrollerStyle::Overlay);
    sv.setAutoresizingMask(sizable());
    scroll_to(&sv, 0.0, 220.0);
    sv.flashScrollers();
    if std::env::var_os("CONTAINERS_SCROLL").is_some() {
        // The shelves go by, up and down.
        let sv = sv.clone();
        every_frame(timers, move |t| {
            let along = (t * 300.0) % 2000.0;
            scroll_to(&sv, 0.0, if along < 1000.0 { along } else { 2000.0 - along });
        });
    }
    Retained::into_super(sv)
}

pub(crate) fn hscroll(mtm: MainThreadMarker, frame: NSRect, timers: &mut Vec<Retained<NSTimer>>) -> Retained<NSView> {
    let width = 12000.0;
    // SAFETY: NSView's designated initializer.
    let timeline: Retained<Timeline> =
        unsafe { msg_send![Timeline::alloc(mtm), initWithFrame: rect(0.0, 0.0, width, 420.0)] };
    let sv = scroll_view(mtm, frame, &timeline);
    sv.setHasHorizontalScroller(true);
    sv.setAutoresizingMask(sizable());
    scroll_to(&sv, 2400.0, 0.0);
    if std::env::var_os("CONTAINERS_SCROLL").is_some() {
        let sv = sv.clone();
        every_frame(timers, move |t| scroll_to(&sv, 2400.0 + (t * 600.0) % 8000.0, 0.0));
    }
    Retained::into_super(sv)
}

pub(crate) fn fling(mtm: MainThreadMarker, frame: NSRect, timers: &mut Vec<Retained<NSTimer>>) -> Retained<NSView> {
    let list = rows(mtm, 5000, frame.size.width, true, 0.4);
    let sv = scroll_view(mtm, frame, &list);
    sv.setAutoresizingMask(sizable());
    let shown = sv.clone();
    // A flick every two seconds, alternately down and up, slowing as a
    // touchpad's momentum does.
    let last = Cell::new(0.0f64);
    let position = Cell::new(0.0f64);
    every_frame(timers, move |t| {
        let phase = t % 2.0;
        let direction = if ((t / 2.0) as u64).is_multiple_of(2) { 1.0 } else { -1.0 };
        let speed = 4000.0 * 0.997f64.powf(phase * 1000.0);
        let dt = t - last.replace(t);
        position.set((position.get() + direction * speed * dt).max(0.0));
        scroll_to(&sv, 0.0, position.get());
    });
    Retained::into_super(shown)
}

pub(crate) fn stream(mtm: MainThreadMarker, frame: NSRect, timers: &mut Vec<Retained<NSTimer>>) -> Retained<NSView> {
    let log = rows(mtm, 40, frame.size.width, true, 0.8);
    // Rows already drawn stay as they are when the log grows: only the new
    // one needs drawing (a view that draws is redrawn whole on a resize
    // unless it says so).
    log.setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::OnSetNeedsDisplay);
    let sv = scroll_view(mtm, frame, &log);
    sv.setAutoresizingMask(sizable());
    let bottom = |sv: &NSScrollView, log: &Rows| {
        let h = log.frame().size.height;
        scroll_to(sv, 0.0, h);
    };
    bottom(&sv, &log);
    let shown = sv.clone();
    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
        // A line more, and the view kept at its end.
        let count = log.ivars().count.get() + 1;
        log.ivars().count.set(count);
        let size = NSSize::new(log.frame().size.width, count as f64 * log.ivars().height);
        log.setFrameSize(size);
        bottom(&sv, &log);
    });
    // SAFETY: the block takes the timer and returns nothing.
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &block) };
    timers.push(timer);
    Retained::into_super(shown)
}

pub(crate) fn idle(mtm: MainThreadMarker, frame: NSRect, timers: &mut Vec<Retained<NSTimer>>) -> Retained<NSView> {
    let list = rows(mtm, 2000, frame.size.width, true, 0.1);
    let sv = scroll_view(mtm, frame, &list);
    sv.setAutoresizingMask(sizable());
    scroll_to(&sv, 0.0, 440.0);
    // A caret in the document, blinking twice a second.
    let caret = crate::swatch(mtm, "", color(0.1, 0.3, 0.9), NSSize::new(-1.0, -1.0));
    caret.setFrame(rect(300.0, 440.0 + 22.0 * 5.0 + 3.0, 2.0, 16.0));
    list.addSubview(&caret);
    let block = RcBlock::new(move |_: NonNull<NSTimer>| caret.setHidden(!caret.isHidden()));
    // SAFETY: the block takes the timer and returns nothing.
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.5, true, &block) };
    timers.push(timer);
    Retained::into_super(sv)
}

/// CONTAINERS_BENCH: time scrolling a scroll view in a window that is never
/// shown (the API's own cost, no drawing): a step is `scrollToPoint:` and
/// `reflectScrolledClipView:`; a tile is `tile` with both scrollers. The
/// median of seven runs.
pub(crate) fn bench(mtm: MainThreadMarker, window: &objc2_app_kit::NSWindow) {
    let median = |f: &dyn Fn() -> f64| {
        let mut runs: Vec<f64> = (0..7).map(|_| f()).collect();
        runs.sort_by(f64::total_cmp);
        runs[3]
    };
    for style in [NSScrollerStyle::Legacy, NSScrollerStyle::Overlay] {
        let list = rows(mtm, 20000, 600.0, true, 0.0);
        let sv = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 600.0, 400.0));
        sv.setScrollerStyle(style);
        sv.setHasVerticalScroller(true);
        sv.setHasHorizontalScroller(true);
        sv.setDocumentView(Some(&list));
        window.setContentView(Some(&sv));
        let clip = sv.contentView();
        let steps = 10000;
        let step = median(&|| {
            let start = Instant::now();
            for i in 0..steps {
                clip.scrollToPoint(NSPoint::new(0.0, (i * 7 % 400000) as f64));
                sv.reflectScrolledClipView(&clip);
            }
            start.elapsed().as_secs_f64() * 1e6 / steps as f64
        });
        let tiles = 2000;
        let tile = median(&|| {
            let start = Instant::now();
            for _ in 0..tiles {
                sv.tile();
            }
            start.elapsed().as_secs_f64() * 1e6 / tiles as f64
        });
        println!("scroll bench, {style:?} scrollers: a scroll step {step:.3} us, a tile {tile:.3} us");
    }
}
