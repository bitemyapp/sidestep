//! `CTFontDescriptor`: an `NSFontDescriptor`, sharing its attribute
//! handling and matching.
//!
//! A descriptor reports the attributes it was made with, as macOS does
//! (`CTFontDescriptorCopyAttributes` gives back the dictionary given), and
//! those of the font it describes for what it wasn't given. Deriving one by
//! family, traits or matching resolves it to a face, and reports that
//! face's name, as macOS does. Matching finds every face of the family
//! asked for; a font file's face that isn't registered with the font
//! manager is in no list of matches (measured on macOS).

use std::ptr::NonNull;

use objc2::Message;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSFontDescriptor;
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFSet, CFString, CFType, CFTypeID, CGFloat};
use objc2_core_text::{CTFontDescriptor, CTFontSymbolicTraits};
use objc2_foundation::{NSArray, NSDictionary, NSMutableArray, NSNumber, NSObjectProtocol, NSString};

use super::{ns_descriptor, ns_string, number, owned, text_of};
use crate::font::{self as nsfont};
use crate::text::fonts::{self, Family, FontSpec};

pub(crate) type Dict = NSDictionary<NSString, AnyObject>;

pub(crate) fn descriptor(d: Retained<NSFontDescriptor>) -> NonNull<CTFontDescriptor> {
    owned(d)
}

fn dict_of(cf: &CFDictionary) -> &Dict {
    // SAFETY: a CFDictionary is an NSDictionary here; looking string keys up
    // is sound whatever it holds.
    unsafe { &*(cf as *const CFDictionary).cast::<Dict>() }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_FONT_DESCRIPTOR
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateWithNameAndSize(
    name: Option<&CFString>,
    size: CGFloat,
) -> Option<NonNull<CTFontDescriptor>> {
    Some(descriptor(NSFontDescriptor::fontDescriptorWithName_size(ns_string(name?), size)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateWithAttributes(
    attributes: Option<&CFDictionary>,
) -> Option<NonNull<CTFontDescriptor>> {
    // SAFETY: the attributes are a dictionary of font attributes.
    Some(descriptor(unsafe { NSFontDescriptor::fontDescriptorWithFontAttributes(attributes.map(dict_of)) }))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateCopyWithAttributes(
    original: Option<&CTFontDescriptor>,
    attributes: Option<&CFDictionary>,
) -> Option<NonNull<CTFontDescriptor>> {
    let original = ns_descriptor(original?);
    let Some(attributes) = attributes else { return Some(descriptor(original.retain())) };
    // SAFETY: the attributes are a dictionary of font attributes.
    Some(descriptor(unsafe { original.fontDescriptorByAddingAttributes(dict_of(attributes)) }))
}

/// A descriptor naming the face `spec` resolves to, as macOS's derived
/// descriptors do; none for a spec no font matches.
pub(crate) fn resolved(spec: FontSpec, with_size: bool) -> Option<Retained<NSFontDescriptor>> {
    if spec.missing {
        return None;
    }
    let face = fonts::resolve(&spec);
    // SAFETY: the constants are this crate's own, always valid.
    let (name_key, size_key) = unsafe { (objc2_app_kit::NSFontNameAttribute, objc2_app_kit::NSFontSizeAttribute) };
    let name = NSString::from_str(&face.postscript_name);
    let attributes: Retained<Dict> = if with_size && spec.size > 0.0 {
        let size = number(spec.size);
        NSDictionary::from_slices(&[name_key, size_key], &[&*name as &AnyObject, &*size])
    } else {
        NSDictionary::from_slices(&[name_key], &[&*name as &AnyObject])
    };
    Some(nsfont::make_descriptor(spec, Some(attributes)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateCopyWithFamily(
    original: Option<&CTFontDescriptor>,
    family: Option<&CFString>,
) -> Option<NonNull<CTFontDescriptor>> {
    let spec = nsfont::descriptor_spec(ns_descriptor(original?));
    let named = fonts::spec_named(&text_of(family?), 0.0)?;
    resolved(FontSpec { family: named.family, missing: false, ..spec }, false).map(descriptor)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateCopyWithSymbolicTraits(
    original: Option<&CTFontDescriptor>,
    value: CTFontSymbolicTraits,
    mask: CTFontSymbolicTraits,
) -> Option<NonNull<CTFontDescriptor>> {
    let original = ns_descriptor(original?);
    let spec = nsfont::descriptor_spec(original);
    let current = nsfont::traits_of(&spec).0;
    let wanted = (current & !mask.0) | (value.0 & mask.0);
    if wanted != current && matches!(spec.family, Family::Data(_)) {
        return None;
    }
    let changed = original.fontDescriptorWithSymbolicTraits(objc2_app_kit::NSFontDescriptorSymbolicTraits(wanted));
    resolved(nsfont::descriptor_spec(&changed), true).map(descriptor)
}

/// A copy with an axis set (by its tag as a number): fonts made from it
/// are matched and laid out at that value, on the axes their face has.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateCopyWithVariation(
    original: Option<&CTFontDescriptor>,
    identifier: Option<&CFNumber>,
    value: CGFloat,
) -> Option<NonNull<CTFontDescriptor>> {
    let original = ns_descriptor(original?);
    // SAFETY: a CFNumber is an NSNumber here.
    let identifier: &NSNumber = unsafe { &*(identifier? as *const CFNumber).cast() };
    // SAFETY: the constant is this crate's own.
    let key = ns_string(unsafe { objc2_core_text::kCTFontVariationAttribute });
    let mut entries: Vec<(Retained<NSNumber>, Retained<AnyObject>)> = Vec::new();
    if let Some(existing) = original.objectForKey(key)
        && let Ok(existing) = existing.downcast::<NSDictionary>()
    {
        // SAFETY: a variation dictionary has numbers for keys.
        let existing: Retained<NSDictionary<NSNumber, AnyObject>> = unsafe { Retained::cast_unchecked(existing) };
        for k in existing.allKeys() {
            if let Some(v) = existing.objectForKey(&k)
                && !k.isEqualToNumber(identifier)
            {
                entries.push((k, v));
            }
        }
    }
    entries.push((identifier.retain(), number(value)));
    let keys: Vec<&NSNumber> = entries.iter().map(|e| &*e.0).collect();
    let values: Vec<&AnyObject> = entries.iter().map(|e| &*e.1).collect();
    let variation = NSDictionary::from_slices(&keys, &values);
    let added = NSDictionary::from_slices(&[key], &[&*variation as &AnyObject]);
    // SAFETY: the attributes are font attributes.
    Some(descriptor(unsafe { original.fontDescriptorByAddingAttributes(&added) }))
}

/// A copy with one more feature setting (replacing any of the same type
/// and selector pair's feature).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateCopyWithFeature(
    original: Option<&CTFontDescriptor>,
    kind: Option<&CFNumber>,
    selector: Option<&CFNumber>,
) -> Option<NonNull<CTFontDescriptor>> {
    let original = ns_descriptor(original?);
    // SAFETY: CFNumbers are NSNumbers here.
    let (kind, selector): (&NSNumber, &NSNumber) =
        unsafe { (&*(kind? as *const CFNumber).cast(), &*(selector? as *const CFNumber).cast()) };
    // SAFETY: the constants are this crate's own.
    let (settings_key, type_key, selector_key) = unsafe {
        (
            ns_string(objc2_core_text::kCTFontFeatureSettingsAttribute),
            ns_string(objc2_core_text::kCTFontFeatureTypeIdentifierKey),
            ns_string(objc2_core_text::kCTFontFeatureSelectorIdentifierKey),
        )
    };
    let items = NSMutableArray::<AnyObject>::new();
    if let Some(existing) = original.fontAttributes().objectForKey(settings_key) {
        for item in nsfont::array_items(&existing) {
            let same_type = item
                .downcast_ref::<NSDictionary>()
                .and_then(|d| {
                    // SAFETY: looking a string key up is sound whatever the
                    // dictionary holds.
                    let d: &Dict = unsafe { &*(d as *const NSDictionary).cast() };
                    d.objectForKey(type_key)
                })
                // SAFETY: isEqual: takes an object and returns BOOL.
                .is_some_and(|t| unsafe { msg_send![&*t, isEqual: kind] });
            if !same_type {
                items.addObject(&item);
            }
        }
    }
    let setting = NSDictionary::from_slices(&[type_key, selector_key], &[kind as &AnyObject, selector as &AnyObject]);
    items.addObject(&setting);
    let added = NSDictionary::from_slices(&[settings_key], &[&*items as &AnyObject]);
    // SAFETY: the attributes are font attributes.
    Some(descriptor(unsafe { original.fontDescriptorByAddingAttributes(&added) }))
}

/// The spec a descriptor matches with: none for one no font matches, or,
/// when `registered`, for a font file's face the font manager hasn't
/// registered (measured on macOS: a font file's own descriptor matches
/// itself, but no list of matches has it).
fn matchable(d: &CTFontDescriptor, registered: bool) -> Option<FontSpec> {
    let spec = nsfont::descriptor_spec(ns_descriptor(d));
    match &spec.family {
        _ if spec.missing => None,
        Family::Data(face) if registered && !fonts::registered_names().iter().any(|(_, f)| f == face) => None,
        _ => Some(spec),
    }
}

/// The faces of the family the descriptor names, the one it names first,
/// unless it names a face (by name or by traits).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateMatchingFontDescriptors(
    descriptor: Option<&CTFontDescriptor>,
    _mandatory: Option<&CFSet>,
) -> Option<NonNull<CFArray>> {
    let descriptor = descriptor?;
    let spec = matchable(descriptor, true)?;
    let first = resolved(spec.clone(), false)?;
    let mut found = vec![first];
    // SAFETY: the constants are this crate's own.
    let (name_key, traits_key) = unsafe {
        (ns_string(objc2_core_text::kCTFontNameAttribute), ns_string(objc2_core_text::kCTFontTraitsAttribute))
    };
    let attributes = ns_descriptor(descriptor).fontAttributes();
    let names_face = attributes.objectForKey(name_key).is_some() || attributes.objectForKey(traits_key).is_some();
    if let Family::Named(family) = &spec.family
        && !names_face
    {
        let first = fonts::resolve(&spec).postscript_name.clone();
        for (weight, italic, stretch) in fonts::family_faces(family) {
            let face = FontSpec { weight, italic, stretch, ..spec.clone() };
            if *fonts::resolve(&face).postscript_name == *first {
                continue;
            }
            if let Some(d) = resolved(face, false)
                && !found.iter().any(|f| f.isEqual(Some(&d)))
            {
                found.push(d);
            }
        }
    }
    Some(owned(NSArray::from_retained_slice(&found)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCreateMatchingFontDescriptor(
    descriptor: Option<&CTFontDescriptor>,
    _mandatory: Option<&CFSet>,
) -> Option<NonNull<CTFontDescriptor>> {
    resolved(matchable(descriptor?, false)?, false).map(self::descriptor)
}

/// Nothing is downloaded: matching finishes at once, without calling the
/// handler, as it does on macOS for fonts that are all there.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorMatchFontDescriptorsWithProgressHandler(
    _descriptors: Option<&CFArray>,
    _mandatory: Option<&CFSet>,
    _progress: *mut std::ffi::c_void,
) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCopyAttributes(
    descriptor: Option<&CTFontDescriptor>,
) -> Option<NonNull<CFDictionary>> {
    Some(owned(ns_descriptor(descriptor?).fontAttributes()))
}

/// What a descriptor was given for `key`, or its font's value.
pub(crate) fn attribute(d: &NSFontDescriptor, key: &CFString) -> Option<Retained<AnyObject>> {
    if let Some(given) = d.fontAttributes().objectForKey(ns_string(key)) {
        return Some(given);
    }
    let spec = nsfont::descriptor_spec(d);
    if spec.missing {
        return None;
    }
    let font = nsfont::make_font(FontSpec { size: super::size_or_default(spec.size), ..spec });
    super::font::attribute_of(super::ct_font(&font), &text_of(key))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDescriptorCopyAttribute(
    descriptor: Option<&CTFontDescriptor>,
    key: Option<&CFString>,
) -> Option<NonNull<CFType>> {
    attribute(ns_descriptor(descriptor?), key?).map(owned)
}

/// # Safety
///
/// `language` is null or valid to write a string pointer through.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontDescriptorCopyLocalizedAttribute(
    descriptor: Option<&CTFontDescriptor>,
    key: Option<&CFString>,
    language: *mut *const CFString,
) -> Option<NonNull<CFType>> {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(language, std::ptr::null()) };
    CTFontDescriptorCopyAttribute(descriptor, key)
}
