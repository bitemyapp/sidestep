//! Save and open panels and the workspace on Linux, end to end without a
//! portal: the session bus is out of reach, so panels fall back on
//! `zenity`, and URLs and files open with `xdg-open`. Stand-ins for both,
//! shell scripts put first on `PATH`, play the desktop's part: the
//! chooser prints what a `mode` file says (or waits, or cancels), and
//! `xdg-open` notes what it was asked to open. What Apple's AppKit does is
//! pinned by `conformance/tests/panels.rs`.
//!
//! AppKit belongs to the main thread, so this file has its own `main`, which
//! sets the environment up before anything reads it.

#[cfg(target_vendor = "apple")]
fn main() {}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    linux::main();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSApplication, NSModalResponse, NSModalResponseAbort, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel,
        NSRunningApplication, NSSavePanel, NSWindow, NSWorkspace, NSWorkspaceOpenConfiguration,
    };
    use objc2_foundation::{NSError, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer, NSURL};
    use sidestep_appkit::testing;

    /// The stand-ins' folder.
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("sidestep-panels-{}", std::process::id()))
    }

    fn s(text: &str) -> Retained<NSString> {
        NSString::from_str(text)
    }

    /// What the stand-in `zenity` does next: `wait`, `cancel`, or print
    /// the text.
    fn chooser_will(mode: &str) {
        std::fs::write(root().join("mode"), mode).expect("the mode");
        let _ = std::fs::remove_file(root().join("pid"));
    }

    /// What the stand-in `xdg-open` was asked to open.
    fn opened() -> Vec<String> {
        std::fs::read_to_string(root().join("opened")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    /// Run the main loop until `done`, for at most five seconds.
    fn wait_for(done: impl Fn() -> bool) -> bool {
        let limit = Instant::now() + Duration::from_secs(5);
        while !done() && Instant::now() < limit {
            testing::run_for(20);
        }
        done()
    }

    /// Run `f` once after `ms` milliseconds, in any common mode (modal
    /// loops too).
    fn after(ms: u64, f: impl Fn() + 'static) -> Retained<NSTimer> {
        let block = RcBlock::new(move |_: NonNull<NSTimer>| f());
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(ms as f64 / 1000.0, false, &block) };
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        timer
    }

    /// The stand-in chooser's process, once it waits.
    fn chooser_pid() -> Option<u32> {
        std::fs::read_to_string(root().join("pid")).ok()?.trim().parse().ok()
    }

    fn running(pid: u32) -> bool {
        // A process killed and waited for is gone from /proc; one killed
        // and not waited for lingers as a zombie ('Z').
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| stat.rsplit(')').next().and_then(|rest| rest.split_whitespace().next()) != Some("Z"))
    }

    /// A scheme the desktop's installed programs name a handler for (in
    /// `mimeinfo.cache`) opens at the first call, before anything else
    /// asked; one nothing handles doesn't.
    fn schemes_from_installed_programs(_mtm: MainThreadMarker) {
        let w = NSWorkspace::sharedWorkspace();
        let url = NSURL::URLWithString(&s("sidestep-test:hello")).expect("a URL");
        assert!(w.openURL(&url));
        let unknown = NSURL::URLWithString(&s("sidestep-nothing:hello")).expect("a URL");
        assert!(!w.openURL(&unknown));
        // Without a portal, xdg-open opens it.
        assert!(wait_for(|| opened().contains(&"sidestep-test:hello".to_owned())), "{:?}", opened());
    }

    /// A program opening a file may run until it exits; the next URL
    /// doesn't wait for it. A file that isn't there opens nothing.
    fn files_open_without_waiting(_mtm: MainThreadMarker) {
        let w = NSWorkspace::sharedWorkspace();
        let (slow, next) = (root().join("block.txt"), root().join("next.txt"));
        std::fs::write(&slow, "").expect("a file");
        std::fs::write(&next, "").expect("a file");
        #[allow(deprecated)]
        let slow_opened = w.openFile(&s(&slow.to_string_lossy()));
        assert!(slow_opened);
        assert!(w.openURL(&NSURL::fileURLWithPath(&s(&next.to_string_lossy()))));
        let want = format!("file://{}", next.to_string_lossy());
        assert!(wait_for(|| opened().contains(&want)), "{:?}", opened());
        assert!(!w.openURL(&NSURL::fileURLWithPath(&s("/nonexistent-sidestep/x.txt"))));
        #[allow(deprecated)]
        let missing = w.openFile(&s("/nonexistent-sidestep/x.txt"));
        assert!(!missing);
    }

    /// What a completion handler heard: the error's code, and whether it
    /// ran off the main thread.
    type Outcome = (Option<isize>, bool);

    /// The handler of `openURL:configuration:completionHandler:` runs later,
    /// off the main thread; an error only when nothing opens the URL.
    fn handlers_run_later(_mtm: MainThreadMarker) {
        let w = NSWorkspace::sharedWorkspace();
        let c = NSWorkspaceOpenConfiguration::configuration();
        let main = std::thread::current().id();
        for (url, want) in [("sidestep-test:later", None), ("sidestep-nothing:later", Some(256))] {
            let got: Arc<Mutex<Option<Outcome>>> = Arc::new(Mutex::new(None));
            let g = got.clone();
            let handler = RcBlock::new(move |_app: *mut NSRunningApplication, error: *mut NSError| {
                let code = unsafe { error.as_ref() }.map(|e| e.code());
                *g.lock().unwrap() = Some((code, std::thread::current().id() != main));
            });
            let url = NSURL::URLWithString(&s(url)).expect("a URL");
            w.openURL_configuration_completionHandler(&url, &c, Some(&handler));
            assert!(wait_for(|| got.lock().unwrap().is_some()));
            assert_eq!(*got.lock().unwrap(), Some((want, true)), "{url:?}");
        }
    }

    /// `runModal` is modal: the panel is the modal window while the chooser
    /// is up, and `cancel:` ends it with Cancel and kills the chooser.
    fn run_modal_is_modal(mtm: MainThreadMarker) {
        chooser_will("wait");
        let p = NSSavePanel::savePanel(mtm);
        let seen = Rc::new(Cell::new(false));
        let (p2, s2) = (p.clone(), seen.clone());
        let _t = after(10, move || {
            let app = NSApplication::sharedApplication(MainThreadMarker::new().expect("the main thread"));
            // Once the chooser runs.
            let limit = Instant::now() + Duration::from_secs(5);
            while chooser_pid().is_none() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(10));
            }
            s2.set(app.modalWindow().is_some_and(|m| std::ptr::eq(&*m, &**p2 as &NSWindow)) && p2.isVisible());
            unsafe { p2.cancel(None) };
        });
        assert_eq!(p.runModal(), NSModalResponseCancel);
        assert!(seen.get());
        assert!(!p.isVisible());
        assert!(NSApplication::sharedApplication(mtm).modalWindow().is_none());
        let pid = chooser_pid().expect("the chooser ran");
        assert!(wait_for(|| !running(pid)), "the chooser is killed");

        // Ended by the program some other way, the chooser goes too.
        chooser_will("wait");
        let _t = after(10, move || {
            let limit = Instant::now() + Duration::from_secs(5);
            while chooser_pid().is_none() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(10));
            }
            NSApplication::sharedApplication(MainThreadMarker::new().expect("the main thread")).abortModal();
        });
        assert_eq!(p.runModal(), NSModalResponseAbort);
        let pid = chooser_pid().expect("the chooser ran");
        assert!(wait_for(|| !running(pid)), "the chooser is killed");
    }

    /// The chooser's answer: the files, or Cancel.
    fn run_modal_answers(mtm: MainThreadMarker) {
        let file = root().join("a b.txt");
        chooser_will(&file.to_string_lossy());
        let p = NSSavePanel::savePanel(mtm);
        assert_eq!(p.runModal(), NSModalResponseOK);
        let path = p.URL().and_then(|u| u.path()).map(|p| p.to_string());
        assert_eq!(path.as_deref(), Some(&*file.to_string_lossy()));

        let (a, b) = (root().join("a.txt"), root().join("b.txt"));
        chooser_will(&format!("{}\n{}", a.to_string_lossy(), b.to_string_lossy()));
        let o = NSOpenPanel::openPanel(mtm);
        o.setAllowsMultipleSelection(true);
        assert_eq!(o.runModal(), NSModalResponseOK);
        let paths: Vec<String> = o.URLs().iter().filter_map(|u| u.path()).map(|p| p.to_string()).collect();
        assert_eq!(paths, [a.to_string_lossy(), b.to_string_lossy()]);

        chooser_will("cancel");
        assert_eq!(NSSavePanel::savePanel(mtm).runModal(), NSModalResponseCancel);
    }

    /// Begun without a loop, the panel is up but not modal; `cancel:`
    /// answers the handler at once, and `ok:` a save panel's with OK.
    fn begun_panels(mtm: MainThreadMarker) {
        for (ok, want) in [(false, NSModalResponseCancel), (true, NSModalResponseOK)] {
            chooser_will("wait");
            let got: Rc<Cell<NSModalResponse>> = Rc::new(Cell::new(-99));
            let g = got.clone();
            let handler = RcBlock::new(move |code: NSModalResponse| g.set(code));
            let q = NSSavePanel::savePanel(mtm);
            q.beginWithCompletionHandler(&handler);
            assert!(q.isVisible());
            assert!(NSApplication::sharedApplication(mtm).modalWindow().is_none());
            assert!(wait_for(|| chooser_pid().is_some()));
            if ok {
                unsafe { q.ok(None) };
            } else {
                unsafe { q.cancel(None) };
            }
            assert_eq!(got.get(), want);
            assert!(!q.isVisible());
            let pid = chooser_pid().expect("the chooser ran");
            assert!(wait_for(|| !running(pid)), "the chooser is killed");
        }
    }

    type Test = (&'static str, fn(MainThreadMarker));

    /// The stand-ins, and an environment that finds them and no portal.
    fn set_up() {
        let root = root();
        let bin = root.join("bin");
        let apps = root.join("share/applications");
        std::fs::create_dir_all(&bin).expect("a folder");
        std::fs::create_dir_all(&apps).expect("a folder");
        let script = |name: &str, text: &str| {
            let path = bin.join(name);
            std::fs::write(&path, text).expect("a script");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("executable");
        };
        script(
            "zenity",
            "#!/bin/sh\ndir=$(dirname \"$0\")/..\nmode=$(cat \"$dir/mode\")\ncase \"$mode\" in\n  wait) echo $$ > \"$dir/pid\"; exec sleep 60 ;;\n  cancel) exit 1 ;;\n  *) printf '%s\\n' \"$mode\" ;;\nesac\n",
        );
        script(
            "xdg-open",
            "#!/bin/sh\ndir=$(dirname \"$0\")/..\necho \"$1\" >> \"$dir/opened\"\ncase \"$1\" in *block*) exec sleep 20 ;; esac\n",
        );
        std::fs::write(apps.join("mimeinfo.cache"), "[MIME Cache]\nx-scheme-handler/sidestep-test=Test.desktop;\n")
            .expect("a cache");
        let path = format!("{}:{}", bin.to_string_lossy(), std::env::var("PATH").unwrap_or_default());
        let vars = [
            ("PATH", path),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent-sidestep/bus".to_owned()),
            ("XDG_DATA_DIRS", root.join("share").to_string_lossy().into_owned()),
            ("XDG_DATA_HOME", root.join("data").to_string_lossy().into_owned()),
            ("XDG_CONFIG_HOME", root.join("config").to_string_lossy().into_owned()),
            ("XDG_CONFIG_DIRS", root.join("config-dirs").to_string_lossy().into_owned()),
        ];
        for (name, value) in vars {
            // SAFETY: nothing else runs yet to read the environment.
            unsafe { std::env::set_var(name, value) };
        }
    }

    pub(crate) fn main() {
        let mtm = MainThreadMarker::new().expect("runs on the main thread");
        set_up();
        testing::use_null_backend();
        let tests: &[Test] = &[
            ("schemes_from_installed_programs", schemes_from_installed_programs),
            ("files_open_without_waiting", files_open_without_waiting),
            ("handlers_run_later", handlers_run_later),
            ("run_modal_is_modal", run_modal_is_modal),
            ("run_modal_answers", run_modal_answers),
            ("begun_panels", begun_panels),
        ];
        let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
        for (name, test) in tests {
            if only.as_deref().is_some_and(|o| !name.contains(o)) {
                continue;
            }
            objc2::rc::autoreleasepool(|_| test(mtm));
            println!("test {name} ... ok");
        }
        let _ = std::fs::remove_dir_all(root());
    }
}
