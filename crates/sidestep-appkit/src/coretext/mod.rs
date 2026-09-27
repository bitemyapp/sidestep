//! CoreText, on Sidestep's text engine: the C functions and types
//! objc2-core-text declares, over the fonts, layout and glyph drawing
//! AppKit's text uses (`text/`: fontique, parley, swash). There is no
//! second shaper: a `CTLine` is a line of the same layout string drawing
//! makes, and drawing glyphs through a `CGContext` records the same glyph
//! runs.
//!
//! As on macOS, CoreText's fonts and AppKit's are the same objects: a
//! `CTFont` *is* an `NSFont` (`CTFontGetTypeID` is the type ID of `NSFont`'s
//! class, and a font made either way can be cast to the other), and a
//! `CTFontDescriptor` is an `NSFontDescriptor`. A `CFAttributedString` is an
//! `NSAttributedString` (`sidestep-foundation`'s `cf`), so CoreText reads
//! attributed strings through the same runs string drawing does, with
//! CoreText's attribute names as well as AppKit's. The other types are
//! classes of their own with Sidestep-private names and type IDs
//! `CFGetTypeID` knows (`sidestep_foundation::cf_type_ids`), as
//! CoreGraphics' are.
//!
//! - [`strings`]: the string constants, with macOS's values.
//! - [`font`]: `CTFont` over `NSFont`: creation (by name, from
//!   descriptors, from a `CGFont`'s file), metrics in the font's own units
//!   scaled exactly, glyphs for characters, advances, bounds, outlines as
//!   `CGPath`s, names and tables.
//! - [`descriptor`]: `CTFontDescriptor` over `NSFontDescriptor`, whose
//!   attribute handling and matching it shares.
//! - [`manager`]: the font manager, registering font files and `CGFont`s.
//! - [`collection`]: font collections.
//! - [`attrs`]: an attributed string's runs as layout attributes, with
//!   what CoreText adds (colors as `CGColor`s, the context's fill color,
//!   tracking, CoreText's paragraph styles).
//! - [`line`]: `CTLine` and `CTRun`.
//! - [`typesetter`]: `CTTypesetter`, `CTFramesetter` and `CTFrame`.
//! - [`paragraph`]: `CTParagraphStyle`, `CTTextTab`, and the small types
//!   (glyph infos, run delegates, ruby annotations).
//! - [`draw`]: glyphs drawn into a CGContext's graphics state, and glyph
//!   outlines; CoreGraphics' own glyph drawing (`CGContextShowGlyphs…`)
//!   goes through it too.
//!
//! Font features set through feature settings (`kCTFontFeatureSettingsAttribute`,
//! by the font feature registry's types and selectors or by OpenType tag)
//! become OpenType features for the shaper, as `NSFont`'s do
//! (`font::registry_feature`), and variation axis values
//! (`kCTFontVariationAttribute`) the face's matched and laid out at. What
//! isn't here: vertical text (the vertical glyph metrics are, drawing and
//! layout aren't), font matrices other than the identity (ignored), ruby,
//! run delegates' sizes, and downloadable fonts.

pub(crate) mod attrs;
pub(crate) mod collection;
pub(crate) mod descriptor;
pub(crate) mod draw;
pub(crate) mod font;
pub(crate) mod line;
pub(crate) mod manager;
pub(crate) mod paragraph;
pub(crate) mod strings;
#[cfg(test)]
mod tests;
pub(crate) mod typesetter;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSFont, NSFontDescriptor};
use objc2_core_foundation::{CFIndex, CFRange, CFString};
use objc2_core_text::{CTFont, CTFontDescriptor};
use objc2_foundation::{NSNumber, NSString};

pub(crate) use crate::coregraphics::{borrowed, owned};

/// A `CTFont` as the `NSFont` it is.
pub(crate) fn ns_font(font: &CTFont) -> &NSFont {
    // SAFETY: every CTFont is an NSFont.
    unsafe { &*(font as *const CTFont).cast::<NSFont>() }
}

/// An `NSFont` as a `CTFont`.
pub(crate) fn ct_font(font: &NSFont) -> &CTFont {
    // SAFETY: as above.
    unsafe { &*(font as *const NSFont).cast::<CTFont>() }
}

/// A `CTFontDescriptor` as the `NSFontDescriptor` it is.
pub(crate) fn ns_descriptor(d: &CTFontDescriptor) -> &NSFontDescriptor {
    // SAFETY: every CTFontDescriptor is an NSFontDescriptor.
    unsafe { &*(d as *const CTFontDescriptor).cast::<NSFontDescriptor>() }
}

pub(crate) fn ns_string(s: &CFString) -> &NSString {
    // SAFETY: a CFString is an NSString here.
    unsafe { &*(s as *const CFString).cast::<NSString>() }
}

pub(crate) fn text_of(s: &CFString) -> String {
    ns_string(s).to_string()
}

pub(crate) fn number(value: f64) -> Retained<AnyObject> {
    any(NSNumber::new_f64(value))
}

pub(crate) fn integer(value: i64) -> Retained<AnyObject> {
    any(NSNumber::new_i64(value))
}

pub(crate) fn any<T: Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY: every object is an AnyObject.
    unsafe { Retained::cast_unchecked(object) }
}

/// The part of `0..len` a CoreFoundation range asks for; a zero length
/// means to the end, as the run functions take it.
pub(crate) fn range_in(range: CFRange, len: usize, zero_is_all: bool) -> std::ops::Range<usize> {
    let start = usize::try_from(range.location).unwrap_or(0).min(len);
    let end = if range.length == 0 && zero_is_all {
        len
    } else {
        start.saturating_add(usize::try_from(range.length).unwrap_or(0)).min(len)
    };
    start..end.max(start)
}

pub(crate) fn cf_range(range: std::ops::Range<usize>) -> CFRange {
    CFRange { location: range.start as CFIndex, length: range.len() as CFIndex }
}

/// The function `block` runs, read from the block's layout (its class, two
/// 32-bit fields, then the function): how blocks taking C's `bool`, which
/// objc2 can't encode, are called.
///
/// # Safety
///
/// `block` is a block; the function is called with the block and the
/// arguments its type says.
pub(crate) unsafe fn block_function<F: ?Sized>(block: &block2::Block<F>) -> *const std::ffi::c_void {
    let base = (block as *const block2::Block<F>).cast::<u8>();
    // SAFETY: every block starts with this layout.
    unsafe { *base.add(std::mem::size_of::<*const u8>() + 8).cast::<*const std::ffi::c_void>() }
}

/// The point size a font made with `size` has: 12 for none, as CoreText's.
pub(crate) fn size_or_default(size: f64) -> f64 {
    if size > 0.0 && size.is_finite() { size } else { 12.0 }
}

/// CoreText's version: that of macOS 26's.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn CTGetCoreTextVersion() -> u32 {
    0x0015_0000
}
