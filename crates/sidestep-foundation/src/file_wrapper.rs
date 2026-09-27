//! `NSFileWrapper`: a file, a directory of file wrappers or a symbolic
//! link, held in memory, read from and written to disk.
//!
//! What macOS does, measured: a new wrapper (`init`) is an empty regular
//! file; a wrapper made in memory has its type and permissions as its
//! attributes (0666, 0777 for a directory) and, a regular file, a
//! modification date of when it was made (one read from disk has the
//! file's attributes); attributes set must have a type and permissions. A
//! directory's children are keyed by their preferred file names, and one
//! added under a name already taken gets the key `1__#$!@%!#__name`,
//! `2__#$!@%!#__name` and so on, keeping its preferred name; a child with
//! no preferred name can't be added, and a preferred name can't be set
//! empty; a directory made from a dictionary keys each child by its
//! preferred name too, the dictionary's key naming only a child without
//! one; adding or removing a child drops the directory's modification
//! date. Asking a file for its children, or a
//! directory for its contents, raises (panics here, with Foundation's
//! message).
//!
//! A wrapper matches what's at a URL when it's the same kind of item, the
//! wrapper has a modification date and the item's is within a second of
//! it, and a directory's children each match the item of their key there;
//! contents aren't compared. Writing gives each item written the
//! wrapper's modification date, so a tree written matches its wrappers.
//! macOS reads a directory's children when first asked for them and
//! compares only the children it has read; Sidestep reads the whole tree
//! at once, so it compares them all.
//!
//! Not here: serialized representations (macOS gives a directory's as
//! flat RTFD; here `serializedRepresentation` is nil and
//! `initWithSerializedRepresentation:` makes nothing) and archiving.

use std::cell::RefCell;
use std::path::Path;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{
    NSCopying, NSData, NSDate, NSDictionary, NSError, NSFileWrapper, NSNumber, NSString, NSUInteger, NSURL,
};

use crate::error::{self, FileOp};
use crate::file_manager::{
    FILE_TYPE, MODIFICATION_DATE, POSIX_PERMISSIONS, TYPE_DIRECTORY, TYPE_REGULAR, TYPE_SYMBOLIC_LINK,
};
use crate::runloop::modes::constant;

sidestep_runtime::static_class!(pub(crate) NSFILEWRAPPER, NSFILEWRAPPER_META = "NSFileWrapper", || {
    let _ = NSFileWrapperImpl::class();
});

/// `NSFileWrapperWritingWithNameUpdating`: give the wrappers written the
/// names they were written under.
const WITH_NAME_UPDATING: NSUInteger = 2;

enum Kind {
    Regular(Retained<NSData>),
    /// Children by key.
    Directory(Vec<(String, Retained<NSFileWrapperImpl>)>),
    Link(Retained<NSURL>),
}

pub(crate) struct WrapperIvars {
    kind: RefCell<Kind>,
    preferred: RefCell<Option<Retained<NSString>>>,
    filename: RefCell<Option<Retained<NSString>>>,
    attributes: RefCell<Retained<AnyObject>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a wrapper is used by
    // one thread at a time, as Foundation's are.
    #[unsafe(super(NSObject))]
    #[name = "NSFileWrapper"]
    #[ivars = WrapperIvars]
    pub(crate) struct NSFileWrapperImpl;

    impl NSFileWrapperImpl {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            make(this, Kind::Regular(NSData::new()))
        }

        #[unsafe(method_id(initRegularFileWithContents:))]
        fn init_regular(this: Allocated<Self>, contents: &NSData) -> Retained<Self> {
            // SAFETY: -copy of data is immutable data.
            let contents: Retained<NSData> = unsafe { msg_send![contents, copy] };
            make(this, Kind::Regular(contents))
        }

        #[unsafe(method_id(initDirectoryWithFileWrappers:))]
        fn init_directory(this: Allocated<Self>, children: &NSDictionary<NSString, AnyObject>) -> Retained<Self> {
            let (keys, values) = children.to_vecs();
            let mut given: Vec<(Retained<NSString>, Retained<NSFileWrapperImpl>)> = keys
                .into_iter()
                .zip(values)
                .filter_map(|(key, child)| Some((key, wrapper(&child)?.retain())))
                .collect();
            given.sort_by_key(|(key, _)| key.to_string());
            let mut entries = Vec::with_capacity(given.len());
            for (key, child) in given {
                let name = child.ivars().preferred.borrow_mut().get_or_insert_with(|| key.copy()).to_string();
                let key = free_key(&entries, &name);
                entries.push((key, child));
            }
            make(this, Kind::Directory(entries))
        }

        #[unsafe(method_id(initSymbolicLinkWithDestinationURL:))]
        fn init_link(this: Allocated<Self>, url: &NSURL) -> Retained<Self> {
            make(this, Kind::Link(url.retain()))
        }

        #[unsafe(method_id(initSymbolicLinkWithDestination:))]
        fn init_link_path(this: Allocated<Self>, path: &NSString) -> Retained<Self> {
            make(this, Kind::Link(NSURL::fileURLWithPath(path)))
        }

        #[unsafe(method_id(initWithURL:options:error:))]
        fn init_with_url(
            this: Allocated<Self>,
            url: &NSURL,
            _options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<Self>> {
            let path = url.path().map(|p| p.to_string()).unwrap_or_default();
            match read(Path::new(&path)) {
                Ok(read) => Some(from_read(this, read)),
                Err(e) => {
                    // SAFETY: the caller passes null or room for an error.
                    unsafe { error::set(error, error::file(FileOp::Read, &e, &path)) };
                    None
                }
            }
        }

        #[unsafe(method_id(initWithPath:))]
        fn init_with_path(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            read(Path::new(&path.to_string())).ok().map(|read| from_read(this, read))
        }

        #[unsafe(method_id(initWithSerializedRepresentation:))]
        fn init_serialized(this: Allocated<Self>, _data: &NSData) -> Option<Retained<Self>> {
            drop(this);
            None
        }

        #[unsafe(method(isDirectory))]
        fn is_directory(&self) -> bool {
            matches!(*self.ivars().kind.borrow(), Kind::Directory(_))
        }

        #[unsafe(method(isRegularFile))]
        fn is_regular_file(&self) -> bool {
            matches!(*self.ivars().kind.borrow(), Kind::Regular(_))
        }

        #[unsafe(method(isSymbolicLink))]
        fn is_symbolic_link(&self) -> bool {
            matches!(*self.ivars().kind.borrow(), Kind::Link(_))
        }

        #[unsafe(method_id(preferredFilename))]
        fn preferred_filename(&self) -> Option<Retained<NSString>> {
            self.ivars().preferred.borrow().clone()
        }

        #[unsafe(method(setPreferredFilename:))]
        fn set_preferred_filename(&self, name: Option<&NSString>) {
            let Some(name) = name.filter(|n| n.length() > 0) else {
                panic!("-[NSFileWrapper setPreferredFilename:] *** preferredFilename cannot be empty.");
            };
            *self.ivars().preferred.borrow_mut() = Some(name.copy());
        }

        #[unsafe(method_id(filename))]
        fn filename(&self) -> Option<Retained<NSString>> {
            self.ivars().filename.borrow().clone()
        }

        #[unsafe(method(setFilename:))]
        fn set_filename(&self, name: Option<&NSString>) {
            *self.ivars().filename.borrow_mut() = name.map(|n| n.copy());
        }

        #[unsafe(method_id(fileAttributes))]
        fn file_attributes(&self) -> Retained<AnyObject> {
            self.ivars().attributes.borrow().clone()
        }

        #[unsafe(method(setFileAttributes:))]
        fn set_file_attributes(&self, attributes: &AnyObject) {
            if value(attributes, &FILE_TYPE).is_none() || value(attributes, &POSIX_PERMISSIONS).is_none() {
                panic!(
                    "-[NSFileWrapper setFileAttributes:] *** file attributes cannot be nil and must contain at least \
                     NSFileType and NSFilePosixPermissions."
                );
            }
            // SAFETY: -copy of a dictionary is an immutable dictionary.
            *self.ivars().attributes.borrow_mut() = unsafe { msg_send![attributes, copy] };
        }

        #[unsafe(method(matchesContentsOfURL:))]
        fn matches_contents(&self, url: &NSURL) -> bool {
            let path = url.path().map(|p| p.to_string()).unwrap_or_default();
            matches(self, Path::new(&path))
        }

        #[unsafe(method(readFromURL:options:error:))]
        fn read_from_url(&self, url: &NSURL, _options: NSUInteger, error: *mut *mut NSError) -> bool {
            let path = url.path().map(|p| p.to_string()).unwrap_or_default();
            match read(Path::new(&path)) {
                Ok(read) => {
                    *self.ivars().kind.borrow_mut() = kind_of(read.kind);
                    *self.ivars().attributes.borrow_mut() = read.attributes;
                    true
                }
                Err(e) => {
                    // SAFETY: the caller passes null or room for an error.
                    unsafe { error::set(error, error::file(FileOp::Read, &e, &path)) };
                    false
                }
            }
        }

        #[unsafe(method(writeToURL:options:originalContentsURL:error:))]
        fn write_to_url(
            &self,
            url: &NSURL,
            options: NSUInteger,
            _original: Option<&NSURL>,
            error: *mut *mut NSError,
        ) -> bool {
            let path = url.path().map(|p| p.to_string()).unwrap_or_default();
            match write(self, Path::new(&path), options & WITH_NAME_UPDATING != 0) {
                Ok(()) => true,
                Err(e) => {
                    // SAFETY: the caller passes null or room for an error.
                    unsafe { error::set(error, error::file(FileOp::Write, &e, &path)) };
                    false
                }
            }
        }

        #[unsafe(method_id(serializedRepresentation))]
        fn serialized_representation(&self) -> Option<Retained<NSData>> {
            None
        }

        #[unsafe(method_id(addFileWrapper:))]
        fn add_file_wrapper(&self, child: &NSFileWrapper) -> Retained<NSString> {
            add(self, child)
        }

        #[unsafe(method_id(addRegularFileWithContents:preferredFilename:))]
        fn add_regular(&self, contents: &NSData, name: &NSString) -> Retained<NSString> {
            let child: Retained<NSFileWrapper> =
                NSFileWrapper::initRegularFileWithContents(NSFileWrapper::alloc(), contents);
            child.setPreferredFilename(Some(name));
            add(self, &child)
        }

        #[unsafe(method(removeFileWrapper:))]
        fn remove_file_wrapper(&self, child: &NSFileWrapper) {
            if let Kind::Directory(children) = &mut *self.ivars().kind.borrow_mut() {
                let target = child as *const NSFileWrapper as *const NSFileWrapperImpl;
                children.retain(|(_, c)| !std::ptr::eq(&**c, target));
            }
            changed(self);
        }

        #[unsafe(method_id(fileWrappers))]
        fn file_wrappers(&self) -> Retained<NSDictionary<NSString, AnyObject>> {
            let kind = self.ivars().kind.borrow();
            let Kind::Directory(children) = &*kind else { directory_only("fileWrappers") };
            let keys: Vec<Retained<NSString>> = children.iter().map(|(k, _)| NSString::from_str(k)).collect();
            let values: Vec<Retained<AnyObject>> = children.iter().map(|(_, c)| c.clone().into()).collect();
            let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            NSDictionary::from_retained_objects(&keys, &values)
        }

        #[unsafe(method_id(keyForFileWrapper:))]
        fn key_for_file_wrapper(&self, child: &NSFileWrapper) -> Option<Retained<NSString>> {
            key_of(self, child)
        }

        #[unsafe(method_id(regularFileContents))]
        fn regular_file_contents(&self) -> Retained<NSData> {
            match &*self.ivars().kind.borrow() {
                Kind::Regular(data) => data.clone(),
                _ => panic!("-[NSFileWrapper regularFileContents] *** this method is only for regular file type NSFileWrappers"),
            }
        }

        #[unsafe(method_id(symbolicLinkDestinationURL))]
        fn symbolic_link_destination_url(&self) -> Option<Retained<NSURL>> {
            destination(self)
        }

        #[unsafe(method_id(symbolicLinkDestination))]
        fn symbolic_link_destination(&self) -> Option<Retained<NSString>> {
            destination(self).and_then(|u| u.path())
        }
    }

    unsafe impl NSObjectProtocol for NSFileWrapperImpl {}
);

/// Add `child` to the directory `this` under its preferred name, or a key
/// made from it that isn't taken, as `addFileWrapper:` does.
fn add(this: &NSFileWrapperImpl, child: &NSFileWrapper) -> Retained<NSString> {
    let child = wrapper(child).unwrap_or_else(|| panic!("-[NSFileWrapper addFileWrapper:] *** not a file wrapper"));
    let Some(name) = child.ivars().preferred.borrow().as_ref().map(|n| n.to_string()) else {
        panic!(
            "-[NSFileWrapper addFileWrapper:] *** a document must have a preferredFilename before it can be added as \
             the subdocument of another document."
        );
    };
    let mut kind = this.ivars().kind.borrow_mut();
    let Kind::Directory(children) = &mut *kind else { directory_only("addFileWrapper:") };
    let key = free_key(children, &name);
    children.push((key.clone(), child.retain()));
    drop(kind);
    changed(this);
    NSString::from_str(&key)
}

/// `name`, or the first of `1__#$!@%!#__name`, `2__#$!@%!#__name`, ...
/// no child has as its key.
fn free_key(children: &[(String, Retained<NSFileWrapperImpl>)], name: &str) -> String {
    let mut key = name.to_owned();
    let mut n = 0;
    while children.iter().any(|(k, _)| *k == key) {
        n += 1;
        key = format!("{n}__#$!@%!#__{name}");
    }
    key
}

/// The key `child` has among the directory `this`'s children.
fn key_of(this: &NSFileWrapperImpl, child: &NSFileWrapper) -> Option<Retained<NSString>> {
    let kind = this.ivars().kind.borrow();
    let Kind::Directory(children) = &*kind else { return None };
    let target = child as *const NSFileWrapper as *const NSFileWrapperImpl;
    children.iter().find(|(_, c)| std::ptr::eq(&**c, target)).map(|(k, _)| NSString::from_str(k))
}

/// A directory whose children changed no longer has the modification date
/// it was read with.
fn changed(this: &NSFileWrapperImpl) {
    let attributes = this.ivars().attributes.borrow().clone();
    if value(&attributes, &MODIFICATION_DATE).is_none() {
        return;
    }
    // SAFETY: the attributes are a dictionary; its mutable copy takes
    // -removeObjectForKey: and copies back to an immutable one.
    let kept: Retained<AnyObject> = unsafe {
        let copy: Retained<AnyObject> = msg_send![&*attributes, mutableCopy];
        let _: () = msg_send![&*copy, removeObjectForKey: constant(&MODIFICATION_DATE)];
        msg_send![&*copy, copy]
    };
    *this.ivars().attributes.borrow_mut() = kept;
}

/// The value for `key` in an attributes dictionary.
fn value(attributes: &AnyObject, key: &'static crate::ConstantString) -> Option<Retained<AnyObject>> {
    // SAFETY: a dictionary; -objectForKey: returns a value or nil.
    unsafe { msg_send![attributes, objectForKey: constant(key)] }
}

/// The modification date in `attributes`, as a time since the Unix epoch.
fn modified(attributes: &AnyObject) -> Option<std::time::SystemTime> {
    let date = value(attributes, &MODIFICATION_DATE)?;
    // SAFETY: the date attribute is an NSDate.
    let since_1970: f64 = unsafe { msg_send![&*date, timeIntervalSince1970] };
    let since = std::time::Duration::try_from_secs_f64(since_1970.abs()).ok()?;
    if since_1970 >= 0.0 { std::time::UNIX_EPOCH.checked_add(since) } else { std::time::UNIX_EPOCH.checked_sub(since) }
}

/// Whether `wrapper` matches the item at `path` (see the module notes).
fn matches(wrapper: &NSFileWrapperImpl, path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    let Some(date) = modified(&wrapper.ivars().attributes.borrow()) else { return false };
    let Ok(mtime) = meta.modified() else { return false };
    let apart = mtime.duration_since(date).unwrap_or_else(|e| e.duration());
    if apart >= std::time::Duration::from_secs(1) {
        return false;
    }
    match &*wrapper.ivars().kind.borrow() {
        Kind::Regular(_) => meta.is_file(),
        Kind::Link(_) => meta.file_type().is_symlink(),
        Kind::Directory(children) => {
            meta.is_dir() && children.iter().all(|(key, child)| matches(child, &path.join(key)))
        }
    }
}

/// A symbolic link's destination.
fn destination(this: &NSFileWrapperImpl) -> Option<Retained<NSURL>> {
    match &*this.ivars().kind.borrow() {
        Kind::Link(url) => Some(url.clone()),
        _ => None,
    }
}

#[cold]
fn directory_only(method: &str) -> ! {
    panic!("-[NSFileWrapper {method}] *** this method is only for directory type NSFileWrappers")
}

/// The wrapper an object is, if it's one of this class's.
fn wrapper(object: &AnyObject) -> Option<&NSFileWrapperImpl> {
    object.downcast_ref::<NSFileWrapperImpl>()
}

/// The attributes of a wrapper made in memory, as macOS gives them.
fn fresh_attributes(kind: &Kind) -> Retained<AnyObject> {
    let (file_type, permissions) = match kind {
        Kind::Regular(_) => (&TYPE_REGULAR, 0o666),
        Kind::Directory(_) => (&TYPE_DIRECTORY, 0o777),
        Kind::Link(_) => (&TYPE_SYMBOLIC_LINK, 0o666),
    };
    let mut keys = vec![constant(&POSIX_PERMISSIONS), constant(&FILE_TYPE)];
    let mut values: Vec<Retained<AnyObject>> =
        vec![NSNumber::new_u16(permissions).into(), constant(file_type).retain().into()];
    if matches!(kind, Kind::Regular(_)) {
        keys.push(constant(&MODIFICATION_DATE));
        values.push(NSDate::now().into());
    }
    NSDictionary::from_retained_objects(&keys, &values).into()
}

fn make(this: Allocated<NSFileWrapperImpl>, kind: Kind) -> Retained<NSFileWrapperImpl> {
    let attributes = fresh_attributes(&kind);
    let this = this.set_ivars(WrapperIvars {
        kind: RefCell::new(kind),
        preferred: RefCell::new(None),
        filename: RefCell::new(None),
        attributes: RefCell::new(attributes),
    });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

/// A file or directory read from disk, before it becomes wrappers.
struct Read {
    name: Option<String>,
    kind: ReadKind,
    attributes: Retained<AnyObject>,
}

enum ReadKind {
    Regular(Vec<u8>),
    Directory(Vec<Read>),
    Link(String),
}

fn read(path: &Path) -> std::io::Result<Read> {
    let meta = std::fs::symlink_metadata(path)?;
    let attributes = crate::file_manager::attributes_of_item(&meta);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let kind = if meta.file_type().is_symlink() {
        ReadKind::Link(std::fs::read_link(path)?.to_string_lossy().into_owned())
    } else if meta.is_dir() {
        let mut children = Vec::new();
        for entry in std::fs::read_dir(path)? {
            children.push(read(&entry?.path())?);
        }
        children.sort_by(|a, b| a.name.cmp(&b.name));
        ReadKind::Directory(children)
    } else {
        ReadKind::Regular(std::fs::read(path)?)
    };
    Ok(Read { name, kind, attributes })
}

fn kind_of(read: ReadKind) -> Kind {
    match read {
        ReadKind::Regular(bytes) => Kind::Regular(NSData::with_bytes(&bytes)),
        ReadKind::Link(dest) => Kind::Link(NSURL::fileURLWithPath(&NSString::from_str(&dest))),
        ReadKind::Directory(children) => Kind::Directory(
            children
                .into_iter()
                .map(|child| (child.name.clone().unwrap_or_default(), from_read(NSFileWrapperImpl::alloc(), child)))
                .collect(),
        ),
    }
}

fn from_read(this: Allocated<NSFileWrapperImpl>, read: Read) -> Retained<NSFileWrapperImpl> {
    let name = read.name.as_deref().map(NSString::from_str);
    let this = make(this, kind_of(read.kind));
    *this.ivars().attributes.borrow_mut() = read.attributes;
    *this.ivars().preferred.borrow_mut() = name.clone();
    *this.ivars().filename.borrow_mut() = name;
    this
}

/// Write `wrapper` at `path` (a directory's children under their keys),
/// giving each child the name it was written under if `rename` (the top
/// keeps its own, as on macOS).
fn write(wrapper: &NSFileWrapperImpl, path: &Path, rename: bool) -> std::io::Result<()> {
    match &*wrapper.ivars().kind.borrow() {
        Kind::Regular(data) => std::fs::write(path, crate::data::to_vec(data))?,
        Kind::Link(url) => {
            let dest = url.path().map(|p| p.to_string()).unwrap_or_default();
            std::os::unix::fs::symlink(dest, path)?;
        }
        Kind::Directory(children) => {
            std::fs::create_dir_all(path)?;
            for (key, child) in children {
                write(child, &path.join(key), rename)?;
                if rename {
                    *child.ivars().filename.borrow_mut() = Some(NSString::from_str(key));
                }
            }
        }
    }
    let is_link = matches!(*wrapper.ivars().kind.borrow(), Kind::Link(_));
    if let Some(date) = modified(&wrapper.ivars().attributes.borrow()).filter(|_| !is_link) {
        std::fs::File::open(path)?.set_modified(date)?;
    }
    Ok(())
}
