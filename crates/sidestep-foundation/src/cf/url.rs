//! `CFURL` functions over `NSURL`.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{ClassType, msg_send};
use objc2_foundation::{NSString, NSURL};

use super::types::{CFTypeID, id, object, owned, owned_by};

type CFIndex = isize;
type Boolean = u8;

/// `kCFURLPOSIXPathStyle`, the only path style Linux has.
const POSIX_PATH_STYLE: CFIndex = 0;

fn url<'a>(cf: *const c_void) -> &'a NSURL {
    // SAFETY: the callers' contracts: `cf` is a URL.
    unsafe { &*cf.cast::<NSURL>() }
}

fn owned_url(url: Option<Retained<NSURL>>) -> *mut c_void {
    url.map_or(std::ptr::null_mut(), owned)
}

fn owned_string(text: Option<String>) -> *mut c_void {
    text.map_or(std::ptr::null_mut(), |t| owned(NSString::from_str(&t)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFURLGetTypeID() -> CFTypeID {
    id::URL
}

/// # Safety
///
/// `string` is a string; `base` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateWithString(
    _alloc: *const c_void,
    string: *const c_void,
    base: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let text = super::string::text(unsafe { object(string) });
    // CoreFoundation doesn't repair strings: invalid characters (or a
    // second `#`) make no URL.
    if !crate::url::parse::is_valid(&text) || text.matches('#').count() > 1 {
        return std::ptr::null_mut();
    }
    let base = (!base.is_null()).then(|| url(base));
    owned_url(crate::url::make(text.into_owned(), base).map(crate::url::as_url))
}

/// # Safety
///
/// `bytes` points to `length` bytes; `base` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateWithBytes(
    _alloc: *const c_void,
    bytes: *const u8,
    length: CFIndex,
    encoding: u32,
    base: *const c_void,
) -> *mut c_void {
    let bytes = if length <= 0 || bytes.is_null() {
        &[][..]
    } else {
        // SAFETY: per this function's contract.
        unsafe { std::slice::from_raw_parts(bytes, length as usize) }
    };
    let Some(text) = super::string::decode(bytes, encoding, false) else { return std::ptr::null_mut() };
    let base = (!base.is_null()).then(|| url(base));
    owned_url(crate::url::make(crate::url::prepare(&text, true).unwrap_or(text), base).map(crate::url::as_url))
}

/// # Safety
///
/// `path` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateWithFileSystemPath(
    _alloc: *const c_void,
    path: *const c_void,
    style: CFIndex,
    is_directory: Boolean,
) -> *mut c_void {
    if style != POSIX_PATH_STYLE {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let path = super::string::text(unsafe { object(path) });
    // SAFETY: +fileURLWithPath:isDirectory: takes a path and a BOOL.
    let made: Option<Retained<NSURL>> = unsafe {
        msg_send![NSURL::class(), fileURLWithPath: &*NSString::from_str(&path), isDirectory: is_directory != 0]
    };
    owned_url(made)
}

/// # Safety
///
/// `path` points to `length` bytes of a path; `base` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateFromFileSystemRepresentation(
    _alloc: *const c_void,
    path: *const u8,
    length: CFIndex,
    is_directory: Boolean,
) -> *mut c_void {
    if path.is_null() || length <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let path = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(path, length as usize) }).into_owned();
    // SAFETY: as in CFURLCreateWithFileSystemPath.
    let made: Option<Retained<NSURL>> = unsafe {
        msg_send![NSURL::class(), fileURLWithPath: &*NSString::from_str(&path), isDirectory: is_directory != 0]
    };
    owned_url(made)
}

/// # Safety
///
/// As [`CFURLCreateFromFileSystemRepresentation`]; `base` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateFromFileSystemRepresentationRelativeToBase(
    alloc: *const c_void,
    path: *const u8,
    length: CFIndex,
    is_directory: Boolean,
    base: *const c_void,
) -> *mut c_void {
    if base.is_null() || (length > 0 && !path.is_null() && unsafe { *path } == b'/') {
        // SAFETY: per this function's contract.
        return unsafe { CFURLCreateFromFileSystemRepresentation(alloc, path, length, is_directory) };
    }
    if path.is_null() || length <= 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let path = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(path, length as usize) }).into_owned();
    // SAFETY: +fileURLWithPath:isDirectory:relativeToURL: with a relative path.
    let made: Option<Retained<NSURL>> = unsafe {
        msg_send![NSURL::class(), fileURLWithPath: &*NSString::from_str(&path), isDirectory: is_directory != 0, relativeToURL: url(base)]
    };
    owned_url(made)
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLGetString(cf: *const c_void) -> *const c_void {
    static KEY: u8 = 0;
    let string: Retained<AnyObject> = url(cf).relativeString().into();
    // SAFETY: per this function's contract.
    owned_by(unsafe { object(cf) }, &KEY, string)
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLGetBaseURL(cf: *const c_void) -> *const c_void {
    // The base lives in the URL.
    url(cf).baseURL().map_or(std::ptr::null(), |b| Retained::as_ptr(&b).cast())
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyAbsoluteURL(cf: *const c_void) -> *mut c_void {
    owned_url(url(cf).absoluteURL())
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyFileSystemPath(cf: *const c_void, style: CFIndex) -> *mut c_void {
    if style != POSIX_PATH_STYLE {
        return std::ptr::null_mut();
    }
    owned_string(url(cf).path().map(|p| p.to_string()))
}

/// # Safety
///
/// `cf` is a URL; `buffer` has room for `max` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLGetFileSystemRepresentation(
    cf: *const c_void,
    resolve_against_base: Boolean,
    buffer: *mut u8,
    max: CFIndex,
) -> Boolean {
    let u = crate::url::url_impl(url(cf));
    let path = if resolve_against_base != 0 { u.path_text() } else { u.relative_path_text() };
    let Some(path) = path else { return 0 };
    if path.len() + 1 > max.max(0) as usize || path.contains('\0') {
        return 0;
    }
    // SAFETY: the caller's buffer holds `max` bytes, more than the path.
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr(), buffer, path.len());
        buffer.add(path.len()).write(0);
    }
    1
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyScheme(cf: *const c_void) -> *mut c_void {
    owned_string(url(cf).scheme().map(|s| s.to_string()))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyHostName(cf: *const c_void) -> *mut c_void {
    owned_string(url(cf).host().map(|s| s.to_string()))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyPath(cf: *const c_void) -> *mut c_void {
    // CoreFoundation's path stays percent-encoded.
    let u = crate::url::url_impl(url(cf));
    let (s, p) = u.absolute();
    owned_string(Some(s[p.path.clone()].to_string()))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyLastPathComponent(cf: *const c_void) -> *mut c_void {
    owned_string(url(cf).lastPathComponent().map(|s| s.to_string()))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyPathExtension(cf: *const c_void) -> *mut c_void {
    owned_string(url(cf).pathExtension().map(|s| s.to_string()).filter(|e| !e.is_empty()))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLHasDirectoryPath(cf: *const c_void) -> Boolean {
    u8::from(url(cf).hasDirectoryPath())
}

/// # Safety
///
/// `cf` is a URL; `component` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateCopyAppendingPathComponent(
    _alloc: *const c_void,
    cf: *const c_void,
    component: *const c_void,
    is_directory: Boolean,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let component = unsafe { &*component.cast::<NSString>() };
    owned_url(url(cf).URLByAppendingPathComponent_isDirectory(component, is_directory != 0))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateCopyDeletingLastPathComponent(
    _alloc: *const c_void,
    cf: *const c_void,
) -> *mut c_void {
    owned_url(url(cf).URLByDeletingLastPathComponent())
}

/// # Safety
///
/// `cf` is a URL; `extension` is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateCopyAppendingPathExtension(
    _alloc: *const c_void,
    cf: *const c_void,
    extension: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    let extension = unsafe { &*extension.cast::<NSString>() };
    owned_url(url(cf).URLByAppendingPathExtension(extension))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateCopyDeletingPathExtension(
    _alloc: *const c_void,
    cf: *const c_void,
) -> *mut c_void {
    owned_url(url(cf).URLByDeletingPathExtension())
}
