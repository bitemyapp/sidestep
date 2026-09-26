//! `NSBundle`: an application's or a bundle directory's resources and
//! `Info.plist`.
//!
//! The main bundle is worked out once, from the executable's resolved
//! path, trying in order:
//!
//! 1. an `X.app/Contents/MacOS/exe` layout: the bundle is `X.app`, with
//!    `Contents/Resources` and `Contents/Info.plist`;
//! 2. `<exe dir>/Resources`;
//! 3. `<exe dir>/../share/<exe name>`, as installed programs lay out;
//! 4. the executable's own directory, as macOS does for a bare executable.
//!
//! In the last three the bundle path is the executable's directory and
//! `Info.plist` sits in the resource directory. Without one the info
//! dictionary is empty and `bundleIdentifier` is nil.
//!
//! Resources are looked up in the resource directory (or the named
//! subdirectory of it), then in `Base.lproj` and `en.lproj`. Bundles are
//! made once per path and live for the rest of the process.
//! `localizedStringForKey:value:table:` returns the value, or the key when
//! the value is nil or empty.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSString, NSURL};

sidestep_runtime::static_class!(pub(crate) NSBUNDLE, NSBUNDLE_META = "NSBundle", || {
    let _ = NSBundleImpl::class();
    crate::perform::install();
});

/// Where a bundle's parts are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) path: PathBuf,
    pub(crate) resources: PathBuf,
    pub(crate) executable: Option<PathBuf>,
    pub(crate) info: PathBuf,
}

pub(crate) struct BundleIvars {
    layout: Layout,
    info: OnceLock<Option<plist::Dictionary>>,
    info_object: OnceLock<Option<Retained<AnyObject>>>,
    main: bool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSBundle"]
    #[ivars = BundleIvars]
    pub(crate) struct NSBundleImpl;

    impl NSBundleImpl {
        #[unsafe(method_id(mainBundle))]
        fn main_bundle() -> Retained<Self> {
            shared_main()
        }

        #[unsafe(method_id(bundleForClass:))]
        fn bundle_for_class(_class: &AnyClass) -> Retained<Self> {
            // Everything is linked into the executable.
            shared_main()
        }

        #[unsafe(method_id(bundleWithPath:))]
        fn bundle_with_path(path: &NSString) -> Option<Retained<Self>> {
            with_path(&path.to_string())
        }

        #[unsafe(method_id(bundleWithURL:))]
        fn bundle_with_url(url: &NSURL) -> Option<Retained<Self>> {
            with_url(url)
        }

        #[unsafe(method_id(initWithPath:))]
        fn init_with_path(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            drop(this);
            with_path(&path.to_string())
        }

        #[unsafe(method_id(initWithURL:))]
        fn init_with_url(this: Allocated<Self>, url: &NSURL) -> Option<Retained<Self>> {
            drop(this);
            with_url(url)
        }

        #[unsafe(method_id(bundlePath))]
        fn bundle_path(&self) -> Retained<NSString> {
            path_string(&self.ivars().layout.path)
        }

        #[unsafe(method_id(bundleURL))]
        fn bundle_url(&self) -> Retained<NSURL> {
            directory_url(&self.ivars().layout.path)
        }

        #[unsafe(method_id(resourcePath))]
        fn resource_path(&self) -> Option<Retained<NSString>> {
            Some(path_string(&self.ivars().layout.resources))
        }

        #[unsafe(method_id(resourceURL))]
        fn resource_url(&self) -> Option<Retained<NSURL>> {
            Some(directory_url(&self.ivars().layout.resources))
        }

        #[unsafe(method_id(executablePath))]
        fn executable_path(&self) -> Option<Retained<NSString>> {
            self.ivars().layout.executable.as_deref().map(path_string)
        }

        #[unsafe(method_id(executableURL))]
        fn executable_url(&self) -> Option<Retained<NSURL>> {
            self.ivars().layout.executable.as_deref().and_then(crate::url::file_url)
        }

        #[unsafe(method_id(bundleIdentifier))]
        fn bundle_identifier(&self) -> Option<Retained<NSString>> {
            self.info_string("CFBundleIdentifier").map(|s| NSString::from_str(&s))
        }

        #[unsafe(method_id(developmentLocalization))]
        fn development_localization(&self) -> Option<Retained<NSString>> {
            self.info_string("CFBundleDevelopmentRegion").map(|s| NSString::from_str(&s))
        }

        #[unsafe(method_id(infoDictionary))]
        fn info_dictionary(&self) -> Option<Retained<AnyObject>> {
            Some(self.info_object())
        }

        #[unsafe(method_id(localizedInfoDictionary))]
        fn localized_info_dictionary(&self) -> Option<Retained<AnyObject>> {
            Some(self.info_object())
        }

        #[unsafe(method_id(objectForInfoDictionaryKey:))]
        fn object_for_info_dictionary_key(&self, key: &NSString) -> Option<Retained<AnyObject>> {
            let info = self.info_object();
            // SAFETY: a dictionary; -objectForKey: returns a value or nil.
            unsafe { msg_send![&*info, objectForKey: key] }
        }

        #[unsafe(method_id(pathForResource:ofType:))]
        fn path_for_resource(&self, name: Option<&NSString>, ext: Option<&NSString>) -> Option<Retained<NSString>> {
            self.find(name, ext, None).map(|p| path_string(&p))
        }

        #[unsafe(method_id(pathForResource:ofType:inDirectory:))]
        fn path_for_resource_in(
            &self,
            name: Option<&NSString>,
            ext: Option<&NSString>,
            directory: Option<&NSString>,
        ) -> Option<Retained<NSString>> {
            self.find(name, ext, directory).map(|p| path_string(&p))
        }

        #[unsafe(method_id(pathForResource:ofType:inDirectory:forLocalization:))]
        fn path_for_resource_localized(
            &self,
            name: Option<&NSString>,
            ext: Option<&NSString>,
            directory: Option<&NSString>,
            _localization: Option<&NSString>,
        ) -> Option<Retained<NSString>> {
            self.find(name, ext, directory).map(|p| path_string(&p))
        }

        #[unsafe(method_id(URLForResource:withExtension:))]
        fn url_for_resource(&self, name: Option<&NSString>, ext: Option<&NSString>) -> Option<Retained<NSURL>> {
            self.find(name, ext, None).as_deref().and_then(crate::url::file_url)
        }

        #[unsafe(method_id(URLForResource:withExtension:subdirectory:))]
        fn url_for_resource_in(
            &self,
            name: Option<&NSString>,
            ext: Option<&NSString>,
            directory: Option<&NSString>,
        ) -> Option<Retained<NSURL>> {
            self.find(name, ext, directory).as_deref().and_then(crate::url::file_url)
        }

        #[unsafe(method_id(URLForResource:withExtension:subdirectory:localization:))]
        fn url_for_resource_localized(
            &self,
            name: Option<&NSString>,
            ext: Option<&NSString>,
            directory: Option<&NSString>,
            _localization: Option<&NSString>,
        ) -> Option<Retained<NSURL>> {
            self.find(name, ext, directory).as_deref().and_then(crate::url::file_url)
        }

        #[unsafe(method_id(pathsForResourcesOfType:inDirectory:))]
        fn paths_for_resources(&self, ext: Option<&NSString>, directory: Option<&NSString>) -> Retained<AnyObject> {
            let paths: Vec<Retained<NSString>> =
                self.find_all(ext, directory).iter().map(|p| path_string(p)).collect();
            objc2_foundation::NSArray::from_retained_slice(&paths).into()
        }

        #[unsafe(method_id(URLsForResourcesWithExtension:subdirectory:))]
        fn urls_for_resources(&self, ext: Option<&NSString>, directory: Option<&NSString>) -> Option<Retained<AnyObject>> {
            let urls: Vec<Retained<NSURL>> =
                self.find_all(ext, directory).iter().filter_map(|p| crate::url::file_url(p)).collect();
            Some(objc2_foundation::NSArray::from_retained_slice(&urls).into())
        }

        #[unsafe(method_id(localizedStringForKey:value:table:))]
        fn localized_string(&self, key: &NSString, value: Option<&NSString>, _table: Option<&NSString>) -> Retained<NSString> {
            match value.map(|v| v.to_string()).filter(|v| !v.is_empty()) {
                Some(value) => NSString::from_str(&value),
                None => NSString::from_str(&key.to_string()),
            }
        }

        #[unsafe(method(load))]
        fn load(&self) -> bool {
            true
        }

        #[unsafe(method(isLoaded))]
        fn is_loaded(&self) -> bool {
            self.ivars().main
        }

        #[unsafe(method(unload))]
        fn unload(&self) -> bool {
            false
        }

        #[unsafe(method(principalClass))]
        fn principal_class(&self) -> Option<&'static AnyClass> {
            let name = self.info_string("NSPrincipalClass")?;
            AnyClass::get(&std::ffi::CString::new(name).ok()?)
        }

        #[unsafe(method(classNamed:))]
        fn class_named(&self, name: &NSString) -> Option<&'static AnyClass> {
            AnyClass::get(&std::ffi::CString::new(name.to_string()).ok()?)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let state = if self.ivars().main { "loaded" } else { "not yet loaded" };
            NSString::from_str(&format!("NSBundle <{}> ({state})", self.ivars().layout.path.to_string_lossy()))
        }
    }

    unsafe impl NSObjectProtocol for NSBundleImpl {}
);

impl NSBundleImpl {
    fn info(&self) -> Option<&plist::Dictionary> {
        self.ivars()
            .info
            .get_or_init(|| {
                let bytes = std::fs::read(&self.ivars().layout.info).ok()?;
                match crate::plist::parse(&bytes)?.0 {
                    plist::Value::Dictionary(dict) => Some(dict),
                    _ => None,
                }
            })
            .as_ref()
    }

    fn info_string(&self, key: &str) -> Option<String> {
        self.info()?.get(key)?.as_string().map(str::to_string)
    }

    fn info_object(&self) -> Retained<AnyObject> {
        let object = self.ivars().info_object.get_or_init(|| {
            let dict = self.info().cloned().unwrap_or_default();
            crate::plist::to_object(&plist::Value::Dictionary(dict))
        });
        object.clone().unwrap_or_else(|| NSDictionary::<AnyObject, AnyObject>::new().into())
    }

    fn find(&self, name: Option<&NSString>, ext: Option<&NSString>, directory: Option<&NSString>) -> Option<PathBuf> {
        let name = name.map(|n| n.to_string()).filter(|n| !n.is_empty());
        let ext = extension(ext);
        let directory = directory.map(|d| d.to_string());
        for dir in self.search_dirs(directory.as_deref()) {
            match &name {
                Some(name) => {
                    let file = if ext.is_empty() { name.clone() } else { format!("{name}.{ext}") };
                    let candidate = dir.join(file);
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
                None => {
                    if let Some(first) = files_with_extension(&dir, &ext).into_iter().next() {
                        return Some(first);
                    }
                }
            }
        }
        None
    }

    fn find_all(&self, ext: Option<&NSString>, directory: Option<&NSString>) -> Vec<PathBuf> {
        let ext = extension(ext);
        let directory = directory.map(|d| d.to_string());
        self.search_dirs(directory.as_deref()).iter().flat_map(|dir| files_with_extension(dir, &ext)).collect()
    }

    /// The directories a resource may be in, in search order.
    fn search_dirs(&self, directory: Option<&str>) -> Vec<PathBuf> {
        let resources = &self.ivars().layout.resources;
        let sub = |base: PathBuf| match directory {
            Some(d) if !d.is_empty() => base.join(d),
            _ => base,
        };
        vec![sub(resources.clone()), sub(resources.join("Base.lproj")), sub(resources.join("en.lproj"))]
    }
}

/// A resource type without a leading dot; empty for none.
fn extension(ext: Option<&NSString>) -> String {
    ext.map(|e| e.to_string().trim_start_matches('.').to_string()).unwrap_or_default()
}

/// The files in a directory with an extension (any files, for none),
/// sorted by name.
fn files_with_extension(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && (ext.is_empty() || p.extension().is_some_and(|e| e == ext)))
        .collect();
    out.sort();
    out
}

fn path_string(path: &Path) -> Retained<NSString> {
    NSString::from_str(&crate::path::without_slash(path))
}

fn directory_url(path: &Path) -> Retained<NSURL> {
    // SAFETY: +fileURLWithPath:isDirectory: with a non-empty path.
    unsafe { msg_send![NSURL::class(), fileURLWithPath: &*path_string(path), isDirectory: true] }
}

/// The layout of the bundle an executable is in.
pub(crate) fn main_layout(exe: &Path) -> Layout {
    let exe_dir = exe.parent().unwrap_or(Path::new("/")).to_path_buf();
    let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // 1. X.app/Contents/MacOS/exe.
    if exe_dir.file_name().is_some_and(|n| n == "MacOS")
        && let Some(contents) = exe_dir.parent().filter(|c| c.file_name().is_some_and(|n| n == "Contents"))
        && let Some(bundle) = contents.parent()
    {
        return Layout {
            path: bundle.to_path_buf(),
            resources: contents.join("Resources"),
            executable: Some(exe.to_path_buf()),
            info: contents.join("Info.plist"),
        };
    }
    let resources = [exe_dir.join("Resources"), exe_dir.join("../share").join(&name)]
        .into_iter()
        .find(|dir| dir.is_dir())
        .map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir))
        .unwrap_or_else(|| exe_dir.clone());
    Layout { info: resources.join("Info.plist"), path: exe_dir, resources, executable: Some(exe.to_path_buf()) }
}

/// The layout of a bundle directory.
pub(crate) fn directory_layout(path: &Path) -> Layout {
    let contents = path.join("Contents");
    if contents.is_dir() {
        let info = contents.join("Info.plist");
        let executable = std::fs::read(&info)
            .ok()
            .and_then(|bytes| crate::plist::parse(&bytes))
            .and_then(|(value, _)| value.as_dictionary()?.get("CFBundleExecutable")?.as_string().map(str::to_string))
            .map(|name| contents.join("MacOS").join(name));
        return Layout { path: path.to_path_buf(), resources: contents.join("Resources"), executable, info };
    }
    let resources = if path.join("Resources").is_dir() { path.join("Resources") } else { path.to_path_buf() };
    Layout { path: path.to_path_buf(), info: resources.join("Info.plist"), resources, executable: None }
}

fn make(layout: Layout, main: bool) -> Retained<NSBundleImpl> {
    let ivars = BundleIvars { layout, info: OnceLock::new(), info_object: OnceLock::new(), main };
    // SAFETY: +alloc through the binding loads the class; NSObject's
    // designated initializer.
    unsafe {
        let this: Allocated<NSBundleImpl> = msg_send![objc2_foundation::NSBundle::class(), alloc];
        let this = this.set_ivars(ivars);
        msg_send![super(this), init]
    }
}

/// Bundles live for the rest of the process, one per path.
fn cached(key: &str, make_bundle: impl FnOnce() -> Option<Retained<NSBundleImpl>>) -> Option<Retained<NSBundleImpl>> {
    static BUNDLES: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
    let mut bundles = crate::thread::lock(BUNDLES.get_or_init(Default::default));
    let ptr = match bundles.get(key) {
        Some(&ptr) => ptr,
        None => {
            let ptr = Retained::into_raw(make_bundle()?) as usize;
            bundles.insert(key.to_string(), ptr);
            ptr
        }
    };
    // SAFETY: bundles in the table are never released.
    unsafe { Retained::retain(ptr as *mut NSBundleImpl) }
}

fn shared_main() -> Retained<NSBundleImpl> {
    static MAIN: OnceLock<usize> = OnceLock::new();
    let ptr = *MAIN.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(crate::path::process_name()));
        let layout = main_layout(&exe);
        let key = layout.path.to_string_lossy().into_owned();
        let bundle = cached(&key, || Some(make(layout, true))).expect("the main bundle");
        Retained::into_raw(bundle) as usize
    });
    // SAFETY: the main bundle is never released.
    unsafe { Retained::retain(ptr as *mut NSBundleImpl) }.expect("the main bundle")
}

fn with_url(url: &NSURL) -> Option<Retained<NSBundleImpl>> {
    with_path(&crate::url::file_path(url)?.to_string_lossy())
}

fn with_path(path: &str) -> Option<Retained<NSBundleImpl>> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        return None;
    }
    // The main bundle answers for its own path.
    let main_bundle = shared_main();
    if main_bundle.ivars().layout.path == dir {
        return Some(main_bundle);
    }
    let key = crate::path::without_slash(dir);
    cached(&key, || Some(make(directory_layout(Path::new(&key)), false)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        let root = std::env::temp_dir().join(format!("sidestep-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("Foo.app/Contents");
        std::fs::create_dir_all(app.join("MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Resources")).unwrap();
        let exe = app.join("MacOS/Foo");
        assert_eq!(
            main_layout(&exe),
            Layout {
                path: root.join("Foo.app"),
                resources: app.join("Resources"),
                executable: Some(exe.clone()),
                info: app.join("Info.plist")
            }
        );

        let bin = root.join("prefix/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("tool");
        assert_eq!(main_layout(&tool).resources, bin, "a bare executable's own directory");
        std::fs::create_dir_all(root.join("prefix/share/tool")).unwrap();
        let share = std::fs::canonicalize(root.join("prefix/share/tool")).unwrap();
        assert_eq!(main_layout(&tool).resources, share);
        std::fs::create_dir_all(bin.join("Resources")).unwrap();
        let layout = main_layout(&tool);
        assert_eq!(layout.resources, std::fs::canonicalize(bin.join("Resources")).unwrap(), "Resources wins");
        assert_eq!(layout.path, bin);
        assert_eq!(layout.info, layout.resources.join("Info.plist"));

        std::fs::write(
            app.join("Info.plist"),
            "<plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>Foo</string></dict></plist>",
        )
        .unwrap();
        let layout = directory_layout(&root.join("Foo.app"));
        assert_eq!(layout.executable, Some(exe));
        assert_eq!(directory_layout(&root).resources, root);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
