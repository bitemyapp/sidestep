//! `CGFont`: a font file's face, read with skrifa (the tables text layout
//! reads too). Values are the file's own, in font units: the metrics from
//! `hhea` and `OS/2`, the bounds from `head`, names from `name` and glyph
//! names from `post` or the CFF charset. Fonts come from a data provider
//! (a font file's bytes) or by name, from the system's fonts as `NSFont`
//! finds them. The face's data is kept as text layout keeps faces'
//! (`parley::FontData`), for CoreText's fonts to be made from.

use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::{CFArray, CFData, CFDictionary, CFString, CFTypeID, CGFloat, CGRect};
use objc2_core_graphics::{CGDataProvider, CGFont, CGFontPostScriptFormat, CGGlyph};
use objc2_foundation::{NSArray, NSNumber, NSString};
use parley::FontData;
use parley::fontique::Blob;
use skrifa::MetadataProvider;
use skrifa::raw::TableProvider;
use skrifa::string::StringId;

sidestep_foundation::constant_string!(kCGFontVariationAxisName = "kCGFontVariationAxisName");
sidestep_foundation::constant_string!(kCGFontVariationAxisMinValue = "kCGFontVariationAxisMinValue");
sidestep_foundation::constant_string!(kCGFontVariationAxisMaxValue = "kCGFontVariationAxisMaxValue");
sidestep_foundation::constant_string!(kCGFontVariationAxisDefaultValue = "kCGFontVariationAxisDefaultValue");

pub(crate) struct FontIvars {
    pub(crate) data: FontData,
    /// Glyphs by name, made the first time one is looked up.
    names: OnceLock<HashMap<String, u16>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; a font is
    // immutable, its name table made once behind a OnceLock.
    #[unsafe(super(NSObject))]
    #[name = "_SidestepCGFont"]
    #[ivars = FontIvars]
    pub(crate) struct CGFontImpl;

    impl CGFontImpl {
        #[unsafe(method_id(description))]
        fn description(&self) -> Retained<NSString> {
            let name = self.string(StringId::POSTSCRIPT_NAME).unwrap_or_default();
            NSString::from_str(&format!("<CGFont ({:#x}): {name}>", self as *const Self as usize))
        }
    }

    unsafe impl NSObjectProtocol for CGFontImpl {}
);

impl CGFontImpl {
    fn font(&self) -> Option<skrifa::FontRef<'_>> {
        skrifa::FontRef::from_index(self.ivars().data.data.data(), self.ivars().data.index).ok()
    }

    fn string(&self, id: StringId) -> Option<String> {
        let font = self.font()?;
        let s: String = font.localized_strings(id).english_or_first()?.chars().collect();
        (!s.is_empty()).then_some(s)
    }

    fn glyph_named(&self, name: &str) -> u16 {
        let names = self.ivars().names.get_or_init(|| {
            let Some(font) = self.font() else { return HashMap::new() };
            font.glyph_names().iter().map(|(id, n)| (n.as_str().to_string(), id.to_u32() as u16)).collect()
        });
        names.get(name).copied().unwrap_or(0)
    }
}

pub(crate) fn font_imp(f: &CGFont) -> &CGFontImpl {
    // SAFETY: every CGFont is a CGFontImpl.
    unsafe { &*(f as *const CGFont).cast::<CGFontImpl>() }
}

/// A font of `data`, if it's one skrifa reads.
pub(crate) fn from_data(data: FontData) -> Option<Retained<CGFontImpl>> {
    skrifa::FontRef::from_index(data.data.data(), data.index).ok()?;
    let this = CGFontImpl::alloc().set_ivars(FontIvars { data, names: OnceLock::new() });
    // SAFETY: NSObject's designated initializer.
    Some(unsafe { msg_send![super(this), init] })
}

/// The system's font named `name` (a PostScript or family name), as
/// `NSFont` finds it.
pub(crate) fn named(name: &str) -> Option<Retained<CGFontImpl>> {
    let spec = crate::text::fonts::spec_named(name, 12.0)?;
    if spec.missing {
        return None;
    }
    let face = crate::text::fonts::resolve(&spec);
    from_data(face.font.clone()?)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetTypeID() -> CFTypeID {
    sidestep_foundation::cf_type_ids::CG_FONT
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCreateWithDataProvider(provider: &CGDataProvider) -> Option<NonNull<CGFont>> {
    // The provider's bytes, shared, not copied.
    let bytes = super::data::provider_imp(provider).bytes()?;
    let blob = Blob::new(std::sync::Arc::new(bytes));
    from_data(FontData::new(blob, 0)).map(super::owned)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCreateWithFontName(name: Option<&CFString>) -> Option<NonNull<CGFont>> {
    named(&super::color::cf_text(name?)).map(super::owned)
}

/// Variations aren't applied: the copy is the font.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCreateCopyWithVariations(
    font: Option<&CGFont>,
    _variations: Option<&CFDictionary>,
) -> Option<NonNull<CGFont>> {
    let f = font_imp(font?);
    from_data(f.ivars().data.clone()).map(super::owned)
}

/// Platform fonts belong to macOS.
///
/// # Safety
///
/// Nothing is read.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGFontCreateWithPlatformFont(
    _platform_font_reference: *mut c_void,
) -> Option<NonNull<CGFont>> {
    None
}

fn with_font<R: Default>(font: Option<&CGFont>, f: impl FnOnce(&skrifa::FontRef<'_>) -> Option<R>) -> R {
    font.and_then(|font| font_imp(font).font().and_then(|r| f(&r))).unwrap_or_default()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetNumberOfGlyphs(font: Option<&CGFont>) -> usize {
    with_font(font, |f| Some(f.maxp().ok()?.num_glyphs() as usize))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetUnitsPerEm(font: Option<&CGFont>) -> c_int {
    with_font(font, |f| Some(c_int::from(f.head().ok()?.units_per_em())))
}

fn name_copy(font: Option<&CGFont>, id: StringId) -> Option<NonNull<CFString>> {
    let name = font_imp(font?).string(id)?;
    Some(super::owned(NSString::from_str(&name)))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCopyPostScriptName(font: Option<&CGFont>) -> Option<NonNull<CFString>> {
    name_copy(font, StringId::POSTSCRIPT_NAME)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCopyFullName(font: Option<&CGFont>) -> Option<NonNull<CFString>> {
    name_copy(font, StringId::FULL_NAME)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetAscent(font: Option<&CGFont>) -> c_int {
    with_font(font, |f| Some(c_int::from(f.hhea().ok()?.ascender().to_i16())))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetDescent(font: Option<&CGFont>) -> c_int {
    with_font(font, |f| Some(c_int::from(f.hhea().ok()?.descender().to_i16())))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetLeading(font: Option<&CGFont>) -> c_int {
    with_font(font, |f| Some(c_int::from(f.hhea().ok()?.line_gap().to_i16())))
}

/// The top of a character's glyph, in font units: what a height falls
/// back to when `OS/2` doesn't give it (tables before version 2).
fn glyph_top(f: &skrifa::FontRef<'_>, c: char) -> Option<c_int> {
    let glyph = f.charmap().map(c)?;
    let metrics = f.glyph_metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
    Some(metrics.bounds(glyph)?.y_max.round() as c_int)
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetCapHeight(font: Option<&CGFont>) -> c_int {
    with_font(font, |f| f.os2().ok().and_then(|t| t.s_cap_height()).map(c_int::from).or_else(|| glyph_top(f, 'H')))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetXHeight(font: Option<&CGFont>) -> c_int {
    with_font(font, |f| f.os2().ok().and_then(|t| t.sx_height()).map(c_int::from).or_else(|| glyph_top(f, 'x')))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetFontBBox(font: Option<&CGFont>) -> CGRect {
    let b = with_font(font, |f| {
        let h = f.head().ok()?;
        Some([h.x_min(), h.y_min(), h.x_max(), h.y_max()].map(f64::from))
    });
    super::geometry::rect(b[0], b[1], b[2] - b[0], b[3] - b[1])
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetItalicAngle(font: Option<&CGFont>) -> CGFloat {
    with_font(font, |f| Some(f.post().ok()?.italic_angle().to_f64()))
}

/// TrueType faces have no stem width to give.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetStemV(_font: Option<&CGFont>) -> CGFloat {
    0.0
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCopyVariationAxes(_font: Option<&CGFont>) -> Option<NonNull<CFArray>> {
    None
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCopyVariations(_font: Option<&CGFont>) -> Option<NonNull<CFDictionary>> {
    None
}

/// # Safety
///
/// `glyphs` holds `count` glyphs and `advances` has room for as many.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGFontGetGlyphAdvances(
    font: Option<&CGFont>,
    glyphs: NonNull<CGGlyph>,
    count: usize,
    advances: NonNull<c_int>,
) -> bool {
    let Some(f) = font.and_then(|f| font_imp(f).font()) else { return false };
    let Ok(hmtx) = f.hmtx() else { return false };
    let glyph_count = f.maxp().map_or(0, |m| m.num_glyphs());
    for i in 0..count {
        // SAFETY: as the caller promises.
        let glyph = unsafe { *glyphs.as_ptr().add(i) };
        // A glyph the font doesn't have advances nothing.
        let advance =
            if glyph < glyph_count { hmtx.advance(skrifa::GlyphId::new(u32::from(glyph))).unwrap_or(0) } else { 0 };
        // SAFETY: as the caller promises.
        unsafe { *advances.as_ptr().add(i) = c_int::from(advance) };
    }
    true
}

/// # Safety
///
/// `glyphs` holds `count` glyphs and `bboxes` has room for as many.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn CGFontGetGlyphBBoxes(
    font: Option<&CGFont>,
    glyphs: NonNull<CGGlyph>,
    count: usize,
    bboxes: NonNull<CGRect>,
) -> bool {
    let Some(f) = font.and_then(|f| font_imp(f).font()) else { return false };
    let metrics = f.glyph_metrics(skrifa::instance::Size::unscaled(), skrifa::instance::LocationRef::default());
    for i in 0..count {
        // SAFETY: as the caller promises.
        let glyph = unsafe { *glyphs.as_ptr().add(i) };
        let b = metrics.bounds(skrifa::GlyphId::new(u32::from(glyph))).unwrap_or_default();
        let r = super::geometry::rect(
            f64::from(b.x_min),
            f64::from(b.y_min),
            f64::from(b.x_max - b.x_min),
            f64::from(b.y_max - b.y_min),
        );
        // SAFETY: as the caller promises.
        unsafe { *bboxes.as_ptr().add(i) = r };
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontGetGlyphWithGlyphName(font: Option<&CGFont>, name: Option<&CFString>) -> CGGlyph {
    let (Some(font), Some(name)) = (font, name) else { return 0 };
    font_imp(font).glyph_named(&super::color::cf_text(name))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCopyGlyphNameForGlyph(
    font: Option<&CGFont>,
    glyph: CGGlyph,
) -> Option<NonNull<CFString>> {
    let f = font_imp(font?).font()?;
    let name = f.glyph_names().get(skrifa::GlyphId::new(u32::from(glyph)))?;
    Some(super::owned(NSString::from_str(name.as_str())))
}

/// A TrueType face can be subset as Type 42 (its outlines as they are),
/// as CoreGraphics answers; making the subsets comes with PDF, which isn't
/// here.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCanCreatePostScriptSubset(
    font: Option<&CGFont>,
    format: CGFontPostScriptFormat,
) -> bool {
    format == CGFontPostScriptFormat::Type42
        && font.and_then(|f| font_imp(f).font()).is_some_and(|f| f.table_data(skrifa::Tag::new(b"glyf")).is_some())
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCopyTableTags(font: Option<&CGFont>) -> Option<NonNull<CFArray>> {
    let f = font_imp(font?).font()?;
    let tags: Vec<objc2::rc::Retained<NSNumber>> = f
        .table_directory
        .table_records()
        .iter()
        .map(|r| NSNumber::new_u32(u32::from_be_bytes(r.tag().to_be_bytes())))
        .collect();
    let array: Retained<NSArray<NSNumber>> = NSArray::from_retained_slice(&tags);
    Some(super::owned(array))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn CGFontCopyTableForTag(font: Option<&CGFont>, tag: u32) -> Option<NonNull<CFData>> {
    let f = font_imp(font?).font()?;
    let data = f.table_data(skrifa::Tag::from_be_bytes(tag.to_be_bytes()))?;
    crate::image_rep::make_data(data.as_bytes()).map(super::owned)
}
