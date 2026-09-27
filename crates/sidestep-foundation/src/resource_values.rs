//! A file URL's resource values (`getResourceValue:forKey:error:`,
//! `resourceValuesForKeys:error:`), read from the file system as they're
//! asked for, and the collections' `…WithContentsOfURL:` and
//! `…WithContentsOfFile:` initializers, which read property lists.
//!
//! As measured on macOS: `resourceValuesForKeys:error:` leaves out keys
//! it has no value for, unknown ones included; a file that isn't there is
//! nil with Cocoa's error 260 (with the path, the URL and the POSIX error
//! underneath); a URL that isn't a file URL has no values and no error.
//! The item's own type counts (a symbolic link is one, not what it points
//! to).
//!
//! The keys answered: name and localized name, the type flags (regular
//! file, directory, symbolic link, volume, package, alias, hidden, mount
//! trigger, system and user immutable), the dates (creation where the file
//! system keeps it, content access and modification, attribute
//! modification), link count, parent directory, sizes (file, allocated,
//! total), access (readable, writable, executable), the path and canonical
//! path, the resource type, the hidden extension flag, the file security
//! (an empty one) and the preferred I/O block size. Linux has no packages,
//! mount triggers, user-immutable flags or hidden extensions, so those are
//! NO; a symbolic link is an alias file, as on macOS, and its access is
//! its own mode's, not what it points to's. The canonical path resolves
//! the directories above the item, not the item itself. No volume keys.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{AnyThread, ClassType, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSArray, NSDate, NSDictionary, NSError, NSMutableDictionary, NSNumber, NSString, NSURL};

use crate::error::{self, FileOp};

/// A resource value of `path` for `key`, if it has one.
fn value(path: &Path, meta: &std::fs::Metadata, key: &str) -> Option<Retained<AnyObject>> {
    let yes_no = |b: bool| Some(object(NSNumber::new_bool(b)));
    let number = |n: u64| Some(object(NSNumber::new_u64(n)));
    let date = |t: std::io::Result<std::time::SystemTime>| {
        let t = t.ok()?;
        let secs = match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        };
        Some(object(NSDate::dateWithTimeIntervalSince1970(secs)))
    };
    let text = |s: &str| Some(object(NSString::from_str(s)));
    let kind = meta.file_type();
    let name = || path.file_name().map_or_else(|| "/".to_owned(), |n| n.to_string_lossy().into_owned());
    match key {
        "NSURLNameKey" | "NSURLLocalizedNameKey" => text(&name()),
        "NSURLIsRegularFileKey" => yes_no(kind.is_file()),
        "NSURLIsDirectoryKey" => yes_no(kind.is_dir()),
        "NSURLIsSymbolicLinkKey" => yes_no(kind.is_symlink()),
        "NSURLIsVolumeKey" => yes_no(is_volume(path, meta)),
        "NSURLIsPackageKey" | "NSURLHasHiddenExtensionKey" | "NSURLIsMountTriggerKey" | "NSURLIsUserImmutableKey" => {
            yes_no(false)
        }
        "NSURLIsAliasFileKey" => yes_no(kind.is_symlink()),
        "NSURLIsSystemImmutableKey" => yes_no(immutable(path, meta)),
        "NSURLFileSecurityKey" => Some(crate::cf::file_security()),
        "NSURLIsHiddenKey" => yes_no(name().starts_with('.')),
        "NSURLCreationDateKey" => date(meta.created()),
        "NSURLContentAccessDateKey" => date(meta.accessed()),
        "NSURLContentModificationDateKey" => date(meta.modified()),
        "NSURLAttributeModificationDateKey" => {
            let at = std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(meta.ctime().max(0) as u64)
                + std::time::Duration::from_nanos(meta.ctime_nsec().max(0) as u64);
            date(Ok(at))
        }
        "NSURLLinkCountKey" => number(meta.nlink()),
        "NSURLParentDirectoryURLKey" => {
            let parent = path.parent()?;
            crate::url::file_url(parent).map(object)
        }
        "NSURLFileSizeKey" | "NSURLTotalFileSizeKey" if !kind.is_dir() => number(meta.len()),
        "NSURLFileAllocatedSizeKey" | "NSURLTotalFileAllocatedSizeKey" if !kind.is_dir() => number(meta.blocks() * 512),
        "NSURLIsReadableKey" => yes_no(allowed(path, meta, libc::R_OK)),
        "NSURLIsWritableKey" => yes_no(allowed(path, meta, libc::W_OK)),
        "NSURLIsExecutableKey" => yes_no(allowed(path, meta, libc::X_OK)),
        "_NSURLPathKey" => text(&path.to_string_lossy()),
        "NSURLCanonicalPathKey" => {
            // The directories above resolved; the item itself (a link,
            // say) kept.
            let canonical = match (path.parent(), path.file_name()) {
                (Some(parent), Some(name)) => std::fs::canonicalize(parent).ok()?.join(name),
                _ => std::fs::canonicalize(path).ok()?,
            };
            text(&canonical.to_string_lossy())
        }
        "NSURLFileResourceTypeKey" => text(if kind.is_dir() {
            "NSURLFileResourceTypeDirectory"
        } else if kind.is_file() {
            "NSURLFileResourceTypeRegular"
        } else if kind.is_symlink() {
            "NSURLFileResourceTypeSymbolicLink"
        } else if kind.is_fifo() {
            "NSURLFileResourceTypeNamedPipe"
        } else if kind.is_char_device() {
            "NSURLFileResourceTypeCharacterSpecial"
        } else if kind.is_block_device() {
            "NSURLFileResourceTypeBlockSpecial"
        } else if kind.is_socket() {
            "NSURLFileResourceTypeSocket"
        } else {
            "NSURLFileResourceTypeUnknown"
        }),
        "NSURLPreferredIOBlockSizeKey" => number(meta.blksize()),
        _ => None,
    }
}

fn object<T: objc2::Message>(value: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(value) }
}

/// The root, or a directory on another device than its parent.
fn is_volume(path: &Path, meta: &std::fs::Metadata) -> bool {
    match path.parent() {
        None => true,
        Some(parent) => std::fs::metadata(parent).is_ok_and(|p| p.dev() != meta.dev()) && meta.is_dir(),
    }
}

/// Whether this process may read, write or execute (`mode`) the item: a
/// symbolic link by its own mode, anything else as `access` says.
fn allowed(path: &Path, meta: &std::fs::Metadata, mode: libc::c_int) -> bool {
    if !meta.file_type().is_symlink() {
        return access(path, mode);
    }
    let bits = meta.mode();
    // SAFETY: plain queries of this process's credentials.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    if uid == 0 {
        // The superuser reads and writes anything, and executes what
        // anyone may.
        return mode != libc::X_OK || bits & 0o111 != 0;
    }
    let shift = if meta.uid() == uid {
        6
    } else if meta.gid() == gid || groups().contains(&meta.gid()) {
        3
    } else {
        0
    };
    (bits >> shift) & mode as u32 != 0
}

/// This process's supplementary groups.
fn groups() -> Vec<libc::gid_t> {
    // SAFETY: a count query, then a buffer of that many.
    unsafe {
        let n = libc::getgroups(0, std::ptr::null_mut());
        let mut groups = vec![0; n.max(0) as usize];
        let n = libc::getgroups(n, groups.as_mut_ptr());
        groups.truncate(n.max(0) as usize);
        groups
    }
}

/// Whether a regular file or directory has the immutable flag (`chattr
/// +i`, which only the superuser sets: macOS's system-immutable flag).
fn immutable(path: &Path, meta: &std::fs::Metadata) -> bool {
    const FS_IMMUTABLE_FL: libc::c_long = 0x10;
    if !(meta.is_file() || meta.is_dir()) {
        return false;
    }
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: a NUL-terminated path; the descriptor is closed below.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 {
        return false;
    }
    let mut flags: libc::c_long = 0;
    // SAFETY: FS_IOC_GETFLAGS writes the flags through the pointer.
    let ok = unsafe { libc::ioctl(fd, libc::FS_IOC_GETFLAGS, &mut flags) } == 0;
    // SAFETY: the descriptor opened above.
    unsafe { libc::close(fd) };
    ok && flags & FS_IMMUTABLE_FL != 0
}

fn access(path: &Path, mode: libc::c_int) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: a NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), mode) == 0 }
}

/// The item's metadata (not following a final symbolic link), or the
/// error to report.
fn metadata(path: &Path) -> Result<std::fs::Metadata, Retained<NSError>> {
    std::fs::symlink_metadata(path).map_err(|e| error::file(FileOp::Read, &e, &path.to_string_lossy()))
}

/// Keys and their values.
pub(crate) type Values = Vec<(Retained<NSString>, Retained<AnyObject>)>;

/// The values of `keys` for `url`: `Ok(None)` for a URL that isn't a
/// file's, which has none. A key the file has no value for takes the
/// temporary value set for it on this URL object, if any.
fn values_for(url: &NSURL, keys: &[Retained<NSString>]) -> Result<Option<Values>, Retained<NSError>> {
    let Some(path) = crate::url::file_path(url) else { return Ok(None) };
    let path = trim_slash(path);
    let meta = metadata(&path)?;
    Ok(Some(
        keys.iter()
            .filter_map(|k| {
                let key = k.to_string();
                value(&path, &meta, &key).or_else(|| temporary(url, &key)).map(|v| (k.clone(), v))
            })
            .collect(),
    ))
}

// Temporary resource values: set on one URL object, read back from it
// alone, dropped with it or when its cache is cleared, as on macOS.

/// A URL object's temporary values, kept as an associated object.
struct Temporary(std::sync::Mutex<Vec<(String, Retained<AnyObject>)>>);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "_SidestepURLTemporaryValues"]
    #[ivars = Temporary]
    struct TemporaryValues;
);

static TEMPORARY_KEY: u8 = 0;
/// Held while a URL's store is looked up or made, so two threads don't
/// each make one.
static TEMPORARY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The URL's store of temporary values, made if `create`.
fn temporaries(url: &NSURL, create: bool) -> Option<Retained<TemporaryValues>> {
    let _guard = crate::thread::lock(&TEMPORARY_LOCK);
    let key = (&TEMPORARY_KEY as *const u8).cast();
    let object = (url as *const NSURL).cast_mut().cast::<AnyObject>();
    // SAFETY: a live object and a static key.
    let found = unsafe { objc2::ffi::objc_getAssociatedObject(object, key) };
    if !found.is_null() {
        // SAFETY: only this function associates objects under the key.
        return unsafe { Retained::retain(found.cast::<TemporaryValues>().cast_mut()) };
    }
    if !create {
        return None;
    }
    let store = TemporaryValues::alloc().set_ivars(Temporary(std::sync::Mutex::new(Vec::new())));
    // SAFETY: NSObject's initializer.
    let store: Retained<TemporaryValues> = unsafe { msg_send![super(store), init] };
    // SAFETY: a retaining association keeps the store for the URL's life.
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            object,
            key,
            Retained::as_ptr(&store).cast_mut().cast(),
            objc2::ffi::OBJC_ASSOCIATION_RETAIN,
        )
    };
    Some(store)
}

/// A resource value of `url` for `key`: `Ok(None)` for none (or a URL
/// that isn't a file's).
pub(crate) fn value_of(url: &NSURL, key: &str) -> Result<Option<Retained<AnyObject>>, Retained<NSError>> {
    let key = NSString::from_str(key);
    Ok(values_for(url, std::slice::from_ref(&key))?.and_then(|v| v.into_iter().next()).map(|(_, v)| v))
}

/// The resource values `url` has of `keys`.
pub(crate) fn values_of(url: &NSURL, keys: &[Retained<NSString>]) -> Result<Option<Values>, Retained<NSError>> {
    values_for(url, keys)
}

fn temporary(url: &NSURL, key: &str) -> Option<Retained<AnyObject>> {
    let store = temporaries(url, false)?;
    let values = crate::thread::lock(&store.ivars().0);
    values.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

pub(crate) fn set_temporary(url: &NSURL, key: &str, value: Option<&AnyObject>) {
    let Some(store) = temporaries(url, value.is_some()) else { return };
    // Released after the lock, as in `clear_temporary`.
    let replaced: Vec<_> = {
        let mut values = crate::thread::lock(&store.ivars().0);
        let replaced = values.extract_if(.., |(k, _)| k == key).collect();
        if let Some(value) = value {
            values.push((key.to_owned(), value.retain()));
        }
        replaced
    };
    drop(replaced);
}

pub(crate) fn clear_temporary(url: &NSURL, key: Option<&str>) {
    let Some(store) = temporaries(url, false) else { return };
    // Released after the lock, since releasing a value can run any code.
    let dropped: Vec<_> = {
        let mut values = crate::thread::lock(&store.ivars().0);
        match key {
            Some(key) => values.extract_if(.., |(k, _)| k == key).collect(),
            None => std::mem::take(&mut *values),
        }
    };
    drop(dropped);
}

/// Set a resource value of a file URL's item: its content modification
/// and access dates, which Linux keeps. Other keys, and URLs that aren't
/// files', are accepted and left as they are, as macOS does for keys it
/// can't set; an item that isn't there is an error.
pub(crate) fn set_value(url: &NSURL, key: &str, value: Option<&AnyObject>) -> Result<(), Retained<NSError>> {
    let Some(path) = crate::url::file_path(url) else { return Ok(()) };
    let path = trim_slash(path);
    let path_text = path.to_string_lossy().into_owned();
    std::fs::symlink_metadata(&path).map_err(|e| error::file(FileOp::Write, &e, &path_text))?;
    let date = value.and_then(|v| v.downcast_ref::<NSDate>()).map(|d| d.timeIntervalSince1970());
    let which = match key {
        "NSURLContentModificationDateKey" => 1,
        "NSURLContentAccessDateKey" => 0,
        _ => return Ok(()),
    };
    let Some(seconds) = date else { return Ok(()) };
    let mut times = [libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT }; 2];
    // Whole seconds down, so the nanoseconds are never negative (a date
    // before 1970 has a negative fraction).
    let whole = seconds.floor();
    times[which] = libc::timespec {
        tv_sec: whole as libc::time_t,
        tv_nsec: ((seconds - whole) * 1e9).round().min(999_999_999.0) as libc::c_long,
    };
    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return Ok(()) };
    // SAFETY: a NUL-terminated path and two timespecs.
    if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) } != 0 {
        return Err(error::file(FileOp::Write, &std::io::Error::last_os_error(), &path_text));
    }
    Ok(())
}

/// A directory URL's path ends in a slash, which names the same item.
fn trim_slash(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if text.len() > 1 && text.ends_with('/') { PathBuf::from(text.trim_end_matches('/')) } else { path }
}

define_class!(
    // NSURL's resource values. `self` is a URL there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepURLResourceValues"]
    struct ResourceValues;

    impl ResourceValues {
        #[unsafe(method(getResourceValue:forKey:error:))]
        fn get_resource_value(&self, out: *mut *mut AnyObject, key: &NSString, error: *mut *mut NSError) -> Bool {
            // SAFETY: the category adds this to NSURL.
            let url = unsafe { &*(self as *const Self).cast::<NSURL>() };
            match values_for(url, std::slice::from_ref(&key.retain())) {
                Ok(values) => {
                    let found = values.and_then(|v| v.into_iter().next()).map(|(_, v)| v);
                    if !out.is_null() {
                        // SAFETY: the caller's storage for an object.
                        unsafe { out.write(found.map_or(std::ptr::null_mut(), Retained::autorelease_ptr)) };
                    }
                    Bool::YES
                }
                Err(e) => {
                    // SAFETY: the caller's error storage, or null.
                    unsafe { error::set(error, e) };
                    Bool::NO
                }
            }
        }

        #[unsafe(method_id(resourceValuesForKeys:error:))]
        fn resource_values_for_keys(
            &self,
            keys: &NSArray<NSString>,
            error: *mut *mut NSError,
        ) -> Option<Retained<NSDictionary<NSString, AnyObject>>> {
            // SAFETY: the category adds this to NSURL.
            let url = unsafe { &*(self as *const Self).cast::<NSURL>() };
            match values_for(url, &keys.to_vec()) {
                Ok(values) => {
                    let values = values.unwrap_or_default();
                    let keys: Vec<&NSString> = values.iter().map(|(k, _)| &**k).collect();
                    let objects: Vec<Retained<AnyObject>> = values.iter().map(|(_, v)| v.clone()).collect();
                    Some(NSDictionary::from_retained_objects(&keys, &objects))
                }
                Err(e) => {
                    // SAFETY: the caller's error storage, or null.
                    unsafe { error::set(error, e) };
                    None
                }
            }
        }

        /// Values aren't cached: each is read when asked for. Clearing the
        /// cache drops the temporary values.
        #[unsafe(method(removeAllCachedResourceValues))]
        fn remove_all_cached_resource_values(&self) {
            // SAFETY: the category adds this to NSURL.
            clear_temporary(unsafe { &*(self as *const Self).cast::<NSURL>() }, None);
        }

        #[unsafe(method(removeCachedResourceValueForKey:))]
        fn remove_cached_resource_value_for_key(&self, key: &NSString) {
            // SAFETY: the category adds this to NSURL.
            clear_temporary(unsafe { &*(self as *const Self).cast::<NSURL>() }, Some(&key.to_string()));
        }

        #[unsafe(method(setTemporaryResourceValue:forKey:))]
        fn set_temporary_resource_value(&self, value: Option<&AnyObject>, key: &NSString) {
            // SAFETY: the category adds this to NSURL.
            set_temporary(unsafe { &*(self as *const Self).cast::<NSURL>() }, &key.to_string(), value);
        }

        #[unsafe(method(setResourceValue:forKey:error:))]
        fn set_resource_value(&self, value: Option<&AnyObject>, key: &NSString, error: *mut *mut NSError) -> Bool {
            // SAFETY: the category adds this to NSURL.
            let url = unsafe { &*(self as *const Self).cast::<NSURL>() };
            match set_value(url, &key.to_string(), value) {
                Ok(()) => Bool::YES,
                Err(e) => {
                    // SAFETY: the caller's error storage, or null.
                    unsafe { error::set(error, e) };
                    Bool::NO
                }
            }
        }

        #[unsafe(method(setResourceValues:error:))]
        fn set_resource_values(&self, values: &NSDictionary<NSString, AnyObject>, error: *mut *mut NSError) -> Bool {
            // SAFETY: the category adds this to NSURL.
            let url = unsafe { &*(self as *const Self).cast::<NSURL>() };
            // SAFETY: a dictionary is a dictionary whatever its types.
            let values = unsafe { &*(values as *const NSDictionary<NSString, AnyObject>).cast::<NSDictionary>() };
            for (key, value) in crate::plist::dictionary_entries(values) {
                // SAFETY: -description of a key; keys are strings.
                let key = unsafe { &*Retained::as_ptr(&key).cast::<NSString>() }.to_string();
                if let Err(e) = set_value(url, &key, Some(&value)) {
                    // SAFETY: the caller's error storage, or null.
                    unsafe { error::set(error, e) };
                    return Bool::NO;
                }
            }
            Bool::YES
        }
    }
);

sidestep_runtime::category!("NSURL"(SidestepResourceValues), |category| {
    // SAFETY: the helper's methods treat their receiver as a URL.
    unsafe { category.add_methods_of(ResourceValues::class()) };
});

// Collections from property-list files.

/// The property list in the file `url` (or `path`) names, if it reads.
fn read_list(path: Option<PathBuf>) -> Option<Retained<AnyObject>> {
    let bytes = std::fs::read(path?).ok()?;
    let (value, _) = crate::plist::parse(&bytes)?;
    crate::plist::to_object(&value)
}

fn url_path(url: &NSURL) -> Option<PathBuf> {
    crate::url::file_path(url)
}

fn string_path(path: &NSString) -> Option<PathBuf> {
    let path = path.to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// `list` if it is a dictionary (or an array).
fn dictionary(list: Option<Retained<AnyObject>>) -> Option<Retained<NSDictionary>> {
    list?.downcast::<NSDictionary>().ok()
}

fn array(list: Option<Retained<AnyObject>>) -> Option<Retained<NSArray>> {
    list?.downcast::<NSArray>().ok()
}

define_class!(
    // `self` is an allocated dictionary (or the dictionary class) there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepDictionaryContents"]
    struct DictionaryContents;

    impl DictionaryContents {
        #[unsafe(method_id(initWithContentsOfURL:))]
        fn init_with_contents_of_url(this: Allocated<Self>, url: &NSURL) -> Option<Retained<Self>> {
            init_dictionary(this, dictionary(read_list(url_path(url))))
        }

        #[unsafe(method_id(initWithContentsOfFile:))]
        fn init_with_contents_of_file(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            init_dictionary(this, dictionary(read_list(string_path(path))))
        }

        /// A mutable dictionary, which serves `NSMutableDictionary` too, as
        /// macOS's does.
        #[unsafe(method_id(dictionaryWithContentsOfURL:))]
        fn dictionary_with_contents_of_url(url: &NSURL) -> Option<Retained<NSMutableDictionary>> {
            dictionary(read_list(url_path(url))).map(|d| mutable_dictionary(&d))
        }

        #[unsafe(method_id(dictionaryWithContentsOfFile:))]
        fn dictionary_with_contents_of_file(path: &NSString) -> Option<Retained<NSMutableDictionary>> {
            dictionary(read_list(string_path(path))).map(|d| mutable_dictionary(&d))
        }
    }
);

define_class!(
    // `self` is an allocated array (or the array class) there.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepArrayContents"]
    struct ArrayContents;

    impl ArrayContents {
        #[unsafe(method_id(initWithContentsOfURL:))]
        fn init_with_contents_of_url(this: Allocated<Self>, url: &NSURL) -> Option<Retained<Self>> {
            init_array(this, array(read_list(url_path(url))))
        }

        #[unsafe(method_id(initWithContentsOfFile:))]
        fn init_with_contents_of_file(this: Allocated<Self>, path: &NSString) -> Option<Retained<Self>> {
            init_array(this, array(read_list(string_path(path))))
        }

        #[unsafe(method_id(arrayWithContentsOfURL:))]
        fn array_with_contents_of_url(url: &NSURL) -> Option<Retained<objc2_foundation::NSMutableArray>> {
            array(read_list(url_path(url))).map(|a| mutable_array(&a))
        }

        #[unsafe(method_id(arrayWithContentsOfFile:))]
        fn array_with_contents_of_file(path: &NSString) -> Option<Retained<objc2_foundation::NSMutableArray>> {
            array(read_list(string_path(path))).map(|a| mutable_array(&a))
        }
    }
);

/// Initialize the allocated dictionary with the list's contents, or give
/// it up for nil.
fn init_dictionary<T: objc2::Message>(this: Allocated<T>, list: Option<Retained<NSDictionary>>) -> Option<Retained<T>> {
    let list = list?;
    // SAFETY: the allocated object is a dictionary, whose initializer takes
    // a dictionary.
    unsafe { msg_send![this, initWithDictionary: &*list] }
}

fn init_array<T: objc2::Message>(this: Allocated<T>, list: Option<Retained<NSArray>>) -> Option<Retained<T>> {
    let list = list?;
    // SAFETY: the allocated object is an array, whose initializer takes an
    // array.
    unsafe { msg_send![this, initWithArray: &*list] }
}

fn mutable_dictionary(d: &NSDictionary) -> Retained<NSMutableDictionary> {
    // SAFETY: -mutableCopy of a dictionary is a mutable dictionary.
    unsafe { msg_send![d, mutableCopy] }
}

fn mutable_array(a: &NSArray) -> Retained<objc2_foundation::NSMutableArray> {
    // SAFETY: -mutableCopy of an array is a mutable array.
    unsafe { msg_send![a, mutableCopy] }
}

sidestep_runtime::category!("NSDictionary"(SidestepContentsOfURL), |category| {
    // SAFETY: the helper's methods treat their receiver as a dictionary
    // (or the class).
    unsafe { category.add_methods_of(DictionaryContents::class()) };
});

sidestep_runtime::category!("NSArray"(SidestepContentsOfURL), |category| {
    // SAFETY: the helper's methods treat their receiver as an array (or the
    // class).
    unsafe { category.add_methods_of(ArrayContents::class()) };
});
