//! `NSURL`, `NSURLComponents` and `NSURLQueryItem`.
//!
//! An `NSURL` keeps the string it was made from (percent-encoded where it
//! had to be), an optional base and the component ranges of that string,
//! parsed once; the resolved absolute form and the file-system
//! representation are worked out on first use and cached, so
//! `-fileSystemRepresentation` returns a pointer that stays valid for the
//! URL's lifetime. Parsing and resolution are in `parse`; the component
//! rules below (a `nil` path for opaque URLs, an empty one for
//! `http://host`, trailing slashes dropped from `-path`, `-host` without
//! IPv6 brackets, `-password` left encoded) are the ones macOS follows,
//! pinned by `conformance/tests/services.rs`.
//!
//! Path derivations (`URLByAppendingPathComponent:` and friends) work on
//! the URL's own, still encoded, path and keep its base, query and
//! fragment. File URLs made from relative paths are relative to the
//! current directory; made from paths of existing directories they gain a
//! trailing slash (a file-system check, as on macOS). Paths aren't
//! normalized to Unicode NFD here: Linux file systems keep names as given.
//!
//! [`file_path`] and [`file_url`] let Rust code (AppKit's pasteboard, drag
//! and drop, open panels) move between paths and file URLs.

pub(crate) mod components;
pub(crate) mod parse;

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSError, NSString, NSUInteger, NSURL, NSZone};

use self::parse::Parts;

sidestep_runtime::static_class!(pub(crate) NSURL_CLASS, NSURL_META = "NSURL", || {
    let _ = NSURLImpl::class();
    crate::perform::install();
});

pub(crate) struct UrlIvars {
    /// The URL string, percent-encoded.
    string: Box<str>,
    parts: Parts,
    base: Option<Retained<NSURL>>,
    /// The resolved string and its parts, for URLs with a base.
    absolute: OnceLock<(Box<str>, Parts)>,
    file_system: OnceLock<CString>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSURL"]
    #[ivars = UrlIvars]
    pub(crate) struct NSURLImpl;

    impl NSURLImpl {
        #[unsafe(method_id(URLWithString:))]
        fn url_with_string(string: &NSString) -> Option<Retained<Self>> {
            from_string(&string.to_string(), None, true)
        }

        #[unsafe(method_id(URLWithString:relativeToURL:))]
        fn url_with_string_relative(string: &NSString, base: Option<&NSURL>) -> Option<Retained<Self>> {
            from_string(&string.to_string(), base, true)
        }

        #[unsafe(method_id(URLWithString:encodingInvalidCharacters:))]
        fn url_with_string_encoding(string: &NSString, encode: bool) -> Option<Retained<Self>> {
            from_string(&string.to_string(), None, encode)
        }

        #[unsafe(method_id(init))]
        fn init_empty(this: Allocated<Self>) -> Option<Retained<Self>> {
            // A URL needs a string.
            drop(this);
            None
        }

        #[unsafe(method_id(initWithString:))]
        fn init_with_string(this: Allocated<Self>, string: &NSString) -> Option<Retained<Self>> {
            init_prepared(this, &string.to_string(), true, None)
        }

        #[unsafe(method_id(initWithString:relativeToURL:))]
        fn init_with_string_relative(
            this: Allocated<Self>,
            string: &NSString,
            base: Option<&NSURL>,
        ) -> Option<Retained<Self>> {
            init_prepared(this, &string.to_string(), true, base)
        }

        #[unsafe(method_id(initWithString:encodingInvalidCharacters:))]
        fn init_with_string_encoding(this: Allocated<Self>, string: &NSString, encode: bool) -> Option<Retained<Self>> {
            init_prepared(this, &string.to_string(), encode, None)
        }

        #[unsafe(method_id(fileURLWithPath:))]
        fn file_url_with_path(path: &NSString) -> Option<Retained<Self>> {
            new_file_url(&path.to_string(), None, None)
        }

        #[unsafe(method_id(fileURLWithPath:isDirectory:))]
        fn file_url_with_path_dir(path: &NSString, is_dir: bool) -> Option<Retained<Self>> {
            new_file_url(&path.to_string(), Some(is_dir), None)
        }

        #[unsafe(method_id(fileURLWithPath:relativeToURL:))]
        fn file_url_with_path_relative(path: &NSString, base: Option<&NSURL>) -> Option<Retained<Self>> {
            new_file_url(&path.to_string(), None, base)
        }

        #[unsafe(method_id(fileURLWithPath:isDirectory:relativeToURL:))]
        fn file_url_with_path_dir_relative(
            path: &NSString,
            is_dir: bool,
            base: Option<&NSURL>,
        ) -> Option<Retained<Self>> {
            new_file_url(&path.to_string(), Some(is_dir), base)
        }

        #[unsafe(method_id(fileURLWithFileSystemRepresentation:isDirectory:relativeToURL:))]
        fn file_url_with_fs(path: NonNull<c_char>, is_dir: bool, base: Option<&NSURL>) -> Option<Retained<Self>> {
            // SAFETY: the caller passes a NUL-terminated path.
            let path = unsafe { CStr::from_ptr(path.as_ptr()) }.to_string_lossy().into_owned();
            new_file_url(&path, Some(is_dir), base)
        }

        #[unsafe(method_id(initFileURLWithFileSystemRepresentation:isDirectory:relativeToURL:))]
        fn init_file_url_with_fs(
            this: Allocated<Self>,
            path: NonNull<c_char>,
            is_dir: bool,
            base: Option<&NSURL>,
        ) -> Option<Retained<Self>> {
            // SAFETY: the caller passes a NUL-terminated path.
            let path = unsafe { CStr::from_ptr(path.as_ptr()) }.to_string_lossy().into_owned();
            init_file(this, &path, Some(is_dir), base)
        }

        #[unsafe(method_id(initFileURLWithPath:))]
        fn init_file_url(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            init_file(this, &path.to_string(), None, None)
        }

        #[unsafe(method_id(initFileURLWithPath:isDirectory:))]
        fn init_file_url_dir(this: Allocated<Self>, path: &NSString, is_dir: bool) -> Option<Retained<Self>> {
            init_file(this, &path.to_string(), Some(is_dir), None)
        }

        #[unsafe(method_id(initFileURLWithPath:relativeToURL:))]
        fn init_file_url_relative(
            this: Allocated<Self>,
            path: &NSString,
            base: Option<&NSURL>,
        ) -> Option<Retained<Self>> {
            init_file(this, &path.to_string(), None, base)
        }

        #[unsafe(method_id(initFileURLWithPath:isDirectory:relativeToURL:))]
        fn init_file_url_dir_relative(
            this: Allocated<Self>,
            path: &NSString,
            is_dir: bool,
            base: Option<&NSURL>,
        ) -> Option<Retained<Self>> {
            init_file(this, &path.to_string(), Some(is_dir), base)
        }

        #[unsafe(method_id(absoluteString))]
        fn absolute_string(&self) -> Option<Retained<NSString>> {
            Some(NSString::from_str(self.absolute().0))
        }

        #[unsafe(method_id(relativeString))]
        fn relative_string(&self) -> Retained<NSString> {
            NSString::from_str(&self.ivars().string)
        }

        #[unsafe(method_id(baseURL))]
        fn base_url(&self) -> Option<Retained<NSURL>> {
            self.ivars().base.clone()
        }

        #[unsafe(method_id(absoluteURL))]
        fn absolute_url(&self) -> Option<Retained<NSURL>> {
            match &self.ivars().base {
                None => Some(as_url(self.retain())),
                Some(_) => make(self.absolute().0.to_string(), None).map(as_url),
            }
        }

        #[unsafe(method_id(scheme))]
        fn scheme(&self) -> Option<Retained<NSString>> {
            let (s, p) = self.absolute();
            p.scheme.clone().map(|r| NSString::from_str(&s[r]))
        }

        #[unsafe(method_id(resourceSpecifier))]
        fn resource_specifier(&self) -> Option<Retained<NSString>> {
            self.resource_specifier_text().map(NSString::from_str)
        }

        #[unsafe(method_id(host))]
        fn host(&self) -> Option<Retained<NSString>> {
            self.host_text().map(decoded)
        }

        #[unsafe(method_id(port))]
        fn port(&self) -> Option<Retained<AnyObject>> {
            self.port_value().and_then(number)
        }

        #[unsafe(method_id(user))]
        fn user(&self) -> Option<Retained<NSString>> {
            let (s, p) = self.absolute();
            p.user.clone().map(|r| decoded(&s[r]))
        }

        #[unsafe(method_id(password))]
        fn password(&self) -> Option<Retained<NSString>> {
            // Left percent-encoded, as on macOS.
            let (s, p) = self.absolute();
            p.password.clone().map(|r| NSString::from_str(&s[r]))
        }

        #[unsafe(method_id(path))]
        fn path(&self) -> Option<Retained<NSString>> {
            self.path_text().map(|p| NSString::from_str(&p))
        }

        #[unsafe(method_id(relativePath))]
        fn relative_path(&self) -> Option<Retained<NSString>> {
            self.relative_path_text().map(|p| NSString::from_str(&p))
        }

        #[unsafe(method_id(parameterString))]
        fn parameter_string(&self) -> Option<Retained<NSString>> {
            // Parameters stay part of the path, as on current macOS.
            None
        }

        #[unsafe(method_id(query))]
        fn query(&self) -> Option<Retained<NSString>> {
            let (s, p) = self.absolute();
            p.query.clone().map(|r| NSString::from_str(&s[r]))
        }

        #[unsafe(method_id(fragment))]
        fn fragment(&self) -> Option<Retained<NSString>> {
            let (s, p) = self.absolute();
            p.fragment.clone().map(|r| NSString::from_str(&s[r]))
        }

        #[unsafe(method(hasDirectoryPath))]
        fn has_directory_path(&self) -> bool {
            self.is_directory()
        }

        #[unsafe(method(isFileURL))]
        fn is_file_url(&self) -> bool {
            self.is_file()
        }

        #[unsafe(method(isFileReferenceURL))]
        fn is_file_reference_url(&self) -> bool {
            false
        }

        #[unsafe(method(fileSystemRepresentation))]
        fn file_system_representation(&self) -> NonNull<c_char> {
            let path = self
                .ivars()
                .file_system
                .get_or_init(|| CString::new(self.path_text().unwrap_or_default()).unwrap_or_default());
            NonNull::new(path.as_ptr().cast_mut()).expect("a C string")
        }

        #[unsafe(method(getFileSystemRepresentation:maxLength:))]
        fn get_file_system_representation(&self, buffer: NonNull<c_char>, max: NSUInteger) -> bool {
            copy_path(self.path_text(), buffer, max)
        }

        #[unsafe(method_id(standardizedURL))]
        fn standardized_url(&self) -> Option<Retained<NSURL>> {
            self.standardized()
        }

        #[unsafe(method_id(filePathURL))]
        fn file_path_url(&self) -> Option<Retained<NSURL>> {
            self.is_file().then(|| as_url(self.retain()))
        }

        #[unsafe(method_id(fileReferenceURL))]
        fn file_reference_url(&self) -> Option<Retained<NSURL>> {
            // No file references on Linux: the path is the reference.
            self.is_file().then(|| as_url(self.retain()))
        }

        #[unsafe(method_id(lastPathComponent))]
        fn last_path_component(&self) -> Option<Retained<NSString>> {
            self.path_text().map(|p| NSString::from_str(last_component(&p)))
        }

        #[unsafe(method_id(pathExtension))]
        fn path_extension(&self) -> Option<Retained<NSString>> {
            self.path_text().map(|p| NSString::from_str(extension(last_component(&p)).unwrap_or("")))
        }

        #[cfg(feature = "collections")]
        #[unsafe(method_id(pathComponents))]
        fn path_components(&self) -> Option<Retained<AnyObject>> {
            self.path_text().map(|path| {
                let parts: Vec<Retained<NSString>> =
                    path_components(&path).into_iter().map(NSString::from_str).collect();
                objc2_foundation::NSArray::from_retained_slice(&parts).into()
            })
        }

        #[unsafe(method_id(fileURLWithPathComponents:))]
        fn file_url_with_components(components: &AnyObject) -> Option<Retained<Self>> {
            // SAFETY: the caller passes an array of strings.
            let count: NSUInteger = unsafe { msg_send![components, count] };
            let mut path = String::new();
            for i in 0..count {
                // SAFETY: `i` is in bounds.
                let part: Retained<NSString> = unsafe { msg_send![components, objectAtIndex: i] };
                let part = part.to_string();
                if !path.is_empty() && !path.ends_with('/') && !part.starts_with('/') {
                    path.push('/');
                }
                path.push_str(if path.ends_with('/') { part.trim_start_matches('/') } else { &part });
            }
            new_file_url(&path, None, None)
        }

        #[unsafe(method_id(URLByAppendingPathComponent:))]
        fn by_appending(&self, component: &NSString) -> Option<Retained<NSURL>> {
            self.appending(&component.to_string(), None)
        }

        #[unsafe(method_id(URLByAppendingPathComponent:isDirectory:))]
        fn by_appending_dir(&self, component: &NSString, is_dir: bool) -> Option<Retained<NSURL>> {
            self.appending(&component.to_string(), Some(is_dir))
        }

        #[unsafe(method_id(URLByDeletingLastPathComponent))]
        fn by_deleting_last(&self) -> Option<Retained<NSURL>> {
            self.with_own_path(|path| Some(delete_last(path)))
        }

        #[unsafe(method_id(URLByAppendingPathExtension:))]
        fn by_appending_extension(&self, ext: &NSString) -> Option<Retained<NSURL>> {
            let ext = ext.to_string();
            let ext = (!ext.contains('/')).then(|| parse::encode_component(&ext, parse::PATH_KEEP));
            self.with_own_path(|path| Some(append_extension(path, &ext?)))
        }

        #[unsafe(method_id(URLByDeletingPathExtension))]
        fn by_deleting_extension(&self) -> Option<Retained<NSURL>> {
            self.with_own_path(|path| Some(delete_extension(path)))
        }

        #[unsafe(method_id(URLByStandardizingPath))]
        fn by_standardizing_path(&self) -> Option<Retained<NSURL>> {
            self.file_path_derived(|path| Some((standardize(path), Some(true))))
        }

        #[unsafe(method_id(URLByResolvingSymlinksInPath))]
        fn by_resolving_symlinks(&self) -> Option<Retained<NSURL>> {
            self.file_path_derived(|path| {
                let resolved = std::fs::canonicalize(path)
                    .map_or_else(|_| standardize(path), |p| p.to_string_lossy().into_owned());
                Some((resolved, None))
            })
        }

        #[unsafe(method(checkResourceIsReachableAndReturnError:))]
        fn check_resource_is_reachable(&self, error: *mut *mut NSError) -> bool {
            let result = self.reachable();
            let reachable = result.is_ok();
            if let Err(e) = result {
                // SAFETY: the caller passes null or room for an error.
                unsafe { crate::error::set(error, e) };
            }
            reachable
        }

        #[unsafe(method(isEqual:))]
        fn is_equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<NSURL>()).is_some_and(|o| self.equals(url_impl(o)))
        }

        #[unsafe(method(hash))]
        fn hash(&self) -> NSUInteger {
            crate::string::hash_str(self.absolute().0)
        }

        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            NSString::from_str(&self.description_text())
        }

        #[unsafe(method_id(copyWithZone:))]
        fn copy_with_zone(&self, _zone: *mut NSZone) -> Retained<Self> {
            // Immutable: a copy is the same object.
            self.retain()
        }
    }

    unsafe impl NSObjectProtocol for NSURLImpl {}
);

impl NSURLImpl {
    fn host_text(&self) -> Option<&str> {
        let (s, p) = self.absolute();
        let host = &s[p.host.clone()?];
        (!host.is_empty()).then_some(host)
    }

    fn port_value(&self) -> Option<i64> {
        let (s, p) = self.absolute();
        port_value(&s[p.port.clone()?])
    }

    fn standardized(&self) -> Option<Retained<NSURL>> {
        if self.path_text().is_none() {
            return Some(as_url(self.retain()));
        }
        self.with_path(|path| Some(if path.is_empty() { String::new() } else { parse::standardize_path(path) }))
    }

    /// [`Self::with_path`], or `None` for URLs without a path.
    fn with_own_path(&self, f: impl FnOnce(&str) -> Option<String>) -> Option<Retained<NSURL>> {
        self.path_text()?;
        self.with_path(f)
    }

    /// A file URL for a path made from this one's, with whether it is a
    /// directory (`Some(true)`: if this URL is one; `None`: look). Other
    /// URLs come back as they are.
    fn file_path_derived(&self, f: impl FnOnce(&str) -> Option<(String, Option<bool>)>) -> Option<Retained<NSURL>> {
        if !self.is_file() {
            return Some(as_url(self.retain()));
        }
        let (path, dir) = f(&self.path_text()?)?;
        let dir = match dir {
            Some(true) => Some(self.is_directory()),
            _ => self.is_directory().then_some(true),
        };
        new_file_url(&path, dir, None).map(as_url)
    }

    /// Whether the file a file URL names exists.
    fn reachable(&self) -> Result<(), Retained<NSError>> {
        if !self.is_file() {
            let info = [(&crate::error::URL, as_url(self.retain()).into())];
            return Err(crate::error::cocoa(crate::error::code::FILE_READ_UNSUPPORTED_SCHEME, &info));
        }
        let path = self.path_text().unwrap_or_default();
        std::fs::metadata(&path).map(drop).map_err(|e| crate::error::file(crate::error::FileOp::Read, &e, &path))
    }

    fn description_text(&self) -> String {
        match &self.ivars().base {
            None => self.ivars().string.to_string(),
            Some(base) => format!("{} -- {}", self.ivars().string, url_impl(base).description_text()),
        }
    }

    /// The resolved string and parts.
    pub(crate) fn absolute(&self) -> (&str, &Parts) {
        let ivars = self.ivars();
        let Some(base) = &ivars.base else { return (&ivars.string, &ivars.parts) };
        let (s, p) = ivars.absolute.get_or_init(|| {
            let resolved = parse::resolve(url_impl(base).absolute().0, &ivars.string);
            let parts = parse::parse(&resolved).unwrap_or_default();
            (resolved.into_boxed_str(), parts)
        });
        (s, p)
    }

    pub(crate) fn has_base(&self) -> bool {
        self.ivars().base.is_some()
    }

    /// `-relativePath`: the decoded path of the URL's own string.
    pub(crate) fn relative_path_text(&self) -> Option<String> {
        path_of(&self.ivars().string, &self.ivars().parts)
    }

    pub(crate) fn is_file(&self) -> bool {
        let (s, p) = self.absolute();
        p.scheme.clone().is_some_and(|r| s[r].eq_ignore_ascii_case("file"))
    }

    fn is_directory(&self) -> bool {
        let (s, p) = self.absolute();
        s[p.path.clone()].ends_with('/')
    }

    /// `-path`: the decoded path, without a trailing slash (unless it is
    /// just "/"); `None` for opaque URLs.
    pub(crate) fn path_text(&self) -> Option<String> {
        let (s, p) = self.absolute();
        path_of(s, p)
    }

    fn resource_specifier_text(&self) -> Option<&str> {
        let ivars = self.ivars();
        let s = &*ivars.string;
        if ivars.base.is_some() {
            return Some(s);
        }
        let p = &ivars.parts;
        if p.authority.as_ref().is_some_and(|a| a.is_empty()) && p.path.is_empty() && p.query.is_none() {
            return None;
        }
        // An empty authority ("file:///x") isn't part of it.
        if p.authority.as_ref().is_some_and(|a| a.is_empty()) {
            return Some(&s[p.path.start..]);
        }
        Some(match &p.scheme {
            Some(scheme) => &s[scheme.end + 1..],
            None => s,
        })
    }

    fn equals(&self, other: &NSURLImpl) -> bool {
        let (a, b) = (self.ivars(), other.ivars());
        a.string == b.string
            && match (&a.base, &b.base) {
                (None, None) => true,
                (Some(x), Some(y)) => url_impl(x).equals(url_impl(y)),
                _ => false,
            }
    }

    /// A URL with the same string except for its path, rewritten by `f`
    /// (given and returning it percent-encoded). Relative URLs keep their
    /// base and rewrite their own path.
    fn with_path(&self, f: impl FnOnce(&str) -> Option<String>) -> Option<Retained<NSURL>> {
        let ivars = self.ivars();
        let (s, p) = (&*ivars.string, &ivars.parts);
        let path = f(&s[p.path.clone()])?;
        let string = format!("{}{path}{}", &s[..p.path.start], &s[p.path.end..]);
        make(string, ivars.base.as_deref()).map(as_url)
    }

    fn appending(&self, component: &str, is_dir: Option<bool>) -> Option<Retained<NSURL>> {
        let path = self.path_text()?;
        let dir = match is_dir {
            Some(dir) => dir,
            // Plain appends to file URLs look at the file system.
            None if self.is_file() => Path::new(&path).join(component.trim_start_matches('/')).is_dir(),
            None => false,
        };
        let mut encoded = parse::encode_component(component, parse::PATH_KEEP);
        if dir && !encoded.ends_with('/') {
            encoded.push('/');
        }
        self.with_path(|path| {
            let mut out = path.to_string();
            if !out.ends_with('/') && !encoded.starts_with('/') {
                out.push('/');
            }
            out.push_str(&encoded);
            Some(out)
        })
    }
}

/// The decoded path of a URL string; `None` for opaque URLs, for strings
/// without a path, and for paths that don't decode to UTF-8.
fn path_of(s: &str, p: &Parts) -> Option<String> {
    let path = &s[p.path.clone()];
    if path.is_empty() {
        // "http://host" has an empty path; "http:", "http://", "" and "?q"
        // have none.
        let authority = p.authority.as_ref().is_some_and(|a| !a.is_empty());
        return authority.then(String::new);
    }
    if p.scheme.is_some() && p.authority.is_none() && !path.starts_with('/') {
        return None;
    }
    let decoded = parse::decode(path)?;
    if decoded.len() > 1 && decoded.ends_with('/') {
        let trimmed = decoded.trim_end_matches('/');
        return Some(if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() });
    }
    Some(decoded)
}

/// The last component of a decoded path: "/" for the root, "" for none.
fn last_component(path: &str) -> &str {
    if path == "/" {
        return "/";
    }
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// The components of a decoded path: "/" first for an absolute one, no
/// empty ones.
#[cfg_attr(not(feature = "collections"), allow(dead_code))]
fn path_components(path: &str) -> Vec<&str> {
    let root = path.starts_with('/').then_some("/");
    root.into_iter().chain(path.split('/').filter(|s| !s.is_empty())).collect()
}

/// The extension of a file name: after the last dot, unless the dot starts
/// the name. `None` without a dot.
fn extension(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    (dot > 0).then(|| &name[dot + 1..])
}

/// `-URLByDeletingLastPathComponent` on an encoded path.
fn delete_last(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let trimmed = path.strip_suffix('/').unwrap_or(path);
    let (head, last) = match trimmed.rfind('/') {
        Some(i) => (&trimmed[..=i], &trimmed[i + 1..]),
        None => ("", trimmed),
    };
    match last {
        // Climbing out of the root or of a parent: one more "..".
        "" | ".." => format!("{trimmed}/../"),
        "." if head.is_empty() => "../".to_string(),
        "." => format!("{trimmed}/../"),
        _ if head.is_empty() => "./".to_string(),
        _ => head.to_string(),
    }
}

/// `-URLByAppendingPathExtension:` on an encoded path.
fn append_extension(path: &str, ext: &str) -> String {
    if path.is_empty() || ext.is_empty() {
        return path.to_string();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return format!("/.{ext}");
    }
    format!("{trimmed}.{ext}{}", &path[trimmed.len()..])
}

/// `-URLByDeletingPathExtension` on an encoded path: the extension is cut
/// from the encoded name, so an escaped dot doesn't start one.
fn delete_extension(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let start = trimmed.rfind('/').map_or(0, |i| i + 1);
    match trimmed[start..].rfind('.') {
        Some(dot) if dot > 0 && start + dot + 1 < trimmed.len() => {
            format!("{}{}", &trimmed[..start + dot], &path[trimmed.len()..])
        }
        _ => path.to_string(),
    }
}

/// A standardized file path: no "." or ".." segments, no doubled or
/// trailing slashes.
pub(crate) fn standardize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." if out.last().is_some_and(|s| *s != "..") => {
                out.pop();
            }
            ".." if absolute => {}
            segment => out.push(segment),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Copy a path into a caller's buffer with its NUL, if it fits.
fn copy_path(path: Option<String>, buffer: NonNull<c_char>, max: NSUInteger) -> bool {
    let Some(path) = path else { return false };
    if path.len() >= max || path.contains('\0') {
        return false;
    }
    // SAFETY: the caller passes room for `max` bytes, more than the path
    // and its NUL need.
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr(), buffer.as_ptr().cast::<u8>(), path.len());
        buffer.as_ptr().add(path.len()).write(0);
    }
    true
}

fn decoded(text: &str) -> Retained<NSString> {
    NSString::from_str(&parse::decode(text).unwrap_or_else(|| text.to_string()))
}

/// A port's value; ports out of range read as `i32::MAX`, as on macOS.
pub(crate) fn port_value(digits: &str) -> Option<i64> {
    if digits.is_empty() {
        return None;
    }
    let max = i64::from(i32::MAX);
    Some(digits.parse::<i64>().map_or(max, |p| p.min(max)))
}

#[cfg(feature = "collections")]
pub(crate) fn number(value: i64) -> Option<Retained<AnyObject>> {
    Some(objc2_foundation::NSNumber::new_i64(value).into())
}

/// `NSNumber` arrives with Foundation's collections; until then there is
/// nothing to box a port in.
#[cfg(not(feature = "collections"))]
pub(crate) fn number(_value: i64) -> Option<Retained<AnyObject>> {
    None
}

pub(crate) fn url_impl(url: &NSURL) -> &NSURLImpl {
    // SAFETY: every NSURL is an instance of this class; nothing subclasses
    // it.
    unsafe { &*(url as *const NSURL).cast::<NSURLImpl>() }
}

pub(crate) fn as_url(url: Retained<NSURLImpl>) -> Retained<NSURL> {
    // SAFETY: NSURLImpl is the class NSURL names.
    unsafe { Retained::cast_unchecked(url) }
}

fn init(this: Allocated<NSURLImpl>, string: String, base: Option<&NSURL>) -> Option<Retained<NSURLImpl>> {
    let Some(parts) = parse::parse(&string) else {
        drop(this);
        return None;
    };
    // A string with a scheme is absolute: it needs no base.
    let base = if parts.scheme.is_some() { None } else { base.map(|b| b.retain()) };
    let this = this.set_ivars(UrlIvars {
        string: string.into_boxed_str(),
        parts,
        base,
        absolute: OnceLock::new(),
        file_system: OnceLock::new(),
    });
    // SAFETY: NSObject's designated initializer.
    Some(unsafe { msg_send![super(this), init] })
}

fn init_prepared(
    this: Allocated<NSURLImpl>,
    string: &str,
    encode: bool,
    base: Option<&NSURL>,
) -> Option<Retained<NSURLImpl>> {
    match prepare(string, encode) {
        Some(string) => init(this, string, base),
        None => {
            drop(this);
            None
        }
    }
}

fn init_file(
    this: Allocated<NSURLImpl>,
    path: &str,
    is_dir: Option<bool>,
    base: Option<&NSURL>,
) -> Option<Retained<NSURLImpl>> {
    match file_url_string(path, is_dir, base) {
        Some((string, base)) => init(this, string, base.as_deref()),
        None => {
            drop(this);
            None
        }
    }
}

/// The string a URL keeps: encoded, or `None` if it needed encoding and
/// mustn't be.
pub(crate) fn prepare(string: &str, encode: bool) -> Option<String> {
    if parse::is_valid(string) {
        Some(string.to_string())
    } else if encode {
        Some(parse::encode_invalid(string))
    } else {
        None
    }
}

/// A new URL.
pub(crate) fn make(string: String, base: Option<&NSURL>) -> Option<Retained<NSURLImpl>> {
    load();
    init(NSURLImpl::alloc(), string, base)
}

fn from_string(string: &str, base: Option<&NSURL>, encode: bool) -> Option<Retained<NSURLImpl>> {
    make(prepare(string, encode)?, base)
}

/// The string and base of a file URL for `path`; `None` for an empty
/// path.
fn file_url_string(
    path: &str,
    is_dir: Option<bool>,
    base: Option<&NSURL>,
) -> Option<(String, Option<Retained<NSURL>>)> {
    if path.is_empty() {
        return None;
    }
    let absolute = path.starts_with('/');
    let base = if absolute {
        None
    } else {
        Some(match base {
            Some(base) => base.retain(),
            None => {
                let cwd =
                    std::env::current_dir().map_or_else(|_| "/".to_string(), |d| d.to_string_lossy().into_owned());
                as_url(new_file_url(&cwd, Some(true), None)?)
            }
        })
    };
    let dir = is_dir.unwrap_or_else(|| match &base {
        None => Path::new(path).is_dir(),
        Some(base) => Path::new(&url_impl(base).path_text().unwrap_or_default()).join(path).is_dir(),
    });
    let mut path = path.to_string();
    if dir {
        if !path.ends_with('/') {
            path.push('/');
        }
    } else {
        while path.len() > 1 && path.ends_with('/') {
            path.pop();
        }
    }
    let encoded = parse::encode_component(&path, parse::PATH_KEEP);
    Some(if absolute { (format!("file://{encoded}"), None) } else { (encoded, base) })
}

fn new_file_url(path: &str, is_dir: Option<bool>, base: Option<&NSURL>) -> Option<Retained<NSURLImpl>> {
    let (string, base) = file_url_string(path, is_dir, base)?;
    make(string, base.as_deref())
}

/// Load the class the `NSURL` shell names.
fn load() {
    // SAFETY: +class takes nothing and returns the receiver.
    let _: *const objc2::runtime::AnyClass = unsafe { msg_send![NSURL::class(), class] };
}

/// The path of a file URL; `None` for other URLs.
pub fn file_path(url: &NSURL) -> Option<PathBuf> {
    let url = url_impl(url);
    if !url.is_file() {
        return None;
    }
    url.path_text().map(PathBuf::from)
}

/// A file URL for an absolute or relative (to the current directory)
/// path; `None` for an empty one. Directories that exist get a trailing
/// slash.
pub fn file_url(path: &Path) -> Option<Retained<NSURL>> {
    new_file_url(&path.to_string_lossy(), None, None).map(as_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_derivations() {
        for (path, deleted) in [
            ("/a/b.txt", "/a/"),
            ("/a/", "/"),
            ("/", "/../"),
            ("", ""),
            ("a", "./"),
            ("a/b", "a/"),
            ("..", "../../"),
            (".", "../"),
            ("./", "../"),
            ("/.", "/./../"),
            ("/a/../", "/a/../../"),
        ] {
            assert_eq!(delete_last(path), deleted, "{path}");
        }
        for (path, appended) in
            [("/a/b.txt", "/a/b.txt.zip"), ("/a/", "/a.zip/"), ("/", "/.zip"), ("", ""), ("/b.", "/b..zip")]
        {
            assert_eq!(append_extension(path, "zip"), appended, "{path}");
        }
        for (path, deleted) in [
            ("/a/b.txt", "/a/b"),
            ("/a/b.tar.gz", "/a/b.tar"),
            ("/.bashrc", "/.bashrc"),
            ("/b.", "/b."),
            ("/a/c.t%2Ex", "/a/c"),
            ("/a.b/c", "/a.b/c"),
            ("/a.zip/", "/a/"),
        ] {
            assert_eq!(delete_extension(path), deleted, "{path}");
        }
        assert_eq!(path_components("/a/b c/"), ["/", "a", "b c"]);
        assert_eq!(path_components("/"), ["/"]);
        assert!(path_components("").is_empty());
        assert_eq!(path_components("a/b"), ["a", "b"]);
    }
}
