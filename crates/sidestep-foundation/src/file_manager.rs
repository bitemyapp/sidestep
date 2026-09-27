//! `NSFileManager` over `std::fs`, with the attribute and resource-key
//! constants.
//!
//! Failures report the Cocoa codes macOS uses for each operation (see
//! `error`): a missing source is 260 when copying but 4 when moving or
//! removing, an existing destination is 516, and so on, always with the
//! POSIX error underneath. Moving and copying refuse to replace an
//! existing item, as on macOS, where `rename(2)` alone would. Copies keep
//! symbolic links as links and recurse into directories.
//!
//! Search-path directories come from `xdg`. `trashItemAtURL:` follows the
//! freedesktop.org trash specification: the home trash for items on the
//! home file system, `$topdir/.Trash-$uid` for others, with a
//! `.trashinfo` record beside each item.

use std::ffi::{CString, c_char};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{ClassType, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSData, NSError, NSString, NSUInteger, NSURL};

use crate::error::{self, FileOp};
use crate::runloop::modes::exported_strings;
use crate::xdg;

sidestep_runtime::static_class!(pub(crate) NSFILEMANAGER, NSFILEMANAGER_META = "NSFileManager", || {
    let _ = NSFileManagerImpl::class();
    crate::perform::install();
});

exported_strings! {
    NSFileType, pub(crate) FILE_TYPE = "NSFileType";
    NSFileTypeDirectory, pub(crate) TYPE_DIRECTORY = "NSFileTypeDirectory";
    NSFileTypeRegular, pub(crate) TYPE_REGULAR = "NSFileTypeRegular";
    NSFileTypeSymbolicLink, pub(crate) TYPE_SYMBOLIC_LINK = "NSFileTypeSymbolicLink";
    NSFileTypeSocket, pub(crate) TYPE_SOCKET = "NSFileTypeSocket";
    NSFileTypeCharacterSpecial, pub(crate) TYPE_CHARACTER_SPECIAL = "NSFileTypeCharacterSpecial";
    NSFileTypeBlockSpecial, pub(crate) TYPE_BLOCK_SPECIAL = "NSFileTypeBlockSpecial";
    NSFileTypeUnknown, pub(crate) TYPE_UNKNOWN = "NSFileTypeUnknown";
    NSFileSize, pub(crate) FILE_SIZE = "NSFileSize";
    NSFileModificationDate, pub(crate) MODIFICATION_DATE = "NSFileModificationDate";
    NSFileReferenceCount, pub(crate) REFERENCE_COUNT = "NSFileReferenceCount";
    NSFileDeviceIdentifier, pub(crate) DEVICE_IDENTIFIER = "NSFileDeviceIdentifier";
    NSFileOwnerAccountName, pub(crate) OWNER_ACCOUNT_NAME = "NSFileOwnerAccountName";
    NSFileGroupOwnerAccountName, pub(crate) GROUP_OWNER_ACCOUNT_NAME = "NSFileGroupOwnerAccountName";
    NSFilePosixPermissions, pub(crate) POSIX_PERMISSIONS = "NSFilePosixPermissions";
    NSFileSystemNumber, pub(crate) SYSTEM_NUMBER = "NSFileSystemNumber";
    NSFileSystemFileNumber, pub(crate) SYSTEM_FILE_NUMBER = "NSFileSystemFileNumber";
    NSFileExtensionHidden, pub(crate) EXTENSION_HIDDEN = "NSFileExtensionHidden";
    NSFileHFSCreatorCode, HFS_CREATOR_CODE = "NSFileHFSCreatorCode";
    NSFileHFSTypeCode, HFS_TYPE_CODE = "NSFileHFSTypeCode";
    NSFileImmutable, pub(crate) IMMUTABLE = "NSFileImmutable";
    NSFileAppendOnly, pub(crate) APPEND_ONLY = "NSFileAppendOnly";
    NSFileCreationDate, pub(crate) CREATION_DATE = "NSFileCreationDate";
    NSFileOwnerAccountID, pub(crate) OWNER_ACCOUNT_ID = "NSFileOwnerAccountID";
    NSFileGroupOwnerAccountID, pub(crate) GROUP_OWNER_ACCOUNT_ID = "NSFileGroupOwnerAccountID";
    NSFileBusy, BUSY = "NSFileBusy";
    NSFileProtectionKey, PROTECTION_KEY = "NSFileProtectionKey";
    NSFileProtectionNone, PROTECTION_NONE = "NSFileProtectionNone";
    NSFileProtectionComplete, PROTECTION_COMPLETE = "NSFileProtectionComplete";
    NSFileProtectionCompleteUnlessOpen, PROTECTION_UNLESS_OPEN = "NSFileProtectionCompleteUnlessOpen";
    NSFileProtectionCompleteUntilFirstUserAuthentication, PROTECTION_UNTIL_AUTHENTICATION = "NSFileProtectionCompleteUntilFirstUserAuthentication";
    NSFileProtectionCompleteWhenUserInactive, PROTECTION_WHEN_INACTIVE = "NSFileProtectionCompleteWhenUserInactive";
    NSFileSystemSize, pub(crate) SYSTEM_SIZE = "NSFileSystemSize";
    NSFileSystemFreeSize, pub(crate) SYSTEM_FREE_SIZE = "NSFileSystemFreeSize";
    NSFileSystemNodes, pub(crate) SYSTEM_NODES = "NSFileSystemNodes";
    NSFileSystemFreeNodes, pub(crate) SYSTEM_FREE_NODES = "NSFileSystemFreeNodes";

    NSURLNameKey, pub(crate) URL_NAME = "NSURLNameKey";
    NSURLLocalizedNameKey, pub(crate) URL_LOCALIZED_NAME = "NSURLLocalizedNameKey";
    NSURLIsRegularFileKey, pub(crate) URL_IS_REGULAR_FILE = "NSURLIsRegularFileKey";
    NSURLIsDirectoryKey, pub(crate) URL_IS_DIRECTORY = "NSURLIsDirectoryKey";
    NSURLIsSymbolicLinkKey, pub(crate) URL_IS_SYMBOLIC_LINK = "NSURLIsSymbolicLinkKey";
    NSURLIsVolumeKey, pub(crate) URL_IS_VOLUME = "NSURLIsVolumeKey";
    NSURLIsPackageKey, pub(crate) URL_IS_PACKAGE = "NSURLIsPackageKey";
    NSURLIsHiddenKey, pub(crate) URL_IS_HIDDEN = "NSURLIsHiddenKey";
    NSURLCreationDateKey, pub(crate) URL_CREATION_DATE = "NSURLCreationDateKey";
    NSURLContentAccessDateKey, pub(crate) URL_CONTENT_ACCESS_DATE = "NSURLContentAccessDateKey";
    NSURLContentModificationDateKey, pub(crate) URL_CONTENT_MODIFICATION_DATE = "NSURLContentModificationDateKey";
    NSURLAttributeModificationDateKey, pub(crate) URL_ATTRIBUTE_MODIFICATION_DATE = "NSURLAttributeModificationDateKey";
    NSURLLinkCountKey, pub(crate) URL_LINK_COUNT = "NSURLLinkCountKey";
    NSURLParentDirectoryURLKey, pub(crate) URL_PARENT_DIRECTORY = "NSURLParentDirectoryURLKey";
    NSURLFileSizeKey, pub(crate) URL_FILE_SIZE = "NSURLFileSizeKey";
    NSURLFileAllocatedSizeKey, pub(crate) URL_FILE_ALLOCATED_SIZE = "NSURLFileAllocatedSizeKey";
    NSURLTotalFileSizeKey, pub(crate) URL_TOTAL_FILE_SIZE = "NSURLTotalFileSizeKey";
    NSURLIsReadableKey, pub(crate) URL_IS_READABLE = "NSURLIsReadableKey";
    NSURLIsWritableKey, pub(crate) URL_IS_WRITABLE = "NSURLIsWritableKey";
    NSURLIsExecutableKey, pub(crate) URL_IS_EXECUTABLE = "NSURLIsExecutableKey";
    NSURLPathKey, pub(crate) URL_PATH = "_NSURLPathKey";
    NSURLCanonicalPathKey, pub(crate) URL_CANONICAL_PATH = "NSURLCanonicalPathKey";
    NSURLFileResourceTypeKey, pub(crate) URL_FILE_RESOURCE_TYPE = "NSURLFileResourceTypeKey";
    NSURLFileResourceTypeDirectory, pub(crate) URL_RESOURCE_DIRECTORY = "NSURLFileResourceTypeDirectory";
    NSURLFileResourceTypeRegular, pub(crate) URL_RESOURCE_REGULAR = "NSURLFileResourceTypeRegular";
    NSURLFileResourceTypeSymbolicLink, pub(crate) URL_RESOURCE_SYMBOLIC_LINK = "NSURLFileResourceTypeSymbolicLink";
    NSURLFileResourceTypeNamedPipe, pub(crate) URL_RESOURCE_NAMED_PIPE = "NSURLFileResourceTypeNamedPipe";
    NSURLFileResourceTypeCharacterSpecial, pub(crate) URL_RESOURCE_CHARACTER_SPECIAL = "NSURLFileResourceTypeCharacterSpecial";
    NSURLFileResourceTypeBlockSpecial, pub(crate) URL_RESOURCE_BLOCK_SPECIAL = "NSURLFileResourceTypeBlockSpecial";
    NSURLFileResourceTypeSocket, pub(crate) URL_RESOURCE_SOCKET = "NSURLFileResourceTypeSocket";
    NSURLFileResourceTypeUnknown, pub(crate) URL_RESOURCE_UNKNOWN = "NSURLFileResourceTypeUnknown";
    NSURLTypeIdentifierKey, pub(crate) URL_TYPE_IDENTIFIER = "NSURLTypeIdentifierKey";
    NSURLContentTypeKey, pub(crate) URL_CONTENT_TYPE = "NSURLContentTypeKey";
    NSURLVolumeURLKey, pub(crate) URL_VOLUME_URL = "NSURLVolumeURLKey";
    NSURLIsAliasFileKey, pub(crate) URL_IS_ALIAS_FILE = "NSURLIsAliasFileKey";
    NSURLHasHiddenExtensionKey, pub(crate) URL_HAS_HIDDEN_EXTENSION = "NSURLHasHiddenExtensionKey";
    NSURLFileResourceIdentifierKey, pub(crate) URL_FILE_RESOURCE_IDENTIFIER = "NSURLFileResourceIdentifierKey";
    NSURLPreferredIOBlockSizeKey, pub(crate) URL_PREFERRED_IO_BLOCK_SIZE = "NSURLPreferredIOBlockSizeKey";
}

/// `NSDirectoryEnumerationSkipsHiddenFiles`.
const SKIPS_HIDDEN_FILES: NSUInteger = 1 << 2;
/// `NSDirectoryEnumerationProducesRelativePathURLs`.
const PRODUCES_RELATIVE_PATH_URLS: NSUInteger = 1 << 4;

#[derive(Default)]
pub(crate) struct ManagerIvars {
    delegate: Mutex<Option<Weak<AnyObject>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "NSFileManager"]
    #[ivars = ManagerIvars]
    pub(crate) struct NSFileManagerImpl;

    impl NSFileManagerImpl {
        #[unsafe(method_id(defaultManager))]
        fn default_manager() -> Retained<Self> {
            default_manager()
        }

        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(ManagerIvars::default());
            // SAFETY: NSObject's designated initializer.
            unsafe { msg_send![super(this), init] }
        }

        #[unsafe(method_id(delegate))]
        fn delegate(&self) -> Option<Retained<AnyObject>> {
            crate::thread::lock(&self.ivars().delegate).as_ref().and_then(Weak::load)
        }

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>) {
            *crate::thread::lock(&self.ivars().delegate) = delegate.map(Weak::new);
        }

        #[unsafe(method(fileExistsAtPath:))]
        fn file_exists(&self, path: &NSString) -> bool {
            std::fs::metadata(path.to_string()).is_ok()
        }

        #[unsafe(method(fileExistsAtPath:isDirectory:))]
        fn file_exists_is_directory(&self, path: &NSString, is_directory: *mut Bool) -> bool {
            let metadata = std::fs::metadata(path.to_string());
            if let (Ok(metadata), false) = (&metadata, is_directory.is_null()) {
                // SAFETY: the caller passes null or somewhere to write.
                unsafe { is_directory.write(Bool::new(metadata.is_dir())) };
            }
            metadata.is_ok()
        }

        #[unsafe(method(isReadableFileAtPath:))]
        fn is_readable(&self, path: &NSString) -> bool {
            access(&path.to_string(), libc::R_OK)
        }

        #[unsafe(method(isWritableFileAtPath:))]
        fn is_writable(&self, path: &NSString) -> bool {
            access(&path.to_string(), libc::W_OK)
        }

        #[unsafe(method(isExecutableFileAtPath:))]
        fn is_executable(&self, path: &NSString) -> bool {
            let path = path.to_string();
            access(&path, libc::X_OK) && std::fs::metadata(&path).is_ok_and(|m| m.is_file())
        }

        #[unsafe(method(isDeletableFileAtPath:))]
        fn is_deletable(&self, path: &NSString) -> bool {
            // Deleting takes write access to the directory holding the item.
            let path = PathBuf::from(path.to_string());
            let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            access(&parent.to_string_lossy(), libc::W_OK)
        }

        #[unsafe(method(createDirectoryAtPath:withIntermediateDirectories:attributes:error:))]
        fn create_directory_at_path(
            &self,
            path: &NSString,
            intermediates: bool,
            attributes: Option<&AnyObject>,
            error: *mut *mut NSError,
        ) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(create_directory(&path.to_string(), intermediates, attributes), error).is_some() }
        }

        #[unsafe(method(createDirectoryAtURL:withIntermediateDirectories:attributes:error:))]
        fn create_directory_at_url(
            &self,
            url: &NSURL,
            intermediates: bool,
            attributes: Option<&AnyObject>,
            error: *mut *mut NSError,
        ) -> bool {
            let result = write_path(url).and_then(|p| create_directory(&p, intermediates, attributes));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error).is_some() }
        }

        #[unsafe(method(createFileAtPath:contents:attributes:))]
        fn create_file(&self, path: &NSString, contents: Option<&NSData>, attributes: Option<&AnyObject>) -> bool {
            let path = path.to_string();
            // SAFETY: nothing mutates the contents while they are written.
            let bytes = contents.map_or(&[][..], |data| unsafe { crate::data::bytes(data) });
            std::fs::write(&path, bytes).is_ok() && apply_attributes(&path, attributes).is_ok()
        }

        #[unsafe(method(setAttributes:ofItemAtPath:error:))]
        fn set_attributes(&self, attributes: &AnyObject, path: &NSString, error: *mut *mut NSError) -> bool {
            let path = path.to_string();
            let result = apply_attributes(&path, Some(attributes)).map_err(|e| error::file(FileOp::Write, &e, &path));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error).is_some() }
        }

        #[unsafe(method(removeItemAtPath:error:))]
        fn remove_item_at_path(&self, path: &NSString, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(remove(&path.to_string()), error).is_some() }
        }

        #[unsafe(method(removeItemAtURL:error:))]
        fn remove_item_at_url(&self, url: &NSURL, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(write_path(url).and_then(|p| remove(&p)), error).is_some() }
        }

        #[unsafe(method(copyItemAtPath:toPath:error:))]
        fn copy_item_at_path(&self, from: &NSString, to: &NSString, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(copy(&from.to_string(), &to.to_string()), error).is_some() }
        }

        #[unsafe(method(copyItemAtURL:toURL:error:))]
        fn copy_item_at_url(&self, from: &NSURL, to: &NSURL, error: *mut *mut NSError) -> bool {
            let result = read_path(from).and_then(|f| write_path(to).and_then(|t| copy(&f, &t)));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error).is_some() }
        }

        #[unsafe(method(moveItemAtPath:toPath:error:))]
        fn move_item_at_path(&self, from: &NSString, to: &NSString, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(move_item(&from.to_string(), &to.to_string()), error).is_some() }
        }

        #[unsafe(method(moveItemAtURL:toURL:error:))]
        fn move_item_at_url(&self, from: &NSURL, to: &NSURL, error: *mut *mut NSError) -> bool {
            let result = write_path(from).and_then(|f| write_path(to).and_then(|t| move_item(&f, &t)));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error).is_some() }
        }

        #[unsafe(method(linkItemAtPath:toPath:error:))]
        fn link_item_at_path(&self, from: &NSString, to: &NSString, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(link(&from.to_string(), &to.to_string()), error).is_some() }
        }

        #[unsafe(method(linkItemAtURL:toURL:error:))]
        fn link_item_at_url(&self, from: &NSURL, to: &NSURL, error: *mut *mut NSError) -> bool {
            let result = write_path(from).and_then(|f| write_path(to).and_then(|t| link(&f, &t)));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error).is_some() }
        }

        #[unsafe(method(createSymbolicLinkAtPath:withDestinationPath:error:))]
        fn create_symbolic_link_at_path(&self, path: &NSString, destination: &NSString, error: *mut *mut NSError) -> bool {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(symlink(&path.to_string(), &destination.to_string()), error).is_some() }
        }

        #[unsafe(method(createSymbolicLinkAtURL:withDestinationURL:error:))]
        fn create_symbolic_link_at_url(&self, url: &NSURL, destination: &NSURL, error: *mut *mut NSError) -> bool {
            // A relative destination URL stays relative in the link.
            let target = crate::url::url_impl(destination);
            let target = if target.has_base() { target.relative_path_text() } else { target.path_text() };
            let result = write_path(url).and_then(|p| symlink(&p, &target.unwrap_or_default()));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error).is_some() }
        }

        #[unsafe(method_id(destinationOfSymbolicLinkAtPath:error:))]
        fn destination_of_symbolic_link(&self, path: &NSString, error: *mut *mut NSError) -> Option<Retained<NSString>> {
            let path = path.to_string();
            let result = std::fs::read_link(&path)
                .map(|t| NSString::from_str(&t.to_string_lossy()))
                .map_err(|e| error::file(FileOp::Read, &e, &path));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error) }
        }

        #[unsafe(method_id(contentsAtPath:))]
        fn contents_at_path(&self, path: &NSString) -> Option<Retained<NSData>> {
            std::fs::read(path.to_string()).ok().map(NSData::from_vec)
        }

        #[unsafe(method(contentsEqualAtPath:andPath:))]
        fn contents_equal(&self, a: &NSString, b: &NSString) -> bool {
            contents_equal(Path::new(&a.to_string()), Path::new(&b.to_string()))
        }

        #[unsafe(method_id(currentDirectoryPath))]
        fn current_directory_path(&self) -> Retained<NSString> {
            let cwd = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
            NSString::from_str(&cwd)
        }

        #[unsafe(method(changeCurrentDirectoryPath:))]
        fn change_current_directory_path(&self, path: &NSString) -> bool {
            std::env::set_current_dir(path.to_string()).is_ok()
        }

        #[unsafe(method_id(homeDirectoryForCurrentUser))]
        fn home_directory_for_current_user(&self) -> Retained<NSURL> {
            directory_url(&xdg::home())
        }

        #[unsafe(method_id(homeDirectoryForUser:))]
        fn home_directory_for_user(&self, user: &NSString) -> Option<Retained<NSURL>> {
            xdg::passwd_by_name(&user.to_string()).map(|entry| directory_url(&entry.home))
        }

        #[unsafe(method_id(temporaryDirectory))]
        fn temporary_directory(&self) -> Retained<NSURL> {
            directory_url(&xdg::temporary())
        }

        #[unsafe(method_id(URLForDirectory:inDomain:appropriateForURL:create:error:))]
        fn url_for_directory(
            &self,
            directory: NSUInteger,
            domain: NSUInteger,
            url: Option<&NSURL>,
            create: bool,
            error: *mut *mut NSError,
        ) -> Option<Retained<NSURL>> {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(url_for_directory(directory, domain, url, create), error) }
        }

        #[unsafe(method(trashItemAtURL:resultingItemURL:error:))]
        fn trash_item(&self, url: &NSURL, resulting: *mut *mut NSURL, error: *mut *mut NSError) -> bool {
            let result = write_path(url).and_then(|path| {
                trash(Path::new(&path)).map_err(|e| error::file(FileOp::Write, &e, &path))
            });
            let done = result.is_ok();
            // SAFETY: the caller passes null or room for an error.
            // SAFETY: the caller passes null or room for an error.
            if let Some(trashed) = unsafe { report(result, error) }
                && !resulting.is_null()
            {
                // SAFETY: the caller passes null or room for a URL.
                unsafe { resulting.write(Retained::autorelease_ptr(directory_or_file_url(&trashed))) };
            }
            done
        }

        #[unsafe(method_id(displayNameAtPath:))]
        fn display_name(&self, path: &NSString) -> Retained<NSString> {
            let path = path.to_string();
            let trimmed = path.trim_end_matches('/');
            let name = trimmed.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(if path.is_empty() { "" } else { "/" });
            NSString::from_str(name)
        }

        #[unsafe(method(fileSystemRepresentationWithPath:))]
        fn file_system_representation(&self, path: &NSString) -> NonNull<c_char> {
            // An autoreleased buffer that lives as long as the caller's pool.
            let mut bytes = path.to_string().into_bytes();
            bytes.retain(|&b| b != 0);
            bytes.push(0);
            let data = Retained::autorelease_ptr(NSData::from_vec(bytes));
            // SAFETY: just made, immutable, and kept alive by the autorelease
            // pool.
            let bytes = unsafe { crate::data::bytes(&*data) };
            NonNull::new(bytes.as_ptr().cast_mut().cast()).expect("a non-empty buffer")
        }

        #[unsafe(method_id(stringWithFileSystemRepresentation:length:))]
        fn string_with_file_system_representation(
            &self,
            text: NonNull<c_char>,
            length: NSUInteger,
        ) -> Option<Retained<NSString>> {
            // SAFETY: the caller passes `length` readable bytes.
            let bytes = unsafe { std::slice::from_raw_parts(text.as_ptr().cast::<u8>(), length) };
            // Bytes that aren't UTF-8 make no string, as on macOS.
            std::str::from_utf8(bytes).ok().map(NSString::from_str)
        }

        #[unsafe(method_id(contentsOfDirectoryAtPath:error:))]
        fn contents_of_directory_at_path(&self, path: &NSString, error: *mut *mut NSError) -> Option<Retained<AnyObject>> {
            let path = path.to_string();
            let result = list(&path).map(|names| strings(&names));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error) }
        }

        #[unsafe(method_id(contentsOfDirectoryAtURL:includingPropertiesForKeys:options:error:))]
        fn contents_of_directory_at_url(
            &self,
            url: &NSURL,
            _keys: Option<&AnyObject>,
            options: NSUInteger,
            error: *mut *mut NSError,
        ) -> Option<Retained<AnyObject>> {
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(contents_of_directory_at_url(url, options), error) }
        }

        #[unsafe(method_id(subpathsOfDirectoryAtPath:error:))]
        fn subpaths_of_directory_at_path(&self, path: &NSString, error: *mut *mut NSError) -> Option<Retained<AnyObject>> {
            let path = path.to_string();
            let result = subpaths(Path::new(&path)).map(|names| strings(&names)).map_err(|e| error::file(FileOp::Read, &e, &path));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error) }
        }

        #[unsafe(method_id(subpathsAtPath:))]
        fn subpaths_at_path(&self, path: &NSString) -> Option<Retained<AnyObject>> {
            subpaths(Path::new(&path.to_string())).ok().map(|names| strings(&names))
        }

        #[unsafe(method_id(attributesOfItemAtPath:error:))]
        fn attributes_of_item(&self, path: &NSString, error: *mut *mut NSError) -> Option<Retained<AnyObject>> {
            let path = path.to_string();
            let result = std::fs::symlink_metadata(&path)
                .map(|m| attributes::of_item(&m))
                .map_err(|e| error::file(FileOp::Read, &e, &path));
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(result, error) }
        }

        #[unsafe(method_id(attributesOfFileSystemForPath:error:))]
        fn attributes_of_file_system(&self, path: &NSString, error: *mut *mut NSError) -> Option<Retained<AnyObject>> {
            let path = path.to_string();
            // SAFETY: the caller passes null or room for an error.
            unsafe { report(attributes::of_file_system(&path), error) }
        }

        #[unsafe(method_id(URLsForDirectory:inDomains:))]
        fn urls_for_directory(&self, directory: NSUInteger, domains: NSUInteger) -> Retained<AnyObject> {
            let urls: Vec<Retained<NSURL>> =
                xdg::search_path(directory, domains).iter().map(|p| directory_url(p)).collect();
            objc2_foundation::NSArray::from_retained_slice(&urls).into()
        }

        #[unsafe(method_id(componentsToDisplayForPath:))]
        fn components_to_display(&self, path: &NSString) -> Option<Retained<AnyObject>> {
            let path = path.to_string();
            let parts: Vec<String> = path.split('/').filter(|p| !p.is_empty()).map(str::to_string).collect();
            Some(strings(&parts))
        }
    }

    unsafe impl NSObjectProtocol for NSFileManagerImpl {}
);

fn default_manager() -> Retained<NSFileManagerImpl> {
    static DEFAULT: OnceLock<usize> = OnceLock::new();
    let ptr = *DEFAULT.get_or_init(|| {
        // SAFETY: +new on this class; the manager lives for the rest of the
        // process.
        let manager: Retained<NSFileManagerImpl> = unsafe { msg_send![NSFileManagerImpl::class(), new] };
        Retained::into_raw(manager) as usize
    });
    // SAFETY: the leaked default manager, never freed.
    unsafe { Retained::retain(ptr as *mut NSFileManagerImpl) }.expect("the default manager")
}

/// Report a result's error through a caller's `NSError **`.
///
/// # Safety
///
/// `error` is null or points to room for an error.
unsafe fn report<T>(result: Result<T, Retained<NSError>>, error: *mut *mut NSError) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(e) => {
            // SAFETY: per this function's contract.
            unsafe { error::set(error, e) };
            None
        }
    }
}

fn access(path: &str, mode: libc::c_int) -> bool {
    let Ok(path) = CString::new(path) else { return false };
    // SAFETY: a NUL-terminated path.
    unsafe { libc::access(path.as_ptr(), mode) == 0 }
}

/// The path of a file URL, or the error for reading another kind.
fn read_path(url: &NSURL) -> Result<String, Retained<NSError>> {
    url_path(url, error::code::FILE_READ_UNSUPPORTED_SCHEME)
}

/// The path of a file URL, or the error for writing another kind.
fn write_path(url: &NSURL) -> Result<String, Retained<NSError>> {
    url_path(url, error::code::FILE_WRITE_UNSUPPORTED_SCHEME)
}

fn url_path(url: &NSURL, code: isize) -> Result<String, Retained<NSError>> {
    use objc2::Message;
    crate::url::file_path(url)
        .map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| error::cocoa(code, &[(&error::URL, url.retain().into())]))
}

/// A file URL for a directory, with its trailing slash.
fn directory_url(path: &Path) -> Retained<NSURL> {
    // SAFETY: +fileURLWithPath:isDirectory: returns a URL for a non-empty path.
    unsafe {
        msg_send![NSURL::class(), fileURLWithPath: &*NSString::from_str(&path.to_string_lossy()), isDirectory: true]
    }
}

/// A file URL, looking at the file system for a directory's slash.
fn directory_or_file_url(path: &Path) -> Retained<NSURL> {
    crate::url::file_url(path).unwrap_or_else(|| directory_url(Path::new("/")))
}

fn exists_error(path: &str) -> Retained<NSError> {
    error::file_with_code(error::code::FILE_WRITE_FILE_EXISTS, Some(libc::EEXIST), path)
}

fn create_directory(path: &str, intermediates: bool, attributes: Option<&AnyObject>) -> Result<(), Retained<NSError>> {
    let made = if intermediates { std::fs::create_dir_all(path) } else { std::fs::create_dir(path) };
    made.and_then(|()| apply_attributes(path, attributes)).map_err(|e| error::file(FileOp::Write, &e, path))
}

/// Apply the attributes Linux has a place for: POSIX permissions and the
/// modification date.
fn apply_attributes(path: &str, attributes: Option<&AnyObject>) -> io::Result<()> {
    let Some(attributes) = attributes else { return Ok(()) };
    let value = |key: &'static crate::ConstantString| -> Option<Retained<AnyObject>> {
        // SAFETY: a dictionary; -objectForKey: returns a value or nil.
        unsafe { msg_send![attributes, objectForKey: crate::runloop::modes::constant(key)] }
    };
    if let Some(mode) = value(&POSIX_PERMISSIONS) {
        // SAFETY: an NSNumber.
        let mode: NSUInteger = unsafe { msg_send![&*mode, unsignedLongValue] };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode as u32 & 0o7777))?;
    }
    if let Some(date) = value(&MODIFICATION_DATE) {
        // SAFETY: an NSDate.
        let time: f64 = unsafe { msg_send![&*date, timeIntervalSinceReferenceDate] };
        let unix = time + crate::date::UNIX_TO_REFERENCE;
        let spec = |t: f64| libc::timespec { tv_sec: t.floor() as libc::time_t, tv_nsec: ((t - t.floor()) * 1e9) as _ };
        let times = [libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT }, spec(unix)];
        let path = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: a NUL-terminated path and two timespecs.
        if unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn remove(path: &str) -> Result<(), Retained<NSError>> {
    let result = std::fs::symlink_metadata(path)
        .and_then(|m| if m.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) });
    result.map_err(|e| error::file(FileOp::Write, &e, path))
}

fn copy(from: &str, to: &str) -> Result<(), Retained<NSError>> {
    std::fs::symlink_metadata(from).map_err(|e| error::file(FileOp::Read, &e, from))?;
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(exists_error(from));
    }
    copy_tree(Path::new(from), Path::new(to)).map_err(|e| error::file(FileOp::Write, &e, from))
}

/// Copy a file, a link (as a link) or a directory and what it holds.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(from)?;
    let kind = metadata.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
    } else if kind.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        std::fs::set_permissions(to, metadata.permissions())
    } else {
        std::fs::copy(from, to).map(drop)
    }
}

fn move_item(from: &str, to: &str) -> Result<(), Retained<NSError>> {
    if std::fs::symlink_metadata(to).is_ok() && std::fs::symlink_metadata(from).is_ok() {
        return Err(exists_error(from));
    }
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        // Across file systems: copy, then remove the original.
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(Path::new(from), Path::new(to)).map_err(|e| error::file(FileOp::Write, &e, from))?;
            remove(from)
        }
        Err(e) => Err(error::file(FileOp::Write, &e, from)),
    }
}

fn link(from: &str, to: &str) -> Result<(), Retained<NSError>> {
    std::fs::hard_link(from, to).map_err(|e| error::file(FileOp::Write, &e, from))
}

fn symlink(path: &str, destination: &str) -> Result<(), Retained<NSError>> {
    std::os::unix::fs::symlink(destination, path).map_err(|e| error::file(FileOp::Write, &e, path))
}

/// Whether two items hold the same: equal bytes for files, equal targets
/// for links, and the same names with equal contents for directories.
fn contents_equal(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) else { return false };
    let (ta, tb) = (ma.file_type(), mb.file_type());
    if ta.is_symlink() || tb.is_symlink() {
        return ta.is_symlink() && tb.is_symlink() && std::fs::read_link(a).ok() == std::fs::read_link(b).ok();
    }
    if ta.is_dir() || tb.is_dir() {
        if !(ta.is_dir() && tb.is_dir()) {
            return false;
        }
        let names = |p: &Path| -> Option<Vec<std::ffi::OsString>> {
            let mut names: Vec<_> = std::fs::read_dir(p).ok()?.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
            names.sort();
            Some(names)
        };
        return match (names(a), names(b)) {
            (Some(na), Some(nb)) => na == nb && na.iter().all(|n| contents_equal(&a.join(n), &b.join(n))),
            _ => false,
        };
    }
    if ma.len() != mb.len() {
        return false;
    }
    (ma.ino() == mb.ino() && ma.dev() == mb.dev())
        || matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

fn url_for_directory(
    directory: NSUInteger,
    domain: NSUInteger,
    url: Option<&NSURL>,
    create: bool,
) -> Result<Retained<NSURL>, Retained<NSError>> {
    if directory == xdg::dir::ITEM_REPLACEMENT {
        let Some(url) = url else { return Err(error::cocoa(error::code::FILE_READ_UNKNOWN, &[])) };
        let path = write_path(url)?;
        let dir = replacement_directory(Path::new(&path)).map_err(|e| error::file(FileOp::Write, &e, &path))?;
        return Ok(directory_url(&dir));
    }
    let Some(path) = xdg::search_path(directory, domain).into_iter().next() else {
        return Err(error::cocoa(error::code::FILE_NO_SUCH_FILE, &[]));
    };
    if create {
        std::fs::create_dir_all(&path).map_err(|e| error::file(FileOp::Write, &e, &path.to_string_lossy()))?;
    }
    Ok(directory_url(&path))
}

/// A new, empty directory on the same file system as `near`, for
/// building a replacement: in the temporary directory if that is on the
/// same file system, else beside the item.
fn replacement_directory(near: &Path) -> io::Result<PathBuf> {
    let parent = if near.is_dir() { near.to_path_buf() } else { near.parent().unwrap_or(Path::new("/")).to_path_buf() };
    let device = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let temp = xdg::temporary();
    let base = if device(&temp).is_some() && device(&temp) == device(&parent) {
        temp.join("TemporaryItems")
    } else {
        parent.join(".TemporaryItems")
    };
    std::fs::create_dir_all(&base)?;
    let process = crate::path::process_name();
    let template = CString::new(format!("{}/NSIRD_{process}_XXXXXX", base.to_string_lossy()))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut template = template.into_bytes_with_nul();
    // SAFETY: a NUL-terminated template ending in six X's, rewritten in
    // place.
    let made = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    if made.is_null() {
        return Err(io::Error::last_os_error());
    }
    template.pop();
    Ok(PathBuf::from(String::from_utf8_lossy(&template).into_owned()))
}

/// Move an item to the trash, returning where it went.
fn trash(path: &Path) -> io::Result<PathBuf> {
    use std::io::Write;
    let path = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    std::fs::symlink_metadata(&path)?;
    let trash = trash_directory(&path)?;
    let files = trash.join("files");
    let info = trash.join("info");
    for dir in [&trash, &files, &info] {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "item".into());
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (&name[..], ""),
    };
    let text = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        crate::url::parse::encode_component(&path.to_string_lossy(), "/"),
        local_time_now()
    );
    for n in 1.. {
        let candidate = if n == 1 { name.clone() } else { format!("{stem} {n}{ext}") };
        let record = info.join(format!("{candidate}.trashinfo"));
        let target = files.join(&candidate);
        if target.exists() || std::fs::symlink_metadata(&target).is_ok() {
            continue;
        }
        // Claim the name with the record first, as the specification asks.
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&record) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        file.write_all(text.as_bytes())?;
        return match std::fs::rename(&path, &target) {
            Ok(()) => Ok(target),
            Err(e) => {
                let _ = std::fs::remove_file(&record);
                Err(e)
            }
        };
    }
    unreachable!("names run out")
}

/// The trash for an item: the home trash on the home file system, else
/// `.Trash-$uid` at the top of the item's file system.
fn trash_directory(path: &Path) -> io::Result<PathBuf> {
    let home_trash = xdg::search_path(xdg::dir::TRASH, xdg::domain::USER).into_iter().next().unwrap_or_default();
    let device = |p: &Path| -> Option<u64> { p.ancestors().find_map(|a| std::fs::metadata(a).ok()).map(|m| m.dev()) };
    let item_device = std::fs::symlink_metadata(path)?.dev();
    if device(&home_trash) == Some(item_device) {
        return Ok(home_trash);
    }
    let mut top = path.to_path_buf();
    while let Some(parent) = top.parent() {
        if std::fs::metadata(parent).map(|m| m.dev()).ok() != Some(item_device) {
            break;
        }
        top = parent.to_path_buf();
    }
    // SAFETY: getuid can't fail.
    Ok(top.join(format!(".Trash-{}", unsafe { libc::getuid() })))
}

/// Local time as the trash records want it: `YYYY-MM-DDThh:mm:ss`.
fn local_time_now() -> String {
    // SAFETY: time(NULL) and localtime_r into a zeroed tm.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

fn strings(names: &[String]) -> Retained<AnyObject> {
    let strings: Vec<Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
    objc2_foundation::NSArray::from_retained_slice(&strings).into()
}

/// A directory's entry names, sorted.
fn list(path: &str) -> Result<Vec<String>, Retained<NSError>> {
    let entries = std::fs::read_dir(path).map_err(|e| error::file(FileOp::Read, &e, path))?;
    let mut names: Vec<String> =
        entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    Ok(names)
}

/// Every path under a directory, relative to it, without following links.
fn subpaths(root: &Path) -> io::Result<Vec<String>> {
    fn walk(root: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(root)?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            out.push(relative.clone());
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&entry.path(), &relative, out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, "", &mut out)?;
    Ok(out)
}

fn contents_of_directory_at_url(url: &NSURL, options: NSUInteger) -> Result<Retained<AnyObject>, Retained<NSError>> {
    let path = read_path(url)?;
    let mut names = list(&path)?;
    if options & SKIPS_HIDDEN_FILES != 0 {
        names.retain(|n| !n.starts_with('.'));
    }
    let dir = Path::new(&path);
    let base = directory_url(dir);
    let urls: Vec<Retained<NSURL>> = names
        .iter()
        .filter_map(|name| {
            let is_dir = dir.join(name).is_dir();
            let string = NSString::from_str(name);
            if options & PRODUCES_RELATIVE_PATH_URLS != 0 {
                // SAFETY: +fileURLWithPath:isDirectory:relativeToURL: with a
                // relative path.
                Some(unsafe {
                    msg_send![NSURL::class(), fileURLWithPath: &*string, isDirectory: is_dir, relativeToURL: &*base]
                })
            } else {
                // SAFETY: -URLByAppendingPathComponent:isDirectory: on a file URL.
                unsafe { msg_send![&*base, URLByAppendingPathComponent: &*string, isDirectory: is_dir] }
            }
        })
        .collect();
    Ok(objc2_foundation::NSArray::from_retained_slice(&urls).into())
}

/// An item's attributes, as `attributesOfItemAtPath:error:` gives them.
pub(crate) fn attributes_of_item(m: &std::fs::Metadata) -> Retained<AnyObject> {
    attributes::of_item(m)
}

mod attributes {
    use super::*;
    use objc2_foundation::{NSDate, NSDictionary, NSNumber};
    use std::os::unix::fs::FileTypeExt;

    fn number(value: u64) -> Retained<AnyObject> {
        NSNumber::new_u64(value).into()
    }

    fn date(time: Option<std::time::SystemTime>) -> Option<Retained<AnyObject>> {
        let since = time?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
        Some(NSDate::dateWithTimeIntervalSinceReferenceDate(since - crate::date::UNIX_TO_REFERENCE).into())
    }

    fn group_name(gid: u32) -> Option<String> {
        let mut buffer = vec![0 as libc::c_char; 4096];
        // SAFETY: zeroed is a valid group; getgrgid_r fills it.
        let mut entry: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        // SAFETY: as above.
        let status = unsafe { libc::getgrgid_r(gid, &mut entry, buffer.as_mut_ptr(), buffer.len(), &mut result) };
        if status != 0 || result.is_null() || entry.gr_name.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated name in `buffer`.
        Some(unsafe { std::ffi::CStr::from_ptr(entry.gr_name) }.to_string_lossy().into_owned())
    }

    pub(super) fn of_item(m: &std::fs::Metadata) -> Retained<AnyObject> {
        let t = m.file_type();
        let kind = if t.is_symlink() {
            &TYPE_SYMBOLIC_LINK
        } else if t.is_dir() {
            &TYPE_DIRECTORY
        } else if t.is_file() {
            &TYPE_REGULAR
        } else if t.is_socket() {
            &TYPE_SOCKET
        } else if t.is_char_device() {
            &TYPE_CHARACTER_SPECIAL
        } else if t.is_block_device() {
            &TYPE_BLOCK_SPECIAL
        } else {
            &TYPE_UNKNOWN
        };
        let constant = |c: &'static crate::ConstantString| -> Retained<AnyObject> {
            crate::runloop::modes::constant(c).retain().into()
        };
        use objc2::Message;
        let mut entries: Vec<(&'static crate::ConstantString, Retained<AnyObject>)> = vec![
            (&FILE_TYPE, constant(kind)),
            (&FILE_SIZE, number(m.len())),
            (&POSIX_PERMISSIONS, number(u64::from(m.mode() & 0o7777))),
            (&REFERENCE_COUNT, number(m.nlink())),
            (&OWNER_ACCOUNT_ID, number(u64::from(m.uid()))),
            (&GROUP_OWNER_ACCOUNT_ID, number(u64::from(m.gid()))),
            (&SYSTEM_NUMBER, number(m.dev())),
            (&SYSTEM_FILE_NUMBER, number(m.ino())),
            (&EXTENSION_HIDDEN, NSNumber::new_bool(false).into()),
            (&IMMUTABLE, NSNumber::new_bool(false).into()),
            (&APPEND_ONLY, NSNumber::new_bool(false).into()),
        ];
        if let Some(modified) = date(m.modified().ok()) {
            entries.push((&MODIFICATION_DATE, modified));
        }
        if let Some(created) = date(m.created().ok().or_else(|| m.modified().ok())) {
            entries.push((&CREATION_DATE, created));
        }
        if let Some(owner) = xdg::passwd_by_uid(m.uid()) {
            entries.push((&OWNER_ACCOUNT_NAME, NSString::from_str(&owner.name).into()));
        }
        if let Some(group) = group_name(m.gid()) {
            entries.push((&GROUP_OWNER_ACCOUNT_NAME, NSString::from_str(&group).into()));
        }
        if t.is_char_device() || t.is_block_device() {
            entries.push((&DEVICE_IDENTIFIER, number(m.rdev())));
        }
        dictionary(&entries)
    }

    pub(super) fn of_file_system(path: &str) -> Result<Retained<AnyObject>, Retained<NSError>> {
        let c = CString::new(path)
            .map_err(|_| error::file_with_code(error::code::FILE_READ_INVALID_FILE_NAME, None, path))?;
        // SAFETY: zeroed is a valid statvfs; statvfs fills it.
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path.
        if unsafe { libc::statvfs(c.as_ptr(), &mut stats) } != 0 {
            return Err(error::file(FileOp::Read, &io::Error::last_os_error(), path));
        }
        let device = std::fs::metadata(path).map(|m| m.dev()).unwrap_or(0);
        let block = stats.f_frsize as u64;
        let entries: Vec<(&'static crate::ConstantString, Retained<AnyObject>)> = vec![
            (&SYSTEM_SIZE, number(stats.f_blocks as u64 * block)),
            (&SYSTEM_FREE_SIZE, number(stats.f_bavail as u64 * block)),
            (&SYSTEM_NODES, number(stats.f_files as u64)),
            (&SYSTEM_FREE_NODES, number(stats.f_ffree as u64)),
            (&SYSTEM_NUMBER, number(device)),
        ];
        Ok(dictionary(&entries))
    }

    fn dictionary(entries: &[(&'static crate::ConstantString, Retained<AnyObject>)]) -> Retained<AnyObject> {
        let keys: Vec<&NSString> = entries.iter().map(|(k, _)| crate::runloop::modes::constant(k)).collect();
        let values: Vec<Retained<AnyObject>> = entries.iter().map(|(_, v)| v.clone()).collect();
        NSDictionary::from_retained_objects(&keys, &values).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_and_equality() {
        let dir = std::env::temp_dir().join(format!("sidestep-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let item = dir.join("a.txt");
        std::fs::write(&item, b"x").unwrap();
        let listed = list(dir.to_str().unwrap()).unwrap();
        assert_eq!(listed, ["a.txt"]);
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b"), b"").unwrap();
        assert_eq!(subpaths(&dir).unwrap(), ["a.txt", "sub", "sub/b"]);
        assert!(contents_equal(&dir, &dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
