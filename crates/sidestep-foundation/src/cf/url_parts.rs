//! `CFURL`'s view of a URL's parts, which differs from `NSURL`'s in places
//! (measured on macOS, `conformance/tests/cf_urls.rs`):
//!
//! - the network location, user, password and port come from the resolved
//!   URL (a relative URL's base supplies them); the path, query, fragment
//!   and resource specifier from the URL's own string;
//! - the strict path is the path without its leading `/`, which says
//!   whether it was absolute;
//! - parameters (`;…`) are part of the path, so there is never a parameter
//!   string;
//! - the resource specifier of a URL with a path is what follows it (`?` or
//!   `#` on), and of one without (`mailto:…`) all after the scheme;
//! - byte ranges are into the URL's own string, with the separators
//!   around each part, or where an absent part would go.
//!
//! And the percent-escape functions, the URL's bytes, and URLs made from
//! bytes or paths relative to a base.

use std::ffi::c_void;
use std::ops::Range;

use objc2::rc::Retained;
use objc2::{ClassType, msg_send};
use objc2_foundation::{NSData, NSString, NSURL};

use super::string::{CFRange, decode, encoding, text};
use super::types::{object, owned};
use crate::url::parse::Parts;

type CFIndex = isize;
type Boolean = u8;
type CFStringEncoding = u32;

fn url<'a>(cf: *const c_void) -> &'a NSURL {
    // SAFETY: the callers' contracts: `cf` is a URL.
    unsafe { &*cf.cast::<NSURL>() }
}

fn owned_string(text: Option<String>) -> *mut c_void {
    text.map_or(std::ptr::null_mut(), |t| owned(NSString::from_str(&t)))
}

/// Whether the URL's own string has a path: no scheme, or one followed by
/// `/`.
fn decomposable(s: &str, p: &Parts) -> bool {
    match &p.scheme {
        None => true,
        Some(scheme) => s[scheme.end + 1..].starts_with('/'),
    }
}

/// Unescape percent escapes as the `…Copy…` functions do: none for a null
/// set of characters to leave escaped, else all but those.
unsafe fn unescape(raw: &str, leave: *const c_void) -> String {
    if leave.is_null() {
        return raw.to_string();
    }
    // SAFETY: the callers' contracts: `leave` is a string.
    let leave = text(unsafe { object(leave) });
    replace_escapes(raw, &leave, encoding::UTF8).unwrap_or_else(|| raw.to_string())
}

/// `raw` with its `%XX` escapes decoded in `encoding`, but for characters
/// in `leave`, whose escapes stay as they were; `None` if an escape is
/// malformed or the bytes aren't valid in the encoding.
fn replace_escapes(raw: &str, leave: &str, encoding: CFStringEncoding) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            let c = raw[i..].chars().next()?;
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        // A run of escapes decodes together (a character may take several).
        let start = i;
        let mut run = Vec::new();
        while i < bytes.len() && bytes[i] == b'%' {
            let hex = raw.get(i + 1..i + 3)?;
            run.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        }
        let escapes = &raw[start..i];
        if encoding == encoding::UTF8 {
            let decoded = std::str::from_utf8(&run).ok()?;
            let mut at = 0;
            for c in decoded.chars() {
                let n = c.len_utf8();
                if leave.contains(c) {
                    out.push_str(&escapes[at * 3..(at + n) * 3])
                } else {
                    out.push(c)
                }
                at += n;
            }
        } else {
            let decoded = decode(&run, encoding, false)?;
            for c in decoded.chars() {
                if leave.contains(c) {
                    let mut again = Vec::new();
                    super::string::encode_char(c, encoding, &mut again);
                    for b in again {
                        out.push_str(&format!("%{b:02X}"));
                    }
                } else {
                    out.push(c);
                }
            }
        }
    }
    Some(out)
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCanBeDecomposed(cf: *const c_void) -> Boolean {
    let (s, p) = crate::url::url_impl(url(cf)).own();
    u8::from(decomposable(s, p))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyNetLocation(cf: *const c_void) -> *mut c_void {
    let (s, p) = crate::url::url_impl(url(cf)).absolute();
    owned_string(p.authority.clone().map(|r| s[r].to_string()).filter(|a| !a.is_empty()))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyUserName(cf: *const c_void) -> *mut c_void {
    let (s, p) = crate::url::url_impl(url(cf)).absolute();
    owned_string(p.user.clone().map(|r| crate::url::parse::decode(&s[r.clone()]).unwrap_or_else(|| s[r].to_string())))
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyPassword(cf: *const c_void) -> *mut c_void {
    let (s, p) = crate::url::url_impl(url(cf)).absolute();
    owned_string(
        p.password.clone().map(|r| crate::url::parse::decode(&s[r.clone()]).unwrap_or_else(|| s[r].to_string())),
    )
}

/// The port, or -1 without one.
///
/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLGetPortNumber(cf: *const c_void) -> i32 {
    let (s, p) = crate::url::url_impl(url(cf)).absolute();
    p.port.clone().and_then(|r| s[r].parse::<i32>().ok()).unwrap_or(-1)
}

/// The URL's own path without its leading `/`; `is_absolute` says whether
/// it had one. NULL for an empty path, or a URL without a path.
///
/// # Safety
///
/// `cf` is a URL; `is_absolute` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyStrictPath(cf: *const c_void, is_absolute: *mut Boolean) -> *mut c_void {
    let (s, p) = crate::url::url_impl(url(cf)).own();
    let path = if decomposable(s, p) { &s[p.path.clone()] } else { "" };
    let absolute = path.starts_with('/');
    if !is_absolute.is_null() {
        // SAFETY: per this function's contract.
        unsafe { is_absolute.write(u8::from(absolute)) };
    }
    if path.is_empty() {
        return std::ptr::null_mut();
    }
    owned_string(Some(path.strip_prefix('/').unwrap_or(path).to_string()))
}

/// NULL: parameters are part of the path, as on current macOS.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFURLCopyParameterString(_cf: *const c_void, _leave: *const c_void) -> *mut c_void {
    std::ptr::null_mut()
}

/// # Safety
///
/// `cf` is a URL; `leave` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyQueryString(cf: *const c_void, leave: *const c_void) -> *mut c_void {
    let (s, p) = crate::url::url_impl(url(cf)).own();
    if !decomposable(s, p) {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    owned_string(p.query.clone().map(|r| unsafe { unescape(&s[r], leave) }))
}

/// # Safety
///
/// `cf` is a URL; `leave` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyFragment(cf: *const c_void, leave: *const c_void) -> *mut c_void {
    let (s, p) = crate::url::url_impl(url(cf)).own();
    if !decomposable(s, p) {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    owned_string(p.fragment.clone().map(|r| unsafe { unescape(&s[r], leave) }))
}

/// Where the resource specifier starts in the URL's own string: after the
/// scheme's colon for a URL without a path, else at the `?` or `#` after
/// the path. `None` without one.
fn specifier_start(s: &str, p: &Parts) -> Option<usize> {
    if !decomposable(s, p) {
        return p.scheme.clone().map(|r| r.end + 1);
    }
    (p.path.end < s.len()).then_some(p.path.end)
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyResourceSpecifier(cf: *const c_void) -> *mut c_void {
    let (s, p) = crate::url::url_impl(url(cf)).own();
    owned_string(specifier_start(s, p).map(|at| s[at..].to_string()))
}

/// The URL's own string's bytes: its length, or -1 if `buffer` holds
/// fewer.
///
/// # Safety
///
/// `cf` is a URL; `buffer` null or with room for `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLGetBytes(cf: *const c_void, buffer: *mut u8, length: CFIndex) -> CFIndex {
    let (s, _) = crate::url::url_impl(url(cf)).own();
    if buffer.is_null() {
        return s.len() as CFIndex;
    }
    if (length.max(0) as usize) < s.len() {
        return -1;
    }
    // SAFETY: room for the bytes, per this function's contract.
    unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), buffer, s.len()) };
    s.len() as CFIndex
}

fn cf_range(range: Range<usize>) -> CFRange {
    CFRange { location: range.start as CFIndex, length: (range.end - range.start) as CFIndex }
}

const NOT_FOUND: CFRange = CFRange { location: -1, length: 0 };

/// A component's byte range and its range with the separators around it
/// (or, for an absent one, an empty range where it would go).
fn component_ranges(s: &str, p: &Parts, component: CFIndex) -> (CFRange, CFRange) {
    let at = |i: usize| CFRange { location: i as CFIndex, length: 0 };
    let absent = |i: usize| (NOT_FOUND, at(i));
    let scheme_colon = p.scheme.as_ref().map(|r| r.end);
    let authority = p.authority.clone();
    // Where the authority's text starts, its `//` just before.
    let after_scheme = match (&authority, scheme_colon) {
        (Some(a), _) => a.start,
        (None, Some(colon)) => colon + 1,
        (None, None) => 0,
    };
    // Where the separators before the authority start: the scheme's colon,
    // or the `//` of a URL without a scheme.
    let lead = scheme_colon.unwrap_or(0);
    // Where absent parts of the authority would go: after the scheme's
    // separators, or at the start without a scheme.
    let scheme_end = if p.scheme.is_some() { after_scheme } else { 0 };
    if !decomposable(s, p) {
        return match component {
            1 => (cf_range(p.scheme.clone().unwrap_or(0..0)), cf_range(0..lead + 1)),
            // Nothing after the colon: none, at the end.
            4 if lead + 1 == s.len() => absent(s.len()),
            4 => (cf_range(lead + 1..s.len()), cf_range(lead..s.len())),
            _ => (NOT_FOUND, NOT_FOUND),
        };
    }
    let host = p.host.clone().map(|h| {
        // With the brackets of an IPv6 address.
        if h.start > 0 && s.as_bytes()[h.start - 1] == b'[' { h.start - 1..h.end + 1 } else { h }
    });
    let userinfo = match (&p.user, &p.password) {
        (Some(u), Some(pw)) => Some(u.start..pw.end),
        (Some(u), None) => Some(u.clone()),
        _ => None,
    };
    let next_is = |end: usize, c: u8| s.as_bytes().get(end) == Some(&c);
    match component {
        // Scheme: with its colon, and the `//` of an authority.
        1 => match &p.scheme {
            Some(r) => (cf_range(r.clone()), cf_range(0..after_scheme)),
            None => absent(0),
        },
        // Net location: after the colon and `//`.
        2 => match authority.filter(|a| !a.is_empty()) {
            Some(a) => (cf_range(a.clone()), cf_range(lead..a.end)),
            None => absent(scheme_end),
        },
        // Path: and the `?` or `#` after it; an empty authority's
        // separators go with it. A relative URL without one (`?q`) has
        // none.
        3 if p.scheme.is_none() && p.authority.is_none() && p.path.is_empty() => absent(p.path.start),
        3 => {
            let start = if p.authority.as_ref().is_some_and(|a| a.is_empty()) { lead } else { p.path.start };
            let end = if p.path.end < s.len() { p.path.end + 1 } else { p.path.end };
            (cf_range(p.path.clone()), cf_range(start..end))
        }
        // Resource specifier: what follows the path's separator.
        4 => match specifier_start(s, p) {
            Some(at) => (cf_range(at + 1..s.len()), cf_range(at..s.len())),
            None => absent(s.len()),
        },
        // User: after the scheme's separators, before `:` or `@`.
        5 => match &p.user {
            Some(u) => (cf_range(u.clone()), cf_range(lead..u.end + 1)),
            None => absent(scheme_end),
        },
        // Password: between `:` and `@`.
        6 => match &p.password {
            Some(pw) => (cf_range(pw.clone()), cf_range(pw.start - 1..pw.end + 1)),
            None => absent(p.user.as_ref().map_or(scheme_end, |u| u.end)),
        },
        // User info: user and password, with the separators and `@`.
        7 => match userinfo {
            Some(u) => (cf_range(u.clone()), cf_range(lead..u.end + 1)),
            None => absent(scheme_end),
        },
        // Host: after `@` (or the scheme's separators), with the `:` of a
        // port.
        8 => match host {
            Some(h) if !h.is_empty() => {
                let start = if userinfo.is_some() { h.start - 1 } else { lead };
                let end = if next_is(h.end, b':') { h.end + 1 } else { h.end };
                (cf_range(h.clone()), cf_range(start..end))
            }
            _ => absent(scheme_end),
        },
        // Port: with its colon (an empty one after a colon too).
        9 => match p.port.clone() {
            Some(r) => (cf_range(r.clone()), cf_range(r.start - 1..r.end)),
            None => absent(host.filter(|h| !h.is_empty()).map_or(scheme_end, |h| h.end)),
        },
        // Parameters are part of the path.
        10 => (NOT_FOUND, NOT_FOUND),
        // Query: with its `?`, and the `#` after it.
        11 => match p.query.clone() {
            Some(q) => {
                let end = if next_is(q.end, b'#') { q.end + 1 } else { q.end };
                (cf_range(q.clone()), cf_range(q.start - 1..end))
            }
            None => absent(p.path.end),
        },
        // Fragment: with its `#`.
        12 => match p.fragment.clone() {
            Some(f) => (cf_range(f.clone()), cf_range(f.start - 1..f.end)),
            None => absent(s.len()),
        },
        _ => (NOT_FOUND, NOT_FOUND),
    }
}

/// # Safety
///
/// `cf` is a URL; `with_separators` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLGetByteRangeForComponent(
    cf: *const c_void,
    component: CFIndex,
    with_separators: *mut CFRange,
) -> CFRange {
    let (s, p) = crate::url::url_impl(url(cf)).own();
    let (range, separated) = component_ranges(s, p, component);
    if !with_separators.is_null() {
        // SAFETY: per this function's contract.
        unsafe { with_separators.write(separated) };
    }
    range
}

/// # Safety
///
/// `cf` is null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateData(
    _alloc: *const c_void,
    cf: *const c_void,
    encoding: CFStringEncoding,
    _escape_whitespace: Boolean,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    // A URL's string is ASCII (what it can't hold is escaped), which each
    // encoding writes as it is, but for the wide ones.
    let (s, _) = crate::url::url_impl(url(cf)).own();
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        if !super::string::encode_char(c, encoding, &mut bytes) {
            return std::ptr::null_mut();
        }
    }
    owned(NSData::with_bytes(&bytes))
}

/// The absolute URL `bytes` make against `base`.
///
/// # Safety
///
/// `bytes` points to `length` bytes; `base` null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateAbsoluteURLWithBytes(
    alloc: *const c_void,
    bytes: *const u8,
    length: CFIndex,
    encoding: CFStringEncoding,
    base: *const c_void,
    compatibility: Boolean,
) -> *mut c_void {
    if compatibility != 0 && !base.is_null() && !bytes.is_null() && length >= 0 {
        // SAFETY: per this function's contract.
        let raw = unsafe { std::slice::from_raw_parts(bytes, length as usize) };
        // SAFETY: a URL; its absolute form, released below.
        let absolute_base = unsafe { super::url::CFURLCopyAbsoluteURL(base) };
        let resolved = decode(raw, encoding, false).and_then(|reference| {
            compatibility_resolution(&url(absolute_base).absoluteString()?.to_string(), &reference)
        });
        // SAFETY: made above.
        unsafe { super::base::CFRelease(absolute_base) };
        if let Some(resolved) = resolved {
            // SAFETY: the resolved URL's UTF-8 bytes.
            return unsafe {
                super::url::CFURLCreateWithBytes(
                    alloc,
                    resolved.as_ptr(),
                    resolved.len() as CFIndex,
                    encoding::UTF8,
                    std::ptr::null(),
                )
            };
        }
    }
    // SAFETY: per this function's contract.
    let relative = unsafe { super::url::CFURLCreateWithBytes(alloc, bytes, length, encoding, base) };
    if relative.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a URL just made, released after.
    unsafe {
        let absolute = super::url::CFURLCopyAbsoluteURL(relative);
        super::base::CFRelease(relative);
        absolute
    }
}

/// `reference` resolved against `base` as `CFURLCreateAbsoluteURLWithBytes`
/// does in compatibility mode (measured on macOS): a reference with the
/// base's scheme is relative, a query or fragment is added to the whole
/// base, and dot segments go, those past the root too unless last. `None`
/// for a base this doesn't cover (one without a scheme and `//`), which is
/// then resolved as without the mode.
fn compatibility_resolution(base: &str, reference: &str) -> Option<String> {
    let (scheme, rest) = base.split_once(':')?;
    let after = rest.strip_prefix("//")?;
    if !scheme_like(scheme) {
        return None;
    }
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (authority, base_rest) = after.split_at(authority_end);
    let base_path = &base_rest[..base_rest.find(['?', '#']).unwrap_or(base_rest.len())];
    let mut reference = reference;
    if let Some((their_scheme, their_rest)) = reference.split_once(':')
        && scheme_like(their_scheme)
    {
        if !their_scheme.eq_ignore_ascii_case(scheme) {
            // Another scheme's URL: absolute, its path's dot segments gone
            // if it has an authority.
            let Some(network) = their_rest.strip_prefix("//") else { return Some(reference.to_string()) };
            return Some(format!("{their_scheme}:{}", network_reference(network)));
        }
        reference = their_rest;
    }
    if reference.is_empty() {
        return Some(base.to_string());
    }
    if reference.starts_with(['?', '#']) {
        // Added to the base as it is; a fragment can't hold another `#`.
        let joined = format!("{base}{reference}");
        return Some(match joined.split_once('#') {
            Some((head, fragment)) => format!("{head}#{}", fragment.replace('#', "%23")),
            None => joined,
        });
    }
    if let Some(network) = reference.strip_prefix("//") {
        return Some(format!("{scheme}:{}", network_reference(network)));
    }
    let tail_at = reference.find(['?', '#']).unwrap_or(reference.len());
    let (path, tail) = reference.split_at(tail_at);
    let merged = if path.starts_with('/') {
        path.to_string()
    } else {
        let directory = match base_path.rfind('/') {
            Some(slash) => &base_path[..=slash],
            None => "/",
        };
        format!("{directory}{path}")
    };
    Some(format!("{scheme}://{authority}{}{tail}", without_dot_segments(&merged)))
}

/// `//authority/path?…` (the `//` left off) with the path's dot segments
/// gone.
fn network_reference(network: &str) -> String {
    let authority_end = network.find(['/', '?', '#']).unwrap_or(network.len());
    let (authority, rest) = network.split_at(authority_end);
    let tail_at = rest.find(['?', '#']).unwrap_or(rest.len());
    let (path, tail) = rest.split_at(tail_at);
    let path = if path.is_empty() { String::new() } else { without_dot_segments(path) };
    format!("//{authority}{path}{tail}")
}

fn scheme_like(scheme: &str) -> bool {
    scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

/// An absolute path without its `.` and `..` segments, as compatibility
/// mode removes them: `.` goes (a last one leaves a directory), `..` takes
/// the segment before it with it, and a `..` with nothing before it goes
/// too unless it is the last segment (a trailing `/` aside), which stays.
fn without_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path.strip_prefix('/').unwrap_or(path).split('/').collect();
    let n = segments.len();
    let mut out: Vec<&str> = Vec::new();
    for (i, &segment) in segments.iter().enumerate() {
        let last = i + 1 == n || (i + 2 == n && segments[n - 1].is_empty());
        match segment {
            "." => {
                if i + 1 == n {
                    out.push("");
                }
            }
            ".." => {
                if out.last().is_some_and(|s| *s != "..") {
                    out.pop();
                    if i + 1 == n {
                        out.push("");
                    }
                } else if last {
                    out.push("..");
                }
            }
            _ => out.push(segment),
        }
    }
    format!("/{}", out.join("/"))
}

/// A file URL for `path`: absolute on its own, else relative to `base`
/// (or, with none, the working directory).
///
/// # Safety
///
/// `path` is null or a string; `base` null or a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateWithFileSystemPathRelativeToBase(
    _alloc: *const c_void,
    path: *const c_void,
    style: CFIndex,
    is_directory: Boolean,
    base: *const c_void,
) -> *mut c_void {
    // kCFURLPOSIXPathStyle, the only one Linux has.
    if path.is_null() || style != 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let path = unsafe { &*path.cast::<NSString>() };
    let base = (!base.is_null()).then(|| url(base));
    // SAFETY: +fileURLWithPath:isDirectory:relativeToURL: takes a path, a
    // BOOL and a URL or nil.
    let made: Option<Retained<NSURL>> = unsafe {
        msg_send![NSURL::class(), fileURLWithPath: path, isDirectory: is_directory != 0, relativeToURL: base]
    };
    made.map_or(std::ptr::null_mut(), owned)
}

/// Whether ASCII `c` stays unescaped unless asked to be escaped: letters,
/// digits and the characters URLs use.
fn legal(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!$&'()*+,-./:;=?@_~".contains(c)
}

/// # Safety
///
/// `original` is null or a string; the others null or strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateStringByAddingPercentEscapes(
    _alloc: *const c_void,
    original: *const c_void,
    leave_unescaped: *const c_void,
    escape_too: *const c_void,
    encoding: CFStringEncoding,
) -> *mut c_void {
    if original.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let (text, leave, also) = unsafe {
        (
            text(object(original)).into_owned(),
            if leave_unescaped.is_null() { String::new() } else { text(object(leave_unescaped)).into_owned() },
            if escape_too.is_null() { String::new() } else { text(object(escape_too)).into_owned() },
        )
    };
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if leave.contains(c) || (legal(c) && !also.contains(c)) {
            out.push(c);
            continue;
        }
        let mut bytes = Vec::new();
        if !super::string::encode_char(c, encoding, &mut bytes) {
            return std::ptr::null_mut();
        }
        for b in bytes {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    owned(NSString::from_str(&out))
}

/// # Safety
///
/// `original` is null or a string; `leave_escaped` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateStringByReplacingPercentEscapes(
    alloc: *const c_void,
    original: *const c_void,
    leave_escaped: *const c_void,
) -> *mut c_void {
    // SAFETY: per this function's contract.
    unsafe { CFURLCreateStringByReplacingPercentEscapesUsingEncoding(alloc, original, leave_escaped, encoding::UTF8) }
}

/// Decode the escapes of `original` in `encoding`, but for characters in
/// `leave_escaped`; a null set leaves every escape. NULL if an escape is
/// malformed or doesn't decode.
///
/// # Safety
///
/// As [`CFURLCreateStringByReplacingPercentEscapes`].
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateStringByReplacingPercentEscapesUsingEncoding(
    _alloc: *const c_void,
    original: *const c_void,
    leave_escaped: *const c_void,
    encoding: CFStringEncoding,
) -> *mut c_void {
    if original.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let text = text(unsafe { object(original) }).into_owned();
    if leave_escaped.is_null() {
        return owned(NSString::from_str(&text));
    }
    // SAFETY: as above.
    let leave = super::string::text(unsafe { object(leave_escaped) }).into_owned();
    owned_string(replace_escapes(&text, &leave, encoding))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(s: &str, component: CFIndex) -> ((CFIndex, CFIndex), (CFIndex, CFIndex)) {
        let p = crate::url::parse::parse(s).unwrap();
        let (a, b) = component_ranges(s, &p, component);
        ((a.location, a.length), (b.location, b.length))
    }

    #[test]
    fn byte_ranges_as_measured() {
        let s = "http://user:pa%20ss@host.com:8080/p/a%20th;par%20am?q%20uery#fr%20ag";
        let want = [
            ((0, 4), (0, 7)),
            ((7, 26), (4, 29)),
            ((33, 18), (33, 19)),
            ((52, 16), (51, 17)),
            ((7, 4), (4, 8)),
            ((12, 7), (11, 9)),
            ((7, 12), (4, 16)),
            ((20, 8), (19, 10)),
            ((29, 4), (28, 5)),
            ((-1, 0), (-1, 0)),
            ((52, 8), (51, 10)),
            ((61, 7), (60, 8)),
        ];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(ranges(s, i as CFIndex + 1), *w, "component {}", i + 1);
        }
        assert_eq!(ranges("file:///tmp/x", 3), ((7, 6), (4, 9)));
        assert_eq!(ranges("file:///tmp/x", 2), ((-1, 0), (7, 0)));
        assert_eq!(ranges("http://[::1]:99/x", 8), ((7, 5), (4, 9)));
        assert_eq!(ranges("http://u@h/", 8), ((9, 1), (8, 2)));
        assert_eq!(ranges("http://u@h/", 6), ((-1, 0), (8, 0)));
        assert_eq!(ranges("//other/x", 2), ((2, 5), (0, 7)));
        assert_eq!(ranges("//other/x", 5), ((-1, 0), (0, 0)));
        assert_eq!(ranges("mailto:someone@example.com?subject=x", 4), ((7, 29), (6, 30)));
    }

    #[test]
    fn escapes() {
        assert_eq!(replace_escapes("a%20b%2Fc", "", encoding::UTF8).as_deref(), Some("a b/c"));
        assert_eq!(replace_escapes("a%20b%2Fc", "/", encoding::UTF8).as_deref(), Some("a b%2Fc"));
        assert_eq!(replace_escapes("%C3%A9", "", encoding::UTF8).as_deref(), Some("é"));
        assert_eq!(replace_escapes("%E9", "", encoding::UTF8), None);
        assert_eq!(replace_escapes("%zz", "", encoding::UTF8), None);
        assert_eq!(replace_escapes("%2", "", encoding::UTF8), None);
        assert_eq!(replace_escapes("%E9x", "", encoding::ISO_LATIN1).as_deref(), Some("éx"));
    }
}
