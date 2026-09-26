//! Cost of keeping tracking areas and cursor rectangles up to date, for a
//! window of many views that each keep one of both the way AppKit programs
//! do (`updateTrackingAreas` replaces the area, `resetCursorRects` adds the
//! rectangle), in microseconds, the median of seven runs. Run in release
//! mode on macOS (AppKit) and on Linux (Sidestep) to compare:
//! `cargo run --release -p trackbench`.
//!
//! The window isn't shown: what's measured is the walk over the views and
//! the messages they get, not the pointer.

use std::cell::RefCell;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSCursor, NSResponder, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};

use sidestep as _;

#[derive(Default)]
struct RowIvars {
    area: RefCell<Option<Retained<NSTrackingArea>>>,
}

define_class!(
    /// A view that keeps a hover area and an I-beam rectangle over itself.
    #[unsafe(super(NSView, NSResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TrackBenchRow"]
    #[ivars = RowIvars]
    struct Row;

    impl Row {
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(old) = self.ivars().area.take() {
                self.removeTrackingArea(&old);
            }
            let options = NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    NSRect::ZERO,
                    options,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            self.ivars().area.replace(Some(area));
            // SAFETY: NSView's updateTrackingAreas takes nothing.
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::IBeamCursor());
        }
    }

    unsafe impl NSObjectProtocol for Row {}
);

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Median of seven timed runs of `f`, in microseconds, after a warm-up.
fn bench(name: &str, mut f: impl FnMut()) {
    f();
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    println!("{name:<52} {:>9.1} us  (min {:.1})", runs[3], runs[0]);
}

/// A window holding `groups` views of `rows` rows each.
fn window(
    mtm: MainThreadMarker,
    groups: usize,
    rows: usize,
) -> (Retained<NSWindow>, Retained<NSView>, Vec<Retained<Row>>) {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 800.0, 600.0),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 800.0, 600.0));
    let mut all = Vec::new();
    for g in 0..groups {
        let group = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, g as f64 * 20.0, 800.0, 20.0 * rows as f64));
        for r in 0..rows {
            let row: Retained<Row> = unsafe {
                msg_send![super(Row::alloc(mtm).set_ivars(RowIvars::default())), initWithFrame: rect(0.0, r as f64 * 20.0, 800.0, 20.0)]
            };
            group.addSubview(&row);
            all.push(row);
        }
        content.addSubview(&group);
    }
    window.setContentView(Some(&content));
    (window, content, all)
}

fn main() {
    let mtm = MainThreadMarker::new().expect("the main thread");
    for (groups, rows) in [(10, 100), (30, 100)] {
        let (window, content, rows_made) = window(mtm, groups, rows);
        let views = 1 + groups + rows_made.len();
        drop(rows_made);
        bench(&format!("{views} views: reset every cursor rectangle"), || window.resetCursorRects());
        let mut x = 0.0;
        bench(&format!("{views} views: move the content, update and reset all"), || {
            x = 1.0 - x;
            content.setFrameOrigin(NSPoint::new(x, 0.0));
            window.resetCursorRects();
        });
    }
}
