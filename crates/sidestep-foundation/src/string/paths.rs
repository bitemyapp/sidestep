//! NSString's path methods (NSPathUtilities).
//!
//! Paths are strings, handled as Foundation does on macOS: components split
//! on `/` with runs of slashes collapsed, a leading `/` and a trailing one
//! kept as components of their own, an extension being what follows the
//! last `.` of the last component unless that `.` starts it, and `~` or
//! `~user` at the start meaning a home directory (from `$HOME`, then the
//! password database).

use std::ffi::{CStr, CString, c_char};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class, msg_send};
use objc2_foundation::{NSArray, NSString, NSUInteger};

use super::view::view;
use super::wtf8;

/// A string's text as UTF-8 (lone surrogates replaced).
fn text(obj: &AnyObject) -> String {
    let v = view(obj);
    let t = v.text();
    wtf8::to_str_lossy(t.bytes, t.flags).into_owned()
}

fn string(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

/// Collapse runs of slashes and drop a trailing one, keeping a lone `/`.
pub(crate) fn normalize(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for c in p.chars() {
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

pub(crate) fn components(p: &str) -> Vec<String> {
    let mut out = Vec::new();
    if p.starts_with('/') {
        out.push("/".to_owned());
    }
    out.extend(p.split('/').filter(|c| !c.is_empty()).map(str::to_owned));
    if p.len() > 1 && p.ends_with('/') {
        out.push("/".to_owned());
    }
    out
}

fn last_component(p: &str) -> String {
    let n = normalize(p);
    if n == "/" {
        return n;
    }
    n.rsplit('/').next().unwrap_or("").to_owned()
}

/// The extension of the last component, and where its `.` is in `n`.
fn extension(n: &str) -> Option<(usize, &str)> {
    let last_start = n.rfind('/').map_or(0, |i| i + 1);
    let last = &n[last_start..];
    let dot = last.rfind('.')?;
    let ext = &last[dot + 1..];
    (dot > 0 && !ext.is_empty()).then_some((last_start + dot, ext))
}

fn delete_last(p: &str) -> String {
    let n = normalize(p);
    match n.rfind('/') {
        None => String::new(),
        Some(0) => "/".to_owned(),
        Some(i) => n[..i].to_owned(),
    }
}

pub(crate) fn append_component(p: &str, c: &str) -> String {
    if p.is_empty() {
        return normalize(c);
    }
    normalize(&format!("{p}/{c}"))
}

/// The home directory of the current user, or of `user`.
fn home(user: Option<&str>) -> Option<String> {
    if user.is_none()
        && let Ok(h) = std::env::var("HOME")
        && !h.is_empty()
    {
        return Some(h);
    }
    let name = match user {
        Some(u) => Some(CString::new(u).ok()?),
        None => None,
    };
    // The reentrant lookups fill a buffer of the caller's, so threads (and
    // other libraries) looking users up at the same time don't share one.
    // SAFETY: sysconf has no preconditions.
    let hint = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut buf = vec![0u8; usize::try_from(hint).unwrap_or(0).clamp(1024, 1 << 16)];
    loop {
        // SAFETY: a passwd is plain data, all zeros being a valid value.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call, and `buf` has the
        // length passed with it; the strings the entry points to live in
        // `buf`.
        let rc = unsafe {
            match &name {
                Some(n) => libc::getpwnam_r(n.as_ptr(), &mut entry, buf.as_mut_ptr().cast(), buf.len(), &mut found),
                None => libc::getpwuid_r(libc::getuid(), &mut entry, buf.as_mut_ptr().cast(), buf.len(), &mut found),
            }
        };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || entry.pw_dir.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated string inside `buf`, read before `buf`
        // goes away.
        return Some(unsafe { CStr::from_ptr(entry.pw_dir) }.to_string_lossy().into_owned());
    }
}

/// `stringByExpandingTildeInPath`: `~` or `~user` at the start becomes a
/// home directory, and slashes are tidied as `normalize` does, tilde or
/// not.
pub(crate) fn expand_tilde(p: &str) -> String {
    let Some(rest) = p.strip_prefix('~') else { return normalize(p) };
    let (user, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    match home((!user.is_empty()).then_some(user)) {
        Some(h) => normalize(&format!("{h}{tail}")),
        None => normalize(p),
    }
}

fn abbreviate(p: &str) -> String {
    let n = normalize(p);
    match home(None) {
        Some(h) if !h.is_empty() && h != "/" => {
            let h = normalize(&h);
            if n == h {
                "~".to_owned()
            } else if let Some(rest) = n.strip_prefix(&h)
                && rest.starts_with('/')
            {
                format!("~{rest}")
            } else {
                n
            }
        }
        _ => n,
    }
}

pub(crate) fn standardize(p: &str) -> String {
    let p = expand_tilde(p);
    let absolute = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/').filter(|c| !c.is_empty()) {
        match c {
            "." if !parts.is_empty() || absolute => {}
            ".." if absolute => {
                parts.pop();
            }
            _ => parts.push(c),
        }
    }
    // A relative path loses a leading "./" but keeps a lone ".".
    if !absolute && parts.len() > 1 && parts[0] == "." {
        parts.remove(0);
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

fn resolve_symlinks(p: &str) -> String {
    let expanded = expand_tilde(p);
    if !expanded.starts_with('/') {
        return standardize(&expanded);
    }
    match std::fs::canonicalize(&expanded) {
        Ok(real) => real.to_string_lossy().into_owned(),
        Err(_) => standardize(&expanded),
    }
}

/// A new NSArray of `items`.
pub(crate) fn new_array<T: objc2::Message>(items: &[Retained<T>]) -> Retained<NSArray<T>> {
    let items = items
        .iter()
        // SAFETY: every object is an AnyObject; retaining it keeps it alive
        // in the array.
        .map(|o| unsafe { Retained::retain(Retained::as_ptr(o).cast_mut().cast::<AnyObject>()) }.expect("an object"))
        .collect();
    // SAFETY: the array holds exactly `items`, each a `T`.
    unsafe { Retained::cast_unchecked(crate::array::make(items)) }
}

/// The strings in an NSArray, read through its primitives.
pub(crate) fn array_strings(array: &AnyObject) -> Vec<String> {
    // SAFETY: an NSArray answers -count and -objectAtIndex:.
    let count: usize = unsafe { msg_send![array, count] };
    (0..count)
        .map(|i| {
            // SAFETY: as above; the elements are strings.
            let s: Retained<NSString> = unsafe { msg_send![array, objectAtIndex: i] };
            text(&s)
        })
        .collect()
}

fn this(obj: &Helper) -> &AnyObject {
    obj
}

define_class!(
    // NSString's path methods, copied onto NSString when it loads.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepStringPaths"]
    pub(crate) struct Helper;

    impl Helper {
        #[unsafe(method_id(pathComponents))]
        fn path_components(&self) -> Retained<NSArray<NSString>> {
            let parts: Vec<Retained<NSString>> = components(&text(this(self))).iter().map(|c| string(c)).collect();
            new_array(&parts)
        }

        #[unsafe(method(isAbsolutePath))]
        fn is_absolute_path(&self) -> bool {
            let t = text(this(self));
            t.starts_with('/') || t.starts_with('~')
        }

        #[unsafe(method_id(lastPathComponent))]
        fn last_path_component(&self) -> Retained<NSString> {
            string(&last_component(&text(this(self))))
        }

        #[unsafe(method_id(stringByDeletingLastPathComponent))]
        fn deleting_last_path_component(&self) -> Retained<NSString> {
            string(&delete_last(&text(this(self))))
        }

        #[unsafe(method_id(stringByAppendingPathComponent:))]
        fn appending_path_component(&self, component: &NSString) -> Retained<NSString> {
            string(&append_component(&text(this(self)), &text(component)))
        }

        #[unsafe(method_id(pathExtension))]
        fn path_extension(&self) -> Retained<NSString> {
            let n = normalize(&text(this(self)));
            string(extension(&n).map_or("", |(_, e)| e))
        }

        #[unsafe(method_id(stringByDeletingPathExtension))]
        fn deleting_path_extension(&self) -> Retained<NSString> {
            let n = normalize(&text(this(self)));
            string(match extension(&n) {
                Some((dot, _)) => &n[..dot],
                None => &n,
            })
        }

        #[unsafe(method_id(stringByAppendingPathExtension:))]
        fn appending_path_extension(&self, ext: &NSString) -> Option<Retained<NSString>> {
            let (p, e) = (text(this(self)), text(ext));
            let n = normalize(&p);
            // There must be a last component to extend, and as on macOS the
            // extension can't hold a slash or a space or end in a dot.
            if n.is_empty() || n == "/" || e.contains(['/', ' ']) || e.ends_with('.') {
                None
            } else if e.is_empty() {
                Some(string(&n))
            } else {
                Some(string(&format!("{n}.{e}")))
            }
        }

        #[unsafe(method_id(stringByAbbreviatingWithTildeInPath))]
        fn abbreviating_with_tilde(&self) -> Retained<NSString> {
            string(&abbreviate(&text(this(self))))
        }

        #[unsafe(method_id(stringByExpandingTildeInPath))]
        fn expanding_tilde(&self) -> Retained<NSString> {
            string(&expand_tilde(&text(this(self))))
        }

        #[unsafe(method_id(stringByStandardizingPath))]
        fn standardizing(&self) -> Retained<NSString> {
            string(&standardize(&text(this(self))))
        }

        #[unsafe(method_id(stringByResolvingSymlinksInPath))]
        fn resolving_symlinks(&self) -> Retained<NSString> {
            string(&resolve_symlinks(&text(this(self))))
        }

        #[unsafe(method_id(stringsByAppendingPaths:))]
        fn strings_by_appending_paths(&self, paths: &NSArray<NSString>) -> Retained<NSArray<NSString>> {
            let base = text(this(self));
            let out: Vec<Retained<NSString>> =
                array_strings(paths).iter().map(|p| string(&append_component(&base, p))).collect();
            new_array(&out)
        }

        #[unsafe(method(fileSystemRepresentation))]
        fn file_system_representation(&self) -> NonNull<c_char> {
            // SAFETY: a string answers -UTF8String.
            let p: *const c_char = unsafe { objc2::msg_send![this(self), UTF8String] };
            NonNull::new(p.cast_mut()).unwrap_or_else(|| {
                panic!("-[NSString fileSystemRepresentation]: the string has no file system representation")
            })
        }

        #[unsafe(method(getFileSystemRepresentation:maxLength:))]
        fn get_file_system_representation(&self, buffer: NonNull<c_char>, max: NSUInteger) -> bool {
            let t = text(this(self));
            let fits = t.len() < max;
            if fits {
                // SAFETY: the caller passes room for `max` bytes.
                unsafe {
                    let out = buffer.as_ptr().cast::<u8>();
                    out.copy_from_nonoverlapping(t.as_ptr(), t.len());
                    *out.add(t.len()) = 0;
                }
            }
            fits
        }
    }
);

extern "C-unwind" fn path_with_components(
    _cls: *const AnyClass,
    _: objc2::runtime::Sel,
    parts: &AnyObject,
) -> *mut AnyObject {
    // Empty components add nothing, not even a separator: ["", "a"] is "a".
    let parts: Vec<String> = array_strings(parts).into_iter().filter(|p| !p.is_empty()).collect();
    let joined = parts.join("/");
    Retained::autorelease_return(string(&normalize(&joined))).cast()
}

/// Add the path methods to NSString.
pub(crate) fn install(target: &AnyClass) {
    super::install::copy_methods(Helper::class(), target, false);
    super::install::class_methods(target, c"_SidestepStringPathClassMethods", |b| {
        // SAFETY: the function matches the selector's convention.
        unsafe {
            b.add_method(objc2::sel!(pathWithComponents:), path_with_components as extern "C-unwind" fn(_, _, _) -> _)
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rules() {
        assert_eq!(components("/a/b/"), ["/", "a", "b", "/"]);
        assert_eq!(components("//"), ["/", "/"]);
        assert_eq!(components("a//b"), ["a", "b"]);
        assert!(components("").is_empty());
        assert_eq!(last_component("/a/b/"), "b");
        assert_eq!(last_component("//"), "/");
        assert_eq!(delete_last("/a/b/"), "/a");
        assert_eq!(delete_last("a"), "");
        assert_eq!(delete_last("/a"), "/");
        assert_eq!(extension(&normalize("a.tar.gz")).map(|e| e.1), Some("gz"));
        assert_eq!(extension(".hidden"), None);
        assert_eq!(extension("a."), None);
        assert_eq!(append_component("a/", "b/"), "a/b");
        assert_eq!(append_component("", "b"), "b");
        assert_eq!(standardize("/a/./b/../c"), "/a/c");
        assert_eq!(standardize("./x"), "x");
        assert_eq!(standardize("../x"), "../x");
    }
}
