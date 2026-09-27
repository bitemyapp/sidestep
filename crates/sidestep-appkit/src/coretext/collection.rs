//! Font collections: the system's families, one descriptor per family
//! (its regular face), or the faces query descriptors match, as fonts are
//! matched, less those exclusion descriptors match.

use std::cmp::Ordering;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::NSFontDescriptor;
use objc2_core_foundation::{CFArray, CFComparisonResult, CFDictionary, CFSet, CFString, CFTypeID};
use objc2_core_text::{
    CTFontCollection, CTFontCollectionCopyOptions, CTFontCollectionSortDescriptorsCallback, CTFontDescriptor,
    CTMutableFontCollection,
};
use objc2_foundation::{NSArray, NSCopying, NSDictionary, NSString};

use super::descriptor::{Dict, attribute, resolved};
use super::{any, owned, text_of};
use crate::font::{self as nsfont};
use crate::text::fonts::{self, Design, Family, FontSpec};

type Descriptors = Option<Retained<NSArray<AnyObject>>>;

pub(crate) struct CollectionIvars {
    /// The query and exclusion descriptors, behind a lock: an immutable
    /// collection may be read from any thread, a mutable one changed.
    descriptors: Mutex<(Descriptors, Descriptors)>,
    /// Made from the available fonts: every family.
    all: bool,
}

impl CollectionIvars {
    fn queries(&self) -> Descriptors {
        self.descriptors.lock().unwrap_or_else(|e| e.into_inner()).0.clone()
    }

    fn exclusions(&self) -> Descriptors {
        self.descriptors.lock().unwrap_or_else(|e| e.into_inner()).1.clone()
    }

    fn set(&self, queries: Option<Descriptors>, exclusions: Option<Descriptors>) {
        let mut d = self.descriptors.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(q) = queries {
            d.0 = q;
        }
        if let Some(e) = exclusions {
            d.1 = e;
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; what a mutable
    // collection changes is behind a lock.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCTFontCollection"]
    #[ivars = CollectionIvars]
    pub(crate) struct CTFontCollectionImpl;

    unsafe impl NSObjectProtocol for CTFontCollectionImpl {}
);

fn collection_imp(c: &CTFontCollection) -> &CTFontCollectionImpl {
    // SAFETY: every CTFontCollection is a CTFontCollectionImpl.
    unsafe { &*(c as *const CTFontCollection).cast::<CTFontCollectionImpl>() }
}

fn new_collection(queries: Descriptors, exclusions: Descriptors, all: bool) -> Retained<CTFontCollectionImpl> {
    let this = CTFontCollectionImpl::alloc()
        .set_ivars(CollectionIvars { descriptors: Mutex::new((queries, exclusions)), all });
    // SAFETY: NSObject's designated initializer.
    unsafe { msg_send![super(this), init] }
}

fn array_of(array: Option<&CFArray>) -> Descriptors {
    // SAFETY: a CFArray is an NSArray here.
    array.map(|a| unsafe { &*(a as *const CFArray).cast::<NSArray<AnyObject>>() }.copy())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_FONT_COLLECTION
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateFromAvailableFonts(
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CTFontCollection>> {
    Some(owned(new_collection(None, None, true)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateWithFontDescriptors(
    queries: Option<&CFArray>,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CTFontCollection>> {
    Some(owned(new_collection(array_of(queries), None, false)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateCopyWithFontDescriptors(
    original: Option<&CTFontCollection>,
    queries: Option<&CFArray>,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CTFontCollection>> {
    let original = collection_imp(original?);
    let mut all: Vec<Retained<AnyObject>> = original.ivars().queries().map(|q| q.to_vec()).unwrap_or_default();
    all.extend(array_of(queries).map(|q| q.to_vec()).unwrap_or_default());
    let exclusions = original.ivars().exclusions();
    Some(owned(new_collection(Some(NSArray::from_retained_slice(&all)), exclusions, original.ivars().all)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateMutableCopy(
    original: Option<&CTFontCollection>,
) -> Option<NonNull<CTMutableFontCollection>> {
    let original = collection_imp(original?);
    let ivars = original.ivars();
    Some(owned(new_collection(ivars.queries(), ivars.exclusions(), ivars.all)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCopyQueryDescriptors(
    collection: Option<&CTFontCollection>,
) -> Option<NonNull<CFArray>> {
    collection_imp(collection?).ivars().queries().map(owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionSetQueryDescriptors(
    collection: Option<&CTMutableFontCollection>,
    descriptors: Option<&CFArray>,
) {
    let Some(c) = collection else { return };
    // SAFETY: a mutable collection is a collection.
    let c = collection_imp(unsafe { &*(c as *const CTMutableFontCollection).cast::<CTFontCollection>() });
    c.ivars().set(Some(array_of(descriptors)), None);
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCopyExclusionDescriptors(
    collection: Option<&CTFontCollection>,
) -> Option<NonNull<CFArray>> {
    collection_imp(collection?).ivars().exclusions().map(owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionSetExclusionDescriptors(
    collection: Option<&CTMutableFontCollection>,
    descriptors: Option<&CFArray>,
) {
    let Some(c) = collection else { return };
    // SAFETY: a mutable collection is a collection.
    let c = collection_imp(unsafe { &*(c as *const CTMutableFontCollection).cast::<CTFontCollection>() });
    c.ivars().set(None, Some(array_of(descriptors)));
}

/// The descriptors a collection matches: every family's regular face for
/// the available fonts, each query's match otherwise; leaving out what an
/// exclusion matches.
fn matching(c: &CTFontCollectionImpl, family: Option<&str>) -> Vec<Retained<NSFontDescriptor>> {
    let mut specs: Vec<FontSpec> = Vec::new();
    if c.ivars().all {
        for name in fonts::family_names() {
            specs.push(FontSpec {
                family: Family::Named(name.as_str().into()),
                ..FontSpec::system(Design::Default, 0.0)
            });
        }
    }
    for q in c.ivars().queries().map(|q| q.to_vec()).unwrap_or_default() {
        if let Ok(d) = q.downcast::<NSFontDescriptor>() {
            let spec = nsfont::descriptor_spec(&d);
            if !spec.missing {
                specs.push(spec);
            }
        }
    }
    let excluded: Vec<String> = c
        .ivars()
        .exclusions()
        .map(|e| e.to_vec())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| d.downcast::<NSFontDescriptor>().ok())
        .map(|d| fonts::resolve(&nsfont::descriptor_spec(&d)).postscript_name.to_string())
        .collect();
    let mut out = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for spec in specs {
        let face = fonts::resolve(&spec);
        if excluded.iter().any(|e| **e == *face.postscript_name)
            || seen.iter().any(|s| **s == *face.postscript_name)
            || family.is_some_and(|f| !face.family_name.eq_ignore_ascii_case(f))
        {
            continue;
        }
        seen.push(face.postscript_name.to_string());
        if let Some(d) = resolved(spec, false) {
            out.push(d);
        }
    }
    out
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateMatchingFontDescriptors(
    collection: Option<&CTFontCollection>,
) -> Option<NonNull<CFArray>> {
    let found = matching(collection_imp(collection?), None);
    (!found.is_empty()).then(|| owned(NSArray::from_retained_slice(&found)))
}

/// Sorted by the callback, which gets each pair and `ref_con`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateMatchingFontDescriptorsSortedWithCallback(
    collection: Option<&CTFontCollection>,
    sort: CTFontCollectionSortDescriptorsCallback,
    ref_con: *mut c_void,
) -> Option<NonNull<CFArray>> {
    let mut found = matching(collection_imp(collection?), None);
    if let Some(sort) = sort {
        // The program's pointer as it gave it, null included (the type
        // says non-null; the ABI is the same).
        type Raw = unsafe extern "C-unwind" fn(
            NonNull<CTFontDescriptor>,
            NonNull<CTFontDescriptor>,
            *mut c_void,
        ) -> CFComparisonResult;
        // SAFETY: the two function types differ only in the pointer's
        // non-null promise.
        let sort: Raw = unsafe { std::mem::transmute(sort) };
        found = merge_sort(found, |a, b| {
            let (a, b) = (NonNull::from(&**a).cast(), NonNull::from(&**b).cast());
            // SAFETY: the callback takes two descriptors and the program's
            // pointer.
            unsafe { sort(a, b, ref_con) }.0.cmp(&0)
        });
    }
    (!found.is_empty()).then(|| owned(NSArray::from_retained_slice(&found)))
}

/// `items` sorted by `order`, stably; an order that isn't consistent
/// gives some order rather than a panic (as the standard sort may).
fn merge_sort<T>(items: Vec<T>, mut order: impl FnMut(&T, &T) -> Ordering) -> Vec<T> {
    fn go<T>(mut items: Vec<T>, order: &mut dyn FnMut(&T, &T) -> Ordering) -> Vec<T> {
        if items.len() < 2 {
            return items;
        }
        let back = items.split_off(items.len() / 2);
        let (front, back) = (go(items, order), go(back, order));
        let mut out = Vec::with_capacity(front.len() + back.len());
        let (mut front, mut back) = (front.into_iter().peekable(), back.into_iter().peekable());
        while let (Some(a), Some(b)) = (front.peek(), back.peek()) {
            let next = if order(b, a) == Ordering::Less { back.next() } else { front.next() };
            out.extend(next);
        }
        out.extend(front);
        out.extend(back);
        out
    }
    go(items, &mut order)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateMatchingFontDescriptorsWithOptions(
    collection: Option<&CTFontCollection>,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CFArray>> {
    CTFontCollectionCreateMatchingFontDescriptors(collection)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCreateMatchingFontDescriptorsForFamily(
    collection: Option<&CTFontCollection>,
    family: Option<&CFString>,
    _options: Option<&CFDictionary>,
) -> Option<NonNull<CFArray>> {
    let family = text_of(family?);
    let found = matching(collection_imp(collection?), Some(&family));
    (!found.is_empty()).then(|| owned(NSArray::from_retained_slice(&found)))
}

/// Each matching font's value for `attribute` (a null for none).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCopyFontAttribute(
    collection: Option<&CTFontCollection>,
    attribute: Option<&CFString>,
    _options: CTFontCollectionCopyOptions,
) -> Option<NonNull<CFArray>> {
    let (collection, attribute) = (collection?, attribute?);
    let values: Vec<Retained<AnyObject>> = matching(collection_imp(collection), None)
        .iter()
        .map(|d| self::attribute(d, attribute).unwrap_or_else(|| any(objc2_foundation::NSNull::null())))
        .collect();
    Some(owned(NSArray::from_retained_slice(&values)))
}

/// Each matching font's values for `attributes`, as a dictionary.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCollectionCopyFontAttributes(
    collection: Option<&CTFontCollection>,
    attributes: Option<&CFSet>,
    _options: CTFontCollectionCopyOptions,
) -> Option<NonNull<CFArray>> {
    let (collection, attributes) = (collection?, attributes?);
    // SAFETY: a CFSet is an NSSet here.
    let keys: Vec<Retained<AnyObject>> =
        unsafe { &*(attributes as *const CFSet).cast::<objc2_foundation::NSSet<AnyObject>>() }.allObjects().to_vec();
    let values: Vec<Retained<Dict>> = matching(collection_imp(collection), None)
        .iter()
        .map(|d| {
            let mut ks: Vec<&NSString> = Vec::new();
            let mut vs: Vec<Retained<AnyObject>> = Vec::new();
            for k in &keys {
                let Some(k) = k.downcast_ref::<NSString>() else { continue };
                // SAFETY: an NSString is a CFString here.
                let cf: &CFString = unsafe { &*(k as *const NSString).cast() };
                if let Some(v) = self::attribute(d, cf) {
                    ks.push(k);
                    vs.push(v);
                }
            }
            let refs: Vec<&AnyObject> = vs.iter().map(|v| &**v).collect();
            NSDictionary::from_slices(&ks, &refs)
        })
        .collect();
    Some(owned(NSArray::from_retained_slice(&values)))
}
