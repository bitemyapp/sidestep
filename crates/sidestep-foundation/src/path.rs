//! Foundation's path functions: `NSHomeDirectory`, `NSTemporaryDirectory`,
//! `NSUserName` and friends, and `NSSearchPathForDirectoriesInDomains`
//! over the XDG mapping in `xdg`.
//!
//! Each returns an autoreleased string, as the C functions do on macOS.
//! The home directory has no trailing slash; the temporary directory has
//! one.

use std::path::Path;

use objc2::rc::Retained;
use objc2_foundation::NSString;

use crate::xdg;

fn autoreleased(text: &str) -> *mut NSString {
    Retained::autorelease_return(NSString::from_str(text))
}

/// A path without a trailing slash, except for "/".
pub(crate) fn without_slash(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() }
}

/// The current user's login name.
pub(crate) fn user_name() -> String {
    xdg::current_user()
        .map(|u| u.name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_default()
}

/// The current user's full name, or the login name when the password
/// database has none.
pub(crate) fn full_user_name() -> String {
    xdg::current_user().map(|u| u.full_name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(user_name)
}

/// The temporary directory with its trailing slash.
pub(crate) fn temporary_directory() -> String {
    let path = without_slash(&xdg::temporary());
    if path == "/" { path } else { format!("{path}/") }
}

/// The process's name: its executable's file name.
pub(crate) fn process_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|arg| Path::new(&arg).file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| std::fs::read_to_string("/proc/self/comm").ok().map(|c| c.trim_end().to_string()))
        .unwrap_or_default()
}

#[unsafe(no_mangle)]
pub extern "C" fn NSUserName() -> *mut NSString {
    autoreleased(&user_name())
}

#[unsafe(no_mangle)]
pub extern "C" fn NSFullUserName() -> *mut NSString {
    autoreleased(&full_user_name())
}

#[unsafe(no_mangle)]
pub extern "C" fn NSHomeDirectory() -> *mut NSString {
    autoreleased(&without_slash(&xdg::home()))
}

/// # Safety
///
/// `user` is null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn NSHomeDirectoryForUser(user: Option<&NSString>) -> *mut NSString {
    match user {
        None => NSHomeDirectory(),
        Some(user) => match xdg::passwd_by_name(&user.to_string()) {
            Some(entry) => autoreleased(&without_slash(&entry.home)),
            None => std::ptr::null_mut(),
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn NSTemporaryDirectory() -> *mut NSString {
    autoreleased(&temporary_directory())
}

#[unsafe(no_mangle)]
pub extern "C" fn NSOpenStepRootDirectory() -> *mut NSString {
    autoreleased("/")
}

/// Search paths, with the home directory abbreviated to "~" unless asked
/// to expand it.
pub(crate) fn search_paths(directory: usize, mask: usize, expand_tilde: bool) -> Vec<String> {
    let home = without_slash(&xdg::home());
    xdg::search_path(directory, mask)
        .iter()
        .map(|p| {
            let path = without_slash(p);
            match path.strip_prefix(&home) {
                Some(rest) if !expand_tilde && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
                _ => path,
            }
        })
        .collect()
}

#[unsafe(no_mangle)]
pub extern "C" fn NSSearchPathForDirectoriesInDomains(
    directory: objc2_foundation::NSUInteger,
    mask: objc2_foundation::NSUInteger,
    expand_tilde: objc2::runtime::Bool,
) -> *mut objc2_foundation::NSArray<NSString> {
    let paths: Vec<Retained<NSString>> =
        search_paths(directory, mask, expand_tilde.as_bool()).iter().map(|p| NSString::from_str(p)).collect();
    Retained::autorelease_return(objc2_foundation::NSArray::from_retained_slice(&paths))
}
