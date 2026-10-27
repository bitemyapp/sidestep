//! Linux only: the desktop's default applications through NSWorkspace,
//! from desktop entries and `mimeapps.list` files in throwaway XDG
//! directories. (On macOS, Launch Services answers, from installed
//! bundles, and setting a default asks the user.)
//!
//! The process's environment points at the throwaway directories, so this
//! file has its own `main` and nothing else runs beside it.

#[cfg(target_vendor = "apple")]
fn main() {}

#[cfg(not(target_vendor = "apple"))]
fn main() {
    linux::main();
}

#[cfg(not(target_vendor = "apple"))]
mod linux {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::{NSError, NSString, NSURL};
    use objc2_uniform_type_identifiers::UTType;

    use sidestep as _;

    fn url(path: &Path) -> objc2::rc::Retained<NSURL> {
        NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
    }

    fn path_of(url: Option<objc2::rc::Retained<NSURL>>) -> Option<String> {
        url.and_then(|u| u.path()).map(|p| p.to_string())
    }

    /// The error code a completion handler heard, if any, waiting for it.
    fn heard(rx: &mpsc::Receiver<Option<isize>>) -> Option<isize> {
        rx.recv_timeout(Duration::from_secs(10)).expect("the completion handler ran")
    }

    pub fn main() {
        let root = std::env::temp_dir().join(format!("sidestep-defaults-{}", std::process::id()));
        let (data, config, empty) = (root.join("data"), root.join("config"), root.join("empty"));
        let applications = data.join("applications");
        for dir in [&applications, &config, &empty] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let entry = |name: &str, exec: &str| {
            let text =
                format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nMimeType=text/markdown;\n");
            std::fs::write(applications.join(format!("{name}.desktop")), text).unwrap();
        };
        entry("test-viewer", "sh -c \"exit 0\" %F");
        entry("test-editor", "sh -c \"exit 0\" %U");
        std::fs::write(
            applications.join("mimeinfo.cache"),
            "[MIME Cache]\ntext/markdown=test-viewer.desktop;test-editor.desktop;\n",
        )
        .unwrap();
        // SAFETY: nothing else in this process reads the environment yet.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &data);
            std::env::set_var("XDG_DATA_DIRS", &empty);
            std::env::set_var("XDG_CONFIG_HOME", &config);
            std::env::set_var("XDG_CONFIG_DIRS", &empty);
            std::env::remove_var("XDG_CURRENT_DESKTOP");
        }
        let workspace = NSWorkspace::sharedWorkspace();
        let markdown = UTType::typeWithMIMEType(&NSString::from_str("text/markdown")).expect("Markdown's type");
        let viewer = applications.join("test-viewer.desktop").to_string_lossy().into_owned();
        let editor = applications.join("test-editor.desktop").to_string_lossy().into_owned();

        // With no default, the first application that declares the type.
        assert_eq!(path_of(workspace.URLForApplicationToOpenContentType(&markdown)), Some(viewer.clone()));
        let document = root.join("notes.md");
        std::fs::write(&document, "# Notes\n").unwrap();
        assert_eq!(path_of(workspace.URLForApplicationToOpenURL(&url(&document))), Some(viewer.clone()));
        let web = NSURL::URLWithString(&NSString::from_str("https://example.com/")).unwrap();
        assert_eq!(path_of(workspace.URLForApplicationToOpenURL(&web)), None, "nothing handles https here");

        // Setting the default writes the user's mimeapps.list.
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send(unsafe { error.as_ref() }.map(|e| e.code()));
        });
        workspace.setDefaultApplicationAtURL_toOpenContentType_completionHandler(
            &url(Path::new(&editor)),
            &markdown,
            Some(&done),
        );
        assert_eq!(heard(&rx), None);
        let list = std::fs::read_to_string(config.join("mimeapps.list")).unwrap();
        assert!(list.contains("[Default Applications]\ntext/markdown=test-editor.desktop;"), "{list}");
        assert_eq!(path_of(workspace.URLForApplicationToOpenContentType(&markdown)), Some(editor.clone()));
        assert_eq!(path_of(workspace.URLForApplicationToOpenURL(&url(&document))), Some(editor.clone()));
        // Something that isn't an application can't be one.
        workspace.setDefaultApplicationAtURL_toOpenContentType_completionHandler(
            &url(&document),
            &markdown,
            Some(&done),
        );
        assert_eq!(heard(&rx), Some(4));

        // Starting an application by its desktop entry, or nothing there.
        let (tx, rx) = mpsc::channel();
        let started = RcBlock::new(move |_: *mut NSRunningApplication, error: *mut NSError| {
            let _ = tx.send(unsafe { error.as_ref() }.map(|e| e.code()));
        });
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        workspace.openApplicationAtURL_configuration_completionHandler(
            &url(Path::new(&viewer)),
            &configuration,
            Some(&started),
        );
        assert_eq!(heard(&rx), None);
        workspace.openApplicationAtURL_configuration_completionHandler(
            &url(&root.join("gone.desktop")),
            &configuration,
            Some(&started),
        );
        assert_eq!(heard(&rx), Some(260));

        // What it starts isn't this process's child (a parent-death signal,
        // as `bwrap --die-with-parent` sets, would kill it when this one
        // quit), and it has a session of its own.
        let report = root.join("probe-stat");
        std::fs::write(
            root.join("probe.sh"),
            format!("cat /proc/$$/stat > \"{0}.tmp\"\nmv \"{0}.tmp\" \"{0}\"\n", report.display()),
        )
        .unwrap();
        let probe = applications.join("test-probe.desktop");
        let exec = format!("sh {}", root.join("probe.sh").display());
        std::fs::write(&probe, format!("[Desktop Entry]\nType=Application\nName=test-probe\nExec={exec}\n")).unwrap();
        workspace.openApplicationAtURL_configuration_completionHandler(&url(&probe), &configuration, Some(&started));
        assert_eq!(heard(&rx), None);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let stat = loop {
            if let Ok(stat) = std::fs::read_to_string(&report) {
                break stat;
            }
            assert!(std::time::Instant::now() < deadline, "the probe wrote nothing");
            std::thread::sleep(Duration::from_millis(10));
        };
        // pid (comm) state ppid pgrp session …
        let pid: u32 = stat.split_whitespace().next().and_then(|p| p.parse().ok()).unwrap();
        let fields: Vec<u32> =
            stat.rsplit_once(')').unwrap().1.split_whitespace().skip(1).take(3).map(|f| f.parse().unwrap()).collect();
        assert_ne!(fields[0], std::process::id(), "not our child: {stat}");
        assert_eq!(fields[2], pid, "its own session: {stat}");

        std::fs::remove_dir_all(&root).unwrap();
        println!("default_applications_linux: ok");
    }
}
