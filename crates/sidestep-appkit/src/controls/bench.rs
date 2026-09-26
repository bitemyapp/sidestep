//! What a window of controls costs to draw: recording 100 push buttons,
//! 100 check boxes and 100 labels through `drawRect:`, as a display pass
//! does, with their text layouts cached (as they are after the first
//! frame). Medians of seven runs, in microseconds per control; run in
//! release mode:
//!
//! ```sh
//! scripts/linux-cargo test --release -p sidestep-appkit bench_controls -- --ignored --nocapture
//! ```

use std::hint::black_box;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::{NSButton, NSButtonType, NSTextField, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::graphics::{self, Xf};
use crate::protocol::Rect;

/// Median microseconds per control of drawing `controls`, over seven runs
/// of `iters`.
fn median(iters: u32, controls: &[Retained<NSView>]) -> f64 {
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iters {
                black_box(record(controls));
            }
            start.elapsed().as_secs_f64() * 1e6 / f64::from(iters) / controls.len() as f64
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    runs[3]
}

/// Record every control's `drawRect:`, as a display pass would; the number
/// of ops recorded.
fn record(controls: &[Retained<NSView>]) -> usize {
    graphics::begin_recording();
    for view in controls {
        let bounds = view.bounds();
        let clip = Rect::new(0.0, 0.0, bounds.size.width as f32, bounds.size.height as f32);
        graphics::set_view(Xf::IDENTITY, clip);
        // SAFETY: drawRect: takes the rect to draw.
        let _: () = unsafe { msg_send![view, drawRect: bounds] };
    }
    graphics::end_recording().len()
}

#[test]
#[ignore]
fn bench_controls_drawing() {
    // The test harness runs this on a thread of its own, not the process's
    // first.
    // SAFETY: every AppKit object here is made, used and dropped on this
    // one thread, and Sidestep's AppKit keeps its state (views, text
    // layouts, the recorder) in the current thread's locals, so nothing
    // here touches state another thread owns. The bench is ignored unless
    // asked for, and runs no event loop.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let frame = NSRect::new(NSPoint::ZERO, NSSize::new(160.0, 24.0));
    let title = |i: usize| NSString::from_str(&format!("Control number {i}"));
    let mut buttons = Vec::new();
    let mut checks = Vec::new();
    let mut labels = Vec::new();
    // Made as the factories make them (which check the thread themselves).
    for i in 0..100 {
        let b = NSButton::initWithFrame(NSButton::alloc(mtm), frame);
        b.setTitle(&title(i));
        buttons.push(Retained::into_super(Retained::into_super(b)));
        let c = NSButton::initWithFrame(NSButton::alloc(mtm), frame);
        c.setButtonType(NSButtonType::Switch);
        c.setTitle(&title(i));
        checks.push(Retained::into_super(Retained::into_super(c)));
        let l = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
        l.setEditable(false);
        l.setSelectable(false);
        l.setBezeled(false);
        l.setDrawsBackground(false);
        l.setStringValue(&title(i));
        labels.push(Retained::into_super(Retained::into_super(l)));
    }
    let all: Vec<Retained<NSView>> = buttons.iter().chain(&checks).chain(&labels).cloned().collect();
    // The first frame lays the text out.
    assert!(record(&all) > 0);
    for (name, controls) in [("push buttons", &buttons), ("check boxes", &checks), ("labels", &labels)] {
        println!("draw {name:<14} {:>8.3} µs per control", median(50, controls));
    }
    println!("draw {:<14} {:>8.3} µs per control", "all", median(50, &all));
}
