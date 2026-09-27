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
//! file, directory, symbolic link, volume, package, alias, hidden), the
//! dates (creation where the file system keeps it, content access and
//! modification, attribute modification), link count, parent directory,
//! sizes (file, allocated, total), access (readable, writable,
//! executable), the path and canonical path, the resource type, the
//! hidden extension flag and the preferred I/O block size. Linux has no
//! packages, aliases or hidden extensions, so those are NO.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{ClassType, Message, define_class, msg_send};
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
        "NSURLIsPackageKey" | "NSURLIsAliasFileKey" | "NSURLHasHiddenExtensionKey" => yes_no(false),
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
        "NSURLIsReadableKey" => yes_no(access(path, libc::R_OK)),
        "NSURLIsWritableKey" => yes_no(access(path, libc::W_OK)),
        "NSURLIsExecutableKey" => yes_no(access(path, libc::X_OK)),
        "_NSURLPathKey" => text(&path.to_string_lossy()),
        "NSURLCanonicalPathKey" => text(&std::fs::canonicalize(path).ok()?.to_string_lossy()),
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
type Values = Vec<(Retained<NSString>, Retained<AnyObject>)>;

/// The values of `keys` for `url`: `Ok(None)` for a URL that isn't a
/// file's, which has none.
fn values_for(url: &NSURL, keys: &[Retained<NSString>]) -> Result<Option<Values>, Retained<NSError>> {
    let Some(path) = crate::url::file_path(url) else { return Ok(None) };
    let path = trim_slash(path);
    let meta = metadata(&path)?;
    Ok(Some(keys.iter().filter_map(|k| value(&path, &meta, &k.to_string()).map(|v| (k.clone(), v))).collect()))
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

        /// Values aren't cached: each is read when asked for.
        #[unsafe(method(removeAllCachedResourceValues))]
        fn remove_all_cached_resource_values(&self) {}

        #[unsafe(method(removeCachedResourceValueForKey:))]
        fn remove_cached_resource_value_for_key(&self, _key: &NSString) {}
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
