//! `CTFont`: an `NSFont` (see the module above). Fonts are made by name,
//! from descriptors, from a `CGFont`'s font file (one face a file, laid out
//! as a family of its own, `text::fonts::register_data`, so that text laid
//! out in it finds exactly it; a file without a character map still makes
//! a font of its bytes) and as the interface fonts; a size of 0 means 12
//! points. Copies with other attributes, sizes or traits are the fonts
//! those make, which `NSFont`'s cache hands out again for the same
//! description, so a copy asking for nothing new is the font itself.
//!
//! Metrics are the font file's integers scaled to the size exactly
//! (`units × size / unitsPerEm`, in double precision), as CoreText gives
//! them: the ascent, descent and leading `NSFont` reports, the cap and x
//! heights (from `OS/2`, or where macOS finds them without it), the
//! underline's position and thickness from `post`, the bounding box from
//! `head`. Glyph functions read the font's tables with skrifa: advances
//! from `hmtx` (a glyph the font doesn't have advances nothing), bounds
//! from the outlines' boxes (a glyph with no outline has an empty one,
//! which a union leaves out; each face remembers its glyphs' boxes, as a
//! CFF outline has to be drawn to find one), outlines as `CGPath`s in the
//! font's coordinates (y up), names from `post` or the CFF charset
//! (remembered by name). The vertical metrics follow macOS for fonts
//! without vertical tables: each glyph turned a quarter, across from its
//! left edge less half its advance, down from the typographic ascender.

use std::ffi::{c_uint, c_void};
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, Message, msg_send};
use objc2_app_kit::NSFont;
use objc2_core_foundation::{
    CFArray, CFCharacterSet, CFData, CFDictionary, CFIndex, CFRange, CFString, CFStringEncoding, CFType, CFTypeID,
    CGAffineTransform, CGFloat, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{CGContext, CGFont, CGGlyph, CGPath};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontOptions, CTFontOrientation, CTFontSymbolicTraits, CTFontTableOptions,
    CTFontTableTag, CTFontUIFontType, kCTFontSlantTrait, kCTFontSymbolicTrait, kCTFontWeightTrait, kCTFontWidthTrait,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber, NSString, ns_string};
use skrifa::MetadataProvider;
use skrifa::raw::TableProvider;
use skrifa::string::StringId;

use super::{integer, ns_descriptor, ns_font, number, owned, size_or_default, text_of};
use crate::coregraphics::geometry::rect;
use crate::font::{self as nsfont};
use crate::text::fonts::{self, Design, Face, Family, FontSpec};

/// What `font` was made from and the face it resolved to.
pub(crate) fn parts(font: &CTFont) -> (&FontSpec, &Arc<Face>) {
    nsfont::parts(ns_font(font))
}

/// Points per font unit at the font's size.
fn scale(font: &CTFont) -> f64 {
    let (spec, face) = parts(font);
    if face.units.per_em > 0.0 { spec.size / face.units.per_em } else { 0.0 }
}

/// The face's font file, read with skrifa, and its variation location.
pub(crate) fn with_skrifa<R>(
    face: &Face,
    f: impl FnOnce(&skrifa::FontRef<'_>, &skrifa::instance::Location) -> R,
) -> Option<R> {
    let data = face.font.as_ref()?;
    let font = skrifa::FontRef::from_index(data.data.data(), data.index).ok()?;
    let location = font.axes().location(face.variations.iter().copied());
    Some(f(&font, &location))
}

/// A font as a +1 `CTFont`.
fn made(font: Retained<NSFont>) -> Option<NonNull<CTFont>> {
    Some(owned(font))
}

/// The font `spec` describes, as `NSFont` makes it.
pub(crate) fn font_of_spec(spec: FontSpec) -> Retained<NSFont> {
    nsfont::make_font(spec)
}

/// The font a name that finds nothing gives: Helvetica, as on macOS
/// (the interface font here).
fn fallback(size: f64) -> FontSpec {
    fonts::spec_named("Helvetica", size).unwrap_or_else(|| FontSpec::system(Design::Default, size))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CT_FONT
}

/// A font's matrix other than the identity isn't applied.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateWithName(
    name: Option<&CFString>,
    size: CGFloat,
    _matrix: *const CGAffineTransform,
) -> Option<NonNull<CTFont>> {
    let size = size_or_default(size);
    let spec = name.and_then(|n| fonts::spec_named(&text_of(n), size)).unwrap_or_else(|| fallback(size));
    made(font_of_spec(spec))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateWithNameAndOptions(
    name: Option<&CFString>,
    size: CGFloat,
    matrix: *const CGAffineTransform,
    _options: CTFontOptions,
) -> Option<NonNull<CTFont>> {
    CTFontCreateWithName(name, size, matrix)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateWithFontDescriptor(
    descriptor: Option<&CTFontDescriptor>,
    size: CGFloat,
    _matrix: *const CGAffineTransform,
) -> Option<NonNull<CTFont>> {
    let spec = descriptor.map(|d| nsfont::descriptor_spec(ns_descriptor(d)));
    let size = if size > 0.0 { size } else { size_or_default(spec.as_ref().map_or(0.0, |s| s.size)) };
    let spec = match spec {
        Some(spec) if !spec.missing => FontSpec { size, ..spec },
        _ => fallback(size),
    };
    made(font_of_spec(spec))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateWithFontDescriptorAndOptions(
    descriptor: Option<&CTFontDescriptor>,
    size: CGFloat,
    matrix: *const CGAffineTransform,
    _options: CTFontOptions,
) -> Option<NonNull<CTFont>> {
    CTFontCreateWithFontDescriptor(descriptor, size, matrix)
}

/// The interface fonts, with AppKit's sizes for them.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateUIFontForLanguage(
    ui_type: CTFontUIFontType,
    size: CGFloat,
    _language: Option<&CFString>,
) -> Option<NonNull<CTFont>> {
    // The sizes and weights macOS gives them.
    let (design, bold, default) = match ui_type.0 {
        0 | 8 | 26 => (Design::Default, false, 12.0),
        1 => (Design::Monospaced, false, 10.0),
        2 | 9 | 11..=14 | 16 | 23 | 27 => (Design::Default, false, 13.0),
        3 | 15 | 18 => (Design::Default, true, 13.0),
        4 | 17 | 21 | 24 | 25 => (Design::Default, false, 11.0),
        5 => (Design::Default, true, 11.0),
        6 | 19 => (Design::Default, false, 9.0),
        7 | 20 => (Design::Default, true, 9.0),
        10 | 22 => (Design::Default, false, 10.0),
        _ => return None,
    };
    let size = if size > 0.0 { size } else { default };
    let mut spec = FontSpec::system(design, size);
    if bold {
        spec.weight = 700.0;
    }
    made(font_of_spec(spec))
}

/// `spec` with a descriptor's attributes applied: all of them, or for a
/// font file's face, which is what it is, those that aren't about which
/// face (its features and size).
fn with_attributes(spec: &FontSpec, attributes: &NSDictionary<NSString, AnyObject>) -> FontSpec {
    let mut out = spec.clone();
    nsfont::apply_attributes(&mut out, attributes);
    if let Family::Data(_) = spec.family {
        (out.family, out.weight, out.italic, out.stretch, out.missing) =
            (spec.family.clone(), spec.weight, spec.italic, spec.stretch, false);
    }
    out
}

fn descriptor_attributes(d: &CTFontDescriptor) -> Retained<NSDictionary<NSString, AnyObject>> {
    ns_descriptor(d).fontAttributes()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateWithGraphicsFont(
    graphics_font: Option<&CGFont>,
    size: CGFloat,
    _matrix: *const CGAffineTransform,
    attributes: Option<&CTFontDescriptor>,
) -> Option<NonNull<CTFont>> {
    let data = &crate::coregraphics::font::font_imp(graphics_font?).ivars().data;
    let family = fonts::register_data(data)?;
    let mut spec = FontSpec::data(family, size_or_default(size));
    if let Some(d) = attributes {
        spec = with_attributes(&spec, &descriptor_attributes(d));
        spec.size = size_or_default(size);
    }
    made(font_of_spec(spec))
}

/// QuickDraw belongs to the classic Mac OS.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateWithQuickdrawInstance(
    _name: *const u8,
    _identifier: i16,
    _style: u8,
    _size: CGFloat,
) -> Option<NonNull<CTFont>> {
    None
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateCopyWithAttributes(
    font: Option<&CTFont>,
    size: CGFloat,
    _matrix: *const CGAffineTransform,
    attributes: Option<&CTFontDescriptor>,
) -> Option<NonNull<CTFont>> {
    let font = font?;
    let (spec, _) = parts(font);
    let mut spec = match attributes {
        Some(d) => with_attributes(spec, &descriptor_attributes(d)),
        None => spec.clone(),
    };
    if size > 0.0 {
        spec.size = size;
    }
    made(nsfont::font_like(ns_font(font), spec))
}

/// Traits a family doesn't have give no font; a font file's face is the
/// only one of its family.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateCopyWithSymbolicTraits(
    font: Option<&CTFont>,
    size: CGFloat,
    _matrix: *const CGAffineTransform,
    value: CTFontSymbolicTraits,
    mask: CTFontSymbolicTraits,
) -> Option<NonNull<CTFont>> {
    let font = font?;
    let (spec, _) = parts(font);
    let current = nsfont::traits_of(spec).0;
    let wanted = (current & !mask.0) | (value.0 & mask.0);
    let mut spec = spec.clone();
    if size > 0.0 {
        spec.size = size;
    }
    if wanted != current {
        if let Family::Data(_) = spec.family {
            return None;
        }
        let traits = objc2_app_kit::NSFontDescriptorSymbolicTraits(wanted);
        let descriptor = nsfont::make_descriptor(spec.clone(), None);
        let changed = descriptor.fontDescriptorWithSymbolicTraits(traits);
        spec = FontSpec { size: spec.size, ..nsfont::descriptor_spec(&changed) };
    }
    made(nsfont::font_like(ns_font(font), spec))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateCopyWithFamily(
    font: Option<&CTFont>,
    size: CGFloat,
    _matrix: *const CGAffineTransform,
    family: Option<&CFString>,
) -> Option<NonNull<CTFont>> {
    let (font, family) = (font?, family?);
    let (spec, _) = parts(font);
    let named = fonts::spec_named(&text_of(family), spec.size)?;
    let size = if size > 0.0 { size } else { spec.size };
    let spec = FontSpec { family: named.family, size, missing: false, ..spec.clone() };
    made(nsfont::font_like(ns_font(font), spec))
}

/// The font to draw `range` of `string` with: `current` if it has the
/// characters, else the one text layout falls back on for them.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateForString(
    current: Option<&CTFont>,
    string: Option<&CFString>,
    range: CFRange,
) -> Option<NonNull<CTFont>> {
    let (current, string) = (current?, string?);
    let units: Vec<u16> = text_of(string).encode_utf16().collect();
    let r = super::range_in(range, units.len(), false);
    let text = String::from_utf16_lossy(&units[r]);
    if text.is_empty() || covers(current, &text) {
        return made(ns_font(current).retain());
    }
    let attrs = crate::text::layout::Attrs::new(nsfont::text_font(ns_font(current)));
    let runs = [crate::text::layout::Run { start: 0, end: text.len(), attrs: 0 }];
    let lines = crate::text::glyphs::glyph_lines(
        &text,
        std::slice::from_ref(&attrs),
        &runs,
        crate::text::glyphs::Breaking::None,
        crate::text::layout::Direction::Natural,
    );
    let own = parts(current).1.font.as_ref().map(|f| (f.data.id(), f.index));
    let other = lines.iter().flat_map(|l| &l.runs).find(|r| Some((r.font.data.id(), r.font.index)) != own);
    match other {
        Some(run) => made(font_for_data(&run.font, parts(current).0.size)?),
        None => made(ns_font(current).retain()),
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCreateForStringWithLanguage(
    current: Option<&CTFont>,
    string: Option<&CFString>,
    range: CFRange,
    _language: Option<&CFString>,
) -> Option<NonNull<CTFont>> {
    CTFontCreateForString(current, string, range)
}

/// Whether `font`'s own face has every character of `text` that draws.
fn covers(font: &CTFont, text: &str) -> bool {
    with_skrifa(parts(font).1, |f, _| {
        let charmap = f.charmap();
        text.chars().all(|c| c.is_control() || charmap.map(c).is_some())
    })
    .unwrap_or(false)
}

/// The font a run laid out in `data` (a fallback face, say) is in, at
/// `size`.
pub(crate) fn font_for_data(data: &parley::FontData, size: f64) -> Option<Retained<NSFont>> {
    let family = fonts::register_data(data)?;
    Some(font_of_spec(FontSpec::data(family, size)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyFontDescriptor(font: Option<&CTFont>) -> Option<NonNull<CTFontDescriptor>> {
    Some(owned(ns_font(font?).fontDescriptor()))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyAttribute(
    font: Option<&CTFont>,
    attribute: Option<&CFString>,
) -> Option<NonNull<CFType>> {
    attribute_of(font?, &text_of(attribute?)).map(owned)
}

/// A font's value for a descriptor attribute, as CoreText reports it.
pub(crate) fn attribute_of(font: &CTFont, key: &str) -> Option<Retained<AnyObject>> {
    use objc2_core_text::{
        kCTFontCharacterSetAttribute, kCTFontDisplayNameAttribute, kCTFontEnabledAttribute, kCTFontFamilyNameAttribute,
        kCTFontFeatureSettingsAttribute, kCTFontFeaturesAttribute, kCTFontFormatAttribute, kCTFontMatrixAttribute,
        kCTFontNameAttribute, kCTFontOrientationAttribute, kCTFontPriorityAttribute, kCTFontRegistrationScopeAttribute,
        kCTFontSizeAttribute, kCTFontStyleNameAttribute, kCTFontTraitsAttribute, kCTFontVariationAxesAttribute,
    };
    let (spec, face) = parts(font);
    let string = |s: &str| super::any(NSString::from_str(s));
    let is = |c: &CFString| text_of(c) == key;
    let file = matches!(spec.family, Family::Data(_));
    // SAFETY: the constants are this crate's own.
    unsafe {
        Some(if is(kCTFontNameAttribute) {
            string(&face.postscript_name)
        } else if is(kCTFontDisplayNameAttribute) {
            string(&face.full_name)
        } else if is(kCTFontFamilyNameAttribute) {
            string(&face.family_name)
        } else if is(kCTFontStyleNameAttribute) {
            string(&name_string(face, StringId::SUBFAMILY_NAME)?)
        } else if is(kCTFontSizeAttribute) {
            number(spec.size)
        } else if is(kCTFontTraitsAttribute) {
            super::any(traits_dictionary(font))
        } else if is(kCTFontFormatAttribute) {
            let cff = with_skrifa(face, |f, _| f.table_data(skrifa::Tag::new(b"CFF ")).is_some()).unwrap_or(false);
            integer(if cff { 1 } else { 2 })
        } else if is(kCTFontFeatureSettingsAttribute) {
            nsfont::settings_of(spec)?
        } else if is(kCTFontMatrixAttribute) {
            let identity = [1.0f64, 0.0, 0.0, 1.0, 0.0, 0.0];
            let bytes: Vec<u8> = identity.iter().flat_map(|v| v.to_ne_bytes()).collect();
            super::any(NSData::with_bytes(&bytes))
        } else if is(kCTFontCharacterSetAttribute) {
            character_set(face)?
        } else if is(kCTFontVariationAxesAttribute) {
            variation_axes(face)?
        } else if is(kCTFontFeaturesAttribute) {
            features(face)?
        } else if is(kCTFontOrientationAttribute) {
            // Horizontal.
            integer(1)
        } else if is(kCTFontEnabledAttribute) {
            integer(1)
        } else if is(kCTFontRegistrationScopeAttribute) && file {
            // A font file's face isn't registered for a scope; it's the
            // process's, first in priority (measured on macOS).
            integer(0)
        } else if is(kCTFontPriorityAttribute) && file {
            integer(60_000)
        } else {
            return None;
        })
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetSize(font: Option<&CTFont>) -> CGFloat {
    font.map_or(0.0, |f| parts(f).0.size)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetMatrix(_font: Option<&CTFont>) -> CGAffineTransform {
    crate::coregraphics::geometry::IDENTITY
}

/// The font's traits: `NSFont`'s, and its stylistic class (the high four
/// bits) from `OS/2`'s family class, as CoreText reports it.
fn symbolic_traits(font: &CTFont) -> u32 {
    let (spec, face) = parts(font);
    let class = with_skrifa(face, |f, _| f.os2().ok().map(|o| u32::from((o.s_family_class() as u16) >> 8) & 0xf))
        .flatten()
        .unwrap_or(0);
    nsfont::traits_of(spec).0 | class << 28
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetSymbolicTraits(font: Option<&CTFont>) -> CTFontSymbolicTraits {
    CTFontSymbolicTraits(font.map_or(0, symbolic_traits))
}

fn traits_dictionary(font: &CTFont) -> Retained<NSDictionary<NSString, AnyObject>> {
    let face = parts(font).1;
    let symbolic = integer(i64::from(symbolic_traits(font)));
    let weight = number(fonts::ns_weight(face.weight));
    let width = number((f64::from(face.stretch) - 1.0) / 1.25);
    // No negative zero for an upright face.
    let slant = number(-face.units.italic_angle / 180.0 + 0.0);
    // SAFETY: the constants are this crate's own, always valid.
    let keys = unsafe {
        [kCTFontSymbolicTrait, kCTFontWeightTrait, kCTFontWidthTrait, kCTFontSlantTrait].map(super::ns_string)
    };
    NSDictionary::from_slices(&keys, &[&*symbolic, &*weight, &*width, &*slant])
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyTraits(font: Option<&CTFont>) -> Option<NonNull<CFDictionary>> {
    Some(owned(traits_dictionary(font?)))
}

/// The families text falls back on: the system's defaults for the generic
/// families, as descriptors.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyDefaultCascadeListForLanguages(
    font: Option<&CTFont>,
    _languages: Option<&CFArray>,
) -> Option<NonNull<CFArray>> {
    let size = parts(font?).0.size;
    let designs = [Design::Default, Design::Serif, Design::Monospaced];
    let descriptors: Vec<Retained<objc2_app_kit::NSFontDescriptor>> =
        designs.iter().map(|&d| nsfont::make_descriptor(FontSpec::system(d, size), None)).collect();
    Some(owned(NSArray::from_retained_slice(&descriptors)))
}

fn name_string(face: &Face, id: StringId) -> Option<String> {
    with_skrifa(face, |f, _| fonts::name_of(f, id)).flatten()
}

fn copy_string(s: &str) -> Option<NonNull<CFString>> {
    Some(owned(NSString::from_str(s)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyPostScriptName(font: Option<&CTFont>) -> Option<NonNull<CFString>> {
    copy_string(&parts(font?).1.postscript_name)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyFamilyName(font: Option<&CTFont>) -> Option<NonNull<CFString>> {
    copy_string(&parts(font?).1.family_name)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyFullName(font: Option<&CTFont>) -> Option<NonNull<CFString>> {
    copy_string(&parts(font?).1.full_name)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyDisplayName(font: Option<&CTFont>) -> Option<NonNull<CFString>> {
    copy_string(&parts(font?).1.full_name)
}

/// The `name` table's entry a name key asks for.
fn name_id(key: &str) -> Option<StringId> {
    use objc2_core_text::{
        kCTFontCopyrightNameKey, kCTFontDescriptionNameKey, kCTFontDesignerNameKey, kCTFontDesignerURLNameKey,
        kCTFontFamilyNameKey, kCTFontFullNameKey, kCTFontLicenseNameKey, kCTFontLicenseURLNameKey,
        kCTFontManufacturerNameKey, kCTFontPostScriptCIDNameKey, kCTFontPostScriptNameKey, kCTFontSampleTextNameKey,
        kCTFontSubFamilyNameKey, kCTFontTrademarkNameKey, kCTFontUniqueNameKey, kCTFontVendorURLNameKey,
        kCTFontVersionNameKey,
    };
    // SAFETY: the constants are this crate's own.
    let ids: [(&CFString, u16); 17] = unsafe {
        [
            (kCTFontCopyrightNameKey, 0),
            (kCTFontFamilyNameKey, 1),
            (kCTFontSubFamilyNameKey, 2),
            (kCTFontUniqueNameKey, 3),
            (kCTFontFullNameKey, 4),
            (kCTFontVersionNameKey, 5),
            (kCTFontPostScriptNameKey, 6),
            (kCTFontTrademarkNameKey, 7),
            (kCTFontManufacturerNameKey, 8),
            (kCTFontDesignerNameKey, 9),
            (kCTFontDescriptionNameKey, 10),
            (kCTFontVendorURLNameKey, 11),
            (kCTFontDesignerURLNameKey, 12),
            (kCTFontLicenseNameKey, 13),
            (kCTFontLicenseURLNameKey, 14),
            (kCTFontSampleTextNameKey, 19),
            (kCTFontPostScriptCIDNameKey, 20),
        ]
    };
    ids.iter().find(|(k, _)| text_of(k) == key).map(|&(_, id)| StringId::new(id))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyName(font: Option<&CTFont>, key: Option<&CFString>) -> Option<NonNull<CFString>> {
    let face = parts(font?).1;
    let id = name_id(&text_of(key?))?;
    // The names CoreText reports as the font's own, which for a face
    // standing in for another family are the face's.
    if id == StringId::POSTSCRIPT_NAME {
        return copy_string(&face.postscript_name);
    }
    if id == StringId::FAMILY_NAME && face.font.is_none() {
        return copy_string(&face.family_name);
    }
    copy_string(&name_string(face, id)?)
}

/// # Safety
///
/// `actual_language` is null or valid to write a string pointer through.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontCopyLocalizedName(
    font: Option<&CTFont>,
    key: Option<&CFString>,
    actual_language: *mut *const CFString,
) -> Option<NonNull<CFString>> {
    let name = CTFontCopyName(font, key);
    // The names are the English ones, or the first there are.
    let language: *const CFString = match name {
        Some(_) => ns_cf(ns_string!("en")),
        None => std::ptr::null(),
    };
    // SAFETY: as the caller promises; the language is a constant string,
    // which the caller doesn't own.
    unsafe { crate::coregraphics::store(actual_language, language) };
    name
}

fn ns_cf(s: &'static NSString) -> *const CFString {
    (s as *const NSString).cast()
}

/// The characters a face maps, as an `NSCharacterSet`.
fn character_set(face: &Face) -> Option<Retained<AnyObject>> {
    let ranges = with_skrifa(face, |f, _| {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for (c, _) in f.charmap().mappings() {
            match ranges.last_mut() {
                Some(last) if last.1 == c => last.1 = c + 1,
                _ => ranges.push((c, c + 1)),
            }
        }
        ranges
    })?;
    let class = objc2::runtime::AnyClass::get(c"NSMutableCharacterSet")?;
    // SAFETY: +new makes an empty mutable set; addCharactersInRange: takes
    // an NSRange of code points.
    unsafe {
        let set: Retained<AnyObject> = msg_send![class, new];
        for (start, end) in ranges {
            let range = objc2_foundation::NSRange::new(start as usize, (end - start) as usize);
            let _: () = msg_send![&*set, addCharactersInRange: range];
        }
        Some(set)
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyCharacterSet(font: Option<&CTFont>) -> Option<NonNull<CFCharacterSet>> {
    character_set(parts(font?).1).map(owned)
}

/// Mac Roman, as CoreText answers for Unicode fonts.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetStringEncoding(_font: Option<&CTFont>) -> CFStringEncoding {
    0
}

/// Languages aren't worked out from fonts' coverage: none are listed.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopySupportedLanguages(font: Option<&CTFont>) -> Option<NonNull<CFArray>> {
    font?;
    Some(owned(NSArray::<NSString>::new()))
}

/// Each UTF-16 unit's glyph: a surrogate pair's in the first unit and 0 in
/// the second, 0 for a character the font doesn't have, which makes the
/// result false.
///
/// # Safety
///
/// `characters` and `glyphs` hold `count` items.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontGetGlyphsForCharacters(
    font: Option<&CTFont>,
    characters: *const u16,
    glyphs: *mut CGGlyph,
    count: CFIndex,
) -> bool {
    let Some(font) = font else { return false };
    let n = usize::try_from(count).unwrap_or(0);
    if characters.is_null() || glyphs.is_null() || n == 0 {
        return n == 0;
    }
    // SAFETY: as the caller promises.
    let (chars, out) =
        unsafe { (std::slice::from_raw_parts(characters, n), std::slice::from_raw_parts_mut(glyphs, n)) };
    with_skrifa(parts(font).1, |f, _| {
        let charmap = f.charmap();
        let mut all = true;
        let mut i = 0;
        while i < n {
            let unit = chars[i];
            let (c, len) = if (0xd800..0xdc00).contains(&unit) && i + 1 < n && (0xdc00..0xe000).contains(&chars[i + 1])
            {
                let c = 0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(chars[i + 1]) - 0xdc00);
                (c, 2)
            } else {
                (u32::from(unit), 1)
            };
            let glyph = charmap.map(c).map_or(0, |g| g.to_u32() as u16);
            all &= glyph != 0;
            out[i] = glyph;
            if len == 2 {
                out[i + 1] = 0;
            }
            i += len;
        }
        all
    })
    .unwrap_or_else(|| {
        out.fill(0);
        false
    })
}

fn scaled(font: Option<&CTFont>, f: impl FnOnce(&fonts::Units) -> f64) -> CGFloat {
    font.map_or(0.0, |font| f(&parts(font).1.units) * scale(font))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetAscent(font: Option<&CTFont>) -> CGFloat {
    scaled(font, |u| u.ascent)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetDescent(font: Option<&CTFont>) -> CGFloat {
    scaled(font, |u| -u.descent)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetLeading(font: Option<&CTFont>) -> CGFloat {
    scaled(font, |u| u.leading)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetUnitsPerEm(font: Option<&CTFont>) -> c_uint {
    font.map_or(0, |f| parts(f).1.units.per_em as c_uint)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetGlyphCount(font: Option<&CTFont>) -> CFIndex {
    font.map_or(0, |f| parts(f).1.glyph_count as CFIndex)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetBoundingBox(font: Option<&CTFont>) -> CGRect {
    let Some(font) = font else { return CGRect::ZERO };
    let s = scale(font);
    let [x0, y0, x1, y1] = parts(font).1.units.bounds;
    rect(x0 * s, y0 * s, (x1 - x0) * s, (y1 - y0) * s)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetUnderlinePosition(font: Option<&CTFont>) -> CGFloat {
    scaled(font, |u| u.underline_position)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetUnderlineThickness(font: Option<&CTFont>) -> CGFloat {
    scaled(font, |u| u.underline_thickness)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetSlantAngle(font: Option<&CTFont>) -> CGFloat {
    font.map_or(0.0, |f| parts(f).1.units.italic_angle)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetCapHeight(font: Option<&CTFont>) -> CGFloat {
    scaled(font, |u| u.cap_height)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetXHeight(font: Option<&CTFont>) -> CGFloat {
    scaled(font, |u| u.x_height)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetGlyphWithName(font: Option<&CTFont>, name: Option<&CFString>) -> CGGlyph {
    let (Some(font), Some(name)) = (font, name) else { return 0 };
    let face = parts(font).1;
    let names = face.glyph_names.get_or_init(|| {
        with_skrifa(face, |f, _| {
            f.glyph_names().iter().map(|(id, n)| (n.as_str().into(), id.to_u32() as u16)).collect()
        })
        .unwrap_or_default()
    });
    names.get(text_of(name).as_str()).copied().unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyNameForGlyph(font: Option<&CTFont>, glyph: CGGlyph) -> Option<NonNull<CFString>> {
    let name = with_skrifa(parts(font?).1, |f, _| {
        f.glyph_names().get(skrifa::GlyphId::new(u32::from(glyph))).map(|n| n.as_str().to_string())
    })??;
    copy_string(&name)
}

/// A glyph's advance in font units; nothing for a glyph the font doesn't
/// have.
pub(crate) struct GlyphUnits {
    pub advance: f64,
}

/// The advances of `glyphs` in `face`, in font units (from `hmtx`).
pub(crate) fn glyph_advances(face: &Face, glyphs: impl Iterator<Item = u16>) -> Vec<Option<GlyphUnits>> {
    with_skrifa(face, |f, location| {
        let count = f.maxp().map_or(0, |m| m.num_glyphs());
        let metrics = f.glyph_metrics(skrifa::instance::Size::unscaled(), location);
        glyphs
            .map(|g| {
                let id = skrifa::GlyphId::new(u32::from(g));
                (g < count).then(|| GlyphUnits { advance: f64::from(metrics.advance_width(id).unwrap_or(0.0)) })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// The boxes of `glyphs`' outlines in `face` (x0, y0, x1, y1, font
/// units), none for an empty outline or a glyph the font doesn't have.
/// A face remembers its glyphs' boxes: an outline without a box of its
/// own (CFF's) is drawn to find one, which is slow.
pub(crate) fn glyph_bounds(face: &Face, glyphs: impl Iterator<Item = u16>) -> Vec<Option<[f64; 4]>> {
    let glyphs: Vec<u16> = glyphs.collect();
    let mut out: Vec<Option<[f64; 4]>> = Vec::with_capacity(glyphs.len());
    let mut missing: Vec<usize> = Vec::new();
    face.boxes.lookup(&glyphs, |k, found| match found {
        Some(b) => out.push(b.map(|b| b.map(f64::from))),
        None => {
            out.push(None);
            missing.push(k);
        }
    });
    if missing.is_empty() {
        return out;
    }
    let found = with_skrifa(face, |f, location| {
        let count = f.maxp().map_or(0, |m| m.num_glyphs());
        let metrics = f.glyph_metrics(skrifa::instance::Size::unscaled(), location);
        let outlines = f.outline_glyphs();
        missing
            .iter()
            .map(|&k| {
                let g = glyphs[k];
                if g >= count {
                    return None;
                }
                let id = skrifa::GlyphId::new(u32::from(g));
                match metrics.bounds(id) {
                    Some(b) if b.x_min < b.x_max || b.y_min < b.y_max => Some([b.x_min, b.y_min, b.x_max, b.y_max]),
                    Some(_) => None,
                    // Outlines without boxes of their own (CFF): the box of
                    // their points.
                    None => outlines.get(id).and_then(|o| {
                        let mut pen = super::draw::BoundsPen::default();
                        let settings =
                            skrifa::outline::DrawSettings::unhinted(skrifa::instance::Size::unscaled(), location);
                        o.draw(settings, &mut pen).ok()?;
                        pen.bounds().map(|b| b.map(|v| v as f32))
                    }),
                }
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_else(|| vec![None; missing.len()]);
    face.boxes.remember(missing.iter().map(|&k| glyphs[k]).zip(found.iter().copied()));
    for (&k, b) in missing.iter().zip(found) {
        out[k] = b.map(|b| b.map(f64::from));
    }
    out
}

/// # Safety
///
/// `glyphs` holds `count` glyphs.
unsafe fn glyph_slice<'a>(glyphs: *const CGGlyph, count: CFIndex) -> &'a [CGGlyph] {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::slice(glyphs, usize::try_from(count).unwrap_or(0)) }
}

/// # Safety
///
/// `out` is null or has room for `count` items.
unsafe fn out_slice<'a, T>(out: *mut T, count: usize) -> Option<&'a mut [T]> {
    // SAFETY: as the caller promises.
    (!out.is_null() && count > 0).then(|| unsafe { std::slice::from_raw_parts_mut(out, count) })
}

fn union(a: Option<CGRect>, b: CGRect) -> Option<CGRect> {
    if b.size.width <= 0.0 && b.size.height <= 0.0 {
        return a;
    }
    Some(match a {
        None => b,
        Some(a) => {
            let x0 = a.origin.x.min(b.origin.x);
            let y0 = a.origin.y.min(b.origin.y);
            let x1 = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
            let y1 = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
            rect(x0, y0, x1 - x0, y1 - y0)
        }
    })
}

/// # Safety
///
/// `glyphs` holds `count` glyphs; `rects` is null or has room for as many.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontGetBoundingRectsForGlyphs(
    font: Option<&CTFont>,
    orientation: CTFontOrientation,
    glyphs: *const CGGlyph,
    rects: *mut CGRect,
    count: CFIndex,
) -> CGRect {
    let Some(font) = font else { return CGRect::ZERO };
    // SAFETY: as the caller promises.
    let glyphs = unsafe { glyph_slice(glyphs, count) };
    // SAFETY: as the caller promises.
    let mut out = unsafe { out_slice(rects, glyphs.len()) };
    let (s, face) = (scale(font), parts(font).1);
    let vertical = orientation == CTFontOrientation::Vertical;
    let advances = if vertical { glyph_advances(face, glyphs.iter().copied()) } else { Vec::new() };
    let mut all = None;
    for (i, b) in glyph_bounds(face, glyphs.iter().copied()).into_iter().enumerate() {
        let r = match b {
            None => CGRect::ZERO,
            Some([x0, y0, x1, y1]) if !vertical => rect(x0 * s, y0 * s, (x1 - x0) * s, (y1 - y0) * s),
            // Turned a quarter about the vertical origin: half the advance
            // across, the typographic ascender down.
            Some([x0, y0, x1, y1]) => {
                let advance = advances.get(i).and_then(Option::as_ref).map_or(0.0, |a| a.advance);
                let half = (advance / 2.0).floor();
                let top = face.units.typo_ascender;
                rect((top - y1) * s, (x0 - half) * s, (y1 - y0) * s, (x1 - x0) * s)
            }
        };
        if let Some(out) = out.as_deref_mut() {
            out[i] = r;
        }
        all = union(all, r);
    }
    all.unwrap_or(CGRect::ZERO)
}

/// Optical bounds: each glyph's advance across and the font's ascent and
/// descent up and down (fonts' optical bounds tables aren't read).
///
/// # Safety
///
/// As `CTFontGetBoundingRectsForGlyphs`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontGetOpticalBoundsForGlyphs(
    font: Option<&CTFont>,
    glyphs: *const CGGlyph,
    rects: *mut CGRect,
    count: CFIndex,
    _options: usize,
) -> CGRect {
    let Some(font) = font else { return CGRect::ZERO };
    // SAFETY: as the caller promises.
    let glyphs = unsafe { glyph_slice(glyphs, count) };
    // SAFETY: as the caller promises.
    let mut out = unsafe { out_slice(rects, glyphs.len()) };
    let (s, face) = (scale(font), parts(font).1);
    let (ascent, descent) = (face.units.ascent * s, -face.units.descent * s);
    let mut all = None;
    for (i, g) in glyph_advances(face, glyphs.iter().copied()).into_iter().enumerate() {
        let advance = g.map_or(0.0, |g| g.advance) * s;
        let r = rect(0.0, -descent, advance, ascent + descent);
        if let Some(out) = out.as_deref_mut() {
            out[i] = r;
        }
        all = union(all, r);
    }
    all.unwrap_or(CGRect::ZERO)
}

/// The advances of `glyphs` in points, across (or, vertically, down) and
/// their total.
///
/// # Safety
///
/// `glyphs` holds `count` glyphs; `advances` is null or has room for as
/// many.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontGetAdvancesForGlyphs(
    font: Option<&CTFont>,
    orientation: CTFontOrientation,
    glyphs: *const CGGlyph,
    advances: *mut CGSize,
    count: CFIndex,
) -> f64 {
    let Some(font) = font else { return 0.0 };
    // SAFETY: as the caller promises.
    let glyphs = unsafe { glyph_slice(glyphs, count) };
    // SAFETY: as the caller promises.
    let mut out = unsafe { out_slice(advances, glyphs.len()) };
    let (s, face) = (scale(font), parts(font).1);
    let vertical = orientation == CTFontOrientation::Vertical;
    let height = face.units.typo_ascender - face.units.typo_descender;
    let inked = if vertical { glyph_bounds(face, glyphs.iter().copied()) } else { Vec::new() };
    let mut total = 0.0;
    for (i, g) in glyph_advances(face, glyphs.iter().copied()).into_iter().enumerate() {
        let advance = match g {
            None => 0.0,
            // Down a vertical line: the typographic height, or across for
            // a glyph with no outline.
            Some(_) if vertical && inked.get(i).is_some_and(Option::is_some) => height * s,
            Some(g) => g.advance * s,
        };
        if let Some(out) = out.as_deref_mut() {
            out[i] = CGSize { width: advance, height: 0.0 };
        }
        total += advance;
    }
    total
}

/// # Safety
///
/// `glyphs` holds `count` glyphs and `translations` has room for as many.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontGetVerticalTranslationsForGlyphs(
    font: Option<&CTFont>,
    glyphs: *const CGGlyph,
    translations: *mut CGSize,
    count: CFIndex,
) {
    let Some(font) = font else { return };
    // SAFETY: as the caller promises.
    let glyphs = unsafe { glyph_slice(glyphs, count) };
    // SAFETY: as the caller promises.
    let Some(out) = (unsafe { out_slice(translations, glyphs.len()) }) else { return };
    let (s, face) = (scale(font), parts(font).1);
    let inked = glyph_bounds(face, glyphs.iter().copied());
    for (i, g) in glyph_advances(face, glyphs.iter().copied()).into_iter().enumerate() {
        out[i] = match g {
            Some(g) if inked[i].is_some() => {
                CGSize { width: -(g.advance / 2.0).floor() * s, height: -face.units.typo_ascender * s }
            }
            _ => CGSize { width: 0.0, height: 0.0 },
        };
    }
}

/// A glyph's outline at the font's size, y up, through `matrix`; none for
/// a glyph with nothing to draw.
///
/// # Safety
///
/// `matrix` is null or points at a transform.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontCreatePathForGlyph(
    font: Option<&CTFont>,
    glyph: CGGlyph,
    matrix: *const CGAffineTransform,
) -> Option<NonNull<CGPath>> {
    let font = font?;
    let face = parts(font).1;
    let data = face.font.as_ref()?;
    let coords = super::draw::coords_of(face);
    let mut path = super::draw::outline(data, &coords, glyph, scale(font))?;
    // SAFETY: as the caller promises.
    if let Some(m) = unsafe { matrix.as_ref() } {
        path.apply_affine(crate::coregraphics::geometry::kurbo_of(*m));
    }
    let shape = crate::coregraphics::path::Shape::from_path(path);
    Some(owned(crate::coregraphics::path::new_path(Arc::new(shape), false)))
}

/// A face's variation axes, as dictionaries of CoreText's axis keys.
fn variation_axes(face: &Face) -> Option<Retained<AnyObject>> {
    let axes = with_skrifa(face, |f, _| {
        f.axes()
            .iter()
            .map(|axis| {
                let tag = u32::from_be_bytes(axis.tag().to_be_bytes());
                let name: String = f
                    .localized_strings(axis.name_id())
                    .english_or_first()
                    .map(|s| s.chars().collect())
                    .unwrap_or_else(|| axis.tag().to_string());
                (tag, f64::from(axis.min_value()), f64::from(axis.max_value()), f64::from(axis.default_value()), name)
            })
            .collect::<Vec<_>>()
    })?;
    if axes.is_empty() {
        return None;
    }
    // SAFETY: the constants are this crate's own.
    let key_refs: Vec<&NSString> = unsafe {
        [
            objc2_core_text::kCTFontVariationAxisIdentifierKey,
            objc2_core_text::kCTFontVariationAxisMinimumValueKey,
            objc2_core_text::kCTFontVariationAxisMaximumValueKey,
            objc2_core_text::kCTFontVariationAxisDefaultValueKey,
            objc2_core_text::kCTFontVariationAxisNameKey,
        ]
    }
    .into_iter()
    .map(super::ns_string)
    .collect();
    let dicts: Vec<Retained<NSDictionary<NSString, AnyObject>>> = axes
        .into_iter()
        .map(|(tag, min, max, default, name)| {
            let values = [
                integer(i64::from(tag)),
                number(min),
                number(max),
                number(default),
                super::any(NSString::from_str(&name)),
            ];
            let refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
            NSDictionary::from_slices(&key_refs, &refs)
        })
        .collect();
    Some(super::any(NSArray::from_retained_slice(&dicts)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyVariationAxes(font: Option<&CTFont>) -> Option<NonNull<CFArray>> {
    variation_axes(parts(font?).1).map(owned)
}

/// The variation the face was matched at, by axis tag; none for a face
/// with no axes set.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyVariation(font: Option<&CTFont>) -> Option<NonNull<CFDictionary>> {
    let face = parts(font?).1;
    if face.variations.is_empty() {
        return None;
    }
    let keys: Vec<Retained<NSNumber>> =
        face.variations.iter().map(|(tag, _)| NSNumber::new_u32(u32::from_be_bytes(tag.to_be_bytes()))).collect();
    let values: Vec<Retained<AnyObject>> = face.variations.iter().map(|(_, v)| number(f64::from(*v))).collect();
    let key_refs: Vec<&NSNumber> = keys.iter().map(|k| &**k).collect();
    let value_refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    Some(owned(NSDictionary::<NSNumber, AnyObject>::from_slices(&key_refs, &value_refs)))
}

/// The features a face's layout tables have, grouped as the font feature
/// registry's types with their selectors, as CoreText lists them.
fn features(face: &Face) -> Option<Retained<AnyObject>> {
    let tags = with_skrifa(face, |f, _| {
        let mut tags: Vec<[u8; 4]> = Vec::new();
        if let Ok(gsub) = f.gsub()
            && let Ok(list) = gsub.feature_list()
        {
            tags.extend(list.feature_records().iter().map(|r| r.feature_tag().to_be_bytes()));
        }
        if let Ok(gpos) = f.gpos()
            && let Ok(list) = gpos.feature_list()
        {
            tags.extend(list.feature_records().iter().map(|r| r.feature_tag().to_be_bytes()));
        }
        tags.sort_unstable();
        tags.dedup();
        tags
    })?;
    /// A feature type and its selectors, with their OpenType tags.
    type Selectors = (i64, Vec<(i64, [u8; 4])>);
    let mut types: Vec<Selectors> = Vec::new();
    for tag in tags {
        let Some((kind, selector)) = nsfont::registry_setting(tag, 1) else { continue };
        match types.iter_mut().find(|t| t.0 == kind) {
            Some(t) => t.1.push((selector, tag)),
            None => types.push((kind, vec![(selector, tag)])),
        }
    }
    types.sort_by_key(|t| t.0);
    use objc2_core_text::{
        kCTFontFeatureSelectorIdentifierKey, kCTFontFeatureTypeIdentifierKey, kCTFontFeatureTypeSelectorsKey,
        kCTFontOpenTypeFeatureTag, kCTFontOpenTypeFeatureValue,
    };
    // SAFETY: the constants are this crate's own.
    let (type_keys, selector_keys) = unsafe {
        (
            [kCTFontFeatureTypeIdentifierKey, kCTFontFeatureTypeSelectorsKey].map(super::ns_string),
            [kCTFontFeatureSelectorIdentifierKey, kCTFontOpenTypeFeatureTag, kCTFontOpenTypeFeatureValue]
                .map(super::ns_string),
        )
    };
    let items: Vec<Retained<NSDictionary<NSString, AnyObject>>> = types
        .into_iter()
        .map(|(kind, selectors)| {
            let selectors: Vec<Retained<NSDictionary<NSString, AnyObject>>> = selectors
                .into_iter()
                .map(|(selector, tag)| {
                    let values =
                        [integer(selector), super::any(NSString::from_str(&String::from_utf8_lossy(&tag))), integer(1)];
                    let refs: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
                    NSDictionary::from_slices(&selector_keys, &refs)
                })
                .collect();
            let array = super::any(NSArray::from_retained_slice(&selectors));
            let kind = integer(kind);
            NSDictionary::from_slices(&type_keys, &[&*kind, &*array])
        })
        .collect();
    Some(super::any(NSArray::from_retained_slice(&items)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyFeatures(font: Option<&CTFont>) -> Option<NonNull<CFArray>> {
    features(parts(font?).1).map(owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyFeatureSettings(font: Option<&CTFont>) -> Option<NonNull<CFArray>> {
    nsfont::settings_of(parts(font?).0).map(owned)
}

/// # Safety
///
/// `attributes` is null or valid to write a descriptor pointer through
/// (it gets none: the font's attributes aren't handed back this way).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontCopyGraphicsFont(
    font: Option<&CTFont>,
    attributes: *mut *const CTFontDescriptor,
) -> Option<NonNull<CGFont>> {
    // SAFETY: as the caller promises.
    unsafe { crate::coregraphics::store(attributes, std::ptr::null()) };
    let data = parts(font?).1.font.clone()?;
    crate::coregraphics::font::from_data(data).map(owned)
}

/// The tables' tags, as numbers (CoreText puts the bare tags in the
/// array; Foundation's arrays hold objects).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyAvailableTables(
    font: Option<&CTFont>,
    _options: CTFontTableOptions,
) -> Option<NonNull<CFArray>> {
    let tags = with_skrifa(parts(font?).1, |f, _| {
        f.table_directory.table_records().iter().map(|r| u32::from_be_bytes(r.tag().to_be_bytes())).collect::<Vec<_>>()
    })?;
    let numbers: Vec<Retained<NSNumber>> = tags.into_iter().map(NSNumber::new_u32).collect();
    Some(owned(NSArray::from_retained_slice(&numbers)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontHasTable(font: Option<&CTFont>, tag: CTFontTableTag) -> bool {
    font.and_then(|f| {
        with_skrifa(parts(f).1, |f, _| f.table_data(skrifa::Tag::from_be_bytes(tag.to_be_bytes())).is_some())
    })
    .unwrap_or(false)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontCopyTable(
    font: Option<&CTFont>,
    tag: CTFontTableTag,
    _options: CTFontTableOptions,
) -> Option<NonNull<CFData>> {
    let bytes = with_skrifa(parts(font?).1, |f, _| {
        f.table_data(skrifa::Tag::from_be_bytes(tag.to_be_bytes())).map(|d| d.as_bytes().to_vec())
    })??;
    crate::image_rep::make_data(&bytes).map(owned)
}

/// Draw `glyphs` at `positions` (text space: through the text matrix) in
/// the context's fill color.
///
/// # Safety
///
/// `glyphs` and `positions` hold `count` items.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontDrawGlyphs(
    font: Option<&CTFont>,
    glyphs: *const CGGlyph,
    positions: *const CGPoint,
    count: usize,
    context: Option<&CGContext>,
) {
    let (Some(font), Some(context)) = (font, context) else { return };
    // SAFETY: as the caller promises.
    let (glyphs, positions) =
        unsafe { (crate::coregraphics::slice(glyphs, count), crate::coregraphics::slice(positions, count)) };
    if glyphs.len() != positions.len() {
        return;
    }
    super::draw::draw_font_glyphs(context, ns_font(font), glyphs, positions);
}

/// Ligature carets from `GDEF` aren't read: none.
///
/// # Safety
///
/// Nothing is written.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CTFontGetLigatureCaretPositions(
    _font: Option<&CTFont>,
    _glyph: CGGlyph,
    _positions: *mut CGFloat,
    _max_positions: CFIndex,
) -> CFIndex {
    0
}

/// Adaptive image glyphs (Genmoji) belong to macOS: no bounds.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontGetTypographicBoundsForAdaptiveImageProvider(
    _font: Option<&CTFont>,
    _provider: *const c_void,
) -> CGRect {
    CGRect::ZERO
}

/// Adaptive image glyphs draw nothing.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTFontDrawImageFromAdaptiveImageProviderAtPoint(
    _font: Option<&CTFont>,
    _provider: *const c_void,
    _point: CGPoint,
    _context: Option<&CGContext>,
) {
}
