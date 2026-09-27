//! `CFURL`'s resource properties, reachability, file reference and path
//! URLs, security-scoped access and bookmarks, and its resource keys.
//!
//! Resource properties are `NSURL`'s resource values (`resource_values`):
//! read from the file system, temporary ones set on one URL object, and
//! the few Linux lets a program set (content modification and access
//! dates). The functions return what they copy with a reference, errors
//! included, as CoreFoundation's do.
//!
//! As measured on macOS (`conformance/tests/cf_urls.rs`):
//!
//! - Linux has no file reference URLs: a file URL of an item that is there
//!   is its own reference, and no URL is a reference URL;
//! - no process is sandboxed, so starting to access a file URL succeeds
//!   (as on macOS for an unsandboxed process), and any other URL fails;
//! - bookmarks are Sidestep's own data, `book` then a property list: the
//!   item's path, its device and inode (so a bookmark finds a file renamed
//!   in its directory, and says it is stale), its path relative to a URL,
//!   and resource values kept for reading without the item. A bookmark
//!   file holds the bookmark's bytes; writing one takes a bookmark made
//!   for it (`kCFURLBookmarkCreationSuitableForBookmarkFile`). An alias
//!   record makes a bookmark that never resolves, as macOS makes of data
//!   it can't use.

use std::ffi::c_void;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSString, NSURL};

use super::types::owned;
use crate::error::{self, FileOp, code};

type Boolean = u8;

macro_rules! keys {
    ($($name:ident = $value:literal,)*) => {
        $(crate::constant_string!($name = $value);)*
    };
}

keys! {
    kCFURLAddedToDirectoryDateKey = "NSURLAddedToDirectoryDateKey",
    kCFURLApplicationIsScriptableKey = "NSURLApplicationIsScriptableKey",
    kCFURLAttributeModificationDateKey = "NSURLAttributeModificationDateKey",
    kCFURLCanonicalPathKey = "NSURLCanonicalPathKey",
    kCFURLContentAccessDateKey = "NSURLContentAccessDateKey",
    kCFURLContentModificationDateKey = "NSURLContentModificationDateKey",
    kCFURLCreationDateKey = "NSURLCreationDateKey",
    kCFURLCustomIconKey = "kCFURLCustomIconKey",
    kCFURLDirectoryEntryCountKey = "NSURLDirectoryEntryCountKey",
    kCFURLDocumentIdentifierKey = "NSURLDocumentIdentifierKey",
    kCFURLEffectiveIconKey = "kCFURLEffectiveIconKey",
    kCFURLFileAllocatedSizeKey = "NSURLFileAllocatedSizeKey",
    kCFURLFileContentIdentifierKey = "NSURLFileContentIdentifierKey",
    kCFURLFileIdentifierKey = "NSURLFileIdentifierKey",
    kCFURLFileProtectionComplete = "NSURLFileProtectionComplete",
    kCFURLFileProtectionCompleteUnlessOpen = "NSURLFileProtectionCompleteUnlessOpen",
    kCFURLFileProtectionCompleteUntilFirstUserAuthentication = "NSURLFileProtectionCompleteUntilFirstUserAuthentication",
    kCFURLFileProtectionCompleteWhenUserInactive = "NSURLFileProtectionCompleteWhenUserInactive",
    kCFURLFileProtectionKey = "NSURLFileProtectionKey",
    kCFURLFileProtectionNone = "NSURLFileProtectionNone",
    kCFURLFileResourceIdentifierKey = "NSURLFileResourceIdentifierKey",
    kCFURLFileResourceTypeBlockSpecial = "NSURLFileResourceTypeBlockSpecial",
    kCFURLFileResourceTypeCharacterSpecial = "NSURLFileResourceTypeCharacterSpecial",
    kCFURLFileResourceTypeDirectory = "NSURLFileResourceTypeDirectory",
    kCFURLFileResourceTypeKey = "NSURLFileResourceTypeKey",
    kCFURLFileResourceTypeNamedPipe = "NSURLFileResourceTypeNamedPipe",
    kCFURLFileResourceTypeRegular = "NSURLFileResourceTypeRegular",
    kCFURLFileResourceTypeSocket = "NSURLFileResourceTypeSocket",
    kCFURLFileResourceTypeSymbolicLink = "NSURLFileResourceTypeSymbolicLink",
    kCFURLFileResourceTypeUnknown = "NSURLFileResourceTypeUnknown",
    kCFURLFileSecurityKey = "NSURLFileSecurityKey",
    kCFURLFileSizeKey = "NSURLFileSizeKey",
    kCFURLGenerationIdentifierKey = "NSURLGenerationIdentifierKey",
    kCFURLHasHiddenExtensionKey = "NSURLHasHiddenExtensionKey",
    kCFURLIsAliasFileKey = "NSURLIsAliasFileKey",
    kCFURLIsApplicationKey = "_NSURLIsApplicationKey",
    kCFURLIsDirectoryKey = "NSURLIsDirectoryKey",
    kCFURLIsExcludedFromBackupKey = "NSURLIsExcludedFromBackupKey",
    kCFURLIsExecutableKey = "NSURLIsExecutableKey",
    kCFURLIsHiddenKey = "NSURLIsHiddenKey",
    kCFURLIsMountTriggerKey = "NSURLIsMountTriggerKey",
    kCFURLIsPackageKey = "NSURLIsPackageKey",
    kCFURLIsPurgeableKey = "NSURLIsPurgeableKey",
    kCFURLIsReadableKey = "NSURLIsReadableKey",
    kCFURLIsRegularFileKey = "NSURLIsRegularFileKey",
    kCFURLIsSparseKey = "NSURLIsSparseKey",
    kCFURLIsSymbolicLinkKey = "NSURLIsSymbolicLinkKey",
    kCFURLIsSystemImmutableKey = "NSURLIsSystemImmutableKey",
    kCFURLIsUbiquitousItemKey = "NSURLIsUbiquitousItemKey",
    kCFURLIsUserImmutableKey = "NSURLIsUserImmutableKey",
    kCFURLIsVolumeKey = "NSURLIsVolumeKey",
    kCFURLIsWritableKey = "NSURLIsWritableKey",
    kCFURLKeysOfUnsetValuesKey = "NSURLKeysOfUnsetValuesKey",
    kCFURLLabelColorKey = "kCFURLLabelColorKey",
    kCFURLLabelNumberKey = "NSURLLabelNumberKey",
    kCFURLLinkCountKey = "NSURLLinkCountKey",
    kCFURLLocalizedLabelKey = "NSURLLocalizedLabelKey",
    kCFURLLocalizedNameKey = "NSURLLocalizedNameKey",
    kCFURLLocalizedTypeDescriptionKey = "NSURLLocalizedTypeDescriptionKey",
    kCFURLMayHaveExtendedAttributesKey = "NSURLMayHaveExtendedAttributesKey",
    kCFURLMayShareFileContentKey = "NSURLMayShareFileContentKey",
    kCFURLNameKey = "NSURLNameKey",
    kCFURLParentDirectoryURLKey = "NSURLParentDirectoryURLKey",
    kCFURLPathKey = "_NSURLPathKey",
    kCFURLPreferredIOBlockSizeKey = "NSURLPreferredIOBlockSizeKey",
    kCFURLQuarantinePropertiesKey = "NSURLQuarantinePropertiesKey",
    kCFURLTagNamesKey = "NSURLTagNamesKey",
    kCFURLTotalFileAllocatedSizeKey = "NSURLTotalFileAllocatedSizeKey",
    kCFURLTotalFileSizeKey = "NSURLTotalFileSizeKey",
    kCFURLTypeIdentifierKey = "NSURLTypeIdentifierKey",
    kCFURLUbiquitousItemDownloadingErrorKey = "NSURLUbiquitousItemDownloadingErrorKey",
    kCFURLUbiquitousItemDownloadingStatusCurrent = "NSURLUbiquitousItemDownloadingStatusCurrent",
    kCFURLUbiquitousItemDownloadingStatusDownloaded = "NSURLUbiquitousItemDownloadingStatusDownloaded",
    kCFURLUbiquitousItemDownloadingStatusKey = "NSURLUbiquitousItemDownloadingStatusKey",
    kCFURLUbiquitousItemDownloadingStatusNotDownloaded = "NSURLUbiquitousItemDownloadingStatusNotDownloaded",
    kCFURLUbiquitousItemHasUnresolvedConflictsKey = "NSURLUbiquitousItemHasUnresolvedConflictsKey",
    kCFURLUbiquitousItemIsDownloadedKey = "NSURLUbiquitousItemIsDownloadedKey",
    kCFURLUbiquitousItemIsDownloadingKey = "NSURLUbiquitousItemIsDownloadingKey",
    kCFURLUbiquitousItemIsExcludedFromSyncKey = "NSURLUbiquitousItemIsExcludedFromSyncKey",
    kCFURLUbiquitousItemIsSyncPausedKey = "NSURLUbiquitousItemIsSyncPausedKey",
    kCFURLUbiquitousItemIsUploadedKey = "NSURLUbiquitousItemIsUploadedKey",
    kCFURLUbiquitousItemIsUploadingKey = "NSURLUbiquitousItemIsUploadingKey",
    kCFURLUbiquitousItemPercentDownloadedKey = "NSURLUbiquitousItemPercentDownloadedKey",
    kCFURLUbiquitousItemPercentUploadedKey = "NSURLUbiquitousItemPercentUploadedKey",
    kCFURLUbiquitousItemSupportedSyncControlsKey = "NSURLUbiquitousItemSupportedSyncControlsKey",
    kCFURLUbiquitousItemUploadingErrorKey = "NSURLUbiquitousItemUploadingErrorKey",
    kCFURLVolumeAvailableCapacityForImportantUsageKey = "NSURLVolumeAvailableCapacityForImportantUsageKey",
    kCFURLVolumeAvailableCapacityForOpportunisticUsageKey = "NSURLVolumeAvailableCapacityForOpportunisticUsageKey",
    kCFURLVolumeAvailableCapacityKey = "NSURLVolumeAvailableCapacityKey",
    kCFURLVolumeCreationDateKey = "NSURLVolumeCreationDateKey",
    kCFURLVolumeIdentifierKey = "NSURLVolumeIdentifierKey",
    kCFURLVolumeIsAutomountedKey = "NSURLVolumeIsAutomountedKey",
    kCFURLVolumeIsBrowsableKey = "NSURLVolumeIsBrowsableKey",
    kCFURLVolumeIsEjectableKey = "NSURLVolumeIsEjectableKey",
    kCFURLVolumeIsEncryptedKey = "NSURLVolumeIsEncryptedKey",
    kCFURLVolumeIsInternalKey = "NSURLVolumeIsInternalKey",
    kCFURLVolumeIsJournalingKey = "NSURLVolumeIsJournalingKey",
    kCFURLVolumeIsLocalKey = "NSURLVolumeIsLocalKey",
    kCFURLVolumeIsReadOnlyKey = "NSURLVolumeIsReadOnlyKey",
    kCFURLVolumeIsRemovableKey = "NSURLVolumeIsRemovableKey",
    kCFURLVolumeIsRootFileSystemKey = "NSURLVolumeIsRootFileSystemKey",
    kCFURLVolumeLocalizedFormatDescriptionKey = "NSURLVolumeLocalizedFormatDescriptionKey",
    kCFURLVolumeLocalizedNameKey = "NSURLVolumeLocalizedNameKey",
    kCFURLVolumeMaximumFileSizeKey = "NSURLVolumeMaximumFileSizeKey",
    kCFURLVolumeMountFromLocationKey = "NSURLVolumeMountFromLocationKey",
    kCFURLVolumeNameKey = "NSURLVolumeNameKey",
    kCFURLVolumeResourceCountKey = "NSURLVolumeResourceCountKey",
    kCFURLVolumeSubtypeKey = "NSURLVolumeSubtypeKey",
    kCFURLVolumeSupportsAccessPermissionsKey = "NSURLVolumeSupportsAccessPermissionsKey",
    kCFURLVolumeSupportsAdvisoryFileLockingKey = "NSURLVolumeSupportsAdvisoryFileLockingKey",
    kCFURLVolumeSupportsCasePreservedNamesKey = "NSURLVolumeSupportsCasePreservedNamesKey",
    kCFURLVolumeSupportsCaseSensitiveNamesKey = "NSURLVolumeSupportsCaseSensitiveNamesKey",
    kCFURLVolumeSupportsCompressionKey = "NSURLVolumeSupportsCompressionKey",
    kCFURLVolumeSupportsExclusiveRenamingKey = "NSURLVolumeSupportsExclusiveRenamingKey",
    kCFURLVolumeSupportsExtendedSecurityKey = "NSURLVolumeSupportsExtendedSecurityKey",
    kCFURLVolumeSupportsFileCloningKey = "NSURLVolumeSupportsFileCloningKey",
    kCFURLVolumeSupportsFileProtectionKey = "NSURLVolumeSupportsFileProtectionKey",
    kCFURLVolumeSupportsHardLinksKey = "NSURLVolumeSupportsHardLinksKey",
    kCFURLVolumeSupportsImmutableFilesKey = "NSURLVolumeSupportsImmutableFilesKey",
    kCFURLVolumeSupportsJournalingKey = "NSURLVolumeSupportsJournalingKey",
    kCFURLVolumeSupportsPersistentIDsKey = "NSURLVolumeSupportsPersistentIDsKey",
    kCFURLVolumeSupportsRenamingKey = "NSURLVolumeSupportsRenamingKey",
    kCFURLVolumeSupportsRootDirectoryDatesKey = "NSURLVolumeSupportsRootDirectoryDatesKey",
    kCFURLVolumeSupportsSparseFilesKey = "NSURLVolumeSupportsSparseFilesKey",
    kCFURLVolumeSupportsSwapRenamingKey = "NSURLVolumeSupportsSwapRenamingKey",
    kCFURLVolumeSupportsSymbolicLinksKey = "NSURLVolumeSupportsSymbolicLinksKey",
    kCFURLVolumeSupportsVolumeSizesKey = "NSURLVolumeSupportsVolumeSizesKey",
    kCFURLVolumeSupportsZeroRunsKey = "NSURLVolumeSupportsZeroRunsKey",
    kCFURLVolumeTotalCapacityKey = "NSURLVolumeTotalCapacityKey",
    kCFURLVolumeTypeNameKey = "NSURLVolumeTypeNameKey",
    kCFURLVolumeURLForRemountingKey = "NSURLVolumeURLForRemountingKey",
    kCFURLVolumeURLKey = "NSURLVolumeURLKey",
    kCFURLVolumeUUIDStringKey = "NSURLVolumeUUIDStringKey",
}

/// `NSFileReadCorruptFileError`.
const FILE_READ_CORRUPT: isize = 259;

fn url<'a>(cf: *const c_void) -> &'a NSURL {
    // SAFETY: the callers' contracts: `cf` is a URL.
    unsafe { &*cf.cast::<NSURL>() }
}

fn string<'a>(cf: *const c_void) -> &'a NSString {
    // SAFETY: the callers' contracts: `cf` is a string.
    unsafe { &*cf.cast::<NSString>() }
}

/// Hand an error to the caller, with a reference as CoreFoundation's
/// errors come.
///
/// # Safety
///
/// `out` is null or writable.
unsafe fn give_error(out: *mut *mut c_void, error: Retained<NSError>) {
    if !out.is_null() {
        // SAFETY: per this function's contract.
        unsafe { out.write(Retained::into_raw(error).cast()) };
    }
}

/// # Safety
///
/// `cf` is a URL; `key` null or a string; `value` null or writable;
/// `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyResourcePropertyForKey(
    cf: *const c_void,
    key: *const c_void,
    value: *mut *mut c_void,
    error: *mut *mut c_void,
) -> Boolean {
    if key.is_null() {
        return 0;
    }
    match crate::resource_values::value_of(url(cf), &string(key).to_string()) {
        Ok(found) => {
            if !value.is_null() {
                // SAFETY: per this function's contract; a +1 reference.
                unsafe { value.write(found.map_or(std::ptr::null_mut(), owned)) };
            }
            1
        }
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, e) };
            0
        }
    }
}

/// # Safety
///
/// `cf` is a URL; `keys` null or an array of strings; `error` null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCopyResourcePropertiesForKeys(
    cf: *const c_void,
    keys: *const c_void,
    error: *mut *mut c_void,
) -> *mut c_void {
    let keys: Vec<Retained<NSString>> = if keys.is_null() {
        Vec::new()
    } else {
        // SAFETY: per this function's contract.
        unsafe { &*keys.cast::<NSArray<NSString>>() }.to_vec()
    };
    match crate::resource_values::values_of(url(cf), &keys) {
        Ok(values) => {
            let values = values.unwrap_or_default();
            let keys: Vec<&NSString> = values.iter().map(|(k, _)| &**k).collect();
            let objects: Vec<Retained<AnyObject>> = values.iter().map(|(_, v)| v.clone()).collect();
            owned(NSDictionary::from_retained_objects(&keys, &objects))
        }
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, e) };
            std::ptr::null_mut()
        }
    }
}

/// # Safety
///
/// `cf` is a URL; `key` null or a string; `value` null or an object;
/// `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLSetResourcePropertyForKey(
    cf: *const c_void,
    key: *const c_void,
    value: *const c_void,
    error: *mut *mut c_void,
) -> Boolean {
    if key.is_null() {
        return 0;
    }
    // SAFETY: per this function's contract.
    let value = unsafe { value.cast::<AnyObject>().as_ref() };
    match crate::resource_values::set_value(url(cf), &string(key).to_string(), value) {
        Ok(()) => 1,
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, e) };
            0
        }
    }
}

/// # Safety
///
/// `cf` is a URL; `values` null or a dictionary keyed by strings; `error`
/// null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLSetResourcePropertiesForKeys(
    cf: *const c_void,
    values: *const c_void,
    error: *mut *mut c_void,
) -> Boolean {
    if values.is_null() {
        return 1;
    }
    // SAFETY: per this function's contract.
    for (key, value) in crate::plist::dictionary_entries(unsafe { &*values.cast::<NSDictionary>() }) {
        // SAFETY: the keys are strings.
        let key = unsafe { &*Retained::as_ptr(&key).cast::<NSString>() }.to_string();
        if let Err(e) = crate::resource_values::set_value(url(cf), &key, Some(&value)) {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, e) };
            return 0;
        }
    }
    1
}

/// # Safety
///
/// `cf` is a URL; `key` null or a string; `value` null or an object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLSetTemporaryResourcePropertyForKey(
    cf: *const c_void,
    key: *const c_void,
    value: *const c_void,
) {
    if !key.is_null() {
        // SAFETY: per this function's contract.
        let value = unsafe { value.cast::<AnyObject>().as_ref() };
        crate::resource_values::set_temporary(url(cf), &string(key).to_string(), value);
    }
}

/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLClearResourcePropertyCache(cf: *const c_void) {
    crate::resource_values::clear_temporary(url(cf), None);
}

/// # Safety
///
/// `cf` is a URL; `key` null or a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLClearResourcePropertyCacheForKey(cf: *const c_void, key: *const c_void) {
    if !key.is_null() {
        crate::resource_values::clear_temporary(url(cf), Some(&string(key).to_string()));
    }
}

/// # Safety
///
/// `cf` is a URL; `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLResourceIsReachable(cf: *const c_void, error: *mut *mut c_void) -> Boolean {
    match crate::url::url_impl(url(cf)).reachable() {
        Ok(()) => 1,
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, e) };
            0
        }
    }
}

/// False: Linux has no file reference URLs.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFURLIsFileReferenceURL(_cf: *const c_void) -> Boolean {
    0
}

/// The error for a URL that isn't a file's.
fn not_a_file() -> Retained<NSError> {
    error::cocoa(code::FILE_READ_UNSUPPORTED_SCHEME, &[])
}

/// A file URL's absolute form, with a reference.
fn absolute_file_url(u: &NSURL) -> *mut c_void {
    u.absoluteURL().map_or(std::ptr::null_mut(), owned)
}

/// The URL that refers to a file URL's item: the URL itself, since Linux
/// has no file references, once the item is there.
///
/// # Safety
///
/// `cf` is null or a URL; `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateFileReferenceURL(
    _alloc: *const c_void,
    cf: *const c_void,
    error: *mut *mut c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    let u = crate::url::url_impl(url(cf));
    let result = if u.is_file() { u.reachable() } else { Err(not_a_file()) };
    match result {
        Ok(()) => absolute_file_url(url(cf)),
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, e) };
            std::ptr::null_mut()
        }
    }
}

/// A file URL with a path for a file URL, whether or not its item is
/// there.
///
/// # Safety
///
/// `cf` is null or a URL; `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateFilePathURL(
    _alloc: *const c_void,
    cf: *const c_void,
    error: *mut *mut c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    if !crate::url::url_impl(url(cf)).is_file() {
        // SAFETY: per this function's contract.
        unsafe { give_error(error, not_a_file()) };
        return std::ptr::null_mut();
    }
    absolute_file_url(url(cf))
}

/// Whether access to a URL's item starts: yes for a file URL, as for an
/// unsandboxed process on macOS (no process is sandboxed here), no for
/// any other.
///
/// # Safety
///
/// `cf` is a URL.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLStartAccessingSecurityScopedResource(cf: *const c_void) -> Boolean {
    u8::from(crate::url::url_impl(url(cf)).is_file())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CFURLStopAccessingSecurityScopedResource(_cf: *const c_void) {}

// Bookmarks.

const MAGIC: &[u8] = b"book";
/// `kCFURLBookmarkCreationSuitableForBookmarkFile`.
const SUITABLE_FOR_FILE: usize = 1 << 10;

/// Resource values a bookmark keeps whatever it's asked for.
const KEPT: [&str; 4] = ["NSURLNameKey", "NSURLIsDirectoryKey", "NSURLIsRegularFileKey", "NSURLIsSymbolicLinkKey"];

fn bookmark_bytes(fields: plist::Dictionary) -> Option<Retained<NSData>> {
    let body = crate::plist::write_binary(&plist::Value::Dictionary(fields))?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&body);
    Some(NSData::with_bytes(&bytes))
}

/// A bookmark's fields, if the data is a bookmark.
fn bookmark_fields(data: *const c_void) -> Option<plist::Dictionary> {
    if data.is_null() {
        return None;
    }
    // SAFETY: the callers' contracts: `data` is a data object, unchanged
    // while read.
    let bytes = unsafe { crate::data::bytes(&*data.cast::<NSData>()) };
    let (value, _) = crate::plist::parse(bytes.strip_prefix(MAGIC)?)?;
    value.into_dictionary()
}

/// A path's absolute form, its directory's symbolic links resolved but not
/// the item's own.
fn absolute(path: &Path) -> PathBuf {
    match (path.parent().and_then(|p| std::fs::canonicalize(p).ok()), path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
    }
}

/// # Safety
///
/// `cf` is null or a URL; `properties` null or an array of strings;
/// `relative_to` null or a URL; `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateBookmarkData(
    _alloc: *const c_void,
    cf: *const c_void,
    options: usize,
    properties: *const c_void,
    relative_to: *const c_void,
    error: *mut *mut c_void,
) -> *mut c_void {
    if cf.is_null() {
        return std::ptr::null_mut();
    }
    let u = url(cf);
    let mut fields = plist::Dictionary::new();
    if options & SUITABLE_FOR_FILE != 0 {
        fields.insert("file".into(), true.into());
    }
    let Some(path) = crate::url::file_path(u) else {
        // A URL that isn't a file's is kept as it is.
        fields.insert("url".into(), u.absoluteString().map(|s| s.to_string()).unwrap_or_default().into());
        return bookmark_bytes(fields).map_or(std::ptr::null_mut(), owned);
    };
    let path = absolute(&path);
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        // SAFETY: per this function's contract.
        unsafe { give_error(error, error::cocoa(code::FILE_READ_NO_SUCH_FILE, &[])) };
        return std::ptr::null_mut();
    };
    fields.insert("path".into(), path.to_string_lossy().into_owned().into());
    fields.insert("device".into(), meta.dev().into());
    fields.insert("inode".into(), meta.ino().into());
    if !relative_to.is_null()
        && let Some(base) = crate::url::file_path(url(relative_to))
        && let Ok(rest) = path.strip_prefix(absolute(&base))
    {
        fields.insert("relative".into(), rest.to_string_lossy().into_owned().into());
    }
    let mut keys: Vec<String> = KEPT.iter().map(|k| k.to_string()).collect();
    if !properties.is_null() {
        // SAFETY: per this function's contract.
        keys.extend(unsafe { &*properties.cast::<NSArray<NSString>>() }.iter().map(|k| k.to_string()));
    }
    let mut kept = plist::Dictionary::new();
    for key in keys {
        if let Ok(Some(value)) = crate::resource_values::value_of(u, &key)
            && let Some(value) = crate::plist::from_object(&value)
        {
            kept.insert(key, value);
        }
    }
    fields.insert("properties".into(), kept.into());
    bookmark_bytes(fields).map_or(std::ptr::null_mut(), owned)
}

/// The item a bookmark's path, device and inode name: at the path, or
/// renamed within its directory (stale); or, failing both, whatever is at
/// the path now (stale).
fn find(fields: &plist::Dictionary) -> Option<(PathBuf, bool)> {
    let path = PathBuf::from(fields.get("path")?.as_string()?);
    let id = (fields.get("device")?.as_unsigned_integer()?, fields.get("inode")?.as_unsigned_integer()?);
    let same = |meta: &std::fs::Metadata| (meta.dev(), meta.ino()) == id;
    let here = std::fs::symlink_metadata(&path).ok();
    if here.as_ref().is_some_and(same) {
        return Some((path, false));
    }
    let moved = path.parent().and_then(|dir| {
        std::fs::read_dir(dir).ok()?.flatten().find(|e| std::fs::symlink_metadata(e.path()).is_ok_and(|m| same(&m)))
    });
    match (moved, here) {
        (Some(entry), _) => Some((entry.path(), true)),
        (None, Some(_)) => Some((path, true)),
        (None, None) => None,
    }
}

/// # Safety
///
/// `data` is null or a data object; `relative_to` null or a URL;
/// `is_stale` and `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateByResolvingBookmarkData(
    _alloc: *const c_void,
    data: *const c_void,
    _options: usize,
    relative_to: *const c_void,
    _properties: *const c_void,
    is_stale: *mut Boolean,
    error: *mut *mut c_void,
) -> *mut c_void {
    let fail = |code: isize| {
        // SAFETY: per this function's contract.
        unsafe { give_error(error, error::cocoa(code, &[])) };
        std::ptr::null_mut()
    };
    let Some(fields) = bookmark_fields(data) else { return fail(FILE_READ_CORRUPT) };
    if fields.contains_key("alias") {
        return fail(code::FILE_NO_SUCH_FILE);
    }
    let mut stale = false;
    let found = if let Some(text) = fields.get("url").and_then(|v| v.as_string()) {
        crate::url::make(text.to_string(), None).map(crate::url::as_url)
    } else {
        let relative = fields.get("relative").and_then(|v| v.as_string()).and_then(|rest| {
            if relative_to.is_null() {
                return None;
            }
            let path = crate::url::file_path(url(relative_to))?.join(rest);
            path.exists().then_some(path)
        });
        match relative {
            Some(path) => crate::url::file_url(&path),
            None => match find(&fields) {
                Some((path, moved)) => {
                    stale = moved;
                    crate::url::file_url(&std::fs::canonicalize(&path).unwrap_or(path))
                }
                None => return fail(code::FILE_NO_SUCH_FILE),
            },
        }
    };
    let Some(found) = found else { return fail(code::FILE_NO_SUCH_FILE) };
    if !is_stale.is_null() {
        // SAFETY: per this function's contract.
        unsafe { is_stale.write(u8::from(stale)) };
    }
    owned(found)
}

/// # Safety
///
/// `key` null or a string; `data` null or a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateResourcePropertyForKeyFromBookmarkData(
    _alloc: *const c_void,
    key: *const c_void,
    data: *const c_void,
) -> *mut c_void {
    let Some(fields) = bookmark_fields(data) else { return std::ptr::null_mut() };
    if key.is_null() {
        return std::ptr::null_mut();
    }
    let kept = fields.get("properties").and_then(|p| p.as_dictionary());
    kept.and_then(|p| p.get(&string(key).to_string()))
        .and_then(crate::plist::to_object)
        .map_or(std::ptr::null_mut(), owned)
}

/// # Safety
///
/// `keys` null or an array of strings; `data` null or a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateResourcePropertiesForKeysFromBookmarkData(
    _alloc: *const c_void,
    keys: *const c_void,
    data: *const c_void,
) -> *mut c_void {
    let Some(fields) = bookmark_fields(data) else { return std::ptr::null_mut() };
    let kept = fields.get("properties").and_then(|p| p.as_dictionary()).cloned().unwrap_or_default();
    let wanted: Vec<Retained<NSString>> = if keys.is_null() {
        Vec::new()
    } else {
        // SAFETY: per this function's contract.
        unsafe { &*keys.cast::<NSArray<NSString>>() }.to_vec()
    };
    let mut found_keys = Vec::new();
    let mut found_values = Vec::new();
    for key in &wanted {
        if let Some(value) = kept.get(&key.to_string()).and_then(crate::plist::to_object) {
            found_keys.push(&**key);
            found_values.push(value);
        }
    }
    owned(NSDictionary::from_retained_objects(&found_keys, &found_values))
}

/// # Safety
///
/// `data` null or a data object; `file` null or a URL; `error` null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLWriteBookmarkDataToFile(
    data: *const c_void,
    file: *const c_void,
    _options: usize,
    error: *mut *mut c_void,
) -> Boolean {
    let suitable = bookmark_fields(data).is_some_and(|f| f.get("file").and_then(|v| v.as_boolean()) == Some(true));
    let path = (!file.is_null()).then(|| crate::url::file_path(url(file))).flatten();
    let (true, Some(path)) = (suitable, path) else {
        // SAFETY: per this function's contract.
        unsafe { give_error(error, error::cocoa(code::FILE_WRITE_UNKNOWN, &[])) };
        return 0;
    };
    // SAFETY: a data object, unchanged while written out.
    let bytes = unsafe { crate::data::bytes(&*data.cast::<NSData>()) };
    match std::fs::write(&path, bytes) {
        Ok(()) => 1,
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, error::file(FileOp::Write, &e, &path.to_string_lossy())) };
            0
        }
    }
}

/// # Safety
///
/// `file` null or a URL; `error` null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateBookmarkDataFromFile(
    _alloc: *const c_void,
    file: *const c_void,
    error: *mut *mut c_void,
) -> *mut c_void {
    let Some(path) = (!file.is_null()).then(|| crate::url::file_path(url(file))).flatten() else {
        // SAFETY: per this function's contract.
        unsafe { give_error(error, not_a_file()) };
        return std::ptr::null_mut();
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { give_error(error, error::file(FileOp::Read, &e, &path.to_string_lossy())) };
            return std::ptr::null_mut();
        }
    };
    let data = NSData::with_bytes(&bytes);
    if bookmark_fields(Retained::as_ptr(&data).cast()).is_none() {
        // SAFETY: per this function's contract.
        unsafe { give_error(error, error::cocoa(code::FILE_READ_UNKNOWN, &[])) };
        return std::ptr::null_mut();
    }
    owned(data)
}

/// A bookmark holding an alias record, which resolves to nothing: Linux
/// has no aliases.
///
/// # Safety
///
/// `data` null or a data object.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CFURLCreateBookmarkDataFromAliasRecord(
    _alloc: *const c_void,
    data: *const c_void,
) -> *mut c_void {
    if data.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: per this function's contract.
    let alias = unsafe { crate::data::bytes(&*data.cast::<NSData>()) }.to_vec();
    let mut fields = plist::Dictionary::new();
    fields.insert("alias".into(), plist::Value::Data(alias));
    bookmark_bytes(fields).map_or(std::ptr::null_mut(), owned)
}
