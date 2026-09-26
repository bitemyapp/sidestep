//! `NSAlert` without running it: defaults, buttons (their tags, key
//! equivalents and targets), the window it makes, and alerts from errors.
//! Running one, modally or as a sheet, shows windows: that part is opt-in
//! (`SIDESTEP_CONFORMANCE_WINDOWS=1`), and on macOS the application is
//! never activated.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, msg_send};
use objc2_app_kit::*;
use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer};

use sidestep as _;

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn is_kind(object: &AnyObject, class: &objc2::runtime::AnyClass) -> bool {
    // SAFETY: isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![object, isKindOfClass: class] }
}

fn titles(a: &NSAlert) -> Vec<String> {
    a.buttons().iter().map(|b| b.title().to_string()).collect()
}

fn defaults(mtm: MainThreadMarker) {
    let a = NSAlert::new(mtm);
    // An OK button until the program adds one.
    assert_eq!(titles(&a), ["OK"]);
    assert_eq!(a.alertStyle(), NSAlertStyle::Warning);
    assert_eq!(a.messageText().to_string(), "");
    assert_eq!(a.informativeText().to_string(), "");
    assert!(!a.showsSuppressionButton());
    assert!(!a.showsHelp());
    assert!(a.accessoryView().is_none());
    assert!(a.delegate().is_none());
    // The application's icon, until one is set.
    assert!(a.icon().is_some());
    // There before it shows, off.
    let suppression = a.suppressionButton().expect("a suppression button");
    assert_eq!(suppression.state(), 0);
    // The window is a panel, made before the alert runs, not on screen.
    let w = a.window();
    assert!(is_kind(&w, NSPanel::class()));
    assert!(!w.isVisible());
    assert_eq!(titles(&a), ["OK"]);
    a.setMessageText(&s("Message"));
    a.setInformativeText(&s("Informative"));
    a.setAlertStyle(NSAlertStyle::Critical);
    a.setShowsSuppressionButton(true);
    assert_eq!(a.messageText().to_string(), "Message");
    assert_eq!(a.informativeText().to_string(), "Informative");
    assert_eq!(a.alertStyle(), NSAlertStyle::Critical);
    assert!(a.showsSuppressionButton());
}

fn buttons(mtm: MainThreadMarker) {
    let a = NSAlert::new(mtm);
    let target: &AnyObject = (&*a as &NSObject).as_ref();
    let cases = [
        // The first button answers Return; Cancel answers Escape; Don't
        // Save answers Command-D; the rest nothing.
        ("Save", "\r", NSEventModifierFlags::empty()),
        ("Cancel", "\u{1b}", NSEventModifierFlags::empty()),
        ("Don't Save", "d", NSEventModifierFlags::Command),
        ("Other", "", NSEventModifierFlags::empty()),
    ];
    for (i, (title, key, mask)) in cases.into_iter().enumerate() {
        let b = a.addButtonWithTitle(&s(title));
        assert_eq!(b.title().to_string(), title);
        assert_eq!(b.tag(), 1000 + i as isize, "{title}");
        assert_eq!(b.keyEquivalent().to_string(), key, "{title}");
        assert_eq!(b.keyEquivalentModifierMask(), mask, "{title}");
        assert_eq!(b.bezelStyle(), NSBezelStyle::Push);
        // They act through the alert.
        assert!(b.target().is_some_and(|t| std::ptr::eq(&*t, target)));
        assert!(b.action().is_some());
    }
    assert_eq!(titles(&a), ["Save", "Cancel", "Don't Save", "Other"]);
    // Cancel answers Escape even first; the typographer's apostrophe works
    // too.
    let x = NSAlert::new(mtm);
    assert_eq!(x.addButtonWithTitle(&s("Cancel")).keyEquivalent().to_string(), "\u{1b}");
    let d = x.addButtonWithTitle(&s("Don\u{2019}t Save"));
    assert_eq!(
        (d.keyEquivalent().to_string(), d.keyEquivalentModifierMask()),
        ("d".into(), NSEventModifierFlags::Command)
    );
    // Laid out, every button is in the window, and one of them takes the
    // keyboard.
    a.setMessageText(&s("Save changes?"));
    a.layout();
    let w = a.window();
    for b in a.buttons().iter() {
        assert!(b.window().is_some_and(|bw| std::ptr::eq(&*bw, &*w)), "{}", b.title());
    }
    let first = w.initialFirstResponder().expect("a first responder");
    assert!(is_kind(&first, NSButton::class()));
    assert!(a.buttons().iter().any(|b| std::ptr::eq(&*first, &*b as &NSView)));
    assert_eq!(NSAlertFirstButtonReturn, 1000);
    assert_eq!(NSAlertSecondButtonReturn, 1001);
    assert_eq!(NSAlertThirdButtonReturn, 1002);
}

fn alerts_from_errors(mtm: MainThreadMarker) {
    // SAFETY: a domain, a code and no user info.
    let e = unsafe { objc2_foundation::NSError::errorWithDomain_code_userInfo(&s("Test"), 3, None) };
    let a = NSAlert::alertWithError(&e, mtm);
    assert_eq!(a.messageText().to_string(), e.localizedDescription().to_string());
    assert_eq!(a.informativeText().to_string(), "");
    assert_eq!(titles(&a), ["OK"]);
}

/// A timer that runs `f` once after `seconds`, in modal loops too.
fn soon(seconds: f64, f: impl Fn() + 'static) -> Retained<NSTimer> {
    let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(seconds, false, &block) };
    unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    timer
}

/// `runModal` returns the tag of the button clicked; the window leaves the
/// screen.
fn run_modal(mtm: MainThreadMarker) {
    let a = NSAlert::new(mtm);
    a.setMessageText(&s("Run"));
    a.addButtonWithTitle(&s("One"));
    a.addButtonWithTitle(&s("Two"));
    a.addButtonWithTitle(&s("Three"));
    let second = a.buttons().objectAtIndex(1);
    let _t = soon(0.2, move || unsafe { second.performClick(None) });
    assert_eq!(a.runModal(), NSAlertSecondButtonReturn);
    assert!(!a.window().isVisible());
    // Again, with another button.
    let third = a.buttons().objectAtIndex(2);
    let _t = soon(0.2, move || unsafe { third.performClick(None) });
    assert_eq!(a.runModal(), NSAlertThirdButtonReturn);
}

/// As a sheet, the handler gets the button's tag.
fn sheets(mtm: MainThreadMarker) {
    let parent = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(60.0, 60.0), NSSize::new(360.0, 240.0)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { parent.setReleasedWhenClosed(false) };
    parent.orderFront(None);
    let a = NSAlert::new(mtm);
    a.setMessageText(&s("Sheet"));
    a.addButtonWithTitle(&s("OK"));
    a.addButtonWithTitle(&s("Cancel"));
    let got = Rc::new(Cell::new(0isize));
    let g = got.clone();
    let handler = RcBlock::new(move |code: NSModalResponse| g.set(code));
    a.beginSheetModalForWindow_completionHandler(&parent, Some(&handler));
    assert!(parent.attachedSheet().is_some_and(|s| std::ptr::eq(&*s, &*a.window())));
    let cancel = a.buttons().objectAtIndex(1);
    unsafe { cancel.performClick(None) };
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while got.get() == 0 && std::time::Instant::now() < limit {
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        unsafe { NSRunLoop::currentRunLoop().runMode_beforeDate(objc2_foundation::NSDefaultRunLoopMode, &until) };
    }
    assert_eq!(got.get(), NSAlertSecondButtonReturn);
    assert!(parent.attachedSheet().is_none());
    parent.orderOut(None);
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let windows = std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_some();
    #[cfg(not(target_vendor = "apple"))]
    if windows && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        // SAFETY: nothing else runs yet to read the environment.
        unsafe { std::env::set_var("SIDESTEP_BACKEND", "null") };
    }
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let mut tests: Vec<Test> =
        vec![("defaults", defaults), ("buttons", buttons), ("alerts_from_errors", alerts_from_errors)];
    if windows {
        tests.extend([("run_modal", run_modal as fn(MainThreadMarker)), ("sheets", sheets)]);
    } else {
        println!("alert: running alerts skipped (SIDESTEP_CONFORMANCE_WINDOWS=1 runs them, which show windows)");
    }
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
