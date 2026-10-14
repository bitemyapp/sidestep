//! MDItem's filesystem metadata. Values come from the filesystem at query
//! time; attributes requiring a Spotlight index are absent on Linux.

use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::SystemTime;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, Message, define_class, msg_send};
use objc2_foundation::{NSArray, NSDate, NSDictionary, NSNull, NSNumber, NSString, NSURL};

macro_rules! keys {
    ($($name:ident = $value:literal,)*) => { $(sidestep_foundation::constant_string!($name = $value);)* };
}
keys! {
    kMDItemTitle = "kMDItemTitle",
    kMDItemTextContent = "kMDItemTextContent",
    kMDItemKeywords = "kMDItemKeywords",
    kMDItemKind = "kMDItemKind",
    kMDItemDisplayName = "kMDItemDisplayName",
    kMDItemPath = "kMDItemPath",
    kMDItemURL = "kMDItemURL",
    kMDItemFSName = "kMDItemFSName",
    kMDItemFSSize = "kMDItemFSSize",
    kMDItemFSOwnerUserID = "kMDItemFSOwnerUserID",
    kMDItemFSOwnerGroupID = "kMDItemFSOwnerGroupID",
    kMDItemFSCreationDate = "kMDItemFSCreationDate",
    kMDItemFSContentChangeDate = "kMDItemFSContentChangeDate",
    kMDItemFSInvisible = "kMDItemFSInvisible",
    kMDItemFSNodeCount = "kMDItemFSNodeCount",
    kMDItemFSIsExtensionHidden = "kMDItemFSIsExtensionHidden",
    kMDItemContentCreationDate = "kMDItemContentCreationDate",
    kMDItemContentModificationDate = "kMDItemContentModificationDate",
    kMDItemAttributeChangeDate = "kMDItemAttributeChangeDate",
    kMDItemLastUsedDate = "kMDItemLastUsedDate",
    kMDItemContentType = "kMDItemContentType",
    kMDItemContentTypeTree = "kMDItemContentTypeTree",
    kMDItemAuthors = "kMDItemAuthors",
    kMDItemComment = "kMDItemComment",
    kMDItemDescription = "kMDItemDescription",
    kMDItemCopyright = "kMDItemCopyright",
    kMDItemSubject = "kMDItemSubject",
    kMDItemVersion = "kMDItemVersion",
    kMDItemIdentifier = "kMDItemIdentifier",
    kMDItemLanguages = "kMDItemLanguages",
    kMDItemOrganizations = "kMDItemOrganizations",
    kMDItemPublishers = "kMDItemPublishers",
    kMDItemContributors = "kMDItemContributors",
    kMDItemCoverage = "kMDItemCoverage",
    kMDItemRights = "kMDItemRights",
    kMDItemEditors = "kMDItemEditors",
    kMDItemCreator = "kMDItemCreator",
    kMDItemEncodingApplications = "kMDItemEncodingApplications",
    kMDItemDueDate = "kMDItemDueDate",
    kMDItemNumberOfPages = "kMDItemNumberOfPages",
    kMDItemPixelWidth = "kMDItemPixelWidth",
    kMDItemPixelHeight = "kMDItemPixelHeight",
    kMDItemDurationSeconds = "kMDItemDurationSeconds",
    kMDItemWhereFroms = "kMDItemWhereFroms",
    kMDItemUserTags = "kMDItemUserTags",
    kMDItemIsUbiquitous = "kMDItemIsUbiquitous",
}

pub struct ItemData {
    path: PathBuf,
    identity: (u64, u64),
}
define_class!(
    // SAFETY: NSObject has no subclassing requirements; a metadata item owns
    // immutable path and identity data and is safe to query on any thread.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepMDItem"]
    #[ivars = ItemData]
    pub struct Item;
    unsafe impl NSObjectProtocol for Item {
        #[unsafe(method(isEqual:))]
        fn equal(&self, other: Option<&AnyObject>) -> bool {
            other.and_then(|o| o.downcast_ref::<Item>()).is_some_and(|o| o.ivars().identity == self.ivars().identity)
        }
        #[unsafe(method(hash))]
        fn item_hash(&self) -> usize {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.ivars().identity.hash(&mut h);
            h.finish() as usize
        }
    }
);

fn item(path: PathBuf) -> Option<Retained<Item>> {
    let path = path.canonicalize().ok()?;
    let metadata = path.metadata().ok()?;
    let this = Item::alloc().set_ivars(ItemData { path, identity: (metadata.dev(), metadata.ino()) });
    Some(unsafe { msg_send![super(this), init] })
}
fn object<T: Message>(value: Retained<T>) -> Retained<AnyObject> {
    unsafe { Retained::cast_unchecked(value) }
}
fn owned<T: Message>(value: Option<Retained<T>>) -> *mut T {
    value.map_or(std::ptr::null_mut(), Retained::into_raw)
}
fn date(time: std::io::Result<SystemTime>) -> Option<Retained<AnyObject>> {
    let time = time.ok()?;
    let seconds = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    };
    Some(object(NSDate::dateWithTimeIntervalSince1970(seconds)))
}

const ATTRIBUTES: &[&str] = &[
    "kMDItemPath",
    "kMDItemFSName",
    "kMDItemDisplayName",
    "kMDItemFSSize",
    "kMDItemFSOwnerUserID",
    "kMDItemFSOwnerGroupID",
    "kMDItemFSCreationDate",
    "kMDItemFSContentChangeDate",
    "kMDItemContentCreationDate",
    "kMDItemContentModificationDate",
    "kMDItemAttributeChangeDate",
    "kMDItemFSInvisible",
    "kMDItemFSNodeCount",
];
fn attribute(item: &Item, name: &str) -> Option<Retained<AnyObject>> {
    let path = &item.ivars().path;
    let metadata = path.metadata().ok()?;
    if (metadata.dev(), metadata.ino()) != item.ivars().identity {
        return None;
    }
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    Some(match name {
        "kMDItemPath" => object(NSString::from_str(&path.to_string_lossy())),
        "kMDItemFSName" | "kMDItemDisplayName" => object(NSString::from_str(&filename)),
        "kMDItemFSSize" => object(NSNumber::new_u64(metadata.len())),
        "kMDItemFSOwnerUserID" => object(NSNumber::new_u32(metadata.uid())),
        "kMDItemFSOwnerGroupID" => object(NSNumber::new_u32(metadata.gid())),
        "kMDItemFSInvisible" => object(NSNumber::new_bool(filename.starts_with('.'))),
        "kMDItemFSNodeCount" if metadata.is_dir() => {
            object(NSNumber::new_u64(std::fs::read_dir(path).ok()?.count() as u64))
        }
        "kMDItemFSCreationDate" | "kMDItemContentCreationDate" => return date(metadata.created()),
        "kMDItemFSContentChangeDate" | "kMDItemContentModificationDate" => return date(metadata.modified()),
        "kMDItemAttributeChangeDate" => {
            object(NSDate::dateWithTimeIntervalSince1970(metadata.ctime() as f64 + metadata.ctime_nsec() as f64 / 1e9))
        }
        _ => return None,
    })
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn MDItemGetTypeID() -> usize {
    201
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn MDItemCreate(_allocator: *const c_void, path: *const NSString) -> *mut Item {
    owned(unsafe { path.as_ref() }.and_then(|p| item(PathBuf::from(p.to_string()))))
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn MDItemCreateWithURL(_allocator: *const c_void, url: *const NSURL) -> *mut Item {
    let Some(url) = (unsafe { url.as_ref() }) else { return std::ptr::null_mut() };
    if !url.isFileURL() {
        return std::ptr::null_mut();
    }
    owned(url.path().and_then(|p| item(PathBuf::from(p.to_string()))))
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn MDItemCopyAttribute(item: *const Item, name: *const NSString) -> *mut AnyObject {
    owned(unsafe { name.as_ref() }.and_then(|name| attribute(unsafe { &*item }, &name.to_string())))
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn MDItemCopyAttributeNames(item: *const Item) -> *mut NSArray<NSString> {
    let item = unsafe { &*item };
    let names: Vec<_> =
        ATTRIBUTES.iter().filter(|name| attribute(item, name).is_some()).map(|n| NSString::from_str(n)).collect();
    Retained::into_raw(NSArray::from_retained_slice(&names))
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn MDItemCopyAttributes(
    item: *const Item,
    names: *const NSArray<NSString>,
) -> *mut NSDictionary<NSString, AnyObject> {
    let Some(names) = (unsafe { names.as_ref() }) else { return std::ptr::null_mut() };
    let item = unsafe { &*item };
    let (mut keys, mut values) = (Vec::new(), Vec::new());
    for name in names {
        if let Some(value) = attribute(item, &name.to_string()) {
            keys.push(name);
            values.push(value);
        }
    }
    let keys: Vec<&NSString> = keys.iter().map(|s| &**s).collect();
    Retained::into_raw(NSDictionary::from_retained_objects(&keys, &values))
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn MDItemsCreateWithURLs(
    allocator: *const c_void,
    urls: *const NSArray<NSURL>,
) -> *mut NSArray<AnyObject> {
    let Some(urls) = (unsafe { urls.as_ref() }) else { return std::ptr::null_mut() };
    let values: Vec<_> = urls
        .iter()
        .map(|url| {
            unsafe { Retained::from_raw(MDItemCreateWithURL(allocator, &*url)) }
                .map(object)
                .unwrap_or_else(|| object(NSNull::null()))
        })
        .collect();
    Retained::into_raw(NSArray::from_retained_slice(&values))
}
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn MDItemsCopyAttributes(
    items: *const NSArray<AnyObject>,
    names: *const NSArray<NSString>,
) -> *mut NSArray<AnyObject> {
    let Some(items) = (unsafe { items.as_ref() }) else { return std::ptr::null_mut() };
    if names.is_null() {
        return std::ptr::null_mut();
    }
    let values: Vec<_> = items
        .iter()
        .map(|value| {
            value
                .downcast_ref::<Item>()
                .and_then(|i| unsafe { Retained::from_raw(MDItemCopyAttributes(i, names)) })
                .map(object)
                .unwrap_or_else(|| object(NSNull::null()))
        })
        .collect();
    Retained::into_raw(NSArray::from_retained_slice(&values))
}
