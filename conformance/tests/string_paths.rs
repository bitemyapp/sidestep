//! NSString's path methods. Expected values are what macOS returns; tilde
//! expansion is checked against `$HOME`. File system representations are
//! only checked for plain ASCII, since normalization legitimately differs by
//! platform.

use std::ffi::CStr;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString};

use sidestep as _;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

fn strings(a: &NSArray<NSString>) -> Vec<String> {
    (0..a.count()).map(|i| a.objectAtIndex(i).to_string()).collect()
}

struct Case {
    path: &'static str,
    last: &'static str,
    ext: &'static str,
    del_ext: &'static str,
    del_last: &'static str,
    plus_comp: &'static str,
    plus_ext: Option<&'static str>,
    absolute: bool,
}

const CASES: [Case; 17] = [
    Case { path: "", last: "", ext: "", del_ext: "", del_last: "", plus_comp: "z", plus_ext: None, absolute: false },
    Case {
        path: "/",
        last: "/",
        ext: "",
        del_ext: "/",
        del_last: "/",
        plus_comp: "/z",
        plus_ext: None,
        absolute: true,
    },
    Case {
        path: "//",
        last: "/",
        ext: "",
        del_ext: "/",
        del_last: "/",
        plus_comp: "/z",
        plus_ext: None,
        absolute: true,
    },
    Case {
        path: "a/",
        last: "a",
        ext: "",
        del_ext: "a",
        del_last: "",
        plus_comp: "a/z",
        plus_ext: Some("a.q"),
        absolute: false,
    },
    Case {
        path: "/a/b/",
        last: "b",
        ext: "",
        del_ext: "/a/b",
        del_last: "/a",
        plus_comp: "/a/b/z",
        plus_ext: Some("/a/b.q"),
        absolute: true,
    },
    Case {
        path: "a//b",
        last: "b",
        ext: "",
        del_ext: "a/b",
        del_last: "a",
        plus_comp: "a/b/z",
        plus_ext: Some("a/b.q"),
        absolute: false,
    },
    Case {
        path: ".hidden",
        last: ".hidden",
        ext: "",
        del_ext: ".hidden",
        del_last: "",
        plus_comp: ".hidden/z",
        plus_ext: Some(".hidden.q"),
        absolute: false,
    },
    Case {
        path: "a.",
        last: "a.",
        ext: "",
        del_ext: "a.",
        del_last: "",
        plus_comp: "a./z",
        plus_ext: Some("a..q"),
        absolute: false,
    },
    Case {
        path: "a.tar.gz",
        last: "a.tar.gz",
        ext: "gz",
        del_ext: "a.tar",
        del_last: "",
        plus_comp: "a.tar.gz/z",
        plus_ext: Some("a.tar.gz.q"),
        absolute: false,
    },
    Case {
        path: "~",
        last: "~",
        ext: "",
        del_ext: "~",
        del_last: "",
        plus_comp: "~/z",
        plus_ext: Some("~.q"),
        absolute: true,
    },
    Case {
        path: "~/x",
        last: "x",
        ext: "",
        del_ext: "~/x",
        del_last: "~",
        plus_comp: "~/x/z",
        plus_ext: Some("~/x.q"),
        absolute: true,
    },
    Case {
        path: "../x",
        last: "x",
        ext: "",
        del_ext: "../x",
        del_last: "..",
        plus_comp: "../x/z",
        plus_ext: Some("../x.q"),
        absolute: false,
    },
    Case {
        path: "./x",
        last: "x",
        ext: "",
        del_ext: "./x",
        del_last: ".",
        plus_comp: "./x/z",
        plus_ext: Some("./x.q"),
        absolute: false,
    },
    Case {
        path: "/a/./b/../c",
        last: "c",
        ext: "",
        del_ext: "/a/./b/../c",
        del_last: "/a/./b/..",
        plus_comp: "/a/./b/../c/z",
        plus_ext: Some("/a/./b/../c.q"),
        absolute: true,
    },
    Case {
        path: "a/b.c/d",
        last: "d",
        ext: "",
        del_ext: "a/b.c/d",
        del_last: "a/b.c",
        plus_comp: "a/b.c/d/z",
        plus_ext: Some("a/b.c/d.q"),
        absolute: false,
    },
    Case {
        path: "/a/b/c.txt",
        last: "c.txt",
        ext: "txt",
        del_ext: "/a/b/c",
        del_last: "/a/b",
        plus_comp: "/a/b/c.txt/z",
        plus_ext: Some("/a/b/c.txt.q"),
        absolute: true,
    },
    Case {
        path: "..",
        last: "..",
        ext: "",
        del_ext: "..",
        del_last: "",
        plus_comp: "../z",
        plus_ext: Some("...q"),
        absolute: false,
    },
];

#[test]
fn path_pieces() {
    for c in CASES {
        let p = s(c.path);
        let what = c.path;
        assert_eq!(p.lastPathComponent().to_string(), c.last, "lastPathComponent of {what:?}");
        assert_eq!(p.pathExtension().to_string(), c.ext, "pathExtension of {what:?}");
        assert_eq!(
            p.stringByDeletingPathExtension().to_string(),
            c.del_ext,
            "stringByDeletingPathExtension of {what:?}"
        );
        assert_eq!(
            p.stringByDeletingLastPathComponent().to_string(),
            c.del_last,
            "stringByDeletingLastPathComponent of {what:?}"
        );
        assert_eq!(
            p.stringByAppendingPathComponent(&s("z")).to_string(),
            c.plus_comp,
            "stringByAppendingPathComponent: of {what:?}"
        );
        assert_eq!(
            p.stringByAppendingPathExtension(&s("q")).map(|x| x.to_string()).as_deref(),
            c.plus_ext,
            "stringByAppendingPathExtension: of {what:?}"
        );
        assert_eq!(p.isAbsolutePath(), c.absolute, "isAbsolutePath of {what:?}");
    }
    assert_eq!(s("a").stringByAppendingPathComponent(&s("/b")).to_string(), "a/b");
    assert_eq!(s("a/").stringByAppendingPathComponent(&s("b/")).to_string(), "a/b");
    assert_eq!(s("").stringByAppendingPathComponent(&s("b")).to_string(), "b");
    assert_eq!(s("a").stringByAppendingPathExtension(&s("")).map(|x| x.to_string()).as_deref(), Some("a"));
    assert_eq!(s("a").stringByAppendingPathExtension(&s(".x")).map(|x| x.to_string()).as_deref(), Some("a..x"));
    assert!(s("a").stringByAppendingPathExtension(&s("x/y")).is_none());
}

#[test]
fn standardizing_and_tildes() {
    let home = std::env::var("HOME").expect("HOME is set");
    let std = |p: &str| s(p).stringByStandardizingPath().to_string();
    assert_eq!(std(""), "");
    assert_eq!(std("/"), "/");
    assert_eq!(std("//"), "/");
    assert_eq!(std("a/"), "a");
    assert_eq!(std("/a/b/"), "/a/b");
    assert_eq!(std("a//b"), "a/b");
    assert_eq!(std("./x"), "x");
    assert_eq!(std("../x"), "../x");
    assert_eq!(std("/a/./b/../c"), "/a/c");
    assert_eq!(std(".."), "..");
    assert_eq!(std("."), ".");
    assert_eq!(std("~"), home);
    assert_eq!(std("~/x"), format!("{home}/x"));
    assert_eq!(s("~/x").stringByExpandingTildeInPath().to_string(), format!("{home}/x"));
    assert_eq!(s("~").stringByExpandingTildeInPath().to_string(), home);
    assert_eq!(s("a/~").stringByExpandingTildeInPath().to_string(), "a/~");
    assert_eq!(s(&format!("{home}/x")).stringByAbbreviatingWithTildeInPath().to_string(), "~/x");
    assert_eq!(s(&home).stringByAbbreviatingWithTildeInPath().to_string(), "~");
    assert_eq!(s("/elsewhere/x").stringByAbbreviatingWithTildeInPath().to_string(), "/elsewhere/x");
}

#[test]
fn extensions_and_tidying() {
    let ext = |p: &str, e: &str| s(p).stringByAppendingPathExtension(&s(e)).map(|r| r.to_string());
    // An extension can't hold a slash or a space, or end in a dot.
    for bad in [" ", "x ", " x", "x y", ".", "x.", "/", "x/y"] {
        assert_eq!(ext("a", bad), None, "{bad:?}");
    }
    assert_eq!(ext("a", "\t"), Some("a.\t".into()));
    assert_eq!(ext("a", "x\u{A0}"), Some("a.x\u{A0}".into()));
    assert_eq!(ext("a", ".x"), Some("a..x".into()));
    assert_eq!(ext("a/b/", "x"), Some("a/b.x".into()));
    assert_eq!(ext("a b", "x"), Some("a b.x".into()));
    // Expanding tidies slashes whether or not there is a tilde.
    let expand = |p: &str| s(p).stringByExpandingTildeInPath().to_string();
    assert_eq!(expand("./"), ".");
    assert_eq!(expand("../"), "..");
    assert_eq!(expand("a//./b/"), "a/./b");
    assert_eq!(expand("//"), "/");
    assert_eq!(expand(""), "");
    assert_eq!(expand("/a/./b/../c/"), "/a/./b/../c");
    let home = std::env::var("HOME").expect("HOME is set");
    assert_eq!(expand("~//x/"), format!("{home}/x"));
}

#[test]
fn resolving_symlinks() {
    let resolve = |p: &str| s(p).stringByResolvingSymlinksInPath().to_string();
    assert_eq!(resolve("a//./b/"), "a/b");
    assert_eq!(resolve("rel/../x"), "rel/../x");
    assert_eq!(resolve("/nonexistent-sidestep/../x"), "/x");
    let dir = std::env::temp_dir().join(format!("sidestep-links-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("real/sub")).unwrap();
    std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
    std::os::unix::fs::symlink("real/sub", dir.join("rel")).unwrap();
    let d = dir.to_str().unwrap();
    // The directory itself may sit behind a symlink (as macOS's does).
    let real = format!("{}/real/sub", resolve(d));
    assert_eq!(resolve(&format!("{d}/link/sub")), real);
    assert_eq!(resolve(&format!("{d}/rel")), real);
    assert_eq!(resolve(&format!("{d}/link/../link/sub/")), real);
    // What doesn't exist is only tidied.
    assert_eq!(resolve(&format!("{d}/link/missing")), format!("{d}/link/missing"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expanding_tildes_from_many_threads() {
    // Home directories are looked up with the reentrant calls, so threads
    // looking up users at the same time don't see each other's answers.
    let expand = |p: &str| s(p).stringByExpandingTildeInPath().to_string();
    let root = expand("~root/x");
    assert!(root.starts_with('/') && root.ends_with("/x"), "{root}");
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                for _ in 0..2_000 {
                    assert_eq!(s("~root/x").stringByExpandingTildeInPath().to_string(), root);
                    assert_eq!(
                        s("~nobody-sidestep/x").stringByExpandingTildeInPath().to_string(),
                        "~nobody-sidestep/x"
                    );
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}

#[test]
fn file_system_representation() {
    let p = s("/tmp/plain name.txt");
    let c = unsafe { CStr::from_ptr(p.fileSystemRepresentation().as_ptr()) };
    assert_eq!(c.to_bytes(), b"/tmp/plain name.txt");
    let mut buf = [0i8; 32];
    assert!(unsafe { p.getFileSystemRepresentation_maxLength(NonNull::new(buf.as_mut_ptr().cast()).unwrap(), 32) });
    assert_eq!(unsafe { CStr::from_ptr(buf.as_ptr().cast()) }.to_bytes(), b"/tmp/plain name.txt");
    assert!(!unsafe { p.getFileSystemRepresentation_maxLength(NonNull::new(buf.as_mut_ptr().cast()).unwrap(), 5) });
}

#[test]
fn components_as_arrays() {
    let comps = |p: &str| strings(&s(p).pathComponents());
    assert_eq!(comps(""), Vec::<String>::new());
    assert_eq!(comps("/"), ["/"]);
    assert_eq!(comps("//"), ["/", "/"]);
    assert_eq!(comps("a/"), ["a", "/"]);
    assert_eq!(comps("/a/b/"), ["/", "a", "b", "/"]);
    assert_eq!(comps("a//b"), ["a", "b"]);
    assert_eq!(comps("~/x"), ["~", "x"]);
    assert_eq!(comps("/a/./b/../c"), ["/", "a", ".", "b", "..", "c"]);
}

#[test]
fn arrays_of_paths() {
    let parts = |v: &[&str]| NSArray::from_retained_slice(&v.iter().map(|p| s(p)).collect::<Vec<_>>());
    assert_eq!(NSString::pathWithComponents(&parts(&["/", "a", "b"])).to_string(), "/a/b");
    assert_eq!(NSString::pathWithComponents(&parts(&["a", "b/", ""])).to_string(), "a/b");
    assert_eq!(NSString::pathWithComponents(&parts(&["", "a"])).to_string(), "a");
    assert_eq!(NSString::pathWithComponents(&parts(&["", "", "a", "", "b"])).to_string(), "a/b");
    assert_eq!(NSString::pathWithComponents(&parts(&["/", "", "a"])).to_string(), "/a");
    assert_eq!(NSString::pathWithComponents(&parts(&[""])).to_string(), "");
    assert_eq!(strings(&s("/a").stringsByAppendingPaths(&parts(&["b", "c/"]))), ["/a/b", "/a/c"]);
}
