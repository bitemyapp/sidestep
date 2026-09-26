//! Cost of the controls' common operations, in nanoseconds per operation,
//! median of seven runs. Run in release mode on macOS (AppKit) and on
//! Linux (Sidestep) to compare: `cargo run --release -p controlbench`. An
//! argument runs only the cases whose names contain it.
//!
//! What's measured is what a window full of controls costs a program:
//! making labels and buttons, sizing them, changing their text and values.

use std::hint::black_box;
use std::time::Instant;

use objc2::rc::{Retained, autoreleasepool};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSButton, NSSegmentSwitchTracking, NSSegmentedControl, NSSlider, NSTextField};
use objc2_foundation::{NSArray, NSString};

use sidestep as _;

fn wanted(name: &str) -> bool {
    std::env::args().nth(1).is_none_or(|filter| name.contains(&filter))
}

fn bench(name: &str, iters: u64, mut f: impl FnMut(u64)) {
    if !wanted(name) {
        return;
    }
    for i in 0..iters / 10 {
        f(i);
    }
    let mut runs: Vec<f64> = (0..7)
        .map(|_| {
            autoreleasepool(|_| {
                let start = Instant::now();
                for i in 0..iters {
                    f(i);
                }
                start.elapsed().as_nanos() as f64 / iters as f64
            })
        })
        .collect();
    runs.sort_by(f64::total_cmp);
    println!("{name:<52} {:>10.1} ns  (min {:.1})", runs[3], runs[0]);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("the main thread");
    let _app = NSApplication::sharedApplication(mtm);
    // A hundred different texts, as a window's labels would have.
    let texts: Vec<Retained<NSString>> =
        (0..100).map(|i| NSString::from_str(&format!("Label number {i}, with some words"))).collect();

    bench("labelWithString: (sized to fit)", 2_000, |i| {
        let l = NSTextField::labelWithString(&texts[(i % 100) as usize], mtm);
        black_box(l);
    });
    let label = NSTextField::labelWithString(&texts[0], mtm);
    bench("label setStringValue: + intrinsicContentSize", 5_000, |i| {
        label.setStringValue(&texts[(i % 100) as usize]);
        black_box(label.intrinsicContentSize());
    });
    bench("label intrinsicContentSize, unchanged", 100_000, |_| {
        black_box(label.intrinsicContentSize());
    });
    bench("buttonWithTitle:target:action:", 2_000, |i| {
        // SAFETY: no target or action.
        let b = unsafe { NSButton::buttonWithTitle_target_action(&texts[(i % 100) as usize], None, None, mtm) };
        black_box(b);
    });
    // SAFETY: no target or action.
    let button = unsafe { NSButton::buttonWithTitle_target_action(&texts[0], None, None, mtm) };
    bench("button intrinsicContentSize, unchanged", 100_000, |_| {
        black_box(button.intrinsicContentSize());
    });
    bench("button setTitle: + intrinsicContentSize", 5_000, |i| {
        button.setTitle(&texts[(i % 100) as usize]);
        black_box(button.intrinsicContentSize());
    });
    bench("button setState: (checkbox)", 100_000, |i| {
        button.setState((i & 1) as isize);
    });
    let slider = NSSlider::initWithFrame(NSSlider::alloc(mtm), objc2_foundation::NSRect::ZERO);
    bench("slider setDoubleValue: + doubleValue", 100_000, |i| {
        slider.setDoubleValue((i % 100) as f64 / 100.0);
        black_box(slider.doubleValue());
    });
    let labels = NSArray::from_retained_slice(&[
        NSString::from_str("One"),
        NSString::from_str("Two"),
        NSString::from_str("Three"),
    ]);
    bench("segmentedControlWithLabels: (3 segments)", 2_000, |_| {
        // SAFETY: no target or action.
        let s = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &labels,
                NSSegmentSwitchTracking::SelectOne,
                None,
                None,
                mtm,
            )
        };
        black_box(s);
    });
    let field = NSTextField::labelWithString(&texts[0], mtm);
    bench("cell setIntValue: + stringValue", 100_000, |i| {
        field.setIntValue(i as i32);
        black_box(field.stringValue());
    });
}
