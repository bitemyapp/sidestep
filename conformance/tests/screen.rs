//! NSScreen: how screens relate to each other and to windows, which holds
//! on any desk. Screens themselves (how many, their sizes and scales)
//! differ from machine to machine, so nothing here depends on them, except
//! when `SIDESTEP_CONFORMANCE_SCREEN` names the one screen expected, in
//! pixels and its scale, as `scripts/headless-wayland` makes one on Linux:
//!
//! ```sh
//! SIZE=1920x1200 SCALE=1.5 scripts/linux-run sh -c \
//!   'SIDESTEP_CONFORMANCE_SCREEN=1920x1200@1.5 scripts/headless-wayland /target/debug/deps/screen-…'
//! ```
//!
//! Without a display (Linux without Wayland) there are no screens, and a
//! window has none.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSScreen, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSAlignmentOptions, NSPoint, NSRect, NSSize, NSString};

use sidestep as _;

fn inside(inner: NSRect, outer: NSRect) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

fn constants(_: MainThreadMarker) {
    use objc2_app_kit::*;
    // SAFETY: AppKit's constants live as long as the program.
    let names = unsafe {
        [
            (NSDeviceIsScreen, "NSDeviceIsScreen"),
            (NSDeviceColorSpaceName, "NSDeviceColorSpaceName"),
            (NSDeviceBitsPerSample, "NSDeviceBitsPerSample"),
            (NSDeviceResolution, "NSDeviceResolution"),
            (NSDeviceSize, "NSDeviceSize"),
            (NSWindowDidChangeScreenNotification, "NSWindowDidChangeScreenNotification"),
        ]
    };
    for (name, value) in names {
        assert_eq!(name.to_string(), value);
    }
}

fn screens_relate(mtm: MainThreadMarker) {
    // The program's first question about screens, before any window.
    let start = std::time::Instant::now();
    let screens = NSScreen::screens(mtm);
    println!("  first NSScreen.screens: {:.2} ms", start.elapsed().as_secs_f64() * 1e3);
    let main = NSScreen::mainScreen(mtm);
    if screens.count() == 0 {
        assert!(main.is_none());
        println!("  (no display: no screens)");
        return;
    }
    let first = screens.objectAtIndex(0);
    assert_eq!(first.frame().origin, NSPoint::new(0.0, 0.0));
    // With no key window, the main screen is the first, the same object.
    let main = main.expect("a main screen");
    assert!(std::ptr::eq(&*main, &*first));
    assert!(NSScreen::deepestScreen(mtm).is_some());
    // The same objects each time.
    let again = NSScreen::screens(mtm);
    assert_eq!(again.count(), screens.count());
    assert!(std::ptr::eq(&*again.objectAtIndex(0), &*first));
    for screen in screens.iter() {
        assert!(inside(screen.visibleFrame(), screen.frame()), "{:?} {:?}", screen.visibleFrame(), screen.frame());
        assert!(screen.backingScaleFactor() >= 1.0);
        assert!(screen.maximumFramesPerSecond() > 0);
        let s = screen.backingScaleFactor();
        let r = NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(3.0, 4.0));
        let backing = screen.convertRectToBacking(r);
        assert_eq!(backing, NSRect::new(NSPoint::new(s, 2.0 * s), NSSize::new(3.0 * s, 4.0 * s)));
        assert_eq!(screen.convertRectFromBacking(backing), r);
        let description = screen.deviceDescription();
        for key in [
            "NSDeviceIsScreen",
            "NSDeviceColorSpaceName",
            "NSDeviceBitsPerSample",
            "NSScreenNumber",
            "NSDeviceResolution",
            "NSDeviceSize",
        ] {
            let key = NSString::from_str(key);
            let value: Option<Retained<AnyObject>> = description.objectForKey(&key);
            assert!(value.is_some(), "{key}");
        }
    }
}

/// `backingAlignedRect:options:` puts the edges it's told about on whole
/// backing pixels, each rounded the way its option says: an edge inward or
/// outward of the rectangle, or to the nearest pixel, and a width or
/// height smaller or larger.
fn backing_alignment(mtm: MainThreadMarker) {
    let Some(screen) = NSScreen::mainScreen(mtm) else { return };
    let s = screen.backingScaleFactor();
    let r = NSRect::new(NSPoint::new(0.3, 0.6), NSSize::new(10.2, 10.7));
    let aligned = |options: NSAlignmentOptions| screen.backingAlignedRect_options(r, options);
    let rect =
        |x: f64, y: f64, max_x: f64, max_y: f64| NSRect::new(NSPoint::new(x, y), NSSize::new(max_x - x, max_y - y));
    let (floor, ceil, round) =
        (|v: f64| (v * s).floor() / s, |v: f64| (v * s).ceil() / s, |v: f64| (v * s).round() / s);
    let (max_x, max_y) = (0.3 + 10.2, 0.6 + 10.7);
    assert_eq!(
        aligned(NSAlignmentOptions::AlignAllEdgesOutward),
        rect(floor(0.3), floor(0.6), ceil(max_x), ceil(max_y))
    );
    assert_eq!(
        aligned(NSAlignmentOptions::AlignAllEdgesInward),
        rect(ceil(0.3), ceil(0.6), floor(max_x), floor(max_y))
    );
    assert_eq!(
        aligned(NSAlignmentOptions::AlignAllEdgesNearest),
        rect(round(0.3), round(0.6), round(max_x), round(max_y))
    );
    // An origin and a size: the size rounds as a length.
    let origin_and_size = aligned(
        NSAlignmentOptions::AlignMinXNearest
            | NSAlignmentOptions::AlignWidthInward
            | NSAlignmentOptions::AlignMinYNearest
            | NSAlignmentOptions::AlignHeightOutward,
    );
    assert_eq!(
        origin_and_size,
        NSRect::new(NSPoint::new(round(0.3), round(0.6)), NSSize::new(floor(10.2), ceil(10.7)))
    );
    // Halfway between two pixels, the nearest is the one above, below zero
    // too; flipped, y's is the one below.
    let (half, len) = (0.5 / s, 10.0 + 1.0 / s);
    let up = |v: f64| (v * s + 0.5).floor() / s;
    let down = |v: f64| (v * s - 0.5).ceil() / s;
    let ties = NSRect::new(NSPoint::new(-half, -half), NSSize::new(len, len));
    let nearest = screen.backingAlignedRect_options(ties, NSAlignmentOptions::AlignAllEdgesNearest);
    assert_eq!(nearest, rect(up(-half), up(-half), up(len - half), up(len - half)));
    assert_eq!(nearest.origin, NSPoint::new(0.0, 0.0));
    let ties = NSRect::new(NSPoint::new(half, half), NSSize::new(len, len));
    let flipped = screen.backingAlignedRect_options(
        ties,
        NSAlignmentOptions::AlignAllEdgesNearest | NSAlignmentOptions::AlignRectFlipped,
    );
    assert_eq!(flipped, rect(up(half), down(half), up(len + half), down(len + half)));
    assert_eq!(flipped.origin.y, 0.0);
}

fn windows_have_screens(mtm: MainThreadMarker) {
    // SAFETY: a titled window, never shown.
    let w = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(100.0, 100.0)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            true,
        )
    };
    // SAFETY: the test keeps its reference.
    unsafe { w.setReleasedWhenClosed(false) };
    match NSScreen::mainScreen(mtm) {
        // A window not on screen is on the main screen.
        Some(main) => {
            let screen = w.screen().expect("a screen");
            assert!(std::ptr::eq(&*screen, &*main));
            assert!(w.deepestScreen().is_some());
            // It draws at its screen's scale.
            let s = screen.backingScaleFactor();
            assert_eq!(w.backingScaleFactor(), s);
            let r = NSRect::new(NSPoint::new(1.0, 2.0), NSSize::new(3.0, 4.0));
            assert_eq!(w.convertRectToBacking(r), NSRect::new(NSPoint::new(s, 2.0 * s), NSSize::new(3.0 * s, 4.0 * s)));
        }
        None => assert!(w.screen().is_none()),
    }
}

/// The screen `scripts/headless-wayland` made, when it says which.
fn the_expected_screen(mtm: MainThreadMarker) {
    let Ok(expected) = std::env::var("SIDESTEP_CONFORMANCE_SCREEN") else { return };
    let (size, scale) = expected.split_once('@').expect("WIDTHxHEIGHT@SCALE");
    let (w, h) = size.split_once('x').expect("WIDTHxHEIGHT");
    let (w, h, scale): (f64, f64, f64) = (w.parse().unwrap(), h.parse().unwrap(), scale.parse().unwrap());
    let screens = NSScreen::screens(mtm);
    assert_eq!(screens.count(), 1);
    let screen = screens.objectAtIndex(0);
    assert_eq!(screen.frame(), NSRect::new(NSPoint::ZERO, NSSize::new(w / scale, h / scale)));
    assert_eq!(screen.backingScaleFactor(), scale);
    println!("  {:?} at {} ({})", screen.frame(), screen.backingScaleFactor(), screen.localizedName());
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let tests: &[Test] = &[
        ("constants", constants),
        ("screens_relate", screens_relate),
        ("backing_alignment", backing_alignment),
        ("windows_have_screens", windows_have_screens),
        ("the_expected_screen", the_expected_screen),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
