//! NSFileWrapper: wrappers made in memory, directories keying their
//! children, what only one kind of wrapper answers, trees read from and
//! written to disk, and when a wrapper matches what's on disk. Expected
//! values are what macOS does.

use std::path::{Path, PathBuf};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSData, NSDictionary, NSFileModificationDate, NSFilePosixPermissions, NSFileSize, NSFileType, NSFileTypeDirectory,
    NSFileTypeRegular, NSFileTypeSymbolicLink, NSFileWrapper, NSFileWrapperReadingOptions, NSFileWrapperWritingOptions,
    NSNumber, NSString, NSURL,
};

use sidestep as _;

fn s(t: &str) -> Retained<NSString> {
    NSString::from_str(t)
}

fn regular(bytes: &[u8], name: Option<&str>) -> Retained<NSFileWrapper> {
    let w = NSFileWrapper::initRegularFileWithContents(NSFileWrapper::alloc(), &NSData::with_bytes(bytes));
    if let Some(name) = name {
        w.setPreferredFilename(Some(&s(name)));
    }
    w
}

fn directory() -> Retained<NSFileWrapper> {
    NSFileWrapper::initDirectoryWithFileWrappers(NSFileWrapper::alloc(), &NSDictionary::new())
}

/// A directory's keys, sorted.
fn keys(w: &NSFileWrapper) -> Vec<String> {
    let mut keys: Vec<String> = w.fileWrappers().expect("children").allKeys().iter().map(|k| k.to_string()).collect();
    keys.sort();
    keys
}

fn child(w: &NSFileWrapper, key: &str) -> Retained<NSFileWrapper> {
    w.fileWrappers().expect("children").objectForKey(&s(key)).expect("the child")
}

fn contents(w: &NSFileWrapper) -> Vec<u8> {
    w.regularFileContents().expect("contents").to_vec()
}

fn attribute(w: &NSFileWrapper, key: &NSString) -> Option<Retained<AnyObject>> {
    let attributes: Retained<NSDictionary<NSString, AnyObject>> = w.fileAttributes();
    attributes.objectForKey(key)
}

fn string_attribute(w: &NSFileWrapper, key: &NSString) -> Option<String> {
    attribute(w, key).and_then(|v| v.downcast::<NSString>().ok()).map(|v| v.to_string())
}

fn number_attribute(w: &NSFileWrapper, key: &NSString) -> Option<i64> {
    attribute(w, key).and_then(|v| v.downcast::<NSNumber>().ok()).map(|v| v.as_i64())
}

/// A fresh directory for one test (removed first, in case a run before
/// left it).
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sidestep-file-wrappers-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&s(path.to_str().expect("a UTF-8 path")))
}

/// The reason of the exception `f` raises (Sidestep panics where Apple
/// raises).
fn raises(f: impl FnOnce()) -> String {
    use std::panic::AssertUnwindSafe;
    #[cfg(target_vendor = "apple")]
    {
        match objc2::exception::catch(AssertUnwindSafe(f)) {
            Ok(()) => panic!("expected an exception"),
            Err(Some(e)) => {
                // SAFETY: an NSException answers -reason with a string.
                let reason: Retained<NSString> = unsafe { objc2::msg_send![&*e, reason] };
                reason.to_string()
            }
            Err(None) => panic!("nil exception"),
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let e = std::panic::catch_unwind(AssertUnwindSafe(f)).expect_err("expected a panic");
        match e.downcast::<String>() {
            Ok(s) => *s,
            Err(e) => e.downcast::<&str>().map(|s| s.to_string()).unwrap_or_default(),
        }
    }
}

/// A wrapper made in memory: `init` makes an empty regular file; none has
/// a name until given one; the attributes are its type and permissions
/// (0777 for a directory, else 0666) and, a regular file's, a
/// modification date.
#[test]
fn wrappers_made_in_memory() {
    // SAFETY: Foundation's attribute key and value constants.
    let (file_type, permissions, modified) = unsafe { (NSFileType, NSFilePosixPermissions, NSFileModificationDate) };
    // SAFETY: as above.
    let (regular_type, directory_type, link_type) =
        unsafe { (NSFileTypeRegular, NSFileTypeDirectory, NSFileTypeSymbolicLink) };

    let empty = NSFileWrapper::new();
    assert!(empty.isRegularFile() && !empty.isDirectory() && !empty.isSymbolicLink());
    assert!(contents(&empty).is_empty());
    assert!(empty.preferredFilename().is_none() && empty.filename().is_none());
    assert_eq!(string_attribute(&empty, file_type).as_deref(), Some(&*regular_type.to_string()));
    assert_eq!(number_attribute(&empty, permissions), Some(0o666));
    assert!(attribute(&empty, modified).is_some());

    let file = regular(b"hello", None);
    assert_eq!(contents(&file), b"hello");
    assert!(file.preferredFilename().is_none() && file.filename().is_none());
    file.setPreferredFilename(Some(&s("hello.txt")));
    assert_eq!(file.preferredFilename().map(|n| n.to_string()).as_deref(), Some("hello.txt"));
    assert!(file.filename().is_none(), "the preferred name isn't the name it was read or written under");

    let dir = directory();
    assert!(dir.isDirectory() && !dir.isRegularFile());
    assert!(keys(&dir).is_empty());
    assert_eq!(string_attribute(&dir, file_type).as_deref(), Some(&*directory_type.to_string()));
    assert_eq!(number_attribute(&dir, permissions), Some(0o777));
    assert!(attribute(&dir, modified).is_none());

    let link =
        NSFileWrapper::initSymbolicLinkWithDestinationURL(NSFileWrapper::alloc(), &url(Path::new("/tmp/elsewhere")));
    assert!(link.isSymbolicLink() && !link.isRegularFile() && !link.isDirectory());
    let dest = link.symbolicLinkDestinationURL().and_then(|u| u.path()).map(|p| p.to_string());
    assert_eq!(dest.as_deref(), Some("/tmp/elsewhere"));
    assert_eq!(string_attribute(&link, file_type).as_deref(), Some(&*link_type.to_string()));
    assert_eq!(number_attribute(&link, permissions), Some(0o666));
    assert!(attribute(&link, modified).is_none());
}

/// A directory keys its children by their preferred names; another child
/// under a name already taken gets the key `1__#$!@%!#__name` (then 2__,
/// and so on) and keeps its preferred name. Removing a child frees its
/// key but the numbering of the others stays.
#[test]
fn directories_key_children_by_name() {
    let dir = directory();
    let a = regular(b"a", Some("x.txt"));
    let b = regular(b"b", Some("x.txt"));
    let c = regular(b"c", Some("x.txt"));
    assert_eq!(dir.addFileWrapper(&a).to_string(), "x.txt");
    assert_eq!(dir.addFileWrapper(&b).to_string(), "1__#$!@%!#__x.txt");
    assert_eq!(dir.addFileWrapper(&c).to_string(), "2__#$!@%!#__x.txt");
    assert_eq!(b.preferredFilename().map(|n| n.to_string()).as_deref(), Some("x.txt"));
    assert!(b.filename().is_none() && a.filename().is_none());

    let z = dir.addRegularFileWithContents_preferredFilename(&NSData::with_bytes(b"z"), &s("z.bin"));
    assert_eq!(z.to_string(), "z.bin");
    assert_eq!(contents(&child(&dir, "z.bin")), b"z");
    assert_eq!(dir.keyForFileWrapper(&b).map(|k| k.to_string()).as_deref(), Some("1__#$!@%!#__x.txt"));
    assert!(dir.keyForFileWrapper(&regular(b"", Some("x.txt"))).is_none());

    dir.removeFileWrapper(&b);
    assert_eq!(keys(&dir), ["2__#$!@%!#__x.txt", "x.txt", "z.bin"]);
    assert!(dir.keyForFileWrapper(&b).is_none());
    assert_eq!(contents(&child(&dir, "2__#$!@%!#__x.txt")), b"c");
}

/// A directory made from a dictionary keys a child by its preferred name
/// too; the dictionary's key names only a child that has none.
#[test]
fn directories_made_from_dictionaries() {
    let named = regular(b"p", Some("p.txt"));
    let unnamed = regular(b"q", None);
    let given = NSDictionary::from_retained_objects(&[&*s("k"), &*s("q")], &[named.clone(), unnamed.clone()]);
    let dir = NSFileWrapper::initDirectoryWithFileWrappers(NSFileWrapper::alloc(), &given);
    assert_eq!(keys(&dir), ["p.txt", "q"]);
    assert_eq!(dir.keyForFileWrapper(&named).map(|k| k.to_string()).as_deref(), Some("p.txt"));
    assert_eq!(unnamed.preferredFilename().map(|n| n.to_string()).as_deref(), Some("q"));
    assert!(named.filename().is_none() && unnamed.filename().is_none());
}

/// Children are only a directory's, contents only a file's, and a child
/// needs a preferred name.
#[test]
fn what_only_one_kind_answers() {
    let file = regular(b"f", Some("f"));
    let reason = raises(|| {
        let _ = file.fileWrappers();
    });
    assert_eq!(reason, "-[NSFileWrapper fileWrappers] *** this method is only for directory type NSFileWrappers");

    let dir = directory();
    let reason = raises(|| {
        let _ = dir.regularFileContents();
    });
    assert_eq!(
        reason,
        "-[NSFileWrapper regularFileContents] *** this method is only for regular file type NSFileWrappers"
    );

    let unnamed = regular(b"n", None);
    let reason = raises(|| {
        let _ = dir.addFileWrapper(&unnamed);
    });
    assert_eq!(
        reason,
        "-[NSFileWrapper addFileWrapper:] *** a document must have a preferredFilename before it can be added as the \
         subdocument of another document."
    );

    for name in [None, Some(s(""))] {
        let reason = raises(|| unnamed.setPreferredFilename(name.as_deref()));
        assert_eq!(reason, "-[NSFileWrapper setPreferredFilename:] *** preferredFilename cannot be empty.");
    }
    assert!(unnamed.preferredFilename().is_none());

    // SAFETY: Foundation's attribute key constant.
    let file_type = unsafe { NSFileType };
    let only_type: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
        &[file_type],
        &[Retained::into_super(Retained::into_super(s("NSFileTypeRegular")))],
    );
    // SAFETY: attributes as a file wrapper takes them (without all it needs).
    let reason = raises(|| unsafe { unnamed.setFileAttributes(&only_type) });
    assert_eq!(
        reason,
        "-[NSFileWrapper setFileAttributes:] *** file attributes cannot be nil and must contain at least NSFileType \
         and NSFilePosixPermissions."
    );
}

/// Reading a URL reads the whole tree: each wrapper named (preferred and
/// actual) after its file, keyed by that name, with the file's attributes.
/// Writing puts the tree back; a missing file is Cocoa's
/// NSFileReadNoSuchFileError (260).
#[test]
fn trees_read_and_written() {
    // SAFETY: Foundation's attribute key constants.
    let (file_type, size) = unsafe { (NSFileType, NSFileSize) };
    // SAFETY: as above.
    let regular_type = unsafe { NSFileTypeRegular };

    let root = scratch("trees");
    std::fs::create_dir_all(root.join("tree/sub")).unwrap();
    std::fs::write(root.join("tree/one.txt"), b"one").unwrap();
    std::fs::write(root.join("tree/sub/two.txt"), b"two!").unwrap();

    let tree = NSFileWrapper::initWithURL_options_error(
        NSFileWrapper::alloc(),
        &url(&root.join("tree")),
        NSFileWrapperReadingOptions::empty(),
    )
    .expect("the tree");
    assert!(tree.isDirectory());
    assert_eq!(tree.preferredFilename().map(|n| n.to_string()).as_deref(), Some("tree"));
    assert_eq!(tree.filename().map(|n| n.to_string()).as_deref(), Some("tree"));
    assert_eq!(keys(&tree), ["one.txt", "sub"]);
    let one = child(&tree, "one.txt");
    assert_eq!(one.preferredFilename().map(|n| n.to_string()).as_deref(), Some("one.txt"));
    assert_eq!(one.filename().map(|n| n.to_string()).as_deref(), Some("one.txt"));
    assert_eq!(contents(&one), b"one");
    assert_eq!(string_attribute(&one, file_type).as_deref(), Some(&*regular_type.to_string()));
    assert_eq!(number_attribute(&one, size), Some(3));
    let sub = child(&tree, "sub");
    assert_eq!(keys(&sub), ["two.txt"]);
    assert_eq!(contents(&child(&sub, "two.txt")), b"two!");

    // Written elsewhere, with a file added.
    tree.addRegularFileWithContents_preferredFilename(&NSData::with_bytes(b"three"), &s("three.txt"));
    let copy = root.join("copy");
    tree.writeToURL_options_originalContentsURL_error(&url(&copy), NSFileWrapperWritingOptions::empty(), None)
        .expect("written");
    assert_eq!(std::fs::read(copy.join("one.txt")).unwrap(), b"one");
    assert_eq!(std::fs::read(copy.join("sub/two.txt")).unwrap(), b"two!");
    assert_eq!(std::fs::read(copy.join("three.txt")).unwrap(), b"three");
    assert!(tree.filename().is_some_and(|n| n.to_string() == "tree"), "written without updating names");
    tree.writeToURL_options_originalContentsURL_error(
        &url(&root.join("renamed")),
        NSFileWrapperWritingOptions::WithNameUpdating,
        None,
    )
    .expect("written");
    assert_eq!(tree.filename().map(|n| n.to_string()).as_deref(), Some("tree"), "the top keeps its name");
    assert_eq!(child(&tree, "three.txt").filename().map(|n| n.to_string()).as_deref(), Some("three.txt"));

    let missing = NSFileWrapper::initWithURL_options_error(
        NSFileWrapper::alloc(),
        &url(&root.join("missing")),
        NSFileWrapperReadingOptions::empty(),
    )
    .expect_err("nothing there");
    assert_eq!((missing.domain().to_string().as_str(), missing.code()), ("NSCocoaErrorDomain", 260));

    let _ = std::fs::remove_dir_all(&root);
}

/// A wrapper matches what's at a URL by kind and modification date, not
/// by contents; one whose children changed (so has no date any more)
/// matches nothing, and a tree written carries its dates, so it matches
/// its wrappers.
#[test]
fn matching_what_is_on_disk() {
    let root = scratch("matching");
    std::fs::create_dir_all(root.join("tree/sub")).unwrap();
    std::fs::write(root.join("tree/one.txt"), b"one").unwrap();
    std::fs::write(root.join("tree/sub/two.txt"), b"two").unwrap();
    let at = |p: &str| url(&root.join(p));
    let read = |p: &str| {
        NSFileWrapper::initWithURL_options_error(NSFileWrapper::alloc(), &at(p), NSFileWrapperReadingOptions::empty())
            .expect("read")
    };

    let tree = read("tree");
    let one = child(&tree, "one.txt");
    let _ = keys(&child(&tree, "sub"));
    assert!(tree.matchesContentsOfURL(&at("tree")));
    assert!(one.matchesContentsOfURL(&at("tree/one.txt")));
    assert!(!one.matchesContentsOfURL(&at("tree/sub")), "a file isn't a directory");
    assert!(!tree.matchesContentsOfURL(&at("tree/one.txt")), "nor a directory a file");
    assert!(!tree.matchesContentsOfURL(&at("missing")));

    // Other contents under the same date still match.
    let date = std::fs::metadata(root.join("tree/one.txt")).unwrap().modified().unwrap();
    std::fs::write(root.join("tree/one.txt"), b"ONE").unwrap();
    let file = std::fs::File::options().write(true).open(root.join("tree/one.txt")).unwrap();
    file.set_modified(date).unwrap();
    drop(file);
    assert!(one.matchesContentsOfURL(&at("tree/one.txt")));
    assert!(tree.matchesContentsOfURL(&at("tree")));

    // Another date doesn't, and the directory holding it doesn't either.
    let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    let file = std::fs::File::options().write(true).open(root.join("tree/one.txt")).unwrap();
    file.set_modified(long_ago).unwrap();
    drop(file);
    assert!(!one.matchesContentsOfURL(&at("tree/one.txt")));
    assert!(!tree.matchesContentsOfURL(&at("tree")));

    // A tree written matches its wrappers (read afresh here).
    let fresh = read("tree");
    fresh
        .writeToURL_options_originalContentsURL_error(&at("copy"), NSFileWrapperWritingOptions::empty(), None)
        .expect("written");
    let copied = std::fs::metadata(root.join("copy/one.txt")).unwrap().modified().unwrap();
    assert_eq!(copied, long_ago, "the file written has the wrapper's date");
    assert!(fresh.matchesContentsOfURL(&at("copy")));

    // A directory whose children changed has no date, and matches nothing.
    // SAFETY: Foundation's attribute key constant.
    let modified = unsafe { NSFileModificationDate };
    assert!(attribute(&fresh, modified).is_some());
    fresh.addRegularFileWithContents_preferredFilename(&NSData::with_bytes(b"new"), &s("new.txt"));
    assert!(attribute(&fresh, modified).is_none());
    assert!(!fresh.matchesContentsOfURL(&at("tree")));
    assert!(child(&fresh, "one.txt").matchesContentsOfURL(&at("tree/one.txt")));

    let fresh = read("tree");
    let _ = keys(&fresh);
    fresh.removeFileWrapper(&child(&fresh, "one.txt"));
    assert!(attribute(&fresh, modified).is_none());

    // One made in memory has a date only if it's a regular file, and so
    // matches only a file of that date.
    let made = regular(b"made", Some("made.txt"));
    made.writeToURL_options_originalContentsURL_error(&at("made.txt"), NSFileWrapperWritingOptions::empty(), None)
        .expect("written");
    assert!(made.matchesContentsOfURL(&at("made.txt")));
    assert!(!made.matchesContentsOfURL(&at("tree/one.txt")));
    assert!(!directory().matchesContentsOfURL(&at("tree/sub")));

    let _ = std::fs::remove_dir_all(&root);
}
