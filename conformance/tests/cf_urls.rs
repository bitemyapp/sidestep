//! CoreFoundation's URL functions beyond the basics (services.rs has
//! those): the parts of a URL as CoreFoundation sees them, byte ranges,
//! percent escapes, URLs from bytes and relative paths, resource
//! properties (temporary ones too), reachability, file path and reference
//! URLs, security-scoped access and bookmarks, and the resource keys. Runs
//! on macOS against Apple's CoreFoundation and on Linux against Sidestep's.
#![allow(deprecated)]

use std::fmt::Debug;
use std::ptr::NonNull;

use objc2_core_foundation::{CFError, CFIndex, CFNumber, CFRetained, CFString, CFType, CFURL};
use sidestep as _;

/// Mismatches, collected so that one run shows them all.
#[derive(Default)]
struct Checks(Vec<String>);

impl Checks {
    fn eq<T: PartialEq + Debug>(&mut self, what: &str, got: T, want: T) {
        if got != want {
            self.0.push(format!("{what}: got {got:?}, want {want:?}"));
        }
    }

    fn done(self) {
        assert!(self.0.is_empty(), "{:#?}", self.0);
    }
}

fn s(text: &str) -> CFRetained<CFString> {
    CFString::from_str(text)
}

fn text(s: Option<CFRetained<CFString>>) -> Option<String> {
    s.map(|s| s.to_string())
}

fn url(string: &str, base: Option<&CFURL>) -> CFRetained<CFURL> {
    CFURL::from_string(None, &s(string), base).unwrap_or_else(|| panic!("a URL of {string:?}"))
}

fn url_text(u: &CFURL) -> String {
    objc2_core_foundation::CFURLGetString(u).unwrap().to_string()
}

/// A caller's error: its domain and code.
fn error(e: *mut CFError) -> Option<(String, CFIndex)> {
    let e = NonNull::new(e)?;
    // SAFETY: the functions return errors with a reference.
    let e = unsafe { CFRetained::from_raw(e) };
    Some((e.domain().unwrap().to_string(), e.code()))
}

fn cocoa(code: CFIndex) -> Option<(String, CFIndex)> {
    Some(("NSCocoaErrorDomain".to_string(), code))
}

/// A scratch directory of the test's own.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sidestep-cf-urls-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn parts_as_corefoundation_sees_them() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let full = url("http://user:pa%20ss@host.com:8080/p/a%20th;par%20am?q%20uery#fr%20ag", None);
    let none_escaped = s("");
    c.eq("decomposable", CFURLCanBeDecomposed(&full), true);
    c.eq("net location", text(CFURLCopyNetLocation(&full)), Some("user:pa%20ss@host.com:8080".into()));
    c.eq("user", text(CFURLCopyUserName(&full)), Some("user".into()));
    c.eq("password", text(CFURLCopyPassword(&full)), Some("pa ss".into()));
    c.eq("port", CFURLGetPortNumber(&full), 8080);
    let mut absolute = 9;
    c.eq("strict path", text(unsafe { CFURLCopyStrictPath(&full, &mut absolute) }), Some("p/a%20th;par%20am".into()));
    c.eq("strict path absolute", absolute, 1);
    c.eq("parameters", text(CFURLCopyParameterString(&full, None)), None);
    c.eq("query", text(CFURLCopyQueryString(&full, None)), Some("q%20uery".into()));
    c.eq("query unescaped", text(CFURLCopyQueryString(&full, Some(&none_escaped))), Some("q uery".into()));
    c.eq("fragment", text(CFURLCopyFragment(&full, None)), Some("fr%20ag".into()));
    c.eq("fragment unescaped", text(CFURLCopyFragment(&full, Some(&none_escaped))), Some("fr ag".into()));
    c.eq("resource specifier", text(CFURLCopyResourceSpecifier(&full)), Some("?q%20uery#fr%20ag".into()));

    let bare = url("http://host.com", None);
    c.eq("no port", CFURLGetPortNumber(&bare), -1);
    c.eq("no user", text(CFURLCopyUserName(&bare)), None);
    let mut absolute = 9;
    c.eq("no strict path", text(unsafe { CFURLCopyStrictPath(&bare, &mut absolute) }), None);
    c.eq("no strict path, not absolute", absolute, 0);
    c.eq("no resource specifier", text(CFURLCopyResourceSpecifier(&bare)), None);
    let root = url("http://u@h/", None);
    c.eq("root strict path", text(unsafe { CFURLCopyStrictPath(&root, &mut absolute) }), Some(String::new()));
    c.eq(
        "user without password",
        (text(CFURLCopyUserName(&root)), text(CFURLCopyPassword(&root))),
        (Some("u".into()), None),
    );

    let mail = url("mailto:someone@example.com?subject=x", None);
    c.eq("mailto decomposable", CFURLCanBeDecomposed(&mail), false);
    c.eq(
        "mailto resource specifier",
        text(CFURLCopyResourceSpecifier(&mail)),
        Some("someone@example.com?subject=x".into()),
    );
    c.eq("mailto query", text(CFURLCopyQueryString(&mail, None)), None);
    c.eq("mailto path", text(unsafe { CFURLCopyStrictPath(&mail, std::ptr::null_mut()) }), None);

    // A relative URL: the base's location, its own path, query and fragment.
    let base = url("http://h.com:81/a/b/", None);
    let relative = url("c/d;p?q#f", Some(&base));
    c.eq("relative net location", text(CFURLCopyNetLocation(&relative)), Some("h.com:81".into()));
    c.eq("relative port", CFURLGetPortNumber(&relative), 81);
    c.eq("relative strict path", text(unsafe { CFURLCopyStrictPath(&relative, &mut absolute) }), Some("c/d;p".into()));
    c.eq("relative path not absolute", absolute, 0);
    c.eq("relative query", text(CFURLCopyQueryString(&relative, None)), Some("q".into()));
    c.eq("relative resource specifier", text(CFURLCopyResourceSpecifier(&relative)), Some("?q#f".into()));
    let v6 = url("http://[::1]:99/x", None);
    c.eq("IPv6 net location", text(CFURLCopyNetLocation(&v6)), Some("[::1]:99".into()));
    c.eq("IPv6 port", CFURLGetPortNumber(&v6), 99);
    c.done();
}

#[test]
fn bytes_and_byte_ranges() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let full = url("http://user:pa%20ss@host.com:8080/p/a%20th;par%20am?q%20uery#fr%20ag", None);
    let length = unsafe { CFURLGetBytes(&full, std::ptr::null_mut(), 0) };
    c.eq("length", length, 68);
    let mut buffer = vec![0u8; 68];
    c.eq("copied", unsafe { CFURLGetBytes(&full, buffer.as_mut_ptr(), 68) }, 68);
    c.eq("bytes", String::from_utf8_lossy(&buffer).into_owned(), url_text(&full));
    c.eq("too small", unsafe { CFURLGetBytes(&full, buffer.as_mut_ptr(), 3) }, -1);
    let ranges = |u: &CFURL, component: CFIndex| {
        let mut with = CFRange { location: -7, length: -7 };
        let r = unsafe { CFURLGetByteRangeForComponent(u, CFURLComponentType(component), &mut with) };
        ((r.location, r.length), (with.location, with.length))
    };
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
        c.eq(&format!("full, component {}", i + 1), ranges(&full, i as CFIndex + 1), *w);
    }
    type Ranges = [((CFIndex, CFIndex), (CFIndex, CFIndex)); 12];
    let cases: [(&str, Ranges); 5] = [
        (
            "http://host.com",
            [
                ((0, 4), (0, 7)),
                ((7, 8), (4, 11)),
                ((15, 0), (15, 0)),
                ((-1, 0), (15, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((7, 8), (4, 11)),
                ((-1, 0), (15, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (15, 0)),
                ((-1, 0), (15, 0)),
            ],
        ),
        (
            "mailto:someone@example.com?subject=x",
            [
                ((0, 6), (0, 7)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
                ((7, 29), (6, 30)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (-1, 0)),
            ],
        ),
        (
            "file:///tmp/x",
            [
                ((0, 4), (0, 7)),
                ((-1, 0), (7, 0)),
                ((7, 6), (4, 9)),
                ((-1, 0), (13, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (13, 0)),
                ((-1, 0), (13, 0)),
            ],
        ),
        (
            "http://[::1]:99/x",
            [
                ((0, 4), (0, 7)),
                ((7, 8), (4, 11)),
                ((15, 2), (15, 2)),
                ((-1, 0), (17, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((-1, 0), (7, 0)),
                ((7, 5), (4, 9)),
                ((13, 2), (12, 3)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (17, 0)),
                ((-1, 0), (17, 0)),
            ],
        ),
        (
            "http://u@h/",
            [
                ((0, 4), (0, 7)),
                ((7, 3), (4, 6)),
                ((10, 1), (10, 1)),
                ((-1, 0), (11, 0)),
                ((7, 1), (4, 5)),
                ((-1, 0), (8, 0)),
                ((7, 1), (4, 5)),
                ((9, 1), (8, 2)),
                ((-1, 0), (10, 0)),
                ((-1, 0), (-1, 0)),
                ((-1, 0), (11, 0)),
                ((-1, 0), (11, 0)),
            ],
        ),
    ];
    for (string, want) in cases {
        let u = url(string, None);
        for (i, w) in want.iter().enumerate() {
            c.eq(&format!("{string}, component {}", i + 1), ranges(&u, i as CFIndex + 1), *w);
        }
    }
    // Relative URLs: their own strings' ranges.
    let base = url("http://h.com:81/a/b/", None);
    let relative = url("c/d;p?q#f", Some(&base));
    c.eq("relative path", ranges(&relative, 3), ((0, 5), (0, 6)));
    c.eq("relative query", ranges(&relative, 11), ((6, 1), (5, 3)));
    c.eq("relative scheme", ranges(&relative, 1), ((-1, 0), (0, 0)));
    // An empty port is there, after its colon; a relative URL of only a
    // query or fragment has no path; nothing after a scheme's colon is no
    // resource specifier.
    c.eq("empty port", ranges(&url("http://h:/", None), 9), ((9, 0), (8, 1)));
    c.eq("empty port, no path", ranges(&url("http://h:", None), 9), ((9, 0), (8, 1)));
    c.eq("empty port after a user", ranges(&url("http://u:@h:/p", None), 9), ((12, 0), (11, 1)));
    c.eq("empty port, network path", ranges(&url("//h:", Some(&base)), 9), ((4, 0), (3, 1)));
    for relative in ["?q", "#f", "?q#f", ""] {
        c.eq(&format!("{relative:?} path"), ranges(&url(relative, Some(&base)), 3), ((-1, 0), (0, 0)));
    }
    c.eq("\"?q\" resource specifier", ranges(&url("?q", Some(&base)), 4), ((1, 1), (0, 2)));
    c.eq("\"x:\" resource specifier", ranges(&url("x:", None), 4), ((-1, 0), (2, 0)));
    c.eq("\"http://h?q\" path", ranges(&url("http://h?q", None), 3), ((8, 0), (8, 1)));
    let other = url("//other/x", Some(&base));
    c.eq("network-path net location", ranges(&other, 2), ((2, 5), (0, 7)));
    c.eq("network-path user", ranges(&other, 5), ((-1, 0), (0, 0)));
    c.eq("network-path port", ranges(&other, 9), ((-1, 0), (7, 0)));
    let data = CFURLCreateData(None, Some(&full), 0x0800_0100, true).unwrap();
    c.eq("data", String::from_utf8_lossy(&data.to_vec()).into_owned(), url_text(&full));
    c.done();
}

#[test]
fn percent_escapes() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let ascii: String = (0x20u8..0x7f).map(|b| b as char).collect();
    let all = s(&(ascii + "é\n"));
    c.eq(
        "escaped by default",
        text(unsafe { CFURLCreateStringByAddingPercentEscapes(None, Some(&all), None, None, 0x0800_0100) }),
        Some(
            "%20!%22%23$%25&'()*+,-./0123456789:;%3C=%3E?@ABCDEFGHIJKLMNOPQRSTUVWXYZ%5B%5C%5D%5E_%60abcdefghijklmnopqrstuvwxyz%7B%7C%7D~%C3%A9%0A"
                .into(),
        ),
    );
    c.eq(
        "escaped as asked",
        text(unsafe {
            CFURLCreateStringByAddingPercentEscapes(
                None,
                Some(&s("a b/c?d")),
                Some(&s(" ")),
                Some(&s("/?")),
                0x0800_0100,
            )
        }),
        Some("a b%2Fc%3Fd".into()),
    );
    c.eq(
        "escaped in Latin 1",
        text(unsafe { CFURLCreateStringByAddingPercentEscapes(None, Some(&s("é")), None, None, 0x0201) }),
        Some("%E9".into()),
    );
    let replaced = |string: &str, leave: Option<&str>| {
        let leave = leave.map(s);
        text(CFURLCreateStringByReplacingPercentEscapes(None, Some(&s(string)), leave.as_deref()))
    };
    c.eq("nothing replaced", replaced("a%20b%2Fc%zz", None), Some("a%20b%2Fc%zz".into()));
    c.eq("all replaced", replaced("a%20b%2Fc", Some("")), Some("a b/c".into()));
    c.eq("some left", replaced("a%20b%2Fc", Some("/")), Some("a b%2Fc".into()));
    c.eq("UTF-8", replaced("%C3%A9", Some("")), Some("é".into()));
    c.eq("not UTF-8", replaced("%E9", Some("")), None);
    c.eq("not an escape", replaced("%zz", Some("")), None);
    c.eq("cut short", replaced("%2", Some("")), None);
    c.eq(
        "Latin 1",
        text(unsafe {
            CFURLCreateStringByReplacingPercentEscapesUsingEncoding(None, Some(&s("%E9x")), Some(&s("")), 0x0201)
        }),
        Some("éx".into()),
    );
    c.done();
}

#[test]
fn urls_from_bytes_and_relative_paths() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let base = url("http://h.com/a/b/c", None);
    let absolute = |rel: &str| {
        let made = unsafe {
            CFURLCreateAbsoluteURLWithBytes(None, rel.as_ptr(), rel.len() as CFIndex, 0x0800_0100, Some(&base), false)
        };
        made.map(|u| (url_text(&u), CFURLGetBaseURL(&u).is_none()))
    };
    c.eq("up", absolute("../d"), Some(("http://h.com/a/d".into(), true)));
    c.eq("escaped", absolute("x y"), Some(("http://h.com/a/b/x%20y".into(), true)));
    c.eq("empty", absolute(""), Some(("http://h.com/a/b/c".into(), true)));
    c.eq("network path", absolute("//o/p"), Some(("http://o/p".into(), true)));
    c.eq("query", absolute("?q"), Some(("http://h.com/a/b/c?q".into(), true)));
    // Compatibility mode: a reference with the base's scheme is relative, a
    // query or fragment goes on the end of the whole base, and dot segments
    // past the root go unless last.
    let compatible = |base: &CFURL, rel: &str| {
        let made = unsafe {
            CFURLCreateAbsoluteURLWithBytes(None, rel.as_ptr(), rel.len() as CFIndex, 0x0800_0100, Some(base), true)
        };
        made.map(|u| url_text(&u))
    };
    for (base, rel, want) in [
        ("http://a/b/c/d;p?q", "g:h", "g:h"),
        ("http://a/b/c/d;p?q", "g", "http://a/b/c/g"),
        ("http://a/b/c/d;p?q", "./g", "http://a/b/c/g"),
        ("http://a/b/c/d;p?q", "g/", "http://a/b/c/g/"),
        ("http://a/b/c/d;p?q", "/g", "http://a/g"),
        ("http://a/b/c/d;p?q", "//g", "http://g"),
        ("http://a/b/c/d;p?q", "?y", "http://a/b/c/d;p?q?y"),
        ("http://a/b/c/d;p?q", "g?y", "http://a/b/c/g?y"),
        ("http://a/b/c/d;p?q", "#s", "http://a/b/c/d;p?q#s"),
        ("http://a/b/c/d;p?q", "g#s", "http://a/b/c/g#s"),
        ("http://a/b/c/d;p?q", "g?y#s", "http://a/b/c/g?y#s"),
        ("http://a/b/c/d;p?q", ";x", "http://a/b/c/;x"),
        ("http://a/b/c/d;p?q", "g;x", "http://a/b/c/g;x"),
        ("http://a/b/c/d;p?q", "g;x?y#s", "http://a/b/c/g;x?y#s"),
        ("http://a/b/c/d;p?q", "", "http://a/b/c/d;p?q"),
        ("http://a/b/c/d;p?q", ".", "http://a/b/c/"),
        ("http://a/b/c/d;p?q", "./", "http://a/b/c/"),
        ("http://a/b/c/d;p?q", "..", "http://a/b/"),
        ("http://a/b/c/d;p?q", "../", "http://a/b/"),
        ("http://a/b/c/d;p?q", "../g", "http://a/b/g"),
        ("http://a/b/c/d;p?q", "../..", "http://a/"),
        ("http://a/b/c/d;p?q", "../../", "http://a/"),
        ("http://a/b/c/d;p?q", "../../g", "http://a/g"),
        ("http://a/b/c/d;p?q", "../../../g", "http://a/g"),
        ("http://a/b/c/d;p?q", "../../../../g", "http://a/g"),
        ("http://a/b/c/d;p?q", "/./g", "http://a/g"),
        ("http://a/b/c/d;p?q", "/../g", "http://a/g"),
        ("http://a/b/c/d;p?q", "g.", "http://a/b/c/g."),
        ("http://a/b/c/d;p?q", ".g", "http://a/b/c/.g"),
        ("http://a/b/c/d;p?q", "g..", "http://a/b/c/g.."),
        ("http://a/b/c/d;p?q", "..g", "http://a/b/c/..g"),
        ("http://a/b/c/d;p?q", "./../g", "http://a/b/g"),
        ("http://a/b/c/d;p?q", "./g/.", "http://a/b/c/g/"),
        ("http://a/b/c/d;p?q", "g/./h", "http://a/b/c/g/h"),
        ("http://a/b/c/d;p?q", "g/../h", "http://a/b/c/h"),
        ("http://a/b/c/d;p?q", "g;x=1/./y", "http://a/b/c/g;x=1/y"),
        ("http://a/b/c/d;p?q", "g;x=1/../y", "http://a/b/c/y"),
        ("http://a/b/c/d;p?q", "g?y/./x", "http://a/b/c/g?y/./x"),
        ("http://a/b/c/d;p?q", "g?y/../x", "http://a/b/c/g?y/../x"),
        ("http://a/b/c/d;p?q", "g#s/./x", "http://a/b/c/g#s/./x"),
        ("http://a/b/c/d;p?q", "g#s/../x", "http://a/b/c/g#s/../x"),
        ("http://a/b/c/d;p?q", "http:g", "http://a/b/c/g"),
        ("http://a/b/c/d;p?q", "http:/g", "http://a/g"),
        ("http://a/b/c/d;p?q", "http://x/../g", "http://x/g"),
        ("http://a/b/c/d;p?q", "file:g", "file:g"),
        ("http://a/b/c/d;p?q", "a%20b", "http://a/b/c/a%20b"),
        ("http://a/b/c/d;p?q", "a b", "http://a/b/c/a%20b"),
        ("http://a/b/c/d;p?q", "%2e%2e/g", "http://a/b/c/%2e%2e/g"),
        ("http://a/b/c/d;p?q", "../g?", "http://a/b/g?"),
        ("http://a/b/c/d;p?q", "..?y", "http://a/b/?y"),
        ("http://a/b/c/d;p?q", "#", "http://a/b/c/d;p?q#"),
        ("http://a/b/c/d;p?q", "?", "http://a/b/c/d;p?q?"),
        ("http://h/a/b?q#f", "g:h", "g:h"),
        ("http://h/a/b?q#f", "?y", "http://h/a/b?q#f?y"),
        ("http://h/a/b?q#f", "#s", "http://h/a/b?q#f%23s"),
        ("http://h/a/b?q#f", "", "http://h/a/b?q#f"),
        ("http://h/a/b?q#f", "..", "http://h/"),
        ("http://h/a/b?q#f", "../", "http://h/"),
        ("http://h/a/b?q#f", "../g", "http://h/g"),
        ("http://h/a/b?q#f", "../..", "http://h/.."),
        ("http://h/a/b?q#f", "../../../g", "http://h/g"),
        ("http://h/a/b?q#f", "/./g", "http://h/g"),
        ("http://h/a/b?q#f", "/../g", "http://h/g"),
        ("http://h/a/b?q#f", "./g/.", "http://h/a/g/"),
        ("http://h/a/b?q#f", "http:g", "http://h/a/g"),
        ("http://h/a/b?q#f", "http:/g", "http://h/g"),
        ("http://h/a/b?q#f", "http://x/../g", "http://x/g"),
        ("http://h/a/b?q#f", "file:g", "file:g"),
        ("http://h/a/b?q#f", "a b", "http://h/a/a%20b"),
        ("http://h/a/b?q#f", "..?y", "http://h/?y"),
        ("http://h/a/b?q#f", "#", "http://h/a/b?q#f%23"),
        ("http://h", "g:h", "g:h"),
        ("http://h", "?y", "http://h?y"),
        ("http://h", "#s", "http://h#s"),
        ("http://h", "", "http://h"),
        ("http://h", "..", "http://h/.."),
        ("http://h", "../", "http://h/../"),
        ("http://h", "../g", "http://h/g"),
        ("http://h", "../..", "http://h/.."),
        ("http://h", "../../../g", "http://h/g"),
        ("http://h", "/./g", "http://h/g"),
        ("http://h", "/../g", "http://h/g"),
        ("http://h", "./g/.", "http://h/g/"),
        ("http://h", "http:g", "http://h/g"),
        ("http://h", "http:/g", "http://h/g"),
        ("http://h", "http://x/../g", "http://x/g"),
        ("http://h", "file:g", "file:g"),
        ("http://h", "a b", "http://h/a%20b"),
        ("http://h", "..?y", "http://h/..?y"),
        ("http://h", "#", "http://h#"),
        ("file:///a/b/c", "g:h", "g:h"),
        ("file:///a/b/c", "?y", "file:///a/b/c?y"),
        ("file:///a/b/c", "#s", "file:///a/b/c#s"),
        ("file:///a/b/c", "", "file:///a/b/c"),
        ("file:///a/b/c", "..", "file:///a/"),
        ("file:///a/b/c", "../", "file:///a/"),
        ("file:///a/b/c", "../g", "file:///a/g"),
        ("file:///a/b/c", "../..", "file:///"),
        ("file:///a/b/c", "../../../g", "file:///g"),
        ("file:///a/b/c", "/./g", "file:///g"),
        ("file:///a/b/c", "/../g", "file:///g"),
        ("file:///a/b/c", "./g/.", "file:///a/b/g/"),
        ("file:///a/b/c", "http:g", "http:g"),
        ("file:///a/b/c", "http:/g", "http:/g"),
        ("file:///a/b/c", "http://x/../g", "http://x/g"),
        ("file:///a/b/c", "file:g", "file:///a/b/g"),
        ("file:///a/b/c", "a b", "file:///a/b/a%20b"),
        ("file:///a/b/c", "..?y", "file:///a/?y"),
        ("file:///a/b/c", "#", "file:///a/b/c#"),
    ] {
        c.eq(
            &format!("{base} + {rel:?} in compatibility mode"),
            compatible(&url(base, None), rel),
            Some(want.to_string()),
        );
    }

    let dir = url("file:///tmp/dir/", None);
    let from_path = |path: &str, is_dir: bool| {
        let made = CFURLCreateWithFileSystemPathRelativeToBase(
            None,
            Some(&s(path)),
            CFURLPathStyle::CFURLPOSIXPathStyle,
            is_dir,
            Some(&dir),
        )
        .unwrap();
        let absolute = CFURLCopyAbsoluteURL(&made).map(|a| url_text(&a));
        (url_text(&made), CFURLGetBaseURL(&made).is_some(), absolute)
    };
    c.eq("relative", from_path("a b", false), ("a%20b".into(), true, Some("file:///tmp/dir/a%20b".into())));
    c.eq("directory", from_path("sub", true), ("sub/".into(), true, Some("file:///tmp/dir/sub/".into())));
    c.eq("absolute", from_path("/abs/x", false), ("file:///abs/x".into(), false, Some("file:///abs/x".into())));
    c.eq("up", from_path("../up", false), ("../up".into(), true, Some("file:///tmp/up".into())));
    // Without a base, the working directory is.
    let made = CFURLCreateWithFileSystemPathRelativeToBase(
        None,
        Some(&s("x")),
        CFURLPathStyle::CFURLPOSIXPathStyle,
        false,
        None,
    )
    .unwrap();
    c.eq("no base", (url_text(&made), CFURLGetBaseURL(&made).is_some()), ("x".into(), true));
    c.done();
}

fn number(value: *const CFType) -> Option<i64> {
    // SAFETY: a number the test asked for.
    (!value.is_null()).then(|| unsafe { &*value.cast::<CFNumber>() }.as_i64()).flatten()
}

#[test]
fn resource_properties() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let dir = scratch("properties");
    let file = dir.join("f.txt");
    std::fs::write(&file, b"hello").unwrap();
    let u = CFURL::from_file_path(&file).unwrap();
    let missing = CFURL::from_file_path(dir.join("missing")).unwrap();
    let http = url("http://h/x", None);
    let size_key = unsafe { kCFURLFileSizeKey }.unwrap();
    let copy = |u: &CFURL, key: &CFString| {
        let mut value: *const CFType = std::ptr::null();
        let mut e: *mut CFError = std::ptr::null_mut();
        let ok = unsafe { CFURLCopyResourcePropertyForKey(u, Some(key), (&raw mut value).cast(), &mut e) };
        let value = NonNull::new(value.cast_mut()).map(|v| unsafe { CFRetained::from_raw(v) });
        (ok, value, error(e))
    };
    let (ok, value, e) = copy(&u, size_key);
    c.eq("size", (ok, value.as_deref().and_then(|v| number(v)), e), (true, Some(5), None));
    let (ok, value, e) = copy(&missing, size_key);
    c.eq("missing", (ok, value.is_none(), e), (false, true, cocoa(260)));
    let (ok, value, e) = copy(&http, size_key);
    c.eq("not a file", (ok, value.is_none(), e), (true, true, None));
    let custom = s("org.sidestep.custom");
    let (ok, value, _) = copy(&u, &custom);
    c.eq("unknown key", (ok, value.is_none()), (true, true));

    // Temporary values: on this URL object only, until its cache clears.
    CFURLSetTemporaryResourcePropertyForKey(&u, Some(&custom), Some(&s("temp")));
    let (_, value, _) = copy(&u, &custom);
    c.eq(
        "temporary",
        value.map(|v| unsafe { &*CFRetained::as_ptr(&v).as_ptr().cast::<CFString>() }.to_string()),
        Some("temp".into()),
    );
    let other = CFURL::from_file_path(&file).unwrap();
    c.eq("another URL object", copy(&other, &custom).1.is_none(), true);
    CFURLClearResourcePropertyCacheForKey(&u, Some(&custom));
    c.eq("cleared for its key", copy(&u, &custom).1.is_none(), true);
    CFURLSetTemporaryResourcePropertyForKey(&u, Some(&custom), Some(&s("temp")));
    CFURLClearResourcePropertyCache(&u);
    c.eq("cleared", copy(&u, &custom).1.is_none(), true);
    // A real value wins over a temporary one.
    CFURLSetTemporaryResourcePropertyForKey(&u, Some(size_key), Some(&CFNumber::new_i32(99)));
    c.eq("real value first", copy(&u, size_key).1.as_deref().and_then(|v| number(v)), Some(5));
    CFURLClearResourcePropertyCache(&u);

    let name_key = unsafe { kCFURLNameKey }.unwrap();
    let dir_key = unsafe { kCFURLIsDirectoryKey }.unwrap();
    let keys = CFArray::from_CFTypes(&[size_key, name_key, &*custom, dir_key]);
    let props = |u: &CFURL| {
        let mut e: *mut CFError = std::ptr::null_mut();
        let d = unsafe { CFURLCopyResourcePropertiesForKeys(u, Some(keys.as_opaque()), &mut e) };
        (d.map(|d| d.count()), error(e))
    };
    c.eq("properties", props(&u), (Some(3), None));
    c.eq("properties of nothing", props(&missing), (None, cocoa(260)));
    c.eq("properties of a web URL", props(&http), (Some(0), None));

    // Setting: dates are set; other keys are taken and left.
    let date_key = unsafe { kCFURLContentModificationDateKey }.unwrap();
    let date = CFDate::new(None, 100_000.0).unwrap();
    let set = |u: &CFURL, key: &CFString, value: &CFType| {
        let mut e: *mut CFError = std::ptr::null_mut();
        let ok = unsafe { CFURLSetResourcePropertyForKey(u, Some(key), Some(value), &mut e) };
        (ok, error(e))
    };
    c.eq("set a date", set(&u, date_key, &date), (true, None));
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    let seconds = modified.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() - 978_307_200.0;
    c.eq("the date", seconds, 100_000.0);
    c.eq("set a size", set(&u, size_key, &CFNumber::new_i32(3)), (true, None));
    c.eq("the size unchanged", copy(&u, size_key).1.as_deref().and_then(|v| number(v)), Some(5));
    c.eq("set an unknown key", set(&u, &custom, &CFNumber::new_i32(3)), (true, None));
    c.eq("unknown key still unset", copy(&u, &custom).1.is_none(), true);
    c.eq("set on nothing", set(&missing, date_key, &date), (false, cocoa(4)));
    c.eq("set on a web URL", set(&http, date_key, &date), (true, None));
    let values = CFDictionary::<CFString, CFType>::from_slices(&[date_key, &*custom], &[&date, &*CFNumber::new_i32(1)]);
    let mut e: *mut CFError = std::ptr::null_mut();
    let ok = unsafe { CFURLSetResourcePropertiesForKeys(&u, Some(values.as_opaque()), &mut e) };
    c.eq("set several", (ok, error(e)), (true, None));

    // Dates before 1970, with fractions of a second.
    for seconds in [-978_307_300.5, -978_307_200.25] {
        c.eq(&format!("set {seconds}"), set(&u, date_key, &CFDate::new(None, seconds).unwrap()), (true, None));
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        let back = -modified.duration_since(std::time::UNIX_EPOCH).unwrap_err().duration().as_secs_f64();
        c.eq(&format!("{seconds} set"), back - 978_307_200.0, seconds);
    }

    // A symbolic link is an alias file; its access is its own mode's, and
    // its canonical path its own (the directories above it resolved).
    let only_readable = dir.join("r.txt");
    std::fs::write(&only_readable, b"r").unwrap();
    std::fs::set_permissions(&only_readable, std::os::unix::fs::PermissionsExt::from_mode(0o400)).unwrap();
    let link = dir.join("link");
    std::os::unix::fs::symlink(&only_readable, &link).unwrap();
    let dangling = dir.join("dangling");
    std::os::unix::fs::symlink(dir.join("nothing"), &dangling).unwrap();
    let flag =
        |u: &CFURL, key: &CFString| copy(u, key).1.and_then(|v| v.downcast_ref::<CFBoolean>().map(|b| b.as_bool()));
    let canonical_dir = std::fs::canonicalize(&dir).unwrap();
    let canonical = |u: &CFURL| {
        copy(u, unsafe { kCFURLCanonicalPathKey }.unwrap())
            .1
            .and_then(|v| v.downcast_ref::<CFString>().map(|t| t.to_string()))
            .map(|t| t.replace(&*canonical_dir.to_string_lossy(), "DIR"))
    };
    for (what, path) in [("link", &link), ("dangling link", &dangling)] {
        let l = CFURL::from_file_path(path).unwrap();
        unsafe {
            c.eq(&format!("{what}: alias"), flag(&l, kCFURLIsAliasFileKey.unwrap()), Some(true));
            c.eq(&format!("{what}: executable"), flag(&l, kCFURLIsExecutableKey.unwrap()), Some(true));
            c.eq(&format!("{what}: writable"), flag(&l, kCFURLIsWritableKey.unwrap()), Some(true));
            c.eq(&format!("{what}: readable"), flag(&l, kCFURLIsReadableKey.unwrap()), Some(true));
        }
        let name = path.file_name().unwrap().to_string_lossy();
        c.eq(&format!("{what}: canonical path"), canonical(&l), Some(format!("DIR/{name}")));
    }
    unsafe {
        c.eq("a file: not an alias", flag(&u, kCFURLIsAliasFileKey.unwrap()), Some(false));
        c.eq("a file: not executable", flag(&u, kCFURLIsExecutableKey.unwrap()), Some(false));
        c.eq("a file's canonical path", canonical(&u), Some("DIR/f.txt".into()));
        // Flags Linux has no counterpart of (or that this file lacks) are
        // there, and false.
        for (name, key) in [
            ("mount trigger", kCFURLIsMountTriggerKey),
            ("system immutable", kCFURLIsSystemImmutableKey),
            ("user immutable", kCFURLIsUserImmutableKey),
        ] {
            c.eq(name, flag(&u, key.unwrap()), Some(false));
        }
        let security = copy(&u, kCFURLFileSecurityKey.unwrap()).1;
        c.eq("file security", security.map(|v| CFGetTypeID(Some(&v))), Some(CFFileSecurity::type_id()));
    }

    let reachable = |u: &CFURL| {
        let mut e: *mut CFError = std::ptr::null_mut();
        let ok = unsafe { CFURLResourceIsReachable(u, &mut e) };
        (ok, error(e))
    };
    c.eq("reachable", reachable(&u), (true, None));
    c.eq("unreachable", reachable(&missing), (false, cocoa(260)));
    c.eq("web URLs aren't", reachable(&http), (false, cocoa(262)));
    let _ = std::fs::remove_dir_all(&dir);
    c.done();
}

#[test]
fn file_path_and_reference_urls() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let dir = scratch("references");
    let file = dir.join("f.txt");
    std::fs::write(&file, b"hello").unwrap();
    let u = CFURL::from_file_path(&file).unwrap();
    let missing = CFURL::from_file_path(dir.join("missing")).unwrap();
    let http = url("http://h/x", None);
    c.eq("a file URL isn't a reference", CFURLIsFileReferenceURL(&u), false);
    let reference = |u: &CFURL| {
        let mut e: *mut CFError = std::ptr::null_mut();
        let made = unsafe { CFURLCreateFileReferenceURL(None, Some(u), &mut e) };
        (made, error(e))
    };
    let (made, e) = reference(&u);
    c.eq("a reference", (made.is_some(), e), (true, None));
    // Its path URL is the file's (a reference URL on macOS, the URL itself
    // on Linux).
    let back = made.and_then(|r| unsafe { CFURLCreateFilePathURL(None, Some(&r), std::ptr::null_mut()) });
    c.eq(
        "back to a path",
        back.map(|b| b.to_file_path().map(|p| std::fs::canonicalize(p).unwrap())),
        Some(Some(std::fs::canonicalize(&file).unwrap())),
    );
    let (made, e) = reference(&missing);
    c.eq("no reference to nothing", (made.is_none(), e), (true, cocoa(260)));
    let (made, e) = reference(&http);
    c.eq("no reference to a web URL", (made.is_none(), e), (true, cocoa(262)));
    let path_url = |u: &CFURL| {
        let mut e: *mut CFError = std::ptr::null_mut();
        let made = unsafe { CFURLCreateFilePathURL(None, Some(u), &mut e) };
        (made.map(|m| url_text(&m)), error(e))
    };
    c.eq("a path URL", path_url(&u), (Some(url_text(&u)), None));
    c.eq("a path URL for nothing", path_url(&missing), (Some(url_text(&missing)), None));
    c.eq("no path URL for a web URL", path_url(&http), (None, cocoa(262)));
    c.eq("access to a file", unsafe { CFURLStartAccessingSecurityScopedResource(&u) }, true);
    c.eq("no access to a web URL", unsafe { CFURLStartAccessingSecurityScopedResource(&http) }, false);
    unsafe { CFURLStopAccessingSecurityScopedResource(&u) };
    let _ = std::fs::remove_dir_all(&dir);
    c.done();
}

fn canonical(u: &CFURL) -> Option<std::path::PathBuf> {
    u.to_file_path().and_then(|p| std::fs::canonicalize(p).ok())
}

#[test]
fn bookmarks() {
    use objc2_core_foundation::*;
    let mut c = Checks::default();
    let dir = scratch("bookmarks");
    let file = dir.join("f.txt");
    std::fs::write(&file, b"hello").unwrap();
    let u = CFURL::from_file_path(&file).unwrap();
    let name_key = unsafe { kCFURLNameKey }.unwrap();
    let size_key = unsafe { kCFURLFileSizeKey }.unwrap();
    let props = CFArray::from_CFTypes(&[name_key, size_key]);
    let create = |u: &CFURL, options: CFURLBookmarkCreationOptions, relative: Option<&CFURL>| {
        let mut e: *mut CFError = std::ptr::null_mut();
        let made =
            unsafe { CFURLCreateBookmarkData(None, Some(u), options, Some(props.as_opaque()), relative, &mut e) };
        (made, error(e))
    };
    let resolve = |data: &CFData, relative: Option<&CFURL>| {
        let mut stale = 9;
        let mut e: *mut CFError = std::ptr::null_mut();
        let made = unsafe {
            CFURLCreateByResolvingBookmarkData(
                None,
                Some(data),
                CFURLBookmarkResolutionOptions(0),
                relative,
                None,
                &mut stale,
                &mut e,
            )
        };
        (made, stale, error(e))
    };
    let (bookmark, e) = create(&u, CFURLBookmarkCreationOptions(0), None);
    c.eq("made", (bookmark.is_some(), e), (true, None));
    let bookmark = bookmark.unwrap();
    c.eq("starts with book", bookmark.to_vec().starts_with(b"book"), true);
    let (back, stale, e) = resolve(&bookmark, None);
    c.eq(
        "resolved",
        (back.as_deref().and_then(canonical), stale, e),
        (Some(std::fs::canonicalize(&file).unwrap()), 0, None),
    );
    let name = unsafe { CFURLCreateResourcePropertyForKeyFromBookmarkData(None, Some(name_key), Some(&bookmark)) };
    c.eq(
        "its name kept",
        name.map(|n| unsafe { &*CFRetained::as_ptr(&n).as_ptr().cast::<CFString>() }.to_string()),
        Some("f.txt".into()),
    );
    let size = unsafe { CFURLCreateResourcePropertyForKeyFromBookmarkData(None, Some(size_key), Some(&bookmark)) };
    c.eq("its size kept", size.as_deref().and_then(|v| number(v)), Some(5));
    let dir_key = unsafe { kCFURLIsDirectoryKey }.unwrap();
    let is_dir = unsafe { CFURLCreateResourcePropertyForKeyFromBookmarkData(None, Some(dir_key), Some(&bookmark)) };
    c.eq("whether it is a directory, kept anyway", is_dir.is_some(), true);
    let kept =
        unsafe { CFURLCreateResourcePropertiesForKeysFromBookmarkData(None, Some(props.as_opaque()), Some(&bookmark)) };
    c.eq("its properties", kept.map(|d| d.count()), Some(2));

    // Relative to a directory.
    let base = CFURL::from_file_path(&dir).unwrap();
    let (relative, _) = create(&u, CFURLBookmarkCreationOptions(0), Some(&base));
    let (back, _, e) = resolve(&relative.unwrap(), Some(&base));
    c.eq("relative", (back.as_deref().and_then(canonical), e), (Some(std::fs::canonicalize(&file).unwrap()), None));

    // Bookmark files take bookmarks made for them.
    let alias = CFURL::from_file_path(dir.join("alias")).unwrap();
    let mut e: *mut CFError = std::ptr::null_mut();
    let ok = unsafe { CFURLWriteBookmarkDataToFile(Some(&bookmark), Some(&alias), 0, &mut e) };
    c.eq("not for a file", (ok, error(e)), (false, cocoa(512)));
    let (for_file, _) = create(&u, CFURLBookmarkCreationOptions::SuitableForBookmarkFile, None);
    let mut e: *mut CFError = std::ptr::null_mut();
    let ok = unsafe { CFURLWriteBookmarkDataToFile(for_file.as_deref(), Some(&alias), 0, &mut e) };
    c.eq("written", (ok, error(e)), (true, None));
    let mut e: *mut CFError = std::ptr::null_mut();
    let read = unsafe { CFURLCreateBookmarkDataFromFile(None, Some(&alias), &mut e) };
    c.eq("read back", (read.is_some(), error(e)), (true, None));
    let (back, _, _) = resolve(&read.unwrap(), None);
    c.eq("read back resolves", back.as_deref().and_then(canonical), Some(std::fs::canonicalize(&file).unwrap()));
    let mut e: *mut CFError = std::ptr::null_mut();
    let read = unsafe { CFURLCreateBookmarkDataFromFile(None, Some(&u), &mut e) };
    c.eq("not a bookmark file", (read.is_none(), error(e)), (true, cocoa(256)));
    let nowhere = CFURL::from_file_path(dir.join("nowhere")).unwrap();
    let mut e: *mut CFError = std::ptr::null_mut();
    let read = unsafe { CFURLCreateBookmarkDataFromFile(None, Some(&nowhere), &mut e) };
    c.eq("no bookmark file", (read.is_none(), error(e)), (true, cocoa(260)));

    // A renamed file is found, and the bookmark is stale.
    std::fs::rename(&file, dir.join("g.txt")).unwrap();
    let (back, stale, e) = resolve(&bookmark, None);
    c.eq(
        "renamed",
        (back.as_deref().and_then(canonical), stale, e),
        (Some(std::fs::canonicalize(dir.join("g.txt")).unwrap()), 1, None),
    );
    std::fs::remove_file(dir.join("g.txt")).unwrap();
    let (back, _, e) = resolve(&bookmark, None);
    c.eq("gone", (back.is_none(), e), (true, cocoa(4)));
    let (made, e) = create(&CFURL::from_file_path(dir.join("missing")).unwrap(), CFURLBookmarkCreationOptions(0), None);
    c.eq("nothing to bookmark", (made.is_none(), e), (true, cocoa(260)));
    let junk = CFData::from_bytes(b"not a bookmark");
    let (back, _, e) = resolve(&junk, None);
    c.eq("not a bookmark", (back.is_none(), e), (true, cocoa(259)));
    c.eq(
        "no properties in junk",
        unsafe { CFURLCreateResourcePropertyForKeyFromBookmarkData(None, Some(name_key), Some(&junk)) }.is_none(),
        true,
    );
    // An alias record makes a bookmark that doesn't resolve.
    let from_alias = unsafe { CFURLCreateBookmarkDataFromAliasRecord(None, Some(&junk)) };
    c.eq("from an alias record", from_alias.is_some(), true);
    let (back, _, e) = resolve(&from_alias.unwrap(), None);
    c.eq("an alias resolves to nothing", (back.is_none(), e), (true, cocoa(4)));
    // Web URLs are bookmarked as they are.
    let web = url("http://h/x", None);
    let (made, e) = create(&web, CFURLBookmarkCreationOptions(0), None);
    c.eq("a web URL", (made.is_some(), e), (true, None));
    let _ = std::fs::remove_dir_all(&dir);
    c.done();
}

#[test]
fn resource_keys() {
    use objc2_core_foundation::*;
    let keys = unsafe {
        [
            (kCFURLNameKey, "NSURLNameKey"),
            (kCFURLIsDirectoryKey, "NSURLIsDirectoryKey"),
            (kCFURLFileSizeKey, "NSURLFileSizeKey"),
            (kCFURLCustomIconKey, "kCFURLCustomIconKey"),
            (kCFURLEffectiveIconKey, "kCFURLEffectiveIconKey"),
            (kCFURLLabelColorKey, "kCFURLLabelColorKey"),
            (kCFURLIsApplicationKey, "_NSURLIsApplicationKey"),
            (kCFURLPathKey, "_NSURLPathKey"),
            (kCFURLFileResourceTypeRegular, "NSURLFileResourceTypeRegular"),
            (kCFURLUbiquitousItemDownloadingStatusCurrent, "NSURLUbiquitousItemDownloadingStatusCurrent"),
            (kCFURLVolumeAvailableCapacityForImportantUsageKey, "NSURLVolumeAvailableCapacityForImportantUsageKey"),
        ]
    };
    for (key, want) in keys {
        assert_eq!(key.map(|k| k.to_string()).as_deref(), Some(want));
    }
}
