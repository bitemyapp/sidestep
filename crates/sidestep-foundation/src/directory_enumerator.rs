//! Lazy directory traversal. Symlinks are yielded but never followed.
use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSDirectoryEnumerator, NSEnumerator, NSError, NSString, NSURL};
use std::{
    cell::{Cell, RefCell},
    fs::{Metadata, ReadDir},
    path::PathBuf,
};
sidestep_runtime::static_class!(pub(crate) DIRECTORY, DIRECTORY_META = "NSDirectoryEnumerator", || { let _ = Directory::class(); });
type Handler = dyn Fn(*mut NSURL, *mut NSError) -> Bool;
struct State {
    root: PathBuf,
    stack: RefCell<Vec<ReadDir>>,
    pending: RefCell<Option<PathBuf>>,
    metadata: RefCell<Option<Metadata>>,
    level: Cell<usize>,
    options: usize,
    paths: bool,
    handler: Option<RcBlock<Handler>>,
}
define_class!(
    #[unsafe(super(NSEnumerator, NSObject))]
    #[name = "NSDirectoryEnumerator"]
    #[ivars = State]
    struct Directory;
    impl Directory {
        #[unsafe(method_id(nextObject))]
        fn next(&self) -> Option<Retained<AnyObject>> { next(self) }
        #[unsafe(method(skipDescendants))]
        fn skip(&self) { self.ivars().pending.take(); }
        #[unsafe(method(skipDescendents))]
        fn skip_old(&self) { self.ivars().pending.take(); }
        #[unsafe(method(level))]
        fn level(&self) -> usize { self.ivars().level.get() }
        #[unsafe(method(isEnumeratingDirectoryPostOrder))]
        fn post_order(&self) -> bool { false }
        #[unsafe(method_id(fileAttributes))]
        fn attributes(&self) -> Option<Retained<AnyObject>> { self.ivars().metadata.borrow().as_ref().map(crate::file_manager::attributes_of_item) }
        #[unsafe(method_id(directoryAttributes))]
        fn directory_attributes(&self) -> Option<Retained<AnyObject>> { std::fs::metadata(&self.ivars().root).ok().as_ref().map(crate::file_manager::attributes_of_item) }
    }
    unsafe impl NSObjectProtocol for Directory {}
);
fn url(path: &std::path::Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}
fn report(this: &Directory, path: &std::path::Path, error: &std::io::Error) -> bool {
    let Some(handler) = &this.ivars().handler else { return true };
    let url = url(path);
    let error = crate::error::file(crate::error::FileOp::Read, error, &path.to_string_lossy());
    handler.call((Retained::as_ptr(&url).cast_mut(), Retained::as_ptr(&error).cast_mut())).as_bool()
}
fn next(this: &Directory) -> Option<Retained<AnyObject>> {
    let s = this.ivars();
    let pending = s.pending.take();
    if let Some(path) = pending {
        match std::fs::read_dir(&path) {
            Ok(dir) => s.stack.borrow_mut().push(dir),
            Err(e) => {
                if !report(this, &path, &e) {
                    s.stack.borrow_mut().clear();
                    return None;
                }
            }
        }
    }
    loop {
        let entry = s.stack.borrow_mut().last_mut()?.next();
        let Some(entry) = entry else {
            s.stack.borrow_mut().pop();
            continue;
        };
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                if !report(this, &s.root, &e) {
                    s.stack.borrow_mut().clear();
                    return None;
                }
                continue;
            }
        };
        let path = entry.path();
        if s.options & 4 != 0 && entry.file_name().as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) => {
                if !report(this, &path, &e) {
                    s.stack.borrow_mut().clear();
                    return None;
                }
                continue;
            }
        };
        let package = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "app" | "bundle" | "framework" | "plugin"));
        if metadata.is_dir() && s.options & 1 == 0 && !(s.options & 2 != 0 && package) {
            s.pending.replace(Some(path.clone()));
        }
        s.metadata.replace(Some(metadata));
        s.level.set(s.stack.borrow().len());
        return Some(if s.paths {
            NSString::from_str(&path.strip_prefix(&s.root).unwrap_or(&path).to_string_lossy()).into()
        } else {
            url(&path).into()
        });
    }
}
pub(crate) fn new(
    root: PathBuf,
    options: usize,
    paths: bool,
    handler: Option<&DynBlock<Handler>>,
) -> Option<Retained<NSDirectoryEnumerator>> {
    // Post-order and relative URL enumeration need different traversal semantics.
    if options & !7 != 0 {
        return None;
    }
    let dir = std::fs::read_dir(&root).ok()?;
    let this = NSDirectoryEnumerator::<AnyObject>::alloc();
    let this: Allocated<Directory> = unsafe { std::mem::transmute(this) };
    let this = this.set_ivars(State {
        root,
        stack: RefCell::new(vec![dir]),
        pending: RefCell::new(None),
        metadata: RefCell::new(None),
        level: Cell::new(0),
        options,
        paths,
        handler: handler.map(|h| h.copy()),
    });
    let this: Retained<Directory> = unsafe { msg_send![super(this), init] };
    Some(unsafe { Retained::cast_unchecked(this) })
}
