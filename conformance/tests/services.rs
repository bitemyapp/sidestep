//! Foundation's value and system classes, checked on macOS and on Linux
//! alike: `NSData`, `NSURL`, `NSURLComponents`, `NSError`,
//! `NSFileManager`, the path and runtime functions, `NSProcessInfo`,
//! `NSUUID`, `NSBundle`, `NSPropertyListSerialization`,
//! `NSJSONSerialization`, the locks, and CoreFoundation's toll-free
//! bridged functions.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, ClassType, msg_send};
use objc2_foundation::{
    NSBundle, NSComparisonResult, NSData, NSDataBase64DecodingOptions, NSDataBase64EncodingOptions,
    NSDataReadingOptions, NSDataSearchOptions, NSDataWritingOptions, NSDate, NSDictionary, NSError, NSFileManager,
    NSMutableData, NSObjectProtocol, NSOperatingSystemVersion, NSProcessInfo, NSPropertyListFormat,
    NSPropertyListMutabilityOptions, NSPropertyListSerialization, NSRange, NSSearchPathDirectory,
    NSSearchPathDomainMask, NSString, NSURL, NSURLComponents, NSURLQueryItem, NSUUID,
};

use sidestep as _;

fn data(bytes: &[u8]) -> Retained<NSData> {
    NSData::with_bytes(bytes)
}

fn bytes_ptr(data: &NSData) -> *const c_void {
    unsafe { msg_send![data, bytes] }
}

#[test]
fn data_equality_and_hash() {
    let (a, b, c) = (data(b"hello world"), data(b"hello world"), data(b"hello worle"));
    assert!(a.isEqual(Some(&b)));
    assert!(a.isEqualToData(&b));
    assert_eq!(a.hash(), b.hash());
    assert!(!a.isEqual(Some(&c)));
    // Mutable and immutable data with the same bytes are equal.
    assert!(NSMutableData::with_bytes(b"hello world").isEqual(Some(&a)));
    assert_eq!(a.length(), 11);
    assert_eq!(a.to_vec(), b"hello world");
}

#[test]
fn empty_data_has_no_bytes() {
    assert!(bytes_ptr(&NSData::new()).is_null());
    assert!(bytes_ptr(&data(&[])).is_null());
    assert_eq!(NSData::new().length(), 0);
    // Mutable data always has somewhere to write.
    let mutable = NSMutableData::new();
    let ptr: *mut c_void = unsafe { msg_send![&*mutable, mutableBytes] };
    assert!(!ptr.is_null());
}

#[test]
fn subdata_ranges_and_bytes() {
    let a = data(b"hello world");
    assert_eq!(a.subdataWithRange(NSRange::new(6, 5)).to_vec(), b"world");
    let mut buf = [0u8; 4];
    unsafe { a.getBytes_length(NonNull::new(buf.as_mut_ptr().cast()).unwrap(), 4) };
    assert_eq!(&buf, b"hell");
    let mut buf = [7u8; 13];
    unsafe { a.getBytes_length(NonNull::new(buf.as_mut_ptr().cast()).unwrap(), 13) };
    assert_eq!(&buf, b"hello world\x07\x07", "only what the data has is copied");
    let mut buf = [0u8; 3];
    unsafe { a.getBytes_range(NonNull::new(buf.as_mut_ptr().cast()).unwrap(), NSRange::new(2, 3)) };
    assert_eq!(&buf, b"llo");

    let hay = data(b"abcabcabc");
    let find = |needle: &[u8], options: usize, location: usize, length: usize| {
        let range =
            hay.rangeOfData_options_range(&data(needle), NSDataSearchOptions(options), NSRange::new(location, length));
        (range.location, range.length)
    };
    let not_found = (isize::MAX as usize, 0);
    assert_eq!(find(b"bc", 0, 0, 9), (1, 2));
    assert_eq!(find(b"bc", 1, 0, 9), (7, 2), "backwards");
    assert_eq!(find(b"ab", 2, 0, 9), (0, 2), "anchored");
    assert_eq!(find(b"bc", 2, 0, 9), not_found);
    assert_eq!(find(b"bc", 3, 0, 9), (7, 2), "backwards and anchored: at the end");
    assert_eq!(find(b"ab", 3, 0, 9), not_found);
    assert_eq!(find(b"bc", 0, 2, 7), (4, 2), "within the range");
    assert_eq!(find(b"", 0, 0, 9), not_found);
    assert_eq!(find(b"zz", 0, 0, 9), not_found);
}

#[test]
fn data_descriptions() {
    assert_eq!(data(&[1, 2, 3]).description().to_string(), "{length = 3, bytes = 0x010203}");
    assert_eq!(NSData::new().description().to_string(), "{length = 0, bytes = 0x}");
    let bytes: Vec<u8> = (0..24).collect();
    assert_eq!(
        data(&bytes).description().to_string(),
        "{length = 24, bytes = 0x000102030405060708090a0b0c0d0e0f1011121314151617}"
    );
    let bytes: Vec<u8> = (0..40).collect();
    assert_eq!(
        data(&bytes).description().to_string(),
        "{length = 40, bytes = 0x00010203 04050607 08090a0b 0c0d0e0f ... 20212223 24252627 }"
    );
    let mutable: Retained<NSData> = NSMutableData::with_bytes(b"abc").into_super();
    assert_eq!(mutable.description().to_string(), "{length = 3, bytes = 0x616263}");
}

#[test]
fn base64() {
    let encode = |bytes: &[u8], options: usize| {
        data(bytes).base64EncodedStringWithOptions(NSDataBase64EncodingOptions(options)).to_string()
    };
    assert_eq!(encode(b"hello", 0), "aGVsbG8=");
    assert_eq!(encode(b"", 0), "");
    let long: Vec<u8> = (0..100).collect();
    let plain = encode(&long, 0);
    assert!(!plain.contains('\r') && !plain.contains('\n'));
    let lines = |text: &str, ending: &str| text.split(ending).map(str::len).collect::<Vec<_>>();
    assert_eq!(lines(&encode(&long, 1), "\r\n"), [64, 64, 8], "64-character lines end with CR LF by default");
    assert_eq!(lines(&encode(&long, 2), "\r\n"), [76, 60]);
    assert_eq!(lines(&encode(&long, 1 | 16), "\r"), [64, 64, 8]);
    assert_eq!(lines(&encode(&long, 1 | 32), "\n"), [64, 64, 8]);
    assert_eq!(lines(&encode(&long, 1 | 16 | 32), "\r\n"), [64, 64, 8]);
    assert_eq!(encode(&long, 16), plain, "line endings need a line length");
    assert_eq!(encode(&long, 1 | 2), plain, "both lengths: no lines");
    assert_eq!(encode(&[0; 48], 1).len(), 64, "no ending after the last line");
    assert_eq!(data(b"hello").base64EncodedDataWithOptions(NSDataBase64EncodingOptions(0)).to_vec(), b"aGVsbG8=");

    let decode = |text: &str, options: usize| {
        NSData::initWithBase64EncodedString_options(
            NSData::alloc(),
            &NSString::from_str(text),
            NSDataBase64DecodingOptions(options),
        )
        .map(|d| d.to_vec())
    };
    let hello = Some(b"hello".to_vec());
    assert_eq!(decode("aGVsbG8=", 0), hello);
    assert_eq!(decode("", 0), Some(Vec::new()));
    assert_eq!(decode("aGVsbG8", 0), None, "padding is required");
    assert_eq!(decode("aGV$sbG8=", 0), None);
    assert_eq!(decode("aGVs\r\nbG8=", 0), None);
    assert_eq!(decode("aGVs bG8=", 0), None);
    assert_eq!(decode("aG==VsbG8=", 0), None);
    assert_eq!(decode("a", 0), None);
    assert_eq!(decode("aGVs=", 0), None);
    assert_eq!(decode("aGVsbG9=", 0), hello, "left-over bits are ignored");
    assert_eq!(decode("aGVsbG8==", 0), hello, "extra padding is tolerated");
    assert_eq!(decode("-_8=", 0), None, "the URL-safe alphabet isn't base64");
    // Ignoring unknown characters skips them, but then wants exact padding.
    assert_eq!(decode("aGV$sbG8=", 1), hello);
    assert_eq!(decode("aGVs\r\nbG8=", 1), hello);
    assert_eq!(decode("aGVs\n", 1), Some(b"hel".to_vec()));
    assert_eq!(decode("aGU==", 1), None);
    let from_data =
        NSData::initWithBase64EncodedData_options(NSData::alloc(), &data(b"aGVsbG8="), NSDataBase64DecodingOptions(0));
    assert_eq!(from_data.unwrap().to_vec(), b"hello");
}

#[test]
fn mutable_data() {
    let m = NSMutableData::with_bytes(b"abcdef");
    m.increaseLengthBy(3);
    assert_eq!(m.to_vec(), b"abcdef\0\0\0");
    m.setLength(2);
    m.setLength(4);
    assert_eq!(m.to_vec(), b"ab\0\0", "growing zero-fills");
    let m = NSMutableData::with_bytes(b"abcdef");
    unsafe { m.replaceBytesInRange_withBytes_length(NSRange::new(1, 2), b"XYZW".as_ptr().cast(), 4) };
    assert_eq!(m.to_vec(), b"aXYZWdef");
    unsafe { m.replaceBytesInRange_withBytes_length(NSRange::new(1, 4), b"Q".as_ptr().cast(), 1) };
    assert_eq!(m.to_vec(), b"aQdef");
    unsafe {
        m.replaceBytesInRange_withBytes(NSRange::new(0, 2), NonNull::new(b"MN".as_ptr().cast_mut().cast()).unwrap())
    };
    assert_eq!(m.to_vec(), b"MNdef");
    m.resetBytesInRange(NSRange::new(0, 2));
    assert_eq!(m.to_vec(), b"\0\0def");
    m.setData(&data(b"zz"));
    m.appendData(&data(b"yy"));
    m.extend_from_slice(b"x");
    assert_eq!(m.to_vec(), b"zzyyx");
    assert_eq!(NSMutableData::dataWithLength(3).unwrap().to_vec(), [0, 0, 0]);
    assert_eq!(NSMutableData::dataWithCapacity(10).unwrap().length(), 0);
}

#[test]
fn copies() {
    let mutable = NSMutableData::with_bytes(b"abc");
    let copy: Retained<NSData> = unsafe { msg_send![&*mutable, copy] };
    let is_mutable: bool = unsafe { msg_send![&*copy, isKindOfClass: NSMutableData::class()] };
    assert!(!is_mutable, "a copy of mutable data is immutable");
    assert_eq!(copy.to_vec(), b"abc");
    mutable.extend_from_slice(b"d");
    assert_eq!(copy.to_vec(), b"abc");

    let immutable = data(b"imm");
    let copy: Retained<NSData> = unsafe { msg_send![&*immutable, copy] };
    assert!(std::ptr::eq(&*copy, &*immutable), "a copy of immutable data is the same object");
    let mutable_copy: Retained<NSMutableData> = unsafe { msg_send![&*immutable, mutableCopy] };
    let is_mutable: bool = unsafe { msg_send![&*mutable_copy, isKindOfClass: NSMutableData::class()] };
    assert!(is_mutable);
}

#[test]
fn no_copy_deallocator_runs_once() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut buffer = vec![1u8, 2, 3, 4];
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    {
        let c = calls.clone();
        let deallocator = RcBlock::new(move |p: NonNull<c_void>, len: usize| {
            c.lock().unwrap().push((p.as_ptr() as usize, len));
            drop(unsafe { Vec::from_raw_parts(p.as_ptr().cast::<u8>(), len, 4) });
        });
        let d = unsafe {
            NSData::initWithBytesNoCopy_length_deallocator(
                NSData::alloc(),
                NonNull::new(ptr.cast()).unwrap(),
                4,
                Some(&deallocator),
            )
        };
        assert_eq!(bytes_ptr(&d), ptr.cast_const().cast(), "no copy");
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(d.to_vec(), [1, 2, 3, 4]);
    }
    assert_eq!(*calls.lock().unwrap(), [(ptr as usize, 4)]);

    // Vec-backed data through objc2's from_vec takes the same route.
    let from_vec = NSData::from_vec(vec![5, 6, 7]);
    assert_eq!(from_vec.to_vec(), [5, 6, 7]);
}

#[test]
fn files() {
    let path = std::env::temp_dir().join(format!("sidestep-data-{}", std::process::id()));
    let path_string = NSString::from_str(path.to_str().unwrap());
    let d = data(b"file contents");
    assert!(d.writeToFile_atomically(&path_string, true));
    assert_eq!(NSData::dataWithContentsOfFile(&path_string).unwrap().to_vec(), b"file contents");
    assert!(data(b"again").writeToFile_atomically(&path_string, false));
    assert_eq!(std::fs::read(&path).unwrap(), b"again");
    std::fs::remove_file(&path).unwrap();
    assert!(NSData::dataWithContentsOfFile(&path_string).is_none());
    assert!(!d.writeToFile_atomically(&NSString::from_str("/nonexistent-dir/x"), true));
}

/// An atomic write replaces the file but keeps its permissions; a new
/// file gets the usual ones. No temporary file is left behind.
#[test]
fn atomic_writes_keep_permissions() {
    use objc2_foundation::NSDataWritingOptions;
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("permissions");
    let path = dir.join("secret");
    let path_string = NSString::from_str(path.to_str().unwrap());
    let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    std::fs::write(&path, b"one").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(data(b"two").writeToFile_atomically(&path_string, true));
    assert_eq!(std::fs::read(&path).unwrap(), b"two");
    assert_eq!(mode(&path), 0o600);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    data(b"three").writeToFile_options_error(&path_string, NSDataWritingOptions::Atomic).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"three");
    assert_eq!(mode(&path), 0o640);
    let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(entries, [std::ffi::OsString::from("secret")], "no temporary file left");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Empty data made without copying may have no buffer at all.
#[test]
fn empty_data_without_a_buffer() {
    use objc2_core_foundation::{CFData, kCFAllocatorNull};
    let empty = unsafe { CFData::with_bytes_no_copy(None, std::ptr::null(), 0, kCFAllocatorNull) }.unwrap();
    assert_eq!(empty.len(), 0);
    assert!(empty.to_vec().is_empty());
    let ns_data: &NSData = unsafe { &*(objc2_core_foundation::CFRetained::as_ptr(&empty).as_ptr() as *const NSData) };
    assert!(ns_data.isEqualToData(&NSData::new()));
    let mutable = NSMutableData::new();
    unsafe {
        let _: () = msg_send![&*mutable, appendBytes: std::ptr::null::<c_void>(), length: 0usize];
        let _: () = msg_send![&*mutable, getBytes: std::ptr::null_mut::<c_void>(), length: 0usize];
    }
    assert_eq!(mutable.length(), 0);
}

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn text(value: Option<Retained<NSString>>) -> Option<String> {
    value.map(|v| v.to_string())
}

fn url(string: &str) -> Retained<NSURL> {
    NSURL::URLWithString(&ns(string)).unwrap_or_else(|| panic!("{string:?} makes a URL"))
}

fn absolute(url: Option<Retained<NSURL>>) -> Option<String> {
    url.and_then(|u| text(u.absoluteString()))
}

fn describe(object: &AnyObject) -> String {
    let text: Retained<NSString> = unsafe { msg_send![object, description] };
    text.to_string()
}

/// A scratch directory of this process's own.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sidestep-services-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn url_components_of_strings() {
    // (string, absoluteString, scheme, host, path, query, fragment, resourceSpecifier, hasDirectoryPath)
    type Row = (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        bool,
    );
    let rows: &[Row] = &[
        ("", "", None, None, None, None, None, Some(""), false),
        ("a b", "a%20b", None, None, Some("a b"), None, None, Some("a%20b"), false),
        (
            "http://[::1]:8080/p%20q?x=1#f",
            "http://[::1]:8080/p%20q?x=1#f",
            Some("http"),
            Some("::1"),
            Some("/p q"),
            Some("x=1"),
            Some("f"),
            Some("//[::1]:8080/p%20q?x=1#f"),
            false,
        ),
        ("%zz", "%25zz", None, None, Some("%zz"), None, None, Some("%25zz"), false),
        (
            "http://example.com/\u{e9}",
            "http://example.com/%C3%A9",
            Some("http"),
            Some("example.com"),
            Some("/\u{e9}"),
            None,
            None,
            Some("//example.com/%C3%A9"),
            false,
        ),
        ("http://host", "http://host", Some("http"), Some("host"), Some(""), None, None, Some("//host"), false),
        ("http://host/", "http://host/", Some("http"), Some("host"), Some("/"), None, None, Some("//host/"), true),
        (
            "http://host/a/b/",
            "http://host/a/b/",
            Some("http"),
            Some("host"),
            Some("/a/b"),
            None,
            None,
            Some("//host/a/b/"),
            true,
        ),
        (
            "mailto:someone@example.com",
            "mailto:someone@example.com",
            Some("mailto"),
            None,
            None,
            None,
            None,
            Some("someone@example.com"),
            false,
        ),
        (
            "file:///tmp/a%20b",
            "file:///tmp/a%20b",
            Some("file"),
            None,
            Some("/tmp/a b"),
            None,
            None,
            Some("/tmp/a%20b"),
            false,
        ),
        ("relative/path", "relative/path", None, None, Some("relative/path"), None, None, Some("relative/path"), false),
        ("//net/path", "//net/path", None, Some("net"), Some("/path"), None, None, Some("//net/path"), false),
        ("?q", "?q", None, None, None, Some("q"), None, Some("?q"), false),
        ("#f", "#f", None, None, None, None, Some("f"), Some("#f"), false),
        (
            "HTTP://Host.COM/Path",
            "HTTP://Host.COM/Path",
            Some("HTTP"),
            Some("Host.COM"),
            Some("/Path"),
            None,
            None,
            Some("//Host.COM/Path"),
            false,
        ),
        (
            "http://h/a/./b/../c",
            "http://h/a/./b/../c",
            Some("http"),
            Some("h"),
            Some("/a/./b/../c"),
            None,
            None,
            Some("//h/a/./b/../c"),
            false,
        ),
        (
            "data:text/plain,hi",
            "data:text/plain,hi",
            Some("data"),
            None,
            None,
            None,
            None,
            Some("text/plain,hi"),
            false,
        ),
        (
            "http://[fe80::1%25en0]/",
            "http://[fe80::1%25en0]/",
            Some("http"),
            Some("fe80::1%en0"),
            Some("/"),
            None,
            None,
            Some("//[fe80::1%25en0]/"),
            true,
        ),
        (
            "http://host/p%",
            "http://host/p%25",
            Some("http"),
            Some("host"),
            Some("/p%"),
            None,
            None,
            Some("//host/p%25"),
            false,
        ),
        (
            "http://ex.com/?q=\u{e9}",
            "http://ex.com/?q=%C3%A9",
            Some("http"),
            Some("ex.com"),
            Some("/"),
            Some("q=%C3%A9"),
            None,
            Some("//ex.com/?q=%C3%A9"),
            true,
        ),
        ("http:", "http:", Some("http"), None, None, None, None, Some(""), false),
        ("http:/", "http:/", Some("http"), None, Some("/"), None, None, Some("/"), true),
        ("http://", "http://", Some("http"), None, None, None, None, None, false),
        ("urn:isbn:123", "urn:isbn:123", Some("urn"), None, None, None, None, Some("isbn:123"), false),
        (
            "http://host:/p",
            "http://host:/p",
            Some("http"),
            Some("host"),
            Some("/p"),
            None,
            None,
            Some("//host:/p"),
            false,
        ),
        (
            "http://h/%7Euser",
            "http://h/%7Euser",
            Some("http"),
            Some("h"),
            Some("/~user"),
            None,
            None,
            Some("//h/%7Euser"),
            false,
        ),
    ];
    for &(string, abs, scheme, host, path, query, fragment, rspec, dir) in rows {
        let u = url(string);
        assert_eq!(text(u.absoluteString()).as_deref(), Some(abs), "absoluteString of {string:?}");
        assert_eq!(u.relativeString().to_string(), abs, "relativeString of {string:?}");
        assert_eq!(text(u.scheme()).as_deref(), scheme, "scheme of {string:?}");
        assert_eq!(text(u.host()).as_deref(), host, "host of {string:?}");
        assert_eq!(text(u.path()).as_deref(), path, "path of {string:?}");
        assert_eq!(text(u.relativePath()).as_deref(), path, "relativePath of {string:?}");
        assert_eq!(text(u.query()).as_deref(), query, "query of {string:?}");
        assert_eq!(text(u.fragment()).as_deref(), fragment, "fragment of {string:?}");
        assert_eq!(text(u.resourceSpecifier()).as_deref(), rspec, "resourceSpecifier of {string:?}");
        assert_eq!(u.hasDirectoryPath(), dir, "hasDirectoryPath of {string:?}");
        assert!(u.baseURL().is_none());
    }

    let u = url("http://user:pw@host:80/p/a.txt;params?q=a%20b#frag%20x");
    assert_eq!(text(u.user()).as_deref(), Some("user"));
    assert_eq!(text(u.password()).as_deref(), Some("pw"));
    assert_eq!(text(u.path()).as_deref(), Some("/p/a.txt;params"), "parameters stay in the path");
    assert_eq!(text(u.query()).as_deref(), Some("q=a%20b"), "queries stay encoded");
    assert_eq!(text(u.fragment()).as_deref(), Some("frag%20x"));
    assert_eq!(text(u.lastPathComponent()).as_deref(), Some("a.txt;params"));
    assert_eq!(text(u.pathExtension()).as_deref(), Some("txt;params"));
    let u = url("http://a%20b:c%40d@h/");
    assert_eq!(text(u.user()).as_deref(), Some("a b"), "users are decoded");
    assert_eq!(text(u.password()).as_deref(), Some("c%40d"), "passwords are not");
    assert_eq!(text(url("http://u:@h/").password()).as_deref(), Some(""));
    assert_eq!(text(url("http://@h/").user()).as_deref(), Some(""));
    assert_eq!(text(url("http://%41/").host()).as_deref(), Some("A"));
    assert_eq!(text(url("http://h/%FF").path()), None, "paths that aren't UTF-8 are nil");
    assert_eq!(text(url("http://h/a%2Fb").path()).as_deref(), Some("/a/b"));
    assert!(url("FILE:///tmp/x").isFileURL());
    assert!(!url("http://h/x").isFileURL());

    // Characters a URL can't hold are encoded, unless the caller says not to.
    for string in ["a b", "http://example.com/\u{e9}", "%zz", "http://h/a b"] {
        assert!(NSURL::URLWithString_encodingInvalidCharacters(&ns(string), false).is_none(), "{string:?}");
    }
    assert!(NSURL::URLWithString_encodingInvalidCharacters(&ns("http://h/ok"), false).is_some());
    for string in ["http://host:abc/p", "http://h:-1/", "1http://h", "ht tp://h"] {
        // A port that isn't a number makes no URL; a bad scheme makes a
        // relative one or none.
        let made = NSURL::URLWithString(&ns(string));
        assert!(made.as_ref().is_none_or(|u| u.scheme().is_none()), "{string:?}");
    }
    assert!(NSURL::URLWithString(&ns("http://host:abc/p")).is_none());
}

#[test]
fn url_ports_and_path_components() {
    use objc2_foundation::{NSArray, NSNumber};
    let port = |string: &str| -> Option<isize> {
        let port: Option<Retained<NSNumber>> = url(string).port();
        port.map(|p| p.integerValue())
    };
    assert_eq!(port("http://[::1]:8080/"), Some(8080));
    assert_eq!(port("http://h:0080/"), Some(80));
    assert_eq!(port("http://h:0/"), Some(0));
    assert_eq!(port("http://host:/p"), None);
    assert_eq!(port("http://h/"), None);
    let components = |string: &str| -> Option<Vec<String>> {
        url(string).pathComponents().map(|a| a.iter().map(|c| c.to_string()).collect())
    };
    assert_eq!(components("http://h/a/b%20c/"), Some(vec!["/".into(), "a".into(), "b c".into()]));
    assert_eq!(components("file:///"), Some(vec!["/".into()]));
    assert_eq!(components("http://h"), Some(vec![]));
    assert_eq!(components("mailto:x@y"), None);
    let parts = NSArray::from_retained_slice(&[ns("/"), ns("a"), ns("b c")]);
    assert_eq!(absolute(NSURL::fileURLWithPathComponents(&parts)).as_deref(), Some("file:///a/b%20c"));
}

#[test]
fn url_relative_resolution() {
    let base = url("http://a/b/c/d;p?q");
    for (reference, expected, path) in [
        ("g", "http://a/b/c/g", Some("/b/c/g")),
        ("./g", "http://a/b/c/g", Some("/b/c/g")),
        ("g/", "http://a/b/c/g/", Some("/b/c/g")),
        ("/g", "http://a/g", Some("/g")),
        ("//g", "http://g", Some("")),
        ("?y", "http://a/b/c/d;p?y", Some("/b/c/d;p")),
        ("g?y", "http://a/b/c/g?y", Some("/b/c/g")),
        ("#s", "http://a/b/c/d;p?q#s", Some("/b/c/d;p")),
        ("g#s", "http://a/b/c/g#s", Some("/b/c/g")),
        (";x", "http://a/b/c/;x", Some("/b/c/;x")),
        ("", "http://a/b/c/d;p?q", Some("/b/c/d;p")),
        (".", "http://a/b/c/", Some("/b/c")),
        ("..", "http://a/b/", Some("/b")),
        ("../g", "http://a/b/g", Some("/b/g")),
        ("../..", "http://a/", Some("/")),
        ("../../g", "http://a/g", Some("/g")),
        ("../../../g", "http://a/../g", Some("/../g")),
        ("/./g", "http://a/./g", Some("/./g")),
        ("/../g", "http://a/../g", Some("/../g")),
        ("g.", "http://a/b/c/g.", Some("/b/c/g.")),
        ("..g", "http://a/b/c/..g", Some("/b/c/..g")),
        ("./../g", "http://a/b/g", Some("/b/g")),
        ("./g/.", "http://a/b/c/g/", Some("/b/c/g")),
        ("g/../h", "http://a/b/c/h", Some("/b/c/h")),
        ("g;x=1/../y", "http://a/b/c/y", Some("/b/c/y")),
        ("g?y/../x", "http://a/b/c/g?y/../x", Some("/b/c/g")),
        ("g#s/../x", "http://a/b/c/g#s/../x", Some("/b/c/g")),
    ] {
        let u = NSURL::URLWithString_relativeToURL(&ns(reference), Some(&base)).unwrap();
        assert_eq!(text(u.absoluteString()).as_deref(), Some(expected), "{reference:?}");
        assert_eq!(text(u.path()).as_deref(), path, "path of {reference:?}");
        assert_eq!(u.relativeString().to_string(), reference);
        assert!(u.baseURL().is_some(), "{reference:?} keeps its base");
    }
    // A string with a scheme needs no base.
    for reference in ["g:h", "http:g"] {
        let u = NSURL::URLWithString_relativeToURL(&ns(reference), Some(&base)).unwrap();
        assert!(u.baseURL().is_none(), "{reference:?}");
        assert_eq!(text(u.absoluteString()).as_deref(), Some(reference));
    }

    let rel = NSURL::URLWithString_relativeToURL(&ns("sub/file.txt"), Some(&url("http://h/dir/"))).unwrap();
    assert_eq!(text(rel.scheme()).as_deref(), Some("http"));
    assert_eq!(text(rel.host()).as_deref(), Some("h"));
    assert_eq!(text(rel.path()).as_deref(), Some("/dir/sub/file.txt"));
    assert_eq!(text(rel.relativePath()).as_deref(), Some("sub/file.txt"));
    assert_eq!(text(rel.resourceSpecifier()).as_deref(), Some("sub/file.txt"));
    assert_eq!(describe(&rel), "sub/file.txt -- http://h/dir/");
    assert_eq!(describe(&url("http://h/x")), "http://h/x");
    let abs = rel.absoluteURL().unwrap();
    assert!(abs.baseURL().is_none());
    assert_eq!(abs.relativeString().to_string(), "http://h/dir/sub/file.txt");
}

#[test]
fn file_urls() {
    let cwd = std::env::current_dir().unwrap();
    let f = NSURL::fileURLWithPath(&ns("rel/file.txt"));
    assert!(f.isFileURL());
    assert_eq!(f.relativeString().to_string(), "rel/file.txt");
    let base = f.baseURL().expect("relative file URLs have the current directory as base");
    assert_eq!(text(base.path()).as_deref(), cwd.to_str());
    assert!(base.hasDirectoryPath());
    assert_eq!(text(f.path()).as_deref(), cwd.join("rel/file.txt").to_str());
    assert_eq!(text(f.relativePath()).as_deref(), Some("rel/file.txt"));
    let fs = unsafe { std::ffi::CStr::from_ptr(f.fileSystemRepresentation().as_ptr()) };
    assert_eq!(fs.to_str().ok(), cwd.join("rel/file.txt").to_str(), "absolute, from the resolved path");

    let dir = scratch("fileurl");
    let dir_string = dir.to_str().unwrap();
    let u = NSURL::fileURLWithPath(&ns(dir_string));
    assert!(u.hasDirectoryPath(), "existing directories gain a slash");
    assert_eq!(text(u.absoluteString()), Some(format!("file://{dir_string}/")));
    assert_eq!(text(u.path()).as_deref(), Some(dir_string));
    let missing = format!("{dir_string}/missing");
    let u = NSURL::fileURLWithPath(&ns(&missing));
    assert!(!u.hasDirectoryPath());
    assert_eq!(text(u.absoluteString()), Some(format!("file://{missing}")));
    // A plain append to a file URL looks at the file system too.
    std::fs::create_dir(dir.join("sub")).unwrap();
    let appended = NSURL::fileURLWithPath(&ns(dir_string)).URLByAppendingPathComponent(&ns("sub")).unwrap();
    assert!(appended.hasDirectoryPath());
    let appended = NSURL::fileURLWithPath(&ns(dir_string)).URLByAppendingPathComponent(&ns("nope")).unwrap();
    assert!(!appended.hasDirectoryPath());

    for (path, is_dir, expected) in [
        ("/a b/c/", false, "file:///a%20b/c"),
        ("/a/b/../c/./d", false, "file:///a/b/../c/./d"),
        ("/a//b", false, "file:///a//b"),
        ("/a/b?c#d%e", false, "file:///a/b%3Fc%23d%25e"),
        ("/x", true, "file:///x/"),
        ("/", true, "file:///"),
    ] {
        let u = NSURL::fileURLWithPath_isDirectory(&ns(path), is_dir);
        assert_eq!(text(u.absoluteString()).as_deref(), Some(expected), "{path:?}");
    }
    let u = NSURL::fileURLWithPath_isDirectory(&ns("/a/b?c#d%e"), false);
    assert_eq!(text(u.path()).as_deref(), Some("/a/b?c#d%e"));
    assert_eq!(text(u.lastPathComponent()).as_deref(), Some("b?c#d%e"));
    let u = NSURL::fileURLWithPath_isDirectory(&ns("/a b/c"), false);
    let fs = unsafe { std::ffi::CStr::from_ptr(u.fileSystemRepresentation().as_ptr()) };
    assert_eq!(fs.to_bytes(), b"/a b/c");
    let fs = unsafe { std::ffi::CStr::from_ptr(url("http://h/a%20b").fileSystemRepresentation().as_ptr()) };
    assert_eq!(fs.to_bytes(), b"/a b", "any URL's path");

    let u = NSURL::fileURLWithPath_relativeToURL(
        &ns("x/y"),
        Some(&NSURL::fileURLWithPath_isDirectory(&ns("/base/dir/"), true)),
    );
    assert_eq!(text(u.absoluteString()).as_deref(), Some("file:///base/dir/x/y"));
    assert_eq!(u.relativeString().to_string(), "x/y");
    let u = NSURL::fileURLWithPath_isDirectory(&ns("~/x"), false);
    assert_eq!(u.relativeString().to_string(), "~/x", "no tilde expansion");

    let mut buf = [0 as std::ffi::c_char; 5];
    let fits = |u: &NSURL, buf: &mut [std::ffi::c_char]| unsafe {
        u.getFileSystemRepresentation_maxLength(NonNull::new(buf.as_mut_ptr()).unwrap(), buf.len())
    };
    assert!(!fits(&url("file:///abcd"), &mut buf), "no room for the NUL");
    assert!(fits(&url("file:///abc"), &mut buf));
    assert_eq!(&buf, b"/abc\0".map(|b| b as std::ffi::c_char).as_slice());
    assert!(!fits(&url("mailto:x"), &mut buf), "no path");

    assert_eq!(absolute(NSURL::fileURLWithPath(&ns("/x")).filePathURL()).as_deref(), Some("file:///x"));
    assert!(url("http://h/a").filePathURL().is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn url_derivations() {
    // (url, last, ext, deleting last, deleting ext, appending "c d", appending dir "e", appending ext "zip")
    for (string, last, ext, del_last, del_ext, app, app_dir, app_ext) in [
        (
            "http://h/a/b.txt",
            "b.txt",
            "txt",
            "http://h/a/",
            "http://h/a/b",
            "http://h/a/b.txt/c%20d",
            "http://h/a/b.txt/e/",
            "http://h/a/b.txt.zip",
        ),
        ("http://h/a/", "a", "", "http://h/", "http://h/a/", "http://h/a/c%20d", "http://h/a/e/", "http://h/a.zip/"),
        ("http://h/", "/", "", "http://h/../", "http://h/", "http://h/c%20d", "http://h/e/", "http://h/.zip"),
        ("http://h", "", "", "http://h", "http://h", "http://h/c%20d", "http://h/e/", "http://h"),
        (
            "file:///a/b.tar.gz",
            "b.tar.gz",
            "gz",
            "file:///a/",
            "file:///a/b.tar",
            "file:///a/b.tar.gz/c%20d",
            "file:///a/b.tar.gz/e/",
            "file:///a/b.tar.gz.zip",
        ),
        (
            "http://h/a/b?q=1#f",
            "b",
            "",
            "http://h/a/?q=1#f",
            "http://h/a/b?q=1#f",
            "http://h/a/b/c%20d?q=1#f",
            "http://h/a/b/e/?q=1#f",
            "http://h/a/b.zip?q=1#f",
        ),
        (
            "http://h/.bashrc",
            ".bashrc",
            "",
            "http://h/",
            "http://h/.bashrc",
            "http://h/.bashrc/c%20d",
            "http://h/.bashrc/e/",
            "http://h/.bashrc.zip",
        ),
        (
            "http://h/a.b/c",
            "c",
            "",
            "http://h/a.b/",
            "http://h/a.b/c",
            "http://h/a.b/c/c%20d",
            "http://h/a.b/c/e/",
            "http://h/a.b/c.zip",
        ),
        (
            "http://h/a/b.",
            "b.",
            "",
            "http://h/a/",
            "http://h/a/b.",
            "http://h/a/b./c%20d",
            "http://h/a/b./e/",
            "http://h/a/b..zip",
        ),
        (
            "http://h/..",
            "..",
            "",
            "http://h/../../",
            "http://h/..",
            "http://h/../c%20d",
            "http://h/../e/",
            "http://h/...zip",
        ),
        (
            "http://h/a%2Fb/c.t%2Ex",
            "c.t.x",
            "x",
            "http://h/a%2Fb/",
            "http://h/a%2Fb/c",
            "http://h/a%2Fb/c.t%2Ex/c%20d",
            "http://h/a%2Fb/c.t%2Ex/e/",
            "http://h/a%2Fb/c.t%2Ex.zip",
        ),
    ] {
        let u = url(string);
        assert_eq!(text(u.lastPathComponent()).as_deref(), Some(last), "lastPathComponent of {string:?}");
        assert_eq!(text(u.pathExtension()).as_deref(), Some(ext), "pathExtension of {string:?}");
        assert_eq!(
            absolute(u.URLByDeletingLastPathComponent()).as_deref(),
            Some(del_last),
            "deleting last of {string:?}"
        );
        assert_eq!(absolute(u.URLByDeletingPathExtension()).as_deref(), Some(del_ext), "deleting ext of {string:?}");
        assert_eq!(
            absolute(u.URLByAppendingPathComponent(&ns("c d"))).as_deref(),
            Some(app),
            "appending to {string:?}"
        );
        assert_eq!(
            absolute(u.URLByAppendingPathComponent_isDirectory(&ns("e"), true)).as_deref(),
            Some(app_dir),
            "appending a directory to {string:?}"
        );
        assert_eq!(
            absolute(u.URLByAppendingPathExtension(&ns("zip"))).as_deref(),
            Some(app_ext),
            "appending an extension to {string:?}"
        );
    }
    let u = url("http://h/a");
    assert_eq!(absolute(u.URLByAppendingPathComponent(&ns("/b"))).as_deref(), Some("http://h/a/b"));
    assert_eq!(absolute(u.URLByAppendingPathComponent(&ns("/q/"))).as_deref(), Some("http://h/a/q/"));
    assert_eq!(absolute(u.URLByAppendingPathComponent(&ns(""))).as_deref(), Some("http://h/a/"));
    assert_eq!(absolute(u.URLByAppendingPathComponent(&ns("q?x#y%"))).as_deref(), Some("http://h/a/q%3Fx%23y%25"));
    assert_eq!(absolute(u.URLByAppendingPathExtension(&ns(""))).as_deref(), Some("http://h/a"));
    assert!(u.URLByAppendingPathExtension(&ns("x/y")).is_none());
    let opaque = url("mailto:x@y");
    assert!(opaque.URLByDeletingLastPathComponent().is_none());
    assert!(opaque.URLByDeletingPathExtension().is_none());
    assert!(opaque.URLByAppendingPathExtension(&ns("e")).is_none());
    assert!(opaque.URLByAppendingPathComponent(&ns("q")).is_none());
    assert!(NSURL::URLWithString(&ns("")).unwrap().URLByDeletingLastPathComponent().is_none());

    // Relative URLs rewrite their own path and keep their base.
    let base = url("http://h/a/b/");
    for (reference, del_last, app) in [
        ("a", "./", "a/z"),
        ("a/b", "a/", "a/b/z"),
        ("..", "../../", "../z"),
        ("a/", "./", "a/z"),
        (".", "../", "./z"),
        ("../a.txt", "../", "../a.txt/z"),
    ] {
        let u = NSURL::URLWithString_relativeToURL(&ns(reference), Some(&base)).unwrap();
        let deleted = u.URLByDeletingLastPathComponent().unwrap();
        assert_eq!(deleted.relativeString().to_string(), del_last, "deleting last of {reference:?}");
        assert!(deleted.baseURL().is_some());
        let appended = u.URLByAppendingPathComponent(&ns("z")).unwrap();
        assert_eq!(appended.relativeString().to_string(), app, "appending to {reference:?}");
        assert!(appended.baseURL().is_some());
    }
}

#[test]
fn url_standardizing() {
    for (string, expected) in [
        ("http://h/a/./b/../c/", "http://h/a/c/"),
        ("http://a/../g", "http://a/g"),
        ("http://a/b/../../g", "http://a/g"),
        ("http://a/b/..", "http://a"),
        ("http://a/b/.", "http://a/b/"),
        ("http://h/a//b", "http://h/a//b"),
        ("http://a", "http://a"),
        ("http://h/a/b/..", "http://h/a"),
        ("http://h/a/b/../", "http://h/a/"),
        ("http://h/..", "http://h/"),
        ("http://h/a/..", "http://h"),
        ("http://h/a/../", "http://h/"),
        ("a/../../x", "x"),
        ("../x", "x"),
        ("./x/", "x/"),
        ("a/.", "a/"),
        ("/a/%2E%2E/b", "/a/%2E%2E/b"),
    ] {
        assert_eq!(absolute(url(string).standardizedURL()).as_deref(), Some(expected), "{string:?}");
    }
    let base = url("http://h/a/b/");
    for (reference, rel, abs) in
        [("../x/./y", "x/y", "http://h/a/b/x/y"), ("x/../y", "y", "http://h/a/b/y"), ("/p/../q", "/q", "http://h/q")]
    {
        let u = NSURL::URLWithString_relativeToURL(&ns(reference), Some(&base)).unwrap().standardizedURL().unwrap();
        assert_eq!(u.relativeString().to_string(), rel, "{reference:?}");
        assert!(u.baseURL().is_some(), "{reference:?} keeps its base");
        assert_eq!(text(u.absoluteString()).as_deref(), Some(abs));
    }

    let f = NSURL::fileURLWithPath_isDirectory(&ns("/a/./b/../c"), false);
    assert_eq!(absolute(f.URLByStandardizingPath()).as_deref(), Some("file:///a/c"));
    let f = NSURL::fileURLWithPath_isDirectory(&ns("/a/./b/"), true);
    assert_eq!(absolute(f.URLByStandardizingPath()).as_deref(), Some("file:///a/b/"));
    assert_eq!(
        absolute(url("http://h/a/../b").URLByStandardizingPath()).as_deref(),
        Some("http://h/a/../b"),
        "only file URLs"
    );
    let f = NSURL::fileURLWithPath_isDirectory(&ns("/nonexistent-sidestep/../a/./b"), false);
    assert_eq!(absolute(f.URLByResolvingSymlinksInPath()).as_deref(), Some("file:///a/b"));

    let dir = scratch("symlinks");
    let real = dir.join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
    let canonical = std::fs::canonicalize(&real).unwrap();
    let link = NSURL::fileURLWithPath(&ns(dir.join("link").to_str().unwrap()));
    let resolved = link.URLByResolvingSymlinksInPath().unwrap();
    assert_eq!(text(resolved.path()).as_deref(), canonical.to_str().map(|p| p.strip_prefix("/private").unwrap_or(p)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn url_equality_and_reachability() {
    let (a, b, c) = (url("http://A/b"), url("http://a/b"), url("http://A/b"));
    assert!(!a.isEqual(Some(&b)), "equality compares the strings");
    assert!(a.isEqual(Some(&c)));
    assert_eq!(a.hash(), c.hash());
    let relative = NSURL::URLWithString_relativeToURL(&ns("b"), Some(&url("http://a/"))).unwrap();
    assert!(!relative.isEqual(Some(&url("http://a/b"))), "a relative URL isn't the absolute one");
    assert!(!a.isEqual(Some(&ns("http://A/b"))));
    let copy: Retained<NSURL> = unsafe { msg_send![&*a, copy] };
    assert!(std::ptr::eq(&*copy, &*a), "URLs are immutable: a copy is the same object");

    let dir = scratch("reachable");
    assert!(NSURL::fileURLWithPath(&ns(dir.to_str().unwrap())).checkResourceIsReachableAndReturnError().is_ok());
    let error = NSURL::fileURLWithPath(&ns(dir.join("missing").to_str().unwrap()))
        .checkResourceIsReachableAndReturnError()
        .unwrap_err();
    assert_eq!(error.domain().to_string(), "NSCocoaErrorDomain");
    assert_eq!(error.code(), 260);
    let error = url("http://h/").checkResourceIsReachableAndReturnError().unwrap_err();
    assert_eq!((error.domain().to_string(), error.code()), ("NSCocoaErrorDomain".to_string(), 262));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[allow(deprecated)] // percentEncodedHost, which is what the setters pair with
fn url_components() {
    let c = NSURLComponents::componentsWithString(&ns("http://user:pw@host:8080/a%20b/c?x=1&y=&z#frag")).unwrap();
    assert_eq!(text(c.scheme()).as_deref(), Some("http"));
    assert_eq!(text(c.user()).as_deref(), Some("user"));
    assert_eq!(text(c.password()).as_deref(), Some("pw"));
    assert_eq!(text(c.host()).as_deref(), Some("host"));
    assert_eq!(text(c.path()).as_deref(), Some("/a b/c"));
    assert_eq!(text(c.percentEncodedPath()).as_deref(), Some("/a%20b/c"));
    assert_eq!(text(c.query()).as_deref(), Some("x=1&y=&z"));
    assert_eq!(text(c.fragment()).as_deref(), Some("frag"));
    assert_eq!(text(c.string()).as_deref(), Some("http://user:pw@host:8080/a%20b/c?x=1&y=&z#frag"));
    assert_eq!(absolute(c.URL()).as_deref(), Some("http://user:pw@host:8080/a%20b/c?x=1&y=&z#frag"));

    for (string, host, pe_host, path, pe_path, result) in [
        ("", None, None, "", "", ""),
        ("a b", None, None, "a b", "a%20b", "a%20b"),
        ("http://h/%zz", Some("h"), Some("h"), "/%zz", "/%25zz", "http://h/%25zz"),
        ("//host/path", Some("host"), Some("host"), "/path", "/path", "//host/path"),
        ("http://[::1]:80/", Some("[::1]"), Some("[::1]"), "/", "/", "http://[::1]:80/"),
        ("mailto:x@y.com", None, None, "x@y.com", "x@y.com", "mailto:x@y.com"),
        ("http://%41/", Some("A"), Some("%41"), "/", "/", "http://%41/"),
        ("http://[::1%25en0]/", Some("[::1%en0]"), Some("[::1%25en0]"), "/", "/", "http://[::1%25en0]/"),
        ("http://h/a b?c d#e f", Some("h"), Some("h"), "/a b", "/a%20b", "http://h/a%20b?c%20d#e%20f"),
        ("file:///x", Some(""), Some(""), "/x", "/x", "file:///x"),
    ] {
        let c = NSURLComponents::componentsWithString(&ns(string)).unwrap();
        assert_eq!(text(c.host()).as_deref(), host, "host of {string:?}");
        assert_eq!(text(c.percentEncodedHost()).as_deref(), pe_host, "percentEncodedHost of {string:?}");
        assert_eq!(text(c.path()).as_deref(), Some(path), "path of {string:?}");
        assert_eq!(text(c.percentEncodedPath()).as_deref(), Some(pe_path), "percentEncodedPath of {string:?}");
        assert_eq!(text(c.string()).as_deref(), Some(result), "string of {string:?}");
    }
    let c = NSURLComponents::componentsWithString(&ns("http://h/?a=1&a=2&b&c=%20d+e")).unwrap();
    assert_eq!(text(c.query()).as_deref(), Some("a=1&a=2&b&c= d+e"), "'+' is not a space");
    assert_eq!(text(c.percentEncodedQuery()).as_deref(), Some("a=1&a=2&b&c=%20d+e"));
    assert!(NSURLComponents::componentsWithString(&ns("http://host:abc/")).is_none());
    assert!(NSURLComponents::componentsWithString_encodingInvalidCharacters(&ns("http://h/a b"), false).is_none());

    // Setters encode what each part can't hold.
    let c = NSURLComponents::new();
    assert_eq!(text(c.string()).as_deref(), Some(""));
    assert_eq!(text(c.path()).as_deref(), Some(""));
    c.setPath(Some(&ns("a b")));
    assert_eq!(text(c.string()).as_deref(), Some("a%20b"));
    c.setHost(Some(&ns("h")));
    assert_eq!(text(c.string()), None, "a host needs an absolute path");
    assert!(c.URL().is_none());
    c.setPath(Some(&ns("/p")));
    assert_eq!(text(c.string()).as_deref(), Some("//h/p"));
    c.setScheme(Some(&ns("http")));
    c.setUser(Some(&ns("u s:@")));
    c.setPassword(Some(&ns("p w:@")));
    assert_eq!(text(c.percentEncodedUser()).as_deref(), Some("u%20s:%40"));
    assert_eq!(text(c.percentEncodedPassword()).as_deref(), Some("p%20w:%40"));
    assert_eq!(text(c.user()).as_deref(), Some("u s:@"));
    c.setUser(None);
    c.setPassword(None);
    c.setHost(Some(&ns("[::1]")));
    assert_eq!(text(c.string()).as_deref(), Some("http://[::1]/p"));
    c.setHost(Some(&ns("h")));
    c.setQuery(Some(&ns("a=b&c#d+e/f?g")));
    assert_eq!(text(c.percentEncodedQuery()).as_deref(), Some("a=b&c%23d+e/f?g"));
    c.setFragment(Some(&ns("x#y?z/")));
    assert_eq!(text(c.percentEncodedFragment()).as_deref(), Some("x%23y?z/"));
    c.setPath(Some(&ns("/a?b#c;d")));
    assert_eq!(text(c.percentEncodedPath()).as_deref(), Some("/a%3Fb%23c;d"));
    assert_eq!(text(c.string()).as_deref(), Some("http://h/a%3Fb%23c;d?a=b&c%23d+e/f?g#x%23y?z/"));
    c.setPercentEncodedPath(Some(&ns("/x%20y")));
    assert_eq!(text(c.path()).as_deref(), Some("/x y"));
    c.setQuery(Some(&ns("")));
    assert_eq!(text(c.string()).as_deref(), Some("http://h/x%20y?#x%23y?z/"));
    c.setQuery(None);
    c.setFragment(None);
    assert_eq!(text(c.string()).as_deref(), Some("http://h/x%20y"));
    c.setPath(Some(&ns("")));
    assert_eq!(text(c.string()).as_deref(), Some("http://h"));

    let c = NSURLComponents::componentsWithString(&ns("http://h/p?x=1")).unwrap();
    c.setScheme(None);
    assert_eq!(text(c.string()).as_deref(), Some("//h/p?x=1"));
    c.setHost(None);
    assert_eq!(text(c.string()).as_deref(), Some("/p?x=1"));

    let rel = NSURL::URLWithString_relativeToURL(&ns("x?y"), Some(&url("http://h/d/"))).unwrap();
    let resolved = NSURLComponents::componentsWithURL_resolvingAgainstBaseURL(&rel, true).unwrap();
    assert_eq!(text(resolved.string()).as_deref(), Some("http://h/d/x?y"));
    let own = NSURLComponents::componentsWithURL_resolvingAgainstBaseURL(&rel, false).unwrap();
    assert_eq!(text(own.string()).as_deref(), Some("x?y"));
    let c = NSURLComponents::componentsWithString(&ns("p/q")).unwrap();
    let u = c.URLRelativeToURL(Some(&url("http://b/d/"))).unwrap();
    assert_eq!(text(u.absoluteString()).as_deref(), Some("http://b/d/p/q"));
    assert!(u.baseURL().is_some());
    let u = NSURLComponents::componentsWithString(&ns("http://h/p"))
        .unwrap()
        .URLRelativeToURL(Some(&url("http://b/")))
        .unwrap();
    assert!(u.baseURL().is_none());

    let c1 = NSURLComponents::componentsWithString(&ns("http://h/p")).unwrap();
    let c2 = NSURLComponents::componentsWithString(&ns("http://h/p")).unwrap();
    assert!(c1.isEqual(Some(&c2)));
    assert!(!c1.isEqual(Some(&NSURLComponents::componentsWithString(&ns("http://h/q")).unwrap())));

    let item = NSURLQueryItem::queryItemWithName_value(&ns("a b"), Some(&ns("c&d")));
    assert_eq!(item.name().to_string(), "a b");
    assert_eq!(text(item.value()).as_deref(), Some("c&d"));
    let same = NSURLQueryItem::queryItemWithName_value(&ns("a b"), Some(&ns("c&d")));
    assert!(item.isEqual(Some(&same)));
    assert_eq!(item.hash(), same.hash());
    let valueless = NSURLQueryItem::queryItemWithName_value(&ns("a b"), None);
    assert!(!item.isEqual(Some(&valueless)));
    assert_eq!(valueless.value(), None);
}

#[test]
fn url_query_items() {
    use objc2_foundation::NSArray;
    let items = |c: &NSURLComponents| -> Option<Vec<(String, Option<String>)>> {
        c.queryItems().map(|a| a.iter().map(|i| (i.name().to_string(), text(i.value()))).collect())
    };
    let c = NSURLComponents::componentsWithString(&ns("http://h/?a=1&a=2&b&c=%20d+e")).unwrap();
    assert_eq!(
        items(&c),
        Some(vec![
            ("a".into(), Some("1".into())),
            ("a".into(), Some("2".into())),
            ("b".into(), None),
            ("c".into(), Some(" d+e".into()))
        ])
    );
    let c = NSURLComponents::componentsWithString(&ns("http://h/?&a&&=b")).unwrap();
    assert_eq!(
        items(&c),
        Some(vec![("".into(), None), ("a".into(), None), ("".into(), None), ("".into(), Some("b".into()))])
    );
    assert_eq!(items(&NSURLComponents::componentsWithString(&ns("http://h/?")).unwrap()), Some(vec![]));
    assert_eq!(items(&NSURLComponents::componentsWithString(&ns("http://h/")).unwrap()), None);
    assert_eq!(
        items(&NSURLComponents::componentsWithString(&ns("http://h/?a=b%26c&d=%3D")).unwrap()),
        Some(vec![("a".into(), Some("b&c".into())), ("d".into(), Some("=".into()))])
    );

    let c = NSURLComponents::new();
    let list = NSArray::from_retained_slice(&[
        NSURLQueryItem::queryItemWithName_value(&ns("a b"), Some(&ns("c&d"))),
        NSURLQueryItem::queryItemWithName_value(&ns("a b"), None),
        NSURLQueryItem::queryItemWithName_value(&ns("x=y"), Some(&ns("\u{e9}+/?"))),
        NSURLQueryItem::queryItemWithName_value(&ns("n=&+#?/"), Some(&ns("v=&+#?/ ;:@"))),
    ]);
    c.setQueryItems(Some(&list));
    assert_eq!(
        text(c.percentEncodedQuery()).as_deref(),
        Some("a%20b=c%26d&a%20b&x%3Dy=%C3%A9+/?&n%3D%26+%23?/=v%3D%26+%23?/%20;:@")
    );
    c.setQueryItems(Some(&NSArray::new()));
    assert_eq!(text(c.string()).as_deref(), Some("?"));
    c.setQueryItems(None);
    assert_eq!(text(c.string()).as_deref(), Some(""));
}

fn error(domain: &str, code: isize, info: &[(&NSString, Retained<AnyObject>)]) -> Retained<NSError> {
    let keys: Vec<&NSString> = info.iter().map(|(k, _)| *k).collect();
    let values: Vec<Retained<AnyObject>> = info.iter().map(|(_, v)| v.clone()).collect();
    let info = (!info.is_empty()).then(|| NSDictionary::from_retained_objects(&keys, &values));
    unsafe { NSError::errorWithDomain_code_userInfo(&ns(domain), code, info.as_deref()) }
}

fn info_value(error: &NSError, key: &NSString) -> Option<Retained<AnyObject>> {
    error.userInfo().objectForKey(key)
}

fn underlying(error: &NSError) -> Option<(String, isize)> {
    let under = info_value(error, unsafe { objc2_foundation::NSUnderlyingErrorKey })?;
    let under = under.downcast::<NSError>().ok()?;
    Some((under.domain().to_string(), under.code()))
}

#[test]
fn error_constants_and_texts() {
    use objc2_foundation::*;
    unsafe {
        for (constant, value) in [
            (NSCocoaErrorDomain, "NSCocoaErrorDomain"),
            (NSPOSIXErrorDomain, "NSPOSIXErrorDomain"),
            (NSOSStatusErrorDomain, "NSOSStatusErrorDomain"),
            (NSMachErrorDomain, "NSMachErrorDomain"),
            (NSUnderlyingErrorKey, "NSUnderlyingError"),
            (NSLocalizedDescriptionKey, "NSLocalizedDescription"),
            (NSLocalizedFailureReasonErrorKey, "NSLocalizedFailureReason"),
            (NSLocalizedRecoverySuggestionErrorKey, "NSLocalizedRecoverySuggestion"),
            (NSLocalizedRecoveryOptionsErrorKey, "NSLocalizedRecoveryOptions"),
            (NSRecoveryAttempterErrorKey, "NSRecoveryAttempter"),
            (NSHelpAnchorErrorKey, "NSHelpAnchor"),
            (NSDebugDescriptionErrorKey, "NSDebugDescription"),
            (NSLocalizedFailureErrorKey, "NSLocalizedFailure"),
            (NSStringEncodingErrorKey, "NSStringEncoding"),
            (NSURLErrorKey, "NSURL"),
            (NSFilePathErrorKey, "NSFilePath"),
            (NSMultipleUnderlyingErrorsKey, "NSMultipleUnderlyingErrorsKey"),
        ] {
            assert_eq!(constant.to_string(), value);
        }
    }

    let e = error("MyDomain", 42, &[]);
    assert_eq!(e.domain().to_string(), "MyDomain");
    assert_eq!(e.code(), 42);
    assert_eq!(e.userInfo().count(), 0, "an empty dictionary, not nil");
    let text_of = |e: &NSError| e.localizedDescription().to_string();
    let generic = text_of(&e);
    assert!(generic.contains("MyDomain") && generic.contains("42"), "{generic}");
    assert_eq!(e.localizedFailureReason(), None);
    assert_eq!(e.localizedRecoverySuggestion(), None);
    assert!(e.localizedRecoveryOptions().is_none());
    assert_eq!(describe(&e), "Error Domain=MyDomain Code=42 \"(null)\"");

    let key = |k: &'static NSString| k;
    unsafe {
        let e = error("MyDomain", 1, &[(key(NSLocalizedDescriptionKey), ns("Custom desc").into())]);
        assert_eq!(text_of(&e), "Custom desc");
        assert_eq!(
            describe(&e),
            "Error Domain=MyDomain Code=1 \"Custom desc\" UserInfo={NSLocalizedDescription=Custom desc}"
        );
        let e = error("MyDomain", 1, &[(key(NSLocalizedFailureReasonErrorKey), ns("Because.").into())]);
        assert!(text_of(&e).ends_with(" Because."), "{}", text_of(&e));
        assert_eq!(text(e.localizedFailureReason()).as_deref(), Some("Because."));
        let e = error("MyDomain", 1, &[(key(NSLocalizedFailureErrorKey), ns("Failed to x.").into())]);
        assert_eq!(text_of(&e), "Failed to x.");
        let e = error(
            "MyDomain",
            1,
            &[
                (key(NSLocalizedFailureErrorKey), ns("Failed to x.").into()),
                (key(NSLocalizedFailureReasonErrorKey), ns("Because.").into()),
            ],
        );
        assert_eq!(text_of(&e), "Failed to x. Because.");
        let e = error("MyDomain", 1, &[(key(NSLocalizedRecoverySuggestionErrorKey), ns("Try again.").into())]);
        assert_eq!(text(e.localizedRecoverySuggestion()).as_deref(), Some("Try again."));
        let e = error("D", 1, &[(key(NSDebugDescriptionErrorKey), ns("dbg").into())]);
        assert_eq!(describe(&e), "Error Domain=D Code=1 \"dbg\" UserInfo={NSDebugDescription=dbg}");
        let e = error("D", 1, &[(&*ns("a"), ns("1").into())]);
        assert_eq!(describe(&e), "Error Domain=D Code=1 \"(null)\" UserInfo={a=1}");

        // POSIX errors describe themselves with the C library's text.
        let e = error("NSPOSIXErrorDomain", 2, &[]);
        assert_eq!(text(e.localizedFailureReason()).as_deref(), Some("No such file or directory"));
        assert!(text_of(&e).ends_with("No such file or directory"));
        assert_eq!(describe(&e), "Error Domain=NSPOSIXErrorDomain Code=2 \"No such file or directory\"");

        let a = error("NSCocoaErrorDomain", 260, &[(key(NSFilePathErrorKey), ns("/tmp/nope.txt").into())]);
        let b = error("NSCocoaErrorDomain", 260, &[(key(NSFilePathErrorKey), ns("/tmp/nope.txt").into())]);
        assert!(a.isEqual(Some(&b)));
        assert_eq!(a.hash(), b.hash());
        assert!(!a.isEqual(Some(&error("NSCocoaErrorDomain", 260, &[]))), "user info counts");
        assert!(!a.isEqual(Some(&error(
            "NSCocoaErrorDomain",
            261,
            &[(key(NSFilePathErrorKey), ns("/tmp/nope.txt").into())]
        ))));
        assert!(text_of(&a).contains("nope.txt"), "file errors name the file: {}", text_of(&a));
    }
}

#[test]
fn file_operation_errors() {
    let dir = scratch("errors");
    let missing = dir.join("missing");
    let missing_string = missing.to_str().unwrap();
    let e =
        NSData::dataWithContentsOfFile_options_error(&ns(missing_string), NSDataReadingOptions::empty()).unwrap_err();
    assert_eq!((e.domain().to_string(), e.code()), ("NSCocoaErrorDomain".into(), 260));
    assert_eq!(underlying(&e), Some(("NSPOSIXErrorDomain".into(), 2)));
    let path = info_value(&e, unsafe { objc2_foundation::NSFilePathErrorKey }).unwrap();
    assert_eq!(describe(&path), missing_string);
    let url_value = info_value(&e, unsafe { objc2_foundation::NSURLErrorKey }).unwrap().downcast::<NSURL>().unwrap();
    assert_eq!(text(url_value.path()).as_deref(), Some(missing_string));

    let file_url = NSURL::fileURLWithPath(&ns(missing_string));
    let e = NSData::dataWithContentsOfURL_options_error(&file_url, NSDataReadingOptions::empty()).unwrap_err();
    assert_eq!(e.code(), 260);
    assert!(NSData::dataWithContentsOfURL(&file_url).is_none());

    let e = NSData::dataWithContentsOfFile_options_error(&ns(dir.to_str().unwrap()), NSDataReadingOptions::empty())
        .unwrap_err();
    assert_eq!(e.code(), 256, "reading a directory");
    assert_eq!(underlying(&e), Some(("NSPOSIXErrorDomain".into(), 21)));

    let d = data(b"hello");
    let e = d
        .writeToFile_options_error(&ns(dir.join("no-dir/x").to_str().unwrap()), NSDataWritingOptions::empty())
        .unwrap_err();
    assert_eq!(e.code(), 4, "writing into a missing directory");
    assert_eq!(underlying(&e), Some(("NSPOSIXErrorDomain".into(), 2)));
    let e = d
        .writeToFile_options_error(&ns(dir.join("no-dir/x").to_str().unwrap()), NSDataWritingOptions::Atomic)
        .unwrap_err();
    assert_eq!(e.code(), 4, "atomically too");

    let target = dir.join("target");
    let target_string = ns(target.to_str().unwrap());
    assert!(d.writeToFile_options_error(&target_string, NSDataWritingOptions::WithoutOverwriting).is_ok());
    let e = d.writeToFile_options_error(&target_string, NSDataWritingOptions::WithoutOverwriting).unwrap_err();
    assert_eq!(e.code(), 516);
    assert_eq!(underlying(&e), Some(("NSPOSIXErrorDomain".into(), 17)));
    assert!(data(b"new").writeToFile_options_error(&target_string, NSDataWritingOptions::Atomic).is_ok());
    assert_eq!(std::fs::read(&target).unwrap(), b"new");

    let e = d.writeToURL_options_error(&url("http://h/x"), NSDataWritingOptions::empty()).unwrap_err();
    assert_eq!((e.domain().to_string(), e.code()), ("NSCocoaErrorDomain".into(), 518));
    assert!(info_value(&e, unsafe { objc2_foundation::NSURLErrorKey }).is_some());

    let target_url = NSURL::fileURLWithPath(&ns(dir.join("by-url").to_str().unwrap()));
    assert!(d.writeToURL_atomically(&target_url, true));
    assert_eq!(NSData::dataWithContentsOfURL(&target_url).unwrap().to_vec(), b"hello");
    let same = NSURL::URLWithString(&ns(&format!("file://{}", dir.join("by-url").to_str().unwrap()))).unwrap();
    assert_eq!(NSData::dataWithContentsOfURL(&same).unwrap().to_vec(), b"hello");
    assert_eq!(NSMutableData::dataWithContentsOfURL(&same).unwrap().to_vec(), b"hello");
    std::fs::remove_dir_all(&dir).unwrap();
}

fn code_and_underlying(result: Result<(), Retained<NSError>>) -> (isize, Option<(String, isize)>) {
    let e = result.expect_err("the operation fails");
    assert_eq!(e.domain().to_string(), "NSCocoaErrorDomain");
    (e.code(), underlying(&e))
}

fn posix(code: isize) -> Option<(String, isize)> {
    Some(("NSPOSIXErrorDomain".to_string(), code))
}

#[test]
fn file_manager_operations() {
    let fm = NSFileManager::defaultManager();
    let mkdir = |path: &NSString, intermediates: bool| unsafe {
        fm.createDirectoryAtPath_withIntermediateDirectories_attributes_error(path, intermediates, None)
    };
    assert!(std::ptr::eq(&*fm, &*NSFileManager::defaultManager()), "one default manager");
    let dir = scratch("fm");
    let p = |name: &str| ns(dir.join(name).to_str().unwrap());

    // Directories.
    assert!(mkdir(&p("d"), false).is_ok());
    assert_eq!(code_and_underlying(mkdir(&p("d"), false)), (516, posix(17)));
    assert!(mkdir(&p("d"), true).is_ok(), "an existing directory is fine with intermediates");
    assert_eq!(code_and_underlying(mkdir(&p("x/y"), false)), (4, posix(2)));
    assert!(mkdir(&p("x/y/z"), true).is_ok());
    std::fs::write(dir.join("f"), b"abc").unwrap();
    assert_eq!(code_and_underlying(mkdir(&p("f"), true)), (516, posix(17)));
    assert_eq!(code_and_underlying(mkdir(&p("f/sub"), true)).0, 512);
    let url_dir = NSURL::fileURLWithPath(&p("by-url"));
    assert!(
        unsafe { fm.createDirectoryAtURL_withIntermediateDirectories_attributes_error(&url_dir, false, None) }.is_ok()
    );
    assert!(dir.join("by-url").is_dir());

    // Existence and access.
    let mut is_dir = objc2::runtime::Bool::NO;
    assert!(unsafe { fm.fileExistsAtPath_isDirectory(&p("f"), &mut is_dir) });
    assert!(!is_dir.as_bool());
    assert!(unsafe { fm.fileExistsAtPath_isDirectory(&p("d"), &mut is_dir) });
    assert!(is_dir.as_bool());
    assert!(!fm.fileExistsAtPath(&p("missing")));
    std::os::unix::fs::symlink(dir.join("missing"), dir.join("dangling")).unwrap();
    assert!(!fm.fileExistsAtPath(&p("dangling")), "links are followed");
    std::os::unix::fs::symlink(dir.join("d"), dir.join("dirlink")).unwrap();
    assert!(unsafe { fm.fileExistsAtPath_isDirectory(&p("dirlink"), &mut is_dir) });
    assert!(is_dir.as_bool());
    assert!(fm.isReadableFileAtPath(&p("f")));
    assert!(fm.isWritableFileAtPath(&p("f")));
    assert!(!fm.isExecutableFileAtPath(&p("f")));
    assert!(fm.isDeletableFileAtPath(&p("f")));
    assert!(!fm.isReadableFileAtPath(&p("missing")));

    // Removing.
    assert_eq!(code_and_underlying(fm.removeItemAtPath_error(&p("missing"))), (4, posix(2)));
    assert_eq!(code_and_underlying(fm.removeItemAtURL_error(&NSURL::fileURLWithPath(&p("missing")))), (4, posix(2)));
    assert!(fm.removeItemAtPath_error(&p("x")).is_ok(), "directories go with what they hold");
    assert!(!dir.join("x").exists());
    assert!(fm.removeItemAtPath_error(&p("dangling")).is_ok());

    // Copying.
    assert!(fm.copyItemAtPath_toPath_error(&p("f"), &p("g")).is_ok());
    assert_eq!(std::fs::read(dir.join("g")).unwrap(), b"abc");
    assert_eq!(code_and_underlying(fm.copyItemAtPath_toPath_error(&p("f"), &p("g"))), (516, posix(17)));
    assert_eq!(code_and_underlying(fm.copyItemAtPath_toPath_error(&p("missing"), &p("h"))), (260, posix(2)));
    assert_eq!(code_and_underlying(fm.copyItemAtPath_toPath_error(&p("f"), &p("nodir/h"))), (4, posix(2)));
    std::fs::create_dir(dir.join("d/sub")).unwrap();
    std::fs::write(dir.join("d/sub/c"), b"c").unwrap();
    assert!(fm.copyItemAtPath_toPath_error(&p("d"), &p("d2")).is_ok());
    assert_eq!(std::fs::read(dir.join("d2/sub/c")).unwrap(), b"c");
    assert!(fm.copyItemAtPath_toPath_error(&p("dirlink"), &p("dirlink2")).is_ok());
    assert!(
        std::fs::symlink_metadata(dir.join("dirlink2")).unwrap().file_type().is_symlink(),
        "links are copied as links"
    );
    assert!(fm.contentsEqualAtPath_andPath(&p("d"), &p("d2")));
    assert!(fm.contentsEqualAtPath_andPath(&p("f"), &p("g")));

    // Moving.
    assert!(fm.moveItemAtPath_toPath_error(&p("g"), &p("g2")).is_ok());
    assert!(!dir.join("g").exists() && dir.join("g2").exists());
    assert_eq!(
        code_and_underlying(fm.moveItemAtPath_toPath_error(&p("f"), &p("g2"))),
        (516, posix(17)),
        "no replacing"
    );
    assert_eq!(code_and_underlying(fm.moveItemAtPath_toPath_error(&p("missing"), &p("h"))), (4, posix(2)));
    assert_eq!(code_and_underlying(fm.moveItemAtPath_toPath_error(&p("f"), &p("nodir/h"))), (4, posix(2)));

    // Links.
    assert!(fm.createSymbolicLinkAtPath_withDestinationPath_error(&p("sl"), &ns("f")).is_ok());
    assert_eq!(
        code_and_underlying(fm.createSymbolicLinkAtPath_withDestinationPath_error(&p("sl"), &ns("f"))),
        (516, posix(17))
    );
    assert_eq!(fm.destinationOfSymbolicLinkAtPath_error(&p("sl")).unwrap().to_string(), "f");
    assert_eq!(code_and_underlying(fm.destinationOfSymbolicLinkAtPath_error(&p("f")).map(drop)), (256, posix(22)));
    assert_eq!(code_and_underlying(fm.destinationOfSymbolicLinkAtPath_error(&p("missing")).map(drop)), (260, posix(2)));
    assert!(fm.linkItemAtPath_toPath_error(&p("f"), &p("hl")).is_ok());
    assert_eq!(code_and_underlying(fm.linkItemAtPath_toPath_error(&p("f"), &p("hl"))), (516, posix(17)));
    assert!(fm.contentsEqualAtPath_andPath(&p("f"), &p("hl")));

    // Contents.
    assert_eq!(fm.contentsAtPath(&p("f")).unwrap().to_vec(), b"abc");
    assert!(fm.contentsAtPath(&p("missing")).is_none());
    assert!(unsafe { fm.createFileAtPath_contents_attributes(&p("new"), Some(&data(b"xy")), None) });
    assert_eq!(std::fs::read(dir.join("new")).unwrap(), b"xy");
    assert!(unsafe { fm.createFileAtPath_contents_attributes(&p("new"), Some(&data(b"z")), None) }, "replaces");
    assert_eq!(std::fs::read(dir.join("new")).unwrap(), b"z");
    assert!(unsafe { fm.createFileAtPath_contents_attributes(&p("empty"), None, None) });
    assert_eq!(std::fs::read(dir.join("empty")).unwrap(), b"");
    assert!(!unsafe { fm.createFileAtPath_contents_attributes(&p("nodir/x"), None, None) });
    assert!(!fm.contentsEqualAtPath_andPath(&p("f"), &p("new")));

    assert_eq!(fm.displayNameAtPath(&p("f")).to_string(), "f");
    let fs = fm.fileSystemRepresentationWithPath(&p("f"));
    let back = unsafe {
        fm.stringWithFileSystemRepresentation_length(fs, std::ffi::CStr::from_ptr(fs.as_ptr()).to_bytes().len())
    };
    assert_eq!(back.to_string(), dir.join("f").to_str().unwrap());

    let cwd = fm.currentDirectoryPath().to_string();
    assert_eq!(std::path::Path::new(&cwd), std::env::current_dir().unwrap());
    assert!(!fm.changeCurrentDirectoryPath(&p("missing")));
    assert_eq!(fm.currentDirectoryPath().to_string(), cwd);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn home_temporary_and_user() {
    use objc2_foundation::{NSFullUserName, NSHomeDirectory, NSHomeDirectoryForUser, NSTemporaryDirectory, NSUserName};
    let fm = NSFileManager::defaultManager();
    let home = NSHomeDirectory().to_string();
    assert!(home.starts_with('/') && (home == "/" || !home.ends_with('/')), "{home}");
    let home_url = fm.homeDirectoryForCurrentUser();
    assert!(home_url.isFileURL() && home_url.hasDirectoryPath());
    assert_eq!(text(home_url.path()).as_deref(), Some(home.as_str()));
    let temp = NSTemporaryDirectory().to_string();
    assert!(temp.starts_with('/') && temp.ends_with('/'), "{temp}");
    let temp_url = fm.temporaryDirectory();
    assert!(temp_url.hasDirectoryPath());
    assert_eq!(text(temp_url.path()).map(|p| format!("{}/", p.trim_end_matches('/'))), Some(temp));
    let user = NSUserName().to_string();
    assert!(!user.is_empty());
    assert!(!NSFullUserName().to_string().is_empty());
    assert_eq!(NSHomeDirectoryForUser(Some(&ns(&user))).map(|h| h.to_string()), Some(home));
    assert!(NSHomeDirectoryForUser(Some(&ns("nobody-sidestep-xyz"))).is_none());
    assert!(fm.homeDirectoryForUser(&ns("nobody-sidestep-xyz")).is_none());
}

#[test]
fn search_path_urls() {
    let fm = NSFileManager::defaultManager();
    let caches = fm
        .URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::CachesDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            None,
            false,
        )
        .unwrap();
    assert!(caches.isFileURL() && caches.hasDirectoryPath());
    let all = fm
        .URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::CachesDirectory,
            NSSearchPathDomainMask::AllDomainsMask,
            None,
            false,
        )
        .unwrap();
    assert!(all.isEqual(Some(&caches)), "the user domain comes first");
    let dir = scratch("replacement");
    let item = dir.join("item");
    std::fs::write(&item, b"").unwrap();
    let replacement = fm
        .URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::ItemReplacementDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            Some(&NSURL::fileURLWithPath(&ns(item.to_str().unwrap()))),
            true,
        )
        .unwrap();
    let replacement_path = text(replacement.path()).unwrap();
    assert!(std::path::Path::new(&replacement_path).is_dir(), "a new directory");
    assert_eq!(std::fs::read_dir(&replacement_path).unwrap().count(), 0, "an empty one");
    std::fs::remove_dir(&replacement_path).unwrap();
    let e = fm
        .URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::ItemReplacementDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            None,
            true,
        )
        .unwrap_err();
    assert_eq!(
        (e.domain().to_string(), e.code()),
        ("NSCocoaErrorDomain".to_string(), 256),
        "a replacement directory needs an item"
    );

    let e = fm
        .trashItemAtURL_resultingItemURL_error(
            &NSURL::fileURLWithPath(&ns(dir.join("missing").to_str().unwrap())),
            None,
        )
        .unwrap_err();
    assert_eq!((e.domain().to_string(), e.code()), ("NSCocoaErrorDomain".to_string(), 4));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// On Linux, trashing moves an item into the freedesktop.org trash with a
/// record of where it came from. (On macOS it would fill the user's
/// Trash.)
#[cfg(target_os = "linux")]
#[test]
fn trash_on_linux() {
    let fm = NSFileManager::defaultManager();
    let dir = scratch("trash");
    let item = dir.join("trash me.txt");
    std::fs::write(&item, b"t").unwrap();
    let mut out: Option<Retained<NSURL>> = None;
    fm.trashItemAtURL_resultingItemURL_error(&NSURL::fileURLWithPath(&ns(item.to_str().unwrap())), Some(&mut out))
        .unwrap();
    assert!(!item.exists());
    let trashed = out.expect("where the item went");
    let trashed_path = std::path::PathBuf::from(text(trashed.path()).unwrap());
    assert_eq!(std::fs::read(&trashed_path).unwrap(), b"t");
    let files = trashed_path.parent().unwrap();
    assert!(files.ends_with("files"));
    let name = trashed_path.file_name().unwrap().to_str().unwrap();
    let info = files.parent().unwrap().join("info").join(format!("{name}.trashinfo"));
    let record = std::fs::read_to_string(&info).unwrap();
    assert!(record.starts_with("[Trash Info]\nPath="), "{record}");
    assert!(record.contains("trash%20me.txt"), "{record}");
    assert!(record.contains("DeletionDate="), "{record}");
    std::fs::remove_file(&trashed_path).unwrap();
    std::fs::remove_file(&info).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn file_manager_listings_and_attributes() {
    use objc2_foundation::{NSDirectoryEnumerationOptions, NSNumber, NSSearchPathForDirectoriesInDomains};
    use std::os::unix::fs::PermissionsExt;
    let fm = NSFileManager::defaultManager();
    let dir = scratch("listing");
    for name in ["b", ".hidden", "a"] {
        std::fs::write(dir.join(name), b"123").unwrap();
    }
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/c"), b"").unwrap();
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let dir_string = ns(dir.to_str().unwrap());
    let names = fm.contentsOfDirectoryAtPath_error(&dir_string).unwrap().iter().map(|n| n.to_string()).collect();
    assert_eq!(sorted(names), [".hidden", "a", "b", "sub"]);
    let subpaths = fm.subpathsOfDirectoryAtPath_error(&dir_string).unwrap().iter().map(|n| n.to_string()).collect();
    assert_eq!(sorted(subpaths), [".hidden", "a", "b", "sub", "sub/c"]);
    let e = fm.contentsOfDirectoryAtPath_error(&ns(dir.join("missing").to_str().unwrap())).unwrap_err();
    assert_eq!(e.code(), 260);
    let e = fm.contentsOfDirectoryAtPath_error(&ns(dir.join("a").to_str().unwrap())).unwrap_err();
    assert_eq!((e.code(), underlying(&e)), (256, posix(20)));
    let urls = fm
        .contentsOfDirectoryAtURL_includingPropertiesForKeys_options_error(
            &NSURL::fileURLWithPath(&dir_string),
            None,
            NSDirectoryEnumerationOptions::SkipsHiddenFiles,
        )
        .unwrap();
    let urls: Vec<(String, bool)> =
        urls.iter().map(|u| (text(u.lastPathComponent()).unwrap(), u.hasDirectoryPath())).collect();
    let mut urls = urls;
    urls.sort();
    assert_eq!(urls, [("a".to_string(), false), ("b".to_string(), false), ("sub".to_string(), true)]);

    let attributes = fm.attributesOfItemAtPath_error(&ns(dir.join("a").to_str().unwrap())).unwrap();
    let get = |key: &NSString| attributes.objectForKey(key).unwrap();
    unsafe {
        use objc2_foundation::{NSFilePosixPermissions, NSFileSize, NSFileType, NSFileTypeRegular};
        assert_eq!(describe(&get(NSFileType)), NSFileTypeRegular.to_string());
        assert_eq!(get(NSFileSize).downcast::<NSNumber>().unwrap().integerValue(), 3);
        let mode = get(NSFilePosixPermissions).downcast::<NSNumber>().unwrap().integerValue();
        assert_eq!(mode as u32, std::fs::metadata(dir.join("a")).unwrap().permissions().mode() & 0o7777);
    }
    std::os::unix::fs::symlink("a", dir.join("link")).unwrap();
    let attributes = fm.attributesOfItemAtPath_error(&ns(dir.join("link").to_str().unwrap())).unwrap();
    unsafe {
        use objc2_foundation::{NSFileType, NSFileTypeSymbolicLink};
        assert_eq!(
            describe(&attributes.objectForKey(NSFileType).unwrap()),
            NSFileTypeSymbolicLink.to_string(),
            "links aren't followed"
        );
    }
    let e = fm.attributesOfItemAtPath_error(&ns(dir.join("missing").to_str().unwrap())).unwrap_err();
    assert_eq!(e.code(), 260);

    let caches =
        fm.URLsForDirectory_inDomains(NSSearchPathDirectory::CachesDirectory, NSSearchPathDomainMask::UserDomainMask);
    assert_eq!(caches.count(), 1);
    assert!(caches.objectAtIndex(0).hasDirectoryPath());
    assert_eq!(
        fm.URLsForDirectory_inDomains(NSSearchPathDirectory::UserDirectory, NSSearchPathDomainMask::UserDomainMask)
            .count(),
        0
    );
    assert_eq!(
        fm.URLsForDirectory_inDomains(
            NSSearchPathDirectory::ItemReplacementDirectory,
            NSSearchPathDomainMask::AllDomainsMask
        )
        .count(),
        0
    );
    let abbreviated = NSSearchPathForDirectoriesInDomains(
        NSSearchPathDirectory::CachesDirectory,
        NSSearchPathDomainMask::UserDomainMask,
        false,
    );
    assert!(abbreviated.objectAtIndex(0).to_string().starts_with("~/"));
    let expanded = NSSearchPathForDirectoriesInDomains(
        NSSearchPathDirectory::CachesDirectory,
        NSSearchPathDomainMask::UserDomainMask,
        true,
    );
    assert_eq!(expanded.objectAtIndex(0).to_string(), text(caches.objectAtIndex(0).path()).unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn process_info() {
    let info = NSProcessInfo::processInfo();
    assert!(std::ptr::eq(&*info, &*NSProcessInfo::processInfo()), "one shared instance");
    assert_eq!(info.processIdentifier() as u32, std::process::id());
    let exe = std::env::current_exe().unwrap();
    assert_eq!(info.processName().to_string(), exe.file_name().unwrap().to_str().unwrap());
    assert!(!info.hostName().to_string().is_empty());
    let (a, b) = (info.globallyUniqueString().to_string(), info.globallyUniqueString().to_string());
    assert_ne!(a, b);
    assert!(a.contains(&format!("-{}-", std::process::id())), "{a}");
    let version = info.operatingSystemVersion();
    assert!(version.majorVersion > 0);
    let at_least = |major, minor, patch| {
        info.isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: major,
            minorVersion: minor,
            patchVersion: patch,
        })
    };
    assert!(at_least(0, 0, 0));
    assert!(at_least(version.majorVersion, version.minorVersion, version.patchVersion));
    assert!(!at_least(version.majorVersion, version.minorVersion, version.patchVersion + 1));
    assert!(!at_least(9999, 0, 0));
    assert!(info.operatingSystemVersionString().to_string().starts_with("Version "));
    assert!(info.processorCount() >= 1 && info.activeProcessorCount() >= 1);
    assert!(info.physicalMemory() > 0);
    let up = info.systemUptime();
    assert!(up > 0.0 && info.systemUptime() >= up);
    assert_eq!(info.userName().to_string(), objc2_foundation::NSUserName().to_string());
    assert!(!info.fullUserName().to_string().is_empty());
    let home = info.environment().objectForKey(&ns("HOME")).map(|h| h.to_string());
    assert_eq!(home, std::env::var("HOME").ok());
    let token = info.beginActivityWithOptions_reason(objc2_foundation::NSActivityOptions::UserInitiated, &ns("test"));
    unsafe { info.endActivity(&token) };
    let ran = std::rc::Rc::new(std::cell::Cell::new(false));
    let block = block2::RcBlock::new({
        let ran = ran.clone();
        move || ran.set(true)
    });
    info.performActivityWithOptions_reason_usingBlock(
        objc2_foundation::NSActivityOptions::UserInitiated,
        &ns("test"),
        &block,
    );
    assert!(ran.get(), "the block runs before the call returns");
    info.disableSuddenTermination();
    info.enableSuddenTermination();
    assert!(!info.isMacCatalystApp());
    assert!(!info.isiOSAppOnMac());
}

#[test]
fn process_arguments() {
    let arguments: Vec<String> = NSProcessInfo::processInfo().arguments().iter().map(|a| a.to_string()).collect();
    let expected: Vec<String> = std::env::args().collect();
    assert_eq!(arguments, expected);
}

#[test]
fn uuids() {
    let parse = |text: &str| NSUUID::initWithUUIDString(NSUUID::alloc(), &ns(text));
    let u = NSUUID::UUID();
    let text = u.UUIDString().to_string();
    assert_eq!(text.len(), 36);
    assert!(text.chars().all(|c| c == '-' || c.is_ascii_digit() || ('A'..='F').contains(&c)), "{text}");
    assert_eq!(&text[14..15], "4", "version 4");
    assert!("89AB".contains(&text[19..20]), "the RFC 4122 variant");
    assert_ne!(NSUUID::UUID().UUIDString().to_string(), text);

    let parsed = parse("e621e1f8-c36c-495a-93fc-0c247a3e6e5f").unwrap();
    assert_eq!(parsed.UUIDString().to_string(), "E621E1F8-C36C-495A-93FC-0C247A3E6E5F");
    assert_eq!(describe(&parsed), "E621E1F8-C36C-495A-93FC-0C247A3E6E5F");
    for bad in ["xyz", "E621E1F8C36C495A93FC0C247A3E6E5F", "{e621e1f8-c36c-495a-93fc-0c247a3e6e5f}", ""] {
        assert!(parse(bad).is_none(), "{bad:?}");
    }
    let same = parse("E621E1F8-C36C-495A-93FC-0C247A3E6E5F").unwrap();
    assert!(parsed.isEqual(Some(&same)));
    assert_eq!(parsed.hash(), same.hash());
    let later = parse("e621e1f8-c36c-495a-93fc-0c247a3e6e60").unwrap();
    assert_eq!(parsed.compare(&later), NSComparisonResult::Ascending);
    assert_eq!(later.compare(&parsed), NSComparisonResult::Descending);
    assert_eq!(parsed.compare(&same), NSComparisonResult::Same);
    let copy: Retained<NSUUID> = unsafe { msg_send![&*parsed, copy] };
    assert!(copy.isEqual(Some(&parsed)));

    let expected = [0xe6, 0x21, 0xe1, 0xf8, 0xc3, 0x6c, 0x49, 0x5a, 0x93, 0xfc, 0x0c, 0x24, 0x7a, 0x3e, 0x6e, 0x5f];
    // macOS's concrete class declares char pointers where the bindings
    // declare `uuid_t`, so there the bytes go through plain messages.
    #[cfg(target_vendor = "apple")]
    let (bytes, from_bytes) = {
        let mut bytes = [0u8; 16];
        let () = unsafe { msg_send![&*parsed, getUUIDBytes: bytes.as_mut_ptr().cast::<std::ffi::c_char>()] };
        let from: Retained<NSUUID> =
            unsafe { msg_send![NSUUID::alloc(), initWithUUIDBytes: expected.as_ptr().cast::<std::ffi::c_char>()] };
        (bytes, from)
    };
    #[cfg(not(target_vendor = "apple"))]
    let (bytes, from_bytes) = (parsed.as_bytes(), NSUUID::from_bytes(expected));
    assert_eq!(bytes, expected);
    assert!(from_bytes.isEqual(Some(&parsed)));
}

#[test]
#[allow(deprecated)] // the C functions, which the new names call
fn runtime_functions() {
    use objc2::runtime::{AnyClass, NSObject};
    use objc2_foundation::{
        NSClassFromString, NSProtocolFromString, NSSelectorFromString, NSStringFromClass, NSStringFromSelector,
    };
    assert_eq!(NSStringFromClass(NSObject::class()).to_string(), "NSObject");
    assert!(std::ptr::eq(NSClassFromString(&ns("NSObject")).unwrap(), NSObject::class()));
    let string_class: &AnyClass = NSString::class();
    let name = NSStringFromClass(string_class);
    assert!(NSClassFromString(&name).is_some());
    assert!(NSClassFromString(&ns("NoSuchClassSidestep")).is_none());
    let selector = NSSelectorFromString(&ns("initWithFrame:"));
    assert_eq!(unsafe { NSStringFromSelector(selector) }.to_string(), "initWithFrame:");
    assert_eq!(selector, objc2::sel!(initWithFrame:));
    assert!(NSProtocolFromString(&ns("NoSuchProtocolSidestep")).is_none());
    // The binding retains the protocol it gets, and Sidestep's runtime
    // can't retain protocols yet (their isa is null).
    #[cfg(target_vendor = "apple")]
    {
        let protocol = NSProtocolFromString(&ns("NSObject")).unwrap();
        assert_eq!(unsafe { objc2_foundation::NSStringFromProtocol(&protocol) }.to_string(), "NSObject");
    }
}

#[test]
fn bundles() {
    let main = NSBundle::mainBundle();
    assert!(std::ptr::eq(&*main, &*NSBundle::mainBundle()));
    let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let canonical = |p: Retained<NSString>| std::fs::canonicalize(p.to_string()).unwrap();
    assert_eq!(canonical(main.bundlePath()), exe.parent().unwrap(), "a bare executable's bundle is its directory");
    assert_eq!(canonical(main.executablePath().unwrap()), exe);
    assert_eq!(canonical(main.resourcePath().unwrap()), exe.parent().unwrap());
    assert!(main.bundleURL().hasDirectoryPath());
    assert!(main.bundleIdentifier().is_none());
    assert_eq!(main.infoDictionary().unwrap().count(), 0);
    assert!(main.objectForInfoDictionaryKey(&ns("CFBundleName")).is_none());
    assert!(main.pathForResource_ofType(Some(&ns("no-such-resource")), Some(&ns("txt"))).is_none());
    assert_eq!(main.localizedStringForKey_value_table(&ns("key"), Some(&ns("value")), None).to_string(), "value");
    assert_eq!(main.localizedStringForKey_value_table(&ns("key"), None, None).to_string(), "key");
    assert_eq!(main.localizedStringForKey_value_table(&ns("key"), Some(&ns("")), None).to_string(), "key");

    let dir = scratch("bundle");
    let contents = dir.join("Foo.app/Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::create_dir_all(contents.join("Resources/sub")).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.example.foo</string><key>CFBundleExecutable</key><string>Foo</string><key>CFBundleName</key><string>Foo</string></dict></plist>"#,
    )
    .unwrap();
    std::fs::write(contents.join("MacOS/Foo"), b"").unwrap();
    for file in ["a.txt", "noext", "sub/c.txt"] {
        std::fs::write(contents.join("Resources").join(file), b"x").unwrap();
    }
    let app = dir.join("Foo.app");
    let app_string = app.to_str().unwrap();
    let bundle = NSBundle::bundleWithPath(&ns(app_string)).unwrap();
    assert!(std::ptr::eq(&*bundle, &*NSBundle::bundleWithPath(&ns(app_string)).unwrap()), "one bundle per path");
    assert_eq!(bundle.bundlePath().to_string(), app_string);
    assert_eq!(text(bundle.resourcePath()), contents.join("Resources").to_str().map(str::to_string));
    assert_eq!(text(bundle.executablePath()), contents.join("MacOS/Foo").to_str().map(str::to_string));
    assert_eq!(text(bundle.bundleIdentifier()).as_deref(), Some("com.example.foo"));
    let name = bundle.objectForInfoDictionaryKey(&ns("CFBundleName")).unwrap();
    assert_eq!(describe(&name), "Foo");
    let resource = |name: Option<&str>, ext: Option<&str>| {
        text(bundle.pathForResource_ofType(name.map(ns).as_deref(), ext.map(ns).as_deref()))
    };
    let resources = contents.join("Resources");
    let expect = |file: &str| resources.join(file).to_str().map(str::to_string);
    assert_eq!(resource(Some("a"), Some("txt")), expect("a.txt"));
    assert_eq!(resource(Some("a.txt"), None), expect("a.txt"));
    assert_eq!(resource(Some("a"), Some(".txt")), expect("a.txt"));
    assert_eq!(resource(Some("noext"), None), expect("noext"));
    assert_eq!(resource(Some("c"), Some("txt")), None, "subdirectories only when asked");
    assert_eq!(
        text(bundle.pathForResource_ofType_inDirectory(Some(&ns("c")), Some(&ns("txt")), Some(&ns("sub")))),
        expect("sub/c.txt")
    );
    assert_eq!(
        absolute(bundle.URLForResource_withExtension(Some(&ns("a")), Some(&ns("txt")))),
        expect("a.txt").map(|p| format!("file://{p}"))
    );
    assert!(
        bundle.URLForResource_withExtension_subdirectory(Some(&ns("c")), Some(&ns("txt")), Some(&ns("sub"))).is_some()
    );
    assert!(describe(&bundle).starts_with(&format!("NSBundle <{app_string}>")));

    assert!(NSBundle::bundleWithPath(&ns(dir.join("missing.app").to_str().unwrap())).is_none());
    let plain = NSBundle::bundleWithPath(&ns(dir.to_str().unwrap())).unwrap();
    assert!(plain.bundleIdentifier().is_none());
    assert!(plain.executablePath().is_none());
    assert_eq!(text(plain.resourcePath()).as_deref(), dir.to_str());
    std::fs::remove_dir_all(&dir).unwrap();
}

fn plist_xml(object: &AnyObject) -> String {
    let data = unsafe {
        NSPropertyListSerialization::dataWithPropertyList_format_options_error(
            object,
            NSPropertyListFormat::XMLFormat_v1_0,
            0,
        )
    }
    .unwrap();
    String::from_utf8(data.to_vec()).unwrap()
}

const PLIST_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n";

#[test]
fn property_list_serialization() {
    let empty = NSDictionary::<NSString, AnyObject>::new();
    let info: Vec<(Retained<NSString>, Retained<AnyObject>)> = vec![
        (ns("zeta"), ns("last").into()),
        (ns("data"), data(b"ab").into()),
        (ns("date"), NSDate::dateWithTimeIntervalSinceReferenceDate(0.75).into()),
        (ns("empty"), empty.into()),
        (ns("esc"), ns("<&>\"'").into()),
    ];
    let keys: Vec<&NSString> = info.iter().map(|(k, _)| &**k).collect();
    let values: Vec<Retained<AnyObject>> = info.iter().map(|(_, v)| v.clone()).collect();
    let dict = NSDictionary::from_retained_objects(&keys, &values);
    let xml = plist_xml(&dict);
    assert_eq!(
        xml,
        format!(
            "{PLIST_HEADER}<dict>\n\t<key>data</key>\n\t<data>\n\tYWI=\n\t</data>\n\t<key>date</key>\n\t<date>2001-01-01T00:00:00Z</date>\n\t<key>empty</key>\n\t<dict/>\n\t<key>esc</key>\n\t<string>&lt;&amp;&gt;\"'</string>\n\t<key>zeta</key>\n\t<string>last</string>\n</dict>\n</plist>\n"
        )
    );
    assert_eq!(plist_xml(&ns("hi")), format!("{PLIST_HEADER}<string>hi</string>\n</plist>\n"));
    assert_eq!(plist_xml(&ns("")), format!("{PLIST_HEADER}<string></string>\n</plist>\n"));
    assert_eq!(
        plist_xml(&data(&[7u8; 100])),
        format!(
            "{PLIST_HEADER}<data>\nBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH\nBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBw==\n</data>\n</plist>\n"
        )
    );

    let mut format = NSPropertyListFormat(0);
    let binary = unsafe {
        NSPropertyListSerialization::dataWithPropertyList_format_options_error(
            &dict,
            NSPropertyListFormat::BinaryFormat_v1_0,
            0,
        )
    }
    .unwrap();
    assert!(binary.to_vec().starts_with(b"bplist00"));
    let back = unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(
            &binary,
            NSPropertyListMutabilityOptions::Immutable,
            &mut format,
        )
    }
    .unwrap();
    assert_eq!(format, NSPropertyListFormat::BinaryFormat_v1_0);
    assert_eq!(plist_xml(&back), xml, "a binary round trip keeps everything");
    let back = unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(
            &data(xml.as_bytes()),
            NSPropertyListMutabilityOptions::Immutable,
            &mut format,
        )
    }
    .unwrap();
    assert_eq!(format, NSPropertyListFormat::XMLFormat_v1_0);
    assert_eq!(plist_xml(&back), xml);
    let old = unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(
            &data(b"{ a = b; }"),
            NSPropertyListMutabilityOptions::Immutable,
            &mut format,
        )
    }
    .unwrap();
    assert_eq!(format, NSPropertyListFormat::OpenStepFormat);
    assert_eq!(
        plist_xml(&old),
        format!("{PLIST_HEADER}<dict>\n\t<key>a</key>\n\t<string>b</string>\n</dict>\n</plist>\n")
    );

    let e = unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(
            &data(b"not a plist <"),
            NSPropertyListMutabilityOptions::Immutable,
            std::ptr::null_mut(),
        )
    }
    .unwrap_err();
    assert_eq!((e.domain().to_string(), e.code()), ("NSCocoaErrorDomain".to_string(), 3840));
    let not_a_plist = url("http://x");
    let e = unsafe {
        NSPropertyListSerialization::dataWithPropertyList_format_options_error(
            &not_a_plist,
            NSPropertyListFormat::XMLFormat_v1_0,
            0,
        )
    }
    .unwrap_err();
    assert_eq!((e.domain().to_string(), e.code()), ("NSCocoaErrorDomain".to_string(), 3851));
    let valid = |object: &AnyObject, format| unsafe {
        NSPropertyListSerialization::propertyList_isValidForFormat(object, format)
    };
    assert!(valid(&dict, NSPropertyListFormat::XMLFormat_v1_0));
    assert!(valid(&dict, NSPropertyListFormat::BinaryFormat_v1_0));
    assert!(!valid(&dict, NSPropertyListFormat::OpenStepFormat), "OpenStep is read-only");
    assert!(!valid(&not_a_plist, NSPropertyListFormat::XMLFormat_v1_0));
}

#[test]
fn property_list_numbers_and_arrays() {
    use objc2_foundation::{NSArray, NSNumber};
    let entries: Vec<(Retained<NSString>, Retained<AnyObject>)> = vec![
        (ns("alpha"), NSNumber::new_i64(42).into()),
        (ns("arr"), NSArray::from_retained_slice(&[ns("x"), ns("y")]).into()),
        (ns("big"), NSNumber::new_u64(u64::MAX).into()),
        (ns("int_real"), NSNumber::new_f64(2.0).into()),
        (ns("mid"), NSNumber::new_bool(true).into()),
        (ns("neg"), NSNumber::new_i64(-7).into()),
        (ns("real"), NSNumber::new_f64(1.5).into()),
        (ns("zero"), NSNumber::new_f64(0.0).into()),
    ];
    let keys: Vec<&NSString> = entries.iter().map(|(k, _)| &**k).collect();
    let values: Vec<Retained<AnyObject>> = entries.iter().map(|(_, v)| v.clone()).collect();
    let dict = NSDictionary::from_retained_objects(&keys, &values);
    assert_eq!(
        plist_xml(&dict),
        format!(
            "{PLIST_HEADER}<dict>\n\t<key>alpha</key>\n\t<integer>42</integer>\n\t<key>arr</key>\n\t<array>\n\t\t<string>x</string>\n\t\t<string>y</string>\n\t</array>\n\t<key>big</key>\n\t<integer>18446744073709551615</integer>\n\t<key>int_real</key>\n\t<real>2</real>\n\t<key>mid</key>\n\t<true/>\n\t<key>neg</key>\n\t<integer>-7</integer>\n\t<key>real</key>\n\t<real>1.5</real>\n\t<key>zero</key>\n\t<real>0.0</real>\n</dict>\n</plist>\n"
        )
    );
    for (value, text) in
        [(0.1, "0.10000000000000001"), (1e20, "1e+20"), (f64::NAN, "nan"), (f64::INFINITY, "+infinity")]
    {
        assert_eq!(plist_xml(&NSNumber::new_f64(value)), format!("{PLIST_HEADER}<real>{text}</real>\n</plist>\n"));
    }
    assert_eq!(plist_xml(&NSArray::<AnyObject>::new()), format!("{PLIST_HEADER}<array/>\n</plist>\n"));
    let binary = unsafe {
        NSPropertyListSerialization::dataWithPropertyList_format_options_error(
            &dict,
            NSPropertyListFormat::BinaryFormat_v1_0,
            0,
        )
    }
    .unwrap();
    let back = unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(
            &binary,
            NSPropertyListMutabilityOptions::Immutable,
            std::ptr::null_mut(),
        )
    }
    .unwrap();
    assert_eq!(plist_xml(&back), plist_xml(&dict));
}

#[test]
fn locks() {
    // SAFETY: the locks are used as locks: unlocked by the thread that
    // locked them.
    unsafe {
        use objc2_foundation::{NSCondition, NSConditionLock, NSLock, NSLocking, NSRecursiveLock};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        let past = NSDate::dateWithTimeIntervalSinceNow(-1.0);
        let soon = || NSDate::dateWithTimeIntervalSinceNow(0.05);

        let lock = NSLock::new();
        assert!(lock.name().is_none());
        lock.setName(Some(&ns("guard")));
        assert_eq!(text(lock.name()).as_deref(), Some("guard"));
        assert!(lock.tryLock());
        assert!(!lock.tryLock(), "not recursive");
        assert!(!lock.lockBeforeDate(&past));
        let start = Instant::now();
        assert!(!lock.lockBeforeDate(&soon()));
        assert!(start.elapsed() >= Duration::from_millis(40), "waits until the date");
        lock.unlock();
        assert!(lock.lockBeforeDate(&soon()));
        lock.unlock();

        // Mutual exclusion: unsynchronized read-modify-write under the lock.
        let lock = Arc::new(NSLock::new());
        let counter = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (lock, counter) = (lock.clone(), counter.clone());
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        lock.lock();
                        let n = counter.load(Ordering::Relaxed);
                        std::hint::spin_loop();
                        counter.store(n + 1, Ordering::Relaxed);
                        lock.unlock();
                    }
                })
            })
            .collect();
        threads.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(counter.load(Ordering::Relaxed), 8000);

        let recursive = Arc::new(NSRecursiveLock::new());
        recursive.lock();
        assert!(recursive.tryLock(), "the owner may lock again");
        let other = {
            let recursive = recursive.clone();
            std::thread::spawn(move || recursive.tryLock())
        };
        assert!(!other.join().unwrap(), "others may not");
        recursive.unlock();
        let other = {
            let recursive = recursive.clone();
            std::thread::spawn(move || recursive.tryLock())
        };
        assert!(!other.join().unwrap(), "still held once");
        recursive.unlock();
        let other = {
            let recursive = recursive.clone();
            std::thread::spawn(move || {
                let got = recursive.tryLock();
                if got {
                    recursive.unlock();
                }
                got
            })
        };
        assert!(other.join().unwrap());

        let condition = Arc::new(NSCondition::new());
        let ready = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (condition, ready) = (condition.clone(), ready.clone());
            std::thread::spawn(move || {
                condition.lock();
                while !ready.load(Ordering::Relaxed) {
                    condition.wait();
                }
                condition.unlock();
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        condition.lock();
        ready.store(true, Ordering::Relaxed);
        condition.signal();
        condition.unlock();
        waiter.join().unwrap();
        condition.lock();
        let start = Instant::now();
        assert!(!condition.waitUntilDate(&soon()), "nobody signals");
        assert!(start.elapsed() >= Duration::from_millis(40));
        condition.unlock();

        let condition_lock = Arc::new(NSConditionLock::initWithCondition(NSConditionLock::alloc(), 0));
        assert_eq!(condition_lock.condition(), 0);
        let order = Arc::new(AtomicUsize::new(0));
        let waiter = {
            let (condition_lock, order) = (condition_lock.clone(), order.clone());
            std::thread::spawn(move || {
                condition_lock.lockWhenCondition(1);
                order.store(2, Ordering::SeqCst);
                condition_lock.unlockWithCondition(2);
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        assert!(!condition_lock.tryLockWhenCondition(1));
        assert!(condition_lock.tryLock());
        order.store(1, Ordering::SeqCst);
        condition_lock.unlockWithCondition(1);
        assert!(condition_lock.lockWhenCondition_beforeDate(2, &NSDate::dateWithTimeIntervalSinceNow(5.0)));
        assert_eq!(order.load(Ordering::SeqCst), 2);
        assert_eq!(condition_lock.condition(), 2);
        condition_lock.unlock();
        waiter.join().unwrap();
    }
}

/// Every lock kind excludes under contention, hands over to waiters, and
/// wakes a timed waiter that gets it in time.
#[test]
fn locks_under_contention() {
    use objc2_foundation::{NSCondition, NSConditionLock, NSLock, NSLocking, NSRecursiveLock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    struct Shared<T>(Retained<T>);
    // SAFETY: Foundation's locks may be used from any thread.
    unsafe impl<T> Send for Shared<T> {}
    unsafe impl<T> Sync for Shared<T> {}
    /// Threads doing an unsynchronized read-modify-write that only the lock
    /// keeps whole, between `lock` and `unlock`; the count they reach.
    fn hammer<T: 'static>(lock: Retained<T>, lock_it: fn(&T), unlock_it: fn(&T)) -> usize {
        let lock = Arc::new(Shared(lock));
        let counter = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..6)
            .map(|_| {
                let (lock, counter) = (lock.clone(), counter.clone());
                std::thread::spawn(move || {
                    for _ in 0..3000 {
                        lock_it(&lock.0);
                        let n = counter.load(Ordering::Relaxed);
                        std::hint::spin_loop();
                        counter.store(n + 1, Ordering::Relaxed);
                        unlock_it(&lock.0);
                    }
                })
            })
            .collect();
        workers.into_iter().for_each(|t| t.join().unwrap());
        counter.load(Ordering::Relaxed)
    }
    // SAFETY: the locks are used as locks: unlocked by the thread that
    // locked them.
    unsafe {
        assert_eq!(hammer(NSLock::new(), |l| l.lock(), |l| l.unlock()), 18000);
        let twice = |l: &NSRecursiveLock| {
            l.lock();
            l.lock();
        };
        let untwice = |l: &NSRecursiveLock| {
            l.unlock();
            l.unlock();
        };
        assert_eq!(hammer(NSRecursiveLock::new(), twice, untwice), 18000);
        assert_eq!(hammer(NSCondition::new(), |l| l.lock(), |l| l.unlock()), 18000);
        assert_eq!(hammer(NSConditionLock::new(), |l| l.lock(), |l| l.unlock()), 18000);

        // A waiter with a date gets the lock when it is given back in time.
        let lock = Arc::new(Shared(NSLock::new()));
        lock.0.lock();
        let waiter = {
            let lock = lock.clone();
            std::thread::spawn(move || {
                let got = lock.0.lockBeforeDate(&NSDate::dateWithTimeIntervalSinceNow(5.0));
                if got {
                    lock.0.unlock();
                }
                got
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        lock.0.unlock();
        assert!(waiter.join().unwrap());

        // Producer and consumer on a condition: no signal is lost.
        let condition = Arc::new(Shared(NSCondition::new()));
        let queued = Arc::new(AtomicUsize::new(0));
        let consumer = {
            let (condition, queued) = (condition.clone(), queued.clone());
            std::thread::spawn(move || {
                for _ in 0..2000 {
                    condition.0.lock();
                    while queued.load(Ordering::Relaxed) == 0 {
                        condition.0.wait();
                    }
                    queued.fetch_sub(1, Ordering::Relaxed);
                    condition.0.unlock();
                }
            })
        };
        for _ in 0..2000 {
            condition.0.lock();
            queued.fetch_add(1, Ordering::Relaxed);
            condition.0.signal();
            condition.0.unlock();
        }
        consumer.join().unwrap();
        assert_eq!(queued.load(Ordering::Relaxed), 0);
    }
}

fn json_text(object: &AnyObject, options: objc2_foundation::NSJSONWritingOptions) -> String {
    let data =
        unsafe { objc2_foundation::NSJSONSerialization::dataWithJSONObject_options_error(object, options) }.unwrap();
    String::from_utf8(data.to_vec()).unwrap()
}

fn json_parse(
    text: &[u8],
    options: objc2_foundation::NSJSONReadingOptions,
) -> Result<Retained<AnyObject>, Retained<NSError>> {
    objc2_foundation::NSJSONSerialization::JSONObjectWithData_options_error(&data(text), options)
}

#[test]
fn json() {
    use objc2_foundation::{NSJSONReadingOptions as Read, NSJSONSerialization, NSJSONWritingOptions as Write};
    let object = json_parse(b"{\"a\":\"b\",\"c\":{\"d\":\"e/f\\u00e9\\n\"}, \"z\": {},}", Read::empty()).unwrap();
    let dict = object.downcast::<NSDictionary>().unwrap();
    assert_eq!(dict.count(), 3);
    assert_eq!(json_text(&dict, Write::SortedKeys), "{\"a\":\"b\",\"c\":{\"d\":\"e\\/f\u{e9}\\n\"},\"z\":{}}");
    assert_eq!(
        json_text(&dict, Write::SortedKeys | Write::WithoutEscapingSlashes),
        "{\"a\":\"b\",\"c\":{\"d\":\"e/f\u{e9}\\n\"},\"z\":{}}"
    );
    assert_eq!(
        json_text(&dict, Write::SortedKeys | Write::PrettyPrinted),
        "{\n  \"a\" : \"b\",\n  \"c\" : {\n    \"d\" : \"e\\/f\u{e9}\\n\"\n  },\n  \"z\" : {\n\n  }\n}"
    );
    assert_eq!(json_text(&NSDictionary::<NSString, AnyObject>::new(), Write::PrettyPrinted), "{\n\n}");
    let duplicate =
        json_parse(br#"{"a":"first","a":"second"}"#, Read::empty()).unwrap().downcast::<NSDictionary>().unwrap();
    assert_eq!(json_text(&duplicate, Write::empty()), "{\"a\":\"first\"}", "the first of repeated keys wins");
    let utf16 = json_parse(&[0xff, 0xfe, b'{', 0, b'}', 0], Read::empty()).unwrap();
    assert_eq!(utf16.downcast::<NSDictionary>().unwrap().count(), 0);

    let fragment = json_parse(b" \"x\" ", Read::FragmentsAllowed).unwrap();
    assert_eq!(fragment.downcast::<NSString>().unwrap().to_string(), "x");
    assert_eq!(json_text(&ns("x"), Write::FragmentsAllowed), "\"x\"");
    for bad in [
        &b"\"x\""[..],
        b"[01]",
        b"[1.]",
        b"[.5]",
        b"[1e400]",
        b"[NaN]",
        b"[\"a\x00b\"]",
        b"",
        b"{} x",
        b"[true",
        b"{1:2}",
        br#"["\ud83d"]"#,
    ] {
        let e = json_parse(bad, Read::empty()).unwrap_err();
        assert_eq!(
            (e.domain().to_string(), e.code()),
            ("NSCocoaErrorDomain".to_string(), 3840),
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
    assert!(unsafe { NSJSONSerialization::isValidJSONObject(&dict) });
    assert!(!unsafe { NSJSONSerialization::isValidJSONObject(&ns("x")) }, "top level must be a container");
}

#[test]
fn json_arrays_and_numbers() {
    use objc2_foundation::{
        NSArray, NSJSONReadingOptions as Read, NSJSONSerialization, NSJSONWritingOptions as Write, NSNull, NSNumber,
    };
    let entries: Vec<(Retained<NSString>, Retained<AnyObject>)> = vec![
        (ns("b"), NSNumber::new_i64(1).into()),
        (ns("a"), NSArray::from_retained_slice(&[ns("x/y"), ns("\u{e9}\"\\\n\t\u{1}")]).into()),
        (ns("c"), NSNull::null().into()),
        (ns("t"), NSNumber::new_bool(true).into()),
        (ns("f"), NSNumber::new_f64(1.5).into()),
        (ns("g"), NSNumber::new_f64(0.1).into()),
        (ns("h"), NSNumber::new_f64(1e20).into()),
        (ns("i"), NSNumber::new_f64(2.0).into()),
        (ns("j"), NSNumber::new_i64(-5).into()),
        (ns("k"), NSNumber::new_u64(u64::MAX).into()),
        (ns("l"), NSArray::<AnyObject>::new().into()),
        (ns("m"), NSNumber::new_f64(1e-7).into()),
        (ns("n"), NSNumber::new_f32(0.1).into()),
    ];
    let keys: Vec<&NSString> = entries.iter().map(|(k, _)| &**k).collect();
    let values: Vec<Retained<AnyObject>> = entries.iter().map(|(_, v)| v.clone()).collect();
    let dict = NSDictionary::from_retained_objects(&keys, &values);
    assert_eq!(
        json_text(&dict, Write::SortedKeys),
        "{\"a\":[\"x\\/y\",\"\u{e9}\\\"\\\\\\n\\t\\u0001\"],\"b\":1,\"c\":null,\"f\":1.5,\"g\":0.10000000000000001,\"h\":1e+20,\"i\":2,\"j\":-5,\"k\":18446744073709551615,\"l\":[],\"m\":9.9999999999999995e-08,\"n\":0.10000000149011612,\"t\":true}"
    );
    assert_eq!(
        json_text(
            &NSArray::from_retained_slice(&[NSArray::from_retained_slice(&[NSNumber::new_i64(1)])]),
            Write::PrettyPrinted
        ),
        "[\n  [\n    1\n  ]\n]"
    );
    assert!(unsafe { NSJSONSerialization::isValidJSONObject(&dict) });
    let parsed = json_parse(b"[1, 2.5, true, 1.0, 10000000000, null, -0]", Read::empty())
        .unwrap()
        .downcast::<NSArray>()
        .unwrap();
    let kind = |i: usize| {
        unsafe { std::ffi::CStr::from_ptr(parsed.objectAtIndex(i).downcast::<NSNumber>().unwrap().objCType().as_ptr()) }
            .to_bytes()
            .to_vec()
    };
    assert_eq!(kind(0), b"q");
    assert_eq!(kind(1), b"d");
    assert_eq!(kind(3), b"d", "a fraction makes a double");
    assert_eq!(kind(4), b"q");
    assert!(parsed.objectAtIndex(2).downcast::<NSNumber>().unwrap().boolValue());
    assert!(parsed.objectAtIndex(5).downcast::<NSNull>().is_ok());
    assert_eq!(
        json_parse(b"[1,]", Read::empty()).unwrap().downcast::<NSArray>().unwrap().count(),
        1,
        "a trailing comma is fine"
    );
    let three = json_parse(b" 3 ", Read::FragmentsAllowed).unwrap().downcast::<NSNumber>().unwrap();
    assert_eq!(three.integerValue(), 3);
}

#[test]
fn core_foundation_strings_and_data() {
    use objc2_core_foundation::{
        CFData, CFGetTypeID, CFRange, CFRetained, CFString, CFStringBuiltInEncodings, CFStringCompareFlags, CFType,
        ConcreteType,
    };
    let s = CFString::from_str("h\u{e9}llo \u{1f600}");
    assert_eq!(s.to_string(), "h\u{e9}llo \u{1f600}");
    assert_eq!(s.length(), 8, "UTF-16 units");
    assert_eq!(unsafe { s.character_at_index(1) }, 0xe9);
    assert_eq!(CFGetTypeID(Some(&*s)), CFString::type_id());
    let as_ns: &NSString = unsafe { &*(CFRetained::as_ptr(&s).as_ptr() as *const NSString) };
    assert_eq!(as_ns.to_string(), "h\u{e9}llo \u{1f600}", "toll-free: the same object is an NSString");
    assert_eq!(CFGetTypeID(Some(unsafe { &*(Retained::as_ptr(&ns("x")) as *const CFType) })), CFString::type_id());
    let mac_roman =
        unsafe { CFString::with_c_string(None, c"caf\x8e".as_ptr(), CFStringBuiltInEncodings::EncodingMacRoman.0) }
            .unwrap();
    assert_eq!(mac_roman.to_string(), "caf\u{e9}");

    let mut buffer = [0 as std::ffi::c_char; 4];
    let short = CFString::from_str("abcd");
    assert!(
        !unsafe { short.c_string(buffer.as_mut_ptr(), 4, CFStringBuiltInEncodings::EncodingUTF8.0) },
        "no room for the NUL"
    );
    let mut buffer = [0 as std::ffi::c_char; 5];
    assert!(unsafe { short.c_string(buffer.as_mut_ptr(), 5, CFStringBuiltInEncodings::EncodingUTF8.0) });
    let mut chars = [0u16; 2];
    unsafe { short.characters(CFRange { location: 1, length: 2 }, chars.as_mut_ptr()) };
    assert_eq!(chars, [b'b' as u16, b'c' as u16]);
    // Read a unit at a time and in chunks, as CFStringInlineBuffer does,
    // over ASCII and other text.
    for text in ["plain ascii text ".repeat(40), "h\u{e9}llo \u{1f600} w\u{f6}rld ".repeat(40)] {
        let string = CFString::from_str(&text);
        let expected: Vec<u16> = text.encode_utf16().collect();
        assert_eq!(string.length() as usize, expected.len());
        let one_by_one: Vec<u16> =
            (0..expected.len()).map(|i| unsafe { string.character_at_index(i as isize) }).collect();
        assert_eq!(one_by_one, expected);
        let mut chunked = vec![0u16; expected.len()];
        for start in (0..expected.len()).step_by(64) {
            let length = 64.min(expected.len() - start);
            let range = CFRange { location: start as isize, length: length as isize };
            unsafe { string.characters(range, chunked[start..].as_mut_ptr()) };
        }
        assert_eq!(chunked, expected);
        let mut bytes = vec![0u8; 64];
        let mut used = 0;
        let range = CFRange { location: 6, length: 4 };
        let converted = unsafe {
            string.bytes(range, CFStringBuiltInEncodings::EncodingUTF8.0, 0, false, bytes.as_mut_ptr(), 64, &mut used)
        };
        assert_eq!(converted, 4);
        let expected_bytes = String::from_utf16(&expected[6..10]).unwrap();
        assert_eq!(&bytes[..used as usize], expected_bytes.as_bytes());
    }

    let (a, b) = (CFString::from_str("a"), CFString::from_str("B"));
    assert_eq!(a.compare(Some(&b), CFStringCompareFlags::empty()).0, 1);
    assert_eq!(a.compare(Some(&b), CFStringCompareFlags::CompareCaseInsensitive).0, -1);
    let (x, y) = (CFString::from_str("file9"), CFString::from_str("file10"));
    assert_eq!(x.compare(Some(&y), CFStringCompareFlags::CompareNumerically).0, -1);
    assert_eq!(x.compare(Some(&y), CFStringCompareFlags::empty()).0, 1);

    let data = CFData::from_bytes(b"abc");
    assert_eq!(data.len(), 3);
    assert_eq!(data.to_vec(), b"abc");
    assert_eq!(CFGetTypeID(Some(&*data)), CFData::type_id());
    let ns_data: &NSData = unsafe { &*(CFRetained::as_ptr(&data).as_ptr() as *const NSData) };
    assert_eq!(ns_data.to_vec(), b"abc");
}

#[test]
fn core_foundation_urls_errors_dates() {
    use objc2_core_foundation::{
        CFDate, CFDictionary, CFError, CFGetTypeID, CFRetained, CFString, CFURL, CFURLPathStyle, ConcreteType,
        kCFErrorDomainPOSIX,
    };
    let url = CFURL::from_string(None, &CFString::from_str("http://example.com/a%20b?q"), None).unwrap();
    assert_eq!(url.string().to_string(), "http://example.com/a%20b?q");
    assert_eq!(CFGetTypeID(Some(&*url)), CFURL::type_id());
    assert!(CFURL::from_string(None, &CFString::from_str("http://example.com/a b"), None).is_none(), "no repairs");
    assert!(CFURL::from_string(None, &CFString::from_str("http://example.com/abc#a#b"), None).is_none());
    let ns_url: &NSURL = unsafe { &*(CFRetained::as_ptr(&url).as_ptr() as *const NSURL) };
    assert_eq!(text(ns_url.host()).as_deref(), Some("example.com"));

    let file = CFURL::from_file_path("/tmp/a b.txt").unwrap();
    assert_eq!(file.string().to_string(), "file:///tmp/a%20b.txt");
    assert_eq!(file.to_file_path(), Some(std::path::PathBuf::from("/tmp/a b.txt")));
    assert_eq!(
        file.file_system_path(CFURLPathStyle::CFURLPOSIXPathStyle).map(|p| p.to_string()).as_deref(),
        Some("/tmp/a b.txt")
    );
    let dir = CFURL::from_directory_path("/usr").unwrap();
    assert!(dir.has_directory_path());
    assert_eq!(dir.string().to_string(), "file:///usr/");

    let domain = unsafe { kCFErrorDomainPOSIX }.unwrap();
    let error = unsafe { CFError::new(None, Some(domain), 2, None) }.unwrap();
    assert_eq!(error.code(), 2);
    assert_eq!(error.domain().map(|d| d.to_string()).as_deref(), Some("NSPOSIXErrorDomain"));
    assert_eq!(CFGetTypeID(Some(&*error)), CFError::type_id());
    let ns_error: &NSError = unsafe { &*(CFRetained::as_ptr(&error).as_ptr() as *const NSError) };
    assert_eq!((ns_error.domain().to_string(), ns_error.code()), ("NSPOSIXErrorDomain".to_string(), 2));

    let date = CFDate::new(None, 5.0).unwrap();
    assert_eq!(date.absolute_time(), 5.0);
    assert_eq!(CFGetTypeID(Some(&*date)), CFDate::type_id());
    let ns_date: &NSDate = unsafe { &*(CFRetained::as_ptr(&date).as_ptr() as *const NSDate) };
    assert_eq!(ns_date.timeIntervalSinceReferenceDate(), 5.0);

    let (k1, k2, v1, v2) =
        (CFString::from_str("a"), CFString::from_str("b"), CFString::from_str("1"), CFString::from_str("2"));
    let dict = CFDictionary::<CFString, CFString>::from_slices(&[&k1, &k2], &[&v1, &v2]);
    assert_eq!(dict.len(), 2);
    assert_eq!(dict.get(&k2).map(|v| v.to_string()).as_deref(), Some("2"));
    assert!(!dict.contains_key(&CFString::from_str("c")));
    assert_eq!(CFGetTypeID(Some(&*dict)), <CFDictionary as ConcreteType>::type_id());
    let ns_dict: &NSDictionary = unsafe { &*(CFRetained::as_ptr(&dict).as_ptr() as *const NSDictionary) };
    assert_eq!(ns_dict.count(), 2);
}

#[test]
fn core_foundation_preferences() {
    use objc2_core_foundation::{
        CFPreferencesAppSynchronize, CFPreferencesCopyAppValue, CFPreferencesSetAppValue, CFString, CFType,
    };
    let app = CFString::from_str(&format!("org.sidestep.cfprefs-test.{}", std::process::id()));
    let key = CFString::from_str("k");
    let value = CFString::from_str("v");
    unsafe { CFPreferencesSetAppValue(&key, Some(&value), &app) };
    let back = CFPreferencesCopyAppValue(&key, &app).unwrap();
    let back: &CFString = unsafe { &*(objc2_core_foundation::CFRetained::as_ptr(&back).as_ptr() as *const CFString) };
    assert_eq!(back.to_string(), "v");
    use objc2_foundation::NSUserDefaults;
    let other = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&ns(&app.to_string()))).unwrap();
    assert_eq!(text(other.stringForKey(&ns("k"))).as_deref(), Some("v"), "the same domain as the suite");
    unsafe { CFPreferencesSetAppValue(&key, None::<&CFType>, &app) };
    assert!(CFPreferencesCopyAppValue(&key, &app).is_none());
    assert!(CFPreferencesAppSynchronize(&app));
    other.removePersistentDomainForName(&ns(&app.to_string()));
}

#[test]
fn core_foundation_numbers_and_arrays() {
    use objc2_core_foundation::{CFArray, CFGetTypeID, CFNumber, CFString, ConcreteType};
    let n = CFNumber::new_i32(42);
    assert_eq!(n.as_i32(), Some(42));
    assert_eq!(n.as_f64(), Some(42.0));
    assert_eq!(CFGetTypeID(Some(&*n)), CFNumber::type_id());
    let f = CFNumber::new_f64(1.5);
    assert_eq!(f.as_i32(), None, "not without loss");
    assert_eq!(f.as_f64(), Some(1.5));
    let (a, b) = (CFString::from_str("a"), CFString::from_str("b"));
    let array = CFArray::from_objects(&[&*a, &*b]);
    assert_eq!(array.len(), 2);
    assert_eq!(array.get(1).map(|s| s.to_string()).as_deref(), Some("b"));
    assert_eq!(CFGetTypeID(Some(&*array)), <CFArray as ConcreteType>::type_id());
    let ns_array: &objc2_foundation::NSArray =
        unsafe { &*(objc2_core_foundation::CFRetained::as_ptr(&array).as_ptr() as *const objc2_foundation::NSArray) };
    assert_eq!(ns_array.count(), 2);
    let yes = objc2_foundation::NSNumber::new_bool(true);
    let yes: &objc2_core_foundation::CFBoolean =
        unsafe { &*(Retained::as_ptr(&yes) as *const objc2_core_foundation::CFBoolean) };
    assert!(yes.as_bool());
}
