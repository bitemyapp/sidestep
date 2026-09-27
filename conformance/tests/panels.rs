//! Save and open panels, the workspace and the running application, without
//! showing a panel: defaults, what the workspace answers about this program,
//! and what it does with URLs nothing opens. Running a panel shows the
//! desktop's file chooser, which a test can't answer, but can cancel: on
//! macOS that part is opt-in (`SIDESTEP_CONFORMANCE_WINDOWS=1`) and never
//! activates the application. On Linux it needs a chooser the test can
//! count on, so `sidestep-appkit`'s `tests/panels.rs` runs it with a
//! stand-in, and its portal tests check what the panels ask the portal and
//! what they make of its answers.
//!
//! AppKit belongs to the main thread, so this file has its own `main`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, MainThreadMarker, msg_send};
use objc2_app_kit::*;
use std::sync::{Arc, Mutex};

use objc2_foundation::{NSCopying, NSString, NSURL};

use sidestep as _;

fn s(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn is_kind(object: &AnyObject, class: &objc2::runtime::AnyClass) -> bool {
    // SAFETY: isKindOfClass: takes a class and returns BOOL.
    unsafe { msg_send![object, isKindOfClass: class] }
}

fn save_panels(mtm: MainThreadMarker) {
    let p = NSSavePanel::savePanel(mtm);
    // A panel, and so a window, not on screen.
    assert!(is_kind(&p, NSPanel::class()) && is_kind(&p, NSWindow::class()));
    assert!(!p.isVisible());
    assert_eq!(p.title().to_string(), "Save");
    assert_eq!(p.prompt().to_string(), "Save");
    assert_eq!(p.nameFieldLabel().to_string(), "Save As:");
    assert_eq!(p.nameFieldStringValue().to_string(), "Untitled");
    assert_eq!(p.message().to_string(), "");
    assert!(p.canCreateDirectories());
    assert!(!p.showsHiddenFiles());
    assert!(!p.allowsOtherFileTypes());
    assert!(p.isExtensionHidden());
    assert!(!p.canSelectHiddenExtension());
    assert!(!p.treatsFilePackagesAsDirectories());
    assert!(!p.isExpanded());
    assert!(unsafe { p.delegate() }.is_none());
    assert!(p.accessoryView().is_none());
    #[allow(deprecated)]
    let types = p.allowedFileTypes();
    assert!(types.is_none());
    // A folder to start in, and a URL in it named as the name field says.
    let dir = p.directoryURL().expect("a folder");
    assert!(dir.isFileURL());
    let url = p.URL().expect("a URL");
    assert_eq!(url.lastPathComponent().map(|c| c.to_string()).as_deref(), Some("Untitled"));
    p.setNameFieldStringValue(&s("Notes.txt"));
    assert_eq!(p.nameFieldStringValue().to_string(), "Notes.txt");
    p.setPrompt(Some(&s("Export")));
    p.setMessage(Some(&s("Where to?")));
    assert_eq!(p.prompt().to_string(), "Export");
    assert_eq!(p.message().to_string(), "Where to?");
    // Each call makes a new panel.
    let q = NSSavePanel::savePanel(mtm);
    assert!(!std::ptr::eq(&*p, &*q));
}

fn open_panels(mtm: MainThreadMarker) {
    let o = NSOpenPanel::openPanel(mtm);
    assert!(is_kind(&o, NSSavePanel::class()));
    assert_eq!(o.title().to_string(), "Open");
    assert_eq!(o.prompt().to_string(), "Open");
    assert!(o.canChooseFiles());
    assert!(!o.canChooseDirectories());
    assert!(!o.allowsMultipleSelection());
    assert!(o.resolvesAliases());
    assert!(!o.canCreateDirectories());
    assert_eq!(o.URLs().count(), 0);
    assert!(o.URL().is_none());
    o.setCanChooseDirectories(true);
    o.setCanChooseFiles(false);
    o.setAllowsMultipleSelection(true);
    assert!(o.canChooseDirectories() && !o.canChooseFiles() && o.allowsMultipleSelection());
}

fn the_workspace(_mtm: MainThreadMarker) {
    let w = NSWorkspace::sharedWorkspace();
    assert!(std::ptr::eq(&*w, &*NSWorkspace::sharedWorkspace()));
    // A notification center of its own.
    let center = w.notificationCenter();
    assert!(!std::ptr::eq(&*center, &*objc2_foundation::NSNotificationCenter::defaultCenter()));
    assert!(std::ptr::eq(&*center, &*w.notificationCenter()));
    if opening_allowed() {
        // Nothing opens a scheme nobody handles, nor a URL without a scheme.
        let bad = NSURL::URLWithString(&s("nosuchscheme-sidestep://x")).expect("a URL");
        assert!(!w.openURL(&bad));
        let relative = NSURL::URLWithString(&s("just/a/path")).expect("a URL");
        assert!(!w.openURL(&relative));
        // A file that isn't there opens nothing.
        let missing = NSURL::fileURLWithPath(&s("/nonexistent-sidestep/x.txt"));
        assert!(!w.openURL(&missing));
        #[allow(deprecated)]
        let opened = w.openFile(&s("/nonexistent-sidestep/x.txt"));
        assert!(!opened);
    }
    // Settings the desktop decides: asked, not pinned.
    let _ = (w.accessibilityDisplayShouldReduceMotion(), w.accessibilityDisplayShouldIncreaseContrast());
}

fn open_configurations(_mtm: MainThreadMarker) {
    let c = NSWorkspaceOpenConfiguration::configuration();
    assert!(c.promptsUserIfNeeded() && c.addsToRecentItems() && c.activates());
    assert!(!c.hides() && !c.hidesOthers() && !c.isForPrinting() && !c.createsNewApplicationInstance());
    assert!(c.allowsRunningApplicationSubstitution() && !c.requiresUniversalLinks());
    assert_eq!((c.arguments().count(), c.environment().count()), (0, 0));
    // A new one each time; copies keep what was set.
    assert!(!std::ptr::eq(&*c, &*NSWorkspaceOpenConfiguration::configuration()));
    c.setActivates(false);
    c.setPromptsUserIfNeeded(false);
    c.setArguments(&objc2_foundation::NSArray::from_retained_slice(&[s("--a")]));
    let copy = c.copy();
    assert!(!std::ptr::eq(&*c, &*copy));
    assert!(!copy.activates() && !copy.promptsUserIfNeeded() && copy.addsToRecentItems());
    assert_eq!(copy.arguments().count(), 1);
    assert!(!NSWorkspaceOpenConfiguration::new().hides());
}

/// Whether the tests may ask the system to open URLs and files. On macOS
/// a URL nothing handles puts up an alert ("There is no application set to
/// open the URL …") that stays until someone dismisses it, one per run, so
/// there it takes SIDESTEP_CONFORMANCE_OPEN_URLS=1.
fn opening_allowed() -> bool {
    !cfg!(target_vendor = "apple") || std::env::var_os("SIDESTEP_CONFORMANCE_OPEN_URLS").is_some()
}

/// Run the main loop until `done`, for at most ten seconds.
fn wait_for(done: impl Fn() -> bool) {
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done() && std::time::Instant::now() < limit {
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05);
        unsafe {
            objc2_foundation::NSRunLoop::currentRunLoop()
                .runMode_beforeDate(objc2_foundation::NSDefaultRunLoopMode, &until)
        };
    }
}

/// What a completion handler heard: whether the application was nil, the
/// error's code, and whether it ran off the main thread.
type Outcome = (bool, Option<isize>, bool);

/// The handler of `openURL:configuration:completionHandler:` runs later,
/// off the main thread, with no application and an error when nothing
/// opens the URL (256), or the file isn't there (260).
fn opening_with_handlers(_mtm: MainThreadMarker) {
    let w = NSWorkspace::sharedWorkspace();
    let c = NSWorkspaceOpenConfiguration::configuration();
    // Nothing to ask the user, nothing to bring forward.
    c.setPromptsUserIfNeeded(false);
    c.setActivates(false);
    let main = std::thread::current().id();
    for (url, code) in [
        (NSURL::URLWithString(&s("nosuchscheme-sidestep://x")).expect("a URL"), 256),
        (NSURL::fileURLWithPath(&s("/nonexistent-sidestep/x.txt")), 260),
    ] {
        let got: Arc<Mutex<Option<Outcome>>> = Arc::new(Mutex::new(None));
        let g = got.clone();
        let handler =
            block2::RcBlock::new(move |app: *mut NSRunningApplication, error: *mut objc2_foundation::NSError| {
                let error = unsafe { error.as_ref() }.map(|e| {
                    assert_eq!(e.domain().to_string(), "NSCocoaErrorDomain");
                    e.code()
                });
                *g.lock().unwrap() = Some((app.is_null(), error, std::thread::current().id() != main));
            });
        // Off the main thread, so not inside the call.
        w.openURL_configuration_completionHandler(&url, &c, Some(&handler));
        wait_for(|| got.lock().unwrap().is_some());
        assert_eq!(*got.lock().unwrap(), Some((true, Some(code), true)), "{url:?}");
    }
}

/// `runModal` is a modal loop for the panel; `cancel:` ends it with Cancel,
/// and ends a panel begun without a loop at once.
#[cfg(target_vendor = "apple")]
fn cancelling_panels(mtm: MainThreadMarker) {
    let p = NSSavePanel::savePanel(mtm);
    let seen = std::rc::Rc::new(std::cell::Cell::new(false));
    let (p2, s2) = (p.clone(), seen.clone());
    let block = block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_foundation::NSTimer>| {
        let app = NSApplication::sharedApplication(MainThreadMarker::new().expect("the main thread"));
        s2.set(app.modalWindow().is_some_and(|m| std::ptr::eq(&*m, &**p2 as &NSWindow)) && p2.isVisible());
        unsafe { p2.cancel(None) };
    });
    let timer = unsafe { objc2_foundation::NSTimer::timerWithTimeInterval_repeats_block(0.5, false, &block) };
    unsafe {
        objc2_foundation::NSRunLoop::currentRunLoop().addTimer_forMode(&timer, objc2_foundation::NSRunLoopCommonModes)
    };
    assert_eq!(p.runModal(), NSModalResponseCancel);
    assert!(seen.get());
    assert!(!p.isVisible());
    // Begun, it is up but not modal; cancel: answers at once.
    let got = std::rc::Rc::new(std::cell::Cell::new(-99));
    let g = got.clone();
    let handler = block2::RcBlock::new(move |code: NSModalResponse| g.set(code));
    let q = NSSavePanel::savePanel(mtm);
    q.beginWithCompletionHandler(&handler);
    assert!(q.isVisible());
    assert!(NSApplication::sharedApplication(mtm).modalWindow().is_none());
    unsafe { q.cancel(None) };
    assert_eq!(got.get(), NSModalResponseCancel);
    wait_for(|| !q.isVisible());
    assert!(!q.isVisible());
}

fn the_running_application(mtm: MainThreadMarker) {
    let r = NSRunningApplication::currentApplication();
    assert_eq!(r.processIdentifier(), std::process::id() as i32);
    // Not bundled: the executable's name, no identifier.
    let exe = std::env::current_exe().expect("an executable");
    let name = exe.file_name().expect("a name").to_string_lossy().into_owned();
    assert_eq!(r.localizedName().map(|n| n.to_string()), Some(name.clone()));
    assert!(r.bundleIdentifier().is_none());
    assert_eq!(r.executableURL().and_then(|u| u.lastPathComponent()).map(|c| c.to_string()), Some(name));
    assert!(!r.isTerminated());
    let app = NSApplication::sharedApplication(mtm);
    assert_eq!(r.activationPolicy(), app.activationPolicy());
    // Never activated here.
    assert!(!r.isActive());
    let found = NSRunningApplication::runningApplicationWithProcessIdentifier(std::process::id() as i32);
    assert!(found.is_some_and(|f| f.processIdentifier() == r.processIdentifier()));
}

type Test = (&'static str, fn(MainThreadMarker));

fn main() {
    let mtm = MainThreadMarker::new().expect("runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let mut tests: Vec<Test> = vec![
        ("save_panels", save_panels),
        ("open_panels", open_panels),
        ("the_workspace", the_workspace),
        ("open_configurations", open_configurations),
        ("the_running_application", the_running_application),
    ];
    if opening_allowed() {
        tests.push(("opening_with_handlers", opening_with_handlers));
    } else {
        println!(
            "panels: opening URLs skipped (SIDESTEP_CONFORMANCE_OPEN_URLS=1 runs it, which leaves macOS alerts up)"
        );
    }
    #[cfg(target_vendor = "apple")]
    if std::env::var_os("SIDESTEP_CONFORMANCE_WINDOWS").is_some() {
        tests.push(("cancelling_panels", cancelling_panels));
    } else {
        println!("panels: running panels skipped (SIDESTEP_CONFORMANCE_WINDOWS=1 runs them, which show panels)");
    }
    for (name, test) in tests {
        objc2::rc::autoreleasepool(|_| test(mtm));
        println!("test {name} ... ok");
    }
}
