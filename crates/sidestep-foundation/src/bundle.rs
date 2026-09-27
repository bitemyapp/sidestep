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
//! subdirectory of it), then in `Base.lproj`, the preferred localization's
//! `.lproj` and the development region's; a lookup for a localization
//! named looks in the resource directory and that localization's alone.
//! Bundles are made once per path and live for the rest of the process.
//! `localizedStringForKey:value:table:` looks the key up in the table's
//! `.strings` files; when it isn't there, it returns the value, or the
//! key when the value is nil or empty.

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
            localization: Option<&NSString>,
        ) -> Option<Retained<NSString>> {
            self.find_localized(name, ext, directory, localization).map(|p| path_string(&p))
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
            localization: Option<&NSString>,
        ) -> Option<Retained<NSURL>> {
            self.find_localized(name, ext, directory, localization).as_deref().and_then(crate::url::file_url)
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
        fn localized_string_for_key(
            &self,
            key: &NSString,
            value: Option<&NSString>,
            table: Option<&NSString>,
        ) -> Retained<NSString> {
            // SAFETY: an NSBundleImpl is an NSBundle.
            let bundle = unsafe { &*(self as *const Self).cast::<objc2_foundation::NSBundle>() };
            let value = value.map(|v| v.to_string());
            let table = table.map(|t| t.to_string());
            NSString::from_str(&localized_string(bundle, &key.to_string(), value.as_deref(), table.as_deref(), None))
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

    fn find_localized(
        &self,
        name: Option<&NSString>,
        ext: Option<&NSString>,
        directory: Option<&NSString>,
        localization: Option<&NSString>,
    ) -> Option<PathBuf> {
        // SAFETY: an NSBundleImpl is an NSBundle.
        let bundle = unsafe { &*(self as *const Self).cast::<objc2_foundation::NSBundle>() };
        let (name, ext, directory, localization) = (
            name.map(|n| n.to_string()),
            ext.map(|e| e.to_string()),
            directory.map(|d| d.to_string()),
            localization.map(|l| l.to_string()),
        );
        find_resource(bundle, name.as_deref(), ext.as_deref(), directory.as_deref(), localization.as_deref())
    }

    fn find_all(&self, ext: Option<&NSString>, directory: Option<&NSString>) -> Vec<PathBuf> {
        let ext = extension(ext);
        let directory = directory.map(|d| d.to_string());
        self.search_dirs(directory.as_deref()).iter().flat_map(|dir| files_with_extension(dir, &ext)).collect()
    }

    /// The directories a resource may be in, in search order.
    fn search_dirs(&self, directory: Option<&str>) -> Vec<PathBuf> {
        self.search_dirs_for(directory, None)
    }

    /// The directories a resource may be in, in search order, as measured
    /// on macOS: for a localization asked for, the resource directory and
    /// that localization's `.lproj` alone; else the resource directory,
    /// `Base.lproj`, the preferred localization's, then the development
    /// region's (English's under either of its names, `en` and `English`).
    fn search_dirs_for(&self, directory: Option<&str>, localization: Option<&str>) -> Vec<PathBuf> {
        let resources = &self.ivars().layout.resources;
        let sub = |base: PathBuf| match directory {
            Some(d) if !d.is_empty() => base.join(d),
            _ => base,
        };
        let names: Vec<String> = match localization {
            Some(localization) => vec![localization.to_string()],
            None => {
                let mut names = vec!["Base".to_string()];
                let chosen = [Some(preferred_localization(self)), self.info_string("CFBundleDevelopmentRegion")];
                for name in chosen.into_iter().flatten() {
                    let other = match name.as_str() {
                        "en" => Some("English"),
                        "English" => Some("en"),
                        _ => None,
                    };
                    for name in std::iter::once(name.as_str()).chain(other) {
                        if !names.iter().any(|n| n == name) {
                            names.push(name.to_string());
                        }
                    }
                }
                names
            }
        };
        std::iter::once(sub(resources.clone()))
            .chain(names.iter().map(|name| sub(resources.join(format!("{name}.lproj")))))
            .collect()
    }
}

/// A bundle's parts.
pub(crate) fn layout(bundle: &objc2_foundation::NSBundle) -> &Layout {
    // SAFETY: every NSBundle is an instance of NSBundleImpl.
    &unsafe { &*(bundle as *const objc2_foundation::NSBundle).cast::<NSBundleImpl>() }.ivars().layout
}

fn imp(bundle: &objc2_foundation::NSBundle) -> &NSBundleImpl {
    // SAFETY: every NSBundle is an instance of NSBundleImpl.
    unsafe { &*(bundle as *const objc2_foundation::NSBundle).cast::<NSBundleImpl>() }
}

/// A bundle's `Info.plist`, as it is.
pub(crate) fn info(bundle: &objc2_foundation::NSBundle) -> Option<plist::Dictionary> {
    imp(bundle).info().cloned()
}

/// The first resource named `name` (or the first of the type) for a
/// localization.
pub(crate) fn find_resource(
    bundle: &objc2_foundation::NSBundle,
    name: Option<&str>,
    ext: Option<&str>,
    directory: Option<&str>,
    localization: Option<&str>,
) -> Option<PathBuf> {
    let ext = ext.map(|e| e.trim_start_matches('.').to_string()).unwrap_or_default();
    for dir in imp(bundle).search_dirs_for(directory, localization) {
        match name.filter(|n| !n.is_empty()) {
            Some(name) => {
                let candidate = dir.join(if ext.is_empty() { name.to_string() } else { format!("{name}.{ext}") });
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

/// Every resource of a type for a localization.
pub(crate) fn find_resources(
    bundle: &objc2_foundation::NSBundle,
    ext: Option<&str>,
    directory: Option<&str>,
    localization: Option<&str>,
) -> Vec<PathBuf> {
    let ext = ext.map(|e| e.trim_start_matches('.').to_string()).unwrap_or_default();
    imp(bundle)
        .search_dirs_for(directory, localization)
        .iter()
        .flat_map(|dir| files_with_extension(dir, &ext))
        .collect()
}

/// The localizations a bundle has: its `.lproj` directories' names.
pub(crate) fn localizations_in(resources: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(resources) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".lproj")).map(str::to_string))
        .collect();
    out.sort();
    out
}

/// Of `available`, the one the user prefers most (their preferred
/// languages matched whole, then by language), or the first.
pub(crate) fn preferred_of(available: &[String], preferred: &[String]) -> Option<String> {
    let language = |id: &str| id.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
    for want in preferred {
        let want_dashed = want.replace('_', "-");
        if let Some(found) = available.iter().find(|a| a.replace('_', "-").eq_ignore_ascii_case(&want_dashed)) {
            return Some(found.clone());
        }
        if let Some(found) = available.iter().find(|a| language(a) == language(want)) {
            return Some(found.clone());
        }
    }
    available.first().cloned()
}

/// The user's preferred languages.
pub(crate) fn preferred_languages() -> Vec<String> {
    objc2_foundation::NSLocale::preferredLanguages().iter().map(|l| l.to_string()).collect()
}

/// The localization a bundle's resources are looked up in: the user's
/// preferred one of those it has, else its development region, else
/// English.
fn preferred_localization(bundle: &NSBundleImpl) -> String {
    let available = localizations_in(&bundle.ivars().layout.resources);
    let preferred = preferred_languages();
    let chosen = available.iter().any(|a| a != "Base").then(|| {
        let real: Vec<String> = available.iter().filter(|a| *a != "Base").cloned().collect();
        preferred_of(&real, &preferred)
    });
    chosen.flatten().or_else(|| bundle.info_string("CFBundleDevelopmentRegion")).unwrap_or_else(|| "en".to_string())
}

/// The strings of a `.strings` table (`Localizable` without one) for a
/// localization, from its `.lproj`, then `Base.lproj`: an old-style
/// property list of strings (UTF-8, or UTF-16 with a byte-order mark), or
/// an XML or binary one.
pub(crate) fn strings_table(
    bundle: &objc2_foundation::NSBundle,
    table: Option<&str>,
    localization: Option<&str>,
) -> HashMap<String, String> {
    let imp = imp(bundle);
    let table = table.filter(|t| !t.is_empty()).unwrap_or("Localizable");
    let localization = localization.map_or_else(|| preferred_localization(imp), str::to_string);
    let resources = &imp.ivars().layout.resources;
    let mut out = HashMap::new();
    // A table outside every .lproj counts for every localization, below
    // the localized ones.
    for dir in [resources.clone(), resources.join("Base.lproj"), resources.join(format!("{localization}.lproj"))] {
        if let Ok(bytes) = std::fs::read(dir.join(format!("{table}.strings"))) {
            out.extend(parse_strings(&bytes));
        }
    }
    out
}

/// The pairs of a strings file.
pub(crate) fn parse_strings(bytes: &[u8]) -> Vec<(String, String)> {
    if bytes.starts_with(b"bplist") || bytes.starts_with(b"<?xml") {
        let Some((plist::Value::Dictionary(dict), _)) = crate::plist::parse(bytes) else { return Vec::new() };
        return dict.into_iter().filter_map(|(k, v)| Some((k, v.into_string()?))).collect();
    }
    let text = match bytes {
        [0xff, 0xfe, rest @ ..] => String::from_utf16_lossy(
            &rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>(),
        ),
        [0xfe, 0xff, rest @ ..] => String::from_utf16_lossy(
            &rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect::<Vec<_>>(),
        ),
        _ => String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).into_owned(),
    };
    let mut tokens = StringsTokens { chars: text.chars().peekable() };
    let mut out = Vec::new();
    while let Some(Token::Text(key)) = tokens.next_token() {
        match tokens.next_token() {
            Some(Token::Equals) => {
                let Some(Token::Text(value)) = tokens.next_token() else { break };
                out.push((key, value));
                // The `;` after the pair.
                if tokens.next_token() != Some(Token::Semicolon) {
                    break;
                }
            }
            // `"key";` stands for `"key" = "key";`.
            Some(Token::Semicolon) => out.push((key.clone(), key)),
            _ => break,
        }
    }
    out
}

/// A token of a `.strings` file.
#[derive(PartialEq)]
enum Token {
    /// A quoted string's text (escapes resolved) or a bare word.
    Text(String),
    Equals,
    Semicolon,
}

/// What octal escapes from `\200` to `\377` stand for (NEXTSTEP's
/// characters), as measured on macOS; the last two are NUL there.
const NEXTSTEP_HIGH: [u16; 128] = [
    0xa0, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc7, 0xc8, 0xc9, 0xca, 0xcb, 0xcc, 0xcd, 0xce, 0xcf, 0xd0, 0xd1, 0xd2,
    0xd3, 0xd4, 0xd5, 0xd6, 0xd9, 0xda, 0xdb, 0xdc, 0xdd, 0xde, 0xb5, 0xd7, 0xf7, 0xa9, 0xa1, 0xa2, 0xa3, 0x2044, 0xa5,
    0x192, 0xa7, 0xa4, 0x2019, 0x201c, 0xab, 0x2039, 0x203a, 0xfb01, 0xfb02, 0xae, 0x2013, 0x2020, 0x2021, 0xb7, 0xa6,
    0xb6, 0x2022, 0x201a, 0x201e, 0x201d, 0xbb, 0x2026, 0x2030, 0xac, 0xbf, 0xb9, 0x2cb, 0xb4, 0x2c6, 0x2dc, 0xaf,
    0x2d8, 0x2d9, 0xa8, 0xb2, 0x2da, 0xb8, 0xb3, 0x2dd, 0x2db, 0x2c7, 0x2014, 0xb1, 0xbc, 0xbd, 0xbe, 0xe0, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe7, 0xe8, 0xe9, 0xea, 0xeb, 0xec, 0xc6, 0xed, 0xaa, 0xee, 0xef, 0xf0, 0xf1, 0x141, 0xd8, 0x152,
    0xba, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xe6, 0xf9, 0xfa, 0xfb, 0x131, 0xfc, 0xfd, 0x142, 0xf8, 0x153, 0xdf, 0xfe,
    0xff, 0, 0,
];

struct StringsTokens<I: Iterator<Item = char>> {
    chars: std::iter::Peekable<I>,
}

impl<I: Iterator<Item = char>> StringsTokens<I> {
    /// The next token, comments and white space skipped.
    fn next_token(&mut self) -> Option<Token> {
        loop {
            match self.chars.peek()? {
                c if c.is_whitespace() => {
                    self.chars.next();
                }
                '/' => {
                    self.chars.next();
                    match self.chars.next()? {
                        '/' => while self.chars.next().is_some_and(|c| c != '\n') {},
                        '*' => {
                            let mut last = ' ';
                            for c in self.chars.by_ref() {
                                if last == '*' && c == '/' {
                                    break;
                                }
                                last = c;
                            }
                        }
                        _ => return None,
                    }
                }
                _ => break,
            }
        }
        let c = self.chars.next()?;
        match c {
            '=' => Some(Token::Equals),
            ';' => Some(Token::Semicolon),
            '"' | '\'' => self.quoted(c).map(Token::Text),
            c if bare(c) => {
                let mut out = c.to_string();
                while let Some(&c) = self.chars.peek().filter(|&&c| bare(c)) {
                    out.push(c);
                    self.chars.next();
                }
                Some(Token::Text(out))
            }
            _ => None,
        }
    }

    /// A quoted string's text, after its opening `quote`, as macOS reads
    /// it: `\a`, `\b`, `\f`, `\n`, `\r`, `\t` and `\v`; one to three
    /// octal digits (NEXTSTEP's characters from `\200`); `\U` and one to
    /// four hex digits, a UTF-16 unit (two make a surrogate pair); any
    /// other character escaped is itself.
    fn quoted(&mut self, quote: char) -> Option<String> {
        let mut units: Vec<u16> = Vec::new();
        let push = |units: &mut Vec<u16>, c: char| units.extend(c.encode_utf16(&mut [0; 2]).iter());
        loop {
            match self.chars.next()? {
                c if c == quote => return Some(String::from_utf16_lossy(&units)),
                '\\' => match self.chars.next()? {
                    'a' => units.push(7),
                    'b' => units.push(8),
                    'f' => units.push(12),
                    'n' => units.push(10),
                    'r' => units.push(13),
                    't' => units.push(9),
                    'v' => units.push(11),
                    d @ '0'..='7' => {
                        let mut value = d.to_digit(8)?;
                        for _ in 0..2 {
                            match self.chars.peek().and_then(|c| c.to_digit(8)) {
                                Some(digit) => {
                                    value = value * 8 + digit;
                                    self.chars.next();
                                }
                                None => break,
                            }
                        }
                        let value = value & 0xff;
                        units.push(if value < 0x80 { value as u16 } else { NEXTSTEP_HIGH[value as usize - 0x80] });
                    }
                    'U' => {
                        let mut value = 0u32;
                        for _ in 0..4 {
                            match self.chars.peek().and_then(|c| c.to_digit(16)) {
                                Some(digit) => {
                                    value = value * 16 + digit;
                                    self.chars.next();
                                }
                                None => break,
                            }
                        }
                        units.push(value as u16);
                    }
                    other => push(&mut units, other),
                },
                c => push(&mut units, c),
            }
        }
    }
}

/// Whether a character may be in an unquoted word.
fn bare(c: char) -> bool {
    c.is_ascii_alphanumeric() || "_$+/:.-".contains(c)
}

/// A bundle's strings looked up: the table's value for `key`, else
/// `value` if it isn't empty, else the key.
pub(crate) fn localized_string(
    bundle: &objc2_foundation::NSBundle,
    key: &str,
    value: Option<&str>,
    table: Option<&str>,
    localization: Option<&str>,
) -> String {
    match strings_table(bundle, table, localization).remove(key) {
        Some(found) => found,
        None => value.filter(|v| !v.is_empty()).unwrap_or(key).to_string(),
    }
}

/// The bundles made so far, the main one first.
pub(crate) fn all() -> Vec<Retained<objc2_foundation::NSBundle>> {
    let main = shared_main();
    let mut out: Vec<Retained<objc2_foundation::NSBundle>> = vec![unsafe { Retained::cast_unchecked(main.clone()) }];
    for ptr in cached_bundles() {
        if ptr != Retained::as_ptr(&main) as usize {
            // SAFETY: bundles in the table are never released.
            if let Some(bundle) = unsafe { Retained::retain(ptr as *mut objc2_foundation::NSBundle) } {
                out.push(bundle);
            }
        }
    }
    out
}

/// The bundle at a directory, made once.
pub(crate) fn at_path(path: &Path) -> Option<Retained<objc2_foundation::NSBundle>> {
    // SAFETY: NSBundleImpl is NSBundle's class.
    with_path(&path.to_string_lossy()).map(|b| unsafe { Retained::cast_unchecked(b) })
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

static BUNDLES: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

/// The bundles made so far, in no order.
fn cached_bundles() -> Vec<usize> {
    crate::thread::lock(BUNDLES.get_or_init(Default::default)).values().copied().collect()
}

/// Bundles live for the rest of the process, one per path.
fn cached(key: &str, make_bundle: impl FnOnce() -> Option<Retained<NSBundleImpl>>) -> Option<Retained<NSBundleImpl>> {
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
