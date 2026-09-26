//! NSScreen: how screens relate to each other and to windows, which holds
//! on any desk. Screens themselves (how many, their sizes and scales)
//! differ from machine to machine, so nothing here depends on them, except
//! when `SIDESTEP_CONFORMANCE_SCREEN` names the one screen expected
//! (`1280x800@1`), as `scripts/headless-wayland` sets one up on Linux.
//! Without a display (Linux without Wayland) there are no screens, and a
//! window has none.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSScreen, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

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
    let screens = NSScreen::screens(mtm);
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
        ("windows_have_screens", windows_have_screens),
        ("the_expected_screen", the_expected_screen),
    ];
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
